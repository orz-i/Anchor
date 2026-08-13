#[cfg(any(feature = "cli", test))]
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
#[cfg(any(feature = "cli", test))]
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::build_identity::BuildIdentity;
use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::gateway_daemon;
use crate::mcp::gateway;
use crate::settings::McpGatewayConfig;

#[cfg(any(unix, windows, test))]
use super::events::read_gateway_events;
use super::events::{MAX_GATEWAY_EVENT_BATCH, MAX_GATEWAY_EVENT_WAIT_MS};
use super::logs::read_gateway_log;
#[cfg(any(unix, windows, test))]
use super::protocol::{
    validate_protocol_version, ERROR_CONFIG_SCOPE_MISMATCH, ERROR_CONTROL_COMMAND_UNAVAILABLE,
    ERROR_LOG_READ_FAILED, ERROR_OPERATION_NOT_FOUND,
};
#[cfg(any(feature = "cli", test))]
use super::protocol::{GatewayAsyncOperation, GatewayControlError, ERROR_OPERATION_FAILED};
use super::protocol::{
    GatewayAsyncState, GatewayControlStatus, GatewayEventBatch, GatewayEventCursor,
    GatewayLogChunk, GatewayLogCursor, GatewayMethod, GatewayOperation, GatewayRequest,
    GatewayResponse, GatewayResult, GATEWAY_CONTROL_PROTOCOL_VERSION,
    GATEWAY_LIFECYCLE_PROTOCOL_MIN_VERSION, MAX_GATEWAY_CONTROL_FRAME_BYTES,
};

const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
const OPERATION_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(any(unix, windows, test))]
const MAX_OPERATIONS: usize = 128;

#[cfg(windows)]
fn create_windows_pipe_server(
    name: &str,
    first_instance: bool,
) -> AppResult<tokio::net::windows::named_pipe::NamedPipeServer> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;

    use tokio::net::windows::named_pipe::ServerOptions;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    let security_sddl = crate::windows_service::control_pipe_security_sddl();
    let sddl = std::ffi::OsStr::new(&security_sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
        .map_err(|error| {
            AppError::Message(format!(
                "failed to build Windows Gateway pipe security descriptor: {error}"
            ))
        })?;
    }

    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let result = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first_instance)
            .reject_remote_clients(true)
            .max_instances(32)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            )
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    result.map_err(|error| {
        AppError::Message(format!(
            "failed to create Windows Gateway named pipe {name}: {error}"
        ))
    })
}

fn should_log_connection_error(error: &AppError) -> bool {
    #[cfg(windows)]
    if matches!(
        error,
        AppError::Io(io_error) if matches!(io_error.raw_os_error(), Some(109 | 232))
    ) {
        return false;
    }
    true
}

