use tauri::State;

use std::time::Duration;

use crate::app_state::AppState;

use crate::control::{self, WorkspaceControlStatus};
use crate::error::{AppError, AppResult};

use crate::runtime::{port_busy_message, try_reclaim_previous_macos_app_port, wait_for_port_free};

use crate::gateway_control::{self, GatewayControlStatus, GatewayOperation};
use crate::gateway_daemon::{self, GatewayDaemonInspection};
use crate::mcp::gateway;

use crate::platform::platform;

use crate::settings::{AppSettings, McpGatewayConfig};
use crate::workspace::resources::{validate_service_start, WorkspaceService};
use crate::workspace::{RuntimeRecoveryDto, RuntimeStatusDto};

fn profile_by_id(state: &AppState, id: &str) -> AppResult<crate::workspace::WorkspaceProfile> {
    state.with_workspaces(|store| {
        store
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })
}

fn tunnel_configured_for_service(
    profile: &crate::workspace::WorkspaceProfile,
    service: WorkspaceService,
) -> bool {
    match service {
        WorkspaceService::Mcp => profile.tunnel.tunnel_type != "none",
        WorkspaceService::Actions => profile.actions.tunnel_type != "none",
    }
}

#[tauri::command]
pub fn get_mcp_gateway(state: State<'_, AppState>) -> AppResult<McpGatewayConfig> {
    state.with_settings(|store| Ok(store.settings().mcp_gateway))
}

#[tauri::command]
pub async fn get_mcp_gateway_status(
    _state: State<'_, AppState>,
) -> AppResult<GatewayControlStatus> {
    gateway_control::status_via_daemon_or_local().await
}

const DESKTOP_DAEMON_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayConfigWriteAction {
    PersistLocally,
    ApplyViaDaemon { pid: u32 },
    ShutdownThenPersist { pid: u32 },
}

fn gateway_config_write_action(
    inspection: &GatewayDaemonInspection,
    enabled: bool,
) -> AppResult<GatewayConfigWriteAction> {
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail.clone()));
    }
    if !inspection.running {
        return Ok(GatewayConfigWriteAction::PersistLocally);
    }
    if !inspection.pid_matches {
        return Err(AppError::Message(
            "Gateway daemon reports running but PID ownership does not match".into(),
        ));
    }
    let pid = inspection
        .state
        .as_ref()
        .map(|state| state.pid)
        .ok_or_else(|| {
            AppError::Message("Gateway daemon reports running without state metadata".into())
        })?;
    if enabled {
        Ok(GatewayConfigWriteAction::ApplyViaDaemon { pid })
    } else {
        Ok(GatewayConfigWriteAction::ShutdownThenPersist { pid })
    }
}

fn selection_includes(
    selection: crate::daemon::ServiceSelection,
    service: WorkspaceService,
) -> bool {
    match service {
        WorkspaceService::Mcp => selection.includes_mcp(),
        WorkspaceService::Actions => selection.includes_actions(),
    }
}

fn empty_recovery(enabled: bool, last_error: String) -> RuntimeRecoveryDto {
    RuntimeRecoveryDto {
        enabled,
        attempt: 0,
        max_attempts: 0,
        retry_in_ms: None,
        recovered_count: 0,
        last_error,
    }
}

