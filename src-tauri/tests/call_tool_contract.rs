mod common;

use std::fs;
use std::process::Command;

use anchor_lib::tools::list_tools_for_profile;
use common::*;
use serde_json::{json, Value};

#[cfg(windows)]
const TEST_PYTHON: &str = "python";
#[cfg(not(windows))]
const TEST_PYTHON: &str = "python3";

#[test]
fn server_info_returns_workspace_and_tools() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "server_info", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["server"], "anchor");
    assert_eq!(payload["version"], env!("CARGO_PKG_VERSION"));
    assert!(payload["tools"].is_array());
    assert!(payload["tool_count"].as_u64().unwrap_or(0) > 0);
    assert_eq!(payload["catalog_published"], false);
    assert_eq!(payload["catalog_changed"], false);
    assert_eq!(payload["reconnect_required"], false);
    assert_eq!(
        payload["running_catalog_digest"],
        payload["current_catalog_digest"]
    );
}

#[test]
fn patch_check_reports_hunk_and_nearest_context_diagnostics() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "patch_check",
        json!({
            "mode": "fuzzy",
            "patch": "--- a/src/math.js\n+++ b/src/math.js\n@@\n-function missing() { return 1; }\n+function missing() { return 2; }\n"
        }),
    );
    let error = assert_err(&out);
    assert_eq!(error["error"]["code"], "PATCH_CONTEXT_MISMATCH");
    let details = &error["error"]["details"];
    assert_eq!(details["file"], "src/math.js");
    assert_eq!(details["hunk_index"], 1);
    assert_eq!(details["hunk_index_zero_based"], 0);
    assert_eq!(details["failure_code"], "PATCH_CONTEXT_MISMATCH");
    assert_eq!(details["mode"], "fuzzy");
    assert!(details["line_hint"].as_u64().is_some());
    assert!(details["nearest_context"].is_array());
    assert_eq!(details["suggested_patch"]["read_tool"], "read_file");
}

#[test]
fn policy_rejection_returns_safe_structured_alternatives() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "exec_command", json!({"cmd": "rg hello"}));
    let error = assert_err(&out);
    assert_eq!(error["error"]["code"], "POLICY_REJECTED");
    assert_eq!(error["error"]["retryable"], true);
    assert_eq!(error["error"]["details"]["recoverable"], true);
    let alternatives = error["error"]["details"]["alternatives"]
        .as_array()
        .expect("alternatives");
    assert!(alternatives
        .iter()
        .any(|alternative| alternative["name"] == "search_text"));
}

#[test]
fn read_file_happy_path() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "read_file", json!({"path": "src/math.js"}));
    let payload = assert_ok(&out);
    assert_eq!(payload["path"], "src/math.js");
    assert_eq!(payload["encoding"], "utf-8");
}

#[test]
fn unknown_tool_is_validation_error() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "definitely_not_a_tool", json!({}));
    let err = assert_err(&out);
    assert_eq!(err["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(err["error"]["category"], "validation");
}

#[test]
fn read_file_explicit_parent_path_is_rejected() {
    let fx = malicious_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "read_file", json!({"path": "../outside-secret.txt"}));
    let result = assert_err(&out);
    assert_eq!(result["error"]["code"], "PATH_OUTSIDE_WORKSPACE");
}

