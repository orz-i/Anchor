#[cfg(any(feature = "cli", test))]
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
#[cfg(any(feature = "cli", test))]
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::daemon;
use crate::error::{AppError, AppResult};
use crate::tunnel::{TunnelServiceKind, TunnelStatus};
use crate::workspace::WorkspaceProfile;

#[cfg(any(unix, test))]
use super::events::read_workspace_events;
use super::events::{MAX_EVENT_BATCH, MAX_EVENT_WAIT_MS};
#[cfg(any(unix, test))]
use super::logs::read_log_batch;
#[cfg(any(feature = "cli", test))]
use super::protocol::ControlError;
#[cfg(any(unix, test))]
use super::protocol::{
    validate_protocol_version, ERROR_CONTROL_COMMAND_UNAVAILABLE, ERROR_INTERNAL,
    ERROR_LOG_READ_FAILED, ERROR_OPERATION_NOT_FOUND, ERROR_WORKSPACE_MISMATCH,
};
use super::protocol::{
    ControlAsyncOperation, ControlAsyncState, ControlConfigApplyResult, ControlEventBatch,
    ControlEventCursor, ControlLogChunk, ControlLogCursor, ControlLogSelection, ControlMethod,
    ControlOperation, ControlRequest, ControlResponse, ControlResult, ControlService,
    ControlTunnelAction, MAX_CONTROL_FRAME_BYTES,
};
use super::{workspace_status, WorkspaceControlStatus};

const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
const CONTROL_OPERATION_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(any(unix, test))]
const MAX_CONTROL_OPERATIONS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonControlCommand {
    Shutdown {
        operation: ControlOperation,
    },
    Tunnel {
        operation_id: String,
        service: TunnelServiceKind,
        action: ControlTunnelAction,
    },
    Reload {
        operation_id: String,
        service: ControlService,
    },
    ApplyConfig {
        operation_id: String,
    },
}

#[cfg(any(feature = "cli", test))]
pub(crate) fn finish_config_apply_operation(
    operation_id: &str,
    result: AppResult<ControlConfigApplyResult>,
) {
    let mut operations = operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(stored) = operations.get_mut(operation_id) else {
        return;
    };
    match result {
        Ok(result) => {
            stored.operation.state = ControlAsyncState::Succeeded;
            stored.operation.tunnel_status = None;
            stored.operation.config_apply = Some(result);
            stored.operation.error = None;
        }
        Err(error) => {
            stored.operation.state = ControlAsyncState::Failed;
            stored.operation.tunnel_status = None;
            stored.operation.config_apply = None;
            stored.operation.error = Some(ControlError {
                code: super::protocol::ERROR_OPERATION_FAILED.to_string(),
                message: error.to_string(),
            });
        }
    }
}

pub async fn request_apply_config_operation(
    profile: &WorkspaceProfile,
    timeout: Duration,
) -> Result<ControlConfigApplyResult, ControlClientError> {
    let inspection = daemon::inspect(profile).map_err(|error| {
        ControlClientError::Protocol(format!(
            "cannot inspect daemon before config apply: {error}"
        ))
    })?;
    let state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .ok_or_else(|| {
            ControlClientError::Protocol(
                "workspace daemon is not running; config apply requires the daemon control plane"
                    .into(),
            )
        })?;
    let (operation_id, daemon_pid) = match request(
        &profile.id,
        ControlMethod::ApplyConfig {
            workspace_id: profile.id.clone(),
        },
    )
    .await?
    {
        ControlResult::OperationAccepted {
            operation_id,
            daemon_pid,
        } => (operation_id, daemon_pid),
        other => {
            return Err(ControlClientError::Protocol(format!(
                "daemon returned unexpected config apply result: {other:?}"
            )))
        }
    };
    if daemon_pid != state.pid {
        return Err(ControlClientError::Protocol(format!(
            "daemon config apply PID mismatch: status file={}, response={daemon_pid}",
            state.pid
        )));
    }
    let operation = wait_for_operation(profile, &operation_id, timeout).await?;
    match operation.state {
        ControlAsyncState::Succeeded => operation.config_apply.ok_or_else(|| {
            ControlClientError::Protocol("successful config apply omitted result".into())
        }),
        ControlAsyncState::Failed => {
            let error = operation.error.ok_or_else(|| {
                ControlClientError::Protocol("failed config apply omitted error details".into())
            })?;
            Err(ControlClientError::Remote {
                code: error.code,
                message: error.message,
            })
        }
        ControlAsyncState::Pending | ControlAsyncState::Running => {
            Err(ControlClientError::Protocol(
                "config apply operation wait returned before reaching terminal state".into(),
            ))
        }
    }
}

pub async fn request_oauth_redirect_policy_update(
    profile: &WorkspaceProfile,
    service: ControlService,
    redirect_uris: &str,
    redirect_hosts: &str,
) -> Result<bool, ControlClientError> {
    let inspection = daemon::inspect(profile).map_err(|error| {
        ControlClientError::Protocol(format!(
            "cannot inspect daemon before OAuth policy update: {error}"
        ))
    })?;
    let state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .ok_or_else(|| {
            ControlClientError::Protocol(
                "workspace daemon is not running; OAuth policy update requires the daemon control plane"
                    .into(),
            )
        })?;
    let selected = match service {
        ControlService::Mcp => state.service.includes_mcp(),
        ControlService::Actions => state.service.includes_actions(),
    };
    if !selected {
        return Ok(false);
    }
    match request(
        &profile.id,
        ControlMethod::UpdateOauthRedirectPolicy {
            workspace_id: profile.id.clone(),
            service,
            redirect_uris: redirect_uris.to_string(),
            redirect_hosts: redirect_hosts.to_string(),
        },
    )
    .await?
    {
        ControlResult::ConfigHotUpdated {
            applied,
            daemon_pid,
        } => {
            if daemon_pid != state.pid {
                return Err(ControlClientError::Protocol(format!(
                    "daemon OAuth policy update PID mismatch: status file={}, response={daemon_pid}",
                    state.pid
                )));
            }
            Ok(applied)
        }
        other => Err(ControlClientError::Protocol(format!(
            "daemon returned unexpected OAuth policy update result: {other:?}"
        ))),
    }
}

async fn wait_for_operation(
    profile: &WorkspaceProfile,
    operation_id: &str,
    timeout: Duration,
) -> Result<ControlAsyncOperation, ControlClientError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let operation = match request(
            &profile.id,
            ControlMethod::OperationStatus {
                workspace_id: profile.id.clone(),
                operation_id: operation_id.to_string(),
            },
        )
        .await?
        {
            ControlResult::OperationStatus { operation } => operation,
            other => {
                return Err(ControlClientError::Protocol(format!(
                    "daemon returned unexpected operation status result: {other:?}"
                )))
            }
        };
        if matches!(
            operation.state,
            ControlAsyncState::Succeeded | ControlAsyncState::Failed
        ) {
            return Ok(operation);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ControlClientError::Remote {
                code: "operation_timeout".into(),
                message: format!("control operation {operation_id} did not finish before timeout"),
            });
        }
        tokio::time::sleep(CONTROL_OPERATION_POLL_INTERVAL).await;
    }
}

