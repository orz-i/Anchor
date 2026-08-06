use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::daemon;
use crate::error::{AppError, AppResult};
use crate::workspace::WorkspaceProfile;

#[cfg(any(unix, test))]
use super::protocol::{validate_protocol_version, ERROR_INTERNAL, ERROR_WORKSPACE_MISMATCH};
use super::protocol::{
    ControlMethod, ControlRequest, ControlResponse, ControlResult, MAX_CONTROL_FRAME_BYTES,
};
use super::{workspace_status, WorkspaceControlStatus};

const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_millis(750);

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
    let endpoint = endpoint(profile_id).map_err(|error| {
        ControlClientError::Protocol(format!("cannot resolve daemon control endpoint: {error}"))
    })?;
    let request = ControlRequest::new(method);
    let request_id = request.request_id.clone();
    let response = tokio::time::timeout(
        CONTROL_REQUEST_TIMEOUT,
        exchange_with_endpoint(&endpoint, &request),
    )
    .await
    .map_err(|_| {
        ControlClientError::Unavailable(format!("daemon control endpoint timed out: {endpoint:?}"))
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
    pub fn start(profile: WorkspaceProfile) -> AppResult<Self> {
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
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, &profile).await {
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
    pub fn start(_profile: WorkspaceProfile) -> AppResult<Self> {
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
async fn handle_connection<S>(mut stream: S, profile: &WorkspaceProfile) -> AppResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request: ControlRequest = read_json_frame(&mut stream)
        .await
        .map_err(|error| AppError::Message(format!("invalid control request: {error:?}")))?;
    let response = handle_request(request, profile);
    write_json_frame(&mut stream, &response).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(any(unix, test))]
fn handle_request(request: ControlRequest, profile: &WorkspaceProfile) -> ControlResponse {
    if let Err(response) = validate_protocol_version(&request) {
        return *response;
    }
    let request_id = request.request_id;
    match request.method {
        ControlMethod::Ping => ControlResponse::success(
            request_id,
            ControlResult::Pong {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
            },
        ),
        ControlMethod::Version => ControlResponse::success(
            request_id,
            ControlResult::Version {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                protocol_version: super::protocol::CONTROL_PROTOCOL_VERSION,
            },
        ),
        ControlMethod::WorkspaceStatus { workspace_id } => {
            if workspace_id != profile.id {
                return ControlResponse::error(
                    request_id,
                    ERROR_WORKSPACE_MISMATCH,
                    format!(
                        "control endpoint belongs to workspace {}, not {workspace_id}",
                        profile.id
                    ),
                );
            }
            match workspace_status(profile) {
                Ok(status) => ControlResponse::success(
                    request_id,
                    ControlResult::WorkspaceStatus {
                        status: Box::new(status),
                    },
                ),
                Err(error) => ControlResponse::error(request_id, ERROR_INTERNAL, error.to_string()),
            }
        }
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
        let server_task =
            tokio::spawn(async move { handle_connection(server, &server_profile).await });
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

    #[test]
    fn request_handler_rejects_cross_workspace_access() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-owner".into()));
        let request = ControlRequest {
            protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
            request_id: "mismatch-request".into(),
            method: ControlMethod::WorkspaceStatus {
                workspace_id: "another-workspace".into(),
            },
        };

        let response = handle_request(request, &profile);

        assert!(!response.ok);
        assert_eq!(
            response.error.expect("workspace mismatch").code,
            super::super::protocol::ERROR_WORKSPACE_MISMATCH
        );
    }

    #[test]
    fn request_handler_reports_protocol_and_daemon_versions() {
        let profile = WorkspaceProfile::new(".".into(), Some("ipc-version".into()));
        let response = handle_request(
            ControlRequest {
                protocol_version: super::super::protocol::CONTROL_PROTOCOL_VERSION,
                request_id: "version-request".into(),
                method: ControlMethod::Version,
            },
            &profile,
        );

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
}
