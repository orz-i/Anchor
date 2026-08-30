use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::auth::{
    authorization_server_metadata, authorize_get, authorize_post, bearer_token,
    constant_time_eq_str, external_base_url, protected_resource_metadata, redirect_uri_log_label,
    register_oauth_runtime, request_origin_allowed, token_exchange, verify_bearer_header,
    verify_oauth_bearer_header, AuthorizeForm, AuthorizeParams, OAuthRuntime, TokenForm,
    OAUTH_MAX_BODY_BYTES,
};
use crate::mcp::protocol::{
    requested_protocol_version, require_current_protocol_version, validate_client_message,
    ClientMessage, InFlightRequests, InFlightReservation, KeyedRateLimiter, LegacyMcpSessionStore,
    RateLimiter, RequestReservation,
};
use crate::mcp::proxy::{parse_mcp_proxy_config, McpProxyServerSpec};
use crate::mcp::server::{
    handle_request_with_protocol_session_and_cancellation, new_state, SharedState,
};
use crate::mcp::{register_activity, McpActivityTracker};
use crate::runtime::{read_public_url, register_public_url, SharedPublicUrl};
use crate::secret::SecretStore;
use crate::tools::policy::PolicySettings;
use crate::tools::Workspace;
use crate::tunnel::append_profile_log;
use crate::workspace::{AuthConfig, RuntimeConfig};
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Form, Path, Query, State};
use axum::http::{
    header::{ACCEPT, ALLOW, AUTHORIZATION, CACHE_CONTROL, WWW_AUTHENTICATE},
    HeaderMap, HeaderValue, StatusCode,
};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{oneshot, Semaphore};

pub type ShutdownSender = oneshot::Sender<()>;

const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
const MCP_MAX_BODY_BYTES: usize = 1_048_576;
const MCP_MAX_CONCURRENT_REQUESTS: usize = 16;
const MCP_MAX_CONCURRENT_REQUESTS_PER_SESSION: usize = 4;
const MCP_MAX_REQUESTS_PER_MINUTE: usize = 240;
const MCP_MAX_REQUESTS_PER_IDENTITY_PER_MINUTE: usize = 120;
const OAUTH_MAX_REQUESTS_PER_MINUTE: usize = 30;
const OAUTH_MAX_REQUESTS_PER_CLIENT_PER_MINUTE: usize = 12;

#[derive(Clone)]
struct ListenerState {
    mcp: SharedState,
    auth: AuthConfig,
    workspace_id: String,
    workspace_name: String,
    workspace_path: PathBuf,
    bind_port: u16,
    configured_public_url: SharedPublicUrl,
    bearer_token: Option<String>,
    oauth: Option<Arc<OAuthRuntime>>,
    oauth_client_secret: Option<String>,
    proxy_specs: Vec<McpProxyServerSpec>,
    sessions: LegacyMcpSessionStore,
    mcp_rate_limiter: RateLimiter,
    mcp_identity_rate_limiter: KeyedRateLimiter,
    oauth_rate_limiter: RateLimiter,
    oauth_identity_rate_limiter: KeyedRateLimiter,
    concurrency: Arc<Semaphore>,
    in_flight: InFlightRequests,
    activity: McpActivityTracker,
}

#[derive(Clone)]
pub(crate) struct McpHandoffReadiness {
    sessions: LegacyMcpSessionStore,
    mcp: SharedState,
    in_flight: InFlightRequests,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpHandoffSnapshot {
    transport_sessions: crate::mcp::protocol::LegacyMcpSessionStoreSnapshot,
    tool_context: crate::tools::context::ToolContextHandoffSnapshot,
}

impl McpHandoffReadiness {
    pub(crate) fn cancel_in_flight(&self) -> usize {
        self.in_flight.cancel_all()
    }

