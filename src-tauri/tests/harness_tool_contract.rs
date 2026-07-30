use std::fs;
use std::process::Command;

use anchor_lib::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

#[test]
fn begin_work_session_binds_history_and_task_idempotently() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "initial\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace.clone(), temp.path().join("harness"))
        .expect("创建上下文");
    let arguments = json!({
        "objective": "统一工作会话测试",
        "session_key": "work-session-contract",
        "workspace_root": workspace.to_string_lossy()
    });

    let first = call_tool(&ctx, "begin_work_session", &arguments);
    assert_eq!(first["ok"], true);
    assert_eq!(first["work_session"]["status"], "active");
    assert_eq!(first["work_session"]["task_created"], true);
    assert_eq!(
        first["work_session"]["history_session_key"],
        "work-session-contract"
    );
    let task_id = first["work_session"]["task_id"]
        .as_str()
        .expect("task id")
        .to_string();
    let history_path = first["work_session"]["history_session_path"]
        .as_str()
        .expect("history path");
    assert!(workspace.join(history_path).exists());
    assert_eq!(first["task"]["history_session_key"], "work-session-contract");

    let second = call_tool(&ctx, "begin_work_session", &arguments);
    assert_eq!(second["ok"], true);
    assert_eq!(second["work_session"]["task_created"], false);
    assert_eq!(second["work_session"]["task_id"], task_id);
}

#[test]
fn close_work_session_closes_task_and_checkpoints_bound_history() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace.clone(), temp.path().join("harness"))
        .expect("创建上下文");
    let started = call_tool(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "关闭统一工作会话",
            "session_key": "close-work-session-contract",
            "workspace_root": workspace.to_string_lossy()
        }),
    );
    let task_id = started["work_session"]["task_id"]
        .as_str()
        .expect("task id");
    let expected_path = started["work_session"]["history_session_path"]
        .as_str()
        .expect("history path")
        .to_string();

    let closed = call_tool(
        &ctx,
        "close_work_session",
        &json!({
            "task_id": task_id,
            "allow_unverified": true,
            "session_status": "paused",
            "summary": "合约测试关闭",
            "checkpoint": {
                "findings": ["close_work_session contract passed"],
                "runtime_state": ["test_fixture=true"]
            }
        }),
    );
    assert_eq!(closed["ok"], true);
    assert_eq!(closed["work_session"]["closed"], true);
    assert_eq!(closed["work_session"]["status"], "paused");
    assert_eq!(closed["work_session"]["task_status"], "completed_unverified");
    assert_eq!(closed["checkpoint"]["session_key"], "close-work-session-contract");
    assert_eq!(closed["checkpoint"]["path"], expected_path);

    let retried = call_tool(
        &ctx,
        "close_work_session",
        &json!({
            "task_id": task_id,
            "allow_unverified": true,
            "session_status": "paused",
            "summary": "合约测试关闭"
        }),
    );
    assert_eq!(retried["ok"], true);
    assert_eq!(retried["finish"]["idempotent_retry"], true);
}

#[test]
fn operation_log_filters_and_collapses_bound_work_session_operations() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace.clone(), temp.path().join("harness"))
        .expect("创建上下文");
    let started = call_tool(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "作用域操作日志",
            "session_key": "operation-scope-contract",
            "workspace_root": workspace.to_string_lossy()
        }),
    );
    let task_id = started["work_session"]["task_id"]
        .as_str()
        .expect("task id");
    let executed = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "python --version", "yield_time_ms": 30000}),
    );
    assert_eq!(executed["command_ok"], true);

    let log = call_tool(
        &ctx,
        "operation_log",
        &json!({
            "task_id": task_id,
            "history_session_key": "operation-scope-contract",
            "tool": "exec_command",
            "collapse": true,
            "limit": 20
        }),
    );
    assert_eq!(log["ok"], true);
    assert_eq!(log["total_matches"], 1);
    assert_eq!(log["operations"].as_array().unwrap().len(), 1);
    let operation = &log["operations"][0];
    assert_eq!(operation["task_id"], task_id);
    assert_eq!(operation["history_session_key"], "operation-scope-contract");
    assert_eq!(operation["tool"], "exec_command");
    assert_eq!(operation["status"], "completed");
    assert!(operation["started_at_iso"]
        .as_str()
        .is_some_and(|value| value.ends_with('Z')));
    assert_eq!(log["summary"]["returned_operations"], 1);
}

