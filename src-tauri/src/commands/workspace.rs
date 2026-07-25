use std::path::PathBuf;

use tauri::State;

use crate::app_state::{teardown_workspace, AppState};
use crate::auth::{update_oauth_redirect_policy, validate_redirect_policy};
use crate::error::{AppError, AppResult};
use crate::platform::open_path_in_file_manager;
use crate::tunnel::drop_workspace as drop_tunnel_workspace;
use crate::tunnel::append_profile_log;
use crate::workspace::resources::{
    assign_free_workspace_ports, validate_workspace_resources_update,
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
        assign_free_workspace_ports(store.list(), &mut profile)?;
        store.register_workspace(profile.clone())?;
        Ok(profile)
    })
}

#[tauri::command]
pub fn update_workspace(state: State<'_, AppState>, profile: WorkspaceProfile) -> AppResult<()> {
    let profile_id = profile.id.clone();
    let mcp_redirect_uris = profile.auth.oauth_redirect_uris.clone();
    let mcp_redirect_hosts = profile.auth.oauth_redirect_hosts.clone();
    let actions_redirect_uris = profile.actions.oauth_redirect_uris.clone();
    let actions_redirect_hosts = profile.actions.oauth_redirect_hosts.clone();
    let mcp_oauth_enabled = profile.auth.oauth_enabled();
    let actions_oauth_enabled = profile.actions.auth_type == "oauth";
    let (mcp_callback_changed, actions_callback_changed) = state.with_workspaces(|store| {
        let current = store
            .get(&profile.id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {}", profile.id)))?;
        validate_workspace_resources_update(store.list(), &current, &profile)?;
        let mcp_callback_changed = current.auth.oauth_redirect_uris != profile.auth.oauth_redirect_uris
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
        Ok((mcp_callback_changed, actions_callback_changed))
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
    Ok(())
}

#[tauri::command]
pub fn open_workspace_directory(path: String) -> AppResult<()> {
    let path = PathBuf::from(path.trim());
    open_path_in_file_manager(&path)
}

#[tauri::command]
pub fn delete_workspace(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let profile = state.with_workspaces(|store| {
        store
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })?;
    crate::async_runtime::block_on(drop_tunnel_workspace(&id))?;
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
