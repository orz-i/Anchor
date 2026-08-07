use serde::{Deserialize, Serialize};

use crate::settings::McpGatewayConfig;

pub const GATEWAY_CONTROL_PROTOCOL_VERSION: u16 = 1;
pub const MAX_GATEWAY_CONTROL_FRAME_BYTES: usize = 64 * 1024;

pub const ERROR_PROTOCOL_VERSION_UNSUPPORTED: &str = "protocol_version_unsupported";
pub const ERROR_CONFIG_SCOPE_MISMATCH: &str = "config_scope_mismatch";
pub const ERROR_CONTROL_COMMAND_UNAVAILABLE: &str = "control_command_unavailable";
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
        Self {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
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
    },
    Status {
        status: Box<GatewayControlStatus>,
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
}
