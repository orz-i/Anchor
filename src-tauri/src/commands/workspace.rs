use std::time::Duration;

use tauri::State;

use crate::app_state::AppState;
use crate::auth::validate_redirect_policy;
use crate::daemon;
use crate::error::{AppError, AppResult};
use crate::management;
use crate::workspace::config_apply::plan_workspace_config_apply;
use crate::workspace::resources::validate_workspace_resources_update;
use crate::workspace::WorkspaceProfile;

const WORKSPACE_CONFIG_APPLY_TIMEOUT: Duration = Duration::from_secs(15);

#[tauri::command]
pub fn list_workspaces(_state: State<'_, AppState>) -> AppResult<Vec<WorkspaceProfile>> {
    management::list_workspaces()
}

fn control_service(
    service: crate::workspace::resources::WorkspaceService,
) -> crate::control::ControlService {
    match service {
        crate::workspace::resources::WorkspaceService::Mcp => crate::control::ControlService::Mcp,
        crate::workspace::resources::WorkspaceService::Actions => {
            crate::control::ControlService::Actions
        }
    }
}

fn oauth_redirect_policy(
    profile: &WorkspaceProfile,
    service: crate::workspace::resources::WorkspaceService,
) -> (&str, &str, bool, &'static str) {
    match service {
        crate::workspace::resources::WorkspaceService::Mcp => (
            &profile.auth.oauth_redirect_uris,
            &profile.auth.oauth_redirect_hosts,
            profile.auth.oauth_enabled(),
            "mcp",
        ),
        crate::workspace::resources::WorkspaceService::Actions => (
            &profile.actions.oauth_redirect_uris,
            &profile.actions.oauth_redirect_hosts,
            profile.actions.auth_type == "oauth",
            "actions",
        ),
    }
}

async fn apply_live_service_config(
    profile: &WorkspaceProfile,
    service: crate::workspace::resources::WorkspaceService,
    daemon_running: bool,
    listener_reload: bool,
    callback_policy_hot_update: bool,
) -> AppResult<bool> {
    let (redirect_uris, redirect_hosts, oauth_enabled, service_name) =
        oauth_redirect_policy(profile, service);
    let callback_policy_hot_update = callback_policy_hot_update && oauth_enabled;
    if !listener_reload && !callback_policy_hot_update {
        return Ok(false);
    }
    if !daemon_running {
        return Ok(false);
    }

    if daemon_running {
        let control_service = control_service(service);
        if listener_reload {
            crate::control::request_reload_operation(
                profile,
                control_service,
                WORKSPACE_CONFIG_APPLY_TIMEOUT,
            )
            .await
            .map_err(|error| {
                AppError::Message(format!(
                    "daemon 未能应用 {service_name} listener 配置：{error}；写操作不会回退到 GUI 运行时"
                ))
            })?;
            return Ok(true);
        }
        if callback_policy_hot_update {
            let applied = crate::control::request_oauth_redirect_policy_update(
                profile,
                control_service,
                redirect_uris,
                redirect_hosts,
            )
            .await
            .map_err(|error| {
                AppError::Message(format!(
                    "daemon 未能热更新 {service_name} OAuth Callback 策略：{error}；写操作不会回退到 GUI 运行时"
                ))
            })?;
            if applied {
                return Ok(true);
            }
            crate::control::request_reload_operation(
                profile,
                control_service,
                WORKSPACE_CONFIG_APPLY_TIMEOUT,
            )
            .await
            .map_err(|error| {
                AppError::Message(format!(
                    "daemon 未找到活动 {service_name} OAuth runtime，且 listener fallback reload 失败：{error}"
                ))
            })?;
            return Ok(true);
        }
    }

    Ok(false)
}

fn restore_workspace_config(
    state: &AppState,
    previous_profile: &WorkspaceProfile,
    previous_settings: &crate::settings::AppSettings,
) -> AppResult<()> {
    state.with_workspaces(|store| {
        store.update(previous_profile.clone())?;
        store.update_settings(previous_settings.clone())
    })
}

#[derive(Debug, Clone, Copy)]
struct WorkspaceConfigRollbackScope {
    daemon_mcp_running: bool,
    daemon_actions_running: bool,
    mcp_applied: bool,
    actions_applied: bool,
    gateway_reload: bool,
}

async fn rollback_workspace_config(
    state: &AppState,
    previous_profile: &WorkspaceProfile,
    previous_settings: &crate::settings::AppSettings,
    scope: WorkspaceConfigRollbackScope,
    original_error: AppError,
) -> AppError {
    if let Err(restore_error) = restore_workspace_config(state, previous_profile, previous_settings)
    {
        return AppError::Message(format!(
            "应用 Workspace 配置失败：{original_error}；恢复旧磁盘配置也失败：{restore_error}"
        ));
    }

    let mut rollback_errors = Vec::new();
    if scope.mcp_applied {
        if let Err(error) = apply_live_service_config(
            previous_profile,
            crate::workspace::resources::WorkspaceService::Mcp,
            scope.daemon_mcp_running,
            true,
            false,
        )
        .await
        {
            rollback_errors.push(format!("MCP listener: {error}"));
        }
    }
    if scope.actions_applied {
        if let Err(error) = apply_live_service_config(
            previous_profile,
            crate::workspace::resources::WorkspaceService::Actions,
            scope.daemon_actions_running,
            true,
            false,
        )
        .await
        {
            rollback_errors.push(format!("Actions listener: {error}"));
        }
    }
    if scope.gateway_reload {
        let gateway_result = crate::gateway_control::request_reload(Duration::from_secs(20))
            .await
            .map_err(|error| AppError::Message(error.to_string()));
        if let Err(error) = gateway_result {
            rollback_errors.push(format!("Gateway: {error}"));
        }
    }

    if rollback_errors.is_empty() {
        AppError::Message(format!(
            "应用 Workspace 配置失败，已恢复旧配置与已触及运行态：{original_error}"
        ))
    } else {
        AppError::Message(format!(
            "应用 Workspace 配置失败：{original_error}；旧磁盘配置已恢复，但运行态回滚存在错误：{}",
            rollback_errors.join("；")
        ))
    }
}

