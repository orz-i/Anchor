use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub status: String,
    pub reason: String,
    pub recoverable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HarnessSessionStatus {
    Active,
    #[default]
    Paused,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageCommitStatus {
    Started,
    ChecksPassed,
    Committed,
    CommittedRecoveryRequired,
    CommittedCheckpointPending,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageCommitReceipt {
    pub workflow_id: String,
    pub idempotency_key: String,
    pub task_id: String,
    pub status: StageCommitStatus,
    pub expected_head: String,
    pub expected_fingerprint: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub checks: Vec<Value>,
    #[serde(default)]
    pub verification_ids: Vec<String>,
    pub commit_sha: Option<String>,
    #[serde(default)]
    pub committed_files: Vec<String>,
    #[serde(default)]
    pub working_tree_files: Vec<String>,
    #[serde(default)]
    pub runtime_artifacts: Vec<String>,
    #[serde(default)]
    pub ignored_files: Vec<String>,
    #[serde(default)]
    pub baseline_refreshed: bool,
    pub checkpoint_hash: Option<String>,
    pub checkpoint_count: Option<u64>,
    pub error: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedWorkspaceState {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub worktree_fingerprint: String,
    pub accepted_at: String,
    pub accepted_by_operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessStatus {
    pub schema_version: u32,
    pub workspace_id: String,
    pub task_id: Option<String>,
    pub task_state: Option<TaskStatus>,
    pub task_updated_at: Option<String>,
    pub session_status: HarnessSessionStatus,
    pub next_stage_started: bool,
    pub writable: bool,
    pub reason: String,
    pub recoverable: bool,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub worktree_fingerprint: String,
    pub expected_branch: Option<String>,
    pub expected_head: Option<String>,
    pub expected_fingerprint: Option<String>,
    pub baseline_matches: Option<bool>,
    pub capabilities: HashMap<String, CapabilityStatus>,
    pub next_actions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Active,
    Paused,
    Verifying,
    Failed,
    Completed,
    CompletedUnverified,
    RolledBack,
}

impl TaskStatus {
    pub fn is_writable(self) -> bool {
        matches!(
            self,
            Self::Active | Self::Paused | Self::Verifying | Self::Failed
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Active, Self::Paused | Self::Verifying | Self::Failed)
                | (Self::Active, Self::CompletedUnverified)
                | (Self::Paused, Self::Active)
                | (
                    Self::Verifying,
                    Self::Completed | Self::CompletedUnverified | Self::Failed
                )
                | (Self::Failed, Self::Active | Self::RolledBack)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub path: String,
    pub exists: bool,
    pub is_binary: bool,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBaseline {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub worktree_fingerprint: String,
    pub entries: Vec<BaselineEntry>,
    pub captured_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSession {
    pub id: String,
    pub workspace_id: String,
    pub objective: String,
    pub status: TaskStatus,
    pub baseline: ProjectBaseline,
    #[serde(default)]
    pub expected_state: Option<ExpectedWorkspaceState>,
    // Kept for backward-compatible reads of schema v1 task files.
    pub expected_fingerprint: String,
    #[serde(default)]
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub pending_steps: Vec<String>,
    pub latest_change_id: Option<String>,
    pub latest_verification_id: Option<String>,
    #[serde(default)]
    pub history_session_key: Option<String>,
    #[serde(default)]
    pub history_session_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasonRecord {
    pub text: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChangeRecord {
    pub path: String,
    pub status: String,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessEvent {
    pub id: String,
    pub task_id: String,
    pub operation_id: String,
    pub kind: String,
    pub tool_name: Option<String>,
    pub input_summary: Value,
    pub result_summary: Value,
    pub reason: Option<ReasonRecord>,
    #[serde(default)]
    pub affected_files: Vec<FileChangeRecord>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: String,
    pub workspace_id: String,
    pub task_id: Option<String>,
    #[serde(default)]
    pub history_session_key: Option<String>,
    #[serde(default)]
    pub mcp_session_id: Option<String>,
    pub tool: String,
    pub kind: String,
    pub input_summary: Value,
    pub result_summary: Value,
    pub reason: Option<String>,
    #[serde(default)]
    pub affected_files: Vec<FileChangeRecord>,
    pub created_at: String,
    #[serde(default)]
    pub created_at_iso: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationRecord {
    pub id: String,
    pub task_id: String,
    pub command: String,
    #[serde(alias = "category")]
    pub kind: String,
    #[serde(default = "default_verification_status")]
    pub status: String,
    pub exit_code: Option<i32>,
    pub passed: bool,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    pub change_id: Option<String>,
    #[serde(default)]
    pub dispositions: Vec<VerificationDispositionRecord>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationDispositionRecord {
    pub id: String,
    pub disposition: String,
    pub reason: String,
    pub source: String,
    pub created_at: String,
}

fn default_verification_status() -> String {
    "unknown".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSet {
    pub id: String,
    pub task_id: String,
    pub objective: String,
    pub reason: ReasonRecord,
    #[serde(default)]
    pub files: Vec<FileChangeRecord>,
    pub commit_sha: Option<String>,
    #[serde(default)]
    pub committed_files: Vec<String>,
    #[serde(default)]
    pub working_tree_files: Vec<String>,
    #[serde(default)]
    pub runtime_artifacts: Vec<String>,
    #[serde(default)]
    pub ignored_files: Vec<String>,
    #[serde(default)]
    pub command_ids: Vec<String>,
    #[serde(default)]
    pub verification_ids: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFileState {
    pub path: String,
    pub status: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectState {
    pub schema_version: u32,
    pub workspace_id: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub clean: bool,
    pub files: Vec<ProjectFileState>,
    pub total_files: usize,
    pub truncated: bool,
    pub active_task_id: Option<String>,
    pub task: Option<TaskSession>,
    pub recent_events: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceHarnessState {
    pub schema_version: u32,
    pub active_task_id: Option<String>,
    #[serde(default)]
    pub session_status: HarnessSessionStatus,
    #[serde(default)]
    pub recent_task_ids: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessIndex {
    pub schema_version: u32,
    #[serde(default)]
    pub workspaces: HashMap<String, WorkspaceHarnessState>,
}
