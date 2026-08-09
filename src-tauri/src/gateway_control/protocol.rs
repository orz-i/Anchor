use serde::{Deserialize, Serialize};

use crate::build_identity::BuildIdentity;
use crate::settings::McpGatewayConfig;

pub const GATEWAY_CONTROL_PROTOCOL_VERSION: u16 = 1;
pub const GATEWAY_LIFECYCLE_PROTOCOL_MIN_VERSION: u16 = 1;
pub const MAX_GATEWAY_CONTROL_FRAME_BYTES: usize = 64 * 1024;

pub const ERROR_PROTOCOL_VERSION_UNSUPPORTED: &str = "protocol_version_unsupported";
pub const ERROR_CONFIG_SCOPE_MISMATCH: &str = "config_scope_mismatch";
pub const ERROR_CONTROL_COMMAND_UNAVAILABLE: &str = "control_command_unavailable";
pub const ERROR_LOG_READ_FAILED: &str = "log_read_failed";
pub const ERROR_OPERATION_FAILED: &str = "operation_failed";
pub const ERROR_OPERATION_NOT_FOUND: &str = "operation_not_found";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRequest {
    pub protocol_version: u16,
    pub request_id: String,
    pub config_scope: String,
    #[serde(flatten)]
    pub method: GatewayMethod,
}

impl GatewayRequest {
    pub fn new(config_scope: String, method: GatewayMethod) -> Self {
        Self::with_protocol_version(config_scope, method, GATEWAY_CONTROL_PROTOCOL_VERSION)
    }