#[cfg(any(feature = "cli", test))]
pub(crate) fn finish_reload_operation(operation_id: &str, result: AppResult<()>) {
    let mut operations = operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(stored) = operations.get_mut(operation_id) else {
        return;
    };
    match result {
        Ok(()) => {
            stored.operation.state = ControlAsyncState::Succeeded;
            stored.operation.tunnel_status = None;
            stored.operation.config_apply = None;
            stored.operation.error = None;
        }
        Err(error) => {
            stored.operation.state = ControlAsyncState::Failed;
            stored.operation.tunnel_status = None;
            stored.operation.config_apply = None;
            stored.operation.error = Some(ControlError {
                code: super::protocol::ERROR_OPERATION_FAILED.to_string(),
                message: error.to_string(),
            });
        }
    }
}

pub async fn request_reload_operation(
    profile: &WorkspaceProfile,
    service: ControlService,
    timeout: Duration,
) -> Result<(), ControlClientError> {
    let inspection = daemon::inspect(profile).map_err(|error| {
        ControlClientError::Protocol(format!("cannot inspect daemon before reload: {error}"))
    })?;
    let state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .ok_or_else(|| {
            ControlClientError::Protocol(
                "workspace daemon is not running; reload requires the daemon control plane".into(),
            )
        })?;
    let selected = match service {
        ControlService::Mcp => state.service.includes_mcp(),
        ControlService::Actions => state.service.includes_actions(),
    };
    if !selected {
        return Err(ControlClientError::Protocol(format!(
            "daemon is not currently running the requested {} service; reload only applies to active services",
            match service {
                ControlService::Mcp => "mcp",
                ControlService::Actions => "actions",
            }
        )));
    }
    let (operation_id, daemon_pid) = match request(
        &profile.id,
        ControlMethod::Reload {
            workspace_id: profile.id.clone(),
            service,
        },
    )
    .await?
    {
        ControlResult::OperationAccepted {
            operation_id,
            daemon_pid,
        } => (operation_id, daemon_pid),
        other => {
            return Err(ControlClientError::Protocol(format!(
                "daemon returned unexpected reload result: {other:?}"
            )))
        }
    };
    if daemon_pid != state.pid {
        return Err(ControlClientError::Protocol(format!(
            "daemon reload PID mismatch: status file={}, response={daemon_pid}",
            state.pid
        )));
    }

    let operation = wait_for_operation(profile, &operation_id, timeout).await?;
    match operation.state {
        ControlAsyncState::Succeeded => Ok(()),
        ControlAsyncState::Failed => {
            let error = operation.error.ok_or_else(|| {
                ControlClientError::Protocol("failed reload omitted error details".into())
            })?;
            Err(ControlClientError::Remote {
                code: error.code,
                message: error.message,
            })
        }
        ControlAsyncState::Pending | ControlAsyncState::Running => {
            Err(ControlClientError::Protocol(
                "reload operation wait returned before reaching a terminal state".into(),
            ))
        }
    }
}

pub async fn request_events(
    profile: &WorkspaceProfile,
    cursor: Option<ControlEventCursor>,
    limit: u32,
    wait_ms: u32,
) -> Result<ControlEventBatch, ControlClientError> {
    let wait_ms = wait_ms.min(MAX_EVENT_WAIT_MS);
    let timeout = Duration::from_millis(u64::from(wait_ms).saturating_add(1_000));
    match request_with_timeout(
        &profile.id,
        ControlMethod::Events {
            workspace_id: profile.id.clone(),
            cursor,
            limit: limit.clamp(1, MAX_EVENT_BATCH),
            wait_ms,
        },
        timeout,
    )
    .await?
    {
        ControlResult::Events { batch } => Ok(batch),
        other => Err(ControlClientError::Protocol(format!(
            "daemon returned unexpected events result: {other:?}"
        ))),
    }
}

#[derive(Debug, Clone)]
#[cfg(any(feature = "cli", test))]
struct StoredControlOperation {
    #[cfg(any(unix, test))]
    workspace_id: String,
    operation: ControlAsyncOperation,
}

#[cfg(any(feature = "cli", test))]
fn operation_store() -> &'static Mutex<HashMap<String, StoredControlOperation>> {
    static STORE: OnceLock<Mutex<HashMap<String, StoredControlOperation>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(any(unix, test))]
fn create_control_operation(workspace_id: &str) -> Option<String> {
    let operation_id = uuid::Uuid::new_v4().to_string();
    let mut operations = operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if operations.len() >= MAX_CONTROL_OPERATIONS {
        let completed = operations
            .iter()
            .find(|(_, stored)| {
                matches!(
                    stored.operation.state,
                    ControlAsyncState::Succeeded | ControlAsyncState::Failed
                )
            })
            .map(|(operation_id, _)| operation_id.clone());
        let completed = completed?;
        operations.remove(&completed);
    }
    operations.insert(
        operation_id.clone(),
        StoredControlOperation {
            workspace_id: workspace_id.to_string(),
            operation: ControlAsyncOperation {
                operation_id: operation_id.clone(),
                state: ControlAsyncState::Pending,
                tunnel_status: None,
                config_apply: None,
                error: None,
            },
        },
    );
    Some(operation_id)
}

#[cfg(any(unix, test))]
fn control_operation(workspace_id: &str, operation_id: &str) -> Option<ControlAsyncOperation> {
    operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(operation_id)
        .filter(|stored| stored.workspace_id == workspace_id)
        .map(|stored| stored.operation.clone())
}

#[cfg(any(feature = "cli", test))]
pub(crate) fn mark_control_operation_running(operation_id: &str) {
    if let Some(stored) = operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_mut(operation_id)
    {
        stored.operation.state = ControlAsyncState::Running;
        stored.operation.error = None;
    }
}

