mod common;

use std::fs;
use std::process::Command;

use anchor_lib::tools::{call_tool_for_session, list_tools_for_profile};
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
    assert_eq!(payload["detail"], "compact");
    assert_eq!(payload["full_detail_available"], true);
    assert_eq!(payload["server"], "anchor");
    assert_eq!(payload["version"], env!("CARGO_PKG_VERSION"));
    assert!(payload.get("tools").is_none());
    assert!(payload.get("tool_groups").is_none());
    assert!(payload["tool_count"].as_u64().unwrap_or(0) > 0);
    assert!(payload["build_identity"]["git_sha"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(payload["build_identity"]["git_dirty"].is_boolean());
    assert_eq!(
        payload["build_identity"]["catalog_version"],
        payload["catalog_version"]
    );
    assert_eq!(
        payload["build_identity"]["package_version"],
        payload["version"]
    );
    assert_eq!(payload["catalog_published"], false);
    assert_eq!(payload["catalog_changed"], false);
    assert_eq!(payload["reconnect_required"], false);
    assert_eq!(
        payload["running_catalog_digest"],
        payload["current_catalog_digest"]
    );
    assert_eq!(
        payload["command_cost_policy"]["workspace_policy_path"],
        Value::Null
    );
    assert_eq!(
        payload["command_cost_policy"]["policy_identifier"],
        "trusted_runtime_config"
    );
    assert!(payload["schema_telemetry"]["definition_bytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0));
    assert!(payload["schema_telemetry"]["largest_tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty() && tools.len() <= 8));
    assert!(payload["response_bytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0));

    let full_out = invoke(&ctx, "server_info", json!({"detail": "full"}));
    let full = assert_ok(&full_out);
    assert_eq!(full["detail"], "full");
    assert!(full["tools"].is_array());
    assert!(full["tool_groups"].is_object());
    assert!(
        full["response_bytes"].as_u64().unwrap_or(0)
            > payload["response_bytes"].as_u64().unwrap_or(u64::MAX),
        "compact={payload} full={full}"
    );
}

#[test]
fn named_node_toolchain_selects_a_trusted_runtime_and_reports_the_selection() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": "node",
            "args": ["-e", "console.log(process.execPath)"],
            "toolchains": {"node": "default"}
        }),
    );
    let payload = assert_ok(&result);
    assert_eq!(payload["command_ok"], true, "{payload}");
    assert_eq!(payload["named_toolchains"]["node"]["selector"], "default");
    assert!(payload["named_toolchains"]["node"]["home"]
        .as_str()
        .is_some_and(|home| !home.is_empty()));
    assert!(payload["resolved_executable"]
        .as_str()
        .is_some_and(|path| path.to_ascii_lowercase().contains("node")));
}

#[test]
fn named_toolchain_conflicting_environment_is_rejected_before_spawn() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('must-not-run')"],
            "toolchains": {"java": "default"},
            "env": {"JAVA_HOME": "legacy-manual-selection"}
        }),
    );
    let payload = assert_err(&result);
    assert_eq!(
        payload["error"]["code"], "TOOLCHAIN_ENV_CONFLICT",
        "{payload}"
    );
    assert_eq!(
        payload["error"]["details"]["cause_scope"],
        "toolchain_registry"
    );
    assert_eq!(payload["error"]["details"]["workspace_mutated"], false);
    assert_eq!(payload["execution_started"], false);
}

#[test]
fn replace_text_routes_through_public_contract_and_honors_preconditions() {
    let fx = tiny_js_fixture();
    let target = fx.root.join("replace.txt");
    fs::write(&target, "old value\nold value\n").expect("fixture");
    let ctx = ctx_for(&fx.root);

    let dry = invoke(
        &ctx,
        "replace_text",
        json!({
            "files": [{"path": "replace.txt", "expected_matches": 2}],
            "find": "old",
            "replace": "new",
            "dry_run": true
        }),
    );
    let dry = assert_ok(&dry);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["total_matches"], 2);
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "old value\nold value\n"
    );

    let sha = dry["files"][0]["before_sha256"]
        .as_str()
        .expect("sha")
        .to_string();
    let committed = invoke(
        &ctx,
        "replace_text",
        json!({
            "files": [{
                "path": "replace.txt",
                "expected_matches": 2,
                "expected_sha256": sha
            }],
            "find": "old",
            "replace": "new"
        }),
    );
    let committed = assert_ok(&committed);
    assert_eq!(committed["transaction"]["cas_verified"], true);
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "new value\nnew value\n"
    );
}

#[test]
fn task_status_surfaces_running_command_heartbeat() {
    let fx = tiny_js_fixture();
    let mut ctx = ctx_for(&fx.root);
    ctx.tool_profile = "advanced".into();
    let started = invoke(
        &ctx,
        "task",
        json!({"operation": "start", "objective": "heartbeat regression"}),
    );
    let task_id = assert_ok(&started)["task"]["id"]
        .as_str()
        .expect("task id")
        .to_string();

    let command = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-u", "-c", "import time; print('started', flush=True); time.sleep(0.4)"],
            "yield_time_ms": 0,
            "timeout_ms": 10_000,
            "verification_level": "diagnostic"
        }),
    );
    let command = assert_ok(&command);
    assert_eq!(command["execution_status"], "running");

    let status = invoke(
        &ctx,
        "task",
        json!({"operation": "status", "task_id": task_id}),
    );
    let status = assert_ok(&status);
    assert_eq!(status["running_command_count"], 1);
    assert_eq!(status["current_operation"]["kind"], "command");
    assert_eq!(
        status["current_operation"]["session_id"],
        command["session_id"]
    );
    assert_eq!(
        status["current_operation"]["next_milestone"],
        "terminal_command_result"
    );

    let waited = invoke(
        &ctx,
        "wait_command",
        json!({"session_id": command["session_id"], "timeout_ms": 2_000}),
    );
    assert_ok(&waited);
}

