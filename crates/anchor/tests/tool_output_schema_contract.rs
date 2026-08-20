mod common;

use std::fs;
use std::process::Command;

use anchor_lib::tools::registry::output_schema;
use common::{assert_err, assert_ok, ctx_for, invoke, tiny_js_fixture};
use serde_json::{json, Value};

#[cfg(windows)]
const TEST_PYTHON: &str = "python";
#[cfg(not(windows))]
const TEST_PYTHON: &str = "python3";

fn assert_matches_output_schema(tool: &str, value: &Value) {
    let schema = output_schema(tool);
    let validator = jsonschema::validator_for(&schema).expect("compile output schema");
    validator
        .validate(value)
        .unwrap_or_else(|error| panic!("{tool} output schema violation: {error}\n{value}"));
}

#[test]
fn worktree_management_successes_match_published_output_schemas() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::write(workspace.join("README.md"), "schema worktree\n").expect("readme");
    fs::write(workspace.join(".gitignore"), "/.anchor/worktrees/\n").expect("ignore");
    for args in [
        ["init"].as_slice(),
        ["config", "user.email", "anchor@example.invalid"].as_slice(),
        ["config", "user.name", "Anchor Tests"].as_slice(),
        ["add", "."].as_slice(),
        ["commit", "--no-gpg-sign", "--no-verify", "-m", "initial"].as_slice(),
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(args)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let ctx = ctx_for(&workspace);

    let created = invoke(
        &ctx,
        "git_worktree_create",
        json!({"name": "schema_test", "base_ref": "HEAD"}),
    );
    assert_ok(&created);
    assert_matches_output_schema("git_worktree_create", &created);

    let listed = invoke(&ctx, "git_worktree_list", json!({}));
    assert_ok(&listed);
    assert_matches_output_schema("git_worktree_list", &listed);

    let removed = invoke(
        &ctx,
        "git_worktree_remove",
        json!({"path": ".anchor/worktrees/schema_test"}),
    );
    assert_ok(&removed);
    assert_matches_output_schema("git_worktree_remove", &removed);

    let pruned = invoke(&ctx, "git_worktree_prune", json!({}));
    assert_ok(&pruned);
    assert_matches_output_schema("git_worktree_prune", &pruned);
}

#[test]
fn high_value_local_tool_successes_match_published_output_schemas() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let cases = [
        ("server_info", json!({})),
        ("read_file", json!({"path": "src/math.js"})),
        (
            "search_text",
            json!({"query": "add", "path": "src", "max_results": 20}),
        ),
        (
            "patch_check",
            json!({
                "patch": "*** Begin Patch\n*** Add File: schema-probe.txt\n+probe\n*** End Patch\n"
            }),
        ),
        (
            "command_cost_explain",
            json!({"cmd": "cargo test", "cost_intent": "local_only"}),
        ),
        (
            "exec_command",
            json!({"cmd": "echo schema-contract", "yield_time_ms": 10_000}),
        ),
        ("git_status", json!({})),
    ];

    for (tool, args) in cases {
        let output = invoke(&ctx, tool, args);
        assert_ok(&output);
        assert_matches_output_schema(tool, &output);
    }

    let applied = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "*** Begin Patch\n*** Add File: schema-probe.txt\n+probe\n*** End Patch\n"
        }),
    );
    assert_ok(&applied);
    assert_matches_output_schema("apply_patch", &applied);
    assert_eq!(applied["terminal_status"], "completed");
    assert_eq!(applied["timeout_ms"], 20_000);
    assert!(applied["duration_ms"].as_u64().is_some());
}

#[test]
fn session_successes_match_published_output_schemas() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let opened = invoke(&ctx, "session_open", json!({"session_dir": "docs/session"}));
    assert_ok(&opened);
    assert_eq!(opened["history_injected"], false);
    assert_eq!(opened["automatic_history_loading"], false);
    assert_eq!(opened["persistence"]["storage"], "workspace_file");
    assert_matches_output_schema("session_open", &opened);

    let session_id = opened["session_id"].as_str().expect("opened session_id");
    let session_path = opened["session_path"]
        .as_str()
        .expect("opened session_path");
    let checkpoint = invoke(
        &ctx,
        "session_checkpoint",
        json!({
            "session_id": session_id,
            "expected_path": session_path,
            "turn_id": "schema-contract-turn",
            "user_intent": "validate output schema",
            "tests": ["schema contract"]
        }),
    );
    assert_ok(&checkpoint);
    assert_eq!(checkpoint["target_preserved"], true);
    assert_eq!(checkpoint["storage"], "workspace_file");
    assert!(checkpoint["git_tracked"].is_boolean());
    assert!(checkpoint["git_ignored"].is_boolean());
    assert!(checkpoint["git_dirty_after_write"].is_boolean());
    assert!(checkpoint["persistence_reason"].is_string());
    assert_matches_output_schema("session_checkpoint", &checkpoint);

    let list = invoke(&ctx, "session_list", json!({"limit": 20}));
    assert_ok(&list);
    assert_matches_output_schema("session_list", &list);

    let get = invoke(&ctx, "session_get", json!({"session_id": session_id}));
    assert_ok(&get);
    assert_matches_output_schema("session_get", &get);

    let validate = invoke(
        &ctx,
        "session_validate",
        json!({"session_dir": "docs/session"}),
    );
    assert_ok(&validate);
    assert_matches_output_schema("session_validate", &validate);
}

