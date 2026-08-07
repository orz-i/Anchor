use std::path::PathBuf;
use std::time::Duration;

use tauri::State;

use crate::app_state::{teardown_workspace, AppState};
use crate::auth::{update_oauth_redirect_policy, validate_redirect_policy};
use crate::daemon;
use crate::error::{AppError, AppResult};
use crate::platform::open_path_in_file_manager;
use crate::runtime::ServiceKind;
use crate::tunnel::append_profile_log;
use crate::tunnel::drop_workspace as drop_tunnel_workspace;
use crate::workspace::resources::{
    assign_free_workspace_ports_with_reserved, validate_workspace_resources_update,
};
use crate::workspace::WorkspaceProfile;

#[tauri::command]
pub fn list_workspaces(state: State<'_, AppState>) -> AppResult<Vec<WorkspaceProfile>> {
    state.with_workspaces(|store| Ok(store.list().to_vec()))
}

#[tauri::command]
pub fn inspect_workspace_skills(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
    roots: String,
) -> AppResult<serde_json::Value> {
    let profile = state.with_workspaces(|store| {
        store
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })?;
    let catalog = crate::skills::SkillCatalog::new(PathBuf::from(profile.path));
    catalog.configure(crate::skills::SkillSettings::from_text(enabled, &roots));
    Ok(serde_json::to_value(catalog.list(None, 200))?)
}

#[tauri::command]
pub fn create_workspace(
    state: State<'_, AppState>,
    path: String,
    name: Option<String>,
) -> AppResult<WorkspaceProfile> {
    state.with_workspaces(|store| {
        let mut profile = WorkspaceProfile::new(path, name);
        // Create should not fail just because default ports are already claimed.
        // Pick free ports now; start/update still enforce conflict checks.
        let gateway = store.settings().mcp_gateway;
        let reserved = if gateway.enabled {
            std::collections::HashSet::from([gateway.local_port])
        } else {
            std::collections::HashSet::new()
        };
        assign_free_workspace_ports_with_reserved(store.list(), &mut profile, &reserved)?;
        store.register_workspace(profile.clone())?;
        Ok(profile)
    })
}

#[cfg(test)]
mod tests {
    use super::validate_live_port_change;
    use crate::workspace::WorkspaceProfile;

    #[test]
    fn running_tunneled_service_rejects_live_port_change() {
        let current = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));
        let mut next = current.clone();
        next.runtime.local_port += 1;

        let error = validate_live_port_change(&current, &next, true, false, false)
            .expect_err("running tunnel must keep its local port");
        assert!(error.to_string().contains("保持当前公网链接不变"));
    }

    #[test]
    fn stopped_service_allows_port_change() {
        let current = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));
        let mut next = current.clone();
        next.runtime.local_port += 1;

        validate_live_port_change(&current, &next, false, false, false)
            .expect("stopped service may change port");
    }

    #[test]
    fn running_service_without_tunnel_allows_port_change() {
        let mut current = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));
        current.tunnel.tunnel_type = "none".into();
        let mut next = current.clone();
        next.runtime.local_port += 1;

        validate_live_port_change(&current, &next, true, false, false)
            .expect("listener-only port change is reloadable");
    }

    #[test]
    fn gateway_managed_mcp_rejects_live_port_change() {
        let mut current = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));
        current.tunnel.tunnel_type = "none".into();
        let mut next = current.clone();
        next.runtime.local_port += 1;

        validate_live_port_change(&current, &next, true, false, true)
            .expect_err("gateway route must keep its live target port");
    }
}

