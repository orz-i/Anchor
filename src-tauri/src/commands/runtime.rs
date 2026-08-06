use tauri::State;

use std::time::Duration;

use crate::app_state::AppState;

use crate::control::{self, WorkspaceControlStatus};
use crate::error::{AppError, AppResult};

use crate::runtime::{
    port_busy_message, try_reclaim_previous_macos_app_port, update_public_url, wait_for_port_free,
};

use crate::mcp::gateway::{self, McpGatewayStatus};

use crate::platform::platform;

use crate::tunnel::{maybe_start_for_runtime, reconcile_mcp_gateway, TunnelServiceKind};

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

#[tauri::command]
pub fn get_mcp_gateway(state: State<'_, AppState>) -> AppResult<McpGatewayConfig> {
    state.with_settings(|store| Ok(store.settings().mcp_gateway))
}

#[tauri::command]
pub async fn get_mcp_gateway_status(state: State<'_, AppState>) -> AppResult<McpGatewayStatus> {
    let config = state.with_settings(|store| Ok(store.settings().mcp_gateway))?;
    Ok(gateway::status(&config).await)
}

fn restart_active_mcp_listeners(
    state: &AppState,
    profiles: &[crate::workspace::WorkspaceProfile],
    active: &std::collections::HashSet<String>,
) -> AppResult<()> {
    for profile in profiles
        .iter()
        .filter(|profile| active.contains(&profile.id))
    {
        let status = state.with_runtime(|runtime| runtime.restart_mcp(profile))?;
        if status.state != "running" {
            return Err(AppError::Message(format!(
                "重启工作区“{}”的 MCP listener 后状态为 {}：{}",
                profile.name, status.state, status.local_message
            )));
        }
    }
    Ok(())
}

const DESKTOP_DAEMON_TIMEOUT: Duration = Duration::from_secs(15);

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
    let (port, local_endpoint, public_endpoint, public_message, label) = match service {
        WorkspaceService::Mcp => (
            &status.mcp,
            profile.local_endpoint(),
            profile.public_endpoint_with(settings),
            profile.mcp_external_base_url_with(settings),
            "MCP",
        ),
        WorkspaceService::Actions => (
            &status.actions,
            profile.actions_local_base_url(),
            profile.actions_openapi_url_with(settings),
            profile.actions_effective_public_url_with(settings),
            "Actions",
        ),
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
    control::set_daemon_service(&profile, service, true, true, DESKTOP_DAEMON_TIMEOUT, true)
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
    control::restart_daemon_service(&profile, service, true, DESKTOP_DAEMON_TIMEOUT, true).await?;
    daemon_runtime_status(state, &profile_by_id(state, id)?, service).await
}

#[tauri::command]
pub async fn set_mcp_gateway(
    state: State<'_, AppState>,
    mut config: McpGatewayConfig,
) -> AppResult<McpGatewayStatus> {
    config.public_url = config.public_url.trim().trim_end_matches('/').to_string();
    let profiles = state.with_workspaces(|store| Ok(store.list().to_vec()))?;
    if config.enabled {
        for profile in &profiles {
            let inspection = crate::daemon::inspect(profile)?;
            if inspection
                .state
                .as_ref()
                .filter(|_| inspection.running)
                .is_some_and(|state| state.service.includes_mcp())
            {
                return Err(AppError::Message(format!(
                    "Workspace {} 的 MCP 正由 daemon 管理。Gateway 写控制尚未迁移到 daemon；请先停止该 Workspace，再配置 Gateway。GUI 不会启动第二套进程内 listener。",
                    profile.name
                )));
            }
        }
    }
    let active = state.with_runtime(|runtime| Ok(runtime.active_mcp_workspace_ids()))?;
    let previous = state.with_settings(|store| Ok(store.settings().mcp_gateway))?;
    let listener_policy_changed = previous.enabled != config.enabled;
    if previous.identity_changed(&config) {
        config.clear_observation();
    } else {
        config.observed_public_url = previous.observed_public_url.clone();
        config.observed_owner_workspace_id = previous.observed_owner_workspace_id.clone();
        config.observed_tunnel_signature = previous.observed_tunnel_signature.clone();
    }
    gateway::validate_config(&config, &profiles)?;
    state.with_settings(|store| {
        let mut settings = store.settings();
        settings.mcp_gateway = config.clone();
        store.update_settings(settings)
    })?;

    let applied = async {
        if listener_policy_changed {
            restart_active_mcp_listeners(&state, &profiles, &active)?;
        }
        if config.enabled {
            reconcile_gateway_state(&state).await
        } else {
            restore_direct_mcp_exposure(&state).await?;
            Ok(gateway::status(&config).await)
        }
    }
    .await;
    if let Err(error) = applied {
        state.with_settings(|store| {
            let mut settings = store.settings();
            settings.mcp_gateway = previous.clone();
            store.update_settings(settings)
        })?;
        let rollback = async {
            if listener_policy_changed {
                restart_active_mcp_listeners(&state, &profiles, &active)?;
            }
            if previous.enabled {
                reconcile_gateway_state(&state).await.map(|_| ())
            } else {
                restore_direct_mcp_exposure(&state).await
            }
        }
        .await;
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(AppError::Message(format!(
                "应用 MCP Gateway 配置失败：{error}；恢复上一配置也失败：{rollback_error}"
            ))),
        };
    }
    applied
}

