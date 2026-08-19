use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{OriginalUri, Path as AxumPath, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, HOST, ORIGIN, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::admin_security::{
    available_privileged_executors, privileged_actions, read_admin_audit_events,
    unavailable_privileged_actions, PrivilegedActionBinding, PrivilegedConfirmationStore,
};
use crate::control::{self, ControlPlaneEventCursor};
use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::management;
use crate::settings::{DownloadConfig, FrpProfileInput, McpGatewayConfig, ProxyConfig};
use crate::workspace::resources::WorkspaceService;
use crate::workspace::WorkspaceProfile;

pub const ADMIN_API_VERSION: u16 = 1;
pub const DEFAULT_ADMIN_PORT: u16 = 28_769;
const ADMIN_SESSION_COOKIE: &str = "anchor_admin_session";
const ADMIN_SESSION_IDLE_TTL: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_ADMIN_SESSIONS: usize = 64;
const WEB_ADMIN_SUPPORTED_COMMANDS: &[&str] = &[
    "prepare_privileged_action",
    "confirm_privileged_action",
    "list_admin_audit_events",
    "list_workspaces",
    "create_workspace",
    "inspect_workspace_skills",
    "open_workspace_directory",
    "delete_workspace",
    "run_health_checks",
    "get_canvs_snapshot",
    "list_canvs_tasks",
    "get_canvs_task_snapshot",
    "get_control_plane_status",
    "get_control_plane_events",
    "get_last_workspace_id",
    "set_last_workspace",
    "list_frp_profiles",
    "save_frp_profile_metadata",
    "set_frp_profile_token",
    "delete_frp_profile",
    "get_runtime_status",
    "test_tunnel",
    "start_runtime",
    "stop_runtime",
    "restart_runtime",
    "get_actions_runtime_status",
    "start_actions_runtime",
    "stop_actions_runtime",
    "restart_actions_runtime",
    "get_workspace_control_status",
    "get_workspace_control_events",
    "get_workspace_secret",
    "set_workspace_secret",
    "regenerate_workspace_secret",
    "get_shared_secret",
    "set_shared_secret",
    "regenerate_shared_secret",
    "get_mcp_gateway",
    "get_mcp_gateway_status",
    "set_mcp_gateway",
    "reload_mcp_gateway",
    "set_mcp_gateway_route",
    "get_gateway_control_events",
    "read_gateway_logs",
    "read_workspace_logs",
    "restart_tunnel",
    "start_tunnel",
    "stop_tunnel",
    "preview_workspace_config",
    "stage_workspace_config",
    "apply_workspace_config",
    "get_windows_service_status",
    "list_software",
    "install_software",
    "uninstall_software",
    "get_proxy",
    "set_proxy",
    "get_download_config",
    "set_download_config",
];

struct AdminStaticAsset {
    content_type: &'static str,
    body: &'static [u8],
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SoftwareMutationArgs {
    kind: String,
    grant_id: String,
}

#[derive(Debug, Deserialize)]
struct IdArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CreateWorkspaceArgs {
    path: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkillsArgs {
    id: String,
    enabled: bool,
    roots: String,
}

#[derive(Debug, Deserialize)]
struct WorkspacePathArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CanvsTaskArgs {
    id: String,
    task_id: String,
}

#[derive(Debug, Deserialize)]
struct FrpProfileMetadataArgs {
    profile: FrpProfileInput,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceEventsArgs {
    id: String,
    #[serde(default)]
    cursor: Option<control::ControlEventCursor>,
    #[serde(default = "default_event_wait_ms")]
    wait_ms: u32,
}

#[derive(Debug, Deserialize)]
struct ProxyArgs {
    proxy: ProxyConfig,
}

#[derive(Debug, Deserialize)]
struct DownloadConfigArgs {
    config: DownloadConfig,
}

#[derive(Debug, Deserialize)]
struct SecretArgs {
    id: String,
    key: String,
}

#[derive(Debug, Deserialize)]
struct SharedSecretArgs {
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetWorkspaceSecretArgs {
    id: String,
    key: String,
    value: String,
    grant_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegenerateWorkspaceSecretArgs {
    id: String,
    key: String,
    grant_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetSharedSecretArgs {
    key: String,
    value: String,
    grant_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegenerateSharedSecretArgs {
    key: String,
    grant_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetFrpProfileTokenArgs {
    id: String,
    token: String,
    grant_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteFrpProfileArgs {
    id: String,
    #[serde(default)]
    grant_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GatewayEventsArgs {
    #[serde(default)]
    cursor: Option<crate::gateway_control::GatewayEventCursor>,
    #[serde(default = "default_event_wait_ms")]
    wait_ms: u32,
}

#[derive(Debug, Deserialize)]
struct LinesArgs {
    #[serde(default = "default_log_lines")]
    lines: u32,
}

#[derive(Debug, Deserialize)]
struct WorkspaceLogsArgs {
    id: String,
    service: String,
}

#[derive(Debug, Deserialize)]
struct TunnelArgs {
    id: String,
    service: String,
}

#[derive(Debug, Deserialize)]
struct GatewayConfigArgs {
    config: McpGatewayConfig,
}

#[derive(Debug, Deserialize)]
struct GatewayRouteArgs {
    id: String,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceConfigArgs {
    base_profile: WorkspaceProfile,
    profile: WorkspaceProfile,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyWorkspaceConfigArgs {
    id: String,
    #[serde(default = "default_config_wait_seconds")]
    wait_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct PrivilegedPrepareArgs {
    action: String,
    binding: PrivilegedActionBinding,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrivilegedConfirmArgs {
    confirmation_id: String,
    confirmation_text: String,
}

#[derive(Debug, Deserialize)]
struct AuditEventsArgs {
    #[serde(default = "default_audit_event_limit")]
    limit: usize,
}

include!(concat!(env!("OUT_DIR"), "/admin_assets.rs"));

#[derive(Clone)]
struct AdminState {
    origin: Arc<str>,
    authority: Arc<str>,
    sessions: Arc<Mutex<HashMap<String, AdminSession>>>,
    privileged_confirmations: Arc<Mutex<PrivilegedConfirmationStore>>,
}

struct AdminSession {
    csrf_token: String,
    created_at: Instant,
    last_seen: Instant,
}

#[derive(Debug, Deserialize, Default)]
struct AdminCommandRequest {
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ControlPlaneEventsArgs {
    #[serde(default)]
    cursor: Option<ControlPlaneEventCursor>,
    #[serde(default = "default_event_wait_ms")]
    wait_ms: u32,
}

fn default_event_wait_ms() -> u32 {
    15_000
}

fn default_log_lines() -> u32 {
    100
}

fn default_config_wait_seconds() -> u64 {
    20
}

fn default_audit_event_limit() -> usize {
    50
}

pub async fn serve(port: u16, as_json: bool) -> AppResult<()> {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| AppError::Message(format!("Web Admin 监听 {address} 失败：{error}")))?;
    let actual = listener
        .local_addr()
        .map_err(|error| AppError::Message(format!("读取 Web Admin 监听地址失败：{error}")))?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "event": "admin_started",
                "apiVersion": ADMIN_API_VERSION,
                "mode": "authenticated_web_admin",
                "uiEmbedded": ADMIN_UI_EMBEDDED,
                "url": format!("http://{actual}")
            }))?
        );
    } else {
        println!(
            "Web Admin 已启动：http://{actual}（API v{ADMIN_API_VERSION}，UI embedded={ADMIN_UI_EMBEDDED}）"
        );
    }

    axum::serve(listener, router(actual))
        .await
        .map_err(|error| AppError::Message(format!("Web Admin 服务异常：{error}")))
}

fn router(address: SocketAddr) -> Router {
    let authority = address.to_string();
    let state = AdminState {
        origin: Arc::from(format!("http://{authority}")),
        authority: Arc::from(authority),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        privileged_confirmations: Arc::new(Mutex::new(PrivilegedConfirmationStore::default())),
    };
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/session", post(create_session))
        .route("/api/v1/commands/{command}", post(command))
        .fallback(get(static_ui))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({
        "ok": true,
        "data": {
            "apiVersion": ADMIN_API_VERSION,
            "buildVersion": env!("CARGO_PKG_VERSION"),
            "mode": "authenticated_bootstrap",
            "mutationsEnabled": true,
            "sessionRequired": true,
            "uiEmbedded": ADMIN_UI_EMBEDDED,
            "supportedCommands": WEB_ADMIN_SUPPORTED_COMMANDS,
            "privilegedCommands": privileged_actions(),
            "privilegedExecutors": available_privileged_executors(),
            "unavailableCommands": unavailable_privileged_actions(),
            "mutationCommands": [
                "create_workspace",
                "open_workspace_directory",
                "delete_workspace",
                "prepare_privileged_action",
                "confirm_privileged_action",
                "set_last_workspace",
                "save_frp_profile_metadata",
                "set_frp_profile_token",
                "delete_frp_profile",
                "set_workspace_secret",
                "regenerate_workspace_secret",
                "set_shared_secret",
                "regenerate_shared_secret",
                "install_software",
                "uninstall_software",
                "set_proxy",
                "set_download_config",
                "stage_workspace_config",
                "apply_workspace_config",
                "start_runtime",
                "stop_runtime",
                "restart_runtime",
                "start_actions_runtime",
                "stop_actions_runtime",
                "restart_actions_runtime",
                "start_tunnel",
                "restart_tunnel",
                "stop_tunnel",
                "test_tunnel",
                "set_mcp_gateway",
                "reload_mcp_gateway",
                "set_mcp_gateway_route"
            ]
        }
    }))
}

async fn static_ui(OriginalUri(uri): OriginalUri) -> Response {
    serve_static_uri(&uri)
}

fn serve_static_uri(uri: &Uri) -> Response {
    let requested = uri.path().trim_start_matches('/');
    if requested.starts_with("api/") || requested.contains("..") || requested.contains('\\') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    let asset = embedded_admin_asset(path).or_else(|| {
        (!path.contains('.'))
            .then(|| embedded_admin_asset("index.html"))
            .flatten()
    });
    let Some(asset) = asset else {
        if !ADMIN_UI_EMBEDDED && (path == "index.html" || !path.contains('.')) {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Web Admin UI 未嵌入当前二进制；请使用 `pnpm cli:build` 构建正式 CLI。",
            )
                .into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    };
    static_asset_response(path, asset)
}

fn static_asset_response(path: &str, asset: AdminStaticAsset) -> Response {
    let mut response = Response::new(Body::from(asset.body));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
    let cache = if path.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

async fn create_session(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err(message) = validate_browser_request(&state, &headers) {
        return error_response(StatusCode::FORBIDDEN, "ADMIN_REQUEST_REJECTED", message);
    }
    let session_id = match random_token() {
        Ok(token) => token,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ADMIN_SESSION_CREATE_FAILED",
                error.to_string(),
            )
        }
    };
    let csrf_token = match random_token() {
        Ok(token) => token,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ADMIN_SESSION_CREATE_FAILED",
                error.to_string(),
            )
        }
    };
    let now = Instant::now();
    let mut sessions = match state.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ADMIN_SESSION_STORE_UNAVAILABLE",
                "Web Admin session store poisoned",
            )
        }
    };
    purge_expired_sessions(&mut sessions, now);
    if sessions.len() >= MAX_ADMIN_SESSIONS {
        if let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, session)| session.created_at)
            .map(|(id, _)| id.clone())
        {
            sessions.remove(&oldest);
        }
    }
    sessions.insert(
        session_id.clone(),
        AdminSession {
            csrf_token: csrf_token.clone(),
            created_at: now,
            last_seen: now,
        },
    );
    drop(sessions);

    let mut response = (
        StatusCode::CREATED,
        Json(json!({
            "ok": true,
            "data": {
                "csrfToken": csrf_token,
                "idleTimeoutSeconds": ADMIN_SESSION_IDLE_TTL.as_secs(),
                "supportedCommands": WEB_ADMIN_SUPPORTED_COMMANDS,
                "privilegedCommands": privileged_actions(),
                "privilegedExecutors": available_privileged_executors(),
                "unavailableCommands": unavailable_privileged_actions()
            }
        })),
    )
        .into_response();
    let cookie = format!(
        "{ADMIN_SESSION_COOKIE}={session_id}; Path=/api/v1; HttpOnly; SameSite=Strict; Max-Age={}",
        ADMIN_SESSION_IDLE_TTL.as_secs()
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn command(
    State(state): State<AdminState>,
    headers: HeaderMap,
    AxumPath(command): AxumPath<String>,
    Json(request): Json<AdminCommandRequest>,
) -> Response {
    if let Err(message) = validate_browser_request(&state, &headers) {
        return error_response(StatusCode::FORBIDDEN, "ADMIN_REQUEST_REJECTED", message);
    }
    let session_id = match authenticate_session(&state, &headers) {
        Ok(session_id) => session_id,
        Err(error) => {
            return error_response(StatusCode::UNAUTHORIZED, error.code(), error.message())
        }
    };
    match dispatch_command(&state, &session_id, &command, request.args).await {
        Ok(data) => (StatusCode::OK, Json(json!({ "ok": true, "data": data }))).into_response(),
        Err(AdminDispatchError::NotMigrated) => error_response(
            StatusCode::NOT_IMPLEMENTED,
            "ADMIN_COMMAND_NOT_MIGRATED",
            format!("Web Admin 命令尚未迁移：{command}"),
        ),
        Err(AdminDispatchError::Rejected(error)) => error_response(
            StatusCode::FORBIDDEN,
            "ADMIN_PRIVILEGED_CONFIRMATION_REJECTED",
            error.to_string(),
        ),
        Err(AdminDispatchError::Failed(error)) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ADMIN_COMMAND_FAILED",
            error.to_string(),
        ),
    }
}

fn validate_browser_request(state: &AdminState, headers: &HeaderMap) -> Result<(), String> {
    let marker = headers
        .get("x-anchor-admin-request")
        .and_then(|value| value.to_str().ok());
    if marker != Some("1") {
        return Err("缺少 Web Admin 请求标记。".into());
    }
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "缺少 Web Admin Host。".to_string())?;
    if host != state.authority.as_ref() {
        return Err(format!("拒绝非 canonical Web Admin Host：{host}"));
    }
    let origin = headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "缺少 Web Admin Origin。".to_string())?;
    if origin != state.origin.as_ref() {
        return Err(format!("拒绝非同源 Web Admin Origin：{origin}"));
    }
    if let Some(site) = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
    {
        if site != "same-origin" {
            return Err(format!("拒绝非 same-origin Fetch：{site}"));
        }
    }
    Ok(())
}