pub async fn request_version() -> Result<GatewayVersionInfo, GatewayControlClientError> {
    let result = match request(GatewayMethod::Version).await {
        Ok(result) => result,
        Err(GatewayControlClientError::VersionMismatch {
            daemon_protocol, ..
        }) if daemon_protocol > 0 && daemon_protocol < GATEWAY_CONTROL_PROTOCOL_VERSION => {
            request_with_protocol_version(GatewayMethod::Version, REQUEST_TIMEOUT, daemon_protocol)
                .await?
        }
        Err(error) => return Err(error),
    };
    match result {
        GatewayResult::Version {
            daemon_version,
            protocol_version,
            build_identity,
        } => Ok(GatewayVersionInfo {
            daemon_version,
            protocol_version,
            build_identity,
        }),
        other => Err(GatewayControlClientError::Protocol(format!(
            "Gateway daemon returned unexpected version result: {other:?}"
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayVersionInfo {
    pub daemon_version: String,
    pub protocol_version: u16,
    pub build_identity: Option<BuildIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayControlCommand {
    Shutdown {
        operation: GatewayOperation,
    },
    Reload {
        operation_id: String,
    },
    ApplyConfig {
        operation_id: String,
        config: Box<McpGatewayConfig>,
    },
}

async fn wait_for_operation(
    operation_id: &str,
    timeout: Duration,
) -> Result<(), GatewayControlClientError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let operation = match request(GatewayMethod::OperationStatus {
            operation_id: operation_id.to_string(),
        })
        .await?
        {
            GatewayResult::OperationStatus { operation } => operation,
            other => {
                return Err(GatewayControlClientError::Protocol(format!(
                    "Gateway daemon returned unexpected operation status: {other:?}"
                )))
            }
        };
        match operation.state {
            GatewayAsyncState::Succeeded => return Ok(()),
            GatewayAsyncState::Failed => {
                let error = operation.error.ok_or_else(|| {
                    GatewayControlClientError::Protocol(
                        "failed Gateway operation omitted error details".into(),
                    )
                })?;
                return Err(GatewayControlClientError::Remote {
                    code: error.code,
                    message: error.message,
                });
            }
            GatewayAsyncState::Pending | GatewayAsyncState::Running => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(GatewayControlClientError::Remote {
                code: "operation_timeout".into(),
                message: format!("Gateway control operation {operation_id} timed out"),
            });
        }
        tokio::time::sleep(OPERATION_POLL_INTERVAL).await;
    }
}

pub async fn request_logs(
    tail_lines: u32,
    cursor: Option<GatewayLogCursor>,
) -> Result<GatewayLogChunk, GatewayControlClientError> {
    match request(GatewayMethod::Logs { tail_lines, cursor }).await? {
        GatewayResult::Logs { chunk } => Ok(chunk),
        other => Err(GatewayControlClientError::Protocol(format!(
            "Gateway daemon returned unexpected logs result: {other:?}"
        ))),
    }
}

pub async fn logs_via_daemon_or_local(
    tail_lines: u32,
    cursor: Option<GatewayLogCursor>,
) -> AppResult<GatewayLogChunk> {
    match request_logs(tail_lines, cursor.clone()).await {
        Ok(chunk) => Ok(chunk),
        Err(error) if error.is_unavailable() => {
            let inspection = gateway_daemon::inspect()?;
            if inspection.running {
                return Err(AppError::Message(format!(
                    "Gateway daemon 正在运行但日志控制端点不可用：{error}；不会回退到直接读取正在写入的日志文件"
                )));
            }
            read_gateway_log(tail_lines, cursor.as_ref())
        }
        Err(error) => Err(AppError::Message(error.to_string())),
    }
}

pub async fn request_events(
    cursor: Option<GatewayEventCursor>,
    limit: u32,
    wait_ms: u32,
) -> Result<GatewayEventBatch, GatewayControlClientError> {
    let wait_ms = wait_ms.min(MAX_GATEWAY_EVENT_WAIT_MS);
    let timeout = Duration::from_millis(u64::from(wait_ms).saturating_add(1_000));
    match request_with_timeout(
        GatewayMethod::Events {
            cursor,
            limit: limit.clamp(1, MAX_GATEWAY_EVENT_BATCH),
            wait_ms,
        },
        timeout,
    )
    .await?
    {
        GatewayResult::Events { batch } => Ok(batch),
        other => Err(GatewayControlClientError::Protocol(format!(
            "Gateway daemon returned unexpected events result: {other:?}"
        ))),
    }
}

pub type GatewayControlSender = tokio::sync::mpsc::UnboundedSender<GatewayControlCommand>;
pub type GatewayControlReceiver = tokio::sync::mpsc::UnboundedReceiver<GatewayControlCommand>;

pub fn control_channel() -> (GatewayControlSender, GatewayControlReceiver) {
    tokio::sync::mpsc::unbounded_channel()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayLocalEndpoint {
    UnixSocket(PathBuf),
    WindowsNamedPipe(String),
}

pub fn endpoint() -> AppResult<GatewayLocalEndpoint> {
    #[cfg(unix)]
    {
        return Ok(GatewayLocalEndpoint::UnixSocket(
            gateway_daemon::control_socket_path()?,
        ));
    }
    #[cfg(windows)]
    {
        return Ok(GatewayLocalEndpoint::WindowsNamedPipe(
            gateway_daemon::control_pipe_name()?,
        ));
    }
    #[allow(unreachable_code)]
    Err(AppError::Message(
        "Gateway control transport is unsupported on this platform".into(),
    ))
}

#[derive(Debug)]
pub enum GatewayControlClientError {
    Unavailable(String),
    VersionMismatch {
        daemon_protocol: u16,
        client_protocol: u16,
    },
    Protocol(String),
    Remote {
        code: String,
        message: String,
    },
    Io(io::Error),
}

impl GatewayControlClientError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl fmt::Display for GatewayControlClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Protocol(message) => formatter.write_str(message),
            Self::VersionMismatch {
                daemon_protocol,
                client_protocol,
            } => write!(
                formatter,
                "Gateway daemon responded with protocol version {daemon_protocol}; client supports {client_protocol}"
            ),
            Self::Remote { code, message } => {
                write!(formatter, "Gateway daemon error {code}: {message}")
            }
            Self::Io(error) => write!(formatter, "Gateway control I/O error: {error}"),
        }
    }
}

impl std::error::Error for GatewayControlClientError {}

#[derive(Debug, Clone)]
#[cfg(any(feature = "cli", test))]
struct StoredOperation {
    #[cfg(any(unix, windows, test))]
    config_scope: String,
    operation: GatewayAsyncOperation,
}

#[cfg(any(feature = "cli", test))]
fn operation_store() -> &'static Mutex<HashMap<String, StoredOperation>> {
    static STORE: OnceLock<Mutex<HashMap<String, StoredOperation>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(any(unix, windows, test))]
fn create_operation(config_scope: &str) -> Option<String> {
    let operation_id = uuid::Uuid::new_v4().to_string();
    let mut operations = operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if operations.len() >= MAX_OPERATIONS {
        let completed = operations
            .iter()
            .find(|(_, stored)| {
                matches!(
                    stored.operation.state,
                    GatewayAsyncState::Succeeded | GatewayAsyncState::Failed
                )
            })
            .map(|(id, _)| id.clone())?;
        operations.remove(&completed);
    }
    operations.insert(
        operation_id.clone(),
        StoredOperation {
            config_scope: config_scope.to_string(),
            operation: GatewayAsyncOperation {
                operation_id: operation_id.clone(),
                state: GatewayAsyncState::Pending,
                error: None,
            },
        },
    );
    Some(operation_id)
}

#[cfg(any(unix, windows, test))]
fn operation(config_scope: &str, operation_id: &str) -> Option<GatewayAsyncOperation> {
    operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(operation_id)
        .filter(|stored| stored.config_scope == config_scope)
        .map(|stored| stored.operation.clone())
}

#[cfg(any(feature = "cli", test))]
pub(crate) fn mark_operation_running(operation_id: &str) {
    if let Some(stored) = operation_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_mut(operation_id)
    {
        stored.operation.state = GatewayAsyncState::Running;
        stored.operation.error = None;
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
            stored.operation.state = GatewayAsyncState::Succeeded;
            stored.operation.error = None;
        }
        Err(error) => {
            stored.operation.state = GatewayAsyncState::Failed;
            stored.operation.error = Some(GatewayControlError {
                code: ERROR_OPERATION_FAILED.to_string(),
                message: error.to_string(),
            });
        }
    }
}