#[test]
fn finish_task_success_matches_published_output_schema() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let started = invoke(
        &ctx,
        "start_task",
        json!({"objective": "finish task schema contract"}),
    );
    assert_ok(&started);
    let task_id = started["task"]["id"].as_str().expect("task id");

    let finished = invoke(
        &ctx,
        "finish_task",
        json!({
            "task_id": task_id,
            "allow_unverified": true,
            "session_status": "active"
        }),
    );
    assert_ok(&finished);
    assert_eq!(finished["worktree_cleanup"]["requested"], false);
    assert_eq!(finished["worktree_cleanup"]["removed"], false);
    assert_matches_output_schema("finish_task", &finished);
}

#[test]
fn incomplete_abort_successes_match_published_output_schemas() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let started = invoke(
        &ctx,
        "start_task",
        json!({"objective": "abort task schema contract"}),
    );
    assert_ok(&started);
    let task_id = started["task"]["id"].as_str().expect("task id");
    let aborted = invoke(
        &ctx,
        "abort_task",
        json!({
            "task_id": task_id,
            "reason": "user requested stop",
            "session_status": "paused"
        }),
    );
    assert_ok(&aborted);
    assert_eq!(aborted["task_status"], "incomplete");
    assert_eq!(aborted["outcome"], "incomplete");
    assert_matches_output_schema("abort_task", &aborted);

    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let started = invoke(
        &ctx,
        "begin_work_session",
        json!({
            "objective": "incomplete work session schema contract",
            "workspace_root": fx.root.to_string_lossy(),
            "contract": {"no_early_stop": true},
            "pending_steps": ["unfinished"]
        }),
    );
    assert_ok(&started);
    let task_id = started["task"]["id"].as_str().expect("task id");
    let closed = invoke(
        &ctx,
        "close_work_session",
        json!({
            "task_id": task_id,
            "outcome": "incomplete",
            "reason": "user requested stop",
            "session_status": "paused",
            "summary": "incomplete output schema"
        }),
    );
    assert_ok(&closed);
    assert_eq!(closed["work_session"]["outcome"], "incomplete");
    assert_eq!(closed["work_session"]["task_status"], "incomplete");
    assert_matches_output_schema("close_work_session", &closed);
}

#[test]
fn task_governance_successes_match_published_output_schemas() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let started = invoke(
        &ctx,
        "start_task",
        json!({"objective": "task governance output schema"}),
    );
    assert_ok(&started);
    let task_id = started["task"]["id"].as_str().expect("task id");

    let slice = invoke(
        &ctx,
        "start_slice",
        json!({
            "task_id": task_id,
            "slice_id": "schema-slice",
            "title": "schema slice",
            "acceptance_checks": [
                {"id": "schema-check", "verification_key": "schema-check"}
            ]
        }),
    );
    assert_ok(&slice);
    assert_matches_output_schema("start_slice", &slice);

    let verifying = invoke(
        &ctx,
        "update_slice",
        json!({"task_id": task_id, "slice_id": "schema-slice", "status": "verifying"}),
    );
    assert_ok(&verifying);
    assert_matches_output_schema("update_slice", &verifying);

    let gate = invoke(&ctx, "task_gate_status", json!({"task_id": task_id}));
    assert_ok(&gate);
    assert_eq!(gate["ready"], false);
    assert_matches_output_schema("task_gate_status", &gate);

    let verified = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('schema-check')"],
            "verification_kind": "test",
            "verification_key": "schema-check",
            "yield_time_ms": 30_000
        }),
    );
    assert_ok(&verified);
    let completed = invoke(
        &ctx,
        "complete_slice",
        json!({"task_id": task_id, "slice_id": "schema-slice"}),
    );
    assert_ok(&completed);
    assert_matches_output_schema("complete_slice", &completed);
}

