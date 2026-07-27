use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::auth::{
    authorization_server_metadata, authorize_get, authorize_post, external_base_url,
    protected_resource_metadata, redirect_uri_log_label, register_oauth_runtime,
    request_origin_allowed, token_exchange, verify_bearer_header, verify_oauth_bearer_header,
    AuthorizeForm, AuthorizeParams, OAuthRuntime, TokenForm, OAUTH_MAX_BODY_BYTES,
};
use crate::mcp::protocol::{
    negotiate_protocol_version, protocol_version_supported, requested_protocol_version,
    validate_client_message, ClientMessage, InFlightRequests, RateLimiter, RequestReservation,
    SessionStore,
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
use axum::extract::{DefaultBodyLimit, Form, Query, State};
use axum::http::{
    header::{ACCEPT, ALLOW, CACHE_CONTROL},
    HeaderMap, HeaderValue, StatusCode,
};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::{oneshot, Semaphore};

pub type ShutdownSender = oneshot::Sender<()>;

const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
const MCP_MAX_BODY_BYTES: usize = 1_048_576;
const MCP_MAX_CONCURRENT_REQUESTS: usize = 16;
const MCP_MAX_REQUESTS_PER_MINUTE: usize = 240;
const OAUTH_MAX_REQUESTS_PER_MINUTE: usize = 30;

#[derive(Clone)]
struct ListenerState {
    mcp: SharedState,
    auth: AuthConfig,
    workspace_id: String,
    workspace_path: String,
    bind_port: u16,
    configured_public_url: SharedPublicUrl,
    bearer_token: Option<String>,
    oauth: Option<Arc<OAuthRuntime>>,
    oauth_client_secret: Option<String>,
    proxy_specs: Vec<McpProxyServerSpec>,
    sessions: SessionStore,
    mcp_rate_limiter: RateLimiter,
    oauth_rate_limiter: RateLimiter,
    concurrency: Arc<Semaphore>,
    in_flight: InFlightRequests,
    activity: McpActivityTracker,
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

#[allow(clippy::too_many_arguments)]
pub fn spawn_listener(
    port: u16,
    workspace_path: PathBuf,
    workspace_id: String,
    auth: AuthConfig,
    public_base_url: String,
    oauth_client_secret: Option<String>,
    oauth_password: Option<String>,
    oauth_token_secret: Option<String>,
    runtime: RuntimeConfig,
) -> Result<(ShutdownSender, crate::async_runtime::JoinHandle<()>), String> {
    let workspace_display = workspace_path.display().to_string();
    let proxy_specs = parse_mcp_proxy_config(&runtime.mcp_config, &workspace_path)?;
    let workspace = Workspace::new(workspace_path)
        .map_err(|e| e.message())?
        .with_strict_read_boundary(runtime.strict_workspace_reads);
    let policy = PolicySettings::from_runtime(&runtime);
    let mcp = new_state(
        workspace,
        auth.clone(),
        policy,
        runtime.tool_profile.clone(),
        runtime.permission_mode.clone(),
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
    let state = ListenerState {
        mcp,
        auth,
        workspace_id,
        workspace_path: workspace_display,
        bind_port: port,
        configured_public_url,
        bearer_token,
        oauth,
        oauth_client_secret,
        proxy_specs,
        sessions: SessionStore::default(),
        mcp_rate_limiter: RateLimiter::new(MCP_MAX_REQUESTS_PER_MINUTE, Duration::from_secs(60)),
        oauth_rate_limiter: RateLimiter::new(
            OAUTH_MAX_REQUESTS_PER_MINUTE,
            Duration::from_secs(60),
        ),
        concurrency: Arc::new(Semaphore::new(MCP_MAX_CONCURRENT_REQUESTS)),
        in_flight: InFlightRequests::default(),
        activity,
    };
    // 在返回 Running 之前完成 bind，避免后台任务里的端口冲突被伪装成启动成功。
    let listener = bind_listener(port)?;
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
    Ok((shutdown_tx, handle))
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
    let app = Router::new()
        .merge(mcp_routes)
        .merge(oauth_routes)
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

fn bind_listener(port: u16) -> Result<tokio::net::TcpListener, String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|err| format!("MCP 本地端口 {port} 绑定失败: {err}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("MCP 本地端口 {port} 设置非阻塞失败: {err}"))?;
    tokio::net::TcpListener::from_std(listener)
        .map_err(|err| format!("MCP 本地监听器初始化失败: {err}"))
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
        let negotiated = negotiate_protocol_version(requested);
        let session_id = state.sessions.create(negotiated, &request_id);
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
    let session_version = session.protocol_version;
    let initialized = session.initialized;
    if let Some(version) = protocol_version_from_headers(&headers) {
        let Ok(version) = version else {
            return http_error(
                StatusCode::BAD_REQUEST,
                "Invalid MCP-Protocol-Version header",
            );
        };
        if !protocol_version_supported(version) || version != session_version {
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

    let Some(cancellation) = state.in_flight.insert(session_id, &request_id) else {
        return jsonrpc_error_response(
            StatusCode::OK,
            request_id,
            json!({ "code": -32600, "message": "Duplicate in-flight request id" }),
        );
    };
    let response = execute_mcp_request(
        &state,
        &body,
        &request_id,
        &method,
        &session_version,
        &cancellation,
        session_id,
    )
    .await;
    state.in_flight.remove(session_id, &request_id);
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
    state
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
    state.activity.request_finished(session_id, request_id);
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
        Some(state.workspace_path.as_str()),
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
    use std::sync::Arc;
    use std::time::Duration;

    use axum::extract::State;
    use axum::http::{
        header::{ALLOW, CACHE_CONTROL},
        HeaderMap, StatusCode,
    };
    use axum::response::IntoResponse;
    use axum::Json;
    use serde_json::{json, Value};
    use tokio::sync::Semaphore;

    use crate::mcp::protocol::{InFlightRequests, RateLimiter, SessionStore};
    use crate::mcp::server::new_state;
    use crate::mcp::McpActivityTracker;
    use crate::runtime::{register_public_url, update_public_url};
    use crate::tools::policy::PolicySettings;
    use crate::tools::Workspace;
    use crate::workspace::AuthConfig;

    use super::{
        accepts_streamable_http, bind_listener, mcp_delete, mcp_method_not_allowed_response,
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
        );
        let workspace_id = format!("listener-test-{}", uuid::Uuid::new_v4());
        (
            workspace,
            ListenerState {
                mcp,
                auth,
                workspace_id: workspace_id.clone(),
                workspace_path: "listener-test".into(),
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
                sessions: SessionStore::default(),
                mcp_rate_limiter: RateLimiter::new(100, Duration::from_secs(60)),
                oauth_rate_limiter: RateLimiter::new(100, Duration::from_secs(60)),
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
    async fn real_mcp_requests_update_activity_snapshot() {
        let (_workspace, state) = test_listener_state();
        assert_eq!(state.activity.snapshot().state, "idle");

        let (_session_id, headers) = initialized_session(&state).await;
        assert_eq!(state.activity.snapshot().state, "idle");

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
        assert!(snapshot.last_activity_at.is_some());
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
        state.sessions = SessionStore::with_limits(Duration::from_secs(60), Duration::ZERO, 4, 16);
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
        assert!(state
            .mcp
            .default_cwd_display_for(Some(&session_id))
            .replace('\\', "/")
            .ends_with("/expired-session"));
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
                    "name": "set_default_cwd",
                    "arguments": {"path": "session-a"}
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
                "params": {"name": "get_default_cwd", "arguments": {}}
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
                "params": {"name": "get_default_cwd", "arguments": {}}
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