fn gateway_context(
    state: &AppState,
) -> AppResult<(
    AppSettings,
    Vec<crate::workspace::WorkspaceProfile>,
    std::collections::HashSet<String>,
)> {
    let (settings, profiles) =
        state.with_settings(|store| Ok((store.settings(), store.list().to_vec())))?;
    let active = state.with_runtime(|runtime| Ok(runtime.active_mcp_workspace_ids()))?;
    Ok((settings, profiles, active))
}

fn persist_gateway_observation(
    state: &AppState,
    config: &McpGatewayConfig,
    profiles: &[crate::workspace::WorkspaceProfile],
    url: &str,
) -> AppResult<()> {
    let normalized = url.trim().trim_end_matches('/');
    if normalized.is_empty() || normalized.starts_with("http://127.0.0.1:") {
        return Ok(());
    }
    let owner = profiles
        .iter()
        .find(|profile| profile.id == config.owner_workspace_id)
        .ok_or_else(|| AppError::Message("MCP Gateway 隧道所有者工作区不存在。".into()))?;
    let signature = gateway::tunnel_identity_signature(config, owner)?;
    let mut candidate = config.clone();
    candidate.observed_public_url = normalized.to_string();
    candidate.observed_owner_workspace_id = config.owner_workspace_id.clone();
    candidate.observed_tunnel_signature = signature.clone();
    gateway::validate_config(&candidate, profiles)?;
    state.with_settings(|store| {
        let mut settings = store.settings();
        if settings.mcp_gateway.identity_changed(config) {
            return Ok(());
        }
        if settings.mcp_gateway.observed_public_url == normalized
            && settings.mcp_gateway.observed_owner_workspace_id == config.owner_workspace_id
            && settings.mcp_gateway.observed_tunnel_signature == signature
        {
            return Ok(());
        }
        settings.mcp_gateway.observed_public_url = normalized.to_string();
        settings.mcp_gateway.observed_owner_workspace_id = config.owner_workspace_id.clone();
        settings.mcp_gateway.observed_tunnel_signature = signature;
        store.update_settings(settings)
    })
}

async fn reconcile_gateway_state(state: &AppState) -> AppResult<McpGatewayStatus> {
    let (settings, profiles, active) = gateway_context(state)?;
    let mut status = gateway::ensure(&settings.mcp_gateway, &profiles, &active).await?;
    let public_url = reconcile_mcp_gateway(&settings.mcp_gateway, &profiles, &active).await?;
    if let Some(public_url) = public_url {
        persist_gateway_observation(state, &settings.mcp_gateway, &profiles, &public_url)?;
        status.public_base_url = public_url;
    }
    gateway::clear_runtime_error().await;
    Ok(status)
}

async fn restore_direct_mcp_exposure(state: &AppState) -> AppResult<()> {
    let (settings, profiles, active) = gateway_context(state)?;
    gateway::stop().await?;
    reconcile_mcp_gateway(&settings.mcp_gateway, &profiles, &active).await?;
    for profile in profiles
        .iter()
        .filter(|profile| active.contains(&profile.id))
    {
        if let Some(url) = maybe_start_for_runtime(profile, TunnelServiceKind::Mcp).await? {
            persist_tunnel_url(state, &profile.id, TunnelServiceKind::Mcp, &url)?;
        }
        let current = profile.effective_public_url_with(&settings);
        update_public_url(&profile.id, "mcp", &current);
    }
    Ok(())
}

fn validate_start_resources(
    state: &AppState,
    id: &str,
    service: WorkspaceService,
) -> AppResult<()> {
    state.with_workspaces(|store| validate_service_start(store.list(), id, service))
}

fn persist_tunnel_url(
    state: &AppState,
    id: &str,
    kind: TunnelServiceKind,
    url: &str,
) -> AppResult<()> {
    if url.is_empty() {
        return Ok(());
    }

    state.with_workspaces(|store| {
        let Some(mut profile) = store.get(id).cloned() else {
            return Ok(());
        };

        match kind {
            TunnelServiceKind::Mcp => profile.tunnel.public_url = url.to_string(),

            TunnelServiceKind::Actions => profile.actions.public_url = url.to_string(),
        }

        store.update(profile)?;

        Ok(())
    })?;
    let service = match kind {
        TunnelServiceKind::Mcp => "mcp",
        TunnelServiceKind::Actions => "actions",
    };
    update_public_url(id, service, url);
    Ok(())
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
