use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tempfile::tempdir;

use super::model::{
    HarnessSessionStatus, WorkSessionCloseOutbox, WorkSessionClosePhase, SCHEMA_VERSION,
};
use super::tools::{call, recover_close_outboxes};
use crate::tools::{CancellationToken, ToolContext};

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

#[test]
fn begin_work_session_reports_session_reactivation_without_conflating_task_state() {
    let temp = tempdir().expect("temp");
    let workspace = temp.path().join("workspace");
    let harness_root = temp.path().join("harness");
    fs::create_dir_all(&workspace).expect("workspace");
    let ctx = ToolContext::for_test(workspace.clone(), harness_root).expect("context");
    let started = call(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "session transition diagnostics",
            "workspace_root": workspace.to_string_lossy()
        }),
        &CancellationToken::default(),
        None,
    )
    .expect("begin");
    let session_id = started["work_session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    let expected_path = started["work_session"]["session_path"]
        .as_str()
        .expect("session path")
        .to_string();
    crate::tools::session::checkpoint(
        &ctx,
        &json!({
            "session_id": session_id,
            "expected_path": expected_path,
            "turn_id": "pause-before-begin",
            "user_intent": "pause session document",
            "session_status": "paused"
        }),
        None,
    )
    .expect("pause checkpoint");

    let resumed = call(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "session transition diagnostics",
            "workspace_root": workspace.to_string_lossy(),
            "session_id": started["work_session"]["session_id"]
        }),
        &CancellationToken::default(),
        None,
    )
    .expect("resume begin");

    assert_eq!(resumed["session_state_transition"]["from"], "paused");
    assert_eq!(resumed["session_state_transition"]["to"], "active");
    assert_eq!(resumed["session_state_transition"]["changed"], true);
    assert_eq!(
        resumed["session_state_transition"]["reason"],
        "begin_work_session"
    );
    assert_eq!(resumed["state_scopes"]["session_lease"]["status"], "active");
    assert_eq!(resumed["state_scopes"]["harness_task"]["status"], "active");
    assert_eq!(resumed["session"]["previous_status"], "paused");
    assert_eq!(resumed["session"]["reactivated"], true);
}

#[test]
fn close_outbox_recovers_on_next_harness_call_after_restart() {
    let temp = tempdir().expect("temp");
    let workspace = temp.path().join("workspace");
    let harness_root = temp.path().join("harness");
    fs::create_dir_all(&workspace).expect("workspace");
    let ctx = ToolContext::for_test(workspace.clone(), harness_root.clone()).expect("context");
    let started = call(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "recover close outbox",
            "workspace_root": workspace.to_string_lossy()
        }),
        &CancellationToken::default(),
        None,
    )
    .expect("begin");
    let task_id = started["work_session"]["task_id"]
        .as_str()
        .expect("task id")
        .to_string();
    let session_id = started["work_session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    let expected_path = started["work_session"]["session_path"]
        .as_str()
        .expect("session path")
        .to_string();
    let session_index = fs::read(workspace.join("docs/session/index.json")).expect("session index");
    let session_markdown = fs::read(workspace.join(&expected_path)).expect("session markdown");
    ctx.harness
        .complete_task(&task_id, false, HarnessSessionStatus::Paused)
        .expect("complete task");
    fs::remove_dir_all(workspace.join("docs/session")).expect("remove session store");

    let now = timestamp();
    ctx.harness
        .save_close_outbox(&WorkSessionCloseOutbox {
            schema_version: SCHEMA_VERSION,
            task_id: task_id.clone(),
            session_id: session_id.clone(),
            session_path: expected_path.clone(),
            session_status: HarnessSessionStatus::Paused,
            finish_args: json!({
                "task_id": task_id,
                "allow_unverified": true,
                "session_status": "paused"
            }),
            checkpoint_args: json!({
                "session_id": session_id,
                "expected_path": expected_path,
                "turn_id": "close-outbox-recovery-test",
                "user_intent": "recover close outbox",
                "session_status": "paused"
            }),
            phase: WorkSessionClosePhase::TaskClosed,
            attempts: 0,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        })
        .expect("save outbox");

    let first = recover_close_outboxes(&ctx).expect("first recovery");
    assert_eq!(first.len(), 1);
    let pending = ctx
        .harness
        .load_close_outbox(&task_id)
        .expect("load")
        .expect("outbox");
    assert_eq!(pending.phase, WorkSessionClosePhase::CheckpointPending);
    assert!(pending.last_error.is_some());

    let session_dir = workspace.join("docs/session");
    fs::create_dir_all(&session_dir).expect("restore session dir");
    fs::write(session_dir.join("index.json"), session_index).expect("restore session index");
    fs::write(workspace.join(&expected_path), session_markdown).expect("restore session markdown");
    drop(ctx);
    let restarted = ToolContext::for_test(workspace, harness_root).expect("restarted context");
    let still_pending = restarted
        .harness
        .load_close_outbox(&task_id)
        .expect("load")
        .expect("outbox");
    assert_eq!(
        still_pending.phase,
        WorkSessionClosePhase::CheckpointPending,
        "ToolContext construction must not block listener startup on outbox recovery"
    );

    let status = call(
        &restarted,
        "harness_status",
        &json!({}),
        &CancellationToken::default(),
        None,
    )
    .expect("harness status triggers recovery");
    assert!(
        status.get("outbox_recovery").is_none(),
        "task-scoped status must not surface peer/completed outbox diagnostics: {status}"
    );
    let completed = restarted
        .harness
        .load_close_outbox(&task_id)
        .expect("load")
        .expect("outbox");
    assert_eq!(completed.phase, WorkSessionClosePhase::Completed);
    assert!(completed.last_error.is_none());
}