#[test]
fn structured_direct_exec_treats_multiline_source_as_literal_argument() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let executed = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": [
                "-c",
                "print('literal && operator')\nprint('structured-direct-ok')"
            ],
            "yield_time_ms": 10_000,
            "timeout_ms": 10_000,
            "verification_level": "diagnostic"
        }),
    );

    let executed = assert_ok(&executed);
    assert_eq!(executed["execution_status"], "succeeded");
    assert!(executed["stdout"]
        .as_str()
        .is_some_and(|output| output.contains("structured-direct-ok")));
}

#[cfg(unix)]
#[test]
fn exec_command_uses_workspace_local_toolchain_path_for_resolution_and_children() {
    use std::os::unix::fs::PermissionsExt;

    let fx = tiny_js_fixture();
    let bin = fx.root.join(".cache/bin");
    fs::create_dir_all(&bin).expect("toolchain bin");
    let fake_node = bin.join("node");
    let fake_tsc = bin.join("tsc");
    fs::write(&fake_node, "#!/bin/sh\nexec tsc\n").expect("fake node");
    fs::write(&fake_tsc, "#!/bin/sh\nprintf 'workspace-toolchain-ok\\n'\n").expect("fake tsc");
    fs::set_permissions(&fake_node, fs::Permissions::from_mode(0o755)).expect("chmod node");
    fs::set_permissions(&fake_tsc, fs::Permissions::from_mode(0o755)).expect("chmod tsc");
    let ctx = ctx_for(&fx.root);

    let executed = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": "node",
            "toolchain_paths": [".cache/bin"],
            "yield_time_ms": 10_000,
            "timeout_ms": 10_000,
            "cost_intent": "local_only",
            "network_mode": "disabled"
        }),
    );
    let executed = assert_ok(&executed);
    assert_eq!(executed["execution_status"], "succeeded");
    assert!(executed["stdout"]
        .as_str()
        .is_some_and(|output| output.contains("workspace-toolchain-ok")));

    let escaped = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": "node",
            "toolchain_paths": ["../outside"]
        }),
    );
    assert_err(&escaped);
    assert_eq!(escaped["execution_started"], false);
}

#[cfg(unix)]
#[test]
fn apply_patch_preserves_existing_executable_mode() {
    use std::os::unix::fs::PermissionsExt;

    let fx = tiny_js_fixture();
    let script = fx.root.join("tool.sh");
    fs::write(&script, "#!/bin/sh\necho before\n").expect("write executable fixture");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod fixture");
    let ctx = ctx_for(&fx.root);

    let patched = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/tool.sh\n+++ b/tool.sh\n@@\n #!/bin/sh\n-echo before\n+echo after\n"
        }),
    );
    assert_ok(&patched);
    let mode = fs::metadata(&script)
        .expect("metadata")
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "executable bits must be preserved");
    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        "#!/bin/sh\necho after\n"
    );
}

#[test]
fn diagnostic_command_failure_does_not_open_task_recovery() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let task = ctx
        .harness
        .start_task("diagnostic command recovery boundary")
        .expect("start task");

    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "raise SystemExit(9)"],
            "timeout_ms": 10_000,
            "yield_time_ms": 10_000,
            "verification_kind": "diagnostic_probe",
            "verification_level": "diagnostic"
        }),
    );
    let result = assert_err(&result);
    assert_eq!(result["execution_started"], true);
    assert_eq!(result["exit_code"], 9);
    assert!(result.get("task_recovery").is_none(), "{result}");
    assert!(ctx
        .harness
        .task(&task.id)
        .expect("reload task")
        .recovery
        .is_none());
}

#[test]
fn running_retry_resolves_recovery_only_after_terminal_success() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let task = ctx
        .harness
        .start_task("terminal recovery resolution")
        .expect("start task");

    let failed = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "raise SystemExit(7)"],
            "timeout_ms": 10_000,
            "yield_time_ms": 10_000
        }),
    );
    let failed = assert_err(&failed);
    let recovery_key = failed["task_recovery"]["recovery_key"]
        .as_str()
        .expect("recovery key")
        .to_string();

    let retry = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-u", "-c", "import time; time.sleep(0.2); print('recovered')"],
            "timeout_ms": 10_000,
            "yield_time_ms": 0,
            "recovery_key": recovery_key
        }),
    );
    let retry = assert_ok(&retry);
    assert_eq!(retry["execution_status"], "running");
    assert_eq!(
        ctx.harness
            .task(&task.id)
            .expect("running retry task")
            .recovery
            .expect("recovery remains open")
            .status,
        anchor_lib::harness::model::TaskRecoveryStatus::Open
    );

    let waited = invoke(
        &ctx,
        "wait_command",
        json!({"session_id": retry["session_id"], "timeout_ms": 2_000}),
    );
    let waited = assert_ok(&waited);
    assert_eq!(waited["execution_status"], "succeeded");
    assert_eq!(waited["task_recovery"]["status"], "resolved");
    assert_eq!(
        ctx.harness
            .task(&task.id)
            .expect("terminal retry task")
            .recovery
            .expect("resolved recovery")
            .status,
        anchor_lib::harness::model::TaskRecoveryStatus::Resolved
    );
}

#[test]
fn preflight_failure_with_verification_identity_still_does_not_open_recovery() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let task = ctx
        .harness
        .start_task("preflight verification recovery boundary")
        .expect("start task");

    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": "which anchor-definitely-missing-program",
            "verification_kind": "toolchain_probe",
            "verification_key": "missing-program-probe",
            "verification_level": "diagnostic"
        }),
    );
    let result = assert_err(&result);
    assert_eq!(result["execution_started"], false);
    assert!(result.get("task_recovery").is_none(), "{result}");
    assert!(ctx
        .harness
        .task(&task.id)
        .expect("reload task")
        .recovery
        .is_none());
}