#[test]
fn model_controlled_permission_tool_is_not_exposed() {
    let tools = list_tools_for_profile("core");
    assert!(!tools
        .iter()
        .any(|tool| tool["name"] == "request_permissions"));

    let fx = tiny_js_fixture();
    let out = invoke(&ctx_for(&fx.root), "request_permissions", json!({}));
    assert_eq!(assert_err(&out)["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn check_exec_environment_reports_policy_metadata() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "check_exec_environment", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["permission_mode"], "trusted");
    assert!(payload["system_command_allowlist"].is_array());
}

#[test]
fn default_cwd_is_used_by_file_and_native_exec_tools() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    assert_ok(&invoke(&ctx, "set_default_cwd", json!({"path": "src"})));

    let file_result = invoke(&ctx, "read_file", json!({"path": "math.js"}));
    let file = assert_ok(&file_result);
    assert_eq!(file["path"], "src/math.js");

    let pwd_result = invoke(&ctx, "exec_command", json!({"cmd": "pwd"}));
    let pwd = assert_ok(&pwd_result);
    assert!(pwd["stdout"].as_str().unwrap_or("").contains("src"));
}

#[test]
fn git_log_root_does_not_pass_empty_pathspec() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("repo");
    fs::create_dir_all(&workspace).expect("创建仓库目录");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");

    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "测试用户"],
        vec!["add", "README.md"],
        vec!["commit", "-q", "-m", "初始化"],
    ] {
        let output = Command::new("git")
            .current_dir(&workspace)
            .args(args)
            .output()
            .expect("执行 git");
        assert!(output.status.success(), "git 命令失败: {:?}", output);
    }

    let ctx = ctx_for(&workspace);
    let result = invoke(&ctx, "git_log", json!({"path": ".", "max_count": 3}));
    let payload = assert_ok(&result);
    assert_eq!(payload["is_repo"], true);
    assert_eq!(payload["commits"].as_array().unwrap().len(), 1);
    for commit in payload["commits"].as_array().unwrap() {
        for field in [
            "hash",
            "short_hash",
            "author_name",
            "author_email",
            "author_date",
            "subject",
        ] {
            assert_eq!(
                commit[field].as_str().unwrap(),
                commit[field].as_str().unwrap().trim()
            );
        }
    }
}

#[test]
fn advanced_profile_exposes_every_declared_tool() {
    let declared = anchor_lib::tools::registry::P0_TOOLS
        .iter()
        .map(|(name, ..)| *name)
        .collect::<std::collections::HashSet<_>>();
    let tool_values = anchor_lib::tools::list_tools_for_profile("advanced");
    let exposed = tool_values
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(declared, exposed);
    assert!(declared
        .iter()
        .all(|name| anchor_lib::tools::is_allowed_tool(name)));
}

#[test]
fn core_profile_keeps_the_default_capabilities_and_adds_history_tools() {
    let tools = anchor_lib::tools::list_tools_for_profile("core");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::HashSet<_>>();
    let expected = anchor_lib::tools::registry::CORE_TOOLS
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names, expected);
    assert_eq!(names.len(), 33);
    assert!(names.contains("list_skills"));
    assert!(names.contains("load_skill"));
    assert!(names.contains("read_skill_resource"));
    assert!(names.contains("search_text"));
    assert!(names.contains("command_cost_explain"));
    assert!(names.contains("git_stage"));
    assert!(names.contains("git_commit"));
    assert!(names.contains("git_restore"));
    assert!(names.contains("update_verification_disposition"));
    assert!(names.contains("history_session_bootstrap"));
    assert!(names.contains("history_session_checkpoint"));
    assert!(names.contains("history_session_validate"));
    assert!(names.contains("wait_command"));
    assert!(names.contains("begin_work_session"));
    assert!(names.contains("close_work_session"));
    assert!(!names.contains("harness_status"));
    assert!(!names.contains("start_task"));
}

#[test]
fn exec_health_check_reports_worker_and_pipe_status() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "exec_health_check", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["worker"]["alive"], true);
    assert_eq!(payload["session_create"], true);
    assert_eq!(payload["command_run"], true);
    assert_eq!(payload["stdout_capture"], true);
    assert_eq!(payload["stderr_capture"], true);
}