fn runtime_status_from_control(
    profile: &crate::workspace::WorkspaceProfile,
    settings: &AppSettings,
    status: &WorkspaceControlStatus,
    service: WorkspaceService,
) -> RuntimeStatusDto {
    let active_tunnel_url = match service {
        WorkspaceService::Mcp => status.mcp_tunnel.as_ref(),
        WorkspaceService::Actions => status.actions_tunnel.as_ref(),
    }
    .filter(|tunnel| tunnel.state == "running")
    .map(|tunnel| tunnel.public_url.trim().trim_end_matches('/'))
    .filter(|url| !url.is_empty());
    let (port, local_endpoint, public_endpoint, public_message, label) = match service {
        WorkspaceService::Mcp => {
            let fallback_base = profile.mcp_external_base_url_with(settings);
            let public_base = active_tunnel_url.unwrap_or(fallback_base.as_str());
            (
                &status.mcp,
                profile.local_endpoint(),
                if public_base.is_empty() {
                    String::new()
                } else {
                    format!("{public_base}/mcp")
                },
                public_base.to_string(),
                "MCP",
            )
        }
        WorkspaceService::Actions => {
            let fallback_base = profile.actions_effective_public_url_with(settings);
            let public_base = active_tunnel_url.unwrap_or(fallback_base.as_str());
            (
                &status.actions,
                profile.actions_local_base_url(),
                if public_base.is_empty() {
                    String::new()
                } else {
                    format!("{public_base}/openapi.json")
                },
                public_base.to_string(),
                "Actions",
            )
        }
    };
    let daemon_pid = status
        .daemon
        .state
        .as_ref()
        .filter(|_| status.daemon.running)
        .map(|state| state.pid);
    let selected = status
        .daemon
        .state
        .as_ref()
        .filter(|_| status.daemon.running)
        .is_some_and(|state| selection_includes(state.service, service));

    let (state, pid, local_message, recovery) =
        if status.daemon.ambiguous || (status.daemon.stale && status.daemon.state.is_some()) {
            (
                "error",
                daemon_pid,
                status.daemon.detail.clone(),
                empty_recovery(false, status.daemon.detail.clone()),
            )
        } else if selected && port.owner == "daemon" {
            (
                "running",
                daemon_pid,
                format!("{label} 由 daemon 监听 127.0.0.1:{}", port.port),
                empty_recovery(false, String::new()),
            )
        } else if port.owner == "external" {
            let message = format!(
                "{label} 端口 {} 由外部 PID {} 占用，GUI 不会接管该进程",
                port.port,
                port.pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "unknown".into())
            );
            (
                "error",
                port.pid,
                message.clone(),
                empty_recovery(false, message),
            )
        } else if selected && status.daemon.running {
            let message = format!(
                "{label} daemon 正在运行，但端口 {} 暂未监听；等待 daemon 自动恢复",
                port.port
            );
            (
                "recovering",
                daemon_pid,
                message.clone(),
                empty_recovery(true, message),
            )
        } else if !status.daemon.supported {
            (
                "stopped",
                None,
                format!("未启动；{}", status.daemon.detail),
                empty_recovery(false, String::new()),
            )
        } else {
            (
                "stopped",
                None,
                "未启动".into(),
                empty_recovery(false, String::new()),
            )
        };

    RuntimeStatusDto {
        state: state.into(),
        pid,
        local_message,
        public_message: if public_message.is_empty() {
            "未配置公网访问".into()
        } else {
            public_message
        },
        local_endpoint,
        public_endpoint,
        recovery,
        activity: match service {
            WorkspaceService::Mcp => status.mcp_activity.clone(),
            WorkspaceService::Actions => None,
        },
    }
}

async fn daemon_runtime_status(
    state: &AppState,
    profile: &crate::workspace::WorkspaceProfile,
    service: WorkspaceService,
) -> AppResult<RuntimeStatusDto> {
    let settings = state.with_settings(|store| Ok(store.settings()))?;
    let status = control::workspace_status_via_daemon_or_local(profile).await?;
    Ok(runtime_status_from_control(
        profile, &settings, &status, service,
    ))
}

fn ensure_daemon_gateway_compatible(
    state: &AppState,
    desired: Option<crate::daemon::ServiceSelection>,
) -> AppResult<()> {
    let gateway_enabled = state.with_settings(|store| Ok(store.settings().mcp_gateway.enabled))?;
    if gateway_enabled && desired.is_some_and(crate::daemon::ServiceSelection::includes_mcp) {
        return Err(AppError::Message(
            "MCP Gateway 模式尚未迁移到统一 daemon 控制面；请先关闭 Gateway，或使用 CLI `anchor gateway serve`。GUI 不会回退到进程内 RuntimeSupervisor。"
                .into(),
        ));
    }
    Ok(())
}

async fn start_desktop_service(
    state: &AppState,
    id: &str,
    service: WorkspaceService,
) -> AppResult<RuntimeStatusDto> {
    validate_start_resources(state, id, service)?;
    let profile = profile_by_id(state, id)?;
    let inspection = crate::daemon::inspect(&profile)?;
    let current = inspection
        .state
        .as_ref()
        .filter(|_| inspection.running)
        .map(|state| state.service);
    let desired = control::desired_service_selection(current, service, true);
    ensure_daemon_gateway_compatible(state, desired)?;
    if !current.is_some_and(|selection| selection_includes(selection, service)) {
        match service {
            WorkspaceService::Mcp => {
                ensure_port_available(profile.runtime.local_port, "本地 MCP").await?
            }
            WorkspaceService::Actions => {
                ensure_port_available(profile.actions.local_port, "本地 Actions").await?
            }
        }
    }
    control::set_daemon_service(
        &profile,
        service,
        true,
        tunnel_configured_for_service(&profile, service),
        DESKTOP_DAEMON_TIMEOUT,
        true,
    )
    .await?;
    daemon_runtime_status(state, &profile_by_id(state, id)?, service).await
}

