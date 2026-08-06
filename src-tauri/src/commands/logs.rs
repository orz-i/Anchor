use std::path::Path;

use serde::Serialize;
use tauri::State;

use crate::app_state::AppState;
use crate::control::{self, ControlLogChunk, ControlLogSelection};
use crate::daemon;
use crate::error::{AppError, AppResult};
use crate::workspace::WorkspaceProfile;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogChunk {
    pub name: String,
    pub content: String,
}

fn profile_by_id(state: &AppState, id: &str) -> AppResult<WorkspaceProfile> {
    state.with_workspaces(|store| {
        store
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })
}

fn control_log_service(service: &str) -> AppResult<ControlLogSelection> {
    Ok(match service {
        "mcp" => ControlLogSelection::Mcp,
        "actions" => ControlLogSelection::Actions,
        other => return Err(AppError::Message(format!("unknown log service: {other}"))),
    })
}

fn gui_log_chunks(chunks: Vec<ControlLogChunk>) -> Vec<LogChunk> {
    chunks
        .into_iter()
        .filter(|chunk| chunk.exists)
        .map(|chunk| LogChunk {
            name: Path::new(&chunk.path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&chunk.name)
                .to_string(),
            content: chunk.content,
        })
        .collect()
}

#[tauri::command]
pub async fn read_workspace_logs(
    state: State<'_, AppState>,
    id: String,
    service: String,
) -> AppResult<Vec<LogChunk>> {
    let profile = profile_by_id(&state, &id)?;
    let selection = control_log_service(&service)?;
    let chunks = if daemon::inspect(&profile)?.running {
        control::request_logs(&profile, selection, 5_000, Vec::new())
            .await
            .map_err(|error| {
                AppError::Message(format!(
                    "daemon 日志请求失败：{error}；运行中的 daemon 不会回退到 GUI 直接文件读取"
                ))
            })?
    } else {
        control::read_log_batch(&profile, selection, 5_000, &[])?
    };
    Ok(gui_log_chunks(chunks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlLogChunk;
    use crate::logging::{profile_log_files, ProfileLogService};

    #[test]
    fn oauth_diagnostic_logs_are_visible_in_gui_log_lists() {
        let profile = WorkspaceProfile::new(".".into(), Some("logs".into()));
        let mcp = profile_log_files(&profile, ProfileLogService::Mcp);
        let actions = profile_log_files(&profile, ProfileLogService::Actions);
        assert!(mcp.iter().any(|file| file.1 == "mcp-oauth.log"));
        assert!(mcp.iter().any(|file| file.1 == "mcp-requests.log"));
        assert!(actions.iter().any(|file| file.1 == "actions-oauth.log"));
    }

    #[test]
    fn control_log_chunks_keep_the_existing_gui_shape() {
        let chunks = gui_log_chunks(vec![ControlLogChunk {
            name: "mcp-oauth".into(),
            path: "logs/mcp-oauth.log".into(),
            content: "entry\n".into(),
            next_offset: 6,
            exists: true,
            truncated: false,
        }]);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "mcp-oauth.log");
        assert_eq!(chunks[0].content, "entry\n");
    }
}
