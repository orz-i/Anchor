use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures_util::StreamExt;
use serde::Serialize;
use tokio::sync::{oneshot, Mutex, OwnedSemaphorePermit, RwLock, Semaphore};

use crate::async_runtime::JoinHandle;
use crate::error::{AppError, AppResult};
use crate::mcp::protocol::RateLimiter;
use crate::settings::McpGatewayConfig;
use crate::workspace::WorkspaceProfile;

const GATEWAY_MAX_BODY_BYTES: usize = 1_048_576;
const GATEWAY_MAX_HEADER_BYTES: usize = 32 * 1024;
const GATEWAY_MAX_CONCURRENT_REQUESTS: usize = 64;
const GATEWAY_MAX_REQUESTS_PER_MINUTE: usize = 1_200;
const GATEWAY_MAX_REQUESTS_PER_WORKSPACE_PER_MINUTE: usize = 300;
const GATEWAY_REQUEST_BODY_TIMEOUT: Duration = Duration::from_secs(15);
const GATEWAY_UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const GATEWAY_UPSTREAM_HEADER_TIMEOUT: Duration = Duration::from_secs(15);
const GATEWAY_NON_SSE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(90);
const GATEWAY_SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
#[cfg(not(test))]
const GATEWAY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(test)]
const GATEWAY_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpGatewayStatus {
    pub state: String,
    pub local_endpoint: String,
    pub public_base_url: String,
    pub route_count: usize,
    pub owner_workspace_id: String,
    pub error: String,
}

pub fn tunnel_identity_signature(
    config: &McpGatewayConfig,
    owner: &WorkspaceProfile,
) -> AppResult<String> {
    let public_url =
        if owner.tunnel.tunnel_type == "cloudflare" && owner.tunnel.cloudflare_mode == "named" {
            let configured = config.public_url.trim().trim_end_matches('/');
            if configured.is_empty() {
                owner.tunnel.public_url.trim().trim_end_matches('/')
            } else {
                configured
            }
        } else {
            ""
        };
    serde_json::to_string(&serde_json::json!({
        "workspaceId": owner.id,
        "localPort": config.local_port,
        "type": owner.tunnel.tunnel_type,
        "frpServer": owner.tunnel.frp_server,
        "frpSubdomain": owner.tunnel.frp_subdomain,
        "frpProfileId": owner.tunnel.frp_profile_id,
        "frpServerPort": owner.tunnel.frp_server_port,
        "cloudflareMode": owner.tunnel.cloudflare_mode,
        "publicUrl": public_url,
        "useProxy": owner.tunnel.use_proxy,
    }))
    .map_err(|error| AppError::Message(format!("MCP Gateway 隧道配置序列化失败：{error}")))
}

pub fn observation_matches_tunnel(config: &McpGatewayConfig, signature: &str) -> bool {
    config.observed_owner_workspace_id == config.owner_workspace_id
        && (config.observed_tunnel_signature.trim().is_empty()
            || config.observed_tunnel_signature == signature)
}

type GatewayStreamError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Copy)]
enum ResponseDeadline {
    Total(tokio::time::Instant),
    Idle(Duration),
}

fn bounded_response_stream(
    upstream: reqwest::Response,
    permit: OwnedSemaphorePermit,
    is_sse: bool,
) -> impl futures_util::Stream<Item = Result<Bytes, GatewayStreamError>> {
    let deadline = if is_sse {
        ResponseDeadline::Idle(GATEWAY_SSE_IDLE_TIMEOUT)
    } else {
        ResponseDeadline::Total(tokio::time::Instant::now() + GATEWAY_NON_SSE_RESPONSE_TIMEOUT)
    };
    futures_util::stream::unfold(
        (upstream.bytes_stream(), Some(permit), deadline, false),
        move |(mut stream, permit, deadline, done)| async move {
            if done {
                return None;
            }
            let next = match deadline {
                ResponseDeadline::Total(deadline) => {
                    tokio::time::timeout_at(deadline, stream.next()).await
                }
                ResponseDeadline::Idle(duration) => {
                    tokio::time::timeout(duration, stream.next()).await
                }
            };
            match next {
                Ok(Some(Ok(bytes))) => Some((Ok(bytes), (stream, permit, deadline, false))),
                Ok(Some(Err(error))) => Some((
                    Err(Box::new(error) as GatewayStreamError),
                    (stream, permit, deadline, true),
                )),
                Ok(None) => None,
                Err(_) => Some((
                    Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        if is_sse {
                            "Gateway SSE upstream became idle"
                        } else {
                            "Gateway upstream response exceeded its deadline"
                        },
                    )) as GatewayStreamError),
                    (stream, permit, deadline, true),
                )),
            }
        },
    )
}

pub fn owner_tunnel_identity_changed(
    config: &McpGatewayConfig,
    current: &WorkspaceProfile,
    next: &WorkspaceProfile,
) -> bool {
    if !config.enabled || config.owner_workspace_id != current.id || current.id != next.id {
        return false;
    }

    current.tunnel.tunnel_type != next.tunnel.tunnel_type
        || current.tunnel.public_url != next.tunnel.public_url
        || current.tunnel.frp_server != next.tunnel.frp_server
        || current.tunnel.frp_subdomain != next.tunnel.frp_subdomain
        || current.tunnel.frp_profile_id != next.tunnel.frp_profile_id
        || current.tunnel.frp_server_port != next.tunnel.frp_server_port
        || current.tunnel.cloudflare_mode != next.tunnel.cloudflare_mode
        || current.tunnel.use_proxy != next.tunnel.use_proxy
}

