use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;
use tauri::State;

use crate::app_state::AppState;
use crate::error::{AppError, AppResult};
use crate::logging::{profile_log_files, ProfileLogService};
use crate::tunnel::log_dir_for_profile;
use crate::workspace::WorkspaceProfile;

const MAX_LOG_BYTES: usize = 8192;
const MAX_LOG_CHARS: usize = 4000;

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

fn profile_log_service(service: &str) -> AppResult<ProfileLogService> {
    Ok(match service {
        "mcp" => ProfileLogService::Mcp,
        "actions" => ProfileLogService::Actions,
        other => return Err(AppError::Message(format!("unknown log service: {other}"))),
    })
}

fn read_log_tail(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)?;
    let size = file.seek(SeekFrom::End(0))?;
    let start = size.saturating_sub(MAX_LOG_BYTES as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok(if text.chars().count() > MAX_LOG_CHARS {
        text.chars()
            .rev()
            .take(MAX_LOG_CHARS)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    } else {
        text
    })
}

#[tauri::command]
pub async fn read_workspace_logs(
    state: State<'_, AppState>,
    id: String,
    service: String,
) -> AppResult<Vec<LogChunk>> {
    let profile = profile_by_id(&state, &id)?;
    let log_dir = log_dir_for_profile(&profile.id);
    let files = profile_log_files(&profile, profile_log_service(&service)?);

    let mut chunks = Vec::new();
    for (_, name) in files {
        let path = log_dir.join(name);
        if !path.exists() {
            continue;
        }
        let content = read_log_tail(&path)?;
        chunks.push(LogChunk {
            name: name.to_string(),
            content,
        });
    }

    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_diagnostic_logs_are_visible_in_gui_log_lists() {
        let profile = WorkspaceProfile::new(".".into(), Some("logs".into()));
        let mcp = profile_log_files(&profile, ProfileLogService::Mcp);
        let actions = profile_log_files(&profile, ProfileLogService::Actions);
        assert!(mcp.iter().any(|file| file.1 == "mcp-oauth.log"));
        assert!(mcp.iter().any(|file| file.1 == "mcp-requests.log"));
        assert!(actions.iter().any(|file| file.1 == "actions-oauth.log"));
    }
}
