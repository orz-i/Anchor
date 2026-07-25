use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{
    header::{AUTHORIZATION, CACHE_CONTROL, PRAGMA, WWW_AUTHENTICATE},
    HeaderMap, HeaderValue, StatusCode,
};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::bearer::constant_time_eq_str;

pub const OAUTH_CODE_TTL_SECONDS: u64 = 300;
pub const OAUTH_TOKEN_TTL_SECONDS: i64 = 60 * 60;
pub const OAUTH_REFRESH_TOKEN_TTL_SECONDS: i64 = 60 * 60 * 24 * 90;
#[allow(dead_code)]
pub const OAUTH_MAX_BODY_BYTES: usize = 8_192;

#[derive(Clone)]
pub struct OAuthRuntime {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub password: String,
    pub token_secret: String,
    redirect_uris: Arc<Vec<String>>,
    pending: Arc<Mutex<HashMap<String, PendingCode>>>,
    used_refresh_tokens: Arc<Mutex<HashMap<String, u64>>>,
}

fn decode_refresh_token(
    token: &str,
    token_secret: &str,
    issuer_url: &str,
    resource_url: &str,
) -> Result<TokenClaims, &'static str> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[resource_url]);
    validation.set_issuer(&[issuer_url]);
    let decoded = decode::<TokenClaims>(
        token,
        &DecodingKey::from_secret(token_secret.as_bytes()),
        &validation,
    )
    .map_err(|_| "refresh token is invalid or expired")?;
    if decoded.claims.token_kind != "refresh" {
        return Err("token is not a refresh token");
    }
    Ok(decoded.claims)
}

fn token_success(tokens: TokenPair) -> Response {
    token_json_response(
        StatusCode::OK,
        json!({
            "access_token": tokens.access_token,
            "refresh_token": tokens.refresh_token,
            "token_type": "Bearer",
            "expires_in": OAUTH_TOKEN_TTL_SECONDS,
            "refresh_token_expires_in": OAUTH_REFRESH_TOKEN_TTL_SECONDS,
            "scope": "mcp"
        }),
    )
}

fn default_access_token_kind() -> String {
    "access".into()
}

fn resource_matches(requested: &str, canonical: &str) -> bool {
    let requested = requested.trim().trim_end_matches('/');
    let canonical = canonical.trim().trim_end_matches('/');
    !requested.is_empty() && constant_time_eq_str(requested, canonical)
}

pub fn parse_redirect_uris(raw: &str) -> Result<Vec<String>, String> {
    let mut values = Vec::new();
    for value in raw.lines().map(str::trim).filter(|value| !value.is_empty()) {
        if value.contains('*') {
            return Err(format!(
                "OAuth redirect URI must be exact and cannot contain wildcards: {value}"
            ));
        }
        let parsed = reqwest::Url::parse(value)
            .map_err(|error| format!("Invalid OAuth redirect URI `{value}`: {error}"))?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(format!(
                "OAuth redirect URI cannot contain user information: {value}"
            ));
        }
        if parsed.fragment().is_some() {
            return Err(format!("OAuth redirect URI cannot contain a fragment: {value}"));
        }
        let host = parsed.host_str().unwrap_or_default();
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
        let secure = parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback);
        if !secure {
            return Err(format!(
                "OAuth redirect URI must use HTTPS or an HTTP loopback host: {value}"
            ));
        }
        if !values.iter().any(|registered| registered == value) {
            values.push(value.to_string());
        }
    }
    Ok(values)
}

#[derive(Clone)]
#[allow(dead_code)]
struct PendingCode {
    code_challenge: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    expires_at: u64,
    issuer_url: String,
    resource_url: String,
}

#[derive(Serialize, Deserialize)]
struct TokenClaims {
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
    scope: String,
    #[serde(default = "default_access_token_kind")]
    token_kind: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    jti: String,
}