#[tauri::command]
pub fn inspect_workspace_skills(
    _state: State<'_, AppState>,
    id: String,
    enabled: bool,
    roots: String,
) -> AppResult<serde_json::Value> {
    management::inspect_workspace_skills(&id, enabled, &roots)
}

#[tauri::command]
pub fn create_workspace(
    state: State<'_, AppState>,
    path: String,
    name: Option<String>,
) -> AppResult<WorkspaceProfile> {
    let profile = management::create_workspace(path, name)?;
    state.reload_data_from_disk()?;
    Ok(profile)
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
    let daemon_mcp_running = daemon_state
        .as_ref()
        .is_some_and(|state| state.service.includes_mcp());
    let daemon_actions_running = daemon_state
        .as_ref()
        .is_some_and(|state| state.service.includes_actions());
    let previous_settings = state.with_settings(|store| Ok(store.settings()))?;
    let gateway_inspection = crate::gateway_daemon::inspect()?;
    if gateway_inspection.ambiguous {
        return Err(AppError::Message(gateway_inspection.detail));
    }
    let gateway_route_running = gateway_inspection.running
        && gateway_inspection
            .state
            .as_ref()
            .is_some_and(|gateway_state| gateway_state.workspace_ids.contains(&profile.id));
    let gateway_owner_running = gateway_inspection.running
        && previous_settings.mcp_gateway.owner_workspace_id == profile.id;
    let gateway_profile_live = gateway_route_running || gateway_owner_running;
    let mcp_running = daemon_mcp_running || gateway_route_running;
    let actions_running = daemon_actions_running;
    let (previous_profile, apply_plan) = state.with_workspaces(|store| {
        let current = store
            .get(&profile.id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {}", profile.id)))?;
        let apply_plan = plan_workspace_config_apply(&current, &profile);
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
        if apply_plan.mcp_callback_policy_hot_update {
            validate_redirect_policy(
                &profile.auth.oauth_redirect_uris,
                &profile.auth.oauth_redirect_hosts,
            )
            .map_err(AppError::Message)?;
        }
        if apply_plan.actions_callback_policy_hot_update {
            validate_redirect_policy(
                &profile.actions.oauth_redirect_uris,
                &profile.actions.oauth_redirect_hosts,
            )
            .map_err(AppError::Message)?;
        }
        store.update(profile.clone())?;
        if reset_gateway_observed {
            let mut settings = store.settings();
            settings.mcp_gateway.clear_observation();
            store.update_settings(settings)?;
        }
        Ok((current, apply_plan))
    })?;

    let mcp_applied = match apply_live_service_config(
        &profile,
        crate::workspace::resources::WorkspaceService::Mcp,
        daemon_mcp_running,
        apply_plan.mcp_listener_reload,
        apply_plan.mcp_callback_policy_hot_update,
    )
    .await
    {
        Ok(applied) => applied,
        Err(error) => {
            return Err(rollback_workspace_config(
                &state,
                &previous_profile,
                &previous_settings,
                WorkspaceConfigRollbackScope {
                    daemon_mcp_running,
                    daemon_actions_running,
                    mcp_applied: false,
                    actions_applied: false,
                    gateway_reload: false,
                },
                error,
            )
            .await)
        }
    };
    let actions_applied = match apply_live_service_config(
        &profile,
        crate::workspace::resources::WorkspaceService::Actions,
        daemon_actions_running,
        apply_plan.actions_listener_reload,
        apply_plan.actions_callback_policy_hot_update,
    )
    .await
    {
        Ok(applied) => applied,
        Err(error) => {
            return Err(rollback_workspace_config(
                &state,
                &previous_profile,
                &previous_settings,
                WorkspaceConfigRollbackScope {
                    daemon_mcp_running,
                    daemon_actions_running,
                    mcp_applied,
                    actions_applied: false,
                    gateway_reload: false,
                },
                error,
            )
            .await)
        }
    };

    let gateway_reload = gateway_profile_live && apply_plan.mcp_tunnel_changed;
    if gateway_reload {
        if let Err(error) =
            crate::gateway_control::request_reload(std::time::Duration::from_secs(20)).await
        {
            return Err(rollback_workspace_config(
                &state,
                &previous_profile,
                &previous_settings,
                WorkspaceConfigRollbackScope {
                    daemon_mcp_running,
                    daemon_actions_running,
                    mcp_applied,
                    actions_applied,
                    gateway_reload: true,
                },
                AppError::Message(error.to_string()),
            )
            .await);
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
    management::open_workspace_directory(&path)
}

#[tauri::command]
pub async fn delete_workspace(state: State<'_, AppState>, id: String) -> AppResult<()> {
    management::delete_workspace(&id).await?;
    state.reload_data_from_disk()
}
