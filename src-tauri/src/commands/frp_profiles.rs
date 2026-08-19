use tauri::State;

use crate::app_state::AppState;

use crate::error::AppResult;
use crate::management;

use crate::settings::{AppSettings, FrpProfileInput, ProxyConfig};

pub use crate::management::FrpProfileDto;

#[tauri::command]

pub fn list_frp_profiles(_state: State<'_, AppState>) -> AppResult<Vec<FrpProfileDto>> {
    management::list_frp_profiles()
}

#[tauri::command]

pub fn save_frp_profile(
    state: State<'_, AppState>,

    profile: FrpProfileInput,

    token: Option<String>,
) -> AppResult<FrpProfileDto> {
    let mut saved = management::save_frp_profile_metadata(profile)?;
    if let Some(token) = token.filter(|value| !value.trim().is_empty()) {
        saved = management::set_frp_profile_token(&saved.id, token.trim())?;
    }
    state.reload_data_from_disk()?;
    Ok(saved)
}

#[tauri::command]

pub fn delete_frp_profile(state: State<'_, AppState>, id: String) -> AppResult<()> {
    management::delete_frp_profile(&id)?;
    state.reload_data_from_disk()
}

#[tauri::command]

pub fn get_app_settings(state: State<'_, AppState>) -> AppResult<AppSettings> {
    state.with_settings(|store| Ok(store.settings()))
}

#[tauri::command]

pub fn get_proxy(_state: State<'_, AppState>) -> AppResult<ProxyConfig> {
    management::get_proxy()
}

#[tauri::command]

pub fn set_proxy(_state: State<'_, AppState>, proxy: ProxyConfig) -> AppResult<()> {
    management::set_proxy(proxy)
}

#[tauri::command]

pub fn set_last_workspace(_state: State<'_, AppState>, id: String) -> AppResult<()> {
    management::set_last_workspace(id)
}

#[tauri::command]

pub fn get_last_workspace_id(_state: State<'_, AppState>) -> AppResult<String> {
    management::get_last_workspace_id()
}
