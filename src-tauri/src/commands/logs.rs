use tauri::State;

use crate::app_state::AppState;
use crate::error::AppResult;
use crate::gateway_control::GatewayLogChunk;
use crate::management;

pub use crate::management::LogChunk;

#[tauri::command]
pub async fn read_workspace_logs(
    _state: State<'_, AppState>,
    id: String,
    service: String,
) -> AppResult<Vec<LogChunk>> {
    management::read_workspace_logs(&id, &service).await
}

#[tauri::command]
pub async fn read_gateway_logs(lines: u32) -> AppResult<GatewayLogChunk> {
    management::read_gateway_logs(lines).await
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
        let chunks = management::gui_log_chunks(vec![ControlLogChunk {
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
