mod common;

use std::fs;

use anchor_lib::tools::{call_tool_for_session, session, ToolContext};
use serde_json::{json, Value};

use common::{assert_err, assert_ok, invoke};

#[cfg(windows)]
const TEST_PYTHON: &str = "python";
#[cfg(not(windows))]
const TEST_PYTHON: &str = "python3";

fn test_context() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let harness = tempfile::tempdir().expect("harness tempdir");
    let ctx = ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
        .expect("tool context");
    (workspace, harness, ctx)
}

fn open_session(ctx: &ToolContext, host_session_key: &str, title: &str) -> Value {
    let result = session::open(
        ctx,
        &json!({
            "_host_session_key": host_session_key,
            "title": title
        }),
    )
    .expect("session open");
    assert_ok(&result).clone()
}

#[test]
fn open_creates_opaque_isolated_session_without_loading_legacy_history() {
    let (workspace, _harness, ctx) = test_context();
    let legacy = workspace.path().join("docs/history-session");
    fs::create_dir_all(&legacy).expect("legacy dir");
    fs::write(
        legacy.join("999.md"),
        "# malformed legacy file\nTHIS_LEGACY_SECRET_MUST_NOT_BE_INJECTED\n",
    )
    .expect("legacy file");

    let opened = open_session(&ctx, "chat-a", "isolated-a");
    let session_id = opened["session_id"].as_str().expect("session id");
    let session_path = opened["session_path"].as_str().expect("session path");

    assert!(session_id.starts_with("ses_"));
    assert_eq!(session_id.len(), 36);
    assert!(session_path.starts_with("docs/session/ses_"));
    assert_eq!(opened["automatic_history_loading"], false);
    assert_eq!(opened["history_injected"], false);
    assert_eq!(
        opened["archive_access"]["legacy_path"],
        "docs/history-session"
    );
    assert_eq!(
        opened["archive_access"]["legacy_migration_performed"],
        false
    );
    for forbidden in [
        "all_history_summary",
        "inherited_summary",
        "session_summaries",
        "latest_handoff",
        "resume_state",
        "history_numbers",
    ] {
        assert!(
            opened.get(forbidden).is_none(),
            "unexpected {forbidden}: {opened}"
        );
    }

    let content = fs::read_to_string(workspace.path().join(session_path)).expect("session file");
    assert!(content.contains(&format!("**Session id:** {session_id}")));
    assert!(content.contains("**Host session key:** chat-a"));
    assert!(!content.contains("THIS_LEGACY_SECRET_MUST_NOT_BE_INJECTED"));
    assert!(!content.contains("继承的历史摘要"));
    assert!(legacy.join("999.md").exists());
}

