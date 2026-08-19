use crate::error::AppResult;
use crate::management;

#[tauri::command]
pub fn get_windows_service_status() -> AppResult<serde_json::Value> {
    management::windows_service_status()
}

#[tauri::command]
pub async fn install_windows_service() -> AppResult<serde_json::Value> {
    management::install_windows_service().await
}

#[tauri::command]
pub async fn uninstall_windows_service() -> AppResult<serde_json::Value> {
    management::uninstall_windows_service().await
}

#[tauri::command]
pub async fn start_windows_service() -> AppResult<serde_json::Value> {
    management::start_windows_service().await
}

#[tauri::command]
pub async fn stop_windows_service() -> AppResult<serde_json::Value> {
    management::stop_windows_service().await
}

#[tauri::command]
pub async fn restart_windows_service() -> AppResult<serde_json::Value> {
    management::restart_windows_service().await
}

#[tauri::command]
pub fn sync_windows_service_plan() -> AppResult<serde_json::Value> {
    management::sync_windows_service_plan()
}
