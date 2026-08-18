use std::time::Duration;

use tauri::State;

use crate::app_state::AppState;
use crate::error::{AppError, AppResult};
use crate::management;

fn ensure_workspace_exists(state: &AppState, id: &str) -> AppResult<()> {
    state.with_workspaces(|store| {
        if store.get(id).is_some() {
            Ok(())
        } else {
            Err(AppError::Message(format!("workspace not found: {id}")))
        }
    })
}

#[tauri::command]
pub fn get_workspace_secret(
    _state: State<'_, AppState>,
    id: String,
    key: String,
) -> AppResult<Option<String>> {
    management::get_workspace_secret(&id, &key)
}

#[tauri::command]
pub fn set_workspace_secret(
    state: State<'_, AppState>,
    id: String,
    key: String,
    value: String,
) -> AppResult<()> {
    management::validate_workspace_secret_key(&key)?;
    ensure_workspace_exists(&state, &id)?;
    state.with_data(|store| store.set_workspace_secret(&id, &key, &value))
}

#[tauri::command]
pub fn regenerate_workspace_secret(
    state: State<'_, AppState>,
    id: String,
    key: String,
) -> AppResult<String> {
    management::validate_workspace_secret_key(&key)?;
    ensure_workspace_exists(&state, &id)?;
    let value = state.with_data(|store| store.regenerate_workspace_secret(&id, &key))?;
    let profile = state.with_workspaces(|store| {
        store
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })?;

    schedule_running_services_restart(vec![profile], key, false);
    Ok(value)
}

const MCP_SHARED_KEYS: &[&str] = &[
    "oauth_client_id",
    "bearer_token",
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
];

const ACTIONS_SHARED_KEYS: &[&str] = &[
    "actions_api_key",
    "actions_oauth_client_secret",
    "actions_oauth_password",
    "actions_oauth_token_secret",
];

#[tauri::command]
pub fn get_shared_secret(_state: State<'_, AppState>, key: String) -> AppResult<Option<String>> {
    management::get_shared_secret(&key)
}

#[tauri::command]
pub fn set_shared_secret(state: State<'_, AppState>, key: String, value: String) -> AppResult<()> {
    management::validate_shared_secret_key(&key)?;
    if value.is_empty() {
        return Err(AppError::Message("密钥不能为空。".into()));
    }
    let changed = state.with_data(|store| {
        if store.get_shared_secret(&key).as_deref() == Some(value.as_str()) {
            return Ok(false);
        }
        store.set_shared_secret(&key, &value)?;
        Ok(true)
    })?;
    if changed {
        let workspaces = state.with_workspaces(|store| Ok(store.list().to_vec()))?;
        schedule_running_services_restart(workspaces, key, true);
    }
    Ok(())
}

#[tauri::command]
pub fn regenerate_shared_secret(state: State<'_, AppState>, key: String) -> AppResult<String> {
    management::validate_shared_secret_key(&key)?;
    let value = state.with_data(|store| store.regenerate_shared_secret(&key))?;

    let workspaces = state.with_workspaces(|store| Ok(store.list().to_vec()))?;
    schedule_running_services_restart(workspaces, key, true);

    Ok(value)
}

fn schedule_running_services_restart(
    profiles: Vec<crate::workspace::WorkspaceProfile>,
    key: String,
    shared: bool,
) {
    crate::async_runtime::spawn(async move {
        for profile in &profiles {
            restart_running_services(profile, &key, shared).await;
        }
    });
}

/// 仅重启当前确实在运行、且使用了这组密钥的服务。
///
/// 密钥命令是桌面端和设置页共用的入口，因此重启必须放在后端统一处理。
/// 前端不再额外调用 restart_*，避免同一次密钥变更触发两次停止/启动竞态。
async fn restart_running_services(
    profile: &crate::workspace::WorkspaceProfile,
    key: &str,
    shared: bool,
) {
    let mcp_relevant = MCP_SHARED_KEYS.contains(&key) && profile.auth.use_shared_secrets == shared;
    let actions_relevant =
        ACTIONS_SHARED_KEYS.contains(&key) && profile.actions.use_shared_secrets == shared;
    match crate::daemon::inspect(profile) {
        Ok(inspection) if inspection.running => {
            let Some(daemon_state) = inspection.state else {
                return;
            };
            let service = if mcp_relevant && daemon_state.service.includes_mcp() {
                Some(crate::workspace::resources::WorkspaceService::Mcp)
            } else if actions_relevant && daemon_state.service.includes_actions() {
                Some(crate::workspace::resources::WorkspaceService::Actions)
            } else {
                None
            };
            if let Some(service) = service {
                if let Err(error) = crate::control::restart_daemon_service(
                    profile,
                    service,
                    daemon_state.tunnel,
                    Duration::from_secs(15),
                    true,
                )
                .await
                {
                    eprintln!(
                        "daemon restart after secret regeneration failed for {}: {error}",
                        profile.id
                    );
                }
            }
            return;
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!(
                "daemon inspection after secret regeneration failed for {}: {error}",
                profile.id
            );
            return;
        }
    }
}