impl OAuthRuntime {
    pub fn new(
        _base_url: String,
        client_id: String,
        client_secret: Option<String>,
        password: String,
        token_secret: String,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            password,
            token_secret,
            redirect_uris: Arc::new(Vec::new()),
            pending: Arc::new(Mutex::new(HashMap::new())),
            used_refresh_tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_redirect_uris(mut self, raw: &str) -> Result<Self, String> {
        self.redirect_uris = Arc::new(parse_redirect_uris(raw)?);
        Ok(self)
    }

    pub fn redirect_uri_allowed(&self, redirect_uri: &str) -> bool {
        let redirect_uri = redirect_uri.trim();
        !redirect_uri.is_empty()
            && self
                .redirect_uris
                .iter()
                .any(|registered| constant_time_eq_str(registered, redirect_uri))
    }

    pub fn client_id_allowed(&self, client_id: &str) -> bool {
        if client_id.is_empty() {
            return false;
        }
        if self.client_id.is_empty() {
            return true;
        }
        constant_time_eq_str(client_id, &self.client_id)
    }

    pub fn verify_access_token(
        &self,
        token: &str,
        issuer_url: &str,
        resource_url: &str,
    ) -> bool {
        let issuer_url = issuer_url.trim_end_matches('/');
        let resource_url = resource_url.trim_end_matches('/');
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[resource_url]);
        validation.set_issuer(&[issuer_url]);
        decode::<TokenClaims>(
            token,
            &DecodingKey::from_secret(self.token_secret.as_bytes()),
            &validation,
        )
        .map(|decoded| decoded.claims.token_kind == "access")
        .unwrap_or(false)
    }
}

pub fn verify_oauth_bearer_header(
    headers: &HeaderMap,
    oauth: &OAuthRuntime,
    issuer_url: &str,
    resource_url: &str,
    resource_metadata_url: &str,
) -> Option<Response> {
    let Some(header_value) = headers.get(AUTHORIZATION) else {
        return Some(oauth_unauthorized(
            "Missing Authorization header",
            resource_metadata_url,
            None,
        ));
    };
    let Ok(header_str) = header_value.to_str() else {
        return Some(oauth_unauthorized(
            "Invalid Authorization header",
            resource_metadata_url,
            Some("invalid_token"),
        ));
    };
    let Some(token) = header_str.strip_prefix("Bearer ").map(str::trim) else {
        return Some(oauth_unauthorized(
            "Invalid bearer token",
            resource_metadata_url,
            Some("invalid_token"),
        ));
    };
    if oauth.verify_access_token(token, issuer_url, resource_url) {
        None
    } else {
        Some(oauth_unauthorized(
            "Invalid bearer token",
            resource_metadata_url,
            Some("invalid_token"),
        ))
    }
}