    pub(crate) fn snapshot_for_handoff(
        &self,
        initiator_pid: u32,
    ) -> Result<McpHandoffSnapshot, String> {
        let initiator_transport_session =
            self.mcp.sessions.prepare_daemon_handoff(initiator_pid)?;
        let active_transport_sessions = self.sessions.active_session_count();
        let initiator_is_active = initiator_transport_session
            .as_deref()
            .is_some_and(|session_id| self.sessions.inspect(session_id).is_some());
        let supported_transport_shape = active_transport_sessions == 0
            || (active_transport_sessions == 1 && initiator_is_active);
        if !supported_transport_shape {
            return Err(format!(
                "zero-downtime handoff currently supports no active MCP transport session or only the upgrade-initiating session: active_transport_sessions={active_transport_sessions}"
            ));
        }
        let non_initiator_requests = self
            .in_flight
            .count_excluding_session(initiator_transport_session.as_deref());
        if non_initiator_requests != 0 {
            return Err(format!(
                "zero-downtime handoff is blocked by concurrent MCP requests: non_initiator_in_flight={non_initiator_requests}"
            ));
        }
        Ok(McpHandoffSnapshot {
            transport_sessions: self.sessions.handoff_snapshot(),
            tool_context: self.mcp.handoff_snapshot(),
        })
    }
}

fn clear_session_associations(state: &ListenerState, session_id: &str) {
    state.in_flight.cancel_session(session_id);
    state.activity.cancel_session(session_id);
    state.mcp.clear_session_state(session_id);
}

fn remove_session(state: &ListenerState, session_id: &str) -> bool {
    let removed = state.sessions.remove(session_id);
    clear_session_associations(state, session_id);
    removed
}

fn cleanup_retired_sessions(state: &ListenerState) {
    for session_id in state.sessions.take_retired() {
        clear_session_associations(state, &session_id);
    }
}

fn accepts_streamable_http(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(ACCEPT).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let accepted = value
        .split(',')
        .map(|item| {
            item.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .collect::<Vec<_>>();
    accepted.iter().any(|item| item == "*/*")
        || (accepted.iter().any(|item| item == "application/json")
            && accepted.iter().any(|item| item == "text/event-stream"))
}

fn bounded_identity(prefix: &str, value: &[u8]) -> String {
    let digest = format!("{:x}", Sha256::digest(value));
    format!("{prefix}:{}", &digest[..16])
}

fn cursor_identity(prefix: &str, value: &[u8]) -> String {
    format!("{prefix}:{:x}", Sha256::digest(value))
}

fn mcp_request_identity(headers: &HeaderMap) -> String {
    if let Some(session_id) = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
    {
        return bounded_identity("session", session_id.as_bytes());
    }
    if let Some(authorization) = headers.get("authorization") {
        return bounded_identity("authorization", authorization.as_bytes());
    }
    "anonymous".into()
}

fn mcp_cursor_scope(state: &ListenerState, headers: &HeaderMap) -> Option<String> {
    let token = bearer_token(headers).ok()?;
    if state.auth.oauth_enabled() {
        let oauth = state.oauth.as_ref()?;
        let issuer_url = resolve_oauth_base(state, headers);
        let resource_url = resolve_oauth_resource(state, headers);
        if let Some(principal) = oauth.access_token_principal(token, &issuer_url, &resource_url) {
            return Some(cursor_identity("oauth-principal", principal.as_bytes()));
        }
        // Access tokens issued before principal_id was introduced remain valid until
        // rotation. Isolate them by token rather than dropping cursor continuity.
        return oauth
            .verify_access_token(token, &issuer_url, &resource_url)
            .then(|| cursor_identity("oauth-token", token.as_bytes()));
    }
    state
        .auth
        .bearer_enabled()
        .then(|| cursor_identity("bearer", token.as_bytes()))
}

fn oauth_client_identity(client_id: &str) -> String {
    let value = client_id.trim();
    if value.is_empty() {
        "anonymous-oauth-client".into()
    } else {
        bounded_identity("oauth-client", value.as_bytes())
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_listener_with_handoff(
    port: u16,
    workspace_path: PathBuf,
    workspace_id: String,
    workspace_name: String,
    auth: AuthConfig,
    public_base_url: String,
    oauth_client_secret: Option<String>,
    oauth_password: Option<String>,
    oauth_token_secret: Option<String>,
    runtime: RuntimeConfig,
    imported_listener: Option<crate::runtime::HandoffListener>,
    handoff_snapshot: Option<McpHandoffSnapshot>,
) -> Result<
    (
        ShutdownSender,
        crate::async_runtime::JoinHandle<()>,
        crate::runtime::HandoffListener,
        McpHandoffReadiness,
    ),
    String,
> {
    let canvs_workspace_path = workspace_path.clone();
    let proxy_specs = parse_mcp_proxy_config(&runtime.mcp_config, &workspace_path)?;
    let allow_external_reads =
        !runtime.strict_workspace_reads && runtime.permission_mode == "dangerous";
    let workspace = Workspace::new(workspace_path)
        .map_err(|e| e.message())?
        .with_strict_read_boundary(!allow_external_reads);
    let policy = PolicySettings::from_runtime(&runtime);
    let mcp = new_state(
        workspace,
        auth.clone(),
        policy,
        runtime.tool_profile.clone(),
        runtime.permission_mode.clone(),
        public_base_url.clone(),
    );
    mcp.skills
        .configure(crate::skills::SkillSettings::from_text(
            runtime.skill_service_enabled,
            &runtime.skill_roots,
        ));
    let bearer_token = if auth.bearer_enabled() {
        let key = "bearer_token";
        if auth.use_shared_secrets {
            SecretStore::get_shared(key).map_err(|e| e.to_string())?
        } else {
            SecretStore::get(&workspace_id, key).map_err(|e| e.to_string())?
        }
    } else {
        None
    };
    if auth.bearer_enabled()
        && bearer_token
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err("MCP Bearer token is not configured".into());
    }

    if auth.oauth_enabled() {
        if auth.oauth_client_id.trim().is_empty() {
            return Err("MCP OAuth client_id is not configured".into());
        }
        if oauth_password.as_ref().is_none_or(|value| value.is_empty()) {
            return Err("MCP OAuth authorization password is not configured".into());
        }
        if oauth_token_secret
            .as_ref()
            .is_none_or(|value| value.is_empty())
        {
            return Err("MCP OAuth token signing secret is not configured".into());
        }
    }
    let configured_public_url =
        register_public_url(&workspace_id, "mcp", public_base_url.trim().to_string());
    let oauth = if auth.oauth_enabled() {
        let password = oauth_password.unwrap_or_default();
        let token_secret = oauth_token_secret.unwrap_or_default();
        let oauth_base = external_base_url(
            &HeaderMap::new(),
            port,
            &read_public_url(&configured_public_url),
        );
        Some(Arc::new(
            OAuthRuntime::new(
                oauth_base,
                auth.oauth_client_id.clone(),
                oauth_client_secret.clone(),
                password,
                token_secret,
            )
            .with_refresh_replay_key(format!("{workspace_id}:mcp"))
            .with_redirect_uris(&auth.oauth_redirect_uris)?
            .with_redirect_host_patterns(&auth.oauth_redirect_hosts)?,
        ))
    } else {
        None
    };
    if let Some(runtime) = oauth.as_ref() {
        register_oauth_runtime(&workspace_id, "mcp", runtime);
    }
    let activity = register_activity(&workspace_id);
    let sessions = LegacyMcpSessionStore::default();
    if let Some(snapshot) = handoff_snapshot {
        sessions.restore_handoff_snapshot(snapshot.transport_sessions);
        mcp.restore_handoff_snapshot(snapshot.tool_context);
    }
    let in_flight = InFlightRequests::default();
    let handoff_readiness = McpHandoffReadiness {
        sessions: sessions.clone(),
        mcp: mcp.clone(),
        in_flight: in_flight.clone(),
    };
    let state = ListenerState {
        mcp,
        auth,
        workspace_id,
        workspace_name,
        workspace_path: canvs_workspace_path,
        bind_port: port,
        configured_public_url,
        bearer_token,
        oauth,
        oauth_client_secret,
        proxy_specs,
        sessions,
        mcp_rate_limiter: RateLimiter::new(MCP_MAX_REQUESTS_PER_MINUTE, Duration::from_secs(60)),
        mcp_identity_rate_limiter: KeyedRateLimiter::new(
            MCP_MAX_REQUESTS_PER_IDENTITY_PER_MINUTE,
            1024,
            Duration::from_secs(60),
        ),
        oauth_rate_limiter: RateLimiter::new(
            OAUTH_MAX_REQUESTS_PER_MINUTE,
            Duration::from_secs(60),
        ),
        oauth_identity_rate_limiter: KeyedRateLimiter::new(
            OAUTH_MAX_REQUESTS_PER_CLIENT_PER_MINUTE,
            1024,
            Duration::from_secs(60),
        ),
        concurrency: Arc::new(Semaphore::new(MCP_MAX_CONCURRENT_REQUESTS)),
        in_flight,
        activity,
    };
    // 在返回 Running 之前完成 bind，避免后台任务里的端口冲突被伪装成启动成功。
    let (listener, handoff) = match imported_listener {
        Some(listener) => listener
            .activate()
            .map_err(|err| format!("MCP handoff listener 激活失败: {err}"))?,
        None => bind_listener(port)?,
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let profile_id = state.workspace_id.clone();
    let handle = crate::async_runtime::spawn(async move {
        let result = serve(listener, port, state, shutdown_rx).await;
        if let Err(err) = &result {
            append_profile_log(
                &profile_id,
                "stderr.log",
                &format!("[mcp] listener stopped: {err}"),
            );
            eprintln!("mcp listener stopped: {err}");
        } else {
            append_profile_log(&profile_id, "stderr.log", "[mcp] listener stopped");
        }
    });
    Ok((shutdown_tx, handle, handoff, handoff_readiness))
}

async fn serve(
    listener: tokio::net::TcpListener,
    port: u16,
    state: ListenerState,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let profile_id = state.workspace_id.clone();
    let proxy_registry = state.mcp.mcp_proxies.clone();
    let proxy_specs = state.proxy_specs.clone();
    let proxy_profile_id = profile_id.clone();
    proxy_registry.begin_configuration();
    tokio::spawn(async move {
        proxy_registry
            .configure(proxy_specs, &proxy_profile_id)
            .await;
    });
    let mcp_routes = Router::new()
        .route(
            "/mcp",
            get(mcp_get_not_supported).post(mcp_post).delete(mcp_delete),
        )
        .layer(DefaultBodyLimit::max(MCP_MAX_BODY_BYTES));
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
    let canvs_routes = Router::new()
        .route("/canvs", get(canvs_task_list_page))
        .route("/canvs/", get(canvs_task_list_page))
        .route("/canvs/tasks/{task_id}", get(canvs_task_detail_page))
        .route("/canvs/api/tasks", get(canvs_task_list_json))
        .route("/canvs/api/tasks/{task_id}", get(canvs_task_detail_json));
    let app = Router::new()
        .merge(mcp_routes)
        .merge(oauth_routes)
        .merge(canvs_routes)
        .with_state(state);

    append_profile_log(
        &profile_id,
        "stdout.log",
        &format!("[mcp] listening on http://127.0.0.1:{port}/mcp"),
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
        .map_err(|err| format!("MCP 本地端口 {port} 绑定失败: {err}"))
}

async fn mcp_get_not_supported(State(state): State<ListenerState>, headers: HeaderMap) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    mcp_method_not_allowed_response()
}

fn mcp_method_not_allowed_response() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static("POST, DELETE"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn canvs_task_list_page(State(state): State<ListenerState>, headers: HeaderMap) -> Response {
    if !canvs_authorized(&state, &headers) {
        return canvs_unauthorized(&state, false);
    }
    match crate::canvs::list_workspace_tasks(&state.workspace_path) {
        Ok(tasks) => canvs_html_response(
            StatusCode::OK,
            crate::canvs_web::task_list_page(&state.workspace_name, &tasks),
        ),
        Err(error) => canvs_harness_error(&state, error, false),
    }
}

async fn canvs_task_detail_page(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Response {
    if !canvs_authorized(&state, &headers) {
        return canvs_unauthorized(&state, false);
    }
    match crate::canvs::workspace_task_snapshot(&state.workspace_path, &task_id) {
        Ok(snapshot) => canvs_html_response(
            StatusCode::OK,
            crate::canvs_web::task_detail_page(&state.workspace_name, &snapshot),
        ),
        Err(error) => canvs_harness_error(&state, error, false),
    }
}

async fn canvs_task_list_json(State(state): State<ListenerState>, headers: HeaderMap) -> Response {
    if !canvs_authorized(&state, &headers) {
        return canvs_unauthorized(&state, true);
    }
    match crate::canvs::list_workspace_tasks(&state.workspace_path) {
        Ok(tasks) => canvs_json_response(StatusCode::OK, serde_json::to_value(tasks)),
        Err(error) => canvs_harness_error(&state, error, true),
    }
}

async fn canvs_task_detail_json(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Response {
    if !canvs_authorized(&state, &headers) {
        return canvs_unauthorized(&state, true);
    }
    match crate::canvs::workspace_task_snapshot(&state.workspace_path, &task_id) {
        Ok(snapshot) => canvs_json_response(StatusCode::OK, serde_json::to_value(snapshot)),
        Err(error) => canvs_harness_error(&state, error, true),
    }
}

fn canvs_authorized(state: &ListenerState, headers: &HeaderMap) -> bool {
    if state.auth.auth_type == "noauth" {
        return true;
    }
    if headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("Bearer "))
        && require_mcp_auth(state, headers).is_none()
    {
        return true;
    }
    let Some(password) = basic_auth_password(headers) else {
        return false;
    };
    let expected = if state.auth.bearer_enabled() {
        state.bearer_token.as_deref()
    } else if state.auth.oauth_enabled() {
        state.oauth.as_ref().map(|oauth| oauth.password.as_str())
    } else {
        None
    };
    expected.is_some_and(|expected| constant_time_eq_str(&password, expected))
}

fn basic_auth_password(headers: &HeaderMap) -> Option<String> {
    let encoded = headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Basic ")?;
    let decoded = STANDARD.decode(encoded.trim()).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    decoded
        .split_once(':')
        .map(|(_, password)| password.to_string())
}

fn canvs_unauthorized(state: &ListenerState, json_response: bool) -> Response {
    let mut response = if json_response {
        canvs_json_response(
            StatusCode::UNAUTHORIZED,
            Ok(json!({"error": "Canvs authentication required"})),
        )
    } else {
        canvs_html_response(
            StatusCode::UNAUTHORIZED,
            crate::canvs_web::unauthorized_page(&state.workspace_name),
        )
    };
    let challenge = format!(
        "Basic realm=\"Anchor Canvs {}\", charset=\"UTF-8\"",
        state.workspace_id
    );
    if let Ok(challenge) = HeaderValue::from_str(&challenge) {
        response.headers_mut().insert(WWW_AUTHENTICATE, challenge);
    }
    response
}

fn canvs_harness_error(
    state: &ListenerState,
    error: crate::harness::HarnessError,
    json_response: bool,
) -> Response {
    let code = error.code().to_string();
    let message = crate::canvs::harness_error_message(error);
    let status = if matches!(code.as_str(), "IO_ERROR" | "INVALID_TASK_ID") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    if json_response {
        canvs_json_response(status, Ok(json!({"error": message, "code": code})))
    } else {
        canvs_html_response(
            status,
            crate::canvs_web::error_page(&state.workspace_name, "无法读取任务", &message),
        )
    }
}

fn canvs_json_response(status: StatusCode, value: Result<Value, serde_json::Error>) -> Response {
    let body = value.unwrap_or_else(|error| json!({"error": error.to_string()}));
    let mut response = (status, Json(body)).into_response();
    apply_canvs_security_headers(&mut response);
    response
}

fn canvs_html_response(status: StatusCode, body: String) -> Response {
    let mut response = (status, Html(body)).into_response();
    apply_canvs_security_headers(&mut response);
    response
}

fn apply_canvs_security_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
}

fn resolve_oauth_base(state: &ListenerState, headers: &HeaderMap) -> String {
    external_base_url(
        headers,
        state.bind_port,
        &read_public_url(&state.configured_public_url),
    )
}

fn origin_allowed(state: &ListenerState, headers: &HeaderMap) -> bool {
    request_origin_allowed(
        headers,
        state.bind_port,
        &read_public_url(&state.configured_public_url),
    )
}

fn resolve_oauth_resource(state: &ListenerState, headers: &HeaderMap) -> String {
    format!(
        "{}/mcp",
        resolve_oauth_base(state, headers).trim_end_matches('/')
    )
}

fn resolve_oauth_resource_metadata(state: &ListenerState, headers: &HeaderMap) -> String {
    format!(
        "{}/.well-known/oauth-protected-resource",
        resolve_oauth_base(state, headers).trim_end_matches('/')
    )
}

async fn mcp_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.mcp_rate_limiter.allow() {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "MCP request rate limit exceeded",
        );
    }
    if !accepts_streamable_http(&headers) {
        return http_error(
            StatusCode::NOT_ACCEPTABLE,
            "Accept must allow application/json and text/event-stream",
        );
    }
    if let Some(response) = require_mcp_auth(&state, &headers) {
        return response;
    }
    let cursor_scope = mcp_cursor_scope(&state, &headers);
    if !state
        .mcp_identity_rate_limiter
        .allow(&mcp_request_identity(&headers))
    {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "MCP request identity rate limit exceeded",
        );
    }
    let _permit = match state.concurrency.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return http_error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many concurrent MCP requests",
            )
        }
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => {
            return http_error(
                StatusCode::BAD_REQUEST,
                &format!("Invalid JSON request body: {}", error.body_text()),
            )
        }
    };
    let message = match validate_client_message(&body) {
        Ok(message) => message,
        Err(error) => return jsonrpc_error_response(StatusCode::BAD_REQUEST, Value::Null, error),
    };