#[test]
fn environment_and_cwd_facades_route_to_existing_contracts() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let environment = invoke(&ctx, "environment", json!({"operation": "check"}));
    let environment = assert_ok(&environment);
    assert_eq!(environment["facade"], "environment");
    assert!(environment["development_environment"]["toolchain_registry"]["runtimes"].is_object());

    assert_eq!(environment["operation"], "check");
    assert_eq!(environment["detail"], "compact");
    assert!(environment["workspace_exec_available"].is_boolean());

    let full_environment = invoke(
        &ctx,
        "environment",
        json!({"operation": "check", "detail": "full"}),
    );
    let full_environment = assert_ok(&full_environment);
    assert_eq!(full_environment["detail"], "full");
    assert!(full_environment["development_environment"]["probes"].is_object());
    assert_eq!(
        full_environment["development_environment"]["toolchain_registry"]["accepts_external_paths"],
        false
    );
    assert!(
        full_environment["development_environment"]["toolchain_registry"]["runtimes"]["node"]
            .is_array()
    );

    let initial = invoke(&ctx, "cwd", json!({"operation": "get"}));
    let initial = assert_ok(&initial);
    assert_eq!(initial["facade"], "cwd");
    assert_eq!(initial["operation"], "get");

    let set = invoke(&ctx, "cwd", json!({"operation": "set", "path": "src"}));
    let set = assert_ok(&set);
    assert_eq!(set["facade"], "cwd");
    assert_eq!(set["operation"], "set");

    let updated = invoke(&ctx, "cwd", json!({"operation": "get"}));
    let updated = assert_ok(&updated);
    assert!(updated["resolved_cwd"]
        .as_str()
        .is_some_and(|cwd| cwd.ends_with("src")));
}

#[test]
fn structured_exec_rejects_sensitive_or_process_control_environment() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    for protected_name in ["PATH", "OPENAI_API_KEY", "LD_PRELOAD", "NODE_OPTIONS"] {
        let result = invoke(
            &ctx,
            "exec_command",
            json!({
                "executable": TEST_PYTHON,
                "args": ["-c", "print('not-run')"],
                "env": {protected_name: "blocked"}
            }),
        );
        let payload = assert_err(&result);
        assert_eq!(payload["error"]["code"], "POLICY_REJECTED");
        assert_eq!(payload["execution_started"], false);
    }
}

#[test]
fn git_facade_routes_to_existing_git_contracts() {
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
    let status = invoke(&ctx, "git", json!({"operation": "status"}));
    let status = assert_ok(&status);
    assert_eq!(status["facade"], "git");
    assert_eq!(status["operation"], "status");
    assert_eq!(status["is_repo"], true);

    let log = invoke(
        &ctx,
        "git",
        json!({"operation": "log", "path": ".", "max_count": 3}),
    );
    let log = assert_ok(&log);
    assert_eq!(log["operation"], "log");
    assert_eq!(log["commits"].as_array().unwrap().len(), 1);

    let invalid_stage = invoke(&ctx, "git", json!({"operation": "stage"}));
    let invalid_stage = assert_err(&invalid_stage);
    assert_eq!(invalid_stage["operation"], "stage");
    assert_eq!(invalid_stage["error"]["code"], "INVALID_TOOL_ARGUMENTS");
    assert_eq!(
        invalid_stage["error"]["details"]["required_arguments"],
        json!(["paths"])
    );

    let original_branch = status["branch"]
        .as_str()
        .expect("original branch")
        .to_string();
    let initial_head = status["head"].as_str().expect("initial head").to_string();
    let created = invoke(
        &ctx,
        "git",
        json!({"operation": "branch_create", "name": "feature/structured-git"}),
    );
    let created = assert_ok(&created);
    assert_eq!(created["branch"], "feature/structured-git");

    let switched = invoke(
        &ctx,
        "git",
        json!({"operation": "switch", "target": "feature/structured-git"}),
    );
    let switched = assert_ok(&switched);
    assert_eq!(switched["after_branch"], "feature/structured-git");

    fs::write(workspace.join("feature.txt"), "structured git\n").expect("write feature");
    assert_ok(&invoke(
        &ctx,
        "git",
        json!({"operation": "stage", "paths": ["feature.txt"]}),
    ));
    let committed = invoke(
        &ctx,
        "git",
        json!({"operation": "commit", "message": "feature commit"}),
    );
    let committed = assert_ok(&committed);
    let feature_head = committed["commit_sha"]
        .as_str()
        .expect("feature head")
        .to_string();

    let ancestry = invoke(
        &ctx,
        "git",
        json!({
            "operation": "is_ancestor",
            "ancestor": initial_head,
            "descendant": feature_head
        }),
    );
    assert_eq!(assert_ok(&ancestry)["is_ancestor"], true);

    assert_ok(&invoke(
        &ctx,
        "git",
        json!({"operation": "switch", "target": original_branch}),
    ));
    let merged = invoke(
        &ctx,
        "git",
        json!({"operation": "merge", "ref": "feature/structured-git"}),
    );
    let merged = assert_ok(&merged);
    assert_eq!(merged["after_head"], feature_head);
    assert_eq!(merged["fast_forwarded"], true);

    let deleted = invoke(
        &ctx,
        "git",
        json!({"operation": "branch_delete", "name": "feature/structured-git"}),
    );
    let deleted = assert_ok(&deleted);
    assert_eq!(deleted["deleted_head"], feature_head);
}