impl McpGatewayStatus {
    fn stopped(config: &McpGatewayConfig) -> Self {
        Self {
            state: "stopped".into(),
            local_endpoint: format!("http://127.0.0.1:{}", config.local_port),
            public_base_url: config.effective_public_url(),
            route_count: 0,
            owner_workspace_id: config.owner_workspace_id.clone(),
            error: String::new(),
        }
    }
}

#[derive(Clone)]
struct GatewayState {
    routes: Arc<RwLock<HashMap<String, u16>>>,
    workspace_rate_limiters: Arc<RwLock<HashMap<String, RateLimiter>>>,
    client: reqwest::Client,
    rate_limiter: RateLimiter,
    concurrency: Arc<Semaphore>,
}

struct GatewayRuntime {
    port: u16,
    routes: std::sync::Arc<RwLock<HashMap<String, u16>>>,
    workspace_rate_limiters: Arc<RwLock<HashMap<String, RateLimiter>>>,
    shutdown: oneshot::Sender<()>,
    handle: JoinHandle<()>,
    server_error: Arc<RwLock<String>>,
}

#[derive(Default)]
struct GatewaySupervisor {
    runtime: Option<GatewayRuntime>,
    last_error: String,
}

static GATEWAY_SUPERVISOR: LazyLock<Mutex<GatewaySupervisor>> =
    LazyLock::new(|| Mutex::new(GatewaySupervisor::default()));

pub fn validate_config(config: &McpGatewayConfig, profiles: &[WorkspaceProfile]) -> AppResult<()> {
    if !config.enabled {
        return Ok(());
    }
    if config.local_port == 0 {
        return Err(AppError::Message("MCP Gateway 本地端口无效。".into()));
    }
    if !safe_workspace_segment(&config.owner_workspace_id) {
        return Err(AppError::Message(
            "MCP Gateway 必须选择有效的隧道所有者工作区。".into(),
        ));
    }
    if !profiles
        .iter()
        .any(|profile| profile.id == config.owner_workspace_id)
    {
        return Err(AppError::Message(
            "MCP Gateway 隧道所有者工作区不存在。".into(),
        ));
    }
    for profile in profiles {
        validate_workspace_ports(config, profile)?;
    }
    if !config.public_url.trim().is_empty() {
        validate_public_base_url(&config.public_url)?;
    }
    if !config.observed_public_url.trim().is_empty() {
        validate_public_base_url(&config.observed_public_url)?;
    }
    Ok(())
}

pub fn validate_workspace_ports(
    config: &McpGatewayConfig,
    profile: &WorkspaceProfile,
) -> AppResult<()> {
    if !config.enabled {
        return Ok(());
    }
    if profile.runtime.local_port == config.local_port
        || profile.actions.local_port == config.local_port
    {
        return Err(AppError::Message(format!(
            "MCP Gateway 端口 {} 已保留，不能作为工作区“{}”的 MCP 或 Actions 端口。",
            config.local_port, profile.name
        )));
    }
    Ok(())
}

pub fn ensure_workspace_is_not_owner(
    config: &McpGatewayConfig,
    workspace_id: &str,
) -> AppResult<()> {
    if config.enabled && config.owner_workspace_id == workspace_id {
        return Err(AppError::Message(
            "该工作区是 MCP Gateway 隧道所有者；请先更换 owner 或禁用 Gateway。".into(),
        ));
    }
    Ok(())
}

#[cfg(any(feature = "cli", test))]
pub fn workspace_base_url(config: &McpGatewayConfig, workspace_id: &str) -> AppResult<String> {
    if !safe_workspace_segment(workspace_id) {
        return Err(AppError::Message("工作区 ID 不能用于 Gateway URL。".into()));
    }
    let base = config.effective_public_url();
    Ok(format!("{base}/w/{workspace_id}"))
}

