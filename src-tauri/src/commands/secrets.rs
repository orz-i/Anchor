use tauri::State;

use crate::app_state::AppState;
use crate::error::AppResult;
use crate::management;

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
    management::set_workspace_secret(&id, &key, &value)?;
    state.reload_data_from_disk()
}

#[tauri::command]
pub fn regenerate_workspace_secret(
    state: State<'_, AppState>,
    id: String,
    key: String,
) -> AppResult<String> {
    let value = management::regenerate_workspace_secret(&id, &key)?;
    state.reload_data_from_disk()?;
    Ok(value)
}

#[tauri::command]
pub fn get_shared_secret(_state: State<'_, AppState>, key: String) -> AppResult<Option<String>> {
    management::get_shared_secret(&key)
}

#[tauri::command]
pub fn set_shared_secret(state: State<'_, AppState>, key: String, value: String) -> AppResult<()> {
    management::set_shared_secret(&key, &value)?;
    state.reload_data_from_disk()
}

#[tauri::command]
pub fn regenerate_shared_secret(state: State<'_, AppState>, key: String) -> AppResult<String> {
    let value = management::regenerate_shared_secret(&key)?;
    state.reload_data_from_disk()?;
    Ok(value)
}
