use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Utc;
use serde::Serialize;
use tauri::State;

use crate::app_state::AppState;
use crate::error::{AppError, AppResult};
use crate::harness::model::VerificationRecord;
use crate::harness::{Harness, TaskSession};

const MAX_RECENT_EVENTS: usize = 24;
const MAX_RECENT_OPERATIONS: usize = 24;
const MAX_RECENT_CHANGES: usize = 12;
const MAX_EFFECTIVE_VERIFICATIONS: usize = 20;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsSnapshot {
    workspace_id: String,
    task: Option<CanvsTask>,
    recent_events: Vec<CanvsEvent>,
    recent_operations: Vec<CanvsOperation>,
    changes: Vec<CanvsChange>,
    verifications: Vec<CanvsVerification>,
    refreshed_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsTask {
    id: String,
    objective: String,
    status: String,
    completed_steps: Vec<String>,
    pending_steps: Vec<String>,
    progress_percent: u8,
    branch: Option<String>,
    head: Option<String>,
    expected_head: Option<String>,
    latest_change_id: Option<String>,
    latest_verification_id: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsEvent {
    id: String,
    kind: String,
    tool_name: Option<String>,
    ok: Option<bool>,
    affected_files: usize,
    created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsOperation {
    id: String,
    tool: String,
    kind: String,
    status: String,
    ok: Option<bool>,
    affected_files: usize,
    duration_ms: Option<u64>,
    created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsChange {
    id: String,
    commit_sha: Option<String>,
    committed_files: Vec<String>,
    verification_count: usize,
    created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvsVerification {
    id: String,
    kind: String,
    command: String,
    status: String,
    level: String,
    passed: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    disposition: String,
    created_at: String,
}

#[tauri::command]
pub fn get_canvs_snapshot(state: State<'_, AppState>, id: String) -> AppResult<CanvsSnapshot> {
    let profile = state.with_workspaces(|store| {
        store
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })?;
    let root = Harness::default_root().map_err(harness_error)?;
    let harness = Harness::new(PathBuf::from(profile.path), root).map_err(harness_error)?;
    let Some(task) = harness.current_task().map_err(harness_error)? else {
        return Ok(CanvsSnapshot {
            workspace_id: harness.workspace_id().to_string(),
            task: None,
            recent_events: Vec::new(),
            recent_operations: Vec::new(),
            changes: Vec::new(),
            verifications: Vec::new(),
            refreshed_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
    };

    let mut events = harness
        .list_events(&task.id, 0, usize::MAX)
        .map_err(harness_error)?;
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

    let mut operations = harness
        .list_operations(0, usize::MAX)
        .map_err(harness_error)?;
    operations.reverse();
    let recent_operations = operations
        .into_iter()
        .filter(|operation| operation.task_id.as_deref() == Some(task.id.as_str()))
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

    let mut changes = harness.list_change_sets(&task.id).map_err(harness_error)?;
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

    let verifications = effective_verifications(
        harness
            .list_verifications(&task.id)
            .map_err(harness_error)?,
    )
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
        task: Some(task_view(task)),
        recent_events,
        recent_operations,
        changes,
        verifications,
        refreshed_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

fn task_view(task: TaskSession) -> CanvsTask {
    let completed = task.completed_steps.len();
    let pending = task.pending_steps.len();
    let total = completed + pending;
    let progress_percent = completed
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100) as u8;
    CanvsTask {
        id: task.id,
        objective: task.objective,
        status: serde_json::to_value(task.status)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".into()),
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
    }
}

fn effective_verifications(records: Vec<VerificationRecord>) -> Vec<VerificationRecord> {
    let mut latest = BTreeMap::<(String, String), VerificationRecord>::new();
    for record in records {
        latest.insert((record.kind.clone(), record.command.clone()), record);
    }
    let mut records = latest.into_values().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then(right.id.cmp(&left.id))
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

fn harness_error(error: crate::harness::HarnessError) -> AppError {
    AppError::Message(format!("{}: {error}", error.code()))
}

#[cfg(test)]
mod tests {
    use super::{effective_verifications, verification_disposition};
    use crate::harness::model::{VerificationDispositionRecord, VerificationRecord};

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
}