pub async fn request_tunnel_operation(
    profile: &WorkspaceProfile,
    service: TunnelServiceKind,
    action: ControlTunnelAction,
    timeout: Duration,
) -> Result<TunnelStatus, ControlClientError> {
    let inspection = daemon::inspect(profile).map_err(|error| {
        ControlClientError::Protocol(format!(
            "cannot inspect daemon before tunnel write: {error}"
        ))
    })?;
    let state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .ok_or_else(|| {
            ControlClientError::Protocol(
                "workspace daemon is not running; tunnel writes require the daemon control plane"
                    .into(),
            )
        })?;
    let (operation_id, daemon_pid) = match request(
        &profile.id,
        ControlMethod::TunnelControl {
            workspace_id: profile.id.clone(),
            service,
            action,
        },
    )
    .await?
    {
        ControlResult::OperationAccepted {
            operation_id,
            daemon_pid,
        } => (operation_id, daemon_pid),
        other => {
            return Err(ControlClientError::Protocol(format!(
                "daemon returned unexpected tunnel operation result: {other:?}"
            )))
        }
    };
    if daemon_pid != state.pid {
        return Err(ControlClientError::Protocol(format!(
            "daemon tunnel operation PID mismatch: status file={}, response={daemon_pid}",
            state.pid
        )));
    }

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let operation = match request(
            &profile.id,
            ControlMethod::OperationStatus {
                workspace_id: profile.id.clone(),
                operation_id: operation_id.clone(),
            },
        )
        .await?
        {
            ControlResult::OperationStatus { operation } => operation,
            other => {
                return Err(ControlClientError::Protocol(format!(
                    "daemon returned unexpected operation status result: {other:?}"
                )))
            }
        };
        match operation.state {
            ControlAsyncState::Succeeded => {
                return operation.tunnel_status.ok_or_else(|| {
                    ControlClientError::Protocol(
                        "successful tunnel operation omitted tunnel status".into(),
                    )
                })
            }
            ControlAsyncState::Failed => {
                let error = operation.error.ok_or_else(|| {
                    ControlClientError::Protocol(
                        "failed tunnel operation omitted error details".into(),
                    )
                })?;
                return Err(ControlClientError::Remote {
                    code: error.code,
                    message: error.message,
                });
            }
            ControlAsyncState::Pending | ControlAsyncState::Running => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ControlClientError::Remote {
                code: "operation_timeout".into(),
                message: format!("tunnel operation {operation_id} did not finish before timeout"),
            });
        }
        tokio::time::sleep(CONTROL_OPERATION_POLL_INTERVAL).await;
    }
}

#[cfg(any(feature = "cli", test))]
pub(crate) fn finish_tunnel_operation(operation_id: &str, result: AppResult<TunnelStatus>) {
    let mut operations = operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(stored) = operations.get_mut(operation_id) else {
        return;
    };
    match result {
        Ok(status) => {
            stored.operation.state = ControlAsyncState::Succeeded;
            stored.operation.tunnel_status = Some(status);
            stored.operation.config_apply = None;
            stored.operation.error = None;
        }
        Err(error) => {
            stored.operation.state = ControlAsyncState::Failed;
            stored.operation.tunnel_status = None;
            stored.operation.config_apply = None;
            stored.operation.error = Some(ControlError {
                code: super::protocol::ERROR_OPERATION_FAILED.to_string(),
                message: error.to_string(),
            });
        }
    }
}

pub async fn request_logs(
    profile: &WorkspaceProfile,
    selection: ControlLogSelection,
    tail_lines: u32,
    cursors: Vec<ControlLogCursor>,
) -> Result<Vec<ControlLogChunk>, ControlClientError> {
    match request(
        &profile.id,
        ControlMethod::Logs {
            workspace_id: profile.id.clone(),
            selection,
            tail_lines,
            cursors,
        },
    )
    .await?
    {
        ControlResult::Logs { chunks } => Ok(chunks),
        other => Err(ControlClientError::Protocol(format!(
            "daemon returned unexpected logs result: {other:?}"
        ))),
    }
}

pub async fn request_daemon_exit(
    profile_id: &str,
    operation: ControlOperation,
) -> Result<u32, ControlClientError> {
    let method = match operation {
        ControlOperation::Shutdown => ControlMethod::Shutdown {
            workspace_id: profile_id.to_string(),
        },
        ControlOperation::Restart => ControlMethod::PrepareRestart {
            workspace_id: profile_id.to_string(),
        },
    };
    match request(profile_id, method).await? {
        ControlResult::Accepted {
            operation: accepted,
            daemon_pid,
        } if accepted == operation => Ok(daemon_pid),
        other => Err(ControlClientError::Protocol(format!(
            "daemon returned unexpected lifecycle result: {other:?}"
        ))),
    }
}

pub type DaemonControlSender = tokio::sync::mpsc::UnboundedSender<DaemonControlCommand>;
pub type DaemonControlReceiver = tokio::sync::mpsc::UnboundedReceiver<DaemonControlCommand>;

pub fn control_channel() -> (DaemonControlSender, DaemonControlReceiver) {
    tokio::sync::mpsc::unbounded_channel()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", content = "address", rename_all = "snake_case")]
pub enum LocalControlEndpoint {
    UnixSocket(PathBuf),
    WindowsNamedPipe(String),
}

pub fn endpoint(profile_id: &str) -> AppResult<LocalControlEndpoint> {
    #[cfg(unix)]
    {
        return Ok(LocalControlEndpoint::UnixSocket(
            daemon::control_socket_path(profile_id)?,
        ));
    }
    #[cfg(windows)]
    {
        return Ok(LocalControlEndpoint::WindowsNamedPipe(
            daemon::control_pipe_name(profile_id)?,
        ));
    }
    #[allow(unreachable_code)]
    Err(AppError::Message(
        "local daemon control transport is unsupported on this platform".into(),
    ))
}

#[derive(Debug)]
pub enum ControlClientError {
    Unavailable(String),
    Protocol(String),
    Remote { code: String, message: String },
    Io(io::Error),
}

impl ControlClientError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl fmt::Display for ControlClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Protocol(message) => formatter.write_str(message),
            Self::Remote { code, message } => write!(formatter, "daemon error {code}: {message}"),
            Self::Io(error) => write!(formatter, "daemon control I/O error: {error}"),
        }
    }
}

impl std::error::Error for ControlClientError {}

pub async fn workspace_status_via_daemon_or_local(
    profile: &WorkspaceProfile,
) -> AppResult<WorkspaceControlStatus> {
    match request_workspace_status(profile).await {
        Ok(status) => Ok(status),
        Err(error) if error.is_unavailable() => workspace_status(profile),
        Err(error) => Err(AppError::Message(error.to_string())),
    }
}

pub async fn request_workspace_status(
    profile: &WorkspaceProfile,
) -> Result<WorkspaceControlStatus, ControlClientError> {
    let result = request(
        &profile.id,
        ControlMethod::WorkspaceStatus {
            workspace_id: profile.id.clone(),
        },
    )
    .await?;
    match result {
        ControlResult::WorkspaceStatus { status } => Ok(*status),
        other => Err(ControlClientError::Protocol(format!(
            "daemon returned unexpected result for workspace status: {other:?}"
        ))),
    }
}

