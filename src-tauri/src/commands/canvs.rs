use tauri::State;

use crate::app_state::AppState;
use crate::canvs::{CanvsSnapshot, CanvsTaskList};
use crate::error::AppResult;
use crate::management;

#[tauri::command]
pub fn get_canvs_snapshot(_state: State<'_, AppState>, id: String) -> AppResult<CanvsSnapshot> {
    management::get_canvs_snapshot(&id)
}

#[tauri::command]
pub fn list_canvs_tasks(_state: State<'_, AppState>, id: String) -> AppResult<CanvsTaskList> {
    management::list_canvs_tasks(&id)
}

#[tauri::command]
pub fn get_canvs_task_snapshot(
    _state: State<'_, AppState>,
    id: String,
    task_id: String,
) -> AppResult<CanvsSnapshot> {
    management::get_canvs_task_snapshot(&id, &task_id)
}
