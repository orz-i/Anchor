use tauri::State;

use crate::app_state::AppState;
use crate::error::AppResult;
use crate::health::HealthItem;
use crate::management;

#[tauri::command]
pub async fn run_health_checks(
    _state: State<'_, AppState>,
    id: String,
) -> AppResult<Vec<HealthItem>> {
    management::run_health_checks(&id).await
}
