use std::path::PathBuf;
use std::sync::Arc;

use crate::auth::{
    authorization_server_metadata, authorize_get, authorize_post, external_base_url,
    protected_resource_metadata, redirect_uri_log_label, register_oauth_runtime,
    request_origin_allowed, token_exchange, AuthorizeForm, AuthorizeParams, OAuthRuntime,
    TokenForm, OAUTH_MAX_BODY_BYTES,
};
use crate::mcp::protocol::RateLimiter;
use crate::runtime::{read_public_url, register_public_url, SharedPublicUrl};
use crate::tools::{self, is_allowed_tool, policy::PolicySettings, wrap_tool_result, ToolContext};
use crate::tunnel::append_profile_log;
use axum::{
    extract::rejection::JsonRejection,
    extract::{DefaultBodyLimit, Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware,
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
    Extension, Router,
};
use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex, RwLock, Semaphore};

use super::auth::{require_actions_auth, AuthConfig};
use super::openapi;

pub type ShutdownSender = oneshot::Sender<()>;

const ACTIONS_MAX_BODY_BYTES: usize = 1_048_576;
const ACTIONS_MAX_REQUESTS_PER_MINUTE: usize = 240;
const OAUTH_MAX_REQUESTS_PER_MINUTE: usize = 30;
const ACTIONS_MAX_CONCURRENT_REQUESTS: usize = 16;

#[derive(Clone)]
struct AppState {
    ctx: Arc<ToolContext>,
    openapi: Arc<RwLock<Value>>,
    auth: Arc<AuthConfig>,
    workspace_name: String,
    workspace_path: String,
    workspace_id: String,
    bind_port: u16,
    configured_public_url: SharedPublicUrl,
    oauth: Option<Arc<OAuthRuntime>>,
    oauth_client_secret: Option<String>,
    write_lock: Arc<Mutex<()>>,
    action_rate_limiter: RateLimiter,
    oauth_rate_limiter: RateLimiter,
    concurrency: Arc<Semaphore>,
}

