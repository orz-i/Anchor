use serde::{Deserialize, Serialize};

use super::WorkspaceControlStatus;
use crate::build_identity::BuildIdentity;
use crate::tunnel::{TunnelServiceKind, TunnelStatus};

pub const CONTROL_PROTOCOL_VERSION: u16 = 7;
pub const CONTROL_LIFECYCLE_PROTOCOL_MIN_VERSION: u16 = 2;
pub const CONTROL_CAPABILITY_ZERO_DOWNTIME_HANDOFF_V1: &str = "zero_downtime_handoff_v1";
pub const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024;

pub const ERROR_PROTOCOL_VERSION_UNSUPPORTED: &str = "protocol_version_unsupported";
pub const ERROR_WORKSPACE_MISMATCH: &str = "workspace_mismatch";
pub const ERROR_CONTROL_COMMAND_UNAVAILABLE: &str = "control_command_unavailable";
pub const ERROR_LOG_READ_FAILED: &str = "log_read_failed";
pub const ERROR_OPERATION_FAILED: &str = "operation_failed";
pub const ERROR_OPERATION_NOT_FOUND: &str = "operation_not_found";
pub const ERROR_CONFIG_HOT_UPDATE_FAILED: &str = "config_hot_update_failed";
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
        Self::with_protocol_version(method, CONTROL_PROTOCOL_VERSION)
    }

    pub fn with_protocol_version(method: ControlMethod, protocol_version: u16) -> Self {
        Self {
            protocol_version,
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
    Logs {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        selection: ControlLogSelection,
        #[serde(rename = "tailLines")]
        tail_lines: u32,
        cursors: Vec<ControlLogCursor>,
    },
    Shutdown {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
    PrepareRestart {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
    PrepareHandoff {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        #[serde(rename = "initiatorPid")]
        initiator_pid: u32,
        #[serde(rename = "executablePath")]
        executable_path: String,
        #[serde(rename = "expectedBuild")]
        expected_build: BuildIdentity,
    },
    TunnelControl {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        service: TunnelServiceKind,
        action: ControlTunnelAction,
    },
    OperationStatus {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        #[serde(rename = "operationId")]
        operation_id: String,
    },
    Events {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<ControlEventCursor>,
        limit: u32,
        #[serde(rename = "waitMs")]
        wait_ms: u32,
    },
    Reload {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        service: ControlService,
    },
    ApplyConfig {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
    UpdateOauthRedirectPolicy {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        service: ControlService,
        #[serde(rename = "redirectUris")]
        redirect_uris: String,
        #[serde(rename = "redirectHosts")]
        redirect_hosts: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlService {
    Mcp,
    Actions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlEventKind {
    DaemonReady,
    DaemonStopping,
    ServiceState,
    TunnelState,
    McpActivity,
    Reload,
    ConfigApply,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEventCursor {
    pub stream_id: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEvent {
    pub sequence: u64,
    pub emitted_at_unix_ms: u64,
    pub kind: ControlEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ControlService>,
    pub state: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEventBatch {
    pub events: Vec<ControlEvent>,
    pub next_cursor: ControlEventCursor,
    pub reset: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlTunnelAction {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAsyncState {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlAsyncOperation {
    pub operation_id: String,
    pub state: ControlAsyncState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_status: Option<TunnelStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_apply: Option<ControlConfigApplyResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlConfigApplyResult {
    pub changed: bool,
    pub mcp_listener_reloaded: bool,
    pub actions_listener_reloaded: bool,
    pub mcp_callback_hot_updated: bool,
    pub actions_callback_hot_updated: bool,
    pub mcp_tunnel_reloaded: bool,
    pub actions_tunnel_reloaded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlLogSelection {
    Daemon,
    Mcp,
    Actions,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLogCursor {
    pub name: String,
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLogChunk {
    pub name: String,
    pub path: String,
    pub content: String,
    pub next_offset: u64,
    pub exists: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperation {
    Shutdown,
    Restart,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build_identity: Option<BuildIdentity>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
    WorkspaceStatus {
        status: Box<WorkspaceControlStatus>,
    },
    Logs {
        chunks: Vec<ControlLogChunk>,
    },
    Accepted {
        operation: ControlOperation,
        daemon_pid: u32,
    },
    OperationAccepted {
        operation_id: String,
        daemon_pid: u32,
    },
    OperationStatus {
        operation: ControlAsyncOperation,
    },
    Events {
        batch: ControlEventBatch,
    },
    ConfigHotUpdated {
        applied: bool,
        daemon_pid: u32,
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

    #[test]
    fn legacy_version_result_without_build_identity_still_decodes() {
        let result: ControlResult = serde_json::from_value(serde_json::json!({
            "type": "version",
            "daemon_version": "0.1.22",
            "protocol_version": 5
        }))
        .expect("legacy version response");
        assert!(matches!(
            result,
            ControlResult::Version {
                daemon_version,
                protocol_version: 5,
                build_identity: None,
                capabilities,
            } if daemon_version == "0.1.22"
                && capabilities.is_empty()
        ));
    }

    #[test]
    fn explicit_protocol_request_keeps_stable_lifecycle_shape() {
        let request = ControlRequest::with_protocol_version(
            ControlMethod::PrepareRestart {
                workspace_id: "workspace-1".into(),
            },
            CONTROL_LIFECYCLE_PROTOCOL_MIN_VERSION,
        );
        let value = serde_json::to_value(request).expect("serialize legacy lifecycle request");
        assert_eq!(
            value["protocolVersion"],
            CONTROL_LIFECYCLE_PROTOCOL_MIN_VERSION
        );
        assert_eq!(value["method"], "prepare_restart");
        assert_eq!(value["workspaceId"], "workspace-1");
    }

    #[test]
    fn write_and_log_methods_keep_stable_json_shapes() {
        let request = ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-3".into(),
            method: ControlMethod::Logs {
                workspace_id: "workspace-1".into(),
                selection: ControlLogSelection::All,
                tail_lines: 50,
                cursors: vec![ControlLogCursor {
                    name: "daemon".into(),
                    offset: 12,
                }],
            },
        };

        let value = serde_json::to_value(&request).expect("serialize logs request");

        assert_eq!(value["method"], "logs");
        assert_eq!(value["workspaceId"], "workspace-1");
        assert_eq!(value["selection"], "all");
        assert_eq!(value["tailLines"], 50);
        assert_eq!(value["cursors"][0]["name"], "daemon");

        let restart = serde_json::to_value(ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-4".into(),
            method: ControlMethod::PrepareRestart {
                workspace_id: "workspace-1".into(),
            },
        })
        .expect("serialize restart request");
        assert_eq!(restart["method"], "prepare_restart");

        let tunnel = serde_json::to_value(ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-5".into(),
            method: ControlMethod::TunnelControl {
                workspace_id: "workspace-1".into(),
                service: crate::tunnel::TunnelServiceKind::Actions,
                action: ControlTunnelAction::Restart,
            },
        })
        .expect("serialize tunnel request");
        assert_eq!(tunnel["method"], "tunnel_control");
        assert_eq!(tunnel["workspaceId"], "workspace-1");
        assert_eq!(tunnel["service"], "actions");
        assert_eq!(tunnel["action"], "restart");

        let operation = serde_json::to_value(ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-6".into(),
            method: ControlMethod::OperationStatus {
                workspace_id: "workspace-1".into(),
                operation_id: "operation-1".into(),
            },
        })
        .expect("serialize operation status request");
        assert_eq!(operation["method"], "operation_status");
        assert_eq!(operation["operationId"], "operation-1");

        let events = serde_json::to_value(ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-7".into(),
            method: ControlMethod::Events {
                workspace_id: "workspace-1".into(),
                cursor: Some(ControlEventCursor {
                    stream_id: "stream-1".into(),
                    sequence: 9,
                }),
                limit: 32,
                wait_ms: 15_000,
            },
        })
        .expect("serialize event request");
        assert_eq!(events["method"], "events");
        assert_eq!(events["cursor"]["streamId"], "stream-1");
        assert_eq!(events["cursor"]["sequence"], 9);
        assert_eq!(events["waitMs"], 15_000);

        let reload = serde_json::to_value(ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-8".into(),
            method: ControlMethod::Reload {
                workspace_id: "workspace-1".into(),
                service: ControlService::Mcp,
            },
        })
        .expect("serialize reload request");
        assert_eq!(reload["method"], "reload");
        assert_eq!(reload["service"], "mcp");

        let hot_update = serde_json::to_value(ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-9".into(),
            method: ControlMethod::UpdateOauthRedirectPolicy {
                workspace_id: "workspace-1".into(),
                service: ControlService::Actions,
                redirect_uris: "https://chatgpt.com/callback".into(),
                redirect_hosts: "*.chatgpt.com".into(),
            },
        })
        .expect("serialize hot update request");
        assert_eq!(hot_update["method"], "update_oauth_redirect_policy");
        assert_eq!(hot_update["workspaceId"], "workspace-1");
        assert_eq!(hot_update["service"], "actions");
        assert_eq!(hot_update["redirectUris"], "https://chatgpt.com/callback");
        assert_eq!(hot_update["redirectHosts"], "*.chatgpt.com");

        let apply = serde_json::to_value(ControlRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "request-10".into(),
            method: ControlMethod::ApplyConfig {
                workspace_id: "workspace-1".into(),
            },
        })
        .expect("serialize config apply request");
        assert_eq!(apply["method"], "apply_config");
        assert_eq!(apply["workspaceId"], "workspace-1");
    }
}