pub async fn ping(profile_id: &str) -> Result<(), ControlClientError> {
    match request(profile_id, ControlMethod::Ping).await? {
        ControlResult::Pong { .. } => Ok(()),
        other => Err(ControlClientError::Protocol(format!(
            "daemon returned unexpected ping result: {other:?}"
        ))),
    }
}

async fn request(
    profile_id: &str,
    method: ControlMethod,
) -> Result<ControlResult, ControlClientError> {
    request_with_timeout(profile_id, method, CONTROL_REQUEST_TIMEOUT).await
}

async fn request_with_timeout(
    profile_id: &str,
    method: ControlMethod,
    timeout: Duration,
) -> Result<ControlResult, ControlClientError> {
    let endpoint = endpoint(profile_id).map_err(|error| {
        ControlClientError::Protocol(format!("cannot resolve daemon control endpoint: {error}"))
    })?;
    let request = ControlRequest::new(method);
    let request_id = request.request_id.clone();
    let response = tokio::time::timeout(timeout, exchange_with_endpoint(&endpoint, &request))
        .await
        .map_err(|_| {
            ControlClientError::Unavailable(format!(
                "daemon control endpoint timed out: {endpoint:?}"
            ))
        })??;

    if response.protocol_version != super::protocol::CONTROL_PROTOCOL_VERSION {
        return Err(ControlClientError::Protocol(format!(
            "daemon responded with protocol version {}; client supports {}",
            response.protocol_version,
            super::protocol::CONTROL_PROTOCOL_VERSION
        )));
    }
    if response.request_id != request_id {
        return Err(ControlClientError::Protocol(format!(
            "daemon response request id mismatch: expected {request_id}, got {}",
            response.request_id
        )));
    }
    if !response.ok {
        let error = response.error.ok_or_else(|| {
            ControlClientError::Protocol("daemon returned ok=false without an error".into())
        })?;
        return Err(ControlClientError::Remote {
            code: error.code,
            message: error.message,
        });
    }
    response.result.ok_or_else(|| {
        ControlClientError::Protocol("daemon returned ok=true without a result".into())
    })
}

#[cfg(unix)]
async fn exchange_with_endpoint(
    endpoint: &LocalControlEndpoint,
    request: &ControlRequest,
) -> Result<ControlResponse, ControlClientError> {
    let LocalControlEndpoint::UnixSocket(path) = endpoint else {
        return Err(ControlClientError::Protocol(
            "Unix client received a non-Unix control endpoint".into(),
        ));
    };
    let mut stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(classify_connect_error)?;
    exchange(&mut stream, request).await
}

#[cfg(windows)]
async fn exchange_with_endpoint(
    endpoint: &LocalControlEndpoint,
    request: &ControlRequest,
) -> Result<ControlResponse, ControlClientError> {
    let LocalControlEndpoint::WindowsNamedPipe(name) = endpoint else {
        return Err(ControlClientError::Protocol(
            "Windows client received a non-pipe control endpoint".into(),
        ));
    };
    let mut stream = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(name)
        .map_err(classify_connect_error)?;
    exchange(&mut stream, request).await
}

#[cfg(not(any(unix, windows)))]
async fn exchange_with_endpoint(
    _endpoint: &LocalControlEndpoint,
    _request: &ControlRequest,
) -> Result<ControlResponse, ControlClientError> {
    Err(ControlClientError::Unavailable(
        "daemon control transport is unsupported on this platform".into(),
    ))
}

async fn exchange<S>(
    stream: &mut S,
    request: &ControlRequest,
) -> Result<ControlResponse, ControlClientError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    write_json_frame(stream, request)
        .await
        .map_err(ControlClientError::Io)?;
    read_json_frame(stream).await.map_err(|error| match error {
        FrameError::Io(error) => ControlClientError::Io(error),
        FrameError::Protocol(message) => ControlClientError::Protocol(message),
    })
}

fn classify_connect_error(error: io::Error) -> ControlClientError {
    if is_unavailable_io(&error) {
        ControlClientError::Unavailable(format!("daemon control endpoint unavailable: {error}"))
    } else {
        ControlClientError::Io(error)
    }
}

fn is_unavailable_io(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    ) || matches!(
        error.raw_os_error(),
        Some(2) | Some(53) | Some(109) | Some(231) | Some(233)
    )
}

#[derive(Debug)]
enum FrameError {
    Io(io::Error),
    Protocol(String),
}

async fn read_json_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let bytes = read_frame(reader).await.map_err(FrameError::Io)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| FrameError::Protocol(format!("invalid daemon control JSON: {error}")))
}

async fn read_frame<R>(reader: &mut R) -> io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut frame = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let read = reader.read(&mut byte).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "daemon control connection closed before newline",
            ));
        }
        if byte[0] == b'\n' {
            return Ok(frame);
        }
        frame.push(byte[0]);
        if frame.len() > MAX_CONTROL_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "daemon control frame exceeds maximum size",
            ));
        }
    }
}

async fn write_json_frame<W, T>(writer: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let encoded = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if encoded.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon control frame exceeds maximum size",
        ));
    }
    writer.write_all(&encoded).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

pub struct ControlServer {
    endpoint: LocalControlEndpoint,
    task: tokio::task::JoinHandle<()>,
}

