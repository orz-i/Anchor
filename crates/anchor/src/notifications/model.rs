use serde::{Deserialize, Serialize};

pub const OUTBOX_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotificationJob {
    pub schema_version: u32,
    pub id: String,
    pub workspace_id: String,
    pub profile_id: String,
    pub task_id: String,
    pub message: String,
    pub created_at_unix_ms: u64,
}

impl NotificationJob {
    pub fn new(
        workspace_id: &str,
        profile_id: &str,
        task_id: &str,
        message: String,
        created_at_unix_ms: u64,
    ) -> Self {
        Self {
            schema_version: OUTBOX_SCHEMA_VERSION,
            id: format!("task-completed-{task_id}"),
            workspace_id: workspace_id.to_string(),
            profile_id: profile_id.to_string(),
            task_id: task_id.to_string(),
            message,
            created_at_unix_ms,
        }
    }
}