    let (request_id, method) = match &message {
        ClientMessage::Request { id, method } => (id.clone(), method.clone()),
        ClientMessage::Notification { method } => (Value::Null, method.clone()),
        ClientMessage::Response => (Value::Null, String::new()),
    };

    if method == "initialize" {
        if !matches!(message, ClientMessage::Request { .. }) {
            return jsonrpc_error_response(
                StatusCode::BAD_REQUEST,
                request_id,
                json!({ "code": -32600, "message": "initialize must be a request" }),
            );
        }
        if session_id_from_headers(&headers).is_some() {
            return http_error(
                StatusCode::BAD_REQUEST,
                "Initialize requests must not include MCP-Session-Id",
            );
        }
        let requested = match requested_protocol_version(&body) {
            Ok(version) => version,
            Err(error) => {
                return jsonrpc_error_response(StatusCode::BAD_REQUEST, request_id, error)
            }
        };
        let negotiated = match require_current_protocol_version(requested) {
            Ok(version) => version,
            Err(error) => {
                return jsonrpc_error_response(StatusCode::BAD_REQUEST, request_id, error)
            }
        };
        let session_id = state.sessions.create(negotiated, &request_id);
        state
            .mcp
            .bind_cursor_scope_for_session(&session_id, cursor_scope.as_deref());
        cleanup_retired_sessions(&state);
        let cancellation = crate::tools::CancellationToken::default();
        let response = execute_mcp_request(
            &state,
            &body,
            &request_id,
            &method,
            negotiated,
            &cancellation,
            &session_id,
        )
        .await;
        return with_session_headers(response, &session_id, negotiated);
    }