impl ControlServer {
    #[cfg(unix)]
    pub fn start(
        profile: WorkspaceProfile,
        command_sender: DaemonControlSender,
    ) -> AppResult<Self> {
        use std::os::unix::fs::PermissionsExt;

        let endpoint = endpoint(&profile.id)?;
        let LocalControlEndpoint::UnixSocket(path) = &endpoint else {
            return Err(AppError::Message(
                "Unix daemon resolved a non-Unix control endpoint".into(),
            ));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let task_profile = profile.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(connection) => connection,
                    Err(error) => {
                        crate::tunnel::append_profile_log(
                            &task_profile.id,
                            "daemon.log",
                            &format!("[control] accept failed: {error}"),
                        );
                        break;
                    }
                };
                let profile = task_profile.clone();
                let command_sender = command_sender.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, &profile, &command_sender).await {
                        crate::tunnel::append_profile_log(
                            &profile.id,
                            "daemon.log",
                            &format!("[control] request failed: {error}"),
                        );
                    }
                });
            }
        });
        Ok(Self { endpoint, task })
    }

    #[cfg(not(unix))]
    pub fn start(
        _profile: WorkspaceProfile,
        _command_sender: DaemonControlSender,
    ) -> AppResult<Self> {
        Err(AppError::Message(
            "daemon control server is not enabled on this platform yet".into(),
        ))
    }

    pub fn endpoint(&self) -> &LocalControlEndpoint {
        &self.endpoint
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.task.abort();
        if let LocalControlEndpoint::UnixSocket(path) = &self.endpoint {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(any(unix, test))]
async fn handle_connection<S>(
    mut stream: S,
    profile: &WorkspaceProfile,
    command_sender: &DaemonControlSender,
) -> AppResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request: ControlRequest = read_json_frame(&mut stream)
        .await
        .map_err(|error| AppError::Message(format!("invalid control request: {error:?}")))?;
    let handled = handle_request(request, profile, !command_sender.is_closed()).await;
    write_json_frame(&mut stream, &handled.response).await?;
    stream.shutdown().await?;
    if let Some(command) = handled.command {
        enum AsyncCommandKind {
            Tunnel(String),
            Reload(String),
            ApplyConfig(String),
        }
        let async_command = match &command {
            DaemonControlCommand::Tunnel { operation_id, .. } => {
                Some(AsyncCommandKind::Tunnel(operation_id.clone()))
            }
            DaemonControlCommand::Reload { operation_id, .. } => {
                Some(AsyncCommandKind::Reload(operation_id.clone()))
            }
            DaemonControlCommand::ApplyConfig { operation_id } => {
                Some(AsyncCommandKind::ApplyConfig(operation_id.clone()))
            }
            DaemonControlCommand::Shutdown { .. } => None,
        };
        if command_sender.send(command).is_err() {
            let error = AppError::Message("daemon control command receiver is unavailable".into());
            match async_command {
                Some(AsyncCommandKind::Tunnel(operation_id)) => finish_tunnel_operation(
                    &operation_id,
                    Err(AppError::Message(error.to_string())),
                ),
                Some(AsyncCommandKind::Reload(operation_id)) => finish_reload_operation(
                    &operation_id,
                    Err(AppError::Message(error.to_string())),
                ),
                Some(AsyncCommandKind::ApplyConfig(operation_id)) => finish_config_apply_operation(
                    &operation_id,
                    Err(AppError::Message(error.to_string())),
                ),
                None => {}
            }
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(any(unix, test))]
struct HandledRequest {
    response: ControlResponse,
    command: Option<DaemonControlCommand>,
}

#[cfg(any(unix, test))]
async fn handle_request(
    request: ControlRequest,
    profile: &WorkspaceProfile,
    command_available: bool,
) -> HandledRequest {
    if let Err(response) = validate_protocol_version(&request) {
        return handled(*response);
    }
    let request_id = request.request_id;
    match request.method {
        ControlMethod::Ping => handled(ControlResponse::success(
            request_id,
            ControlResult::Pong {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
            },
        )),
        ControlMethod::Version => handled(ControlResponse::success(
            request_id,
            ControlResult::Version {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                protocol_version: super::protocol::CONTROL_PROTOCOL_VERSION,
            },
        )),
        ControlMethod::WorkspaceStatus { workspace_id } => {
            if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
                return handled(response);
            }
            match workspace_status(profile) {
                Ok(mut status) => {
                    if status
                        .daemon
                        .state
                        .as_ref()
                        .filter(|_| status.daemon.running)
                        .is_some_and(|state| state.service.includes_mcp())
                    {
                        status.mcp_activity = Some(crate::mcp::activity_snapshot(&profile.id));
                    }
                    let settings = match crate::settings::AppSettings::load() {
                        Ok(settings) => settings,
                        Err(error) => {
                            return handled(ControlResponse::error(
                                request_id,
                                ERROR_INTERNAL,
                                error.to_string(),
                            ));
                        }
                    };
                    let tunnels = crate::tunnel::supervisor().lock().await;
                    status.mcp_tunnel = Some(tunnels.status(
                        profile,
                        crate::tunnel::TunnelServiceKind::Mcp,
                        &settings,
                    ));
                    status.actions_tunnel = Some(tunnels.status(
                        profile,
                        crate::tunnel::TunnelServiceKind::Actions,
                        &settings,
                    ));
                    handled(ControlResponse::success(
                        request_id,
                        ControlResult::WorkspaceStatus {
                            status: Box::new(status),
                        },
                    ))
                }
                Err(error) => handled(ControlResponse::error(
                    request_id,
                    ERROR_INTERNAL,
                    error.to_string(),
                )),
            }
        }
        ControlMethod::Logs {
            workspace_id,
            selection,
            tail_lines,
            cursors,
        } => {
            if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
                return handled(response);
            }
            match read_log_batch(profile, selection, tail_lines, &cursors) {
                Ok(chunks) => handled(ControlResponse::success(
                    request_id,
                    ControlResult::Logs { chunks },
                )),
                Err(error) => handled(ControlResponse::error(
                    request_id,
                    ERROR_LOG_READ_FAILED,
                    error.to_string(),
                )),
            }
        }
        ControlMethod::Shutdown { workspace_id } => lifecycle_request(
            request_id,
            profile,
            workspace_id,
            ControlOperation::Shutdown,
            command_available,
        ),
        ControlMethod::PrepareRestart { workspace_id } => lifecycle_request(
            request_id,
            profile,
            workspace_id,
            ControlOperation::Restart,
            command_available,
        ),
        ControlMethod::TunnelControl {
            workspace_id,
            service,
            action,
        } => {
            if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
                return handled(response);
            }
            if !command_available {
                return handled(ControlResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    "daemon tunnel command receiver is unavailable",
                ));
            }
            let Some(operation_id) = create_control_operation(&profile.id) else {
                return handled(ControlResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    format!(
                        "daemon already has {MAX_CONTROL_OPERATIONS} active control operations"
                    ),
                ));
            };
            HandledRequest {
                response: ControlResponse::success(
                    request_id,
                    ControlResult::OperationAccepted {
                        operation_id: operation_id.clone(),
                        daemon_pid: std::process::id(),
                    },
                ),
                command: Some(DaemonControlCommand::Tunnel {
                    operation_id,
                    service,
                    action,
                }),
            }
        }
        ControlMethod::OperationStatus {
            workspace_id,
            operation_id,
        } => {
            if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
                return handled(response);
            }
            match control_operation(&profile.id, &operation_id) {
                Some(operation) => handled(ControlResponse::success(
                    request_id,
                    ControlResult::OperationStatus { operation },
                )),
                None => handled(ControlResponse::error(
                    request_id,
                    ERROR_OPERATION_NOT_FOUND,
                    format!("unknown control operation: {operation_id}"),
                )),
            }
        }
        ControlMethod::Events {
            workspace_id,
            cursor,
            limit,
            wait_ms,
        } => {
            if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
                return handled(response);
            }
            let batch = read_workspace_events(&profile.id, cursor.as_ref(), limit, wait_ms).await;
            handled(ControlResponse::success(
                request_id,
                ControlResult::Events { batch },
            ))
        }
        ControlMethod::Reload {
            workspace_id,
            service,
        } => {
            if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
                return handled(response);
            }
            if !command_available {
                return handled(ControlResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    "daemon reload command receiver is unavailable",
                ));
            }
            let Some(operation_id) = create_control_operation(&profile.id) else {
                return handled(ControlResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    format!(
                        "daemon already has {MAX_CONTROL_OPERATIONS} active control operations"
                    ),
                ));
            };
            HandledRequest {
                response: ControlResponse::success(
                    request_id,
                    ControlResult::OperationAccepted {
                        operation_id: operation_id.clone(),
                        daemon_pid: std::process::id(),
                    },
                ),
                command: Some(DaemonControlCommand::Reload {
                    operation_id,
                    service,
                }),
            }
        }
        ControlMethod::ApplyConfig { workspace_id } => {
            if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
                return handled(response);
            }
            if !command_available {
                return handled(ControlResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    "daemon config apply command receiver is unavailable",
                ));
            }
            let Some(operation_id) = create_control_operation(&profile.id) else {
                return handled(ControlResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    format!(
                        "daemon already has {MAX_CONTROL_OPERATIONS} active control operations"
                    ),
                ));
            };
            HandledRequest {
                response: ControlResponse::success(
                    request_id,
                    ControlResult::OperationAccepted {
                        operation_id: operation_id.clone(),
                        daemon_pid: std::process::id(),
                    },
                ),
                command: Some(DaemonControlCommand::ApplyConfig { operation_id }),
            }
        }
        ControlMethod::UpdateOauthRedirectPolicy {
            workspace_id,
            service,
            redirect_uris,
            redirect_hosts,
        } => {
            if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
                return handled(response);
            }
            let service_name = match service {
                ControlService::Mcp => "mcp",
                ControlService::Actions => "actions",
            };
            match crate::auth::update_oauth_redirect_policy(
                &profile.id,
                service_name,
                &redirect_uris,
                &redirect_hosts,
            ) {
                Ok(applied) => handled(ControlResponse::success(
                    request_id,
                    ControlResult::ConfigHotUpdated {
                        applied,
                        daemon_pid: std::process::id(),
                    },
                )),
                Err(error) => handled(ControlResponse::error(
                    request_id,
                    super::protocol::ERROR_CONFIG_HOT_UPDATE_FAILED,
                    error,
                )),
            }
        }
    }
}

