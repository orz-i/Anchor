use std::path::PathBuf;

use tauri::State;

use crate::app_state::AppState;
use crate::canvs::{
    current_workspace_snapshot, harness_error_message, list_workspace_tasks,
    workspace_task_snapshot, CanvsSnapshot, CanvsTaskList,
};
use crate::error::{AppError, AppResult};

fn workspace_path(state: &AppState, id: &str) -> AppResult<PathBuf> {
    state.with_workspaces(|store| {
        store
            .get(id)
            .map(|profile| PathBuf::from(&profile.path))
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })
}

#[tauri::command]
pub fn get_canvs_snapshot(state: State<'_, AppState>, id: String) -> AppResult<CanvsSnapshot> {
    current_workspace_snapshot(&workspace_path(&state, &id)?)
        .map_err(|error| AppError::Message(harness_error_message(error)))
}

#[tauri::command]
pub fn list_canvs_tasks(state: State<'_, AppState>, id: String) -> AppResult<CanvsTaskList> {
    list_workspace_tasks(&workspace_path(&state, &id)?)
        .map_err(|error| AppError::Message(harness_error_message(error)))
}

#[tauri::command]
pub fn get_canvs_task_snapshot(
    state: State<'_, AppState>,
    id: String,
    task_id: String,
) -> AppResult<CanvsSnapshot> {
    workspace_task_snapshot(&workspace_path(&state, &id)?, &task_id)
        .map_err(|error| AppError::Message(harness_error_message(error)))
}