fn actions_http_error(status: StatusCode, detail: &str) -> Response {
    (status, Json(json!({ "detail": detail }))).into_response()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_listener_with_handoff(
    workspace_id: &str,
    workspace_name: String,
    actions_port: u16,
    workspace_path: PathBuf,
    public_base_url: String,
    auth_type: String,
    api_key: Option<String>,
    oauth_client_id: String,
    oauth_redirect_uris: String,
    oauth_redirect_hosts: String,
    oauth_client_secret: Option<String>,
    oauth_password: Option<String>,
    oauth_token_secret: Option<String>,
    policy: PolicySettings,
) -> Result<
    (
        ShutdownSender,
        crate::async_runtime::JoinHandle<()>,
        crate::runtime::HandoffListener,
    ),
    String,
> {
    if auth_type == "api_key" && api_key.as_ref().is_none_or(String::is_empty) {
        return Err("Actions API key is not configured".into());
    }
    if auth_type == "oauth" {
        if oauth_client_id.trim().is_empty() {
            return Err("Actions OAuth client_id is not configured".into());
        }
        if oauth_password.as_ref().is_none_or(String::is_empty) {
            return Err("Actions OAuth password is not configured".into());
        }
        if oauth_token_secret.as_ref().is_none_or(String::is_empty) {
            return Err("Actions OAuth token secret is not configured".into());
        }
    }

    let configured_public_url =
        register_public_url(workspace_id, "actions", public_base_url.trim().to_string());
    let oauth = if auth_type == "oauth" {
        let oauth_base = external_base_url(
            &HeaderMap::new(),
            actions_port,
            &read_public_url(&configured_public_url),
        );
        Some(Arc::new(
            OAuthRuntime::new(
                oauth_base,
                oauth_client_id,
                oauth_client_secret.clone(),
                oauth_password.unwrap_or_default(),
                oauth_token_secret.unwrap_or_default(),
            )
            .with_refresh_replay_key(format!("{workspace_id}:actions"))
            .with_redirect_uris(&oauth_redirect_uris)?
            .with_redirect_host_patterns(&oauth_redirect_hosts)?,
        ))
    } else {
        None
    };
    if let Some(runtime) = oauth.as_ref() {
        register_oauth_runtime(workspace_id, "actions", runtime);
    }

    // 在返回 Running 之前完成 bind，避免后台任务里的端口冲突被伪装成启动成功。
    let (listener, handoff) = bind_listener(actions_port)?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let profile_id = workspace_id.to_string();
    let handle = crate::async_runtime::spawn(async move {
        let result = serve(
            listener,
            actions_port,
            &profile_id,
            workspace_name,
            workspace_path,
            configured_public_url,
            auth_type,
            api_key,
            oauth,
            oauth_client_secret,
            policy,
            shutdown_rx,
        )
        .await;
        if let Err(err) = &result {
            append_profile_log(
                &profile_id,
                "actions-stderr.log",
                &format!("[actions] listener stopped: {err}"),
            );
            eprintln!("actions listener stopped: {err}");
        } else {
            append_profile_log(
                &profile_id,
                "actions-stderr.log",
                "[actions] listener stopped",
            );
        }
    });
    Ok((shutdown_tx, handle, handoff))
}

#[allow(clippy::too_many_arguments)]
async fn serve(
    listener: tokio::net::TcpListener,
    actions_port: u16,
    profile_id: &str,
    workspace_name: String,
    workspace_path: PathBuf,
    configured_public_url: SharedPublicUrl,
    auth_type: String,
    api_key: Option<String>,
    oauth: Option<Arc<OAuthRuntime>>,
    oauth_client_secret: Option<String>,
    policy: PolicySettings,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = tools::Workspace::new(workspace_path.clone()).map_err(|e| e.message())?;
    let ctx = Arc::new(ToolContext::from_workspace(
        workspace,
        crate::workspace::AuthConfig {
            auth_type: auth_type.clone(),
            ..crate::workspace::AuthConfig::default()
        },
        policy.clone(),
        "core".into(),
        policy.permission_mode.clone(),
    ));
    let effective_catalog = tools::build_effective_catalog_from_parts("core", false, Vec::new())
        .map_err(|error| std::io::Error::other(error.message()))?;
    let tools: Vec<Value> = effective_catalog
        .tools
        .into_iter()
        .filter(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .map(is_allowed_tool)
                .unwrap_or(false)
        })
        .collect();
    let current_public_url = read_public_url(&configured_public_url);
    let public_base_url = if current_public_url.is_empty() {
        format!("http://127.0.0.1:{actions_port}")
    } else {
        current_public_url
    };
    let openapi_doc = openapi::build_openapi(&tools, &public_base_url, &auth_type);

    let auth = Arc::new(AuthConfig::new(
        auth_type,
        api_key,
        oauth.clone(),
        actions_port,
        configured_public_url.clone(),
    ));

    let state = AppState {
        workspace_name,
        workspace_path: ctx.workspace_path(),
        workspace_id: profile_id.to_string(),
        ctx,
        openapi: Arc::new(RwLock::new(openapi_doc)),
        auth: auth.clone(),
        bind_port: actions_port,
        configured_public_url,
        oauth,
        oauth_client_secret,
        write_lock: Arc::new(Mutex::new(())),
        action_rate_limiter: RateLimiter::new(
            ACTIONS_MAX_REQUESTS_PER_MINUTE,
            std::time::Duration::from_secs(60),
        ),
        oauth_rate_limiter: RateLimiter::new(
            OAUTH_MAX_REQUESTS_PER_MINUTE,
            std::time::Duration::from_secs(60),
        ),
        concurrency: Arc::new(Semaphore::new(ACTIONS_MAX_CONCURRENT_REQUESTS)),
    };

    let protected = Router::new()
        .route("/actions/{tool_name}", post(execute_action))
        .layer(middleware::from_fn(require_actions_auth))
        .layer(Extension(auth))
        .layer(DefaultBodyLimit::max(ACTIONS_MAX_BODY_BYTES));

    let oauth_routes = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_server_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource_metadata),
        )
        .route(
            "/oauth/authorize",
            get(oauth_authorize_get).post(oauth_authorize_post),
        )
        .route("/oauth/token", post(oauth_token_post))
        .layer(DefaultBodyLimit::max(OAUTH_MAX_BODY_BYTES));
    let app = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi_json))
        .route("/privacy", get(privacy))
        .merge(oauth_routes)
        .merge(protected)
        .with_state(state);

    append_profile_log(
        profile_id,
        "actions-stdout.log",
        &format!(
            "[actions] listening on http://127.0.0.1:{actions_port} (public: {public_base_url})"
        ),
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown.await;
        })
        .await?;
    Ok(())
}

fn bind_listener(
    port: u16,
) -> Result<(tokio::net::TcpListener, crate::runtime::HandoffListener), String> {
    crate::runtime::bind_loopback_listener(port)
        .map_err(|err| format!("Actions 本地端口 {port} 绑定失败: {err}"))
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let tools_loaded = state
        .openapi
        .read()
        .await
        .get("paths")
        .and_then(Value::as_object)
        .map(|paths| paths.len())
        .unwrap_or(0);

    Json(json!({
        "ok": true,
        "service": crate::brand::ACTIONS_SERVER_NAME,
        "workspace": state.workspace_path,
        "auth_type": state.auth.auth_type,
        "tools_loaded": tools_loaded
    }))
}