async fn stop_desktop_service(
    state: &AppState,
    id: &str,
    service: WorkspaceService,
) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(state, id)?;
    control::set_daemon_service(
        &profile,
        service,
        false,
        false,
        DESKTOP_DAEMON_TIMEOUT,
        true,
    )
    .await?;
    daemon_runtime_status(state, &profile_by_id(state, id)?, service).await
}

async fn restart_desktop_service(
    state: &AppState,
    id: &str,
    service: WorkspaceService,
) -> AppResult<RuntimeStatusDto> {
    validate_start_resources(state, id, service)?;
    let profile = profile_by_id(state, id)?;
    let inspection = crate::daemon::inspect(&profile)?;
    let current = inspection
        .state
        .as_ref()
        .filter(|_| inspection.running)
        .map(|state| state.service);
    let desired = control::desired_service_selection(current, service, true);
    ensure_daemon_gateway_compatible(state, desired)?;
    if !current.is_some_and(|selection| selection_includes(selection, service)) {
        match service {
            WorkspaceService::Mcp => {
                ensure_port_available(profile.runtime.local_port, "本地 MCP").await?
            }
            WorkspaceService::Actions => {
                ensure_port_available(profile.actions.local_port, "本地 Actions").await?
            }
        }
    }
    control::restart_daemon_service(
        &profile,
        service,
        tunnel_configured_for_service(&profile, service),
        DESKTOP_DAEMON_TIMEOUT,
        true,
    )
    .await?;
    daemon_runtime_status(state, &profile_by_id(state, id)?, service).await
}

#[tauri::command]
pub async fn set_mcp_gateway(
    state: State<'_, AppState>,
    mut config: McpGatewayConfig,
) -> AppResult<GatewayControlStatus> {
    config.public_url = config.public_url.trim().trim_end_matches('/').to_string();
    let profiles = state.with_workspaces(|store| Ok(store.list().to_vec()))?;
    let previous = state.with_settings(|store| Ok(store.settings().mcp_gateway))?;
    let legacy_status = gateway::status(&previous).await;
    if legacy_status.state == "running" && previous != config {
        return Err(AppError::Message(
            "当前桌面进程仍在运行旧版兼容 Gateway。为避免已运行 listener 与新配置分叉，本次热修改已拒绝；请先退出旧桌面运行态，再保存配置。新的后台 Gateway 运行应由独立 `anchor gateway start` 控制域负责。"
                .into(),
        ));
    }
    if previous.identity_changed(&config) {
        config.clear_observation();
    } else {
        config.observed_public_url = previous.observed_public_url.clone();
        config.observed_owner_workspace_id = previous.observed_owner_workspace_id.clone();
        config.observed_tunnel_signature = previous.observed_tunnel_signature.clone();
    }
    gateway::validate_config(&config, &profiles)?;

    let inspection = gateway_daemon::inspect()?;
    match gateway_config_write_action(&inspection, config.enabled)? {
        GatewayConfigWriteAction::ApplyViaDaemon { pid } => {
            gateway_control::ping().await.map_err(|error| {
                AppError::Message(format!("Gateway daemon IPC 不可用：{error}"))
            })?;
            gateway_control::request_apply_config(config, DESKTOP_DAEMON_TIMEOUT)
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            let status = gateway_control::request_status()
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            if status.pid != Some(pid) {
                return Err(AppError::Message(format!(
                    "Gateway apply_config PID changed unexpectedly: expected {pid}, status={:?}",
                    status.pid
                )));
            }
        }
        GatewayConfigWriteAction::ShutdownThenPersist { pid } => {
            gateway_control::ping().await.map_err(|error| {
                AppError::Message(format!("Gateway daemon IPC 不可用：{error}"))
            })?;
            let accepted_pid = gateway_control::request_exit(GatewayOperation::Shutdown)
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            if accepted_pid != pid {
                return Err(AppError::Message(format!(
                    "Gateway disable PID mismatch: state={}, response={accepted_pid}",
                    pid
                )));
            }
            gateway_daemon::wait_for_exit(pid, DESKTOP_DAEMON_TIMEOUT, false).await?;
            gateway_control::persist_config(&config)?;
        }
        GatewayConfigWriteAction::PersistLocally => gateway_control::persist_config(&config)?,
    }
    state.reload_data_from_disk()?;
    gateway_control::status_via_daemon_or_local().await
}

fn validate_start_resources(
    state: &AppState,
    id: &str,
    service: WorkspaceService,
) -> AppResult<()> {
    state.with_workspaces(|store| validate_service_start(store.list(), id, service))
}