pub async fn ensure(
    config: &McpGatewayConfig,
    profiles: &[WorkspaceProfile],
    active_workspace_ids: &HashSet<String>,
) -> AppResult<McpGatewayStatus> {
    validate_config(config, profiles)?;
    let mut supervisor = GATEWAY_SUPERVISOR.lock().await;
    if !config.enabled || active_workspace_ids.is_empty() {
        supervisor.stop().await?;
        return Ok(McpGatewayStatus::stopped(config));
    }

    let routes = profiles
        .iter()
        .filter(|profile| active_workspace_ids.contains(&profile.id))
        .filter(|profile| safe_workspace_segment(&profile.id))
        .map(|profile| (profile.id.clone(), profile.runtime.local_port))
        .collect::<HashMap<_, _>>();
    if routes.is_empty() {
        supervisor.stop().await?;
        return Ok(McpGatewayStatus::stopped(config));
    }

    let must_restart = supervisor
        .runtime
        .as_ref()
        .is_some_and(|runtime| runtime.port != config.local_port || runtime.handle.is_finished());
    if must_restart {
        supervisor.stop().await?;
    }

    if let Some(runtime) = supervisor.runtime.as_ref() {
        let active_ids = routes.keys().cloned().collect::<HashSet<_>>();
        let mut limiters = runtime.workspace_rate_limiters.write().await;
        limiters.retain(|workspace_id, _| active_ids.contains(workspace_id));
        for workspace_id in active_ids {
            limiters.entry(workspace_id).or_insert_with(|| {
                RateLimiter::new(
                    GATEWAY_MAX_REQUESTS_PER_WORKSPACE_PER_MINUTE,
                    Duration::from_secs(60),
                )
            });
        }
        drop(limiters);
        *runtime.routes.write().await = routes;
    } else {
        match spawn(config.local_port, routes).await {
            Ok(runtime) => {
                supervisor.runtime = Some(runtime);
                supervisor.last_error.clear();
            }
            Err(error) => {
                supervisor.last_error = error.to_string();
                return Err(error);
            }
        }
    }

    let route_count = supervisor
        .runtime
        .as_ref()
        .map(|runtime| runtime.routes.clone())
        .expect("gateway runtime just initialized")
        .read()
        .await
        .len();
    Ok(McpGatewayStatus {
        state: "running".into(),
        local_endpoint: format!("http://127.0.0.1:{}", config.local_port),
        public_base_url: config.effective_public_url(),
        route_count,
        owner_workspace_id: config.owner_workspace_id.clone(),
        error: String::new(),
    })
}

pub async fn status(config: &McpGatewayConfig) -> McpGatewayStatus {
    let supervisor = GATEWAY_SUPERVISOR.lock().await;
    let Some(runtime) = supervisor.runtime.as_ref() else {
        let mut status = McpGatewayStatus::stopped(config);
        status.error = supervisor.last_error.clone();
        return status;
    };
    let state = if runtime.handle.is_finished() {
        "error"
    } else {
        "running"
    };
    let local_endpoint = format!("http://127.0.0.1:{}", runtime.port);
    let routes = runtime.routes.clone();
    let route_count = routes.read().await.len();
    let runtime_error = runtime.server_error.read().await.clone();
    let error = if !runtime_error.is_empty() {
        runtime_error
    } else if runtime.handle.is_finished() && supervisor.last_error.is_empty() {
        "MCP Gateway task exited unexpectedly".into()
    } else {
        supervisor.last_error.clone()
    };
    McpGatewayStatus {
        state: if !error.is_empty() {
            "error".into()
        } else {
            state.into()
        },
        local_endpoint,
        public_base_url: config.effective_public_url(),
        route_count,
        owner_workspace_id: config.owner_workspace_id.clone(),
        error,
    }
}

pub async fn stop() -> AppResult<()> {
    GATEWAY_SUPERVISOR.lock().await.stop().await
}

pub async fn record_runtime_error(message: impl Into<String>) {
    GATEWAY_SUPERVISOR.lock().await.last_error = message.into();
}

pub async fn clear_runtime_error() {
    GATEWAY_SUPERVISOR.lock().await.last_error.clear();
}

impl GatewaySupervisor {
    async fn stop(&mut self) -> AppResult<()> {
        let Some(runtime) = self.runtime.take() else {
            self.last_error.clear();
            return Ok(());
        };
        let _ = runtime.shutdown.send(());
        let mut handle = runtime.handle;
        match tokio::time::timeout(GATEWAY_SHUTDOWN_TIMEOUT, &mut handle).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.is_cancelled() => {}
            Ok(Err(error)) => {
                let message = format!("MCP Gateway 任务异常退出：{error}");
                self.last_error = message.clone();
                return Err(AppError::Message(message));
            }
            Err(_) => {
                handle.abort();
                let _ = handle.await;
            }
        }
        let server_error = runtime.server_error.read().await.clone();
        if !server_error.is_empty() {
            self.last_error = server_error.clone();
            return Err(AppError::Message(format!(
                "MCP Gateway 服务异常退出：{server_error}"
            )));
        }
        self.last_error.clear();
        Ok(())
    }
}

async fn spawn(port: u16, routes: HashMap<String, u16>) -> AppResult<GatewayRuntime> {
    spawn_with_limits(
        port,
        routes,
        GATEWAY_MAX_CONCURRENT_REQUESTS,
        GATEWAY_MAX_REQUESTS_PER_MINUTE,
        GATEWAY_MAX_REQUESTS_PER_WORKSPACE_PER_MINUTE,
    )
    .await
}