pub async fn ping() -> Result<(), GatewayControlClientError> {
    match request(GatewayMethod::Ping).await? {
        GatewayResult::Pong { .. } => Ok(()),
        other => Err(GatewayControlClientError::Protocol(format!(
            "Gateway daemon returned unexpected ping result: {other:?}"
        ))),
    }
}

pub async fn request_status() -> Result<GatewayControlStatus, GatewayControlClientError> {
    match request(GatewayMethod::Status).await? {
        GatewayResult::Status { status } => Ok(*status),
        other => Err(GatewayControlClientError::Protocol(format!(
            "Gateway daemon returned unexpected status result: {other:?}"
        ))),
    }
}

pub async fn status_via_daemon_or_local() -> AppResult<GatewayControlStatus> {
    match request_status().await {
        Ok(status) => Ok(status),
        Err(error) if error.is_unavailable() => local_status().await,
        Err(error) => Err(AppError::Message(error.to_string())),
    }
}

pub async fn request_exit(operation: GatewayOperation) -> Result<u32, GatewayControlClientError> {
    let method = match operation {
        GatewayOperation::Shutdown => GatewayMethod::Shutdown,
        GatewayOperation::Restart => GatewayMethod::PrepareRestart,
    };
    let result = match request(method.clone()).await {
        Ok(result) => result,
        Err(error) => {
            let Some(protocol_version) = legacy_lifecycle_retry_protocol(&error) else {
                return Err(error);
            };
            request_with_protocol_version(method, REQUEST_TIMEOUT, protocol_version).await?
        }
    };
    match result {
        GatewayResult::Accepted {
            operation: accepted,
            daemon_pid,
        } if accepted == operation => Ok(daemon_pid),
        other => Err(GatewayControlClientError::Protocol(format!(
            "Gateway daemon returned unexpected lifecycle result: {other:?}"
        ))),
    }
}

pub async fn request_reload(timeout: Duration) -> Result<(), GatewayControlClientError> {
    let inspection = gateway_daemon::inspect().map_err(|error| {
        GatewayControlClientError::Protocol(format!(
            "cannot inspect Gateway daemon before reload: {error}"
        ))
    })?;
    let state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .ok_or_else(|| {
            GatewayControlClientError::Protocol(
                "Gateway daemon is not running; reload requires its control plane".into(),
            )
        })?;
    let (operation_id, daemon_pid) = match request(GatewayMethod::Reload).await? {
        GatewayResult::OperationAccepted {
            operation_id,
            daemon_pid,
        } => (operation_id, daemon_pid),
        other => {
            return Err(GatewayControlClientError::Protocol(format!(
                "Gateway daemon returned unexpected reload result: {other:?}"
            )))
        }
    };
    if daemon_pid != state.pid {
        return Err(GatewayControlClientError::Protocol(format!(
            "Gateway reload PID mismatch: status file={}, response={daemon_pid}",
            state.pid
        )));
    }
    wait_for_operation(&operation_id, timeout).await
}

pub async fn request_apply_config(
    config: McpGatewayConfig,
    timeout: Duration,
) -> Result<(), GatewayControlClientError> {
    if !config.enabled {
        return Err(GatewayControlClientError::Protocol(
            "disabling a running Gateway requires shutdown before persisting the disabled config"
                .into(),
        ));
    }
    let inspection = gateway_daemon::inspect().map_err(|error| {
        GatewayControlClientError::Protocol(format!(
            "cannot inspect Gateway daemon before config apply: {error}"
        ))
    })?;
    let state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .ok_or_else(|| {
            GatewayControlClientError::Protocol(
                "Gateway daemon is not running; apply_config requires its control plane".into(),
            )
        })?;
    let (operation_id, daemon_pid) = match request(GatewayMethod::ApplyConfig {
        config: Box::new(config),
    })
    .await?
    {
        GatewayResult::OperationAccepted {
            operation_id,
            daemon_pid,
        } => (operation_id, daemon_pid),
        other => {
            return Err(GatewayControlClientError::Protocol(format!(
                "Gateway daemon returned unexpected apply_config result: {other:?}"
            )))
        }
    };
    if daemon_pid != state.pid {
        return Err(GatewayControlClientError::Protocol(format!(
            "Gateway apply_config PID mismatch: status file={}, response={daemon_pid}",
            state.pid
        )));
    }
    wait_for_operation(&operation_id, timeout).await
}

