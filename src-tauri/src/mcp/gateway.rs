use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use serde::Serialize;
use tokio::sync::{oneshot, Mutex, RwLock};

use crate::async_runtime::JoinHandle;
use crate::error::{AppError, AppResult};
use crate::settings::McpGatewayConfig;
use crate::workspace::WorkspaceProfile;

const GATEWAY_MAX_BODY_BYTES: usize = 1_048_576;

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

impl McpGatewayStatus {
    fn stopped(config: &McpGatewayConfig) -> Self {
        Self {
            state: "stopped".into(),
            local_endpoint: format!("http://127.0.0.1:{}", config.local_port),
            public_base_url: config.public_url.trim().trim_end_matches('/').to_string(),
            route_count: 0,
            owner_workspace_id: config.owner_workspace_id.clone(),
            error: String::new(),
        }
    }
}

#[derive(Clone)]
struct GatewayState {
    routes: std::sync::Arc<RwLock<HashMap<String, u16>>>,
    client: reqwest::Client,
}

struct GatewayRuntime {
    port: u16,
    routes: std::sync::Arc<RwLock<HashMap<String, u16>>>,
    shutdown: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct GatewaySupervisor {
    runtime: Option<GatewayRuntime>,
    last_error: String,
}

static GATEWAY_SUPERVISOR: LazyLock<Mutex<GatewaySupervisor>> =
    LazyLock::new(|| Mutex::new(GatewaySupervisor::default()));

pub fn validate_config(
    config: &McpGatewayConfig,
    profiles: &[WorkspaceProfile],
) -> AppResult<()> {
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

#[allow(dead_code)]
pub fn workspace_base_url(config: &McpGatewayConfig, workspace_id: &str) -> AppResult<String> {
    if !safe_workspace_segment(workspace_id) {
        return Err(AppError::Message("工作区 ID 不能用于 Gateway URL。".into()));
    }
    let base = if config.public_url.trim().is_empty() {
        format!("http://127.0.0.1:{}", config.local_port)
    } else {
        config.public_url.trim().trim_end_matches('/').to_string()
    };
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
        supervisor.stop().await;
        return Ok(McpGatewayStatus::stopped(config));
    }

    let routes = profiles
        .iter()
        .filter(|profile| active_workspace_ids.contains(&profile.id))
        .filter(|profile| safe_workspace_segment(&profile.id))
        .map(|profile| (profile.id.clone(), profile.runtime.local_port))
        .collect::<HashMap<_, _>>();
    if routes.is_empty() {
        supervisor.stop().await;
        return Ok(McpGatewayStatus::stopped(config));
    }

    let must_restart = supervisor.runtime.as_ref().is_some_and(|runtime| {
        runtime.port != config.local_port || runtime.handle.is_finished()
    });
    if must_restart {
        supervisor.stop().await;
    }

    if let Some(runtime) = supervisor.runtime.as_ref() {
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
        public_base_url: config.public_url.trim().trim_end_matches('/').to_string(),
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
    let error = supervisor.last_error.clone();
    McpGatewayStatus {
        state: state.into(),
        local_endpoint,
        public_base_url: config.public_url.trim().trim_end_matches('/').to_string(),
        route_count,
        owner_workspace_id: config.owner_workspace_id.clone(),
        error,
    }
}

pub async fn stop() {
    GATEWAY_SUPERVISOR.lock().await.stop().await;
}

impl GatewaySupervisor {
    async fn stop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let _ = runtime.shutdown.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), runtime.handle).await;
    }
}

async fn spawn(port: u16, routes: HashMap<String, u16>) -> AppResult<GatewayRuntime> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
        AppError::Message(format!("MCP Gateway 本地端口 {port} 绑定失败：{error}"))
    })?;
    listener.set_nonblocking(true).map_err(|error| {
        AppError::Message(format!("MCP Gateway 端口设置非阻塞失败：{error}"))
    })?;
    let listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|error| AppError::Message(format!("MCP Gateway 初始化失败：{error}")))?;
    let routes = std::sync::Arc::new(RwLock::new(routes));
    let state = GatewayState {
        routes: routes.clone(),
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| AppError::Message(format!("MCP Gateway HTTP 客户端失败：{error}")))?,
    };
    let app = Router::new()
        .route("/health", get(gateway_health))
        .route("/w/{workspace_id}/{*upstream_path}", any(proxy_request))
        .layer(axum::extract::DefaultBodyLimit::max(GATEWAY_MAX_BODY_BYTES))
        .with_state(state);
    let (shutdown, shutdown_rx) = oneshot::channel();
    let handle = crate::async_runtime::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });
    Ok(GatewayRuntime {
        port,
        routes,
        shutdown,
        handle,
    })
}