async fn spawn_with_limits(
    port: u16,
    routes: HashMap<String, u16>,
    max_concurrent_requests: usize,
    max_requests_per_minute: usize,
    max_requests_per_workspace_per_minute: usize,
) -> AppResult<GatewayRuntime> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
        AppError::Message(format!("MCP Gateway 本地端口 {port} 绑定失败：{error}"))
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| AppError::Message(format!("MCP Gateway 端口设置非阻塞失败：{error}")))?;
    let listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|error| AppError::Message(format!("MCP Gateway 初始化失败：{error}")))?;
    let workspace_rate_limiters = Arc::new(RwLock::new(
        routes
            .keys()
            .cloned()
            .map(|workspace_id| {
                (
                    workspace_id,
                    RateLimiter::new(
                        max_requests_per_workspace_per_minute,
                        Duration::from_secs(60),
                    ),
                )
            })
            .collect(),
    ));
    let routes = Arc::new(RwLock::new(routes));
    let server_error = Arc::new(RwLock::new(String::new()));
    let state = GatewayState {
        routes: routes.clone(),
        workspace_rate_limiters: workspace_rate_limiters.clone(),
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(GATEWAY_UPSTREAM_CONNECT_TIMEOUT)
            .build()
            .map_err(|error| AppError::Message(format!("MCP Gateway HTTP 客户端失败：{error}")))?,
        rate_limiter: RateLimiter::new(max_requests_per_minute, Duration::from_secs(60)),
        concurrency: Arc::new(Semaphore::new(max_concurrent_requests)),
    };
    let app = Router::new()
        .route("/w/{workspace_id}/{*upstream_path}", any(proxy_request))
        .with_state(state);
    let (shutdown, shutdown_rx) = oneshot::channel();
    let task_error = server_error.clone();
    let handle = crate::async_runtime::spawn(async move {
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
        {
            *task_error.write().await = error.to_string();
        }
    });
    Ok(GatewayRuntime {
        port,
        routes,
        workspace_rate_limiters,
        shutdown,
        handle,
        server_error,
    })
}

async fn proxy_request(
    State(state): State<GatewayState>,
    Path((workspace_id, upstream_path)): Path<(String, String)>,
    OriginalUri(original_uri): OriginalUri,
    request: Request,
) -> Response {
    if !safe_workspace_segment(&workspace_id) || !allowed_upstream_path(&upstream_path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (port, workspace_rate_limiter) = {
        let routes = state.routes.read().await;
        let Some(port) = routes.get(&workspace_id).copied() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        drop(routes);
        let limiters = state.workspace_rate_limiters.read().await;
        let Some(rate_limiter) = limiters.get(&workspace_id).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        (port, rate_limiter)
    };
    if !state.rate_limiter.allow() {
        return gateway_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Gateway request rate limit exceeded",
        );
    }
    if !workspace_rate_limiter.allow() {
        return gateway_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Workspace gateway request rate limit exceeded",
        );
    }
    let permit = match state.concurrency.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return gateway_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Gateway concurrency limit exceeded",
            )
        }
    };
    let (parts, request_body) = request.into_parts();
    if !allowed_method(&parts.method) {
        return gateway_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    if header_bytes(&parts.headers) > GATEWAY_MAX_HEADER_BYTES {
        return gateway_error(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "Gateway request headers are too large",
        );
    }
    let body = match tokio::time::timeout(
        GATEWAY_REQUEST_BODY_TIMEOUT,
        axum::body::to_bytes(request_body, GATEWAY_MAX_BODY_BYTES),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => {
            return gateway_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Gateway request body is too large",
            )
        }
        Err(_) => {
            return gateway_error(
                StatusCode::REQUEST_TIMEOUT,
                "Gateway request body timed out",
            )
        }
    };
    let query = original_uri
        .query()
        .map(|value| format!("?{value}"))
        .unwrap_or_default();
    let target = format!("http://127.0.0.1:{port}/{upstream_path}{query}");
    let request_connection_headers = connection_header_names(&parts.headers);
    let mut upstream_request = match state
        .client
        .request(parts.method, target)
        .body(body)
        .build()
    {
        Ok(request) => request,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    for (name, value) in &parts.headers {
        if !hop_by_hop_header(name.as_str())
            && !request_connection_headers.contains(name.as_str())
            && name.as_str() != "host"
            && name.as_str() != "content-length"
        {
            upstream_request
                .headers_mut()
                .append(name.clone(), value.clone());
        }
    }
    let upstream = match tokio::time::timeout(
        GATEWAY_UPSTREAM_HEADER_TIMEOUT,
        state.client.execute(upstream_request),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => return StatusCode::BAD_GATEWAY.into_response(),
        Err(_) => return StatusCode::GATEWAY_TIMEOUT.into_response(),
    };
    let status = upstream.status();
    let response_headers = upstream.headers().clone();
    if header_bytes(&response_headers) > GATEWAY_MAX_HEADER_BYTES {
        return gateway_error(
            StatusCode::BAD_GATEWAY,
            "Upstream response headers are too large",
        );
    }
    let response_connection_headers = connection_header_names(&response_headers);
    let is_sse = response_headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
    let stream = bounded_response_stream(upstream, permit, is_sse);
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    for (name, value) in &response_headers {
        if !hop_by_hop_header(name.as_str())
            && !response_connection_headers.contains(name.as_str())
            && name.as_str() != "content-length"
        {
            response.headers_mut().append(name.clone(), value.clone());
        }
    }
    response
}

fn gateway_error(status: StatusCode, message: &'static str) -> Response {
    (status, message).into_response()
}

fn allowed_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::POST | Method::DELETE)
}

fn header_bytes(headers: &HeaderMap) -> usize {
    headers
        .iter()
        .map(|(name, value)| name.as_str().len().saturating_add(value.as_bytes().len()))
        .sum()
}