async fn openapi_json(State(state): State<AppState>) -> Json<Value> {
    let document = state.openapi.read().await.clone();
    Json(openapi_with_current_public_url(
        document,
        state.bind_port,
        &state.configured_public_url,
    ))
}

fn openapi_with_current_public_url(
    mut document: Value,
    bind_port: u16,
    configured_public_url: &SharedPublicUrl,
) -> Value {
    let configured = read_public_url(configured_public_url);
    let public_url = if configured.is_empty() {
        format!("http://127.0.0.1:{bind_port}")
    } else {
        configured
    };
    if let Some(server) = document
        .get_mut("servers")
        .and_then(Value::as_array_mut)
        .and_then(|servers| servers.first_mut())
        .and_then(Value::as_object_mut)
    {
        server.insert("url".into(), Value::String(public_url));
    }
    document
}

async fn privacy() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8">
    <title>Anchor Actions Privacy</title>
  </head>
  <body>
    <h1>隐私政策</h1>
    <p>本服务仅供仓库所有者本人使用。</p>
    <p>请求内容只用于执行用户主动发起的代码操作。</p>
    <p>服务不会出售或共享请求数据。</p>
    <p>API 密钥、GitHub 令牌和环境变量不会返回给模型。</p>
  </body>
</html>"#,
    )
}

fn resolve_oauth_base(state: &AppState, headers: &HeaderMap) -> String {
    external_base_url(
        headers,
        state.bind_port,
        &read_public_url(&state.configured_public_url),
    )
}

fn origin_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    request_origin_allowed(
        headers,
        state.bind_port,
        &read_public_url(&state.configured_public_url),
    )
}

fn resolve_oauth_resource(state: &AppState, headers: &HeaderMap) -> String {
    resolve_oauth_base(state, headers)
        .trim_end_matches('/')
        .to_string()
}

async fn oauth_authorization_server_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.oauth_rate_limiter.allow() {
        return actions_http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth request rate limit exceeded",
        );
    }
    if !state.auth.oauth_enabled() {
        return oauth_not_configured();
    }
    Json(authorization_server_metadata(
        &resolve_oauth_base(&state, &headers),
        state.oauth_client_secret.as_deref(),
    ))
    .into_response()
}

async fn oauth_protected_resource_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.oauth_rate_limiter.allow() {
        return actions_http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth request rate limit exceeded",
        );
    }
    if !state.auth.oauth_enabled() {
        return oauth_not_configured();
    }
    let authorization_server = resolve_oauth_base(&state, &headers);
    let resource = resolve_oauth_resource(&state, &headers);
    Json(protected_resource_metadata(
        &resource,
        &authorization_server,
    ))
    .into_response()
}

async fn oauth_authorize_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.oauth_rate_limiter.allow() {
        return actions_http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth request rate limit exceeded",
        );
    }
    let Some(oauth) = state.oauth.as_ref() else {
        return oauth_not_configured();
    };
    let redirect_label = redirect_uri_log_label(&params.redirect_uri);
    let redirect_status = oauth.redirect_uri_status_label(&params.redirect_uri);
    append_profile_log(
        &state.workspace_id,
        "actions-oauth.log",
        &format!(
            "[oauth] event=authorize_received method=GET {redirect_label} redirect_status={redirect_status}"
        ),
    );
    let response = authorize_get(
        oauth,
        params,
        &resolve_oauth_resource(&state, &headers),
        Some(state.workspace_name.as_str()),
    );
    append_profile_log(
        &state.workspace_id,
        "actions-oauth.log",
        &format!(
            "[oauth] event=authorize_page_result method=GET status={} {redirect_label}",
            response.status().as_u16()
        ),
    );
    response
}

async fn oauth_authorize_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AuthorizeForm>,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.oauth_rate_limiter.allow() {
        return actions_http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth request rate limit exceeded",
        );
    }
    let Some(oauth) = state.oauth.as_ref() else {
        return oauth_not_configured();
    };
    let redirect_label = redirect_uri_log_label(&form.redirect_uri);
    let before_status = oauth.redirect_uri_status_label(&form.redirect_uri);
    let redirect_uri = form.redirect_uri.clone();
    append_profile_log(
        &state.workspace_id,
        "actions-oauth.log",
        &format!(
            "[oauth] event=authorize_submitted method=POST {redirect_label} redirect_status={before_status}"
        ),
    );
    let response = authorize_post(
        oauth,
        form,
        &resolve_oauth_base(&state, &headers),
        &resolve_oauth_resource(&state, &headers),
    );
    let after_status = oauth.redirect_uri_status_label(&redirect_uri);
    append_profile_log(
        &state.workspace_id,
        "actions-oauth.log",
        &format!(
            "[oauth] event=authorize_result method=POST status={} {redirect_label} redirect_status_after={after_status}",
            response.status().as_u16()
        ),
    );
    response
}