#[cfg(any(unix, test))]
fn lifecycle_request(
    request_id: String,
    profile: &WorkspaceProfile,
    workspace_id: String,
    operation: ControlOperation,
    command_available: bool,
) -> HandledRequest {
    if let Some(response) = workspace_mismatch(&request_id, profile, &workspace_id) {
        return handled(response);
    }
    if !command_available {
        return handled(ControlResponse::error(
            request_id,
            ERROR_CONTROL_COMMAND_UNAVAILABLE,
            "daemon lifecycle command receiver is unavailable",
        ));
    }
    HandledRequest {
        response: ControlResponse::success(
            request_id,
            ControlResult::Accepted {
                operation,
                daemon_pid: std::process::id(),
            },
        ),
        command: Some(DaemonControlCommand::Shutdown { operation }),
    }
}

#[cfg(any(unix, test))]
fn workspace_mismatch(
    request_id: &str,
    profile: &WorkspaceProfile,
    workspace_id: &str,
) -> Option<ControlResponse> {
    (workspace_id != profile.id).then(|| {
        ControlResponse::error(
            request_id.to_string(),
            ERROR_WORKSPACE_MISMATCH,
            format!(
                "control endpoint belongs to workspace {}, not {workspace_id}",
                profile.id
            ),
        )
    })
}