#[allow(clippy::collapsible_if)]
async fn ensure_port_available(port: u16, service_label: &str) -> AppResult<()> {
    let Some(pid) = platform().find_pid_listening_on_port(port)? else {
        return Ok(());
    };

    if crate::runtime::is_own_process(pid) {
        if wait_for_port_free(port, Duration::from_secs(3)).await {
            return Ok(());
        }
    }

    if try_reclaim_previous_macos_app_port(port) {
        return Ok(());
    }

    if let Some(pid) = platform().find_pid_listening_on_port(port)? {
        return Err(AppError::Message(port_busy_message(
            port,
            service_label,
            pid,
        )));
    }

    Ok(())
}

#[tauri::command]

pub async fn start_runtime(state: State<'_, AppState>, id: String) -> AppResult<RuntimeStatusDto> {
    start_desktop_service(&state, &id, WorkspaceService::Mcp).await
}

#[tauri::command]
pub async fn get_workspace_control_status(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<WorkspaceControlStatus> {
    let profile = profile_by_id(&state, &id)?;
    control::workspace_status_via_daemon_or_local(&profile).await
}

#[tauri::command]
pub async fn get_workspace_control_events(
    state: State<'_, AppState>,
    id: String,
    cursor: Option<control::ControlEventCursor>,
    wait_ms: u32,
) -> AppResult<Option<control::ControlEventBatch>> {
    let profile = profile_by_id(&state, &id)?;
    map_control_events(control::request_events(&profile, cursor, 64, wait_ms).await)
}

fn map_control_events(
    result: Result<control::ControlEventBatch, control::ControlClientError>,
) -> AppResult<Option<control::ControlEventBatch>> {
    match result {
        Ok(batch) => Ok(Some(batch)),
        Err(error) if error.is_unavailable() => Ok(None),
        Err(error) => Err(AppError::Message(error.to_string())),
    }
}

#[tauri::command]

pub async fn stop_runtime(state: State<'_, AppState>, id: String) -> AppResult<RuntimeStatusDto> {
    stop_desktop_service(&state, &id, WorkspaceService::Mcp).await
}

#[tauri::command]

pub async fn get_runtime_status(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(&state, &id)?;
    daemon_runtime_status(&state, &profile, WorkspaceService::Mcp).await
}

#[tauri::command]

pub async fn start_actions_runtime(
    state: State<'_, AppState>,

    id: String,
) -> AppResult<RuntimeStatusDto> {
    start_desktop_service(&state, &id, WorkspaceService::Actions).await
}

#[tauri::command]

pub async fn stop_actions_runtime(
    state: State<'_, AppState>,

    id: String,
) -> AppResult<RuntimeStatusDto> {
    stop_desktop_service(&state, &id, WorkspaceService::Actions).await
}

#[tauri::command]

pub async fn get_actions_runtime_status(
    state: State<'_, AppState>,

    id: String,
) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(&state, &id)?;
    daemon_runtime_status(&state, &profile, WorkspaceService::Actions).await
}

