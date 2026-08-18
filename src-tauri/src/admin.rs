use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::Path as AxumPath;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::control::{self, ControlPlaneEventCursor};
use crate::data::DataStore;
use crate::error::{AppError, AppResult};

pub const ADMIN_API_VERSION: u16 = 1;
pub const DEFAULT_ADMIN_PORT: u16 = 28_769;

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
                "mode": "read_only_bootstrap",
                "url": format!("http://{actual}")
            }))?
        );
    } else {
        println!(
            "Web Admin bootstrap 已启动：http://{actual}（API v{ADMIN_API_VERSION}，当前仅开放只读迁移命令）"
        );
    }

    axum::serve(listener, router())
        .await
        .map_err(|error| AppError::Message(format!("Web Admin 服务异常：{error}")))
}

fn router() -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/commands/{command}", post(command))
}

async fn health() -> Json<Value> {
    Json(json!({
        "ok": true,
        "data": {
            "apiVersion": ADMIN_API_VERSION,
            "buildVersion": env!("CARGO_PKG_VERSION"),
            "mode": "read_only_bootstrap",
            "mutationsEnabled": false
        }
    }))
}

async fn command(
    headers: HeaderMap,
    AxumPath(command): AxumPath<String>,
    Json(request): Json<AdminCommandRequest>,
) -> Response {
    if let Err(message) = validate_headers(&headers) {
        return error_response(StatusCode::FORBIDDEN, "ADMIN_REQUEST_REJECTED", message);
    }
    match dispatch_read_command(&command, request.args).await {
        Ok(data) => (StatusCode::OK, Json(json!({ "ok": true, "data": data }))).into_response(),
        Err(AdminDispatchError::NotMigrated) => error_response(
            StatusCode::NOT_IMPLEMENTED,
            "ADMIN_COMMAND_NOT_MIGRATED",
            format!("Web Admin 命令尚未迁移：{command}"),
        ),
        Err(AdminDispatchError::Failed(error)) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ADMIN_COMMAND_FAILED",
            error.to_string(),
        ),
    }
}

fn validate_headers(headers: &HeaderMap) -> Result<(), String> {
    let marker = headers
        .get("x-anchor-admin-request")
        .and_then(|value| value.to_str().ok());
    if marker != Some("1") {
        return Err("缺少 Web Admin 请求标记。".into());
    }
    if let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) {
        let loopback = origin == "http://127.0.0.1"
            || origin.starts_with("http://127.0.0.1:")
            || origin == "http://localhost"
            || origin.starts_with("http://localhost:")
            || origin == "http://[::1]"
            || origin.starts_with("http://[::1]:");
        if !loopback {
            return Err(format!("拒绝非 loopback Origin：{origin}"));
        }
    }
    Ok(())
}

enum AdminDispatchError {
    NotMigrated,
    Failed(AppError),
}

impl From<AppError> for AdminDispatchError {
    fn from(value: AppError) -> Self {
        Self::Failed(value)
    }
}

async fn dispatch_read_command(command: &str, args: Value) -> Result<Value, AdminDispatchError> {
    match command {
        "list_workspaces" => {
            let store = DataStore::load()?;
            serde_json::to_value(store.list())
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
        "get_last_workspace_id" => {
            let store = DataStore::load()?;
            Ok(Value::String(store.settings().last_workspace_id))
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

    #[test]
    fn admin_header_guard_accepts_loopback_and_rejects_remote_origin() {
        let mut headers = HeaderMap::new();
        headers.insert("x-anchor-admin-request", HeaderValue::from_static("1"));
        headers.insert("origin", HeaderValue::from_static("http://127.0.0.1:28769"));
        assert!(validate_headers(&headers).is_ok());

        headers.insert("origin", HeaderValue::from_static("https://example.com"));
        assert!(validate_headers(&headers).is_err());
    }

    #[tokio::test]
    async fn unimplemented_command_never_falls_through_to_mutation() {
        assert!(matches!(
            dispatch_read_command("set_proxy", json!({})).await,
            Err(AdminDispatchError::NotMigrated)
        ));
        assert!(matches!(
            dispatch_read_command("get_shared_secret", json!({})).await,
            Err(AdminDispatchError::NotMigrated)
        ));
    }
}