fn connection_header_names(headers: &HeaderMap) -> HashSet<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn allowed_upstream_path(path: &str) -> bool {
    matches!(
        path,
        "mcp"
            | ".well-known/oauth-authorization-server"
            | ".well-known/oauth-protected-resource"
            | "oauth/authorize"
            | "oauth/token"
            | "canvs"
            | "canvs/"
    ) || path.starts_with("canvs/tasks/")
        || path == "canvs/api/tasks"
        || path.starts_with("canvs/api/tasks/")
}

fn safe_workspace_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_public_base_url(value: &str) -> AppResult<()> {
    let url = reqwest::Url::parse(value.trim())
        .map_err(|_| AppError::Message("MCP Gateway 公网地址不是有效 URL。".into()))?;
    let host = url.host_str().unwrap_or_default();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
    let secure = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !secure
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(AppError::Message(
            "MCP Gateway 公网地址必须使用 HTTPS（HTTP 仅允许 loopback），且不能包含凭据、子路径、查询参数或 fragment。".into(),
        ));
    }
    Ok(())
}

fn hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use serde_json::{json, Value};
    use std::convert::Infallible;

    async fn upstream_echo(OriginalUri(uri): OriginalUri, headers: HeaderMap) -> Response {
        let marker = headers
            .get("x-gateway-test")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        let hop = headers
            .get("x-remove-at-gateway")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        let mut response = format!("{}|{marker}|{hop}", uri).into_response();
        response.headers_mut().insert(
            "mcp-session-id",
            "upstream-session".parse().expect("session header"),
        );
        response
            .headers_mut()
            .append("set-cookie", "a=1".parse().expect("cookie"));
        response
            .headers_mut()
            .append("set-cookie", "b=2".parse().expect("cookie"));
        response
            .headers_mut()
            .insert("connection", "x-response-hop".parse().expect("connection"));
        response
            .headers_mut()
            .insert("x-response-hop", "secret".parse().expect("response hop"));
        response
    }

    async fn never_ending_stream() -> Response {
        let stream = futures_util::stream::pending::<Result<axum::body::Bytes, Infallible>>();
        let mut response = Response::new(Body::from_stream(stream));
        response.headers_mut().insert(
            "content-type",
            "text/event-stream".parse().expect("content type"),
        );
        response
    }

    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
        listener.local_addr().expect("local addr").port()
    }

    #[test]
    fn workspace_urls_are_stable_and_path_scoped() {
        let config = McpGatewayConfig {
            enabled: true,
            local_port: 28765,
            owner_workspace_id: "owner".into(),
            public_url: "https://mcp.example.com/".into(),
            ..McpGatewayConfig::default()
        };
        assert_eq!(
            workspace_base_url(&config, "workspace_a").unwrap(),
            "https://mcp.example.com/w/workspace_a"
        );
        assert!(workspace_base_url(&config, "../escape").is_err());

        let mut observed = config;
        observed.observed_public_url = "https://observed.example.com".into();
        observed.observed_owner_workspace_id = observed.owner_workspace_id.clone();
        assert_eq!(
            workspace_base_url(&observed, "workspace_a").unwrap(),
            "https://observed.example.com/w/workspace_a"
        );
        observed.owner_workspace_id = "other-owner".into();
        assert_eq!(
            workspace_base_url(&observed, "workspace_a").unwrap(),
            "https://mcp.example.com/w/workspace_a"
        );
    }

    #[test]
    fn observed_url_requires_matching_owner_and_tunnel_signature() {
        let mut config = McpGatewayConfig {
            enabled: true,
            owner_workspace_id: "owner".into(),
            observed_public_url: "https://observed.example.com".into(),
            observed_owner_workspace_id: "owner".into(),
            observed_tunnel_signature: "sig-a".into(),
            ..McpGatewayConfig::default()
        };
        assert!(observation_matches_tunnel(&config, "sig-a"));
        assert!(!observation_matches_tunnel(&config, "sig-b"));
        config.observed_owner_workspace_id = "other".into();
        assert!(!observation_matches_tunnel(&config, "sig-a"));
    }

    #[test]
    fn gateway_only_forwards_known_protocol_paths() {
        assert!(allowed_upstream_path("mcp"));
        assert!(allowed_upstream_path("oauth/token"));
        assert!(allowed_upstream_path("canvs"));
        assert!(allowed_upstream_path("canvs/tasks/task-123"));
        assert!(allowed_upstream_path("canvs/api/tasks/task-123"));
        assert!(!allowed_upstream_path("admin"));
        assert!(!allowed_upstream_path("mcp/extra"));
        assert!(!allowed_upstream_path("canvs-admin"));
        assert!(!allowed_upstream_path("canvs/api/tasks-extra"));
    }

    #[test]
    fn public_base_url_requires_https_and_site_root() {
        assert!(validate_public_base_url("https://mcp.example.com").is_ok());
        assert!(validate_public_base_url("http://127.0.0.1:28765").is_ok());
        assert!(validate_public_base_url("http://mcp.example.com").is_err());
        assert!(validate_public_base_url("https://mcp.example.com/base").is_err());
        assert!(validate_public_base_url("https://user@mcp.example.com").is_err());
    }

    #[test]
    fn gateway_port_must_not_overlap_workspace_services() {
        let mut profile = WorkspaceProfile::new("C:/workspace".into(), None);
        profile.id = "owner".into();
        let config = McpGatewayConfig {
            enabled: true,
            local_port: profile.runtime.local_port,
            owner_workspace_id: profile.id.clone(),
            ..McpGatewayConfig::default()
        };
        assert!(validate_config(&config, &[profile]).is_err());
    }

    #[test]
    fn enabled_gateway_owner_cannot_be_removed() {
        let config = McpGatewayConfig {
            enabled: true,
            local_port: 28765,
            owner_workspace_id: "owner".into(),
            ..McpGatewayConfig::default()
        };
        assert!(ensure_workspace_is_not_owner(&config, "owner").is_err());
        assert!(ensure_workspace_is_not_owner(&config, "other").is_ok());
    }

    #[tokio::test]
    async fn reverse_proxy_is_workspace_scoped_and_preserves_protocol_headers() {
        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream");
        let upstream_port = upstream_listener
            .local_addr()
            .expect("upstream addr")
            .port();
        let (upstream_shutdown, upstream_shutdown_rx) = oneshot::channel();
        let upstream_handle = tokio::spawn(async move {
            let app = Router::new().route("/oauth/authorize", get(upstream_echo));
            axum::serve(upstream_listener, app)
                .with_graceful_shutdown(async {
                    let _ = upstream_shutdown_rx.await;
                })
                .await
                .expect("upstream serve");
        });

        let gateway_port = free_port();
        let runtime = spawn(
            gateway_port,
            HashMap::from([("workspace-a".to_string(), upstream_port)]),
        )
        .await
        .expect("spawn gateway");
        let client = reqwest::Client::new();
        let response = client
            .get(format!(
                "http://127.0.0.1:{gateway_port}/w/workspace-a/oauth/authorize?state=abc"
            ))
            .header("x-gateway-test", "forwarded")
            .header("connection", "x-remove-at-gateway")
            .header("x-remove-at-gateway", "secret")
            .send()
            .await
            .expect("gateway request");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok()),
            Some("upstream-session")
        );
        assert_eq!(response.headers().get_all("set-cookie").iter().count(), 2);
        assert!(response.headers().get("x-response-hop").is_none());
        assert_eq!(
            response.text().await.expect("response text"),
            "/oauth/authorize?state=abc|forwarded|missing"
        );

        let unknown = client
            .get(format!("http://127.0.0.1:{gateway_port}/w/workspace-b/mcp"))
            .send()
            .await
            .expect("unknown workspace");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let disallowed = client
            .get(format!(
                "http://127.0.0.1:{gateway_port}/w/workspace-a/admin"
            ))
            .send()
            .await
            .expect("disallowed path");
        assert_eq!(disallowed.status(), StatusCode::NOT_FOUND);

        let _ = runtime.shutdown.send(());
        let _ = runtime.handle.await;
        let _ = upstream_shutdown.send(());
        let _ = upstream_handle.await;
    }

    #[test]
    fn method_and_header_budget_helpers_are_fail_closed() {
        assert!(allowed_method(&Method::GET));
        assert!(allowed_method(&Method::POST));
        assert!(allowed_method(&Method::DELETE));
        assert!(!allowed_method(&Method::PUT));

        let mut headers = HeaderMap::new();
        headers.insert("connection", "x-remove, keep-alive".parse().unwrap());
        let dynamic = connection_header_names(&headers);
        assert!(dynamic.contains("x-remove"));
        assert!(dynamic.contains("keep-alive"));
        headers.insert(
            "x-large",
            "a".repeat(GATEWAY_MAX_HEADER_BYTES)
                .parse()
                .expect("large header"),
        );
        assert!(header_bytes(&headers) > GATEWAY_MAX_HEADER_BYTES);
    }

    #[tokio::test]
    async fn gateway_rate_limit_rejects_excess_requests() {
        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream");
        let upstream_port = upstream_listener
            .local_addr()
            .expect("upstream addr")
            .port();
        let (upstream_shutdown, upstream_shutdown_rx) = oneshot::channel();
        let upstream_handle = tokio::spawn(async move {
            let app = Router::new().route("/oauth/authorize", get(upstream_echo));
            axum::serve(upstream_listener, app)
                .with_graceful_shutdown(async {
                    let _ = upstream_shutdown_rx.await;
                })
                .await
                .expect("upstream serve");
        });
        let gateway_port = free_port();
        let runtime = spawn_with_limits(
            gateway_port,
            HashMap::from([("workspace-a".to_string(), upstream_port)]),
            4,
            1,
            10,
        )
        .await
        .expect("gateway");
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://127.0.0.1:{gateway_port}/w/workspace-a/oauth/authorize");
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let mut supervisor = GatewaySupervisor {
            runtime: Some(runtime),
            last_error: String::new(),
        };
        supervisor.stop().await.unwrap();
        let _ = upstream_shutdown.send(());
        let _ = upstream_handle.await;
    }

    #[tokio::test]
    async fn workspace_rate_limit_does_not_consume_another_workspace_budget() {
        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream");
        let upstream_port = upstream_listener
            .local_addr()
            .expect("upstream addr")
            .port();
        let (upstream_shutdown, upstream_shutdown_rx) = oneshot::channel();
        let upstream_handle = tokio::spawn(async move {
            let app = Router::new().route("/oauth/authorize", get(upstream_echo));
            axum::serve(upstream_listener, app)
                .with_graceful_shutdown(async {
                    let _ = upstream_shutdown_rx.await;
                })
                .await
                .expect("upstream serve");
        });
        let gateway_port = free_port();
        let runtime = spawn_with_limits(
            gateway_port,
            HashMap::from([
                ("workspace-a".to_string(), upstream_port),
                ("workspace-b".to_string(), upstream_port),
            ]),
            4,
            10,
            1,
        )
        .await
        .expect("gateway");
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url_a = format!("http://127.0.0.1:{gateway_port}/w/workspace-a/oauth/authorize");
        let url_b = format!("http://127.0.0.1:{gateway_port}/w/workspace-b/oauth/authorize");
        assert_eq!(
            client.get(&url_a).send().await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            client.get(&url_a).send().await.unwrap().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            client.get(&url_b).send().await.unwrap().status(),
            StatusCode::OK
        );

        let mut supervisor = GatewaySupervisor {
            runtime: Some(runtime),
            last_error: String::new(),
        };
        supervisor.stop().await.unwrap();
        let _ = upstream_shutdown.send(());
        let _ = upstream_handle.await;
    }

    #[tokio::test]
    async fn gateway_concurrency_limit_is_held_for_stream_lifetime() {
        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream");
        let upstream_port = upstream_listener
            .local_addr()
            .expect("upstream addr")
            .port();
        let (upstream_shutdown, upstream_shutdown_rx) = oneshot::channel();
        let upstream_handle = tokio::spawn(async move {
            let app = Router::new().route("/oauth/authorize", get(never_ending_stream));
            axum::serve(upstream_listener, app)
                .with_graceful_shutdown(async {
                    let _ = upstream_shutdown_rx.await;
                })
                .await
                .expect("upstream serve");
        });
        let gateway_port = free_port();
        let runtime = spawn_with_limits(
            gateway_port,
            HashMap::from([("workspace-a".to_string(), upstream_port)]),
            1,
            10,
            10,
        )
        .await
        .expect("gateway");
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://127.0.0.1:{gateway_port}/w/workspace-a/oauth/authorize");
        let first = client.get(&url).send().await.expect("first stream");
        assert_eq!(first.status(), StatusCode::OK);
        let second = client.get(&url).send().await.expect("second request");
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);

        let mut supervisor = GatewaySupervisor {
            runtime: Some(runtime),
            last_error: String::new(),
        };
        supervisor.stop().await.unwrap();
        drop(first);
        let _ = upstream_shutdown.send(());
        let _ = upstream_handle.await;
    }

    async fn post_mcp(
        client: &reqwest::Client,
        url: &str,
        body: Value,
        session_id: Option<&str>,
    ) -> reqwest::Response {
        let mut request = client
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2025-11-25")
            .json(&body);
        if let Some(session_id) = session_id {
            request = request.header("mcp-session-id", session_id);
        }
        request.send().await.expect("MCP gateway request")
    }

    fn initialize_request(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "gateway-test", "version": "1" }
            }
        })
    }

    #[tokio::test]
    async fn real_workspace_listeners_reject_cross_workspace_sessions() {
        let root = tempfile::tempdir().expect("temp root");
        let workspace_a = root.path().join("workspace-a");
        let workspace_b = root.path().join("workspace-b");
        std::fs::create_dir_all(&workspace_a).expect("workspace a");
        std::fs::create_dir_all(&workspace_b).expect("workspace b");
        std::fs::write(workspace_b.join("secret.txt"), "workspace-b-secret")
            .expect("workspace b secret");

        let gateway_port = free_port();
        let port_a = free_port();
        let port_b = free_port();
        let auth = crate::workspace::AuthConfig {
            auth_type: "noauth".into(),
            ..crate::workspace::AuthConfig::default()
        };
        let runtime_config = crate::workspace::RuntimeConfig {
            strict_workspace_reads: true,
            ..crate::workspace::RuntimeConfig::default()
        };
        let (shutdown_a, handle_a) = crate::mcp::spawn_listener(
            port_a,
            workspace_a.clone(),
            "workspace-a".into(),
            "Workspace A".into(),
            auth.clone(),
            format!("http://127.0.0.1:{gateway_port}/w/workspace-a"),
            None,
            None,
            None,
            runtime_config.clone(),
        )
        .expect("listener a");
        let (shutdown_b, handle_b) = crate::mcp::spawn_listener(
            port_b,
            workspace_b.clone(),
            "workspace-b".into(),
            "Workspace B".into(),
            auth,
            format!("http://127.0.0.1:{gateway_port}/w/workspace-b"),
            None,
            None,
            None,
            runtime_config,
        )
        .expect("listener b");
        let gateway_runtime = spawn(
            gateway_port,
            HashMap::from([
                ("workspace-a".to_string(), port_a),
                ("workspace-b".to_string(), port_b),
            ]),
        )
        .await
        .expect("gateway");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");
        let url_a = format!("http://127.0.0.1:{gateway_port}/w/workspace-a/mcp");
        let url_b = format!("http://127.0.0.1:{gateway_port}/w/workspace-b/mcp");

        let initialized_a = post_mcp(&client, &url_a, initialize_request(1), None).await;
        assert_eq!(initialized_a.status(), StatusCode::OK);
        let session_a = initialized_a.headers()["mcp-session-id"]
            .to_str()
            .expect("session a")
            .to_string();
        let initialized_b = post_mcp(&client, &url_b, initialize_request(2), None).await;
        assert_eq!(initialized_b.status(), StatusCode::OK);
        let session_b = initialized_b.headers()["mcp-session-id"]
            .to_str()
            .expect("session b")
            .to_string();
        assert_ne!(session_a, session_b);

        let wrong_workspace = post_mcp(
            &client,
            &url_b,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
            Some(&session_a),
        )
        .await;
        assert_eq!(wrong_workspace.status(), StatusCode::NOT_FOUND);

        for (url, session) in [(&url_a, &session_a), (&url_b, &session_b)] {
            let notification = post_mcp(
                &client,
                url,
                json!({
                    "jsonrpc":"2.0",
                    "method":"notifications/initialized",
                    "params": {}
                }),
                Some(session),
            )
            .await;
            assert_eq!(notification.status(), StatusCode::ACCEPTED);
        }

        let cwd_a = post_mcp(
            &client,
            &url_a,
            json!({
                "jsonrpc":"2.0",
                "id":4,
                "method":"tools/call",
                "params":{"name":"get_default_cwd","arguments":{}}
            }),
            Some(&session_a),
        )
        .await
        .text()
        .await
        .expect("cwd a");
        let cwd_b = post_mcp(
            &client,
            &url_b,
            json!({
                "jsonrpc":"2.0",
                "id":5,
                "method":"tools/call",
                "params":{"name":"get_default_cwd","arguments":{}}
            }),
            Some(&session_b),
        )
        .await
        .text()
        .await
        .expect("cwd b");
        assert!(cwd_a.contains("workspace-a"));
        assert!(!cwd_a.contains("workspace-b"));
        assert!(cwd_b.contains("workspace-b"));
        assert!(!cwd_b.contains("workspace-a"));

        let cross_workspace_read = post_mcp(
            &client,
            &url_a,
            json!({
                "jsonrpc":"2.0",
                "id":6,
                "method":"tools/call",
                "params":{
                    "name":"read_file",
                    "arguments":{"path":workspace_b.join("secret.txt").display().to_string()}
                }
            }),
            Some(&session_a),
        )
        .await
        .text()
        .await
        .expect("cross workspace read response");
        assert!(cross_workspace_read.contains("PATH_OUTSIDE_WORKSPACE"));

        let own_workspace_read = post_mcp(
            &client,
            &url_b,
            json!({
                "jsonrpc":"2.0",
                "id":7,
                "method":"tools/call",
                "params":{
                    "name":"read_file",
                    "arguments":{"path":"secret.txt"}
                }
            }),
            Some(&session_b),
        )
        .await
        .text()
        .await
        .expect("own workspace read response");
        assert!(own_workspace_read.contains("workspace-b-secret"));

        *gateway_runtime.routes.write().await =
            HashMap::from([("workspace-b".to_string(), port_b)]);
        assert_eq!(
            post_mcp(
                &client,
                &url_a,
                json!({"jsonrpc":"2.0","id":8,"method":"ping"}),
                Some(&session_a)
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post_mcp(
                &client,
                &url_b,
                json!({"jsonrpc":"2.0","id":9,"method":"ping"}),
                Some(&session_b)
            )
            .await
            .status(),
            StatusCode::OK
        );

        let mut supervisor = GatewaySupervisor {
            runtime: Some(gateway_runtime),
            last_error: String::new(),
        };
        supervisor.stop().await.expect("stop gateway");
        let _ = shutdown_a.send(());
        let _ = shutdown_b.send(());
        let _ = handle_a.await;
        let _ = handle_b.await;
    }

    #[tokio::test]
    async fn shutdown_aborts_a_lingering_stream_after_the_deadline() {
        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream");
        let upstream_port = upstream_listener
            .local_addr()
            .expect("upstream addr")
            .port();
        let (upstream_shutdown, upstream_shutdown_rx) = oneshot::channel();
        let upstream_handle = tokio::spawn(async move {
            let app = Router::new().route("/oauth/authorize", get(never_ending_stream));
            axum::serve(upstream_listener, app)
                .with_graceful_shutdown(async {
                    let _ = upstream_shutdown_rx.await;
                })
                .await
                .expect("upstream serve");
        });
        let gateway_port = free_port();
        let runtime = spawn(
            gateway_port,
            HashMap::from([("workspace-a".to_string(), upstream_port)]),
        )
        .await
        .expect("gateway");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");
        let response = client
            .get(format!(
                "http://127.0.0.1:{gateway_port}/w/workspace-a/oauth/authorize"
            ))
            .send()
            .await
            .expect("stream response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut supervisor = GatewaySupervisor {
            runtime: Some(runtime),
            last_error: String::new(),
        };
        supervisor.stop().await.expect("forced gateway stop");
        drop(response);
        let _ = upstream_shutdown.send(());
        let _ = upstream_handle.await;
    }
}
