use crate::error::{AppError, AppResult};

#[cfg(not(windows))]
fn unsupported() -> AppError {
    AppError::Message("Windows SCM Service 仅支持 Windows".into())
}

#[cfg(windows)]
async fn run_elevated(action: &'static str) -> AppResult<()> {
    tokio::task::spawn_blocking(move || crate::windows_service::run_elevated_admin_action(action))
        .await
        .map_err(|error| AppError::Message(format!("Windows UAC helper task failed: {error}")))?
}

#[tauri::command]
pub fn get_windows_service_status() -> AppResult<serde_json::Value> {
    crate::management::windows_service_status()
}

#[tauri::command]
pub async fn install_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_elevated("install").await?;
        Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
    }
    #[cfg(not(windows))]
    Err(unsupported())
}

#[tauri::command]
pub async fn uninstall_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_elevated("uninstall").await?;
        Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
    }
    #[cfg(not(windows))]
    Err(unsupported())
}

#[tauri::command]
pub async fn start_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_elevated("start").await?;
        Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
    }
    #[cfg(not(windows))]
    Err(unsupported())
}

#[tauri::command]
pub async fn stop_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_elevated("stop").await?;
        Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
    }
    #[cfg(not(windows))]
    Err(unsupported())
}

#[tauri::command]
pub async fn restart_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_elevated("restart").await?;
        Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
    }
    #[cfg(not(windows))]
    Err(unsupported())
}

#[tauri::command]
pub fn sync_windows_service_plan() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        let _ = crate::windows_service::sync_plan_from_running()?;
        Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
    }
    #[cfg(not(windows))]
    Err(unsupported())
}