async fn local_status() -> AppResult<GatewayControlStatus> {
    let store = DataStore::load()?;
    let config = store.settings().mcp_gateway;
    let inspection = gateway_daemon::inspect()?;
    let route_workspace_ids = if inspection.running {
        inspection
            .state
            .as_ref()
            .map(|state| state.workspace_ids.clone())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let pid = inspection
        .running
        .then(|| inspection.state.as_ref().map(|state| state.pid))
        .flatten();
    let build_identity = inspection
        .state
        .as_ref()
        .and_then(|state| state.build_identity.clone());
    let state = if inspection.running {
        "error"
    } else if config.enabled {
        "configured"
    } else {
        "stopped"
    };
    let error = if inspection.running {
        "Gateway daemon 状态显示进程运行，但控制端点不可用".to_string()
    } else if inspection.stale {
        inspection.detail.clone()
    } else {
        String::new()
    };
    Ok(GatewayControlStatus {
        daemon_supported: inspection.supported,
        running: inspection.running,
        pid,
        build_identity,
        state: state.into(),
        local_endpoint: format!("http://127.0.0.1:{}", config.local_port),
        public_base_url: config.effective_public_url(),
        route_count: route_workspace_ids.len(),
        route_workspace_ids,
        owner_workspace_id: config.owner_workspace_id,
        error,
        detail: inspection.detail,
    })
}

#[cfg(any(unix, windows, test))]
async fn daemon_status() -> AppResult<GatewayControlStatus> {
    let store = DataStore::load()?;
    let config = store.settings().mcp_gateway;
    let inspection = gateway_daemon::inspect()?;
    let runtime = gateway::status(&config).await;
    let route_workspace_ids = inspection
        .state
        .as_ref()
        .map(|state| state.workspace_ids.clone())
        .unwrap_or_default();
    let build_identity = inspection
        .state
        .as_ref()
        .and_then(|state| state.build_identity.clone());
    Ok(GatewayControlStatus {
        daemon_supported: inspection.supported,
        running: inspection.running,
        pid: inspection.state.as_ref().map(|state| state.pid),
        build_identity,
        state: runtime.state,
        local_endpoint: runtime.local_endpoint,
        public_base_url: runtime.public_base_url,
        route_count: runtime.route_count,
        route_workspace_ids,
        owner_workspace_id: runtime.owner_workspace_id,
        error: runtime.error,
        detail: inspection.detail,
    })
}

async fn request(method: GatewayMethod) -> Result<GatewayResult, GatewayControlClientError> {
    request_with_timeout(method, REQUEST_TIMEOUT).await
}

async fn request_with_timeout(
    method: GatewayMethod,
    timeout: Duration,
) -> Result<GatewayResult, GatewayControlClientError> {
    request_with_protocol_version(method, timeout, GATEWAY_CONTROL_PROTOCOL_VERSION).await
}

async fn request_with_protocol_version(
    method: GatewayMethod,
    timeout: Duration,
    protocol_version: u16,
) -> Result<GatewayResult, GatewayControlClientError> {
    let config_scope = gateway_daemon::config_scope().map_err(|error| {
        GatewayControlClientError::Protocol(format!("cannot resolve Gateway config scope: {error}"))
    })?;
    let endpoint = endpoint().map_err(|error| {
        GatewayControlClientError::Protocol(format!(
            "cannot resolve Gateway control endpoint: {error}"
        ))
    })?;
    let request = GatewayRequest::with_protocol_version(config_scope, method, protocol_version);
    let request_id = request.request_id.clone();
    let response = tokio::time::timeout(timeout, exchange_with_endpoint(&endpoint, &request))
        .await
        .map_err(|_| {
            GatewayControlClientError::Unavailable(format!(
                "Gateway control endpoint timed out: {endpoint:?}"
            ))
        })??;
    if response.protocol_version != protocol_version {
        return Err(GatewayControlClientError::VersionMismatch {
            daemon_protocol: response.protocol_version,
            client_protocol: GATEWAY_CONTROL_PROTOCOL_VERSION,
        });
    }
    if response.request_id != request_id {
        return Err(GatewayControlClientError::Protocol(format!(
            "Gateway response request id mismatch: expected {request_id}, got {}",
            response.request_id
        )));
    }
    if !response.ok {
        let error = response.error.ok_or_else(|| {
            GatewayControlClientError::Protocol(
                "Gateway daemon returned ok=false without an error".into(),
            )
        })?;
        return Err(GatewayControlClientError::Remote {
            code: error.code,
            message: error.message,
        });
    }
    response.result.ok_or_else(|| {
        GatewayControlClientError::Protocol("Gateway daemon returned ok=true without result".into())
    })
}

fn legacy_lifecycle_retry_protocol(error: &GatewayControlClientError) -> Option<u16> {
    let GatewayControlClientError::VersionMismatch {
        daemon_protocol,
        client_protocol,
    } = error
    else {
        return None;
    };
    (*daemon_protocol < *client_protocol
        && *daemon_protocol >= GATEWAY_LIFECYCLE_PROTOCOL_MIN_VERSION)
        .then_some(*daemon_protocol)
}

#[cfg(unix)]
async fn exchange_with_endpoint(
    endpoint: &GatewayLocalEndpoint,
    request: &GatewayRequest,
) -> Result<GatewayResponse, GatewayControlClientError> {
    let GatewayLocalEndpoint::UnixSocket(path) = endpoint else {
        return Err(GatewayControlClientError::Protocol(
            "Unix client received a non-Unix Gateway endpoint".into(),
        ));
    };
    let mut stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(classify_connect_error)?;
    exchange(&mut stream, request).await
}

#[cfg(windows)]
async fn exchange_with_endpoint(
    endpoint: &GatewayLocalEndpoint,
    request: &GatewayRequest,
) -> Result<GatewayResponse, GatewayControlClientError> {
    let GatewayLocalEndpoint::WindowsNamedPipe(name) = endpoint else {
        return Err(GatewayControlClientError::Protocol(
            "Windows client received a non-pipe Gateway endpoint".into(),
        ));
    };
    let mut stream = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(name)
        .map_err(classify_connect_error)?;
    exchange(&mut stream, request).await
}

#[cfg(not(any(unix, windows)))]
async fn exchange_with_endpoint(
    _endpoint: &GatewayLocalEndpoint,
    _request: &GatewayRequest,
) -> Result<GatewayResponse, GatewayControlClientError> {
    Err(GatewayControlClientError::Unavailable(
        "Gateway control transport is unsupported on this platform".into(),
    ))
}

async fn exchange<S>(
    stream: &mut S,
    request: &GatewayRequest,
) -> Result<GatewayResponse, GatewayControlClientError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    write_json_frame(stream, request)
        .await
        .map_err(GatewayControlClientError::Io)?;
    read_json_frame(stream).await.map_err(|error| match error {
        FrameError::Io(error) => GatewayControlClientError::Io(error),
        FrameError::Protocol(message) => GatewayControlClientError::Protocol(message),
    })
}