#[tauri::command]
pub async fn update_workspace(
    state: State<'_, AppState>,
    profile: WorkspaceProfile,
) -> AppResult<()> {
    let daemon_inspection = daemon::inspect(&profile)?;
    let daemon_state = daemon_inspection
        .running
        .then_some(daemon_inspection.state)
        .flatten();
    let (legacy_mcp_running, legacy_actions_running) = state.with_runtime(|runtime| {
        Ok((
            runtime.is_running(&profile.id, ServiceKind::Mcp),
            runtime.is_running(&profile.id, ServiceKind::Actions),
        ))
    })?;
    let previous_settings = state.with_settings(|store| Ok(store.settings()))?;
    let gateway_inspection = crate::gateway_daemon::inspect()?;
    if gateway_inspection.ambiguous {
        return Err(AppError::Message(gateway_inspection.detail));
    }
    #[cfg(windows)]
    let server_gateway_status = if super::runtime::desktop_server_mode() {
        Some(crate::mcp::gateway::status(&previous_settings.mcp_gateway).await)
    } else {
        None
    };
    #[cfg(not(windows))]
    let server_gateway_status: Option<crate::mcp::gateway::McpGatewayStatus> = None;
    let gateway_route_running = (gateway_inspection.running
        && gateway_inspection
            .state
            .as_ref()
            .is_some_and(|gateway_state| gateway_state.workspace_ids.contains(&profile.id)))
        || server_gateway_status.as_ref().is_some_and(|status| {
            status.state == "running" && status.route_workspace_ids.contains(&profile.id)
        });
    let gateway_owner_running = (gateway_inspection.running
        && previous_settings.mcp_gateway.owner_workspace_id == profile.id)
        || server_gateway_status.as_ref().is_some_and(|status| {
            status.state == "running" && status.owner_workspace_id == profile.id
        });
    let gateway_profile_live = gateway_route_running || gateway_owner_running;
    let mcp_running = daemon_state
        .as_ref()
        .is_some_and(|state| state.service.includes_mcp())
        || legacy_mcp_running
        || gateway_route_running;
    let actions_running = daemon_state
        .as_ref()
        .is_some_and(|state| state.service.includes_actions())
        || legacy_actions_running;
    let profile_id = profile.id.clone();
    let mcp_redirect_uris = profile.auth.oauth_redirect_uris.clone();
    let mcp_redirect_hosts = profile.auth.oauth_redirect_hosts.clone();
    let actions_redirect_uris = profile.actions.oauth_redirect_uris.clone();
    let actions_redirect_hosts = profile.actions.oauth_redirect_hosts.clone();
    let mcp_oauth_enabled = profile.auth.oauth_enabled();
    let actions_oauth_enabled = profile.actions.auth_type == "oauth";
    let (previous_profile, mcp_callback_changed, actions_callback_changed) = state
        .with_workspaces(|store| {
            let current = store
                .get(&profile.id)
                .cloned()
                .ok_or_else(|| AppError::Message(format!("workspace not found: {}", profile.id)))?;
            validate_live_port_change(
                &current,
                &profile,
                mcp_running,
                actions_running,
                store.settings().mcp_gateway.enabled,
            )?;
            validate_workspace_resources_update(store.list(), &current, &profile)?;
            let gateway = store.settings().mcp_gateway;
            crate::mcp::gateway::validate_workspace_ports(&gateway, &profile)?;
            let reset_gateway_observed =
                crate::mcp::gateway::owner_tunnel_identity_changed(&gateway, &current, &profile);
            let mcp_callback_changed = current.auth.oauth_redirect_uris
                != profile.auth.oauth_redirect_uris
                || current.auth.oauth_redirect_hosts != profile.auth.oauth_redirect_hosts;
            let actions_callback_changed = current.actions.oauth_redirect_uris
                != profile.actions.oauth_redirect_uris
                || current.actions.oauth_redirect_hosts != profile.actions.oauth_redirect_hosts;
            if mcp_callback_changed {
                validate_redirect_policy(&mcp_redirect_uris, &mcp_redirect_hosts)
                    .map_err(AppError::Message)?;
            }
            if actions_callback_changed {
                validate_redirect_policy(&actions_redirect_uris, &actions_redirect_hosts)
                    .map_err(AppError::Message)?;
            }
            store.update(profile)?;
            if reset_gateway_observed {
                let mut settings = store.settings();
                settings.mcp_gateway.clear_observation();
                store.update_settings(settings)?;
            }
            Ok((current, mcp_callback_changed, actions_callback_changed))
        })?;

    if mcp_callback_changed && mcp_oauth_enabled {
        let hot_updated = update_oauth_redirect_policy(
            &profile_id,
            "mcp",
            &mcp_redirect_uris,
            &mcp_redirect_hosts,
        )
        .map_err(AppError::Message)?;
        append_profile_log(
            &profile_id,
            "mcp-oauth.log",
            &format!(
                "[oauth] event=callback_policy_saved service=mcp runtime_hot_updated={hot_updated}"
            ),
        );
    }
    if actions_callback_changed && actions_oauth_enabled {
        let hot_updated = update_oauth_redirect_policy(
            &profile_id,
            "actions",
            &actions_redirect_uris,
            &actions_redirect_hosts,
        )
        .map_err(AppError::Message)?;
        append_profile_log(
            &profile_id,
            "actions-oauth.log",
            &format!(
                "[oauth] event=callback_policy_saved service=actions runtime_hot_updated={hot_updated}"
            ),
        );
    }
    if gateway_profile_live {
        #[cfg(windows)]
        if super::runtime::desktop_server_mode() {
            if let Err(error) = super::runtime::reconcile_server_gateway(&state).await {
                let restore = state.with_workspaces(|store| {
                    store.update(previous_profile.clone())?;
                    store.update_settings(previous_settings.clone())
                });
                return match restore {
                    Ok(()) => match super::runtime::reconcile_server_gateway(&state).await {
                        Ok(_) => Err(AppError::Message(format!(
                            "Windows GUI Server Gateway 未能应用 Workspace 配置，已恢复旧配置与旧运行态：{error}"
                        ))),
                        Err(reconcile_error) => Err(AppError::Message(format!(
                            "Windows GUI Server Gateway 未能应用 Workspace 配置：{error}；已恢复磁盘配置，但再次恢复旧运行态失败：{reconcile_error}"
                        ))),
                    },
                    Err(restore_error) => Err(AppError::Message(format!(
                        "Windows GUI Server Gateway 未能应用 Workspace 配置：{error}；恢复旧 Workspace 配置也失败：{restore_error}"
                    ))),
                };
            }
            return Ok(());
        }
        if let Err(error) =
            crate::gateway_control::request_reload(std::time::Duration::from_secs(20)).await
        {
            let restore = state.with_workspaces(|store| {
                store.update(previous_profile.clone())?;
                store.update_settings(previous_settings.clone())
            });
            return match restore {
                Ok(()) => {
                    let reconcile = crate::gateway_control::request_reload(
                        std::time::Duration::from_secs(20),
                    )
                    .await;
                    match reconcile {
                        Ok(()) => Err(AppError::Message(format!(
                            "Gateway daemon 未能应用 Workspace 配置，已恢复旧配置与旧运行态：{error}"
                        ))),
                        Err(reconcile_error) => Err(AppError::Message(format!(
                            "Gateway daemon 未能应用 Workspace 配置：{error}；已恢复磁盘配置，但再次 reload 旧配置失败：{reconcile_error}"
                        ))),
                    }
                }
                Err(restore_error) => Err(AppError::Message(format!(
                    "Gateway daemon 未能应用 Workspace 配置：{error}；恢复旧 Workspace 配置也失败：{restore_error}"
                ))),
            };
        }
        state.reload_data_from_disk()?;
    }
    Ok(())
}

