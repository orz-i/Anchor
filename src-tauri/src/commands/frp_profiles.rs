use tauri::State;

use crate::app_state::AppState;

use crate::error::{AppError, AppResult};
use crate::management;

use crate::settings::{AppSettings, FrpProfile, FrpProfileInput, ProxyConfig};

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
    if profile.name.trim().is_empty() || profile.server.trim().is_empty() {
        return Err(AppError::Message("FRP 配置名称和服务器不能为空。".into()));
    }

    let mut saved = FrpProfile::from(profile);

    saved.name = saved.name.trim().to_string();

    saved.server = saved.server.trim().to_string();

    if saved.id.trim().is_empty() {
        saved.id = uuid::Uuid::new_v4().to_string().replace('-', "");
    }

    state.with_settings(|store| {
        let mut settings = store.settings();

        if let Some(existing) = settings
            .frp_profiles
            .iter_mut()
            .find(|item| item.id == saved.id)
        {
            *existing = saved.clone();
        } else {
            settings.frp_profiles.push(saved.clone());
        }

        store.update_settings(settings)?;

        if let Some(token) = token.filter(|value| !value.trim().is_empty()) {
            store.set_app_secret("frp_profile_token", &saved.id, token.trim())?;
        }

        Ok(())
    })?;

    let has_token = state.with_settings(|store| {
        Ok(store
            .get_app_secret("frp_profile_token", &saved.id)
            .is_some_and(|value| !value.trim().is_empty()))
    })?;

    Ok(FrpProfileDto {
        id: saved.id.clone(),

        name: saved.name,

        server: saved.server,

        server_port: saved.server_port,

        has_token,
    })
}

#[tauri::command]

pub fn delete_frp_profile(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.with_settings(|store| {
        let mut settings = store.settings();

        settings.frp_profiles.retain(|profile| profile.id != id);

        store.update_settings(settings)?;

        store.delete_app_secret("frp_profile_token", &id)
    })
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