enum AdminAuthError {
    Missing,
    Expired,
    Csrf,
}

impl AdminAuthError {
    fn code(&self) -> &'static str {
        match self {
            Self::Missing => "ADMIN_SESSION_REQUIRED",
            Self::Expired => "ADMIN_SESSION_EXPIRED",
            Self::Csrf => "ADMIN_CSRF_REJECTED",
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::Missing => "缺少有效 Web Admin 管理会话。",
            Self::Expired => "Web Admin 管理会话已过期。",
            Self::Csrf => "Web Admin CSRF token 无效。",
        }
    }
}

fn authenticate_session(state: &AdminState, headers: &HeaderMap) -> Result<String, AdminAuthError> {
    let session_id = cookie_value(headers, ADMIN_SESSION_COOKIE).ok_or(AdminAuthError::Missing)?;
    let csrf = headers
        .get("x-anchor-admin-csrf")
        .and_then(|value| value.to_str().ok())
        .ok_or(AdminAuthError::Csrf)?;
    let now = Instant::now();
    let mut sessions = state.sessions.lock().map_err(|_| AdminAuthError::Missing)?;
    let expired = sessions
        .get(&session_id)
        .is_some_and(|session| now.duration_since(session.last_seen) > ADMIN_SESSION_IDLE_TTL);
    if expired {
        sessions.remove(&session_id);
        return Err(AdminAuthError::Expired);
    }
    let session = sessions
        .get_mut(&session_id)
        .ok_or(AdminAuthError::Missing)?;
    if session.csrf_token != csrf {
        return Err(AdminAuthError::Csrf);
    }
    session.last_seen = now;
    Ok(session_id)
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookies = headers.get(COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn purge_expired_sessions(sessions: &mut HashMap<String, AdminSession>, now: Instant) {
    sessions.retain(|_, session| now.duration_since(session.last_seen) <= ADMIN_SESSION_IDLE_TTL);
}

fn random_token() -> AppResult<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        AppError::Message(format!("生成 Web Admin session token 失败：{error}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

enum AdminDispatchError {
    NotMigrated,
    Rejected(AppError),
    Failed(AppError),
}

impl From<AppError> for AdminDispatchError {
    fn from(value: AppError) -> Self {
        Self::Failed(value)
    }
}

fn consume_privileged_grant(
    state: &AdminState,
    session_id: &str,
    grant_id: &str,
    action: &str,
    binding: &PrivilegedActionBinding,
) -> Result<(), AdminDispatchError> {
    state
        .privileged_confirmations
        .lock()
        .map_err(|_| {
            AdminDispatchError::Failed(AppError::Message(
                "privileged confirmation store poisoned".into(),
            ))
        })?
        .consume_grant(session_id, grant_id, action, binding)
        .map_err(AdminDispatchError::Rejected)
}

fn finish_privileged_execution<T>(
    state: &AdminState,
    session_id: &str,
    action: &str,
    result: AppResult<T>,
) -> Result<T, AdminDispatchError> {
    let succeeded = result.is_ok();
    let audit = state
        .privileged_confirmations
        .lock()
        .map_err(|_| AppError::Message("privileged confirmation store poisoned".into()))
        .and_then(|store| store.record_execution_outcome(session_id, action, succeeded));
    match (result, audit) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(AdminDispatchError::Failed(error)),
        (Ok(_), Err(audit_error)) => Err(AdminDispatchError::Failed(AppError::Message(format!(
            "高权限操作已执行，但审计结果写入失败：{audit_error}"
        )))),
        (Err(error), Err(audit_error)) => Err(AdminDispatchError::Failed(AppError::Message(
            format!("{error}；审计结果写入也失败：{audit_error}"),
        ))),
    }
}

async fn dispatch_command(
    state: &AdminState,
    session_id: &str,
    command: &str,
    args: Value,
) -> Result<Value, AdminDispatchError> {
    match command {
        "prepare_privileged_action" => {
            let input: PrivilegedPrepareArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let prepared = state
                .privileged_confirmations
                .lock()
                .map_err(|_| AppError::Message("privileged confirmation store poisoned".into()))?
                .prepare(session_id, &input.action, &input.binding)
                .map_err(AdminDispatchError::Rejected)?;
            serde_json::to_value(prepared)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "confirm_privileged_action" => {
            let input: PrivilegedConfirmArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let grant = state
                .privileged_confirmations
                .lock()
                .map_err(|_| AppError::Message("privileged confirmation store poisoned".into()))?
                .confirm(session_id, &input.confirmation_id, &input.confirmation_text)
                .map_err(AdminDispatchError::Rejected)?;
            serde_json::to_value(grant)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "list_admin_audit_events" => {
            let input: AuditEventsArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(read_admin_audit_events(input.limit)?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "list_workspaces" => serde_json::to_value(management::list_workspaces()?)
            .map_err(AppError::from)
            .map_err(Into::into),
        "create_workspace" => {
            let input: CreateWorkspaceArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::create_workspace(input.path, input.name)?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "inspect_workspace_skills" => {
            let input: SkillsArgs = serde_json::from_value(args).map_err(AppError::from)?;
            management::inspect_workspace_skills(&input.id, input.enabled, &input.roots)
                .map_err(Into::into)
        }
        "open_workspace_directory" => {
            let input: WorkspacePathArgs = serde_json::from_value(args).map_err(AppError::from)?;
            management::open_workspace_directory(&input.path)?;
            Ok(Value::Null)
        }
        "delete_workspace" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            management::delete_workspace(&input.id).await?;
            Ok(Value::Null)
        }
        "run_health_checks" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::run_health_checks(&input.id).await?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "get_canvs_snapshot" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::get_canvs_snapshot(&input.id)?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "list_canvs_tasks" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::list_canvs_tasks(&input.id)?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "get_canvs_task_snapshot" => {
            let input: CanvsTaskArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::get_canvs_task_snapshot(
                &input.id,
                &input.task_id,
            )?)
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "get_control_plane_status" => {
            let store = DataStore::load()?;
            let profiles = store.list().to_vec();
            drop(store);
            let status = control::control_plane_status(&profiles).await?;
            serde_json::to_value(status)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "get_control_plane_events" => {
            let input: ControlPlaneEventsArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let store = DataStore::load()?;
            let profiles = store.list().to_vec();
            drop(store);
            let batch =
                control::control_plane_events(&profiles, input.cursor, 64, input.wait_ms).await?;
            serde_json::to_value(batch)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "get_last_workspace_id" => Ok(Value::String(management::get_last_workspace_id()?)),
        "set_last_workspace" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            management::set_last_workspace(input.id)?;
            Ok(Value::Null)
        }
        "list_frp_profiles" => serde_json::to_value(management::list_frp_profiles()?)
            .map_err(AppError::from)
            .map_err(Into::into),
        "save_frp_profile_metadata" => {
            let input: FrpProfileMetadataArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::save_frp_profile_metadata(input.profile)?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "set_frp_profile_token" => {
            let input: SetFrpProfileTokenArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let binding = PrivilegedActionBinding::frp_profile(&input.id);
            consume_privileged_grant(
                state,
                session_id,
                &input.grant_id,
                "set_frp_profile_token",
                &binding,
            )?;
            let result = management::set_frp_profile_token(&input.id, &input.token);
            let saved =
                finish_privileged_execution(state, session_id, "set_frp_profile_token", result)?;
            serde_json::to_value(saved)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "delete_frp_profile" => {
            let input: DeleteFrpProfileArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let has_token = management::frp_profile_has_token(&input.id)?;
            if let Some(grant_id) = input.grant_id.as_deref() {
                let binding = PrivilegedActionBinding::frp_profile(&input.id);
                consume_privileged_grant(
                    state,
                    session_id,
                    grant_id,
                    "delete_frp_profile",
                    &binding,
                )?;
                finish_privileged_execution(
                    state,
                    session_id,
                    "delete_frp_profile",
                    management::delete_frp_profile(&input.id),
                )?;
            } else if has_token {
                return Err(AdminDispatchError::Rejected(AppError::Message(
                    "删除含 FRP Token 的 profile 需要高权限确认。".into(),
                )));
            } else {
                management::delete_frp_profile(&input.id)?;
            }
            Ok(Value::Null)
        }
        "get_runtime_status" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::runtime_status(&input.id, WorkspaceService::Mcp).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "test_tunnel" => {
            let input: TunnelArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::test_workspace_tunnel(&input.id, &input.service).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "start_runtime" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::start_workspace_service(&input.id, WorkspaceService::Mcp).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "stop_runtime" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::stop_workspace_service(&input.id, WorkspaceService::Mcp).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "restart_runtime" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::restart_workspace_service(&input.id, WorkspaceService::Mcp).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "get_actions_runtime_status" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::runtime_status(&input.id, WorkspaceService::Actions).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "start_actions_runtime" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::start_workspace_service(&input.id, WorkspaceService::Actions).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "stop_actions_runtime" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::stop_workspace_service(&input.id, WorkspaceService::Actions).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "restart_actions_runtime" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::restart_workspace_service(&input.id, WorkspaceService::Actions).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "get_workspace_control_status" => {
            let input: IdArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::workspace_control_status(&input.id).await?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "get_workspace_control_events" => {
            let input: WorkspaceEventsArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::workspace_control_events(&input.id, input.cursor, input.wait_ms)
                    .await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "get_workspace_secret" => {
            let input: SecretArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::get_workspace_secret(&input.id, &input.key)?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "set_workspace_secret" => {
            let input: SetWorkspaceSecretArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let binding = PrivilegedActionBinding::workspace_secret(&input.id, &input.key);
            consume_privileged_grant(
                state,
                session_id,
                &input.grant_id,
                "set_workspace_secret",
                &binding,
            )?;
            finish_privileged_execution(
                state,
                session_id,
                "set_workspace_secret",
                management::set_workspace_secret(&input.id, &input.key, &input.value),
            )?;
            Ok(Value::Null)
        }
        "regenerate_workspace_secret" => {
            let input: RegenerateWorkspaceSecretArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let binding = PrivilegedActionBinding::workspace_secret(&input.id, &input.key);
            consume_privileged_grant(
                state,
                session_id,
                &input.grant_id,
                "regenerate_workspace_secret",
                &binding,
            )?;
            let value = finish_privileged_execution(
                state,
                session_id,
                "regenerate_workspace_secret",
                management::regenerate_workspace_secret(&input.id, &input.key),
            )?;
            Ok(Value::String(value))
        }
        "get_shared_secret" => {
            let input: SharedSecretArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::get_shared_secret(&input.key)?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "set_shared_secret" => {
            let input: SetSharedSecretArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let binding = PrivilegedActionBinding::shared_secret(&input.key);
            consume_privileged_grant(
                state,
                session_id,
                &input.grant_id,
                "set_shared_secret",
                &binding,
            )?;
            finish_privileged_execution(
                state,
                session_id,
                "set_shared_secret",
                management::set_shared_secret(&input.key, &input.value),
            )?;
            Ok(Value::Null)
        }
        "regenerate_shared_secret" => {
            let input: RegenerateSharedSecretArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let binding = PrivilegedActionBinding::shared_secret(&input.key);
            consume_privileged_grant(
                state,
                session_id,
                &input.grant_id,
                "regenerate_shared_secret",
                &binding,
            )?;
            let value = finish_privileged_execution(
                state,
                session_id,
                "regenerate_shared_secret",
                management::regenerate_shared_secret(&input.key),
            )?;
            Ok(Value::String(value))
        }
        "get_mcp_gateway" => serde_json::to_value(management::get_mcp_gateway()?)
            .map_err(AppError::from)
            .map_err(Into::into),
        "get_mcp_gateway_status" => {
            serde_json::to_value(management::get_mcp_gateway_status().await?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "set_mcp_gateway" => {
            let input: GatewayConfigArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::set_mcp_gateway(input.config).await?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "reload_mcp_gateway" => serde_json::to_value(management::reload_mcp_gateway().await?)
            .map_err(AppError::from)
            .map_err(Into::into),
        "set_mcp_gateway_route" => {
            let input: GatewayRouteArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::set_gateway_workspace_route(&input.id, input.enabled).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "get_gateway_control_events" => {
            let input: GatewayEventsArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::get_gateway_control_events(input.cursor, input.wait_ms).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "read_gateway_logs" => {
            let input: LinesArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::read_gateway_logs(input.lines).await?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "read_workspace_logs" => {
            let input: WorkspaceLogsArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::read_workspace_logs(&input.id, &input.service).await?)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "restart_tunnel" => {
            let input: TunnelArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::restart_workspace_tunnel(&input.id, &input.service).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "start_tunnel" => {
            let input: TunnelArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::start_workspace_tunnel(&input.id, &input.service).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "stop_tunnel" => {
            let input: TunnelArgs = serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::stop_workspace_tunnel(&input.id, &input.service).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "preview_workspace_config" => {
            let input: WorkspaceConfigArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::preview_workspace_config(
                &input.base_profile,
                &input.profile,
            )?)
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "stage_workspace_config" => {
            let input: WorkspaceConfigArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(management::stage_workspace_config(
                &input.base_profile,
                &input.profile,
            )?)
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "apply_workspace_config" => {
            let input: ApplyWorkspaceConfigArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            serde_json::to_value(
                management::apply_workspace_config(input.id, input.wait_seconds).await?,
            )
            .map_err(AppError::from)
            .map_err(Into::into)
        }
        "get_windows_service_status" => management::windows_service_status().map_err(Into::into),
        "list_software" => serde_json::to_value(management::list_software()?)
            .map_err(AppError::from)
            .map_err(Into::into),
        "install_software" => {
            let input: SoftwareMutationArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            let version = management::software_target_version(&input.kind)?;
            let binding = PrivilegedActionBinding::software_install(&input.kind, version);
            consume_privileged_grant(
                state,
                session_id,
                &input.grant_id,
                "install_software",
                &binding,
            )?;
            let installed = finish_privileged_execution(
                state,
                session_id,
                "install_software",
                management::install_software(&input.kind).await,
            )?;
            serde_json::to_value(installed)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "uninstall_software" => {
            let input: SoftwareMutationArgs =
                serde_json::from_value(args).map_err(AppError::from)?;
            management::software_target_version(&input.kind)?;
            let binding = PrivilegedActionBinding::software_uninstall(&input.kind);
            consume_privileged_grant(
                state,
                session_id,
                &input.grant_id,
                "uninstall_software",
                &binding,
            )?;
            let uninstalled = finish_privileged_execution(
                state,
                session_id,
                "uninstall_software",
                management::uninstall_software(&input.kind),
            )?;
            serde_json::to_value(uninstalled)
                .map_err(AppError::from)
                .map_err(Into::into)
        }
        "get_proxy" => serde_json::to_value(management::get_proxy()?)
            .map_err(AppError::from)
            .map_err(Into::into),
        "set_proxy" => {
            let input: ProxyArgs = serde_json::from_value(args).map_err(AppError::from)?;
            management::set_proxy(input.proxy)?;
            Ok(Value::Null)
        }
        "get_download_config" => serde_json::to_value(management::get_download_config()?)
            .map_err(AppError::from)
            .map_err(Into::into),
        "set_download_config" => {
            let input: DownloadConfigArgs = serde_json::from_value(args).map_err(AppError::from)?;
            management::set_download_config(input.config)?;
            Ok(Value::Null)
        }
        _ => Err(AdminDispatchError::NotMigrated),
    }
}

fn error_response(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "ok": false,
            "error": {
                "code": code,
                "message": message.into()
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn browser_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-anchor-admin-request", HeaderValue::from_static("1"));
        headers.insert(HOST, HeaderValue::from_static("127.0.0.1:28769"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:28769"));
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        headers
    }

    fn test_state(sessions: HashMap<String, AdminSession>) -> AdminState {
        AdminState {
            origin: Arc::from("http://127.0.0.1:28769"),
            authority: Arc::from("127.0.0.1:28769"),
            sessions: Arc::new(Mutex::new(sessions)),
            privileged_confirmations: Arc::new(Mutex::new(PrivilegedConfirmationStore::default())),
        }
    }

    #[test]
    fn admin_header_guard_requires_exact_host_and_origin() {
        let state = test_state(HashMap::new());
        let mut headers = browser_headers();
        assert!(validate_browser_request(&state, &headers).is_ok());

        headers.insert("origin", HeaderValue::from_static("https://example.com"));
        assert!(validate_browser_request(&state, &headers).is_err());

        headers.insert("origin", HeaderValue::from_static("http://127.0.0.1:28769"));
        headers.insert(HOST, HeaderValue::from_static("localhost:28769"));
        assert!(validate_browser_request(&state, &headers).is_err());
    }

    #[test]
    fn admin_session_requires_cookie_and_matching_csrf() {
        let state = test_state(HashMap::from([(
            "session-1".into(),
            AdminSession {
                csrf_token: "csrf-1".into(),
                created_at: Instant::now(),
                last_seen: Instant::now(),
            },
        )]));
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("anchor_admin_session=session-1"),
        );
        headers.insert("x-anchor-admin-csrf", HeaderValue::from_static("csrf-1"));
        assert!(authenticate_session(&state, &headers).is_ok());

        headers.insert("x-anchor-admin-csrf", HeaderValue::from_static("wrong"));
        assert!(matches!(
            authenticate_session(&state, &headers),
            Err(AdminAuthError::Csrf)
        ));
    }

    #[tokio::test]
    async fn admin_session_cookie_is_http_only_and_strict_same_site() {
        let state = test_state(HashMap::new());
        let response = create_session(State(state), browser_headers()).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("session cookie");
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/api/v1"));
    }

    #[test]
    fn embedded_admin_root_serves_svelte_with_security_headers() {
        assert!(
            ADMIN_UI_EMBEDDED,
            "pnpm build must run before CLI verification"
        );
        let response = serve_static_uri(&Uri::from_static("/"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let csp = response
            .headers()
            .get("content-security-policy")
            .and_then(|value| value.to_str().ok())
            .expect("content security policy");
        assert!(csp.contains("connect-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
    }

    #[tokio::test]
    async fn unimplemented_command_never_falls_through_to_mutation() {
        let state = test_state(HashMap::new());
        assert!(matches!(
            dispatch_command(&state, "session", "install_windows_service", json!({})).await,
            Err(AdminDispatchError::NotMigrated)
        ));
    }

    #[tokio::test]
    async fn privileged_secret_executor_requires_a_valid_grant_before_mutation() {
        let state = test_state(HashMap::new());
        assert!(matches!(
            dispatch_command(
                &state,
                "session",
                "set_shared_secret",
                json!({
                    "key": "bearer_token",
                    "value": "test-secret-value",
                    "grantId": "missing-grant"
                }),
            )
            .await,
            Err(AdminDispatchError::Rejected(_))
        ));
    }
}