#[test]
fn git_facade_describes_and_reports_operation_specific_arguments() {
    let tools = list_tools_for_profile("core");
    let git = tools
        .iter()
        .find(|tool| tool["name"] == "git")
        .expect("git facade");
    assert_eq!(
        git["inputSchema"]["properties"]["include_ignored"]["description"],
        "Only for: clean"
    );
    assert!(git["inputSchema"]["properties"]["operation"]["description"]
        .as_str()
        .is_some_and(|description| description.contains("Required:")));

    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let filtered = invoke(
        &ctx,
        "git",
        json!({"operation": "status", "include_ignored": false}),
    );
    let filtered = assert_ok(&filtered);
    assert_eq!(filtered["facade"], "git");
    assert_eq!(filtered["operation"], "status");
    assert_eq!(filtered["ignored_arguments"], json!(["include_ignored"]));
}

#[test]
fn facade_filters_public_arguments_that_belong_to_other_operations() {
    let fx = tiny_js_fixture();
    let mut ctx = ctx_for(&fx.root);
    ctx.tool_profile = "advanced".into();

    let opened = invoke(&ctx, "session", json!({"operation": "open"}));
    let opened = assert_ok(&opened);
    let session_id = opened["session_id"].as_str().expect("session id");

    let listed = invoke(
        &ctx,
        "session",
        json!({"operation": "list", "max_bytes": 131072}),
    );
    let listed = assert_ok(&listed);
    assert_eq!(listed["ignored_arguments"], json!(["max_bytes"]));

    let fetched = invoke(
        &ctx,
        "session",
        json!({"operation": "get", "session_id": session_id, "limit": 5}),
    );
    let fetched = assert_ok(&fetched);
    assert_eq!(fetched["ignored_arguments"], json!(["limit"]));

    let started = invoke(
        &ctx,
        "task",
        json!({"operation": "start", "objective": "facade filter regression"}),
    );
    let started = assert_ok(&started);
    let task_id = started["task"]["id"].as_str().expect("task id");

    let status = invoke(
        &ctx,
        "task",
        json!({"operation": "status", "task_id": task_id}),
    );
    let status = assert_ok(&status);
    assert_eq!(status["ignored_arguments"], json!(["task_id"]));

    let events = invoke(
        &ctx,
        "task",
        json!({"operation": "events", "task_id": task_id, "detail": "full"}),
    );
    let events = assert_ok(&events);
    assert_eq!(events["detail"], "full");
    assert!(events.get("ignored_arguments").is_none());
    assert!(events["events"]
        .as_array()
        .is_some_and(|items| !items.is_empty()));
}

#[test]
fn git_facade_enforces_profile_specific_operations() {
    let fx = tiny_js_fixture();
    let mut ctx = ctx_for(&fx.root);
    ctx.tool_profile = "read-only".into();

    let denied = invoke(
        &ctx,
        "git",
        json!({"operation": "reset", "revision": "HEAD", "mode": "mixed"}),
    );
    let denied = assert_err(&denied);
    assert_eq!(denied["facade"], "git");
    assert_eq!(denied["operation"], "reset");
    assert_eq!(
        denied["error"]["code"],
        "FACADE_OPERATION_NOT_AVAILABLE_FOR_PROFILE"
    );
    assert_eq!(denied["error"]["details"]["tool_profile"], "read-only");
}

#[test]
fn harness_facades_delegate_to_existing_task_slice_and_commit_stage_contracts() {
    let fx = tiny_js_fixture();
    let mut ctx = ctx_for(&fx.root);
    ctx.tool_profile = "advanced".into();

    let status = invoke(&ctx, "task", json!({"operation": "status"}));
    let status = assert_ok(&status);
    assert_eq!(status["facade"], "task");
    assert_eq!(status["operation"], "status");

    let invalid_start = invoke(&ctx, "task", json!({"operation": "start"}));
    let invalid_start = assert_err(&invalid_start);
    assert_eq!(invalid_start["error"]["code"], "INVALID_TOOL_ARGUMENTS");
    assert_eq!(
        invalid_start["error"]["details"]["required_arguments"],
        json!(["objective"])
    );

    let started = invoke(
        &ctx,
        "task",
        json!({"operation": "start", "objective": "facade task"}),
    );
    let started = assert_ok(&started);
    let task_id = started["task"]["id"].as_str().expect("task id");
    assert_eq!(started["facade"], "task");
    assert_eq!(started["operation"], "start");

    let slice = invoke(
        &ctx,
        "slice",
        json!({
            "operation": "start",
            "task_id": task_id,
            "slice_id": "facade-slice",
            "title": "Facade slice"
        }),
    );
    let slice = assert_ok(&slice);
    assert_eq!(slice["facade"], "slice");
    assert_eq!(slice["operation"], "start");
    assert_eq!(slice["slice"]["id"], "facade-slice");

    let invalid_commit_stage = invoke(&ctx, "commit_stage", json!({"operation": "run"}));
    let invalid_commit_stage = assert_err(&invalid_commit_stage);
    assert_eq!(invalid_commit_stage["facade"], "commit_stage");
    assert_eq!(invalid_commit_stage["operation"], "run");
    assert_eq!(
        invalid_commit_stage["error"]["code"],
        "INVALID_TOOL_ARGUMENTS"
    );
}

#[test]
fn command_discovery_failure_does_not_open_task_recovery() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let task = ctx
        .harness
        .start_task("command discovery recovery boundary")
        .expect("start task");

    let result = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": "which anchor-definitely-missing-program"}),
    );
    let result = assert_err(&result);
    assert_eq!(result["error"]["code"], "COMMAND_NOT_FOUND");
    assert_eq!(result["execution_started"], false);
    assert!(result.get("task_recovery").is_none(), "{result}");
    assert!(ctx
        .harness
        .task(&task.id)
        .expect("reload task")
        .recovery
        .is_none());
}