async fn gateway_health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn proxy_request(
    State(state): State<GatewayState>,
    Path((workspace_id, upstream_path)): Path<(String, String)>,
    OriginalUri(original_uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !safe_workspace_segment(&workspace_id) || !allowed_upstream_path(&upstream_path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let port = {
        let routes = state.routes.read().await;
        routes.get(&workspace_id).copied()
    };
    let Some(port) = port else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let query = original_uri
        .query()
        .map(|value| format!("?{value}"))
        .unwrap_or_default();
    let target = format!("http://127.0.0.1:{port}/{upstream_path}{query}");
    let mut request = state.client.request(method, target).body(body);
    for (name, value) in &headers {
        if !hop_by_hop_header(name.as_str()) && name.as_str() != "host" {
            request = request.header(name, value);
        }
    }
    let upstream = match request.send().await {
        Ok(response) => response,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let status = upstream.status();
    let response_headers = upstream.headers().clone();
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    for (name, value) in &response_headers {
        if !hop_by_hop_header(name.as_str()) && name.as_str() != "content-length" {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

fn allowed_upstream_path(path: &str) -> bool {
    matches!(
        path,
        "mcp"
            | ".well-known/oauth-authorization-server"
            | ".well-known/oauth-protected-resource"
            | "oauth/authorize"
            | "oauth/token"
    )
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
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::Message(
            "MCP Gateway 公网地址必须是无凭据、无查询参数的 HTTP(S) 基础 URL。".into(),
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

    async fn upstream_echo(
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
    ) -> Response {
        let marker = headers
            .get("x-gateway-test")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        let mut response = format!("{}|{marker}", uri).into_response();
        response.headers_mut().insert(
            "mcp-session-id",
            "upstream-session".parse().expect("session header"),
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
        };
        assert_eq!(
            workspace_base_url(&config, "workspace_a").unwrap(),
            "https://mcp.example.com/w/workspace_a"
        );
        assert!(workspace_base_url(&config, "../escape").is_err());
    }

    #[test]
    fn gateway_only_forwards_known_protocol_paths() {
        assert!(allowed_upstream_path("mcp"));
        assert!(allowed_upstream_path("oauth/token"));
        assert!(!allowed_upstream_path("admin"));
        assert!(!allowed_upstream_path("mcp/extra"));
    }

    #[test]
    fn gateway_port_must_not_overlap_workspace_services() {
        let mut profile = WorkspaceProfile::new("C:/workspace".into(), None);
        profile.id = "owner".into();
        let config = McpGatewayConfig {
            enabled: true,
            local_port: profile.runtime.local_port,
            owner_workspace_id: profile.id.clone(),
            public_url: String::new(),
        };
        assert!(validate_config(&config, &[profile]).is_err());
    }

    #[test]
    fn enabled_gateway_owner_cannot_be_removed() {
        let config = McpGatewayConfig {
            enabled: true,
            local_port: 28765,
            owner_workspace_id: "owner".into(),
            public_url: String::new(),
        };
        assert!(ensure_workspace_is_not_owner(&config, "owner").is_err());
        assert!(ensure_workspace_is_not_owner(&config, "other").is_ok());
    }

    #[tokio::test]
    async fn reverse_proxy_is_workspace_scoped_and_preserves_protocol_headers() {
        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream");
        let upstream_port = upstream_listener.local_addr().expect("upstream addr").port();
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
        assert_eq!(
            response.text().await.expect("response text"),
            "/oauth/authorize?state=abc|forwarded"
        );

        let unknown = client
            .get(format!(
                "http://127.0.0.1:{gateway_port}/w/workspace-b/mcp"
            ))
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
}