fn classify_connect_error(error: io::Error) -> GatewayControlClientError {
    if is_unavailable_io(&error) {
        GatewayControlClientError::Unavailable(format!(
            "Gateway control endpoint unavailable: {error}"
        ))
    } else {
        GatewayControlClientError::Io(error)
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
        .map_err(|error| FrameError::Protocol(format!("invalid Gateway control JSON: {error}")))
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
                "Gateway control connection closed before newline",
            ));
        }
        if byte[0] == b'\n' {
            return Ok(frame);
        }
        frame.push(byte[0]);
        if frame.len() > MAX_GATEWAY_CONTROL_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Gateway control frame exceeds maximum size",
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
    if encoded.len() > MAX_GATEWAY_CONTROL_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Gateway control frame exceeds maximum size",
        ));
    }
    writer.write_all(&encoded).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

pub struct GatewayControlServer {
    endpoint: GatewayLocalEndpoint,
    task: tokio::task::JoinHandle<()>,
}

impl GatewayControlServer {
    #[cfg(unix)]
    pub fn start(command_sender: GatewayControlSender) -> AppResult<Self> {
        use std::os::unix::fs::PermissionsExt;

        let endpoint = endpoint()?;
        let GatewayLocalEndpoint::UnixSocket(path) = &endpoint else {
            return Err(AppError::Message(
                "Unix Gateway daemon resolved a non-Unix control endpoint".into(),
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
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(connection) => connection,
                    Err(error) => {
                        gateway_daemon::append_log(&format!("[control] accept failed: {error}"));
                        break;
                    }
                };
                let command_sender = command_sender.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, &command_sender).await {
                        gateway_daemon::append_log(&format!("[control] request failed: {error}"));
                    }
                });
            }
        });
        Ok(Self { endpoint, task })
    }

    #[cfg(windows)]
    pub fn start(command_sender: GatewayControlSender) -> AppResult<Self> {
        let endpoint = endpoint()?;
        let GatewayLocalEndpoint::WindowsNamedPipe(name) = &endpoint else {
            return Err(AppError::Message(
                "Windows Gateway daemon resolved a non-pipe control endpoint".into(),
            ));
        };
        let mut server = create_windows_pipe_server(name, true)?;
        let pipe_name = name.clone();
        let task = tokio::spawn(async move {
            loop {
                if let Err(error) = server.connect().await {
                    gateway_daemon::append_log(&format!(
                        "[control] named pipe connect failed: {error}"
                    ));
                    break;
                }
                let next = match create_windows_pipe_server(&pipe_name, false) {
                    Ok(next) => next,
                    Err(error) => {
                        gateway_daemon::append_log(&format!(
                            "[control] named pipe instance creation failed: {error}"
                        ));
                        break;
                    }
                };
                let connected = std::mem::replace(&mut server, next);
                let command_sender = command_sender.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(connected, &command_sender).await {
                        if should_log_connection_error(&error) {
                            gateway_daemon::append_log(&format!(
                                "[control] request failed: {error}"
                            ));
                        }
                    }
                });
            }
        });
        Ok(Self { endpoint, task })
    }

    #[cfg(not(any(unix, windows)))]
    pub fn start(_command_sender: GatewayControlSender) -> AppResult<Self> {
        Err(AppError::Message(
            "Gateway control server is not enabled on this platform yet".into(),
        ))
    }

    pub fn endpoint(&self) -> &GatewayLocalEndpoint {
        &self.endpoint
    }
}