fn validate_live_port_change(
    current: &WorkspaceProfile,
    next: &WorkspaceProfile,
    mcp_running: bool,
    actions_running: bool,
    gateway_enabled: bool,
) -> AppResult<()> {
    let mcp_tunnel_active =
        gateway_enabled || matches!(current.tunnel.tunnel_type.as_str(), "cloudflare" | "frp");
    if mcp_running && mcp_tunnel_active && current.runtime.local_port != next.runtime.local_port {
        return Err(AppError::Message(format!(
            "MCP 隧道正在使用本地端口 {}。为保持当前公网链接不变，运行期间不能改为端口 {}。请保留当前端口；如确需迁移端口，请先停止服务并重新配置隧道。",
            current.runtime.local_port, next.runtime.local_port
        )));
    }

    let actions_tunnel_active =
        matches!(current.actions.tunnel_type.as_str(), "cloudflare" | "frp");
    if actions_running
        && actions_tunnel_active
        && current.actions.local_port != next.actions.local_port
    {
        return Err(AppError::Message(format!(
            "Actions 隧道正在使用本地端口 {}。为保持当前公网链接不变，运行期间不能改为端口 {}。请保留当前端口；如确需迁移端口，请先停止服务并重新配置隧道。",
            current.actions.local_port, next.actions.local_port
        )));
    }
    Ok(())
}

#[tauri::command]
pub fn open_workspace_directory(path: String) -> AppResult<()> {
    let path = PathBuf::from(path.trim());
    open_path_in_file_manager(&path)
}

#[tauri::command]
pub async fn delete_workspace(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.with_settings(|store| {
        crate::mcp::gateway::ensure_workspace_is_not_owner(&store.settings().mcp_gateway, &id)
    })?;
    let gateway_inspection = crate::gateway_daemon::inspect()?;
    if gateway_inspection.ambiguous {
        return Err(AppError::Message(gateway_inspection.detail));
    }
    if gateway_inspection.running
        && gateway_inspection
            .state
            .as_ref()
            .is_some_and(|gateway_state| gateway_state.workspace_ids.contains(&id))
    {
        return Err(AppError::Message(
            "该 Workspace 正由 Gateway daemon 提供路由。请先执行 `anchor gateway stop`，再删除 Workspace；GUI 不会在后台静默改写 Gateway route 集合。"
                .into(),
        ));
    }
    #[cfg(windows)]
    if super::runtime::desktop_server_mode() {
        let gateway = state.with_settings(|store| Ok(store.settings().mcp_gateway))?;
        let status = crate::mcp::gateway::status(&gateway).await;
        if status.state == "running" && status.route_workspace_ids.contains(&id) {
            return Err(AppError::Message(
                "该 Workspace 正由 Windows GUI Server Gateway 提供路由。请先在 Workspace 页面停止 MCP 服务，再删除 Workspace。"
                    .into(),
            ));
        }
    }
    let profile = state.with_workspaces(|store| {
        store
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })?;
    crate::control::request_daemon_exit_and_wait(
        &profile,
        crate::control::ControlOperation::Shutdown,
        Duration::from_secs(15),
        true,
    )
    .await?;
    drop_tunnel_workspace(&id).await?;
    state.with_runtime(|runtime| {
        runtime.drop_workspace(&profile);
        Ok(())
    })?;
    state.with_workspaces(|store| {
        if store.remove(&id)?.is_some() {
            teardown_workspace(store, &id)?;
        }
        Ok(())
    })
}