#[test]
fn 无任务时仍可执行_dry_run_预检() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "dry_run": true,
            "patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+预检内容\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["preflight"], true);
    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(
        fs::read_to_string(temp.path().join("workspace/README.md")).unwrap(),
        "初始内容\n"
    );
}

#[test]
fn finish_task_rejects_uncommitted_business_changes() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(args)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.email", "anchor-tests@example.invalid"]);
    git(&["config", "user.name", "Anchor Tests"]);
    fs::write(workspace.join("main.txt"), "before\n").expect("写入文件");
    git(&["add", "main.txt"]);
    git(&["commit", "-m", "initial"]);

    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "提交前不得关闭"}));
    let task_id = started["task"]["id"].as_str().expect("任务 ID");
    let patched = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Update File: main.txt\n@@\n-before\n+after\n*** End Patch\n"
        }),
    );
    assert_eq!(patched["ok"], true);
    let verified = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "python -c \"print('verified')\"",
            "verification_kind": "test",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(verified["command_ok"], true);

    let finished = call_tool(&ctx, "finish_task", &json!({"task_id": task_id}));
    assert_eq!(finished["ok"], false);
    assert_eq!(finished["task_status"], "verifying");
    assert_eq!(finished["closed"], false);
    assert_eq!(finished["working_tree_files"], json!(["main.txt"]));
    assert!(finished["reason"].as_str().unwrap().contains("未提交"));
}

#[test]
fn successful_verification_retry_supersedes_previous_failure() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "验证重试"}));
    let task_id = started["task"]["id"].as_str().expect("任务 ID");
    let command =
        "python -c \"import pathlib,sys; sys.exit(0 if pathlib.Path('ok.flag').exists() else 1)\"";

    let failed = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": command, "verification_kind": "test", "yield_time_ms": 30000}),
    );
    assert_eq!(failed["command_ok"], false);
    fs::write(workspace.join("ok.flag"), "ready\n").expect("写入验证标记");
    let task = ctx.harness.current_task().unwrap().unwrap();
    ctx.harness
        .refresh_expected_state_for_operation(&task.id, Some("test-fixture"))
        .expect("refresh fixture change");
    let passed = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": command, "verification_kind": "test", "yield_time_ms": 30000}),
    );
    assert_eq!(passed["command_ok"], true);

    let finished = call_tool(&ctx, "finish_task", &json!({"task_id": task_id}));
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["verification_status"], "verified");
    assert_eq!(finished["task_status"], "completed");
}

#[test]
fn retained_git_commit_refreshes_expected_head_after_session_exit() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(args)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.email", "anchor-tests@example.invalid"]);
    git(&["config", "user.name", "Anchor Tests"]);
    fs::write(workspace.join("main.txt"), "before\n").expect("写入文件");
    git(&["add", "main.txt"]);
    git(&["commit", "-m", "initial"]);

    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "异步提交后继续任务"}),
    );
    let task_id = started["task"]["id"].as_str().expect("任务 ID");
    let patched = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Update File: main.txt\n@@\n-before\n+after\n*** End Patch\n"
        }),
    );
    assert_eq!(patched["ok"], true);

    let command = "python -c \"import subprocess,time; time.sleep(0.25); subprocess.check_call(['git','add','main.txt']); subprocess.check_call(['git','commit','-m','async commit'])\"";
    let launched = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": command,
            "yield_time_ms": 1,
            "timeout_ms": 10000
        }),
    );
    assert_eq!(launched["ok"], true);
    assert_eq!(launched["status"], "running");
    let session_id = launched["session_id"].as_str().expect("session ID");

    let completed = call_tool(
        &ctx,
        "write_stdin",
        &json!({"session_id": session_id, "chars": "", "yield_time_ms": 2000}),
    );
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["status"], "exited");
    assert_eq!(completed["exit_code"], 0);

    let current_head = Command::new("git")
        .arg("-C")
        .arg(&workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git head");
    let current_head = String::from_utf8_lossy(&current_head.stdout)
        .trim()
        .to_string();
    let status = call_tool(&ctx, "harness_status", &json!({}));
    assert_eq!(status["ok"], true);
    assert_eq!(status["task_id"], task_id);
    assert_eq!(status["baseline_matches"], true);
    assert_eq!(status["expected_head"], current_head);
    assert_eq!(status["head"], current_head);
}