fn oauth_unauthorized(
    message: &'static str,
    resource_metadata_url: &str,
    error: Option<&str>,
) -> Response {
    let mut response = (StatusCode::UNAUTHORIZED, message).into_response();
    let challenge = match error {
        Some(error) => format!(
            "Bearer error=\"{error}\", scope=\"mcp\", resource_metadata=\"{}\"",
            resource_metadata_url.trim()
        ),
        None => format!(
            "Bearer scope=\"mcp\", resource_metadata=\"{}\"",
            resource_metadata_url.trim()
        ),
    };
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    response
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeParams {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub resource: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeForm {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub resource: String,
    pub password: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct TokenForm {
    pub grant_type: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub code_verifier: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub resource: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub scope: String,
}

pub fn authorize_get(
    oauth: &OAuthRuntime,
    params: AuthorizeParams,
    canonical_resource_url: &str,
    workspace_path: Option<&str>,
) -> Response {
    if params.response_type != "code" {
        return html_error("response_type must be 'code'", StatusCode::BAD_REQUEST);
    }
    if !oauth.client_id_allowed(&params.client_id) {
        return html_error("Unknown client_id", StatusCode::BAD_REQUEST);
    }
    if !oauth.redirect_uri_allowed(&params.redirect_uri) {
        return html_error("redirect_uri is not registered", StatusCode::BAD_REQUEST);
    }
    if params.code_challenge_method != "S256" || params.code_challenge.is_empty() {
        return html_error(
            "code_challenge_method must be S256 and code_challenge is required",
            StatusCode::BAD_REQUEST,
        );
    }
    if !resource_matches(&params.resource, canonical_resource_url) {
        return html_error("Unknown resource", StatusCode::BAD_REQUEST);
    }
    Html(login_page(
        &params.client_id,
        &params.redirect_uri,
        &params.code_challenge,
        &params.code_challenge_method,
        &params.state,
        canonical_resource_url,
        "",
        workspace_path,
    ))
    .into_response()
}

pub fn authorize_post(
    oauth: &OAuthRuntime,
    form: AuthorizeForm,
    issuer_url: &str,
    canonical_resource_url: &str,
) -> Response {
    if !oauth.client_id_allowed(&form.client_id) {
        return Html(login_page(
            &form.client_id,
            &form.redirect_uri,
            &form.code_challenge,
            &form.code_challenge_method,
            &form.state,
            &form.resource,
            "Invalid client",
            None,
        ))
        .into_response();
    }
    if !oauth.redirect_uri_allowed(&form.redirect_uri) {
        return html_error("redirect_uri is not registered", StatusCode::BAD_REQUEST);
    }
    if form.code_challenge_method != "S256" || form.code_challenge.is_empty() {
        return Html(login_page(
            &form.client_id,
            &form.redirect_uri,
            &form.code_challenge,
            &form.code_challenge_method,
            &form.state,
            &form.resource,
            "Invalid PKCE parameters",
            None,
        ))
        .into_response();
    }
    if !resource_matches(&form.resource, canonical_resource_url) {
        return Html(login_page(
            &form.client_id,
            &form.redirect_uri,
            &form.code_challenge,
            &form.code_challenge_method,
            &form.state,
            &form.resource,
            "Invalid resource",
            None,
        ))
        .into_response();
    }
    if !constant_time_eq_str(&form.password, &oauth.password) {
        return (
            StatusCode::UNAUTHORIZED,
            Html(login_page(
                &form.client_id,
                &form.redirect_uri,
                &form.code_challenge,
                &form.code_challenge_method,
                &form.state,
                &form.resource,
                "Invalid password",
                None,
            )),
        )
            .into_response();
    }

    let issuer_url = issuer_url.trim_end_matches('/').to_string();
    let resource_url = canonical_resource_url.trim_end_matches('/').to_string();
    let code = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = unix_now();
    {
        let mut pending = oauth.pending.lock().expect("oauth pending lock");
        pending.retain(|_, v| v.expires_at >= now);
        pending.insert(
            code.clone(),
            PendingCode {
                code_challenge: form.code_challenge.clone(),
                client_id: form.client_id.clone(),
                redirect_uri: form.redirect_uri.clone(),
                state: form.state.clone(),
                expires_at: now + OAUTH_CODE_TTL_SECONDS,
                issuer_url: issuer_url.clone(),
                resource_url,
            },
        );
    }

    let mut qs = format!("code={}", urlencoding_encode(&code));
    if !form.state.is_empty() {
        qs.push_str(&format!("&state={}", urlencoding_encode(&form.state)));
    }
    let sep = if form.redirect_uri.contains('?') { '&' } else { '?' };
    // 授权页面通过 POST 表单提交，但客户端回调必须使用 GET。
    // 307 会保留 POST 并把表单体转发到 ChatGPT connector，导致 Bad Request。
    Redirect::to(&format!("{}{}{}", form.redirect_uri, sep, qs)).into_response()
}

pub fn token_exchange(
    oauth: &OAuthRuntime,
    headers: &HeaderMap,
    mut form: TokenForm,
    issuer_url: &str,
    canonical_resource_url: &str,
) -> Response {
    if let Some((id, secret)) = basic_auth_credentials(headers) {
        if form.client_id.is_empty() {
            form.client_id = id;
        }
        if form.client_secret.is_empty() {
            form.client_secret = secret;
        }
    }

    if !oauth.client_id_allowed(&form.client_id) {
        return token_error("invalid_client", "Unknown client_id");
    }
    if let Some(expected) = oauth.client_secret.as_deref() {
        if !constant_time_eq_str(&form.client_secret, expected) {
            return token_error("invalid_client", "Invalid client_secret");
        }
    }
    match form.grant_type.as_str() {
        "authorization_code" => exchange_authorization_code(
            oauth,
            form,
            issuer_url,
            canonical_resource_url,
        ),
        "refresh_token" => refresh_access_token(
            oauth,
            form,
            issuer_url,
            canonical_resource_url,
        ),
        _ => token_error(
            "unsupported_grant_type",
            "Supported grant types are authorization_code and refresh_token",
        ),
    }
}

fn exchange_authorization_code(
    oauth: &OAuthRuntime,
    form: TokenForm,
    issuer_url: &str,
    canonical_resource_url: &str,
) -> Response {
    if form.code.is_empty() {
        return token_error("invalid_grant", "code is required");
    }
    if !valid_code_verifier(&form.code_verifier) {
        return token_error("invalid_grant", "Invalid code_verifier");
    }

    let code_data = {
        let mut pending = oauth.pending.lock().expect("oauth pending lock");
        pending.remove(&form.code)
    };
    let Some(code_data) = code_data else {
        return token_error("invalid_grant", "Unknown or already-used authorization code");
    };
    if unix_now() > code_data.expires_at {
        return token_error("invalid_grant", "Authorization code expired");
    }
    if !constant_time_eq_str(&code_data.client_id, &form.client_id) {
        return token_error("invalid_grant", "client_id mismatch");
    }
    if !constant_time_eq_str(&code_data.redirect_uri, &form.redirect_uri) {
        return token_error("invalid_grant", "redirect_uri mismatch");
    }
    if !oauth.redirect_uri_allowed(&form.redirect_uri) {
        return token_error("invalid_grant", "redirect_uri is not registered");
    }
    if !verify_pkce(&form.code_verifier, &code_data.code_challenge) {
        return token_error("invalid_grant", "PKCE verification failed");
    }
    if !resource_matches(&form.resource, &code_data.resource_url)
        || !resource_matches(&code_data.resource_url, canonical_resource_url)
    {
        return token_error("invalid_target", "resource mismatch");
    }

    let issuer = if code_data.issuer_url.trim().is_empty() {
        issuer_url.trim_end_matches('/').to_string()
    } else {
        code_data.issuer_url.trim_end_matches('/').to_string()
    };
    match create_token_pair(
        &issuer,
        &code_data.resource_url,
        &oauth.token_secret,
        &form.client_id,
    ) {
        Ok(tokens) => token_success(tokens),
        Err(_) => token_error("server_error", "Failed to issue access token"),
    }
}

fn refresh_access_token(
    oauth: &OAuthRuntime,
    form: TokenForm,
    issuer_url: &str,
    canonical_resource_url: &str,
) -> Response {
    if form.refresh_token.trim().is_empty() {
        return token_error("invalid_grant", "refresh_token is required");
    }
    let issuer = issuer_url.trim_end_matches('/');
    let resource = canonical_resource_url.trim_end_matches('/');
    let claims = match decode_refresh_token(
        &form.refresh_token,
        &oauth.token_secret,
        issuer,
        resource,
    ) {
        Ok(claims) => claims,
        Err(message) => return token_error("invalid_grant", message),
    };
    if !claims.client_id.is_empty()
        && !constant_time_eq_str(&claims.client_id, &form.client_id)
    {
        return token_error("invalid_grant", "refresh token client_id mismatch");
    }
    if !resource_matches(&form.resource, resource) {
        return token_error("invalid_target", "resource mismatch");
    }
    if !form.scope.trim().is_empty() && form.scope.trim() != claims.scope {
        return token_error("invalid_scope", "refresh scope cannot be expanded");
    }

    let tokens = match create_token_pair(
        issuer,
        resource,
        &oauth.token_secret,
        &form.client_id,
    ) {
        Ok(tokens) => tokens,
        Err(_) => return token_error("server_error", "Failed to rotate refresh token"),
    };

    let now = unix_now();
    let mut used = oauth
        .used_refresh_tokens
        .lock()
        .expect("oauth refresh token lock");
    used.retain(|_, expires_at| *expires_at >= now);
    if claims.jti.is_empty() || used.contains_key(&claims.jti) {
        return token_error(
            "invalid_grant",
            "refresh token was already used; re-authorization is required",
        );
    }
    used.insert(claims.jti, claims.exp.max(0) as u64);
    drop(used);

    token_success(tokens)
}

struct TokenPair {
    access_token: String,
    refresh_token: String,
}

fn create_token_pair(
    issuer_url: &str,
    resource_url: &str,
    token_secret: &str,
    client_id: &str,
) -> Result<TokenPair, ()> {
    Ok(TokenPair {
        access_token: create_token(
            issuer_url,
            resource_url,
            token_secret,
            client_id,
            "access",
            OAUTH_TOKEN_TTL_SECONDS,
        )?,
        refresh_token: create_token(
            issuer_url,
            resource_url,
            token_secret,
            client_id,
            "refresh",
            OAUTH_REFRESH_TOKEN_TTL_SECONDS,
        )?,
    })
}

fn create_token(
    issuer_url: &str,
    resource_url: &str,
    token_secret: &str,
    client_id: &str,
    token_kind: &str,
    ttl: i64,
) -> Result<String, ()> {
    let now = unix_now() as i64;
    let claims = TokenClaims {
        iss: issuer_url.to_string(),
        aud: resource_url.to_string(),
        iat: now,
        exp: now + ttl,
        scope: "mcp".into(),
        token_kind: token_kind.into(),
        client_id: client_id.into(),
        jti: uuid::Uuid::new_v4().to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(token_secret.as_bytes()),
    )
    .map_err(|_| ())
}

fn verify_pkce(code_verifier: &str, code_challenge: &str) -> bool {
    let digest = Sha256::digest(code_verifier.as_bytes());
    let expected = URL_SAFE_NO_PAD.encode(digest);
    constant_time_eq_str(&expected, code_challenge)
}

fn valid_code_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~'))
}

fn basic_auth_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (id, secret) = text.split_once(':')?;
    Some((id.to_string(), secret.to_string()))
}

fn token_error(error: &str, description: &str) -> Response {
    token_json_response(
        StatusCode::BAD_REQUEST,
        json!({
            "error": error,
            "error_description": description
        }),
    )
}

fn token_json_response(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = (status, axum::Json(body)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn html_error(message: &str, status: StatusCode) -> Response {
    (status, Html(format!("<h2>Error</h2><p>{message}</p>"))).into_response()
}

#[allow(clippy::too_many_arguments)]
fn login_page(
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    code_challenge_method: &str,
    state: &str,
    resource: &str,
    error: &str,
    workspace_path: Option<&str>,
) -> String {
    let error_block = if error.is_empty() {
        String::new()
    } else {
        format!("<p style=\"color:red\">{}</p>", html_escape(error))
    };
    let workspace_block = workspace_path
        .filter(|path| !path.is_empty())
        .map(|path| format!("<p>Workspace: <code>{}</code></p>", html_escape(path)))
        .unwrap_or_default();
    format!(
        "<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'>\
        <title>Authorize MCP Server</title>\
        <style>body{{font-family:sans-serif;max-width:380px;margin:4rem auto;padding:1rem}}\
        input{{width:100%;padding:.5rem;margin:.4rem 0;box-sizing:border-box}}\
        button{{width:100%;padding:.7rem;background:#0066cc;color:#fff;border:none;cursor:pointer}}</style>\
        </head><body>\
        <h2>Authorize Coding Tools MCP</h2>\
        {workspace_block}\
        <p>Client: <strong>{}</strong></p>\
        <p>Redirect URI: <code>{}</code></p>\
        {error_block}\
        <form method='POST' action='/oauth/authorize'>\
        <input type='hidden' name='client_id' value='{}'>\
        <input type='hidden' name='redirect_uri' value='{}'>\
        <input type='hidden' name='code_challenge' value='{}'>\
        <input type='hidden' name='code_challenge_method' value='{}'>\
        <input type='hidden' name='state' value='{}'>\
        <input type='hidden' name='resource' value='{}'>\
        <label>Password<input type='password' name='password' autocomplete='current-password' required></label>\
        <button type='submit'>Authorize</button>\
        </form></body></html>",
        html_escape(client_id),
        html_escape(redirect_uri),
        html_escape(client_id),
        html_escape(redirect_uri),
        html_escape(code_challenge),
        html_escape(code_challenge_method),
        html_escape(state),
        html_escape(resource),
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&#39;")
}

fn urlencoding_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("response body");
        serde_json::from_slice(&bytes).expect("response json")
    }

    #[tokio::test]
    async fn token_exchange_without_client_secret_issues_rotating_refresh_token() {
        use axum::http::HeaderMap;

        let oauth = OAuthRuntime::new(
            "https://lb.example.com".into(),
            "chatgpt-client-test".into(),
            None,
            "test-password".into(),
            "token-signing-secret".into(),
        )
        .with_redirect_uris("https://chatgpt.com/connector/oauth/test")
        .expect("redirect URI config");
        let verifier = "dBjftJeZ4CVP-mB92Kpru-AEJvkQlLgi3ThpmQ45N_Xyo";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let redirect_uri = "https://chatgpt.com/connector/oauth/test";
        let resource_url = "https://lb.example.com/mcp";
        let redirect = authorize_post(
            &oauth,
            AuthorizeForm {
                client_id: "chatgpt-client-test".into(),
                redirect_uri: redirect_uri.into(),
                code_challenge: challenge,
                code_challenge_method: "S256".into(),
                state: "state".into(),
                resource: resource_url.into(),
                password: "test-password".into(),
            },
            "https://lb.example.com",
            resource_url,
        );
        assert_eq!(redirect.status(), StatusCode::SEE_OTHER);
        let code = {
            let pending = oauth.pending.lock().expect("lock");
            pending.keys().next().cloned().unwrap()
        };

        let response = token_exchange(
            &oauth,
            &HeaderMap::new(),
            TokenForm {
                grant_type: "authorization_code".into(),
                code,
                redirect_uri: redirect_uri.into(),
                code_verifier: verifier.into(),
                client_id: "chatgpt-client-test".into(),
                client_secret: String::new(),
                resource: resource_url.into(),
                ..TokenForm::default()
            },
            "https://lb.example.com",
            resource_url,
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[PRAGMA], "no-cache");
        let issued = response_json(response).await;
        let access_token = issued["access_token"].as_str().expect("access token");
        let refresh_token = issued["refresh_token"].as_str().expect("refresh token");
        assert!(oauth.verify_access_token(
            access_token,
            "https://lb.example.com",
            resource_url
        ));
        assert!(!oauth.verify_access_token(
            refresh_token,
            "https://lb.example.com",
            resource_url
        ));

        let refreshed_response = token_exchange(
            &oauth,
            &HeaderMap::new(),
            TokenForm {
                grant_type: "refresh_token".into(),
                client_id: "chatgpt-client-test".into(),
                resource: resource_url.into(),
                refresh_token: refresh_token.into(),
                ..TokenForm::default()
            },
            "https://lb.example.com",
            resource_url,
        );
        assert_eq!(refreshed_response.status(), StatusCode::OK);
        let refreshed = response_json(refreshed_response).await;
        assert_ne!(refreshed["refresh_token"], issued["refresh_token"]);

        let replay = token_exchange(
            &oauth,
            &HeaderMap::new(),
            TokenForm {
                grant_type: "refresh_token".into(),
                client_id: "chatgpt-client-test".into(),
                resource: resource_url.into(),
                refresh_token: refresh_token.into(),
                ..TokenForm::default()
            },
            "https://lb.example.com",
            resource_url,
        );
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
        let replay_body = response_json(replay).await;
        assert_eq!(replay_body["error"], "invalid_grant");
        assert!(replay_body["error_description"]
            .as_str()
            .expect("description")
            .contains("already used"));
    }

    #[test]
    fn missing_bearer_token_advertises_protected_resource_metadata() {
        let oauth = OAuthRuntime::new(
            "https://lb.example.com".into(),
            "chatgpt-client-test".into(),
            None,
            "test-password".into(),
            "token-signing-secret".into(),
        );
        let response = verify_oauth_bearer_header(
            &HeaderMap::new(),
            &oauth,
            "https://lb.example.com",
            "https://lb.example.com/mcp",
            "https://lb.example.com/.well-known/oauth-protected-resource",
        )
        .expect("missing token should be rejected");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers()[WWW_AUTHENTICATE],
            "Bearer scope=\"mcp\", resource_metadata=\"https://lb.example.com/.well-known/oauth-protected-resource\""
        );
    }

    #[test]
    fn redirect_uri_registration_is_exact_and_secure() {
        let oauth = OAuthRuntime::new(
            "https://lb.example.com".into(),
            "chatgpt-client-test".into(),
            None,
            "test-password".into(),
            "token-signing-secret".into(),
        )
        .with_redirect_uris(
            "https://chatgpt.com/connector/oauth/callback-1\nhttp://127.0.0.1:3000/callback",
        )
        .expect("redirect URI config");

        assert!(oauth.redirect_uri_allowed(
            "https://chatgpt.com/connector/oauth/callback-1"
        ));
        assert!(!oauth.redirect_uri_allowed(
            "https://chatgpt.com/connector/oauth/callback-2"
        ));
        assert!(parse_redirect_uris("https://*.example.com/callback").is_err());
        assert!(parse_redirect_uris("http://example.com/callback").is_err());
        assert!(parse_redirect_uris("http://localhost:3000/callback").is_ok());
    }

    #[test]
    fn pkce_round_trip() {
        let verifier = "dBjftJeZ4CVP-mB92Kpru-AEJvkQlLgi3ThpmQ45N_Xyo";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert!(verify_pkce(verifier, &challenge));
    }
}