#[test]
fn native_diagnostics_support_pwd_and_ls_without_a_shell() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let pwd_result = invoke(&ctx, "exec_command", json!({"cmd": "pwd"}));
    let pwd = assert_ok(&pwd_result);
    assert_eq!(pwd["command"], "pwd");
    assert!(pwd["stdout"]
        .as_str()
        .unwrap_or("")
        .contains("tiny-js-project"));
    assert_eq!(pwd["execution_mode"], "native_builtin");
    assert_eq!(pwd["harness_mode"], "standalone");
    assert_eq!(pwd["task_required"], false);
    assert_eq!(pwd["command_runner"], "native_builtin");
    assert_eq!(pwd["status"], "exited");
    assert_eq!(pwd["exit_code"], 0);
    assert_eq!(pwd["transport_ok"], true);
    assert_eq!(pwd["command_ok"], true);
    assert_eq!(pwd["duration_ms"], 0);
    assert_eq!(pwd["elapsed_ms"], 0);
    assert!(pwd["stdout"].is_string());
    assert_eq!(pwd["stderr"], "");

    let ls_result = invoke(&ctx, "exec_command", json!({"cmd": "ls"}));
    let ls = assert_ok(&ls_result);
    assert!(ls["stdout"].as_str().unwrap_or("").contains("src"));
    assert_eq!(ls["exit_code"], 0);
}

#[test]
fn direct_exec_uses_the_same_result_contract() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": format!("{TEST_PYTHON} --version"), "filesystem_scope": "workspace"}),
    );
    let payload = assert_ok(&result);

    assert_eq!(payload["command"], format!("{TEST_PYTHON} --version"));
    assert_eq!(payload["execution_mode"], "direct");
    assert_eq!(payload["harness_mode"], "standalone");
    assert_eq!(payload["task_required"], false);
    assert_eq!(payload["status"], "exited");
    assert_eq!(payload["exit_code"], 0);
    assert!(payload["stdout"].is_string());
    assert!(payload["stderr"].is_string());
    assert!(payload["duration_ms"].is_u64());
    assert_eq!(payload["duration_ms"], payload["elapsed_ms"]);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], true);
    assert_eq!(payload["cost_policy"]["cost_class"], "free");
}

#[test]
fn wait_command_returns_terminal_state_and_incremental_output() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!(
                "{TEST_PYTHON} -c \"import time; print('started', flush=True); time.sleep(0.1); print('finished', flush=True)\""
            ),
            "filesystem_scope": "workspace",
            "timeout_ms": 5_000,
            "yield_time_ms": 0
        }),
    );
    let payload = assert_ok(&result);
    let session_id = payload["session_id"].as_str().expect("session id");

    let waited = invoke(
        &ctx,
        "wait_command",
        json!({
            "session_id": session_id,
            "timeout_ms": 2_000,
            "stdout_offset": 0,
            "stderr_offset": 0,
            "return_incremental_output": true
        }),
    );
    let waited = assert_ok(&waited);
    assert_eq!(waited["state"], "completed");
    assert_eq!(waited["exit_code"], 0);
    assert_eq!(waited["command_ok"], true);
    assert!(waited["started_at"]
        .as_str()
        .is_some_and(|value| value.ends_with('Z')));
    assert!(waited["last_output_at"]
        .as_str()
        .is_some_and(|value| value.ends_with('Z')));
    let stdout = waited["stdout"]["content"].as_str().expect("stdout");
    assert!(stdout.contains("started"));
    assert!(stdout.contains("finished"));
}

#[test]
fn nonzero_command_exit_keeps_transport_ok_but_sets_command_ok_false() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import sys; sys.exit(1)\""),
            "filesystem_scope": "workspace"
        }),
    );
    let payload = assert_ok(&result);

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], false);
    assert_eq!(payload["status"], "exited");
    assert_eq!(payload["exit_code"], 1);
}