#[test]
fn command_that_started_and_failed_still_opens_task_recovery() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let task = ctx
        .harness
        .start_task("started command recovery boundary")
        .expect("start task");

    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "raise SystemExit(7)"],
            "timeout_ms": 10_000,
            "yield_time_ms": 10_000
        }),
    );
    let result = assert_err(&result);
    assert_eq!(result["execution_started"], true);
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["task_recovery"]["status"], "open");
    assert!(ctx
        .harness
        .task(&task.id)
        .expect("reload task")
        .recovery
        .is_some());
}

#[test]
fn wait_command_maintains_the_output_cursor_for_each_caller_session() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let caller_session = "managed-cursor-caller";
    let started = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('cursor-line', flush=True)"],
            "yield_time_ms": 0,
            "timeout_ms": 5_000
        }),
        caller_session,
    );
    let started = assert_ok(&started);
    let command_session = started["session_id"].as_str().expect("command session");

    let first = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 5_000}),
        caller_session,
    );
    let first = assert_ok(&first);
    assert!(first["stdout"]["content"]
        .as_str()
        .is_some_and(|content| content.contains("cursor-line")));

    let second = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 0}),
        caller_session,
    );
    let second = assert_ok(&second);
    assert_eq!(second["stdout"]["content"], "");

    let replay = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({
            "session_id": command_session,
            "timeout_ms": 0,
            "stdout_offset": 0,
            "stderr_offset": 0
        }),
        caller_session,
    );
    let replay = assert_ok(&replay);
    assert!(replay["stdout"]["content"]
        .as_str()
        .is_some_and(|content| content.contains("cursor-line")));
}

#[test]
fn wait_command_cursor_survives_stateless_transport_for_the_same_principal_only() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let first_caller = "standalone-principal-transport-a";
    let second_caller = "standalone-principal-transport-b";
    let other_caller = "standalone-other-principal";
    ctx.bind_cursor_scope_for_session(first_caller, Some("oauth-client:principal-a"));

    let started = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('principal-cursor-line', flush=True)"],
            "yield_time_ms": 0,
            "timeout_ms": 5_000
        }),
        first_caller,
    );
    let started = assert_ok(&started);
    let command_session = started["session_id"].as_str().expect("command session");

    let first = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 5_000}),
        first_caller,
    );
    let first = assert_ok(&first);
    assert!(first["stdout"]["content"]
        .as_str()
        .is_some_and(|content| content.contains("principal-cursor-line")));
    ctx.clear_session_state(first_caller);

    ctx.bind_cursor_scope_for_session(second_caller, Some("oauth-client:principal-a"));
    let second = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 0}),
        second_caller,
    );
    let second = assert_ok(&second);
    assert_eq!(second["stdout"]["content"], "");

    ctx.bind_cursor_scope_for_session(other_caller, Some("oauth-client:principal-b"));
    let isolated = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 0}),
        other_caller,
    );
    let isolated = assert_ok(&isolated);
    assert!(isolated["stdout"]["content"]
        .as_str()
        .is_some_and(|content| content.contains("principal-cursor-line")));
}

#[test]
fn list_command_sessions_separates_execution_duration_from_retention_age() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let caller_session = "session-duration-caller";
    let started = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": [
                "-u",
                "-c",
                "import time; print('duration-line', flush=True); time.sleep(0.15)"
            ],
            "yield_time_ms": 0,
            "timeout_ms": 5_000
        }),
        caller_session,
    );
    let started = assert_ok(&started);
    let command_session = started["session_id"].as_str().expect("command session");
    std::thread::sleep(std::time::Duration::from_millis(350));

    let first = call_tool_for_session(
        &ctx,
        "list_command_sessions",
        &json!({"include_terminal": true, "max_output_bytes": 1_024}),
        caller_session,
    );
    let first = assert_ok(&first);
    assert_eq!(first["requires_followup"], true);
    assert_eq!(first["pending_result_count"], 1);
    assert_eq!(first["unobserved_terminal_count"], 1);
    let first_session = first["sessions"]
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["session_id"] == command_session)
        })
        .expect("retained command session");
    assert_eq!(first_session["execution_status"], "succeeded");
    assert_eq!(first_session["result_observed"], false);
    assert_eq!(
        first_session["elapsed_ms"],
        first_session["execution_duration_ms"]
    );
    let execution_duration = first_session["execution_duration_ms"]
        .as_u64()
        .expect("execution duration");
    let first_age = first_session["session_age_ms"]
        .as_u64()
        .expect("session age");
    let first_retained = first_session["retained_ms"].as_u64().expect("retained age");

    std::thread::sleep(std::time::Duration::from_millis(120));
    let second = call_tool_for_session(
        &ctx,
        "list_command_sessions",
        &json!({"include_terminal": true, "max_output_bytes": 1_024}),
        caller_session,
    );
    let second = assert_ok(&second);
    let second_session = second["sessions"]
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["session_id"] == command_session)
        })
        .expect("retained command session");
    assert_eq!(second_session["execution_duration_ms"], execution_duration);
    assert!(second_session["session_age_ms"].as_u64().unwrap_or(0) > first_age);
    assert!(second_session["retained_ms"].as_u64().unwrap_or(0) >= first_retained);

    let running_only = call_tool_for_session(
        &ctx,
        "list_command_sessions",
        &json!({"include_terminal": false, "max_output_bytes": 0}),
        caller_session,
    );
    let running_only = assert_ok(&running_only);
    assert_eq!(running_only["session_count"], 0);
    assert_eq!(running_only["pending_result_count"], 1);
    assert_eq!(running_only["unobserved_terminal_count"], 1);
    assert_eq!(running_only["requires_followup"], true);

    let waited = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 0}),
        caller_session,
    );
    let waited = assert_ok(&waited);
    assert_eq!(waited["result_observed"], true);
    assert_eq!(waited["execution_duration_ms"], execution_duration);

    let final_list = call_tool_for_session(
        &ctx,
        "list_command_sessions",
        &json!({"include_terminal": true, "max_output_bytes": 0}),
        caller_session,
    );
    let final_list = assert_ok(&final_list);
    assert_eq!(final_list["pending_result_count"], 0);
    assert_eq!(final_list["requires_followup"], false);
}