    pub fn with_protocol_version(
        config_scope: String,
        method: GatewayMethod,
        protocol_version: u16,
    ) -> Self {
        Self {
            protocol_version,
            request_id: uuid::Uuid::new_v4().to_string(),
            config_scope,
            method,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum GatewayMethod {
    Ping,
    Version,
    Status,
    Logs {
        #[serde(rename = "tailLines")]
        tail_lines: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<GatewayLogCursor>,
    },
    Events {
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<GatewayEventCursor>,
        limit: u32,
        #[serde(rename = "waitMs")]
        wait_ms: u32,
    },
    Shutdown,
    PrepareRestart,
    Reload,
    ApplyConfig {
        config: Box<McpGatewayConfig>,
    },
    OperationStatus {
        #[serde(rename = "operationId")]
        operation_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayOperation {
    Shutdown,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayAsyncState {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayControlError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAsyncOperation {
    pub operation_id: String,
    pub state: GatewayAsyncState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<GatewayControlError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayControlStatus {
    pub daemon_supported: bool,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_identity: Option<BuildIdentity>,
    pub state: String,
    pub local_endpoint: String,
    pub public_base_url: String,
    pub route_count: usize,
    pub route_workspace_ids: Vec<String>,
    pub owner_workspace_id: String,
    pub error: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayLogCursor {
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayLogChunk {
    pub name: String,
    pub path: String,
    pub content: String,
    pub next_offset: u64,
    pub exists: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayEventKind {
    DaemonReady,
    DaemonStopping,
    GatewayState,
    RouteState,
    TunnelState,
    Reload,
    ConfigApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayEventCursor {
    pub stream_id: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayEvent {
    pub sequence: u64,
    pub emitted_at_unix_ms: u64,
    pub kind: GatewayEventKind,
    pub state: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayEventBatch {
    pub events: Vec<GatewayEvent>,
    pub next_cursor: GatewayEventCursor,
    pub reset: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<GatewayResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<GatewayControlError>,
}

impl GatewayResponse {
    pub fn success(request_id: String, result: GatewayResult) -> Self {
        Self {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(request_id: String, code: &str, message: impl Into<String>) -> Self {
        Self {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id,
            ok: false,
            result: None,
            error: Some(GatewayControlError {
                code: code.to_string(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayResult {
    Pong {
        daemon_version: String,
    },
    Version {
        daemon_version: String,
        protocol_version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build_identity: Option<BuildIdentity>,
    },
    Status {
        status: Box<GatewayControlStatus>,
    },
    Logs {
        chunk: GatewayLogChunk,
    },
    Events {
        batch: GatewayEventBatch,
    },
    Accepted {
        operation: GatewayOperation,
        daemon_pid: u32,
    },
    OperationAccepted {
        operation_id: String,
        daemon_pid: u32,
    },
    OperationStatus {
        operation: GatewayAsyncOperation,
    },
}

pub fn validate_protocol_version(request: &GatewayRequest) -> Result<(), Box<GatewayResponse>> {
    if request.protocol_version == GATEWAY_CONTROL_PROTOCOL_VERSION {
        return Ok(());
    }
    Err(Box::new(GatewayResponse::error(
        request.request_id.clone(),
        ERROR_PROTOCOL_VERSION_UNSUPPORTED,
        format!(
            "unsupported Gateway control protocol version {}; daemon supports {}",
            request.protocol_version, GATEWAY_CONTROL_PROTOCOL_VERSION
        ),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_keeps_scope_version_and_method() {
        let request = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id: "request-1".into(),
            config_scope: "scope-1".into(),
            method: GatewayMethod::Reload,
        };
        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(value["protocolVersion"], GATEWAY_CONTROL_PROTOCOL_VERSION);
        assert_eq!(value["requestId"], "request-1");
        assert_eq!(value["configScope"], "scope-1");
        assert_eq!(value["method"], "reload");
        assert_eq!(
            serde_json::from_value::<GatewayRequest>(value).expect("deserialize request"),
            request
        );
    }

    #[test]
    fn unsupported_protocol_uses_stable_error_code() {
        let request = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION + 1,
            request_id: "request-2".into(),
            config_scope: "scope-1".into(),
            method: GatewayMethod::Ping,
        };
        let response = *validate_protocol_version(&request).expect_err("version must fail");
        assert_eq!(
            response.error.expect("error").code,
            ERROR_PROTOCOL_VERSION_UNSUPPORTED
        );
    }

    #[test]
    fn legacy_version_result_without_build_identity_still_decodes() {
        let result: GatewayResult = serde_json::from_value(serde_json::json!({
            "type": "version",
            "daemon_version": "0.1.22",
            "protocol_version": 1
        }))
        .expect("legacy Gateway version response");
        assert!(matches!(
            result,
            GatewayResult::Version {
                daemon_version,
                protocol_version: 1,
                build_identity: None,
            } if daemon_version == "0.1.22"
        ));
    }

    #[test]
    fn apply_config_request_has_a_stable_tagged_shape() {
        let config = McpGatewayConfig {
            enabled: true,
            local_port: 31_234,
            ..McpGatewayConfig::default()
        };
        let request = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id: "request-3".into(),
            config_scope: "scope-1".into(),
            method: GatewayMethod::ApplyConfig {
                config: Box::new(config),
            },
        };
        let value = serde_json::to_value(request).expect("serialize apply config");
        assert_eq!(value["method"], "apply_config");
        assert_eq!(value["configScope"], "scope-1");
        assert_eq!(value["config"]["enabled"], true);
        assert_eq!(value["config"]["localPort"], 31_234);
    }

    #[test]
    fn logs_and_events_are_additive_v1_method_tags() {
        let logs = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id: "request-logs".into(),
            config_scope: "scope-1".into(),
            method: GatewayMethod::Logs {
                tail_lines: 100,
                cursor: Some(GatewayLogCursor { offset: 42 }),
            },
        };
        let logs_value = serde_json::to_value(logs).expect("serialize logs request");
        assert_eq!(logs_value["protocolVersion"], 1);
        assert_eq!(logs_value["method"], "logs");
        assert_eq!(logs_value["tailLines"], 100);
        assert_eq!(logs_value["cursor"]["offset"], 42);

        let events = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id: "request-events".into(),
            config_scope: "scope-1".into(),
            method: GatewayMethod::Events {
                cursor: Some(GatewayEventCursor {
                    stream_id: "stream-1".into(),
                    sequence: 7,
                }),
                limit: 16,
                wait_ms: 5_000,
            },
        };
        let events_value = serde_json::to_value(events).expect("serialize events request");
        assert_eq!(events_value["protocolVersion"], 1);
        assert_eq!(events_value["method"], "events");
        assert_eq!(events_value["cursor"]["streamId"], "stream-1");
        assert_eq!(events_value["cursor"]["sequence"], 7);
        assert_eq!(events_value["limit"], 16);
        assert_eq!(events_value["waitMs"], 5_000);
    }
}
