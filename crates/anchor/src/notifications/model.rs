use serde::{Deserialize, Serialize};

pub const OUTBOX_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationChannel {
    Ilink,
}

impl NotificationChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ilink => "ilink",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotificationJob {
    pub schema_version: u32,
    pub id: String,
    pub channel: NotificationChannel,
    pub workspace_id: String,
    pub task_id: String,
    pub message: String,
    pub created_at_unix_ms: u64,
}

impl NotificationJob {
    pub fn new(
        channel: NotificationChannel,
        workspace_id: &str,
        task_id: &str,
        message: String,
        created_at_unix_ms: u64,
    ) -> Self {
        Self {
            schema_version: OUTBOX_SCHEMA_VERSION,
            id: format!("task-completed-{task_id}"),
            channel,
            workspace_id: workspace_id.to_string(),
            task_id: task_id.to_string(),
            message,
            created_at_unix_ms,
        }
    }
}
