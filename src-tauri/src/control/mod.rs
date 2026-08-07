mod aggregate;
mod events;
mod ipc;
mod lifecycle;
mod logs;
pub mod protocol;

use serde::{Deserialize, Serialize};

use crate::daemon::{self, DaemonInspection};
use crate::error::AppResult;
use crate::platform::platform;
use crate::tunnel::TunnelStatus;
use crate::workspace::{McpActivityDto, WorkspaceProfile};

pub use aggregate::{
    control_plane_events, control_plane_status, workspace_service_state, ControlPlaneEvent,
    ControlPlaneEventBatch, ControlPlaneEventCursor, ControlPlaneEventSource, ControlPlaneStatus,
    ControlPlaneWorkspaceStatus,
};
pub(crate) use events::{
    publish_workspace_event, read_workspace_events, reset_workspace_event_stream,
};
pub use ipc::{
    control_channel, endpoint, ping as ipc_ping, request_daemon_exit, request_events, request_logs,
    request_reload_operation, request_tunnel_operation, request_workspace_status,
    workspace_status_via_daemon_or_local, ControlClientError, ControlServer, DaemonControlCommand,
    DaemonControlReceiver, DaemonControlSender, LocalControlEndpoint,
};
#[cfg(any(feature = "cli", test))]
pub(crate) use ipc::{
    finish_reload_operation, finish_tunnel_operation, mark_control_operation_running,
};
pub use lifecycle::{
    desired_service_selection, desired_tunnel_selection, ensure_daemon_running, reconcile_daemon,
    request_daemon_exit_and_wait, restart_daemon_service, service_is_selected, set_daemon_service,
    DaemonLaunchSpec,
};
pub use logs::read_log_batch;
pub use protocol::{
    ControlEvent, ControlEventBatch, ControlEventCursor, ControlEventKind, ControlLogChunk,
    ControlLogCursor, ControlLogSelection, ControlOperation, ControlService, ControlTunnelAction,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortStatus {
    pub service: String,
    pub port: u16,
    pub listening: bool,
    pub pid: Option<u32>,
    pub owner: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceControlStatus {
    pub id: String,
    pub name: String,
    pub path: String,
    pub daemon: DaemonInspection,
    pub mcp: PortStatus,
    pub actions: PortStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_activity: Option<McpActivityDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_tunnel: Option<TunnelStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions_tunnel: Option<TunnelStatus>,
}

pub fn workspace_status(profile: &WorkspaceProfile) -> AppResult<WorkspaceControlStatus> {
    let daemon = daemon::inspect(profile)?;
    let daemon_pid = daemon
        .state
        .as_ref()
        .filter(|_| daemon.running)
        .map(|state| state.pid);

    let mcp = port_status(
        "mcp",
        profile.runtime.local_port,
        profile.local_endpoint(),
        daemon_pid,
    )?;
    let mcp_activity = mcp
        .listening
        .then(|| crate::mcp::activity_snapshot(&profile.id));

    Ok(WorkspaceControlStatus {
        id: profile.id.clone(),
        name: profile.name.clone(),
        path: profile.path.clone(),
        daemon,
        mcp,
        actions: port_status(
            "actions",
            profile.actions.local_port,
            profile.actions_local_base_url(),
            daemon_pid,
        )?,
        mcp_activity,
        mcp_tunnel: None,
        actions_tunnel: None,
    })
}

pub fn workspace_statuses(profiles: &[WorkspaceProfile]) -> AppResult<Vec<WorkspaceControlStatus>> {
    profiles.iter().map(workspace_status).collect()
}

fn port_status(
    service: &str,
    port: u16,
    endpoint: String,
    daemon_pid: Option<u32>,
) -> AppResult<PortStatus> {
    let pid = platform().find_pid_listening_on_port(port)?;
    Ok(PortStatus {
        service: service.to_string(),
        port,
        listening: pid.is_some(),
        pid,
        owner: port_owner(pid, daemon_pid).to_string(),
        endpoint,
    })
}

fn port_owner(pid: Option<u32>, daemon_pid: Option<u32>) -> &'static str {
    match pid {
        Some(pid) if Some(pid) == daemon_pid => "daemon",
        Some(pid) if crate::runtime::is_own_process(pid) => "server",
        Some(_) => "external",
        None => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::port_owner;

    #[test]
    fn port_ownership_distinguishes_daemon_external_and_stopped() {
        assert_eq!(port_owner(Some(7), Some(7)), "daemon");
        assert_eq!(port_owner(Some(std::process::id()), None), "server");
        assert_eq!(port_owner(None, Some(7)), "none");
    }
}