#[test]
fn consumed_terminal_sessions_do_not_exhaust_the_sixty_four_session_limit() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let caller_session = "terminal-capacity-regression";

    for index in 0..70 {
        let started = call_tool_for_session(
            &ctx,
            "exec_command",
            &json!({
                "executable": TEST_PYTHON,
                "args": [
                    "-u",
                    "-c",
                    format!("import time; time.sleep(0.01); print('done-{index}', flush=True)")
                ],
                "yield_time_ms": 0,
                "timeout_ms": 5_000,
                "verification_level": "diagnostic"
            }),
            caller_session,
        );
        let started = assert_ok(&started);
        assert_eq!(started["execution_status"], "running", "iteration {index}");
        let command_session = started["session_id"]
            .as_str()
            .expect("retained command session");

        let waited = call_tool_for_session(
            &ctx,
            "wait_command",
            &json!({"session_id": command_session, "timeout_ms": 5_000}),
            caller_session,
        );
        let waited = assert_ok(&waited);
        assert_eq!(waited["execution_status"], "succeeded", "iteration {index}");
        assert_eq!(waited["result_observed"], true, "iteration {index}");
    }

    let retained = call_tool_for_session(
        &ctx,
        "list_command_sessions",
        &json!({"include_terminal": true, "max_output_bytes": 0}),
        caller_session,
    );
    let retained = assert_ok(&retained);
    assert_eq!(retained["pending_result_count"], 0);
    assert_eq!(retained["requires_followup"], false);
    assert!(retained["session_count"].as_u64().unwrap_or(0) >= 64);
}

#[test]
fn wait_command_cursor_survives_transport_session_rebinding_to_the_same_task() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let task = ctx
        .harness
        .start_task("cursor transport rebind")
        .expect("task");
    let first_caller = "managed-cursor-transport-a";
    let second_caller = "managed-cursor-transport-b";
    ctx.bind_task_for_session(Some(first_caller), &task.id)
        .expect("bind first transport");
    let started = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('transport-rebind-line', flush=True)"],
            "yield_time_ms": 0,
            "timeout_ms": 5_000
        }),
        first_caller,
    );
    let started = assert_ok(&started);
    let command_session = started["session_id"].as_str().expect("command session");

    let first = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 5_000}),
        first_caller,
    );
    let first = assert_ok(&first);
    assert!(first["stdout"]["content"]
        .as_str()
        .is_some_and(|content| content.contains("transport-rebind-line")));

    let second = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 0}),
        second_caller,
    );
    let second = assert_ok(&second);
    assert_eq!(second["stdout"]["content"], "");
    assert!(ctx.task_for_session(Some(second_caller)).is_none());
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
    let out = invoke(&ctx, "exec_command", json!({"cmd": "findstr hello"}));
    let error = assert_err(&out);
    assert_eq!(error["error"]["code"], "POLICY_REJECTED");
    assert_eq!(error["error"]["retryable"], true);
    assert_eq!(error["error"]["details"]["recoverable"], true);
    let alternatives = error["error"]["details"]["alternatives"]
        .as_array()
        .expect("alternatives");
    assert!(alternatives
        .iter()
        .any(|alternative| alternative["name"] == "search"));
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
    assert_eq!(payload["detail"], "compact");
    assert_eq!(payload["full_detail_available"], true);
    assert!(payload["response_bytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0));
    assert_eq!(payload["permission_mode"], "trusted");
    assert!(payload["system_command_allowlist"].is_array());
    assert_eq!(
        payload["development_environment"]["docker_execution_allowed"],
        payload["system_command_allowlist"]
            .as_array()
            .is_some_and(|commands| commands.iter().any(|command| command == "docker"))
    );
    assert!(
        payload["development_environment"]["toolchain_search_path"]["effective_additions"]
            .is_array()
    );
    assert!(payload["healthy"].is_boolean());
    assert!(matches!(
        payload["status"].as_str(),
        Some("healthy" | "degraded")
    ));
    assert!(payload["retryable"].is_boolean());

    let full_out = invoke(&ctx, "check_exec_environment", json!({"detail": "full"}));
    let full = assert_ok(&full_out);
    assert_eq!(full["detail"], "full");
    assert!(full["development_environment"]["probes"].is_object());
    assert!(
        full["response_bytes"].as_u64().unwrap_or(0)
            > payload["response_bytes"].as_u64().unwrap_or(u64::MAX),
        "compact={payload} full={full}"
    );
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
    let declared = anchor_lib::tools::registry::exposed_tool_names("advanced")
        .into_iter()
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
    for internal in anchor_lib::tools::registry::P0_TOOLS
        .iter()
        .map(|(name, ..)| *name)
        .filter(|name| anchor_lib::tools::registry::is_facade_operation_tool(name))
    {
        assert!(
            !exposed.contains(internal),
            "internal operation leaked: {internal}"
        );
        assert!(!anchor_lib::tools::is_allowed_tool(internal));
    }
}

#[test]
fn core_profile_keeps_default_capabilities_and_exposes_one_session_facade() {
    let tools = anchor_lib::tools::list_tools_for_profile("core");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::HashSet<_>>();
    let expected = anchor_lib::tools::registry::exposed_tool_names("core")
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names, expected);
    assert_eq!(names.len(), 25);
    assert!(names.contains("git"));
    assert!(names.contains("task"));
    assert!(names.contains("skill"));
    assert!(names.contains("environment"));
    assert!(names.contains("cwd"));
    assert!(names.contains("search"));
    assert!(!names.contains("grep"));
    assert!(!names.contains("search_text"));
    assert!(!names.contains("command_cost_explain"));
    assert!(names.contains("session"));
    assert!(!names
        .iter()
        .any(|name| name.starts_with("history_session_")));
    assert!(names.contains("wait_command"));
    assert!(names.contains("list_command_sessions"));
    assert!(names.contains("browser_build_info"));
    assert!(names.contains("browser_wait_for_build"));
    assert!(names.contains("begin_work_session"));
    assert!(names.contains("close_work_session"));
    assert!(!names.contains("harness_status"));
    assert!(!names.contains("start_task"));
    assert!(!names.contains("list_skills"));
    assert!(!names.contains("load_skill"));
    assert!(!names.contains("read_skill_resource"));
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
    assert!(payload.get("cost_policy").is_none());
    assert!(payload.get("filesystem_scope").is_none());
    assert!(payload.get("execution_boundary").is_none());

    let diagnostic_result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} --version"),
            "include_diagnostics": true
        }),
    );
    let diagnostic = assert_ok(&diagnostic_result);
    assert_eq!(diagnostic["cost_policy"]["cost_class"], "free");
    assert_eq!(diagnostic["filesystem_scope"], "workspace");
    assert_eq!(diagnostic["execution_boundary"], "policy_only");
}