#[test]
fn codex_patch格式支持新增文件dry_run和实际应用() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let patch = "*** Begin Patch\n*** Add File: probe.txt\n+probe-v2\n*** End Patch\n";

    let dry_run = call_tool(
        &ctx,
        "apply_patch",
        &json!({"patch": patch, "dry_run": true}),
    );
    assert_eq!(dry_run["ok"], true);
    assert_eq!(dry_run["dry_run"], true);
    assert!(dry_run["affected_files"]
        .as_array()
        .expect("影响文件")
        .iter()
        .any(|file| file["path"] == "probe.txt" && file["operation"] == "add"));
    assert!(!workspace.join("probe.txt").exists());

    let applied = call_tool(&ctx, "apply_patch", &json!({"patch": patch}));
    assert_eq!(applied["ok"], true);
    assert_eq!(
        fs::read_to_string(workspace.join("probe.txt")).expect("读取新增文件"),
        "probe-v2\n"
    );
}

#[test]
fn 无任务时普通_patch也可执行并保留撤销能力() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+已修改\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["harness_mode"], "standalone");
    assert!(!result
        .as_object()
        .unwrap()
        .contains_key("pre_change_snapshot_id"));
    assert_eq!(
        fs::read_to_string(workspace.join("README.md")).unwrap(),
        "已修改\n"
    );

    let log = call_tool(&ctx, "operation_log", &json!({}));
    assert_eq!(log["ok"], true);
    assert!(log["operations"]
        .as_array()
        .expect("操作日志")
        .iter()
        .any(|operation| operation["tool"] == "apply_patch"));
}

#[test]
fn 无任务时_exec_command不返回任务门禁错误() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "git status", "filesystem_scope": "workspace"}),
    );

    assert_ne!(result["error"]["code"], "TASK_STATE_REQUIRED");
    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(result["execution_mode"], "direct");
    assert_eq!(result["task_required"], false);
    assert_eq!(result["command"], "git status");
    assert_eq!(result["status"], "exited");
    assert!(result["exit_code"].is_i64() || result["exit_code"].is_u64());
    assert!(result["duration_ms"].is_u64());
    assert_eq!(result["duration_ms"], result["elapsed_ms"]);
    assert_eq!(result["next_actions"], json!([]));
    assert!(result["recovery_hint"].is_string());
    assert!(!result
        .as_object()
        .unwrap()
        .contains_key("pre_change_snapshot_id"));
}

#[test]
fn 无任务时_exec错误不应建议启动任务() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "python -c \"import sys; sys.exit(1)\""}),
    );

    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(result["task_required"], false);
    assert_eq!(result["next_actions"], json!([]));
    assert!(result["recovery_hint"].is_string());
    if let Some(actions) = result["harness"]["next_actions"].as_array() {
        assert!(!actions.iter().any(|action| action == "start_task"));
    }
}

#[test]
fn workspace_allows_exec_during_transition() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(&ctx, "exec_command", &json!({"cmd": "python --version"}));

    assert_ne!(result["error"]["code"], "EXEC_SANDBOX_UNAVAILABLE");
    assert_eq!(result["execution_mode"], "direct");
    assert_eq!(result["filesystem_scope"], "workspace");
    assert_eq!(result["sandbox_enforced"], false);
}

#[test]
fn harness_tools_support_task_lifecycle() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "补齐 Harness 状态"}),
    );
    assert_eq!(started["ok"], true);
    let task_id = started["task"]["id"].as_str().expect("任务 ID");

    let updated = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "pending_steps": ["接入门禁"]}),
    );
    assert_eq!(updated["ok"], true);
    let context = call_tool(&ctx, "task_context", &json!({}));
    assert_eq!(context["ok"], true);
    assert_eq!(context["task"]["id"], task_id);
    assert!(!context["events"].as_array().expect("事件").is_empty());

    let finished = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "allow_unverified": true}),
    );
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["task_status"], "completed_unverified");
    assert_eq!(finished["verification_status"], "unverified");
    assert_eq!(finished["closed"], true);
    assert_eq!(finished["session_status"], "paused");
    assert_eq!(finished["next_stage_started"], false);
    assert_eq!(finished["task"]["status"], "completed_unverified");
    assert!(finished["task"]["baseline"]["file_count"].is_u64());
    assert!(finished["task"]["baseline"].get("entries").is_none());
    assert!(finished["response_bytes"].as_u64().unwrap() <= 32 * 1024);
    let serialized_bytes = serde_json::to_vec(&finished).unwrap().len();
    assert!(serialized_bytes <= 32 * 1024);
    assert_eq!(finished["response_bytes"], serialized_bytes);
}