#[tauri::command]
pub async fn restart_runtime(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<RuntimeStatusDto> {
    restart_desktop_service(&state, &id, WorkspaceService::Mcp).await
}

#[tauri::command]
pub async fn restart_actions_runtime(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<RuntimeStatusDto> {
    restart_desktop_service(&state, &id, WorkspaceService::Actions).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{DaemonInspection, DaemonState, ServiceSelection};
    use crate::settings::AppSettings;
    use crate::workspace::WorkspaceProfile;

    fn gateway_inspection(
        running: bool,
        ambiguous: bool,
        pid_matches: bool,
    ) -> GatewayDaemonInspection {
        GatewayDaemonInspection {
            supported: true,
            running,
            stale: false,
            ambiguous,
            pid_matches,
            state: running.then(|| crate::gateway_daemon::GatewayDaemonState {
                schema_version: 1,
                config_scope: "scope".into(),
                pid: 42,
                started_at_unix: 1,
                workspace_ids: vec!["workspace".into()],
                local_port: 28_765,
                log_path: "gateway.log".into(),
                version: "test".into(),
            }),
            detail: if ambiguous {
                "ambiguous".into()
            } else {
                "ok".into()
            },
        }
    }

    #[test]
    fn gateway_config_writes_never_fallback_while_daemon_is_running() {
        let running = gateway_inspection(true, false, true);
        assert_eq!(
            gateway_config_write_action(&running, true).expect("enabled action"),
            GatewayConfigWriteAction::ApplyViaDaemon { pid: 42 }
        );
        assert_eq!(
            gateway_config_write_action(&running, false).expect("disabled action"),
            GatewayConfigWriteAction::ShutdownThenPersist { pid: 42 }
        );

        let stopped = gateway_inspection(false, false, false);
        assert_eq!(
            gateway_config_write_action(&stopped, true).expect("stopped action"),
            GatewayConfigWriteAction::PersistLocally
        );

        let ambiguous = gateway_inspection(false, true, false);
        assert!(gateway_config_write_action(&ambiguous, true).is_err());
        let wrong_owner = gateway_inspection(true, false, false);
        assert!(gateway_config_write_action(&wrong_owner, true).is_err());
    }

    #[test]
    fn event_monitor_only_falls_back_when_daemon_endpoint_is_unavailable() {
        let unavailable = map_control_events(Err(control::ControlClientError::Unavailable(
            "not running".into(),
        )))
        .expect("unavailable endpoint should permit polling fallback");
        assert!(unavailable.is_none());

        let protocol = map_control_events(Err(control::ControlClientError::Protocol(
            "version mismatch".into(),
        )))
        .expect_err("protocol errors must not silently fall back");
        assert!(protocol.to_string().contains("version mismatch"));
    }

    #[test]
    fn daemon_status_maps_each_service_without_process_local_runtime_state() {
        let profile = WorkspaceProfile::new(".".into(), Some("desktop-status".into()));
        let daemon_state = DaemonState {
            schema_version: 1,
            workspace_id: profile.id.clone(),
            workspace_name: profile.name.clone(),
            workspace_path: profile.path.clone(),
            pid: 42,
            started_at_unix: 1,
            service: ServiceSelection::Mcp,
            tunnel: true,
            tunnel_services: Some(ServiceSelection::Mcp),
            log_path: "daemon.log".into(),
            version: "test".into(),
        };
        let status = WorkspaceControlStatus {
            id: profile.id.clone(),
            name: profile.name.clone(),
            path: profile.path.clone(),
            daemon: DaemonInspection {
                supported: true,
                running: true,
                stale: false,
                ambiguous: false,
                pid_matches: true,
                state: Some(daemon_state),
                detail: "running".into(),
            },
            mcp: control::PortStatus {
                service: "mcp".into(),
                port: profile.runtime.local_port,
                listening: true,
                pid: Some(42),
                owner: "daemon".into(),
                endpoint: profile.local_endpoint(),
            },
            actions: control::PortStatus {
                service: "actions".into(),
                port: profile.actions.local_port,
                listening: false,
                pid: None,
                owner: "none".into(),
                endpoint: profile.actions_local_base_url(),
            },
            mcp_activity: None,
            mcp_tunnel: Some(crate::tunnel::TunnelStatus {
                state: "running".into(),
                public_url: "https://live-tunnel.example.com".into(),
                tunnel_pid: Some(77),
            }),
            actions_tunnel: None,
        };

        let mcp = runtime_status_from_control(
            &profile,
            &AppSettings::default(),
            &status,
            WorkspaceService::Mcp,
        );
        let actions = runtime_status_from_control(
            &profile,
            &AppSettings::default(),
            &status,
            WorkspaceService::Actions,
        );

        assert_eq!(mcp.state, "running");
        assert_eq!(mcp.pid, Some(42));
        assert_eq!(mcp.public_endpoint, "https://live-tunnel.example.com/mcp");
        assert_eq!(actions.state, "stopped");
    }

    #[test]
    fn external_port_is_reported_as_error_instead_of_being_adopted() {
        let profile = WorkspaceProfile::new(".".into(), Some("external-port".into()));
        let status = WorkspaceControlStatus {
            id: profile.id.clone(),
            name: profile.name.clone(),
            path: profile.path.clone(),
            daemon: DaemonInspection {
                supported: true,
                running: false,
                stale: false,
                ambiguous: false,
                pid_matches: false,
                state: None,
                detail: "stopped".into(),
            },
            mcp: control::PortStatus {
                service: "mcp".into(),
                port: profile.runtime.local_port,
                listening: true,
                pid: Some(99),
                owner: "external".into(),
                endpoint: profile.local_endpoint(),
            },
            actions: control::PortStatus {
                service: "actions".into(),
                port: profile.actions.local_port,
                listening: false,
                pid: None,
                owner: "none".into(),
                endpoint: profile.actions_local_base_url(),
            },
            mcp_activity: None,
            mcp_tunnel: None,
            actions_tunnel: None,
        };

        let runtime = runtime_status_from_control(
            &profile,
            &AppSettings::default(),
            &status,
            WorkspaceService::Mcp,
        );

        assert_eq!(runtime.state, "error");
        assert_eq!(runtime.pid, Some(99));
        assert!(runtime.local_message.contains("不会接管"));
    }
}