async fn oauth_token_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.oauth_rate_limiter.allow() {
        return actions_http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth request rate limit exceeded",
        );
    }
    let Some(oauth) = state.oauth.as_ref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unsupported_grant_type" })),
        )
            .into_response();
    };
    let grant_type = form.grant_type.clone();
    let redirect_label = if form.redirect_uri.trim().is_empty() {
        "redirect_host=none redirect_sha256=none".to_string()
    } else {
        redirect_uri_log_label(&form.redirect_uri)
    };
    append_profile_log(
        &state.workspace_id,
        "actions-oauth.log",
        &format!("[oauth] event=token_exchange_received grant_type={grant_type} {redirect_label}"),
    );
    let response = token_exchange(
        oauth,
        &headers,
        form,
        &resolve_oauth_base(&state, &headers),
        &resolve_oauth_resource(&state, &headers),
    );
    append_profile_log(
        &state.workspace_id,
        "actions-oauth.log",
        &format!(
            "[oauth] event=token_exchange_result grant_type={grant_type} status={} {redirect_label}",
            response.status().as_u16()
        ),
    );
    response
}

fn oauth_not_configured() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "OAuth not configured" })),
    )
        .into_response()
}

async fn execute_action(
    State(state): State<AppState>,
    Path(tool_name): Path<String>,
    body: Result<Option<Json<Value>>, JsonRejection>,
) -> Response {
    if !state.action_rate_limiter.allow() {
        return actions_http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Actions request rate limit exceeded",
        );
    }
    let _permit = match state.concurrency.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return actions_http_error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many concurrent Actions requests",
            )
        }
    };
    let body = match body {
        Ok(body) => body,
        Err(error) => {
            return actions_http_error(
                StatusCode::BAD_REQUEST,
                &format!("Invalid JSON request body: {}", error.body_text()),
            )
        }
    };
    let arguments = match body {
        Some(Json(value)) if value.is_object() || value.is_null() => {
            if value.is_null() {
                json!({})
            } else {
                value
            }
        }
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "detail": "Request body must be a JSON object" })),
            )
                .into_response();
        }
        None => json!({}),
    };

    if let Err(err) = tools::policy::validate_actions_exposure(&tool_name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "detail": err.to_string() })),
        )
            .into_response();
    }

    let structured = if tools::registry::MUTATING_TOOLS.contains(&tool_name.as_str()) {
        let _guard = state.write_lock.lock().await;
        tools::call_tool(state.ctx.as_ref(), &tool_name, &arguments)
    } else {
        tools::call_tool(state.ctx.as_ref(), &tool_name, &arguments)
    };
    let result = wrap_tool_result(structured);
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status = if is_error {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(json!({
            "ok": !is_error,
            "tool": tool_name,
            "structured_content": result.get("structuredContent").cloned().unwrap_or(Value::Null),
            "content": result.get("content").cloned().unwrap_or_else(|| json!([])),
            "is_error": is_error
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::runtime::{register_public_url, update_public_url};

    use super::{bind_listener, openapi_with_current_public_url};

    #[test]
    fn bind_listener_reports_port_conflict_synchronously() {
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("占用测试端口");
        let port = occupied.local_addr().expect("读取测试端口").port();

        assert!(bind_listener(port).is_err());
    }

    #[test]
    fn openapi_public_url_hot_updates_without_listener_restart() {
        let workspace_id = format!("actions-test-{}", uuid::Uuid::new_v4());
        let public_url =
            register_public_url(&workspace_id, "actions", "https://old.example.com".into());
        let document = json!({ "servers": [{ "url": "http://127.0.0.1:28767" }] });
        assert_eq!(
            openapi_with_current_public_url(document.clone(), 28767, &public_url)["servers"][0]
                ["url"],
            "https://old.example.com"
        );
        assert!(update_public_url(
            &workspace_id,
            "actions",
            "https://new.example.com"
        ));
        assert_eq!(
            openapi_with_current_public_url(document, 28767, &public_url)["servers"][0]["url"],
            "https://new.example.com"
        );
    }
}