#[test]
fn finish_task_requires_structured_verification_and_then_closes_atomically() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "验证后原子关闭任务"}),
    );
    let task_id = started["task"]["id"].as_str().expect("任务 ID");

    let blocked = call_tool(&ctx, "finish_task", &json!({"task_id": task_id}));
    assert_eq!(blocked["ok"], false);
    assert_eq!(blocked["task_status"], "verifying");
    assert_eq!(blocked["verification_status"], "missing");
    assert_eq!(blocked["closed"], false);
    assert_eq!(blocked["session_status"], "active");
    assert!(blocked["reason"].is_string());

    let verified = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "python -c \"print('lint-ok')\"",
            "verification_kind": "lint",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(verified["ok"], true);
    assert_eq!(verified["command_ok"], true);
    assert_eq!(verified["verification"]["kind"], "lint");
    assert_eq!(verified["verification"]["status"], "passed");

    let finished = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "session_status": "paused"}),
    );
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["task_status"], "completed");
    assert_eq!(finished["verification_status"], "verified");
    assert_eq!(finished["closed"], true);
    assert_eq!(finished["session_status"], "paused");
    assert_eq!(finished["next_stage_started"], false);
    assert_eq!(
        finished["change_summary"]["verification"][0]["kind"],
        "lint"
    );
    assert_eq!(
        finished["change_summary"]["verification"][0]["status"],
        "passed"
    );
    assert!(finished["response_bytes"].as_u64().unwrap() <= 32 * 1024);
    let serialized_bytes = serde_json::to_vec(&finished).unwrap().len();
    assert!(serialized_bytes <= 32 * 1024);
    assert_eq!(finished["response_bytes"], serialized_bytes);

    let status = call_tool(&ctx, "harness_status", &json!({}));
    assert_eq!(status["ok"], true);
    assert_eq!(status["task_id"], Value::Null);
    assert_eq!(status["session_status"], "paused");
    assert_eq!(status["next_stage_started"], false);
}

#[test]
fn finish_task_hard_limits_extreme_response_size() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let oversized_objective = "超大任务说明".repeat(20_000);
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": oversized_objective}),
    );
    let task_id = started["task"]["id"].as_str().expect("任务 ID");

    let finished = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "allow_unverified": true}),
    );
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["truncated"], true);
    assert!(finished["response_bytes"].as_u64().unwrap() <= 32 * 1024);
    let serialized_bytes = serde_json::to_vec(&finished).unwrap().len();
    assert!(serialized_bytes <= 32 * 1024);
    assert_eq!(finished["response_bytes"], serialized_bytes);
    assert_eq!(finished["details_tool"]["name"], "change_summary");
}

#[test]
fn 外部修改会在写工具执行前被拒绝() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "检查外部变化"}));
    let task_id = started["task"]["id"].as_str().expect("任务 ID");
    fs::write(workspace.join("README.md"), "外部修改\n").expect("模拟外部修改");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "git status", "filesystem_scope": "workspace"}),
    );

    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "FILE_CHANGED_EXTERNALLY");
    assert_eq!(
        ctx.harness
            .current_task()
            .expect("读取任务")
            .expect("活动任务")
            .id,
        task_id
    );
}

#[test]
fn 工具清单包含项目状态和任务上下文能力() {
    let tools = anchor_lib::tools::list_tools_for_profile("advanced");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        "project_state",
        "start_task",
        "task_context",
        "list_task_events",
        "change_summary",
    ] {
        assert!(names.contains(&expected), "缺少工具 {expected}");
    }
    assert!(!names.contains(&"undo_last_patch"));
}
