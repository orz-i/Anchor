use tauri::State;

use std::time::Duration;

use crate::app_state::AppState;

use crate::error::{AppError, AppResult};

use crate::runtime::{
    await_listener_shutdown, port_busy_message, try_reclaim_previous_macos_app_port,
    update_public_url, wait_for_port_free, ServiceKind,
};

use crate::mcp::gateway::{self, McpGatewayStatus};

use crate::platform::platform;

use crate::tunnel::{
    maybe_start_for_runtime, reconcile_mcp_gateway, stop_for_runtime,
    sync_managed_runtime_routes, TunnelServiceKind,
};

use crate::workspace::resources::{validate_service_start, WorkspaceService};
use crate::workspace::RuntimeStatusDto;
use crate::settings::{AppSettings, McpGatewayConfig};

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
pub async fn get_mcp_gateway_status(
    state: State<'_, AppState>,
) -> AppResult<McpGatewayStatus> {
    let config = state.with_settings(|store| Ok(store.settings().mcp_gateway))?;
    Ok(gateway::status(&config).await)
}

#[tauri::command]
pub async fn set_mcp_gateway(
    state: State<'_, AppState>,
    mut config: McpGatewayConfig,
) -> AppResult<McpGatewayStatus> {
    config.public_url = config.public_url.trim().trim_end_matches('/').to_string();
    let profiles = state.with_workspaces(|store| Ok(store.list().to_vec()))?;
    gateway::validate_config(&config, &profiles)?;
    let previous = state.with_settings(|store| Ok(store.settings().mcp_gateway))?;
    state.with_settings(|store| {
        let mut settings = store.settings();
        settings.mcp_gateway = config.clone();
        store.update_settings(settings)
    })?;

    let applied = if config.enabled {
        reconcile_gateway_state(&state).await
    } else {
        match restore_direct_mcp_exposure(&state).await {
            Ok(()) => Ok(gateway::status(&config).await),
            Err(error) => Err(error),
        }
    };
    if let Err(error) = applied {
        state.with_settings(|store| {
            let mut settings = store.settings();
            settings.mcp_gateway = previous.clone();
            store.update_settings(settings)
        })?;
        let rollback = if previous.enabled {
            reconcile_gateway_state(&state).await.map(|_| ())
        } else {
            restore_direct_mcp_exposure(&state).await
        };
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
) -> AppResult<(AppSettings, Vec<crate::workspace::WorkspaceProfile>, std::collections::HashSet<String>)>
{
    let (settings, profiles) = state.with_settings(|store| {
        Ok((store.settings(), store.list().to_vec()))
    })?;
    let active = state.with_runtime(|runtime| Ok(runtime.active_mcp_workspace_ids()))?;
    Ok((settings, profiles, active))
}

fn persist_gateway_public_url(state: &AppState, url: &str) -> AppResult<()> {
    let normalized = url.trim().trim_end_matches('/');
    if normalized.is_empty() || normalized.starts_with("http://127.0.0.1:") {
        return Ok(());
    }
    state.with_settings(|store| {
        let mut settings = store.settings();
        if settings.mcp_gateway.public_url == normalized {
            return Ok(());
        }
        settings.mcp_gateway.public_url = normalized.to_string();
        store.update_settings(settings)
    })
}

async fn reconcile_gateway_state(state: &AppState) -> AppResult<McpGatewayStatus> {
    let (settings, profiles, active) = gateway_context(state)?;
    let mut status = gateway::ensure(&settings.mcp_gateway, &profiles, &active).await?;
    let public_url = reconcile_mcp_gateway(&settings.mcp_gateway, &profiles, &active).await?;
    if let Some(public_url) = public_url {
        persist_gateway_public_url(state, &public_url)?;
        status.public_base_url = public_url;
    }
    Ok(status)
}

async fn restore_direct_mcp_exposure(state: &AppState) -> AppResult<()> {
    let (settings, profiles, active) = gateway_context(state)?;
    gateway::stop().await;
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
            eprintln!("mcp gateway auto-start failed for {id}: {error}");
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

        Ok(runtime.mcp_status(&profile))
    })
}

#[tauri::command]

pub async fn stop_runtime(state: State<'_, AppState>, id: String) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(&state, &id)?;

    let port = profile.runtime.local_port;

    let handle = state.with_runtime(|runtime| Ok(runtime.begin_stop(&id, ServiceKind::Mcp)))?;

    await_listener_shutdown(handle, port).await;

    state.with_runtime(|runtime| {
        runtime.finish_stop(&id, ServiceKind::Mcp);

        Ok(runtime.mcp_status(&profile))
    })?;
    let gateway_enabled = state.with_settings(|store| Ok(store.settings().mcp_gateway.enabled))?;
    if gateway_enabled {
        reconcile_gateway_state(&state).await?;
    } else {
        stop_for_runtime(&profile, TunnelServiceKind::Mcp).await?;
    }
    sync_tunnel_routes_from_runtime(&state).await?;
    state.with_runtime(|runtime| Ok(runtime.mcp_status(&profile)))
}

#[tauri::command]

pub fn get_runtime_status(state: State<'_, AppState>, id: String) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(&state, &id)?;

    state.with_runtime(|runtime| {
        runtime.refresh_mcp(&profile);

        Ok(runtime.mcp_status(&profile))
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

        Ok(runtime.actions_status(&profile))
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

        Ok(runtime.actions_status(&profile))
    })?;
    stop_for_runtime(&profile, TunnelServiceKind::Actions).await?;
    sync_tunnel_routes_from_runtime(&state).await?;
    state.with_runtime(|runtime| Ok(runtime.actions_status(&profile)))
}

#[tauri::command]

pub fn get_actions_runtime_status(
    state: State<'_, AppState>,

    id: String,
) -> AppResult<RuntimeStatusDto> {
    let profile = profile_by_id(&state, &id)?;

    state.with_runtime(|runtime| {
        runtime.refresh_actions(&profile);

        Ok(runtime.actions_status(&profile))
    })
}

#[tauri::command]

pub fn restart_runtime(state: State<'_, AppState>, id: String) -> AppResult<RuntimeStatusDto> {
    validate_start_resources(&state, &id, WorkspaceService::Mcp)?;
    let profile = profile_by_id(&state, &id)?;

    state.with_runtime(|runtime| runtime.restart_mcp(&profile))
}

#[tauri::command]

pub fn restart_actions_runtime(
    state: State<'_, AppState>,

    id: String,
) -> AppResult<RuntimeStatusDto> {
    validate_start_resources(&state, &id, WorkspaceService::Actions)?;
    let profile = profile_by_id(&state, &id)?;

    state.with_runtime(|runtime| runtime.restart_actions(&profile))
}