#[test]
fn structured_exec_preserves_exact_args_and_environment() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": [
                "-c",
                "import os,sys; print(os.environ['ANCHOR_STRUCTURED_ENV']); print(sys.argv[1])",
                "value with spaces\\and-backslashes"
            ],
            "env": {"ANCHOR_STRUCTURED_ENV": "structured-ok"}
        }),
    );
    let payload = assert_ok(&result);
    assert_eq!(payload["execution_mode"], "structured_direct");
    let stdout = payload["stdout"].as_str().expect("stdout");
    assert!(stdout.contains("structured-ok"));
    assert!(stdout.contains("value with spaces\\and-backslashes"));
}

#[cfg(windows)]
#[test]
fn structured_powershell_exec_uses_explicit_shell_args_and_env() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "shell": "powershell",
            "args": ["-NoProfile", "-Command", "Write-Output $env:ANCHOR_PS_ENV"],
            "env": {"ANCHOR_PS_ENV": "powershell-ok"}
        }),
    );
    let payload = assert_ok(&result);
    assert_eq!(payload["execution_mode"], "powershell");
    assert!(payload["stdout"]
        .as_str()
        .is_some_and(|stdout| stdout.contains("powershell-ok")));
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
            "yield_time_ms": 0,
            "durable": true
        }),
    );
    let payload = assert_ok(&result);
    assert_eq!(payload["durable"], true, "{payload}");
    assert_eq!(payload["process_bound"], false, "{payload}");
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
    assert_eq!(waited["affected_files"], json!([]));
    assert_eq!(waited["mutation_attributed"], false);
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
fn durable_wait_recovers_after_tool_context_reconstruction() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": [
                "-u",
                "-c",
                "import time; print('before-reconnect', flush=True); time.sleep(0.35); print('after-reconnect', flush=True)"
            ],
            "filesystem_scope": "workspace",
            "timeout_ms": 5_000,
            "yield_time_ms": 0,
            "durable": true
        }),
    );
    let payload = assert_ok(&result);
    assert_eq!(payload["durable"], true, "{payload}");
    assert_eq!(payload["process_bound"], false, "{payload}");
    let session_id = payload["session_id"]
        .as_str()
        .expect("durable session id")
        .to_string();

    drop(ctx);
    let recovered_ctx = ctx_for(&fx.root);
    let sessions = invoke(
        &recovered_ctx,
        "list_command_sessions",
        json!({"include_terminal": true, "max_output_bytes": 0}),
    );
    let sessions = assert_ok(&sessions);
    assert!(sessions["sessions"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["session_id"] == session_id)));

    let waited = invoke(
        &recovered_ctx,
        "wait_command",
        json!({
            "session_id": session_id,
            "timeout_ms": 3_000,
            "stdout_offset": 0,
            "stderr_offset": 0,
            "return_incremental_output": true
        }),
    );
    let waited = assert_ok(&waited);
    assert_eq!(waited["state"], "completed", "{waited}");
    assert_eq!(waited["durable"], true, "{waited}");
    let stdout = waited["stdout"]["content"].as_str().expect("stdout");
    assert!(stdout.contains("before-reconnect"), "{waited}");
    assert!(stdout.contains("after-reconnect"), "{waited}");
}

#[test]
fn durable_kill_works_after_tool_context_reconstruction() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let started = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": ["-u", "-c", "import time; print('durable-running', flush=True); time.sleep(10)"],
            "filesystem_scope": "workspace",
            "timeout_ms": 15_000,
            "yield_time_ms": 0,
            "durable": true
        }),
    );
    let started = assert_ok(&started);
    let session_id = started["session_id"]
        .as_str()
        .expect("durable session id")
        .to_string();
    drop(ctx);

    let recovered_ctx = ctx_for(&fx.root);
    let killed = invoke(
        &recovered_ctx,
        "kill_session",
        json!({
            "session_id": session_id,
            "signal": "KILL",
            "wait_ms": 3_000,
            "max_output_bytes": 4096
        }),
    );
    let killed = assert_err(&killed);
    assert_eq!(killed["killed"], true, "{killed}");
    assert_eq!(killed["status"], "killed", "{killed}");
    assert_eq!(killed["durable"], true, "{killed}");
    assert_eq!(killed["process_bound"], false, "{killed}");
    assert_eq!(killed["error"]["code"], "COMMAND_KILLED", "{killed}");
}

