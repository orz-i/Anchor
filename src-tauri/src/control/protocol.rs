use serde::{Deserialize, Serialize};

use super::WorkspaceControlStatus;

pub const CONTROL_PROTOCOL_VERSION: u16 = 1;
pub const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024;

pub const ERROR_PROTOCOL_VERSION_UNSUPPORTED: &str = "protocol_version_unsupported";
pub const ERROR_WORKSPACE_MISMATCH: &str = "workspace_mismatch";
pub const ERROR_INTERNAL: &str = "internal_error";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRequest {
    pub protocol_version: u16,
    pub request_id: String,
    #[serde(flatten)]
    pub method: ControlMethod,
}

impl ControlRequest {
    pub fn new(method: ControlMethod) -> Self {
        Self {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: uuid::Uuid::new_v4().to_string(),
            method,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum ControlMethod {
    Ping,
    Version,
    WorkspaceStatus {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ControlResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
}

impl ControlResponse {
    pub fn success(request_id: String, result: ControlResult) -> Self {
        Self {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(request_id: String, code: &str, message: impl Into<String>) -> Self {
        Self {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id,
            ok: false,
            result: None,
            error: Some(ControlError {
                code: code.to_string(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlResult {
    Pong {
        daemon_version: String,
    },
    Version {
        daemon_version: String,
        protocol_version: u16,
    },
    WorkspaceStatus {
        status: Box<WorkspaceControlStatus>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlError {
    pub code: String,
    pub message: String,
}

pub fn validate_protocol_version(request: &ControlRequest) -> Result<(), Box<ControlResponse>> {
    if request.protocol_version == CONTROL_PROTOCOL_VERSION {
        return Ok(());
    }
    Err(Box::new(ControlResponse::error(
        request.request_id.clone(),
        ERROR_PROTOCOL_VERSION_UNSUPPORTED,
        format!(
            "unsupported control protocol version {}; daemon supports {}",
            request.protocol_version, CONTROL_PROTOCOL_VERSION
        ),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_keeps_explicit_version_id_and_method() {
        let request = ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-1".into(),
            method: ControlMethod::WorkspaceStatus {
                workspace_id: "workspace-1".into(),
            },
        };

        let value = serde_json::to_value(&request).expect("serialize request");

        assert_eq!(value["protocolVersion"], CONTROL_PROTOCOL_VERSION);
        assert_eq!(value["requestId"], "request-1");
        assert_eq!(value["method"], "workspace_status");
        assert_eq!(value["workspaceId"], "workspace-1");
        assert_eq!(
            serde_json::from_value::<ControlRequest>(value).expect("deserialize request"),
            request
        );
    }

    #[test]
    fn unsupported_protocol_returns_stable_error() {
        let request = ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION + 1,
            request_id: "request-2".into(),
            method: ControlMethod::Ping,
        };

        let response = *validate_protocol_version(&request).expect_err("version must fail");

        assert_eq!(response.protocol_version, CONTROL_PROTOCOL_VERSION);
        assert_eq!(response.request_id, request.request_id);
        assert!(!response.ok);
        assert_eq!(
            response.error.expect("protocol error").code,
            ERROR_PROTOCOL_VERSION_UNSUPPORTED
        );
    }
}
