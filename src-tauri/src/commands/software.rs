use tauri::State;

use crate::app_state::AppState;
use crate::error::AppResult;
use crate::management;
use crate::settings::DownloadConfig;
use crate::tunnel::SoftwareStatus;

/// List install status for frpc and cloudflared.
#[tauri::command]
pub fn list_software() -> AppResult<Vec<SoftwareStatus>> {
    management::list_software()
}

/// Download-install the requested binary ("frpc" | "cloudflared").
#[tauri::command]
pub async fn install_software(kind: String) -> AppResult<SoftwareStatus> {
    management::install_software(&kind).await
}

/// Remove a cache-managed binary ("frpc" | "cloudflared").
#[tauri::command]
pub fn uninstall_software(kind: String) -> AppResult<SoftwareStatus> {
    management::uninstall_software(&kind)
}

/// Read the download config (mirror + proxy).
#[tauri::command]
pub fn get_download_config(_state: State<'_, AppState>) -> AppResult<DownloadConfig> {
    management::get_download_config()
}

/// Persist the download config (mirror + proxy).
#[tauri::command]
pub fn set_download_config(_state: State<'_, AppState>, config: DownloadConfig) -> AppResult<()> {
    management::set_download_config(config)
}