#[test]
fn retained_session_timeout_stops_the_process_after_deadline() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(2)\""),
            "filesystem_scope": "workspace",
            "timeout_ms": 100,
            "yield_time_ms": 0
        }),
    );
    let payload = assert_ok(&result);
    assert_eq!(payload["status"], "running");
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], Value::Null);
    assert_eq!(payload["stdin_open"], true);
    let session_id = payload["session_id"].as_str().expect("session id");

    std::thread::sleep(std::time::Duration::from_millis(250));
    let after = invoke(
        &ctx,
        "write_stdin",
        json!({"session_id": session_id, "chars": ""}),
    );
    assert_eq!(after["termination_reason"], "timeout");
    assert_eq!(after["status"], "exited");
    assert_eq!(after["transport_ok"], true);
    assert_eq!(after["command_ok"], false);
    assert_eq!(after["stdin_open"], false);
    #[cfg(unix)]
    assert_eq!(after["exit_code"], Value::Null);
}

#[test]
fn killed_session_reports_command_failure_even_when_transport_succeeds() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(2)\""),
            "filesystem_scope": "workspace",
            "timeout_ms": 10_000,
            "yield_time_ms": 0
        }),
    );
    let payload = assert_ok(&result);
    let session_id = payload["session_id"].as_str().expect("session id");

    let killed = invoke(
        &ctx,
        "kill_session",
        json!({"session_id": session_id, "wait_ms": 2_000}),
    );
    let killed = assert_ok(&killed);
    assert_eq!(killed["status"], "killed");
    assert_eq!(killed["killed"], true);
    assert_eq!(killed["transport_ok"], true);
    assert_eq!(killed["command_ok"], false);
    #[cfg(unix)]
    assert_eq!(killed["exit_code"], Value::Null);
}

#[test]
fn list_files_filters_with_patterns() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "list_files",
        json!({"patterns": ["**/*.js"], "max_results": 10}),
    );
    let payload = assert_ok(&out);
    let files = payload["files"].as_array().expect("files array");
    assert!(!files.is_empty());
    assert!(files
        .iter()
        .all(|f| f["path"].as_str().unwrap_or("").ends_with(".js")));
}

#[test]
fn search_text_filters_by_glob() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let hit = invoke(
        &ctx,
        "search_text",
        json!({"query": "function add", "include_globs": ["**/*.js"], "max_results": 10}),
    );
    let hit_payload = assert_ok(&hit);
    assert!(hit_payload["total_matches"].as_u64().unwrap_or(0) > 0);

    let miss = invoke(
        &ctx,
        "search_text",
        json!({"query": "function add", "include_globs": ["**/*.py"]}),
    );
    let miss_payload = assert_ok(&miss);
    assert_eq!(miss_payload["total_matches"].as_u64().unwrap_or(1), 0);
}

#[test]
fn search_text_truncates_multibyte_preview_on_a_utf8_boundary() {
    let fx = tiny_js_fixture();
    fs::write(
        fx.root.join("src/multibyte.txt"),
        format!("marker {}\n", "连接正常".repeat(40)),
    )
    .expect("write multibyte fixture");
    let result = invoke(
        &ctx_for(&fx.root),
        "search_text",
        json!({
            "query": "marker",
            "path": "src/multibyte.txt",
            "max_preview_bytes": 64
        }),
    );
    let payload = assert_ok(&result);
    let preview = payload["matches"][0]["preview"]
        .as_str()
        .expect("preview string");

    assert!(preview.ends_with("..."));
    assert!(preview.is_char_boundary(preview.len()));
    assert!(preview.len() <= 67);
}

#[test]
fn blocking_exec_timeout_preserves_the_declared_output_contract() {
    let fx = tiny_js_fixture();
    let result = invoke(
        &ctx_for(&fx.root),
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(2)\""),
            "timeout_ms": 100,
            "yield_time_ms": 1_000
        }),
    );
    let payload = assert_ok(&result);

    assert_eq!(payload["termination_reason"], "timeout");
    assert_eq!(payload["child_process"], true);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], false);
    assert_eq!(payload["error"]["code"], "TIMEOUT");
    assert!(payload["suggestion"].is_string());
    assert!(payload["duration_ms"].is_u64());
    assert_eq!(payload["duration_ms"], payload["elapsed_ms"]);
    assert!(payload["warnings"].is_array());
}