    let Some(session_id) = session_id_from_headers(&headers) else {
        return http_error(
            StatusCode::BAD_REQUEST,
            "MCP-Session-Id is required after initialization",
        );
    };
    let session = state.sessions.inspect(session_id);
    cleanup_retired_sessions(&state);
    let Some(session) = session else {
        return http_error(StatusCode::NOT_FOUND, "Unknown or expired MCP session");
    };
    state
        .mcp
        .bind_cursor_scope_for_session(session_id, cursor_scope.as_deref());
    let session_version = session.protocol_version;
    let initialized = session.initialized;
    if let Some(version) = protocol_version_from_headers(&headers) {
        let Ok(version) = version else {
            return http_error(
                StatusCode::BAD_REQUEST,
                "Invalid MCP-Protocol-Version header",
            );
        };
        if version != session_version {
            return http_error(
                StatusCode::BAD_REQUEST,
                "MCP-Protocol-Version does not match the negotiated session version",
            );
        }
    }

    if message == ClientMessage::Response {
        if !initialized {
            return http_error(StatusCode::BAD_REQUEST, "MCP session is not initialized");
        }
        if !state.sessions.touch(session_id) {
            cleanup_retired_sessions(&state);
            return http_error(StatusCode::NOT_FOUND, "Unknown or expired MCP session");
        }
        state.activity.transport_activity("response");
        cleanup_retired_sessions(&state);
        return StatusCode::ACCEPTED.into_response();
    }

    if matches!(message, ClientMessage::Notification { .. }) {
        if method == "notifications/initialized" {
            if !state.sessions.mark_initialized(session_id) {
                cleanup_retired_sessions(&state);
                return http_error(StatusCode::NOT_FOUND, "Unknown or expired MCP session");
            }
        } else if !initialized {
            return http_error(
                StatusCode::BAD_REQUEST,
                "Only notifications/initialized is accepted before MCP initialization completes",
            );
        } else if method == "notifications/cancelled" {
            if let Some(request_id) = body
                .get("params")
                .and_then(|params| params.get("requestId"))
            {
                state.in_flight.cancel(session_id, request_id);
            }
            state.sessions.touch(session_id);
        } else {
            state.sessions.touch(session_id);
        }
        state.activity.transport_activity(&method);
        cleanup_retired_sessions(&state);
        append_profile_log(
            &state.workspace_id,
            "mcp-requests.log",
            &format!("[rpc] accepted_notification method={method} duration_ms=0"),
        );
        return StatusCode::ACCEPTED.into_response();
    }

    if !initialized && method != "ping" {
        return jsonrpc_error_response(
            StatusCode::OK,
            request_id,
            json!({
                "code": -32002,
                "message": "MCP session is not initialized",
                "data": { "reason": "initialized_notification_required" }
            }),
        );
    }

    match state.sessions.reserve_request_id(session_id, &request_id) {
        RequestReservation::Reserved => {}
        RequestReservation::Duplicate => {
            cleanup_retired_sessions(&state);
            return jsonrpc_error_response(
                StatusCode::OK,
                request_id,
                json!({
                    "code": -32600,
                    "message": "Request id has already been used in this MCP session"
                }),
            );
        }
        RequestReservation::Exhausted => {
            remove_session(&state, session_id);
            cleanup_retired_sessions(&state);
            return jsonrpc_error_response(
                StatusCode::OK,
                request_id,
                json!({
                    "code": -32003,
                    "message": "MCP session request-id budget is exhausted; initialize a new session",
                    "data": {
                        "reason": "session_request_budget_exhausted",
                        "retryable": true
                    }
                }),
            );
        }
        RequestReservation::UnknownSession => {
            cleanup_retired_sessions(&state);
            return http_error(StatusCode::NOT_FOUND, "Unknown or expired MCP session");
        }
    }
    cleanup_retired_sessions(&state);

    let in_flight_request = match state.in_flight.insert_with_session_limit(
        session_id,
        &request_id,
        MCP_MAX_CONCURRENT_REQUESTS_PER_SESSION,
    ) {
        InFlightReservation::Inserted(request) => request,
        InFlightReservation::Duplicate => {
            return jsonrpc_error_response(
                StatusCode::OK,
                request_id,
                json!({
                    "code": -32600,
                    "message": "Duplicate in-flight request id",
                    "data": {"reason": "duplicate_in_flight_request_id"}
                }),
            )
        }
        InFlightReservation::SessionLimit => {
            return jsonrpc_error_response(
                StatusCode::OK,
                request_id,
                json!({
                    "code": -32002,
                    "message": "MCP session concurrency limit exceeded",
                    "data": {
                        "reason": "session_concurrency_limit",
                        "maximum_in_flight": MCP_MAX_CONCURRENT_REQUESTS_PER_SESSION,
                        "retryable": true
                    }
                }),
            )
        }
        InFlightReservation::InvalidRequestId => {
            return jsonrpc_error_response(
                StatusCode::OK,
                request_id,
                json!({"code": -32600, "message": "Invalid request id"}),
            )
        }
    };
    let response = execute_mcp_request(
        &state,
        &body,
        &request_id,
        &method,
        &session_version,
        in_flight_request.cancellation(),
        session_id,
    )
    .await;
    state.sessions.touch(session_id);
    cleanup_retired_sessions(&state);
    response
}

