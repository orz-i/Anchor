use tauri::State;

use std::time::Duration;

use crate::app_state::AppState;

use crate::control::{self, WorkspaceControlStatus};
use crate::error::{AppError, AppResult};

use crate::runtime::{
    await_listener_shutdown, port_busy_message, try_reclaim_previous_macos_app_port,
    update_public_url, wait_for_port_free, ServiceKind,
};

use crate::mcp::gateway::{self, McpGatewayStatus};

use crate::platform::platform;

use crate::tunnel::{
    maybe_start_for_runtime, reconcile_mcp_gateway, stop_for_runtime,
    supervisor as tunnel_supervisor, sync_managed_runtime_routes, TunnelServiceKind,
};

use crate::settings::{AppSettings, McpGatewayConfig};
use crate::workspace::resources::{validate_service_start, WorkspaceService};
use crate::workspace::RuntimeStatusDto;

fn profile_by_id(state: &AppState, id: &str) -> AppResult<crate::workspace::WorkspaceProfile> {
    state.with_workspaces(|store| {
        store
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TunnelContinuitySnapshot {
    running: bool,
    public_url: String,
    pid: Option<u32>,
}

impl TunnelContinuitySnapshot {
    fn from_direct(status: crate::tunnel::TunnelStatus) -> Self {
        Self {
            running: status.state == "running",
            public_url: normalize_public_url(&status.public_url),
            pid: status.tunnel_pid,
        }
    }

    fn from_gateway(status: McpGatewayStatus) -> Self {
        Self {
            running: status.state == "running",
            public_url: normalize_public_url(&status.public_base_url),
            pid: None,
        }
    }
}

fn normalize_public_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn tunnel_continuity_preserved(
    before: &TunnelContinuitySnapshot,
    after: &TunnelContinuitySnapshot,
) -> bool {
    if !before.running {
        return true;
    }
    after.running
        && before.public_url == after.public_url
        && (before.pid.is_none() || before.pid == after.pid)
}

async fn tunnel_continuity_snapshot(
    state: &AppState,
    profile: &crate::workspace::WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<TunnelContinuitySnapshot> {
    let settings = state.with_settings(|store| Ok(store.settings()))?;
    if kind == TunnelServiceKind::Mcp && settings.mcp_gateway.enabled {
        return Ok(TunnelContinuitySnapshot::from_gateway(
            gateway::status(&settings.mcp_gateway).await,
        ));
    }
    let guard = tunnel_supervisor().lock().await;
    Ok(TunnelContinuitySnapshot::from_direct(
        guard.status(profile, kind, &settings),
    ))
}

async fn restart_listener_preserving_tunnel(
    state: &AppState,
    profile: &crate::workspace::WorkspaceProfile,
    kind: ServiceKind,
) -> AppResult<RuntimeStatusDto> {
    let tunnel_kind = match kind {
        ServiceKind::Mcp => TunnelServiceKind::Mcp,
        ServiceKind::Actions => TunnelServiceKind::Actions,
    };
    let before = tunnel_continuity_snapshot(state, profile, tunnel_kind).await?;
    let status = state.with_runtime(|runtime| match kind {
        ServiceKind::Mcp => runtime.restart_mcp(profile),
        ServiceKind::Actions => runtime.restart_actions(profile),
    })?;
    let after = tunnel_continuity_snapshot(state, profile, tunnel_kind).await?;
    let tunnel_preserved = tunnel_continuity_preserved(&before, &after);

    if status.state != "running" {
        let tunnel_detail = if tunnel_preserved {
            "隧道仍按原公网地址保留"
        } else {
            "同时检测到隧道连续性异常"
        };
        let message = format!(
            "{} listener 重载后状态为 {}：{}。{}，本地服务尚未恢复，请修正配置后再次重载。",
            match kind {
                ServiceKind::Mcp => "MCP",
                ServiceKind::Actions => "Actions",
            },
            status.state,
            status.local_message,
            tunnel_detail,
        );
        crate::tunnel::append_profile_log(
            &profile.id,
            match kind {
                ServiceKind::Mcp => "stderr.log",
                ServiceKind::Actions => "actions-stderr.log",
            },
            &format!(
                "[reload] listener_restarted=false tunnel_preserved={tunnel_preserved} {message}"
            ),
        );
        return Err(AppError::Message(message));
    }

    if !tunnel_preserved {
        let message = format!(
            "{} listener 已重启，但隧道连续性校验失败：重载前 URL={} PID={:?}，重载后 URL={} PID={:?}。请保持当前服务运行并检查隧道日志；不要重新注册 ChatGPT 插件，除非公网地址确实发生变化。",
            match kind {
                ServiceKind::Mcp => "MCP",
                ServiceKind::Actions => "Actions",
            },
            before.public_url,
            before.pid,
            after.public_url,
            after.pid,
        );
        crate::tunnel::append_profile_log(
            &profile.id,
            match kind {
                ServiceKind::Mcp => "stderr.log",
                ServiceKind::Actions => "actions-stderr.log",
            },
            &format!("[reload] tunnel_preserved=false {message}"),
        );
        return Err(AppError::Message(message));
    }

    crate::tunnel::append_profile_log(
        &profile.id,
        match kind {
            ServiceKind::Mcp => "stdout.log",
            ServiceKind::Actions => "actions-stdout.log",
        },
        &format!(
            "[reload] service={} listener_restarted=true tunnel_preserved=true public_url={} tunnel_pid={:?}",
            match kind {
                ServiceKind::Mcp => "mcp",
                ServiceKind::Actions => "actions",
            },
            after.public_url,
            after.pid,
        ),
    );
    Ok(status)
}

async fn rollback_started_mcp_runtime(
    state: &AppState,
    profile: &crate::workspace::WorkspaceProfile,
) -> AppResult<()> {
    let handle =
        state.with_runtime(|runtime| Ok(runtime.begin_stop(&profile.id, ServiceKind::Mcp)))?;
    await_listener_shutdown(handle, profile.runtime.local_port).await;
    state.with_runtime(|runtime| {
        runtime.finish_stop(&profile.id, ServiceKind::Mcp);
        Ok(())
    })?;
    sync_tunnel_routes_from_runtime(state).await
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

#[tauri::command]
pub async fn set_mcp_gateway(
    state: State<'_, AppState>,
    mut config: McpGatewayConfig,
) -> AppResult<McpGatewayStatus> {
    config.public_url = config.public_url.trim().trim_end_matches('/').to_string();
    let profiles = state.with_workspaces(|store| Ok(store.list().to_vec()))?;
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

async fn sync_tunnel_routes_from_runtime(state: &AppState) -> AppResult<()> {
    let active_keys = state.with_runtime(|runtime| Ok(runtime.active_tunnel_service_keys()))?;
    sync_managed_runtime_routes(active_keys).await
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
    validate_start_resources(&state, &id, WorkspaceService::Mcp)?;
    let profile = profile_by_id(&state, &id)?;

    ensure_port_available(profile.runtime.local_port, "本地 MCP").await?;

    state.with_runtime(|runtime| runtime.start_mcp(&profile))?;
    sync_tunnel_routes_from_runtime(&state).await?;

    let gateway_enabled = state.with_settings(|store| Ok(store.settings().mcp_gateway.enabled))?;
    if gateway_enabled {
        if let Err(error) = reconcile_gateway_state(&state).await {
            let rollback = rollback_started_mcp_runtime(&state, &profile).await;
            return match rollback {
                Ok(()) => Err(AppError::Message(format!(
                    "MCP Gateway 启动失败，已回滚工作区 listener：{error}"
                ))),
                Err(rollback_error) => Err(AppError::Message(format!(
                    "MCP Gateway 启动失败：{error}；回滚工作区 listener 也失败：{rollback_error}"
                ))),
            };
        }
    } else {
        match maybe_start_for_runtime(&profile, TunnelServiceKind::Mcp).await {
            Ok(Some(url)) => {
                persist_tunnel_url(&state, &id, TunnelServiceKind::Mcp, &url)?;
            }

            Ok(None) => {}

            Err(error) => {
                eprintln!("mcp tunnel auto-start failed for {id}: {error}");
            }
        }
    }

    let profile = profile_by_id(&state, &id)?;

    tokio::time::sleep(Duration::from_millis(250)).await;

    state.with_runtime(|runtime| {
        runtime.refresh_mcp(&profile);
        runtime.mcp_status(&profile)
    })
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
    let profile = profile_by_id(&state, &id)?;

    let port = profile.runtime.local_port;

    let handle = state.with_runtime(|runtime| Ok(runtime.begin_stop(&id, ServiceKind::Mcp)))?;

    await_listener_shutdown(handle, port).await;

    state.with_runtime(|runtime| {
        runtime.finish_stop(&id, ServiceKind::Mcp);
        Ok(())
    })?;
    let gateway_enabled = state.with_settings(|store| Ok(store.settings().mcp_gateway.enabled))?;
    if gateway_enabled {
        reconcile_gateway_state(&state).await?;
    } else {
        stop_for_runtime(&profile, TunnelServiceKind::Mcp).await?;
    }
    sync_tunnel_routes_from_runtime(&state).await?;
    state.with_runtime(|runtime| runtime.mcp_status(&profile))
}

#[tauri::command]

pub fn get_runtime_status(state: State<'_, AppState>, id: String) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(&state, &id)?;

    state.with_runtime(|runtime| {
        runtime.refresh_mcp(&profile);
        runtime.mcp_status(&profile)
    })
}

#[tauri::command]

pub async fn start_actions_runtime(
    state: State<'_, AppState>,

    id: String,
) -> AppResult<RuntimeStatusDto> {
    validate_start_resources(&state, &id, WorkspaceService::Actions)?;
    let profile = profile_by_id(&state, &id)?;

    ensure_port_available(profile.actions.local_port, "本地 Actions").await?;

    state.with_runtime(|runtime| runtime.start_actions(&profile))?;
    sync_tunnel_routes_from_runtime(&state).await?;

    match maybe_start_for_runtime(&profile, TunnelServiceKind::Actions).await {
        Ok(Some(url)) => {
            persist_tunnel_url(&state, &id, TunnelServiceKind::Actions, &url)?;
        }

        Ok(None) => {}

        Err(error) => {
            eprintln!("actions tunnel auto-start failed for {id}: {error}");
        }
    }

    let profile = profile_by_id(&state, &id)?;

    tokio::time::sleep(Duration::from_millis(250)).await;

    state.with_runtime(|runtime| {
        runtime.refresh_actions(&profile);
        runtime.actions_status(&profile)
    })
}

#[tauri::command]

pub async fn stop_actions_runtime(
    state: State<'_, AppState>,

    id: String,
) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(&state, &id)?;

    let port = profile.actions.local_port;

    let handle = state.with_runtime(|runtime| Ok(runtime.begin_stop(&id, ServiceKind::Actions)))?;

    await_listener_shutdown(handle, port).await;

    state.with_runtime(|runtime| {
        runtime.finish_stop(&id, ServiceKind::Actions);
        Ok(())
    })?;
    stop_for_runtime(&profile, TunnelServiceKind::Actions).await?;
    sync_tunnel_routes_from_runtime(&state).await?;
    state.with_runtime(|runtime| runtime.actions_status(&profile))
}

#[tauri::command]

pub fn get_actions_runtime_status(
    state: State<'_, AppState>,

    id: String,
) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(&state, &id)?;

    state.with_runtime(|runtime| {
        runtime.refresh_actions(&profile);
        runtime.actions_status(&profile)
    })
}

#[tauri::command]
pub async fn restart_runtime(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<RuntimeStatusDto> {
    validate_start_resources(&state, &id, WorkspaceService::Mcp)?;
    let profile = profile_by_id(&state, &id)?;
    restart_listener_preserving_tunnel(&state, &profile, ServiceKind::Mcp).await
}

#[tauri::command]
pub async fn restart_actions_runtime(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<RuntimeStatusDto> {
    validate_start_resources(&state, &id, WorkspaceService::Actions)?;
    let profile = profile_by_id(&state, &id)?;
    restart_listener_preserving_tunnel(&state, &profile, ServiceKind::Actions).await
}

#[cfg(test)]
mod tests {
    use super::{tunnel_continuity_preserved, TunnelContinuitySnapshot};

    fn snapshot(running: bool, url: &str, pid: Option<u32>) -> TunnelContinuitySnapshot {
        TunnelContinuitySnapshot {
            running,
            public_url: url.into(),
            pid,
        }
    }

    #[test]
    fn stopped_tunnel_does_not_block_listener_reload() {
        assert!(tunnel_continuity_preserved(
            &snapshot(false, "", None),
            &snapshot(false, "", None),
        ));
    }

    #[test]
    fn running_tunnel_requires_same_url_and_process() {
        let before = snapshot(true, "https://stable.example.com", Some(42));
        assert!(tunnel_continuity_preserved(
            &before,
            &snapshot(true, "https://stable.example.com", Some(42)),
        ));
        assert!(!tunnel_continuity_preserved(
            &before,
            &snapshot(true, "https://changed.example.com", Some(42)),
        ));
        assert!(!tunnel_continuity_preserved(
            &before,
            &snapshot(true, "https://stable.example.com", Some(43)),
        ));
        assert!(!tunnel_continuity_preserved(
            &before,
            &snapshot(false, "https://stable.example.com", None),
        ));
    }

    #[test]
    fn gateway_continuity_uses_stable_public_url_without_pid() {
        assert!(tunnel_continuity_preserved(
            &snapshot(true, "https://gateway.example.com", None),
            &snapshot(true, "https://gateway.example.com", None),
        ));
    }
}