#[test]
fn durable_verification_finalizes_once_after_context_reconstruction() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let task = ctx
        .harness
        .start_task("durable verification finalization")
        .expect("start task");
    let started = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": TEST_PYTHON,
            "args": [
                "-u",
                "-c",
                "import time; print('durable-verification', flush=True); time.sleep(0.2)"
            ],
            "filesystem_scope": "workspace",
            "timeout_ms": 5_000,
            "yield_time_ms": 0,
            "durable": true,
            "verification_kind": "test",
            "verification_key": "durable-reconnect-verification",
            "verification_level": "required"
        }),
    );
    let started = assert_ok(&started);
    let session_id = started["session_id"]
        .as_str()
        .expect("durable session id")
        .to_string();
    drop(ctx);

    let recovered_ctx = ctx_for(&fx.root);
    let waited = invoke(
        &recovered_ctx,
        "wait_command",
        json!({"session_id": session_id, "timeout_ms": 3_000}),
    );
    let waited = assert_ok(&waited);
    assert_eq!(waited["execution_status"], "succeeded", "{waited}");
    assert!(waited["verification_id"].as_str().is_some(), "{waited}");

    let records = recovered_ctx
        .harness
        .list_verifications(&task.id)
        .expect("durable verification records");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        records[0].verification_key.as_deref(),
        Some("durable-reconnect-verification")
    );
    assert!(records[0].passed);

    let waited_again = invoke(
        &recovered_ctx,
        "wait_command",
        json!({"session_id": session_id, "timeout_ms": 0}),
    );
    assert_ok(&waited_again);
    let records_after = recovered_ctx
        .harness
        .list_verifications(&task.id)
        .expect("verification records after second wait");
    assert_eq!(
        records_after.len(),
        1,
        "repeated wait must not duplicate verification"
    );
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
    let payload = assert_err(&result);

    assert_eq!(payload["ok"], false);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["transport_status"], "ok");
    assert_eq!(payload["execution_status"], "failed");
    assert_eq!(payload["success"], false);
    assert_eq!(payload["command_ok"], false);
    assert_eq!(payload["status"], "exited");
    assert_eq!(payload["exit_code"], 1);
    assert_eq!(payload["error"]["code"], "COMMAND_EXIT_NONZERO");
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
    let killed = assert_err(&killed);
    assert_eq!(killed["status"], "killed");
    assert_eq!(killed["killed"], true);
    assert_eq!(killed["transport_ok"], true);
    assert_eq!(killed["transport_status"], "ok");
    assert_eq!(killed["execution_status"], "killed");
    assert_eq!(killed["success"], false);
    assert_eq!(killed["command_ok"], false);
    assert_eq!(killed["error"]["code"], "COMMAND_KILLED");
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
fn search_filters_by_glob() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let hit = invoke(
        &ctx,
        "search",
        json!({"query": "function add", "include_globs": ["**/*.js"], "max_results": 10}),
    );
    let hit_payload = assert_ok(&hit);
    assert!(hit_payload["data"]["total_matches"].as_u64().unwrap_or(0) > 0);

    let miss = invoke(
        &ctx,
        "search",
        json!({"query": "function add", "include_globs": ["**/*.py"]}),
    );
    let miss_payload = assert_ok(&miss);
    assert_eq!(
        miss_payload["data"]["total_matches"].as_u64().unwrap_or(1),
        0
    );
}

#[test]
fn legacy_text_search_names_route_without_catalog_exposure() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "search_text",
        json!({"query": "function add", "include_globs": ["**/*.js"]}),
    );
    let payload = assert_ok(&result);
    assert!(payload["total_matches"].as_u64().unwrap_or(0) > 0);

    let grep_result = invoke(
        &ctx,
        "grep",
        json!({"query": "function add", "include_globs": ["**/*.js"]}),
    );
    let grep_payload = assert_ok(&grep_result);
    assert!(grep_payload["total_matches"].as_u64().unwrap_or(0) > 0);

    let tools = anchor_lib::tools::list_tools_for_profile("core");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(names.contains("search"));
    assert!(!names.contains("grep"));
    assert!(!names.contains("search_text"));
}

#[test]
fn search_truncates_multibyte_preview_on_a_utf8_boundary() {
    let fx = tiny_js_fixture();
    fs::write(
        fx.root.join("src/multibyte.txt"),
        format!("marker {}\n", "连接正常".repeat(40)),
    )
    .expect("write multibyte fixture");
    let result = invoke(
        &ctx_for(&fx.root),
        "search",
        json!({
            "query": "marker",
            "path": "src/multibyte.txt",
            "max_preview_bytes": 64
        }),
    );
    let payload = assert_ok(&result);
    let preview = payload["data"]["matches"][0]["preview"]
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
    let payload = assert_err(&result);

    assert_eq!(payload["termination_reason"], "timeout");
    assert_eq!(payload["child_process"], true);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["transport_status"], "ok");
    assert_eq!(payload["execution_status"], "timed_out");
    assert_eq!(payload["success"], false);
    assert_eq!(payload["command_ok"], false);
    assert_eq!(payload["error"]["code"], "TIMEOUT");
    assert!(payload["suggestion"].is_string());
    assert!(payload["duration_ms"].is_u64());
    assert_eq!(payload["duration_ms"], payload["elapsed_ms"]);
    assert!(payload["warnings"].is_array());
}
