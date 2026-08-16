use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::harness::model::VerificationRecord;
use crate::harness::{Harness, HarnessError, HarnessResult, TaskSession};

const MAX_RECENT_EVENTS: usize = 24;
const MAX_RECENT_OPERATIONS: usize = 24;
const MAX_RECENT_CHANGES: usize = 12;
const MAX_EFFECTIVE_VERIFICATIONS: usize = 20;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsTaskList {
    pub workspace_id: String,
    pub tasks: Vec<CanvsTask>,
    pub refreshed_at: String,
}

fn safe_task_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsSnapshot {
    pub workspace_id: String,
    pub task: Option<CanvsTask>,
    pub recent_events: Vec<CanvsEvent>,
    pub recent_operations: Vec<CanvsOperation>,
    pub changes: Vec<CanvsChange>,
    pub verifications: Vec<CanvsVerification>,
    pub refreshed_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsTask {
    pub id: String,
    pub objective: String,
    pub status: String,
    pub workspace_mode: String,
    pub current: bool,
    pub active: bool,
    pub completed_steps: Vec<String>,
    pub pending_steps: Vec<String>,
    pub progress_percent: u8,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub expected_head: Option<String>,
    pub latest_change_id: Option<String>,
    pub latest_verification_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsEvent {
    pub id: String,
    pub kind: String,
    pub tool_name: Option<String>,
    pub ok: Option<bool>,
    pub affected_files: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsOperation {
    pub id: String,
    pub tool: String,
    pub kind: String,
    pub status: String,
    pub ok: Option<bool>,
    pub affected_files: usize,
    pub duration_ms: Option<u64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsChange {
    pub id: String,
    pub commit_sha: Option<String>,
    pub committed_files: Vec<String>,
    pub verification_count: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsVerification {
    pub id: String,
    pub kind: String,
    pub command: String,
    pub status: String,
    pub level: String,
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub disposition: String,
    pub created_at: String,
}

pub fn list_workspace_tasks(workspace_path: &Path) -> HarnessResult<CanvsTaskList> {
    let harness = workspace_harness(workspace_path)?;
    task_list(&harness)
}

fn task_list(harness: &Harness) -> HarnessResult<CanvsTaskList> {
    let current_task_id = harness.current_task()?.map(|task| task.id);
    let mut tasks = harness.list_tasks()?;
    tasks.sort_by(|left, right| {
        timestamp_sort_key(task_recency(right))
            .cmp(&timestamp_sort_key(task_recency(left)))
            .then_with(|| task_recency(right).cmp(task_recency(left)))
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok(CanvsTaskList {
        workspace_id: harness.workspace_id().to_string(),
        tasks: tasks
            .into_iter()
            .map(|task| {
                let current = current_task_id.as_deref() == Some(task.id.as_str());
                task_view(task, current)
            })
            .collect(),
        refreshed_at: now(),
    })
}

#[cfg(feature = "desktop")]
pub fn current_workspace_snapshot(workspace_path: &Path) -> HarnessResult<CanvsSnapshot> {
    let harness = workspace_harness(workspace_path)?;
    let Some(task) = harness.current_task()? else {
        return Ok(empty_snapshot(&harness));
    };
    task_snapshot(&harness, task, true)
}

pub fn workspace_task_snapshot(
    workspace_path: &Path,
    task_id: &str,
) -> HarnessResult<CanvsSnapshot> {
    if !safe_task_id(task_id) {
        return Err(HarnessError::new(
            "INVALID_TASK_ID",
            "task id must contain only ASCII letters, numbers, hyphens, or underscores",
        ));
    }
    let harness = workspace_harness(workspace_path)?;
    let current_task_id = harness.current_task()?.map(|task| task.id);
    let task = harness.task(task_id)?;
    let current = current_task_id.as_deref() == Some(task.id.as_str());
    task_snapshot(&harness, task, current)
}

fn workspace_harness(workspace_path: &Path) -> HarnessResult<Harness> {
    Harness::new(PathBuf::from(workspace_path), Harness::default_root()?)
}

#[cfg(feature = "desktop")]
fn empty_snapshot(harness: &Harness) -> CanvsSnapshot {
    CanvsSnapshot {
        workspace_id: harness.workspace_id().to_string(),
        task: None,
        recent_events: Vec::new(),
        recent_operations: Vec::new(),
        changes: Vec::new(),
        verifications: Vec::new(),
        refreshed_at: now(),
    }
}

fn task_snapshot(
    harness: &Harness,
    task: TaskSession,
    current: bool,
) -> HarnessResult<CanvsSnapshot> {
    let task_id = task.id.clone();

    let mut events = harness.list_events(&task_id, 0, usize::MAX)?;
    events.reverse();
    let recent_events = events
        .into_iter()
        .take(MAX_RECENT_EVENTS)
        .map(|event| CanvsEvent {
            id: event.id,
            kind: event.kind,
            tool_name: event.tool_name,
            ok: event
                .result_summary
                .get("ok")
                .and_then(serde_json::Value::as_bool),
            affected_files: event.affected_files.len(),
            created_at: event.created_at,
        })
        .collect();

    let mut operations = harness.list_operations(0, usize::MAX)?;
    operations.reverse();
    let recent_operations = operations
        .into_iter()
        .filter(|operation| operation.task_id.as_deref() == Some(task_id.as_str()))
        .take(MAX_RECENT_OPERATIONS)
        .map(|operation| {
            let ok = operation
                .result_summary
                .get("ok")
                .and_then(serde_json::Value::as_bool);
            let status = operation
                .result_summary
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| match ok {
                    Some(true) => "completed".into(),
                    Some(false) => "failed".into(),
                    None => operation.kind.clone(),
                });
            let duration_ms = operation
                .result_summary
                .get("duration_ms")
                .or_else(|| operation.result_summary.get("elapsed_ms"))
                .and_then(serde_json::Value::as_u64);
            CanvsOperation {
                id: operation.id,
                tool: operation.tool,
                kind: operation.kind,
                status,
                ok,
                affected_files: operation.affected_files.len(),
                duration_ms,
                created_at: if operation.created_at_iso.is_empty() {
                    operation.created_at
                } else {
                    operation.created_at_iso
                },
            }
        })
        .collect();

    let mut changes = harness.list_change_sets(&task_id)?;
    changes.reverse();
    let changes = changes
        .into_iter()
        .take(MAX_RECENT_CHANGES)
        .map(|change| CanvsChange {
            id: change.id,
            commit_sha: change.commit_sha,
            committed_files: change.committed_files,
            verification_count: change.verification_ids.len(),
            created_at: change.created_at,
        })
        .collect();

    let verifications = effective_verifications(harness.list_verifications(&task_id)?)
        .into_iter()
        .take(MAX_EFFECTIVE_VERIFICATIONS)
        .map(|verification| {
            let disposition = verification_disposition(&verification);
            CanvsVerification {
                id: verification.id,
                kind: verification.kind,
                command: verification.command,
                status: verification.status,
                level: verification.level,
                passed: verification.passed,
                exit_code: verification.exit_code,
                duration_ms: verification.duration_ms,
                disposition,
                created_at: verification.created_at,
            }
        })
        .collect();

    Ok(CanvsSnapshot {
        workspace_id: harness.workspace_id().to_string(),
        task: Some(task_view(task, current)),
        recent_events,
        recent_operations,
        changes,
        verifications,
        refreshed_at: now(),
    })
}

fn task_view(task: TaskSession, current: bool) -> CanvsTask {
    let completed = task.completed_steps.len();
    let pending = task.pending_steps.len();
    let total = completed + pending;
    let progress_percent = completed
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100) as u8;
    let active = matches!(
        task.status,
        crate::harness::TaskStatus::Active | crate::harness::TaskStatus::Verifying
    );
    let workspace_mode = if task.git_worktree.is_some() {
        "worktree"
    } else {
        "shared"
    };
    CanvsTask {
        id: task.id,
        objective: task.objective,
        status: task_status(task.status),
        workspace_mode: workspace_mode.into(),
        current,
        active,
        completed_steps: task.completed_steps,
        pending_steps: task.pending_steps,
        progress_percent,
        branch: task.expected_state.branch,
        head: task.baseline.head,
        expected_head: task.expected_state.head,
        latest_change_id: task.latest_change_id,
        latest_verification_id: task.latest_verification_id,
        created_at: task.created_at,
        updated_at: task.updated_at,
        last_activity_at: task.last_activity_at,
    }
}

fn task_recency(task: &TaskSession) -> &str {
    match task.last_activity_at.as_deref() {
        Some(last_activity_at)
            if timestamp_sort_key(last_activity_at) >= timestamp_sort_key(&task.updated_at) =>
        {
            last_activity_at
        }
        _ => task.updated_at.as_str(),
    }
}

fn task_status(status: crate::harness::TaskStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

fn effective_verifications(records: Vec<VerificationRecord>) -> Vec<VerificationRecord> {
    let mut latest = BTreeMap::<(String, String), VerificationRecord>::new();
    for record in records {
        latest.insert((record.kind.clone(), record.command.clone()), record);
    }
    let mut records = latest.into_values().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        timestamp_sort_key(&right.created_at)
            .cmp(&timestamp_sort_key(&left.created_at))
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| right.id.cmp(&left.id))
    });
    records
}

fn verification_disposition(record: &VerificationRecord) -> String {
    record
        .dispositions
        .last()
        .map(|entry| entry.disposition.clone())
        .unwrap_or_else(|| {
            if record.passed {
                "passed".into()
            } else {
                record.status.clone()
            }
        })
}

fn timestamp_sort_key(raw: &str) -> i128 {
    let raw = raw.trim();
    if let Some(value) = raw.strip_prefix("unix:") {
        return value.parse::<i128>().unwrap_or_default() * 1_000;
    }
    if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        let parsed = raw.parse::<i128>().unwrap_or_default();
        return if raw.len() <= 10 {
            parsed * 1_000
        } else {
            parsed
        };
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.timestamp_millis() as i128)
        .unwrap_or_default()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn harness_error_message(error: HarnessError) -> String {
    format!("{}: {error}", error.code())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        effective_verifications, safe_task_id, task_list, task_status, timestamp_sort_key,
        verification_disposition,
    };
    use crate::harness::model::{TaskStatus, VerificationDispositionRecord, VerificationRecord};

    fn verification(id: &str, created_at: &str, passed: bool) -> VerificationRecord {
        VerificationRecord {
            id: id.into(),
            task_id: "task".into(),
            command: "cargo test".into(),
            kind: "test".into(),
            verification_key: None,
            test_file: None,
            test_name: None,
            status: if passed { "passed" } else { "failed" }.into(),
            level: "blocking".into(),
            exit_code: Some(if passed { 0 } else { 1 }),
            passed,
            duration_ms: Some(10),
            change_id: None,
            dispositions: Vec::new(),
            supersedes: Vec::new(),
            created_at: created_at.into(),
        }
    }

    #[test]
    fn effective_verifications_keep_the_latest_command_result() {
        let records = effective_verifications(vec![
            verification("old", "2026-08-01T00:00:00Z", false),
            verification("new", "2026-08-01T00:00:01Z", true),
        ]);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "new");
    }

    #[test]
    fn explicit_verification_disposition_takes_precedence() {
        let mut record = verification("failed", "2026-08-01T00:00:00Z", false);
        record.dispositions.push(VerificationDispositionRecord {
            id: "disposition".into(),
            disposition: "diagnostic_only".into(),
            reason: "environment probe".into(),
            source: "test".into(),
            created_at: "2026-08-01T00:00:01Z".into(),
        });

        assert_eq!(verification_disposition(&record), "diagnostic_only");
    }

    #[test]
    fn timestamps_sort_across_supported_storage_formats() {
        assert_eq!(timestamp_sort_key("unix:10"), 10_000);
        assert_eq!(timestamp_sort_key("10000"), 10_000_000);
        assert!(timestamp_sort_key("2026-08-01T00:00:01Z") > 0);
    }

    #[test]
    fn task_status_uses_public_snake_case_values() {
        assert_eq!(
            task_status(TaskStatus::CompletedUnverified),
            "completed_unverified"
        );
        assert_eq!(task_status(TaskStatus::Incomplete), "incomplete");
    }

    #[test]
    fn task_ids_reject_path_traversal() {
        assert!(safe_task_id("task-123_abc"));
        assert!(!safe_task_id("../task"));
        assert!(!safe_task_id("task/child"));
    }

    #[test]
    fn task_list_includes_current_and_history_with_newest_first() {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = tempfile::tempdir().expect("harness root");
        let harness =
            crate::harness::Harness::new(workspace.path().to_path_buf(), root.path().to_path_buf())
                .expect("harness");

        let history = harness.start_task("history task").expect("history task");
        harness
            .transition(&history.id, crate::harness::TaskStatus::CompletedUnverified)
            .expect("complete history task");
        std::thread::sleep(Duration::from_millis(2));
        let parallel = harness.start_task("parallel task").expect("parallel task");
        std::thread::sleep(Duration::from_millis(2));
        let current = harness.start_task("current task").expect("current task");
        std::thread::sleep(Duration::from_millis(2));
        harness
            .resume_task_for_activity(&parallel.id, "read_file", None)
            .expect("parallel activity");

        let list = task_list(&harness).expect("task list");
        assert_eq!(list.tasks.len(), 3);
        assert_eq!(list.tasks[0].id, parallel.id);
        assert!(!list.tasks[0].current);
        assert!(list.tasks[0].active);
        assert!(list.tasks[0].last_activity_at.is_some());
        assert_eq!(list.tasks[1].id, current.id);
        assert!(list.tasks[1].current);
        assert!(list.tasks[1].active);
        assert_eq!(list.tasks[2].id, history.id);
        assert!(!list.tasks[2].current);
        assert!(!list.tasks[2].active);
    }
}