#[test]
fn complete_work_session_success_matches_published_output_schema() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let started = invoke(
        &ctx,
        "begin_work_session",
        json!({
            "objective": "complete work session output schema",
            "workspace_root": fx.root.to_string_lossy()
        }),
    );
    assert_ok(&started);
    let task_id = started["task"]["id"].as_str().expect("task id");
    let verified = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('verified')"],
            "verification_kind": "test",
            "verification_key": "complete-work-session-schema",
            "yield_time_ms": 30_000
        }),
    );
    assert_ok(&verified);

    let completed = invoke(
        &ctx,
        "complete_work_session",
        json!({
            "task_id": task_id,
            "summary": "output schema complete",
            "checkpoint": {"tests": ["complete_work_session output schema"]}
        }),
    );
    assert_ok(&completed);
    assert_eq!(completed["work_session"]["closed"], true);
    assert_matches_output_schema("complete_work_session", &completed);
}

#[test]
fn incomplete_work_session_conflicts_preserve_business_errors_and_match_output_schemas() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let started = invoke(
        &ctx,
        "begin_work_session",
        json!({
            "objective": "incomplete work session conflict output schema",
            "workspace_root": fx.root.to_string_lossy(),
            "phase": "verifying",
            "pending_steps": ["intentional unfinished work"],
            "contract": {"no_early_stop": true}
        }),
    );
    assert_ok(&started);
    let task_id = started["task"]["id"].as_str().expect("task id");

    let aborted = invoke(
        &ctx,
        "abort_task",
        json!({
            "task_id": task_id,
            "reason": "intentional schema conflict fixture",
            "session_status": "paused"
        }),
    );
    assert_ok(&aborted);
    assert_eq!(aborted["task_status"], "incomplete");

    let wrong_outcome = invoke(
        &ctx,
        "close_work_session",
        json!({
            "task_id": task_id,
            "outcome": "completed",
            "session_status": "paused"
        }),
    );
    assert_err(&wrong_outcome);
    assert_eq!(
        wrong_outcome["error"]["code"],
        "WORK_SESSION_TASK_INCOMPLETE"
    );
    assert_matches_output_schema("close_work_session", &wrong_outcome);

    let closed = invoke(
        &ctx,
        "close_work_session",
        json!({
            "task_id": task_id,
            "outcome": "incomplete",
            "reason": "intentional schema conflict fixture",
            "session_status": "paused"
        }),
    );
    assert_ok(&closed);
    assert_eq!(closed["work_session"]["outcome"], "incomplete");
    assert_matches_output_schema("close_work_session", &closed);

    let rewrite = invoke(
        &ctx,
        "complete_work_session",
        json!({"task_id": task_id, "summary": "must not rewrite incomplete"}),
    );
    assert_err(&rewrite);
    assert_eq!(rewrite["error"]["code"], "WORK_SESSION_ALREADY_ABORTING");
    assert_matches_output_schema("complete_work_session", &rewrite);
}

#[test]
fn retained_session_tools_match_published_output_schemas() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let command = format!(
        "{TEST_PYTHON} -c \"import sys,time; print('ready', flush=True); line=sys.stdin.readline(); print('got:'+line.strip(), flush=True); time.sleep(30)\""
    );
    let started = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": command, "yield_time_ms": 500}),
    );
    assert_ok(&started);
    assert_matches_output_schema("exec_command", &started);
    let session_id = started["session_id"].as_str().expect("retained session id");
    let stdout_ref = started["output_refs"]["stdout"]
        .as_str()
        .expect("stdout ref");

    let written = invoke(
        &ctx,
        "write_stdin",
        json!({
            "session_id": session_id,
            "chars": "hello\n",
            "yield_time_ms": 1_000
        }),
    );
    assert_ok(&written);
    assert_matches_output_schema("write_stdin", &written);

    let output = invoke(
        &ctx,
        "read_output",
        json!({"output_ref": stdout_ref, "stream": "stdout", "limit": 4096}),
    );
    assert_ok(&output);
    assert_matches_output_schema("read_output", &output);
    assert!(output["content"]
        .as_str()
        .unwrap_or_default()
        .contains("got:hello"));

    let killed = invoke(
        &ctx,
        "kill_session",
        json!({"session_id": session_id, "signal": "TERM", "wait_ms": 5_000}),
    );
    assert_err(&killed);
    assert_eq!(killed["transport_status"], "ok");
    assert_eq!(killed["execution_status"], "killed");
    assert_eq!(killed["success"], false);
    assert_eq!(killed["error"]["code"], "COMMAND_KILLED");
    assert_matches_output_schema("kill_session", &killed);
}