impl Drop for GatewayControlServer {
    fn drop(&mut self) {
        self.task.abort();
        if let GatewayLocalEndpoint::UnixSocket(path) = &self.endpoint {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(any(unix, windows, test))]
async fn handle_connection<S>(mut stream: S, command_sender: &GatewayControlSender) -> AppResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request: GatewayRequest = read_json_frame(&mut stream)
        .await
        .map_err(|error| AppError::Message(format!("invalid Gateway request: {error:?}")))?;
    let handled = handle_request(request, !command_sender.is_closed()).await;
    write_json_frame(&mut stream, &handled.response).await?;
    // The response has already been flushed. A Windows Named Pipe client can
    // close before server-side shutdown and yield ERROR_NO_DATA (232); treat
    // that teardown race as successful completion rather than a request error.
    let _ = stream.shutdown().await;
    if let Some(command) = handled.command {
        let async_operation = match &command {
            GatewayControlCommand::Reload { operation_id }
            | GatewayControlCommand::ApplyConfig { operation_id, .. } => Some(operation_id.clone()),
            GatewayControlCommand::Shutdown { .. } => None,
        };
        if command_sender.send(command).is_err() {
            let error = AppError::Message("Gateway control receiver is unavailable".into());
            if let Some(operation_id) = async_operation {
                finish_reload_operation(&operation_id, Err(AppError::Message(error.to_string())));
            }
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(any(unix, windows, test))]
struct HandledRequest {
    response: GatewayResponse,
    command: Option<GatewayControlCommand>,
}

#[cfg(any(unix, windows, test))]
fn handled(response: GatewayResponse) -> HandledRequest {
    HandledRequest {
        response,
        command: None,
    }
}

#[cfg(any(unix, windows, test))]
async fn handle_request(request: GatewayRequest, command_available: bool) -> HandledRequest {
    if let Err(response) = validate_protocol_version(&request) {
        return handled(*response);
    }
    let request_id = request.request_id.clone();
    let expected_scope = match gateway_daemon::config_scope() {
        Ok(scope) => scope,
        Err(error) => {
            return handled(GatewayResponse::error(
                request_id,
                ERROR_CONFIG_SCOPE_MISMATCH,
                format!("cannot resolve server config scope: {error}"),
            ))
        }
    };
    if request.config_scope != expected_scope {
        return handled(GatewayResponse::error(
            request_id,
            ERROR_CONFIG_SCOPE_MISMATCH,
            format!(
                "Gateway config scope mismatch: request={}, daemon={expected_scope}",
                request.config_scope
            ),
        ));
    }
    match request.method {
        GatewayMethod::Ping => handled(GatewayResponse::success(
            request_id,
            GatewayResult::Pong {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
            },
        )),
        GatewayMethod::Version => handled(GatewayResponse::success(
            request_id,
            GatewayResult::Version {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
                build_identity: Some(crate::build_identity::BuildIdentity::current()),
            },
        )),
        GatewayMethod::Status => match daemon_status().await {
            Ok(status) => handled(GatewayResponse::success(
                request_id,
                GatewayResult::Status {
                    status: Box::new(status),
                },
            )),
            Err(error) => handled(GatewayResponse::error(
                request_id,
                ERROR_OPERATION_FAILED,
                error.to_string(),
            )),
        },
        GatewayMethod::Logs { tail_lines, cursor } => {
            match read_gateway_log(tail_lines, cursor.as_ref()) {
                Ok(chunk) => handled(GatewayResponse::success(
                    request_id,
                    GatewayResult::Logs { chunk },
                )),
                Err(error) => handled(GatewayResponse::error(
                    request_id,
                    ERROR_LOG_READ_FAILED,
                    error.to_string(),
                )),
            }
        }
        GatewayMethod::Events {
            cursor,
            limit,
            wait_ms,
        } => {
            let batch = read_gateway_events(&expected_scope, cursor.as_ref(), limit, wait_ms).await;
            handled(GatewayResponse::success(
                request_id,
                GatewayResult::Events { batch },
            ))
        }
        GatewayMethod::Shutdown => {
            lifecycle_request(request_id, GatewayOperation::Shutdown, command_available)
        }
        GatewayMethod::PrepareRestart => {
            lifecycle_request(request_id, GatewayOperation::Restart, command_available)
        }
        GatewayMethod::Reload => {
            if !command_available {
                return handled(GatewayResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    "Gateway reload receiver is unavailable",
                ));
            }
            let Some(operation_id) = create_operation(&expected_scope) else {
                return handled(GatewayResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    format!("Gateway already has {MAX_OPERATIONS} retained control operations"),
                ));
            };
            HandledRequest {
                response: GatewayResponse::success(
                    request_id,
                    GatewayResult::OperationAccepted {
                        operation_id: operation_id.clone(),
                        daemon_pid: std::process::id(),
                    },
                ),
                command: Some(GatewayControlCommand::Reload { operation_id }),
            }
        }
        GatewayMethod::ApplyConfig { config } => {
            if !command_available {
                return handled(GatewayResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    "Gateway apply_config receiver is unavailable",
                ));
            }
            let Some(operation_id) = create_operation(&expected_scope) else {
                return handled(GatewayResponse::error(
                    request_id,
                    ERROR_CONTROL_COMMAND_UNAVAILABLE,
                    format!("Gateway already has {MAX_OPERATIONS} retained control operations"),
                ));
            };
            HandledRequest {
                response: GatewayResponse::success(
                    request_id,
                    GatewayResult::OperationAccepted {
                        operation_id: operation_id.clone(),
                        daemon_pid: std::process::id(),
                    },
                ),
                command: Some(GatewayControlCommand::ApplyConfig {
                    operation_id,
                    config,
                }),
            }
        }
        GatewayMethod::OperationStatus { operation_id } => {
            match operation(&expected_scope, &operation_id) {
                Some(operation) => handled(GatewayResponse::success(
                    request_id,
                    GatewayResult::OperationStatus { operation },
                )),
                None => handled(GatewayResponse::error(
                    request_id,
                    ERROR_OPERATION_NOT_FOUND,
                    format!("unknown Gateway control operation: {operation_id}"),
                )),
            }
        }
    }
}