#[test]
fn same_host_conversation_reopens_the_same_session_without_duplicates() {
    let (workspace, _harness, ctx) = test_context();
    let first = open_session(&ctx, "chat-stable", "first-title");
    let second = open_session(&ctx, "chat-stable", "ignored-title");

    assert_eq!(first["session_id"], second["session_id"]);
    assert_eq!(first["session_path"], second["session_path"]);
    assert_eq!(first["created"], true);
    assert_eq!(second["created"], false);
    assert_eq!(second["resumed"], true);

    let markdown_count = fs::read_dir(workspace.path().join("docs/session"))
        .expect("session dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("md"))
        .count();
    assert_eq!(markdown_count, 1);
}

#[test]
fn distinct_sessions_remain_active_and_never_inherit_or_pause_each_other() {
    let (workspace, _harness, ctx) = test_context();
    let first = open_session(&ctx, "chat-a", "alpha-private-marker");
    let second = open_session(&ctx, "chat-b", "beta-private-marker");
    assert_ne!(first["session_id"], second["session_id"]);

    let first_content = fs::read_to_string(
        workspace
            .path()
            .join(first["session_path"].as_str().expect("first path")),
    )
    .expect("first session");
    let second_content = fs::read_to_string(
        workspace
            .path()
            .join(second["session_path"].as_str().expect("second path")),
    )
    .expect("second session");
    assert!(first_content.contains("**Status:** active"));
    assert!(second_content.contains("**Status:** active"));
    assert!(first_content.contains("alpha-private-marker"));
    assert!(!first_content.contains("beta-private-marker"));
    assert!(second_content.contains("beta-private-marker"));
    assert!(!second_content.contains("alpha-private-marker"));
}

#[test]
fn list_reads_metadata_only_and_get_reads_one_explicit_session() {
    let (_workspace, _harness, ctx) = test_context();
    let first = open_session(&ctx, "chat-a", "alpha-title");
    let second = open_session(&ctx, "chat-b", "beta-title");
    let first_id = first["session_id"].as_str().expect("first id");
    let first_path = first["session_path"].as_str().expect("first path");

    let checkpoint = invoke(
        &ctx,
        "session_checkpoint",
        json!({
            "session_id": first_id,
            "expected_path": first_path,
            "turn_id": "alpha-checkpoint",
            "findings": ["alpha-body-only-marker"]
        }),
    );
    assert_ok(&checkpoint);

    let listed = invoke(&ctx, "session_list", json!({"limit": 1}));
    let listed = assert_ok(&listed);
    assert_eq!(listed["sessions"].as_array().expect("sessions").len(), 1);
    assert_eq!(listed["total"], 2);
    assert!(listed["next_cursor"].is_number());
    let serialized = listed.to_string();
    assert!(!serialized.contains("alpha-body-only-marker"));
    assert!(!serialized.contains("content\""));
    assert!(!serialized.contains("summary\""));

    let fetched = invoke(
        &ctx,
        "session_get",
        json!({"session_id": first_id, "max_bytes": 131072}),
    );
    let fetched = assert_ok(&fetched);
    assert_eq!(fetched["session_id"], first["session_id"]);
    assert!(fetched["content"]
        .as_str()
        .expect("content")
        .contains("alpha-body-only-marker"));
    assert!(!fetched["content"]
        .as_str()
        .expect("content")
        .contains("beta-title"));
    assert_eq!(second["created"], true);
}

#[test]
fn open_response_size_is_constant_as_session_count_grows() {
    let (_workspace, _harness, ctx) = test_context();
    let first = open_session(&ctx, "chat-000", "constant-title");
    let first_size = serde_json::to_vec(&first).expect("serialize first").len();
    for index in 1..80 {
        let _ = open_session(&ctx, &format!("chat-{index:03}"), "constant-title");
    }
    let last = open_session(&ctx, "chat-final", "constant-title");
    let last_size = serde_json::to_vec(&last).expect("serialize last").len();
    assert!(
        last_size.abs_diff(first_size) < 128,
        "open response grew with archive: first={first_size}, last={last_size}"
    );
    assert!(last.get("session_summaries").is_none());
}

#[test]
fn legacy_archive_cannot_be_selected_as_the_new_writable_session_store() {
    let (_workspace, _harness, ctx) = test_context();
    let result = session::open(
        &ctx,
        &json!({
            "_host_session_key": "legacy-write-attempt",
            "session_dir": "docs/history-session"
        }),
    )
    .expect_err("legacy store must be rejected")
    .to_error_value();
    assert_eq!(result["code"], "LEGACY_SESSION_ARCHIVE_READ_ONLY");
    assert_eq!(result["details"]["migration_performed"], false);
}

#[test]
fn validate_never_scans_or_migrates_the_legacy_archive() {
    let (workspace, _harness, ctx) = test_context();
    let legacy = workspace.path().join("docs/history-session");
    fs::create_dir_all(&legacy).expect("legacy dir");
    fs::write(legacy.join("not-a-session.txt"), vec![0xff, 0xfe, 0xfd]).expect("bad legacy");
    let _ = open_session(&ctx, "chat-a", "valid-new-session");

    let validated = invoke(&ctx, "session_validate", json!({"repair": false}));
    let validated = assert_ok(&validated);
    assert_eq!(validated["valid"], true);
    assert_eq!(validated["document_count"], 1);
    assert_eq!(validated["legacy_path"], "docs/history-session");
    assert_eq!(validated["legacy_scanned"], false);
    assert_eq!(validated["legacy_migration_performed"], false);
    assert!(legacy.join("not-a-session.txt").exists());
}

#[test]
fn checkpoint_is_idempotent_and_cannot_cross_session_targets() {
    let (_workspace, _harness, ctx) = test_context();
    let first = open_session(&ctx, "chat-a", "alpha");
    let second = open_session(&ctx, "chat-b", "beta");
    let args = json!({
        "session_id": first["session_id"],
        "expected_path": first["session_path"],
        "turn_id": "stable-turn",
        "findings": ["stable finding"]
    });
    let saved = invoke(&ctx, "session_checkpoint", args.clone());
    let saved = assert_ok(&saved);
    assert_eq!(saved["checkpoint_count"], 1);
    let duplicate = invoke(&ctx, "session_checkpoint", args);
    let duplicate = assert_ok(&duplicate);
    assert_eq!(duplicate["duplicate_ignored"], true);
    assert_eq!(duplicate["checkpoint_count"], 1);

    let crossed = invoke(
        &ctx,
        "session_checkpoint",
        json!({
            "session_id": first["session_id"],
            "expected_path": second["session_path"],
            "turn_id": "crossed"
        }),
    );
    let crossed = assert_err(&crossed);
    assert_eq!(crossed["error"]["code"], "SESSION_TARGET_MISMATCH");
}

#[test]
fn automatic_milestone_checkpoint_uses_explicit_session_id_binding() {
    let (_workspace, _harness, ctx) = test_context();
    let opened = open_session(&ctx, "chat-auto-checkpoint", "automatic checkpoint");
    let session_id = opened["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    let session_path = opened["session_path"]
        .as_str()
        .expect("session path")
        .to_string();
    let task = ctx
        .harness
        .start_task("automatic checkpoint binding")
        .expect("task");
    ctx.harness
        .bind_session(&task.id, &session_id, &session_path)
        .expect("bind session");

    let checkpoint = session::auto_checkpoint_after_tool(
        &ctx,
        "git_commit",
        &json!({}),
        &json!({
            "ok": true,
            "commit_sha": "0123456789abcdef0123456789abcdef01234567",
            "affected_files": []
        }),
        Some(&task.id),
    )
    .expect("automatic checkpoint")
    .expect("checkpoint created");

    assert_eq!(checkpoint["session_id"], session_id);
    assert_eq!(checkpoint["path"], session_path);
    assert_eq!(checkpoint["checkpoint_count"], 1);
    let fetched_result = invoke(
        &ctx,
        "session_get",
        json!({"session_id": checkpoint["session_id"]}),
    );
    let fetched = assert_ok(&fetched_result);
    assert!(fetched["content"]
        .as_str()
        .expect("content")
        .contains("自动阶段检查点"));
}

#[test]
fn checkpoint_rejects_running_or_unconsumed_command_results_for_the_same_caller() {
    let (_workspace, _harness, ctx) = test_context();
    let caller = "chat-command-owner";
    let opened = open_session(&ctx, caller, "command-owner");
    let started = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-u", "-c", "import time; print('pending', flush=True); time.sleep(0.4)"],
            "yield_time_ms": 0,
            "timeout_ms": 5_000
        }),
        caller,
    );
    let command_session_id = assert_ok(&started)["session_id"]
        .as_str()
        .expect("command session")
        .to_string();
    let checkpoint_args = json!({
        "session_id": opened["session_id"],
        "expected_path": opened["session_path"],
        "turn_id": "pending-command"
    });
    let running = call_tool_for_session(&ctx, "session_checkpoint", &checkpoint_args, caller);
    let running = assert_err(&running);
    assert_eq!(running["error"]["code"], "SESSION_COMMAND_RESULTS_PENDING");

    std::thread::sleep(std::time::Duration::from_millis(650));
    let unconsumed = call_tool_for_session(&ctx, "session_checkpoint", &checkpoint_args, caller);
    assert_eq!(
        assert_err(&unconsumed)["error"]["code"],
        "SESSION_COMMAND_RESULTS_PENDING"
    );

    let waited = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session_id, "timeout_ms": 0}),
        caller,
    );
    assert_eq!(assert_ok(&waited)["result_observed"], true);
    let saved = call_tool_for_session(&ctx, "session_checkpoint", &checkpoint_args, caller);
    assert_eq!(assert_ok(&saved)["checkpoint_count"], 1);
}

#[test]
fn get_is_bounded_on_utf8_boundaries() {
    let (_workspace, _harness, ctx) = test_context();
    let opened = open_session(&ctx, "chat-utf8", "中文会话");
    let fetched = invoke(
        &ctx,
        "session_get",
        json!({"session_id": opened["session_id"], "max_bytes": 17}),
    );
    let fetched = assert_ok(&fetched);
    assert_eq!(fetched["content_truncated"], true);
    assert!(fetched["content"].as_str().is_some());
}
