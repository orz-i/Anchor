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
fn history_successes_match_published_output_schemas() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let session_key = "output-schema-contract-session";
    let bootstrap = invoke(
        &ctx,
        "history_session_bootstrap",
        json!({
            "session_key": session_key,
            "history_dir": "docs/history-session"
        }),
    );
    assert_ok(&bootstrap);
    assert_eq!(bootstrap["target_preserved"], true);
    assert!(bootstrap.get("host_session_key_mismatch").is_none());
    assert!(bootstrap.get("host_session_key_mismatch_level").is_none());
    assert_eq!(bootstrap["persistence"]["storage"], "workspace_file");
    assert_matches_output_schema("history_session_bootstrap", &bootstrap);

    let current_path = bootstrap["current_path"]
        .as_str()
        .expect("bootstrap current_path");
    let checkpoint = invoke(
        &ctx,
        "history_session_checkpoint",
        json!({
            "session_key": session_key,
            "expected_path": current_path,
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
    assert_matches_output_schema("history_session_checkpoint", &checkpoint);

    let validate = invoke(
        &ctx,
        "history_session_validate",
        json!({"history_dir": "docs/history-session"}),
    );
    assert_ok(&validate);
    assert_matches_output_schema("history_session_validate", &validate);
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