async fn mcp_delete(State(state): State<ListenerState>, headers: HeaderMap) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.mcp_rate_limiter.allow() {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "MCP request rate limit exceeded",
        );
    }
    if let Some(response) = require_mcp_auth(&state, &headers) {
        return response;
    }
    let Some(session_id) = session_id_from_headers(&headers) else {
        return http_error(StatusCode::BAD_REQUEST, "MCP-Session-Id is required");
    };
    cleanup_retired_sessions(&state);
    if remove_session(&state, session_id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        http_error(StatusCode::NOT_FOUND, "Unknown or expired MCP session")
    }
}

async fn execute_mcp_request(
    state: &ListenerState,
    body: &Value,
    request_id: &Value,
    method: &str,
    protocol_version: &str,
    cancellation: &crate::tools::CancellationToken,
    session_id: &str,
) -> Response {
    let tool_name = body
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let activity_request = state
        .activity
        .request_started(session_id, request_id, method, &tool_name);
    append_profile_log(
        &state.workspace_id,
        "mcp-requests.log",
        &format!(
            "[rpc] request id={} method={} tool={}",
            request_id, method, tool_name
        ),
    );

    let started = Instant::now();
    let execution = tokio::time::timeout(
        MCP_REQUEST_TIMEOUT,
        handle_request_with_protocol_session_and_cancellation(
            &state.mcp,
            body,
            protocol_version,
            cancellation,
            Some(session_id),
        ),
    );
    let (response, outcome) = match execution.await {
        Ok(response) => (response, "ok"),
        Err(_) => {
            cancellation.cancel();
            (
                json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {
                        "code": -32001,
                        "message": "MCP request timed out",
                        "data": {
                            "reason": "request_timeout",
                            "timeout_seconds": MCP_REQUEST_TIMEOUT.as_secs(),
                            "retryable": true
                        }
                    }
                }),
                "timeout",
            )
        }
    };
    let duration_ms = started.elapsed().as_millis();
    append_profile_log(
        &state.workspace_id,
        "mcp-requests.log",
        &format!(
            "[rpc] completed id={} method={} tool={} outcome={} duration_ms={}",
            request_id, method, tool_name, outcome, duration_ms
        ),
    );
    if method == "tools/list" {
        log_tools_list_catalog(&state.workspace_id, &response);
    }
    if let Some(activity_request) = activity_request {
        activity_request.complete();
    }
    if tool_name == "exec_command" || tool_name == "exec_health_check" {
        let structured = response
            .get("result")
            .and_then(|result| result.get("structuredContent"));
        let status = structured
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let termination_reason = structured
            .and_then(|value| value.get("termination_reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let exit_code = structured
            .and_then(|value| value.get("exit_code"))
            .map(Value::to_string)
            .unwrap_or_default();
        let is_error = response
            .get("result")
            .and_then(|result| result.get("isError"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        append_profile_log(
            &state.workspace_id,
            "mcp-requests.log",
            &format!(
                "[exec] id={} tool={} is_error={} status={} termination_reason={} exit_code={}",
                request_id, tool_name, is_error, status, termination_reason, exit_code
            ),
        );
    }
    Json(response).into_response()
}

fn log_tools_list_catalog(workspace_id: &str, response: &Value) {
    if let Some(metrics) = response
        .get("result")
        .and_then(|result| result.get("_meta"))
        .and_then(|meta| meta.get("anchor/catalog"))
    {
        let page = response
            .get("result")
            .and_then(|result| result.get("_meta"))
            .and_then(|meta| meta.get("anchor/page"));
        append_profile_log(
            workspace_id,
            "mcp-requests.log",
            &format!(
                "[catalog] status=ok local_tool_count={} proxy_tool_count={} tool_count={} catalog_bytes={} estimated_tokens={} page_start={} page_end={} has_next_cursor={}",
                metrics["local_tool_count"],
                metrics["proxy_tool_count"],
                metrics["tool_count"],
                metrics["catalog_bytes"],
                metrics["estimated_tokens"],
                page.and_then(|value| value.get("start")).unwrap_or(&Value::Null),
                page.and_then(|value| value.get("end")).unwrap_or(&Value::Null),
                response
                    .get("result")
                    .and_then(|result| result.get("nextCursor"))
                    .is_some()
            ),
        );
        return;
    }

    let data = response.get("error").and_then(|error| error.get("data"));
    let details = data.and_then(|data| data.get("details"));
    if data
        .and_then(|data| data.get("code"))
        .and_then(Value::as_str)
        == Some("EFFECTIVE_CATALOG_CHATGPT_BUDGET_EXCEEDED")
    {
        append_profile_log(
            workspace_id,
            "mcp-requests.log",
            &format!(
                "[catalog] status=rejected reason=chatgpt_catalog_budget_exceeded local_tool_count={} proxy_tool_count={} tool_count={} catalog_bytes={} estimated_tokens={} budget={}",
                details.and_then(|value| value.get("local_tool_count")).unwrap_or(&Value::Null),
                details.and_then(|value| value.get("proxy_tool_count")).unwrap_or(&Value::Null),
                details.and_then(|value| value.get("tool_count")).unwrap_or(&Value::Null),
                details.and_then(|value| value.get("catalog_bytes")).unwrap_or(&Value::Null),
                details.and_then(|value| value.get("estimated_tokens")).unwrap_or(&Value::Null),
                details.and_then(|value| value.get("budget")).unwrap_or(&Value::Null)
            ),
        );
    }
}

fn session_id_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn protocol_version_from_headers(headers: &HeaderMap) -> Option<Result<&str, ()>> {
    headers
        .get("mcp-protocol-version")
        .map(|value| value.to_str().map(str::trim).map_err(|_| ()))
}

fn with_session_headers(
    mut response: Response,
    session_id: &str,
    protocol_version: &str,
) -> Response {
    if let Ok(value) = HeaderValue::from_str(session_id) {
        response.headers_mut().insert("mcp-session-id", value);
    }
    if let Ok(value) = HeaderValue::from_str(protocol_version) {
        response.headers_mut().insert("mcp-protocol-version", value);
    }
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn http_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn jsonrpc_error_response(status: StatusCode, id: Value, error: Value) -> Response {
    (
        status,
        Json(json!({ "jsonrpc": "2.0", "id": id, "error": error })),
    )
        .into_response()
}

fn require_mcp_auth(state: &ListenerState, headers: &HeaderMap) -> Option<Response> {
    if state.auth.bearer_enabled() {
        let expected = state.bearer_token.as_deref().unwrap_or("");
        return verify_bearer_header(headers, expected);
    }
    if state.auth.oauth_enabled() {
        if let Some(oauth) = state.oauth.as_ref() {
            let issuer_url = resolve_oauth_base(state, headers);
            let resource_url = resolve_oauth_resource(state, headers);
            let resource_metadata_url = resolve_oauth_resource_metadata(state, headers);
            return verify_oauth_bearer_header(
                headers,
                oauth,
                &issuer_url,
                &resource_url,
                &resource_metadata_url,
            );
        }
    }
    None
}

async fn oauth_authorization_server_metadata(
    State(state): State<ListenerState>,
    headers: HeaderMap,
) -> Response {
    if !state.auth.oauth_enabled() {
        return oauth_not_configured();
    }
    let base = resolve_oauth_base(&state, &headers);
    Json(authorization_server_metadata(
        &base,
        state.oauth_client_secret.as_deref(),
    ))
    .into_response()
}

async fn oauth_protected_resource_metadata(
    State(state): State<ListenerState>,
    headers: HeaderMap,
) -> Response {
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
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.oauth_rate_limiter.allow() {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth request rate limit exceeded",
        );
    }
    if !state
        .oauth_identity_rate_limiter
        .allow(&oauth_client_identity(&params.client_id))
    {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth client rate limit exceeded",
        );
    }
    let Some(oauth) = state.oauth.as_ref() else {
        return oauth_not_configured();
    };
    let redirect_label = redirect_uri_log_label(&params.redirect_uri);
    let redirect_status = oauth.redirect_uri_status_label(&params.redirect_uri);
    append_profile_log(
        &state.workspace_id,
        "mcp-oauth.log",
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
        "mcp-oauth.log",
        &format!(
            "[oauth] event=authorize_page_result method=GET status={} {redirect_label}",
            response.status().as_u16()
        ),
    );
    response
}

async fn oauth_authorize_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Form(form): Form<AuthorizeForm>,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.oauth_rate_limiter.allow() {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth request rate limit exceeded",
        );
    }
    if !state
        .oauth_identity_rate_limiter
        .allow(&oauth_client_identity(&form.client_id))
    {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth client rate limit exceeded",
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
        "mcp-oauth.log",
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
        "mcp-oauth.log",
        &format!(
            "[oauth] event=authorize_result method=POST status={} {redirect_label} redirect_status_after={after_status}",
            response.status().as_u16()
        ),
    );
    response
}