#[cfg(any(unix, test))]
fn handled(response: ControlResponse) -> HandledRequest {
    HandledRequest {
        response,
        command: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn framed_json_round_trip_is_bounded_and_newline_delimited() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let request = ControlRequest {
            protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
            request_id: "frame-request".into(),
            method: ControlMethod::Ping,
        };
        let expected = request.clone();

        let writer = tokio::spawn(async move { write_json_frame(&mut client, &request).await });
        let decoded: ControlRequest = read_json_frame(&mut server).await.expect("read frame");

        writer.await.expect("writer task").expect("write frame");
        assert_eq!(decoded, expected);
    }

    #[tokio::test]
    async fn unavailable_endpoint_falls_back_to_local_read_only_status() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-fallback".into()));

        let status = workspace_status_via_daemon_or_local(&profile)
            .await
            .expect("fallback status");

        assert_eq!(status.id, profile.id);
        assert_eq!(status.name, profile.name);
    }

    #[tokio::test]
    async fn generic_connection_serves_workspace_status_over_framed_json() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-server".into()));
        let expected_id = profile.id.clone();
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let server_profile = profile.clone();
        let (command_sender, _command_receiver) = control_channel();
        let server_task = tokio::spawn(async move {
            handle_connection(server, &server_profile, &command_sender).await
        });
        let request = ControlRequest {
            protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
            request_id: "status-request".into(),
            method: ControlMethod::WorkspaceStatus {
                workspace_id: expected_id.clone(),
            },
        };

        write_json_frame(&mut client, &request)
            .await
            .expect("write request");
        let response: ControlResponse = read_json_frame(&mut client).await.expect("read response");
        server_task
            .await
            .expect("server task")
            .expect("serve request");

        assert!(response.ok);
        assert_eq!(response.request_id, request.request_id);
        match response.result.expect("status result") {
            ControlResult::WorkspaceStatus { status } => assert_eq!(status.id, expected_id),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn endpoint_transport_matches_the_current_platform() {
        let endpoint = endpoint("workspace-1").expect("endpoint");
        #[cfg(unix)]
        assert!(matches!(endpoint, LocalControlEndpoint::UnixSocket(_)));
        #[cfg(windows)]
        assert!(matches!(
            endpoint,
            LocalControlEndpoint::WindowsNamedPipe(_)
        ));
    }

    #[tokio::test]
    async fn request_handler_rejects_cross_workspace_access() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-owner".into()));
        let request = ControlRequest {
            protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
            request_id: "mismatch-request".into(),
            method: ControlMethod::WorkspaceStatus {
                workspace_id: "another-workspace".into(),
            },
        };

        let response = handle_request(request, &profile, true).await.response;

        assert!(!response.ok);
        assert_eq!(
            response.error.expect("workspace mismatch").code,
            super::super::protocol::ERROR_WORKSPACE_MISMATCH
        );
    }

    #[tokio::test]
    async fn request_handler_reports_protocol_and_daemon_versions() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-version".into()));
        let response = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "version-request".into(),
                method: ControlMethod::Version,
            },
            &profile,
            true,
        )
        .await
        .response;

        assert!(response.ok);
        assert_eq!(response.request_id, "version-request");
        assert!(matches!(
            response.result,
            Some(ControlResult::Version {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn shutdown_response_is_flushed_before_command_delivery() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-shutdown".into()));
        let workspace_id = profile.id.clone();
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let (command_sender, mut command_receiver) = control_channel();
        let server_profile = profile.clone();
        let server_task = tokio::spawn(async move {
            handle_connection(server, &server_profile, &command_sender).await
        });
        let request = ControlRequest {
            protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
            request_id: "shutdown-request".into(),
            method: ControlMethod::Shutdown {
                workspace_id: workspace_id.clone(),
            },
        };

        write_json_frame(&mut client, &request)
            .await
            .expect("write shutdown request");
        let response: ControlResponse = read_json_frame(&mut client).await.expect("read response");
        assert!(response.ok);
        assert!(matches!(
            response.result,
            Some(ControlResult::Accepted {
                operation: ControlOperation::Shutdown,
                ..
            })
        ));
        server_task
            .await
            .expect("server task")
            .expect("serve shutdown request");
        assert_eq!(
            command_receiver.recv().await,
            Some(DaemonControlCommand::Shutdown {
                operation: ControlOperation::Shutdown
            })
        );
    }

    #[tokio::test]
    async fn tunnel_response_is_flushed_before_execution_and_operation_tracks_completion() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-tunnel".into()));
        let workspace_id = profile.id.clone();
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let (command_sender, mut command_receiver) = control_channel();
        let server_profile = profile.clone();
        let server_task = tokio::spawn(async move {
            handle_connection(server, &server_profile, &command_sender).await
        });
        let request = ControlRequest {
            protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
            request_id: "tunnel-request".into(),
            method: ControlMethod::TunnelControl {
                workspace_id: workspace_id.clone(),
                service: TunnelServiceKind::Mcp,
                action: ControlTunnelAction::Restart,
            },
        };

        write_json_frame(&mut client, &request)
            .await
            .expect("write tunnel request");
        let response: ControlResponse = read_json_frame(&mut client).await.expect("read response");
        let operation_id = match response.result.expect("accepted result") {
            ControlResult::OperationAccepted {
                operation_id,
                daemon_pid,
            } => {
                assert_eq!(daemon_pid, std::process::id());
                operation_id
            }
            other => panic!("unexpected response: {other:?}"),
        };
        server_task
            .await
            .expect("server task")
            .expect("serve tunnel request");
        assert_eq!(
            command_receiver.recv().await,
            Some(DaemonControlCommand::Tunnel {
                operation_id: operation_id.clone(),
                service: TunnelServiceKind::Mcp,
                action: ControlTunnelAction::Restart,
            })
        );

        mark_control_operation_running(&operation_id);
        let running = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "operation-running".into(),
                method: ControlMethod::OperationStatus {
                    workspace_id: workspace_id.clone(),
                    operation_id: operation_id.clone(),
                },
            },
            &profile,
            true,
        )
        .await
        .response;
        assert!(matches!(
            running.result,
            Some(ControlResult::OperationStatus {
                operation: ControlAsyncOperation {
                    state: ControlAsyncState::Running,
                    ..
                }
            })
        ));

        let expected_status = TunnelStatus {
            state: "running".into(),
            public_url: "https://stable.example.com".into(),
            tunnel_pid: Some(42),
        };
        finish_tunnel_operation(&operation_id, Ok(expected_status.clone()));
        let completed = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "operation-completed".into(),
                method: ControlMethod::OperationStatus {
                    workspace_id,
                    operation_id,
                },
            },
            &profile,
            true,
        )
        .await
        .response;
        match completed.result.expect("operation result") {
            ControlResult::OperationStatus { operation } => {
                assert_eq!(operation.state, ControlAsyncState::Succeeded);
                assert_eq!(operation.tunnel_status, Some(expected_status));
                assert!(operation.error.is_none());
            }
            other => panic!("unexpected operation response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn reload_response_is_flushed_before_execution_and_operation_tracks_completion() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-reload".into()));
        let workspace_id = profile.id.clone();
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let (command_sender, mut command_receiver) = control_channel();
        let server_profile = profile.clone();
        let server_task = tokio::spawn(async move {
            handle_connection(server, &server_profile, &command_sender).await
        });
        let request = ControlRequest {
            protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
            request_id: "reload-request".into(),
            method: ControlMethod::Reload {
                workspace_id: workspace_id.clone(),
                service: ControlService::Mcp,
            },
        };

        write_json_frame(&mut client, &request)
            .await
            .expect("write reload request");
        let response: ControlResponse = read_json_frame(&mut client).await.expect("read response");
        let operation_id = match response.result.expect("accepted result") {
            ControlResult::OperationAccepted {
                operation_id,
                daemon_pid,
            } => {
                assert_eq!(daemon_pid, std::process::id());
                operation_id
            }
            other => panic!("unexpected response: {other:?}"),
        };
        server_task
            .await
            .expect("server task")
            .expect("serve reload request");
        assert_eq!(
            command_receiver.recv().await,
            Some(DaemonControlCommand::Reload {
                operation_id: operation_id.clone(),
                service: ControlService::Mcp,
            })
        );

        mark_control_operation_running(&operation_id);
        finish_reload_operation(&operation_id, Ok(()));
        let completed = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "reload-completed".into(),
                method: ControlMethod::OperationStatus {
                    workspace_id,
                    operation_id,
                },
            },
            &profile,
            true,
        )
        .await
        .response;
        match completed.result.expect("operation result") {
            ControlResult::OperationStatus { operation } => {
                assert_eq!(operation.state, ControlAsyncState::Succeeded);
                assert!(operation.tunnel_status.is_none());
                assert!(operation.error.is_none());
            }
            other => panic!("unexpected operation response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn config_apply_response_is_flushed_before_execution_and_returns_structured_result() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-config-apply".into()));
        let workspace_id = profile.id.clone();
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let (command_sender, mut command_receiver) = control_channel();
        let server_profile = profile.clone();
        let server_task = tokio::spawn(async move {
            handle_connection(server, &server_profile, &command_sender).await
        });
        let request = ControlRequest {
            protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
            request_id: "config-apply-request".into(),
            method: ControlMethod::ApplyConfig {
                workspace_id: workspace_id.clone(),
            },
        };

        write_json_frame(&mut client, &request)
            .await
            .expect("write config apply request");
        let response: ControlResponse = read_json_frame(&mut client).await.expect("read response");
        let operation_id = match response.result.expect("accepted result") {
            ControlResult::OperationAccepted {
                operation_id,
                daemon_pid,
            } => {
                assert_eq!(daemon_pid, std::process::id());
                operation_id
            }
            other => panic!("unexpected response: {other:?}"),
        };
        server_task
            .await
            .expect("server task")
            .expect("serve config apply request");
        assert_eq!(
            command_receiver.recv().await,
            Some(DaemonControlCommand::ApplyConfig {
                operation_id: operation_id.clone(),
            })
        );

        mark_control_operation_running(&operation_id);
        let expected = ControlConfigApplyResult {
            changed: true,
            mcp_listener_reloaded: true,
            actions_listener_reloaded: false,
            mcp_callback_hot_updated: false,
            actions_callback_hot_updated: true,
            mcp_tunnel_reloaded: false,
            actions_tunnel_reloaded: false,
        };
        finish_config_apply_operation(&operation_id, Ok(expected.clone()));
        let completed = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "config-apply-completed".into(),
                method: ControlMethod::OperationStatus {
                    workspace_id,
                    operation_id,
                },
            },
            &profile,
            true,
        )
        .await
        .response;
        match completed.result.expect("operation result") {
            ControlResult::OperationStatus { operation } => {
                assert_eq!(operation.state, ControlAsyncState::Succeeded);
                assert_eq!(operation.config_apply, Some(expected));
                assert!(operation.tunnel_status.is_none());
                assert!(operation.error.is_none());
            }
            other => panic!("unexpected operation response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn oauth_redirect_policy_hot_update_runs_inside_the_control_owner_process() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-oauth-hot-update".into()));
        let runtime = std::sync::Arc::new(crate::auth::OAuthRuntime::new(
            "https://service.example".into(),
            "client".into(),
            None,
            "password".into(),
            "token-secret".into(),
        ));
        crate::auth::register_oauth_runtime(&profile.id, "mcp", &runtime);

        let response = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "oauth-hot-update".into(),
                method: ControlMethod::UpdateOauthRedirectPolicy {
                    workspace_id: profile.id.clone(),
                    service: ControlService::Mcp,
                    redirect_uris: "https://chatgpt.com/callback/new".into(),
                    redirect_hosts: "*.chatgpt.com".into(),
                },
            },
            &profile,
            true,
        )
        .await
        .response;

        assert!(response.ok);
        match response.result.expect("hot update result") {
            ControlResult::ConfigHotUpdated {
                applied,
                daemon_pid,
            } => {
                assert!(applied);
                assert_eq!(daemon_pid, std::process::id());
            }
            other => panic!("unexpected hot update response: {other:?}"),
        }
        assert!(runtime.redirect_uri_allowed("https://chatgpt.com/callback/new"));
        assert_eq!(
            runtime.redirect_uri_status_label("https://oauth.chatgpt.com/callback/dynamic"),
            "auto_enrollment_allowed"
        );
    }

    #[tokio::test]
    async fn oauth_redirect_policy_hot_update_reports_unloaded_runtime_without_fallback() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-oauth-no-runtime".into()));
        let response = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "oauth-hot-update-missing".into(),
                method: ControlMethod::UpdateOauthRedirectPolicy {
                    workspace_id: profile.id.clone(),
                    service: ControlService::Actions,
                    redirect_uris: "https://chatgpt.com/callback".into(),
                    redirect_hosts: "chatgpt.com".into(),
                },
            },
            &profile,
            true,
        )
        .await
        .response;

        assert!(response.ok);
        assert!(matches!(
            response.result,
            Some(ControlResult::ConfigHotUpdated { applied: false, .. })
        ));
    }

    #[tokio::test]
    async fn event_requests_resume_from_cursor_and_report_stream_reset() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-events".into()));
        super::super::reset_workspace_event_stream(&profile.id);
        super::super::publish_workspace_event(
            &profile.id,
            super::super::protocol::ControlEventKind::DaemonReady,
            None,
            "running",
            "ready",
        );
        let first = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "events-first".into(),
                method: ControlMethod::Events {
                    workspace_id: profile.id.clone(),
                    cursor: None,
                    limit: 8,
                    wait_ms: 0,
                },
            },
            &profile,
            true,
        )
        .await
        .response;
        let first_batch = match first.result.expect("events result") {
            ControlResult::Events { batch } => batch,
            other => panic!("unexpected event response: {other:?}"),
        };
        assert_eq!(first_batch.events.len(), 1);
        assert!(!first_batch.reset);

        super::super::publish_workspace_event(
            &profile.id,
            super::super::protocol::ControlEventKind::McpActivity,
            Some(ControlService::Mcp),
            "active",
            "tool started",
        );
        let resumed = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "events-resumed".into(),
                method: ControlMethod::Events {
                    workspace_id: profile.id.clone(),
                    cursor: Some(first_batch.next_cursor.clone()),
                    limit: 8,
                    wait_ms: 0,
                },
            },
            &profile,
            true,
        )
        .await
        .response;
        let resumed_batch = match resumed.result.expect("events result") {
            ControlResult::Events { batch } => batch,
            other => panic!("unexpected event response: {other:?}"),
        };
        assert_eq!(resumed_batch.events.len(), 1);
        assert_eq!(resumed_batch.events[0].sequence, 2);

        super::super::reset_workspace_event_stream(&profile.id);
        let reset = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "events-reset".into(),
                method: ControlMethod::Events {
                    workspace_id: profile.id.clone(),
                    cursor: Some(resumed_batch.next_cursor),
                    limit: 8,
                    wait_ms: 0,
                },
            },
            &profile,
            true,
        )
        .await
        .response;
        match reset.result.expect("events result") {
            ControlResult::Events { batch } => assert!(batch.reset),
            other => panic!("unexpected event response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn lifecycle_request_fails_closed_without_a_command_receiver() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-no-command".into()));
        let handled = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "restart-request".into(),
                method: ControlMethod::PrepareRestart {
                    workspace_id: profile.id.clone(),
                },
            },
            &profile,
            false,
        )
        .await;

        assert!(!handled.response.ok);
        assert!(handled.command.is_none());
        assert_eq!(
            handled.response.error.expect("command error").code,
            super::super::protocol::ERROR_CONTROL_COMMAND_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn unavailable_write_request_never_falls_back() {
        let profile_id = format!("missing-write-{}", uuid::Uuid::new_v4());

        let error = request_daemon_exit(&profile_id, ControlOperation::Shutdown)
            .await
            .expect_err("missing daemon must fail");

        assert!(error.is_unavailable());
    }

    #[tokio::test]
    async fn tunnel_write_without_daemon_never_falls_back() {
        let profile = WorkspaceProfile::new(".".into(), Some("missing-tunnel-daemon".into()));

        let error = request_tunnel_operation(
            &profile,
            TunnelServiceKind::Mcp,
            ControlTunnelAction::Start,
            Duration::from_millis(50),
        )
        .await
        .expect_err("tunnel write must require a running daemon");

        assert!(error.to_string().contains("daemon is not running"));
    }
}