#[cfg(any(unix, windows, test))]
fn lifecycle_request(
    request_id: String,
    operation: GatewayOperation,
    command_available: bool,
) -> HandledRequest {
    if !command_available {
        return handled(GatewayResponse::error(
            request_id,
            ERROR_CONTROL_COMMAND_UNAVAILABLE,
            "Gateway lifecycle command receiver is unavailable",
        ));
    }
    HandledRequest {
        response: GatewayResponse::success(
            request_id,
            GatewayResult::Accepted {
                operation,
                daemon_pid: std::process::id(),
            },
        ),
        command: Some(GatewayControlCommand::Shutdown { operation }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_named_pipe_peer_close_errors_are_not_logged_as_request_failures() {
        for code in [109, 232] {
            let error = AppError::Io(io::Error::from_raw_os_error(code));
            assert!(!should_log_connection_error(&error));
        }
        assert!(should_log_connection_error(&AppError::Io(
            io::Error::from_raw_os_error(5)
        )));
    }

    #[test]
    fn gateway_lifecycle_drain_only_retries_supported_older_protocols() {
        let compatible = GatewayControlClientError::VersionMismatch {
            daemon_protocol: GATEWAY_LIFECYCLE_PROTOCOL_MIN_VERSION,
            client_protocol: GATEWAY_LIFECYCLE_PROTOCOL_MIN_VERSION + 1,
        };
        assert_eq!(
            legacy_lifecycle_retry_protocol(&compatible),
            Some(GATEWAY_LIFECYCLE_PROTOCOL_MIN_VERSION)
        );

        let newer = GatewayControlClientError::VersionMismatch {
            daemon_protocol: GATEWAY_CONTROL_PROTOCOL_VERSION + 1,
            client_protocol: GATEWAY_CONTROL_PROTOCOL_VERSION,
        };
        assert_eq!(legacy_lifecycle_retry_protocol(&newer), None);
    }
    use crate::settings::McpGatewayConfig;

    #[tokio::test]
    async fn framed_json_round_trip_is_newline_delimited() {
        let request = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id: "frame-request".into(),
            config_scope: "scope".into(),
            method: GatewayMethod::Ping,
        };
        let (mut client, mut server) = tokio::io::duplex(4096);
        let expected = request.clone();
        let writer = tokio::spawn(async move { write_json_frame(&mut client, &request).await });
        let decoded: GatewayRequest = read_json_frame(&mut server).await.expect("read frame");
        writer.await.expect("writer").expect("write frame");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn endpoint_transport_matches_current_platform() {
        let endpoint = endpoint().expect("endpoint");
        #[cfg(windows)]
        assert!(matches!(
            endpoint,
            GatewayLocalEndpoint::WindowsNamedPipe(_)
        ));
        #[cfg(unix)]
        assert!(matches!(endpoint, GatewayLocalEndpoint::UnixSocket(_)));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_gateway_named_pipe_accepts_repeated_requests() {
        let (command_sender, _command_receiver) = control_channel();
        let server = GatewayControlServer::start(command_sender).expect("start gateway pipe");
        assert!(matches!(
            server.endpoint(),
            GatewayLocalEndpoint::WindowsNamedPipe(name)
                if name.starts_with(r"\\.\pipe\") && name.contains("gateway")
        ));
        ping().await.expect("first gateway ping");
        ping().await.expect("second gateway ping");
    }

    #[tokio::test]
    async fn request_handler_rejects_another_config_scope() {
        let request = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id: "scope-mismatch".into(),
            config_scope: "definitely-not-the-current-scope".into(),
            method: GatewayMethod::Ping,
        };
        let handled = handle_request(request, true).await;
        assert!(!handled.response.ok);
        assert!(handled.command.is_none());
        assert_eq!(
            handled.response.error.expect("scope error").code,
            ERROR_CONFIG_SCOPE_MISMATCH
        );
    }

    #[tokio::test]
    async fn shutdown_response_is_flushed_before_command_delivery() {
        let scope = gateway_daemon::config_scope().expect("scope");
        let (mut client, server) = tokio::io::duplex(16 * 1024);
        let (sender, mut receiver) = control_channel();
        let server_task = tokio::spawn(async move { handle_connection(server, &sender).await });
        let request = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id: "shutdown-request".into(),
            config_scope: scope,
            method: GatewayMethod::Shutdown,
        };

        write_json_frame(&mut client, &request)
            .await
            .expect("write shutdown request");
        let response: GatewayResponse = read_json_frame(&mut client).await.expect("response");
        match response.result.expect("accepted") {
            GatewayResult::Accepted {
                operation,
                daemon_pid,
            } => {
                assert_eq!(operation, GatewayOperation::Shutdown);
                assert_eq!(daemon_pid, std::process::id());
            }
            other => panic!("unexpected response: {other:?}"),
        }
        server_task
            .await
            .expect("server task")
            .expect("serve shutdown");
        assert_eq!(
            receiver.recv().await,
            Some(GatewayControlCommand::Shutdown {
                operation: GatewayOperation::Shutdown,
            })
        );
    }

    #[tokio::test]
    async fn reload_is_accepted_before_execution_and_tracks_completion() {
        let scope = gateway_daemon::config_scope().expect("scope");
        let request = GatewayRequest {
            protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
            request_id: "reload-request".into(),
            config_scope: scope.clone(),
            method: GatewayMethod::Reload,
        };
        let handled = handle_request(request, true).await;
        let operation_id = match handled.response.result.expect("accepted") {
            GatewayResult::OperationAccepted {
                operation_id,
                daemon_pid,
            } => {
                assert_eq!(daemon_pid, std::process::id());
                operation_id
            }
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(
            handled.command,
            Some(GatewayControlCommand::Reload {
                operation_id: operation_id.clone(),
            })
        );

        mark_operation_running(&operation_id);
        finish_reload_operation(&operation_id, Ok(()));
        let completed = handle_request(
            GatewayRequest {
                protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
                request_id: "reload-status".into(),
                config_scope: scope,
                method: GatewayMethod::OperationStatus {
                    operation_id: operation_id.clone(),
                },
            },
            true,
        )
        .await;
        match completed.response.result.expect("status") {
            GatewayResult::OperationStatus { operation } => {
                assert_eq!(operation.operation_id, operation_id);
                assert_eq!(operation.state, GatewayAsyncState::Succeeded);
                assert!(operation.error.is_none());
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_config_is_a_daemon_command_not_a_local_write() {
        let scope = gateway_daemon::config_scope().expect("scope");
        let config = McpGatewayConfig {
            enabled: true,
            local_port: 31_234,
            ..McpGatewayConfig::default()
        };
        let handled = handle_request(
            GatewayRequest {
                protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
                request_id: "apply-config".into(),
                config_scope: scope,
                method: GatewayMethod::ApplyConfig {
                    config: Box::new(config.clone()),
                },
            },
            true,
        )
        .await;
        assert!(handled.response.ok);
        match handled.command.expect("apply command") {
            GatewayControlCommand::ApplyConfig { config: next, .. } => {
                assert_eq!(*next, config);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_request_is_read_only_and_resumable_through_v1_handler() {
        let scope = gateway_daemon::config_scope().expect("scope");
        super::super::events::reset_gateway_event_stream(&scope);
        super::super::events::publish_gateway_event(
            &scope,
            super::super::protocol::GatewayEventKind::DaemonReady,
            "running",
            "ready",
        );
        let first = handle_request(
            GatewayRequest {
                protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
                request_id: "events-first".into(),
                config_scope: scope.clone(),
                method: GatewayMethod::Events {
                    cursor: None,
                    limit: 8,
                    wait_ms: 0,
                },
            },
            false,
        )
        .await;
        assert!(first.command.is_none());
        let first_batch = match first.response.result.expect("events result") {
            GatewayResult::Events { batch } => batch,
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(first_batch.events.len(), 1);

        super::super::events::publish_gateway_event(
            &scope,
            super::super::protocol::GatewayEventKind::TunnelState,
            "recovering",
            "retry",
        );
        let resumed = handle_request(
            GatewayRequest {
                protocol_version: GATEWAY_CONTROL_PROTOCOL_VERSION,
                request_id: "events-resumed".into(),
                config_scope: scope,
                method: GatewayMethod::Events {
                    cursor: Some(first_batch.next_cursor),
                    limit: 8,
                    wait_ms: 0,
                },
            },
            false,
        )
        .await;
        assert!(resumed.command.is_none());
        match resumed.response.result.expect("resumed events") {
            GatewayResult::Events { batch } => {
                assert_eq!(batch.events.len(), 1);
                assert_eq!(
                    batch.events[0].kind,
                    super::super::protocol::GatewayEventKind::TunnelState
                );
                assert_eq!(batch.events[0].sequence, 2);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }
}