async fn oauth_token_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !state.oauth_rate_limiter.allow() {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth request rate limit exceeded",
        );
    }
    if !state
        .oauth_identity_rate_limiter
        .allow(&oauth_client_identity(&form.client_id))
    {
        return http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "OAuth client rate limit exceeded",
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
        "mcp-oauth.log",
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
        "mcp-oauth.log",
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::extract::State;
    use axum::http::{
        header::{ALLOW, AUTHORIZATION, CACHE_CONTROL, WWW_AUTHENTICATE},
        HeaderMap, StatusCode,
    };
    use axum::response::IntoResponse;
    use axum::Json;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::{json, Value};
    use tokio::sync::Semaphore;

    use crate::mcp::protocol::{
        InFlightRequests, KeyedRateLimiter, LegacyMcpSessionStore, RateLimiter,
    };
    use crate::mcp::server::new_state;
    use crate::mcp::McpActivityTracker;
    use crate::runtime::{register_public_url, update_public_url};
    use crate::tools::policy::PolicySettings;
    use crate::tools::Workspace;
    use crate::workspace::AuthConfig;

    use super::{
        accepts_streamable_http, basic_auth_password, bind_listener, canvs_authorized,
        canvs_unauthorized, mcp_cursor_scope, mcp_delete, mcp_method_not_allowed_response,
        mcp_post, origin_allowed, resolve_oauth_base, ListenerState,
    };

    #[test]
    fn bind_listener_reports_port_conflict_synchronously() {
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("占用测试端口");
        let port = occupied.local_addr().expect("读取测试端口").port();

        assert!(bind_listener(port).is_err());
    }

    #[test]
    fn unsupported_get_returns_405_and_prevents_caching() {
        let response = mcp_method_not_allowed_response().into_response();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[ALLOW], "POST, DELETE");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }

    #[test]
    fn post_accept_must_allow_json_and_event_stream() {
        let mut headers = HeaderMap::new();
        assert!(!accepts_streamable_http(&headers));
        headers.insert("accept", "application/json".parse().unwrap());
        assert!(!accepts_streamable_http(&headers));
        headers.insert(
            "accept",
            "application/json, text/event-stream".parse().unwrap(),
        );
        assert!(accepts_streamable_http(&headers));
        headers.insert("accept", "*/*".parse().unwrap());
        assert!(accepts_streamable_http(&headers));
    }

    #[test]
    fn canvs_basic_auth_is_scoped_to_the_workspace_secret() {
        let (_workspace, mut state) = test_listener_state();
        state.auth.auth_type = "bearer".into();
        state.bearer_token = Some("workspace-secret".into());

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Basic {}", STANDARD.encode("viewer:workspace-secret"))
                .parse()
                .unwrap(),
        );
        assert_eq!(
            basic_auth_password(&headers).as_deref(),
            Some("workspace-secret")
        );
        assert!(canvs_authorized(&state, &headers));

        headers.insert(
            AUTHORIZATION,
            format!("Basic {}", STANDARD.encode("viewer:other-secret"))
                .parse()
                .unwrap(),
        );
        assert!(!canvs_authorized(&state, &headers));

        let response = canvs_unauthorized(&state, false);
        assert!(response.headers()[WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains(&state.workspace_id));
    }

    #[test]
    fn bearer_cursor_scope_is_stable_and_noauth_does_not_share() {
        let (_workspace, mut state) = test_listener_state();
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer workspace-secret".parse().unwrap());
        assert_eq!(mcp_cursor_scope(&state, &headers), None);

        state.auth.auth_type = "bearer".into();
        state.bearer_token = Some("workspace-secret".into());
        let first = mcp_cursor_scope(&state, &headers).expect("bearer cursor scope");
        let second = mcp_cursor_scope(&state, &headers).expect("stable bearer cursor scope");
        assert_eq!(first, second);

        headers.insert(AUTHORIZATION, "Bearer other-secret".parse().unwrap());
        assert_ne!(
            first,
            mcp_cursor_scope(&state, &headers).expect("isolated bearer cursor scope")
        );
    }

    fn test_listener_state() -> (tempfile::TempDir, ListenerState) {
        let workspace = tempfile::tempdir().expect("workspace");
        let auth = AuthConfig {
            auth_type: "noauth".into(),
            ..AuthConfig::default()
        };
        let mcp = new_state(
            Workspace::new(workspace.path().to_path_buf()).expect("workspace state"),
            auth.clone(),
            PolicySettings::default(),
            "core".into(),
            "trusted".into(),
            "https://mcp.example.com/workspace/listener-test".into(),
        );
        let workspace_id = format!("listener-test-{}", uuid::Uuid::new_v4());
        let workspace_path = workspace.path().to_path_buf();
        (
            workspace,
            ListenerState {
                mcp,
                auth,
                workspace_id: workspace_id.clone(),
                workspace_name: "Listener Test".into(),
                workspace_path,
                bind_port: 28766,
                configured_public_url: register_public_url(
                    &workspace_id,
                    "mcp",
                    "https://mcp.example.com".into(),
                ),
                bearer_token: None,
                oauth: None,
                oauth_client_secret: None,
                proxy_specs: Vec::new(),
                sessions: LegacyMcpSessionStore::default(),
                mcp_rate_limiter: RateLimiter::new(100, Duration::from_secs(60)),
                mcp_identity_rate_limiter: KeyedRateLimiter::new(100, 32, Duration::from_secs(60)),
                oauth_rate_limiter: RateLimiter::new(100, Duration::from_secs(60)),
                oauth_identity_rate_limiter: KeyedRateLimiter::new(
                    100,
                    32,
                    Duration::from_secs(60),
                ),
                concurrency: Arc::new(Semaphore::new(4)),
                in_flight: InFlightRequests::default(),
                activity: McpActivityTracker::default(),
            },
        )
    }

    #[test]
    fn public_url_hot_update_changes_oauth_base_without_restarting_listener() {
        let (_workspace, state) = test_listener_state();
        assert_eq!(
            resolve_oauth_base(&state, &HeaderMap::new()),
            "https://mcp.example.com"
        );
        assert!(update_public_url(
            &state.workspace_id,
            "mcp",
            "https://new.example.com"
        ));
        assert_eq!(
            resolve_oauth_base(&state, &HeaderMap::new()),
            "https://new.example.com"
        );
        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://new.example.com".parse().unwrap());
        assert!(origin_allowed(&state, &headers));
        headers.insert("origin", "https://mcp.example.com".parse().unwrap());
        assert!(!origin_allowed(&state, &headers));
    }

    fn request_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "accept",
            "application/json, text/event-stream".parse().unwrap(),
        );
        headers
    }

    fn initialize_request(version: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1" }
            }
        })
    }

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("response body");
        serde_json::from_slice(&bytes).expect("json response")
    }

    async fn initialized_session(state: &ListenerState) -> (String, HeaderMap) {
        let response = mcp_post(
            State(state.clone()),
            request_headers(),
            Ok(Json(initialize_request("2025-11-25"))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session_id = response.headers()["mcp-session-id"]
            .to_str()
            .expect("session header")
            .to_string();
        let mut headers = request_headers();
        headers.insert("mcp-session-id", session_id.parse().unwrap());
        headers.insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
        let notification = mcp_post(
            State(state.clone()),
            headers.clone(),
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))),
        )
        .await;
        assert_eq!(notification.status(), StatusCode::ACCEPTED);
        (session_id, headers)
    }

    #[tokio::test]
    async fn streamable_http_exposes_openai_native_skill_import_contract() {
        let (workspace, state) = test_listener_state();
        let skill_dir = workspace.path().join("skills/http-skill");
        fs::create_dir_all(skill_dir.join("references")).expect("skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: http-skill\ndescription: Exercise the HTTP Skill import contract.\n---\nUse the reference.\n",
        )
        .expect("skill md");
        fs::write(
            skill_dir.join("references/INFO.md"),
            "HTTP import reference.\n",
        )
        .expect("skill resource");

        let response = mcp_post(
            State(state.clone()),
            request_headers(),
            Ok(Json(initialize_request("2025-11-25"))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session_id = response.headers()["mcp-session-id"]
            .to_str()
            .expect("session header")
            .to_string();
        let initialized = response_json(response).await;
        assert_eq!(
            initialized["result"]["capabilities"]["extensions"]["io.modelcontextprotocol/skills"],
            json!({})
        );

        let mut headers = request_headers();
        headers.insert("mcp-session-id", session_id.parse().unwrap());
        headers.insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
        let notification = mcp_post(
            State(state.clone()),
            headers.clone(),
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))),
        )
        .await;
        assert_eq!(notification.status(), StatusCode::ACCEPTED);

        let listed = mcp_post(
            State(state.clone()),
            headers.clone(),
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "skills/list",
                "params": {}
            }))),
        )
        .await;
        assert_eq!(listed.status(), StatusCode::OK);
        let listed = response_json(listed).await;
        let skill = &listed["result"]["skills"][0];
        assert_eq!(skill["uri"], "skill://anchor/http-skill/SKILL.md");
        assert_eq!(skill["frontmatter"]["name"], "http-skill");
        assert_eq!(skill["resources"].as_array().expect("resources").len(), 2);

        let fetched = mcp_post(
            State(state.clone()),
            headers.clone(),
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "skills/get",
                "params": {"uri": "skill://anchor/http-skill/SKILL.md"}
            }))),
        )
        .await;
        assert_eq!(response_json(fetched).await["result"]["skill"], *skill);

        for (index, resource) in skill["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .enumerate()
        {
            let uri = resource["uri"].as_str().expect("resource uri");
            let read = mcp_post(
                State(state.clone()),
                headers.clone(),
                Ok(Json(json!({
                    "jsonrpc": "2.0",
                    "id": 4 + index,
                    "method": "resources/read",
                    "params": {"uri": uri}
                }))),
            )
            .await;
            let read = response_json(read).await;
            let contents = read["result"]["contents"]
                .as_array()
                .unwrap_or_else(|| panic!("resources/read response missing contents: {read:#}"));
            assert_eq!(contents.len(), 1);
            assert_eq!(contents[0]["uri"], uri);
        }
    }

    #[tokio::test]
    async fn real_mcp_requests_update_activity_snapshot() {
        let (_workspace, state) = test_listener_state();
        assert_eq!(state.activity.snapshot().state, "idle");

        let (_session_id, headers) = initialized_session(&state).await;
        let initialized = state.activity.snapshot();
        assert_eq!(initialized.state, "idle");
        assert_eq!(initialized.completed_requests, 0);
        assert!(initialized.transport_requests >= 2);
        assert_eq!(
            initialized.last_transport_method,
            "notifications/initialized"
        );
        assert!(initialized.last_transport_activity_at.is_some());

        let response = mcp_post(
            State(state.clone()),
            headers,
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "server_info", "arguments": {}}
            }))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let snapshot = state.activity.snapshot();
        assert_eq!(snapshot.state, "recent");
        assert_eq!(snapshot.in_flight_requests, 0);
        assert_eq!(snapshot.completed_requests, 1);
        assert!(snapshot.transport_requests >= 3);
        assert_eq!(snapshot.last_transport_method, "tools/call");
        assert!(snapshot.last_activity_at.is_some());
        assert!(snapshot.last_transport_activity_at.is_some());
    }

    #[tokio::test]
    async fn long_running_tool_call_is_visible_while_in_flight() {
        if which::which("python").is_err() {
            return;
        }
        let (_workspace, state) = test_listener_state();
        let (_session_id, headers) = initialized_session(&state).await;
        let worker_state = state.clone();
        let worker = tokio::spawn(async move {
            mcp_post(
                State(worker_state),
                headers,
                Ok(Json(json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "exec_command",
                        "arguments": {
                            "cmd": "python -c \"import time; time.sleep(2)\"",
                            "timeout_ms": 10_000,
                            "yield_time_ms": 3_000
                        }
                    }
                }))),
            )
            .await
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = state.activity.snapshot();
            if snapshot.in_flight_requests == 1 {
                assert_eq!(snapshot.state, "active");
                assert_eq!(snapshot.current_method, "tools/call");
                assert_eq!(snapshot.current_tool, "exec_command");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "activity was never observed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let response = worker.await.expect("tool call task");
        assert_eq!(response.status(), StatusCode::OK);
        let snapshot = state.activity.snapshot();
        assert_eq!(snapshot.state, "recent");
        assert_eq!(snapshot.in_flight_requests, 0);
        assert_eq!(snapshot.completed_requests, 1);
    }

    #[tokio::test]
    async fn streamable_http_enforces_session_lifecycle() {
        let (_workspace, state) = test_listener_state();
        let response = mcp_post(
            State(state.clone()),
            request_headers(),
            Ok(Json(initialize_request("2025-11-25"))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session_id = response.headers()["mcp-session-id"]
            .to_str()
            .expect("session header")
            .to_string();
        assert_eq!(response.headers()["mcp-protocol-version"], "2025-11-25");
        let initialized = response_json(response).await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");

        let missing_session = mcp_post(
            State(state.clone()),
            request_headers(),
            Ok(Json(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))),
        )
        .await;
        assert_eq!(missing_session.status(), StatusCode::BAD_REQUEST);

        let mut session_headers = request_headers();
        session_headers.insert("mcp-session-id", session_id.parse().unwrap());
        session_headers.insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
        let before_initialized = mcp_post(
            State(state.clone()),
            session_headers.clone(),
            Ok(Json(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))),
        )
        .await;
        assert_eq!(before_initialized.status(), StatusCode::OK);
        assert_eq!(
            response_json(before_initialized).await["error"]["data"]["reason"],
            "initialized_notification_required"
        );

        let premature_notification = mcp_post(
            State(state.clone()),
            session_headers.clone(),
            Ok(Json(json!({
                "jsonrpc":"2.0",
                "method":"notifications/cancelled",
                "params":{"requestId": 3}
            }))),
        )
        .await;
        assert_eq!(premature_notification.status(), StatusCode::BAD_REQUEST);

        let response_without_session = mcp_post(
            State(state.clone()),
            request_headers(),
            Ok(Json(json!({
                "jsonrpc":"2.0",
                "id": 3,
                "result": {}
            }))),
        )
        .await;
        assert_eq!(response_without_session.status(), StatusCode::BAD_REQUEST);

        let notification = mcp_post(
            State(state.clone()),
            session_headers.clone(),
            Ok(Json(json!({
                "jsonrpc":"2.0",
                "method":"notifications/initialized",
                "params": {}
            }))),
        )
        .await;
        assert_eq!(notification.status(), StatusCode::ACCEPTED);

        let tools = mcp_post(
            State(state.clone()),
            session_headers.clone(),
            Ok(Json(json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}))),
        )
        .await;
        assert_eq!(tools.status(), StatusCode::OK);
        assert!(response_json(tools).await["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()));

        let deleted = mcp_delete(State(state.clone()), session_headers.clone()).await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        let deleted_again = mcp_delete(State(state), session_headers).await;
        assert_eq!(deleted_again.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn expired_transport_session_clears_session_scoped_tool_state() {
        let (workspace, mut state) = test_listener_state();
        state.sessions =
            LegacyMcpSessionStore::with_limits(Duration::from_secs(60), Duration::ZERO, 4, 16);
        let response = mcp_post(
            State(state.clone()),
            request_headers(),
            Ok(Json(initialize_request("2025-11-25"))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session_id = response.headers()["mcp-session-id"]
            .to_str()
            .expect("session header")
            .to_string();
        let session_dir = workspace.path().join("expired-session");
        std::fs::create_dir_all(&session_dir).expect("session directory");
        state
            .mcp
            .set_default_cwd_for(Some(&session_id), session_dir);
        assert_eq!(
            state
                .mcp
                .default_cwd_display_for(Some(&session_id))
                .replace('\\', "/"),
            "expired-session"
        );
        std::thread::sleep(Duration::from_millis(1));

        let mut headers = request_headers();
        headers.insert("mcp-session-id", session_id.parse().unwrap());
        headers.insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
        let expired = mcp_post(
            State(state.clone()),
            headers,
            Ok(Json(json!({"jsonrpc":"2.0","id":2,"method":"ping"}))),
        )
        .await;
        assert_eq!(expired.status(), StatusCode::NOT_FOUND);
        assert_eq!(state.mcp.default_cwd_display_for(Some(&session_id)), ".");
    }

    #[tokio::test]
    async fn default_cwd_is_isolated_between_mcp_sessions() {
        let (workspace, state) = test_listener_state();
        std::fs::create_dir_all(workspace.path().join("session-a")).expect("session directory");
        std::fs::write(workspace.path().join("session-a/inside.txt"), "session-a")
            .expect("session file");
        let (session_a, headers_a) = initialized_session(&state).await;
        let (_session_b, headers_b) = initialized_session(&state).await;

        let set_a = mcp_post(
            State(state.clone()),
            headers_a.clone(),
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "cwd",
                    "arguments": {"operation": "set", "path": "session-a"}
                }
            }))),
        )
        .await;
        assert_eq!(
            response_json(set_a).await["result"]["structuredContent"]["default_cwd"],
            "session-a"
        );

        let get_a = mcp_post(
            State(state.clone()),
            headers_a.clone(),
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "cwd", "arguments": {"operation": "get"}}
            }))),
        )
        .await;
        assert_eq!(
            response_json(get_a).await["result"]["structuredContent"]["default_cwd"],
            "session-a"
        );

        let get_b = mcp_post(
            State(state.clone()),
            headers_b,
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "cwd", "arguments": {"operation": "get"}}
            }))),
        )
        .await;
        assert_eq!(
            response_json(get_b).await["result"]["structuredContent"]["default_cwd"],
            "."
        );
        assert_eq!(state.mcp.default_cwd_display(), ".");

        let read_a = mcp_post(
            State(state.clone()),
            headers_a.clone(),
            Ok(Json(json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "read_file",
                    "arguments": {"path": "inside.txt"}
                }
            }))),
        )
        .await;
        assert_eq!(
            response_json(read_a).await["result"]["structuredContent"]["content"],
            "session-a"
        );

        let deleted = mcp_delete(State(state.clone()), headers_a).await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert_eq!(state.mcp.default_cwd_display_for(Some(&session_a)), ".");
    }

    #[tokio::test]
    async fn streamable_http_rejects_bad_origin_accept_and_protocol_version() {
        let (_workspace, state) = test_listener_state();
        let mut bad_origin = request_headers();
        bad_origin.insert("origin", "https://attacker.example".parse().unwrap());
        let response = mcp_post(
            State(state.clone()),
            bad_origin,
            Ok(Json(initialize_request("2025-11-25"))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = mcp_post(
            State(state.clone()),
            HeaderMap::new(),
            Ok(Json(initialize_request("2025-11-25"))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);

        let response = mcp_post(
            State(state.clone()),
            request_headers(),
            Ok(Json(initialize_request("2025-06-18"))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response_json(response).await["error"]["code"], -32602);

        let initialized = mcp_post(
            State(state.clone()),
            request_headers(),
            Ok(Json(initialize_request("2025-11-25"))),
        )
        .await;
        let session_id = initialized.headers()["mcp-session-id"]
            .to_str()
            .expect("session")
            .to_string();
        let mut headers = request_headers();
        headers.insert("mcp-session-id", session_id.parse().unwrap());
        headers.insert("mcp-protocol-version", "2025-06-18".parse().unwrap());
        let response = mcp_post(
            State(state),
            headers,
            Ok(Json(json!({"jsonrpc":"2.0","id":2,"method":"ping"}))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
