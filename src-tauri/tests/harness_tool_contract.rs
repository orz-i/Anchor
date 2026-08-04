use std::fs;
use std::process::Command;

use anchor_lib::tools::{call_tool, call_tool_for_session, ToolContext};
use serde_json::{json, Value};

#[cfg(windows)]
const TEST_PYTHON: &str = "python";
#[cfg(not(windows))]
const TEST_PYTHON: &str = "python3";

fn initialize_git(root: &std::path::Path) {
    for args in [
        vec!["init"],
        vec!["config", "user.email", "anchor@example.invalid"],
        vec!["config", "user.name", "Anchor Tests"],
        vec!["add", "."],
        vec!["commit", "--no-gpg-sign", "--no-verify", "-m", "initial"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("运行 git");
        assert!(
            output.status.success(),
            "git 初始化失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn begin_work_session_binds_history_and_task_idempotently() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "initial\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let arguments = json!({
        "objective": "统一工作会话测试",
        "session_key": "work-session-contract",
        "workspace_root": workspace.to_string_lossy()
    });
    let mcp_session = "work-session-a";

    let first = call_tool_for_session(&ctx, "begin_work_session", &arguments, mcp_session);
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
    assert_eq!(
        first["task"]["history_session_key"],
        "work-session-contract"
    );

    let second = call_tool_for_session(&ctx, "begin_work_session", &arguments, mcp_session);
    assert_eq!(second["ok"], true);
    assert_eq!(second["work_session"]["task_created"], false);
    assert_eq!(second["work_session"]["task_id"], task_id);

    let paused = call_tool_for_session(
        &ctx,
        "pause_task",
        &json!({"task_id": task_id}),
        mcp_session,
    );
    assert_eq!(paused["task"]["status"], "paused");
    assert_eq!(
        call_tool_for_session(&ctx, "harness_status", &json!({}), mcp_session)["task_state"],
        "paused"
    );
    let reconnected = call_tool_for_session(&ctx, "begin_work_session", &arguments, mcp_session);
    assert_eq!(reconnected["ok"], true);
    assert_eq!(reconnected["work_session"]["task_created"], false);
    assert_eq!(reconnected["work_session"]["task_id"], task_id);
    assert_eq!(reconnected["task"]["status"], "active");
    assert_eq!(reconnected["harness"]["task_state"], "active");
}

#[test]
fn latest_baseline_is_captured_and_accepted_in_one_call() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("main.txt"), "one\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "原子接受基线"}));
    let task_id = started["task"]["id"].as_str().expect("task id");
    fs::write(workspace.join("main.txt"), "two\n").expect("外部修改");
    assert_eq!(
        call_tool(&ctx, "harness_status", &json!({}))["baseline_matches"],
        false
    );

    let accepted = call_tool(
        &ctx,
        "accept_latest_baseline",
        &json!({
            "task_id": task_id,
            "reason": "接受已审阅的并发修改",
            "max_attempts": 3
        }),
    );

    assert_eq!(accepted["ok"], true);
    assert_eq!(accepted["accepted"], true);
    assert!(accepted["attempts"]
        .as_u64()
        .is_some_and(|attempts| attempts >= 1));
    assert_eq!(accepted["harness"]["baseline_matches"], true);
    assert_eq!(
        accepted["accepted_state"]["worktree_fingerprint"],
        accepted["harness"]["expected_fingerprint"]
    );
}

#[test]
fn begin_work_session_can_handoff_and_switch_between_tasks() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "handoff\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let mcp_session = "handoff-session";
    let first = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "任务 A",
            "session_key": "task-handoff-contract",
            "workspace_root": workspace.to_string_lossy()
        }),
        mcp_session,
    );
    assert_eq!(first["ok"], true);
    let first_id = first["task"]["id"].as_str().expect("first id").to_string();

    let second = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "任务 B",
            "session_key": "task-handoff-contract",
            "workspace_root": workspace.to_string_lossy(),
            "pause_current_and_start": true
        }),
        mcp_session,
    );
    assert_eq!(second["ok"], true);
    assert_eq!(second["work_session"]["previous_task_id"], first_id);
    let second_id = second["task"]["id"]
        .as_str()
        .expect("second id")
        .to_string();
    assert_ne!(second_id, first_id);
    assert_eq!(
        ctx.harness.task(&first_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Paused
    );
    assert_eq!(second["harness"]["task_id"], second_id);

    let switched = call_tool_for_session(
        &ctx,
        "switch_task",
        &json!({"task_id": first_id}),
        mcp_session,
    );
    assert_eq!(switched["ok"], true);
    assert_eq!(switched["task"]["id"], first_id);
    assert_eq!(switched["task"]["status"], "active");
    assert_eq!(
        ctx.harness.task(&second_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Paused
    );
    assert_eq!(switched["harness"]["active_task_count"], 1);
}

#[test]
fn sessions_keep_distinct_tasks_and_transfer_the_single_writer_lease() {
    use std::sync::{Arc, Barrier};

    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "parallel\n").expect("写入文件");
    let ctx = Arc::new(
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文"),
    );

    let first = call_tool_for_session(
        ctx.as_ref(),
        "start_task",
        &json!({"objective": "并行任务 A"}),
        "parallel-session-a",
    );
    let second = call_tool_for_session(
        ctx.as_ref(),
        "start_task",
        &json!({"objective": "并行任务 B"}),
        "parallel-session-b",
    );
    let first_id = first["task"]["id"].as_str().expect("first id").to_string();
    let second_id = second["task"]["id"]
        .as_str()
        .expect("second id")
        .to_string();
    assert_ne!(first_id, second_id);

    let first_status = call_tool_for_session(
        ctx.as_ref(),
        "harness_status",
        &json!({}),
        "parallel-session-a",
    );
    let second_status = call_tool_for_session(
        ctx.as_ref(),
        "harness_status",
        &json!({}),
        "parallel-session-b",
    );
    assert_eq!(first_status["task_id"], first_id);
    assert_eq!(second_status["task_id"], second_id);
    assert_eq!(first_status["active_task_count"], 1);
    assert_eq!(second_status["active_task_count"], 1);
    assert_eq!(
        ctx.harness.task(&first_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Paused
    );
    assert_eq!(
        ctx.harness.task(&second_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Active
    );
    let project = call_tool_for_session(
        ctx.as_ref(),
        "project_state",
        &json!({"max_files": 20}),
        "parallel-session-a",
    );
    assert_eq!(project["selected_task_id"], first_id);
    assert_eq!(project["task_count"], 2);
    assert_eq!(project["tasks"].as_array().map(Vec::len), Some(2));

    let barrier = Arc::new(Barrier::new(3));
    let first_worker = {
        let ctx = Arc::clone(&ctx);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            call_tool_for_session(
                ctx.as_ref(),
                "apply_patch",
                &json!({
                    "patch": "*** Begin Patch\n*** Add File: task-a.txt\n+task-a\n*** End Patch\n"
                }),
                "parallel-session-a",
            )
        })
    };
    let second_worker = {
        let ctx = Arc::clone(&ctx);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            call_tool_for_session(
                ctx.as_ref(),
                "apply_patch",
                &json!({
                    "patch": "*** Begin Patch\n*** Add File: task-b.txt\n+task-b\n*** End Patch\n"
                }),
                "parallel-session-b",
            )
        })
    };
    barrier.wait();
    let first_write = first_worker.join().expect("first worker");
    let second_write = second_worker.join().expect("second worker");
    assert_eq!(first_write["ok"], true, "{first_write}");
    assert_eq!(second_write["ok"], true, "{second_write}");
    assert!(workspace.join("task-a.txt").exists());
    assert!(workspace.join("task-b.txt").exists());

    let first_after = call_tool_for_session(
        ctx.as_ref(),
        "harness_status",
        &json!({}),
        "parallel-session-a",
    );
    let second_after = call_tool_for_session(
        ctx.as_ref(),
        "harness_status",
        &json!({}),
        "parallel-session-b",
    );
    assert_eq!(first_after["baseline_matches"], true);
    assert_eq!(second_after["baseline_matches"], true);
    assert_eq!(first_after["active_task_count"], 1);
    assert_eq!(second_after["active_task_count"], 1);
    assert_eq!(ctx.harness.active_tasks().unwrap().len(), 1);

    let first_operations = call_tool(
        ctx.as_ref(),
        "operation_log",
        &json!({"task_id": first_id, "tool": "apply_patch", "collapse": true}),
    );
    let second_operations = call_tool(
        ctx.as_ref(),
        "operation_log",
        &json!({"task_id": second_id, "tool": "apply_patch", "collapse": true}),
    );
    assert_eq!(first_operations["total_matches"], 1);
    assert_eq!(second_operations["total_matches"], 1);
}

#[test]
fn unique_active_writer_is_rebound_but_ambiguous_paused_tasks_fail_closed() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "binding gate\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let first = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "绑定任务 A"}),
        "binding-session-a",
    );
    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "绑定任务 B"}),
        "binding-session-b",
    );
    assert_eq!(first["ok"], true);
    assert_eq!(second["ok"], true);
    assert_eq!(ctx.harness.active_tasks().unwrap().len(), 1);

    let rebound = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: rebound.txt\n+rebound\n*** End Patch\n"
        }),
        "unbound-third-session",
    );
    assert_eq!(rebound["ok"], true, "{rebound}");
    assert!(workspace.join("rebound.txt").exists());

    let paused = call_tool_for_session(
        &ctx,
        "pause_task",
        &json!({"task_id": second["task"]["id"]}),
        "binding-session-b",
    );
    assert_eq!(paused["ok"], true, "{paused}");
    let rejected = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: forbidden.txt\n+forbidden\n*** End Patch\n"
        }),
        "ambiguous-fourth-session",
    );
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "TASK_BINDING_REQUIRED");
    assert!(!workspace.join("forbidden.txt").exists());
}

#[test]
fn running_command_blocks_writer_lease_transfer_to_another_task() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "writer busy\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let first = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "持有写租约的任务"}),
        "writer-busy-session-a",
    );
    assert_eq!(first["ok"], true, "{first}");
    let command = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "executable": if cfg!(windows) { "python" } else { "python3" },
            "args": ["-c", "import time; time.sleep(2)"],
            "yield_time_ms": 0,
            "timeout_ms": 10_000
        }),
        "writer-busy-session-a",
    );
    assert_eq!(command["ok"], true, "{command}");
    assert_eq!(command["status"], "running", "{command}");
    let command_session_id = command["session_id"].as_str().expect("command session");

    let rejected = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "竞争写租约的任务"}),
        "writer-busy-session-b",
    );
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "WORKSPACE_WRITER_BUSY");

    let killed = call_tool_for_session(
        &ctx,
        "kill_session",
        &json!({"session_id": command_session_id}),
        "writer-busy-session-a",
    );
    assert_eq!(killed["ok"], false, "{killed}");
    assert_eq!(killed["killed"], true, "{killed}");
    assert_eq!(killed["execution_status"], "killed", "{killed}");

    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "竞争写租约的任务"}),
        "writer-busy-session-b",
    );
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(ctx.harness.active_tasks().unwrap().len(), 1);
}

#[test]
fn failed_controlled_command_with_mutation_returns_the_refreshed_baseline() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "controlled mutation\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let caller_session = "failed-mutation-session";
    let started = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "失败命令仍归因文件变更"}),
        caller_session,
    );
    assert_eq!(started["ok"], true, "{started}");

    let script =
        "from pathlib import Path; Path('changed.txt').write_text('changed'); raise SystemExit(1)";
    let failed = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "executable": if cfg!(windows) { "python" } else { "python3" },
            "args": ["-c", script]
        }),
        caller_session,
    );
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["mutation_attributed"], true, "{failed}");
    assert_eq!(failed["harness"]["baseline_matches"], true, "{failed}");
    assert_eq!(failed["harness"]["writable"], true, "{failed}");
    assert!(workspace.join("changed.txt").exists());

    let follow_up = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: follow-up.txt\n+ok\n*** End Patch\n"
        }),
        caller_session,
    );
    assert_eq!(follow_up["ok"], true, "{follow_up}");
}

#[test]
fn history_checkpoint_cannot_complete_an_active_bound_task() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "completion gate\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let started = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "必须通过关闭入口完成",
            "session_key": "history-completion-gate",
            "workspace_root": workspace.to_string_lossy()
        }),
        "history-completion-session",
    );
    assert_eq!(started["ok"], true, "{started}");
    let session_key = started["history"]["session_key"]
        .as_str()
        .expect("session key");
    let expected_path = started["history"]["current_path"]
        .as_str()
        .expect("history path");

    let rejected = call_tool_for_session(
        &ctx,
        "history_session_checkpoint",
        &json!({
            "session_key": session_key,
            "expected_path": expected_path,
            "turn_id": "illegal-completion",
            "user_intent": "绕过关闭流程",
            "session_status": "completed"
        }),
        "history-completion-session",
    );
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "HISTORY_TASK_STILL_ACTIVE");
}

#[test]
fn work_session_export_is_versioned_portable_and_overwrite_safe() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "portable handoff\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let caller_session = "handoff-export-session";
    let started = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "导出可移植交接",
            "session_key": "handoff-export-history",
            "workspace_root": workspace.to_string_lossy()
        }),
        caller_session,
    );
    assert_eq!(started["ok"], true, "{started}");
    let task_id = started["task"]["id"].as_str().expect("task id");
    let export_path = ".anchor/handoffs/portable.json";

    let exported = call_tool_for_session(
        &ctx,
        "export_work_session",
        &json!({"path": export_path}),
        caller_session,
    );
    assert_eq!(exported["ok"], true, "{exported}");
    assert_eq!(exported["format"], "anchor.work-session-handoff");
    assert_eq!(exported["schema_version"], 1);
    assert_eq!(exported["task_id"], task_id);
    assert_eq!(exported["resume_strategy"], "begin_work_session");
    assert_eq!(exported["git_ignored_recommended"], true);
    assert!(exported["content_bytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0));
    assert_eq!(exported["content_hash"].as_str().map(str::len), Some(64));

    let bytes = fs::read(workspace.join(export_path)).expect("读取导出文件");
    let document: serde_json::Value = serde_json::from_slice(&bytes).expect("解析导出 JSON");
    assert_eq!(document["format"], "anchor.work-session-handoff");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["plugin"]["catalog_version"], 22);
    assert_eq!(document["task"]["id"], task_id);
    assert_eq!(
        document["history_session"]["session_key"],
        "handoff-export-history"
    );
    assert!(document["commits"].is_array());
    assert!(document["verifications"].is_array());
    assert!(document["remaining_issues"].is_array());
    assert_eq!(document["resume"]["strategy"], "begin_work_session");

    let duplicate = call_tool_for_session(
        &ctx,
        "export_work_session",
        &json!({"path": export_path}),
        caller_session,
    );
    assert_eq!(duplicate["ok"], false, "{duplicate}");
    assert_eq!(duplicate["error"]["code"], "HANDOFF_EXPORT_EXISTS");

    let overwritten = call_tool_for_session(
        &ctx,
        "export_work_session",
        &json!({"path": export_path, "overwrite": true}),
        caller_session,
    );
    assert_eq!(overwritten["ok"], true, "{overwritten}");
}

#[test]
fn worktree_mode_is_optional_and_routes_task_operations_without_touching_primary_checkout() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "optional worktree\n").expect("写入文件");
    fs::write(
        workspace.join(".gitignore"),
        "/.anchor/worktrees/\n/.anchor/handoffs/\n/docs/history-session/\n",
    )
    .expect("写入 ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let shared = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "默认共享工作区"}),
        "optional-shared-session",
    );
    assert_eq!(shared["ok"], true, "{shared}");
    assert_eq!(shared["workspace_mode"], "shared");
    assert!(shared["git_worktree"].is_null());
    let shared_task_id = shared["task"]["id"].as_str().expect("shared task id");
    let shared_branch = shared["task"]["expected_state"]["branch"]
        .as_str()
        .expect("shared branch")
        .to_string();

    let started = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "显式隔离工作区",
            "session_key": "optional-worktree-history",
            "workspace_root": workspace.to_string_lossy(),
            "workspace_mode": "worktree",
            "worktree_base_ref": "HEAD"
        }),
        "optional-worktree-session",
    );
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["work_session"]["workspace_mode"], "worktree");
    assert_eq!(started["work_session"]["parallel"], true);
    assert_eq!(started["harness"]["baseline_matches"], true);
    let task_id = started["task"]["id"].as_str().expect("task id");
    let worktree_path = std::path::PathBuf::from(
        started["task"]["git_worktree"]["path"]
            .as_str()
            .expect("worktree path"),
    );
    assert!(worktree_path.is_dir());
    assert!(worktree_path.ends_with(task_id));
    assert_eq!(
        ctx.harness
            .task(shared_task_id)
            .expect("shared after worktree create")
            .expected_state
            .branch
            .as_deref(),
        Some(shared_branch.as_str())
    );

    let patched = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: isolated.txt\n+isolated\n*** End Patch\n"
        }),
        "optional-worktree-session",
    );
    assert_eq!(patched["ok"], true, "{patched}");
    assert!(worktree_path.join("isolated.txt").exists());
    assert!(!workspace.join("isolated.txt").exists());
    assert_eq!(
        ctx.harness
            .task(shared_task_id)
            .expect("shared after isolated patch")
            .expected_state
            .branch
            .as_deref(),
        Some(shared_branch.as_str())
    );

    let status = call_tool_for_session(&ctx, "git_status", &json!({}), "optional-worktree-session");
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["branch"], format!("anchor/task/{task_id}"));
    assert_eq!(status["clean"], false);
    assert_eq!(
        ctx.harness
            .task(shared_task_id)
            .expect("shared after worktree status")
            .expected_state
            .branch
            .as_deref(),
        Some(shared_branch.as_str())
    );

    let primary_status = Command::new("git")
        .arg("-C")
        .arg(&workspace)
        .args(["status", "--porcelain=v1"])
        .output()
        .expect("读取主工作区状态");
    assert!(primary_status.status.success());
    let primary_status = String::from_utf8_lossy(&primary_status.stdout);
    assert!(!primary_status.contains("isolated.txt"), "{primary_status}");
    assert!(
        !primary_status.contains(".anchor/worktrees"),
        "{primary_status}"
    );

    let exported = call_tool_for_session(
        &ctx,
        "export_work_session",
        &json!({"path": ".anchor/handoffs/worktree.json"}),
        "optional-worktree-session",
    );
    assert_eq!(exported["ok"], true, "{exported}");
    let handoff: Value = serde_json::from_slice(
        &fs::read(workspace.join(".anchor/handoffs/worktree.json")).expect("读取交接"),
    )
    .expect("解析交接");
    assert_eq!(handoff["workspace"]["mode"], "worktree");
    assert_eq!(
        handoff["workspace"]["execution_path"],
        worktree_path.to_string_lossy().as_ref()
    );
    assert_eq!(
        ctx.harness
            .task(shared_task_id)
            .expect("shared after export")
            .expected_state
            .branch
            .as_deref(),
        Some(shared_branch.as_str())
    );

    let remove_in_use = call_tool_for_session(
        &ctx,
        "git_worktree_remove",
        &json!({"path": format!(".anchor/worktrees/{task_id}")}),
        "optional-worktree-session",
    );
    assert_eq!(remove_in_use["ok"], false, "{remove_in_use}");
    assert_eq!(remove_in_use["error"]["code"], "GIT_WORKTREE_IN_USE");
    assert_eq!(
        ctx.harness
            .task(shared_task_id)
            .expect("shared before switch")
            .expected_state
            .branch
            .as_deref(),
        Some(shared_branch.as_str())
    );

    let switched = call_tool_for_session(
        &ctx,
        "switch_task",
        &json!({"task_id": shared_task_id}),
        "optional-worktree-session",
    );
    assert_eq!(switched["ok"], true, "{switched}");
    assert_eq!(switched["workspace_mode"], "shared");
    assert_eq!(switched["task"]["id"], shared_task_id);
    assert!(switched["task"]["git_worktree"].is_null());
    assert_eq!(
        ctx.bound_task_for_session(Some("optional-worktree-session"))
            .expect("bound shared task")
            .id,
        shared_task_id
    );
    assert!(ctx
        .harness
        .task(shared_task_id)
        .expect("persisted shared task")
        .git_worktree
        .is_none());
    assert_eq!(
        ctx.harness
            .task(shared_task_id)
            .expect("shared after switch")
            .expected_state
            .branch
            .as_deref(),
        Some(shared_branch.as_str())
    );
    let cwd = call_tool_for_session(
        &ctx,
        "get_default_cwd",
        &json!({}),
        "optional-worktree-session",
    );
    assert_eq!(cwd["ok"], true, "{cwd}");
    assert_eq!(cwd["default_cwd"], ".");
    assert_eq!(
        ctx.harness
            .task(shared_task_id)
            .expect("shared after cwd")
            .expected_state
            .branch
            .as_deref(),
        Some(shared_branch.as_str())
    );
    let shared_patch = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: shared-only.txt\n+shared\n*** End Patch\n"
        }),
        "optional-worktree-session",
    );
    assert_eq!(shared_patch["ok"], true, "{shared_patch}");
    assert!(workspace.join("shared-only.txt").exists());
    assert!(!worktree_path.join("shared-only.txt").exists());
}

#[test]
fn independent_worktree_tasks_remain_active_and_do_not_share_running_command_leases() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "parallel worktrees\n").expect("写入文件");
    fs::write(workspace.join(".gitignore"), "/.anchor/worktrees/\n").expect("写入 ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let first = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "隔离任务 A", "workspace_mode": "worktree"}),
        "worktree-parallel-a",
    );
    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "隔离任务 B", "workspace_mode": "worktree"}),
        "worktree-parallel-b",
    );
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(ctx.harness.active_tasks().expect("active tasks").len(), 2);

    let first_path = std::path::PathBuf::from(
        first["task"]["git_worktree"]["path"]
            .as_str()
            .expect("first path"),
    );
    let second_path = std::path::PathBuf::from(
        second["task"]["git_worktree"]["path"]
            .as_str()
            .expect("second path"),
    );
    assert_ne!(first_path, second_path);

    let running = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "import time; time.sleep(2)"],
            "yield_time_ms": 0,
            "timeout_ms": 10_000
        }),
        "worktree-parallel-a",
    );
    assert_eq!(running["ok"], true, "{running}");
    assert_eq!(running["status"], "running", "{running}");

    let second_write = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: task-b.txt\n+B\n*** End Patch\n"
        }),
        "worktree-parallel-b",
    );
    assert_eq!(second_write["ok"], true, "{second_write}");
    assert!(second_path.join("task-b.txt").exists());
    assert!(!first_path.join("task-b.txt").exists());

    let killed = call_tool_for_session(
        &ctx,
        "kill_session",
        &json!({"session_id": running["session_id"]}),
        "worktree-parallel-observer",
    );
    assert_eq!(killed["killed"], true, "{killed}");
}

#[test]
fn close_work_session_can_remove_a_clean_managed_worktree_when_explicitly_requested() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "cleanup worktree\n").expect("写入文件");
    fs::write(workspace.join(".gitignore"), "/.anchor/worktrees/\n").expect("写入 ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let caller = "worktree-cleanup-session";

    let started = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "关闭后清理隔离目录",
            "session_key": "worktree-cleanup-history",
            "workspace_root": workspace.to_string_lossy(),
            "workspace_mode": "worktree",
            "worktree_remove_on_close": true
        }),
        caller,
    );
    assert_eq!(started["ok"], true, "{started}");
    let task_id = started["task"]["id"].as_str().expect("task id");
    let worktree_path = std::path::PathBuf::from(
        started["task"]["git_worktree"]["path"]
            .as_str()
            .expect("worktree path"),
    );

    let patched = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: committed.txt\n+committed\n*** End Patch\n"
        }),
        caller,
    );
    assert_eq!(patched["ok"], true, "{patched}");
    assert_eq!(
        call_tool_for_session(
            &ctx,
            "git_stage",
            &json!({"paths": ["committed.txt"]}),
            caller,
        )["ok"],
        true
    );
    let committed = call_tool_for_session(
        &ctx,
        "git_commit",
        &json!({"message": "test: commit isolated worktree"}),
        caller,
    );
    assert_eq!(committed["ok"], true, "{committed}");

    let verified = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "git status --porcelain=v1",
            "verification_kind": "test",
            "verification_key": "worktree-clean-close",
            "verification_level": "blocking"
        }),
        caller,
    );
    assert_eq!(verified["ok"], true, "{verified}");

    let closed = call_tool_for_session(
        &ctx,
        "close_work_session",
        &json!({
            "task_id": task_id,
            "session_status": "completed",
            "summary": "worktree cleanup test"
        }),
        caller,
    );
    assert_eq!(closed["ok"], true, "{closed}");
    assert_eq!(closed["worktree_cleanup"]["requested"], true);
    assert_eq!(closed["worktree_cleanup"]["removed"], true);
    assert!(!worktree_path.exists());
    assert_eq!(
        ctx.harness.task(task_id).expect("task").status,
        anchor_lib::harness::TaskStatus::Completed
    );
}

#[test]
fn direct_worktree_tools_create_list_remove_and_prune_only_managed_paths() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "manual worktree\n").expect("写入文件");
    fs::write(workspace.join(".gitignore"), "/.anchor/worktrees/\n").expect("写入 ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let created = call_tool(
        &ctx,
        "git_worktree_create",
        &json!({"name": "manual_test", "base_ref": "HEAD"}),
    );
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["path"], ".anchor/worktrees/manual_test");
    assert!(
        std::path::Path::new(created["absolute_path"].as_str().expect("absolute path")).is_dir()
    );

    let listed = call_tool(&ctx, "git_worktree_list", &json!({}));
    assert_eq!(listed["ok"], true, "{listed}");
    assert_eq!(listed["count"], 2);
    assert!(listed["worktrees"].as_array().is_some_and(|worktrees| {
        worktrees.iter().any(|entry| {
            entry["managed"] == true && entry["managed_path"] == ".anchor/worktrees/manual_test"
        })
    }));

    let outside = call_tool(&ctx, "git_worktree_remove", &json!({"path": "."}));
    assert_eq!(outside["ok"], false, "{outside}");
    assert_eq!(outside["error"]["code"], "GIT_WORKTREE_PATH_NOT_MANAGED");

    let removed = call_tool(
        &ctx,
        "git_worktree_remove",
        &json!({"path": ".anchor/worktrees/manual_test"}),
    );
    assert_eq!(removed["ok"], true, "{removed}");
    let pruned = call_tool(&ctx, "git_worktree_prune", &json!({}));
    assert_eq!(pruned["ok"], true, "{pruned}");
    assert_eq!(pruned["remaining_count"], 1);
}

#[test]
fn parallel_task_can_finish_while_peer_owned_changes_remain_dirty() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "parallel finish\n").expect("写入文件");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let first = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "可独立关闭的任务"}),
        "finish-session-a",
    );
    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "保留脏改动的任务"}),
        "finish-session-b",
    );
    let first_id = first["task"]["id"].as_str().expect("first id");
    let second_id = second["task"]["id"].as_str().expect("second id");

    let peer_write = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: peer-owned.txt\n+peer\n*** End Patch\n"
        }),
        "finish-session-b",
    );
    assert_eq!(peer_write["ok"], true, "{peer_write}");

    let finished = call_tool_for_session(
        &ctx,
        "finish_task",
        &json!({
            "task_id": first_id,
            "allow_unverified": true,
            "session_status": "paused"
        }),
        "finish-session-a",
    );
    assert_eq!(finished["ok"], true, "{finished}");
    assert_eq!(finished["task_status"], "completed_unverified");
    assert_eq!(finished["session_status"], "active");
    assert_eq!(finished["requested_session_status"], "paused");
    assert_eq!(finished["change_summary"]["working_tree_files"], json!([]));
    assert_eq!(
        finished["change_summary"]["peer_working_tree_files"],
        json!(["peer-owned.txt"])
    );
    assert_eq!(
        ctx.harness.task(second_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Active
    );
    assert!(workspace.join("peer-owned.txt").exists());

    let first_status =
        call_tool_for_session(&ctx, "harness_status", &json!({}), "finish-session-a");
    let second_status =
        call_tool_for_session(&ctx, "harness_status", &json!({}), "finish-session-b");
    assert!(first_status["task_id"].is_null());
    assert_eq!(second_status["task_id"], second_id);
    assert_eq!(second_status["baseline_matches"], true);
}

#[test]
fn sequential_retained_commands_keep_verifications_bound_to_their_sessions() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "parallel commands\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let first = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "命令任务 A"}),
        "command-session-a",
    );
    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "命令任务 B"}),
        "command-session-b",
    );
    let first_id = first["task"]["id"].as_str().expect("first id").to_string();
    let second_id = second["task"]["id"]
        .as_str()
        .expect("second id")
        .to_string();

    let first_started = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(0.2); print('task-a')\""),
            "yield_time_ms": 0,
            "timeout_ms": 5_000,
            "verification_kind": "test",
            "verification_key": "parallel-command-a",
            "verification_level": "required"
        }),
        "command-session-a",
    );
    let first_session = first_started["session_id"]
        .as_str()
        .expect("first command session");

    let first_waited = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": first_session, "timeout_ms": 5_000}),
        "command-session-a",
    );
    assert_eq!(first_waited["command_ok"], true, "{first_waited}");

    let second_started = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(0.2); print('task-b')\""),
            "yield_time_ms": 0,
            "timeout_ms": 5_000,
            "verification_kind": "test",
            "verification_key": "parallel-command-b",
            "verification_level": "required"
        }),
        "command-session-b",
    );
    let second_session = second_started["session_id"]
        .as_str()
        .expect("second command session");
    assert_ne!(first_session, second_session);
    let second_waited = call_tool_for_session(
        &ctx,
        "wait_command",
        &json!({"session_id": second_session, "timeout_ms": 5_000}),
        "command-session-b",
    );
    assert_eq!(second_waited["command_ok"], true, "{second_waited}");

    let first_verifications = ctx
        .harness
        .list_verifications(&first_id)
        .expect("first verifications");
    let second_verifications = ctx
        .harness
        .list_verifications(&second_id)
        .expect("second verifications");
    assert_eq!(first_verifications.len(), 1);
    assert_eq!(second_verifications.len(), 1);
    assert_eq!(
        first_verifications[0].verification_key.as_deref(),
        Some("parallel-command-a")
    );
    assert_eq!(
        second_verifications[0].verification_key.as_deref(),
        Some("parallel-command-b")
    );
}

#[test]
fn reconnected_transport_recovers_task_by_history_session_not_workspace_default() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "parallel reconnect\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let first_args = json!({
        "objective": "可恢复并行任务 A",
        "session_key": "parallel-history-a",
        "workspace_root": workspace.to_string_lossy()
    });
    let first = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &first_args,
        "transport-a-original",
    );
    let first_id = first["task"]["id"].as_str().expect("first id").to_string();

    let second = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "并行任务 B 成为工作区默认",
            "session_key": "parallel-history-b",
            "workspace_root": workspace.to_string_lossy()
        }),
        "transport-b",
    );
    let second_id = second["task"]["id"].as_str().expect("second id");
    assert_ne!(first_id, second_id);
    assert_eq!(ctx.harness.current_task().unwrap().unwrap().id, second_id);

    let renewed_status = call_tool_for_session(
        &ctx,
        "harness_status",
        &json!({}),
        "transport-a-reconnected",
    );
    assert_eq!(renewed_status["task_id"], second_id);
    assert_eq!(renewed_status["active_task_count"], 1);
    assert_eq!(renewed_status["writable"], true);

    let reconnected = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &first_args,
        "transport-a-reconnected",
    );
    assert_eq!(reconnected["ok"], true, "{reconnected}");
    assert_eq!(reconnected["task"]["id"], first_id);
    assert_eq!(reconnected["work_session"]["task_created"], false);
    assert_eq!(reconnected["harness"]["task_id"], first_id);
    assert_eq!(reconnected["harness"]["active_task_count"], 1);
    assert_eq!(
        ctx.harness.task(second_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Paused
    );
}

#[test]
fn continued_tool_activity_auto_resumes_a_paused_task() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "resume me\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "暂停后继续执行"}));
    let task_id = started["task"]["id"].as_str().expect("task id").to_string();

    let paused = call_tool(&ctx, "pause_task", &json!({"task_id": task_id}));
    assert_eq!(paused["ok"], true);
    assert_eq!(paused["task"]["status"], "paused");
    let inspected = call_tool(&ctx, "harness_status", &json!({}));
    assert_eq!(inspected["task_state"], "paused");

    let read = call_tool(&ctx, "read_file", &json!({"path": "README.md"}));
    assert_eq!(read["ok"], true);
    let resumed = call_tool(&ctx, "harness_status", &json!({}));
    assert_eq!(resumed["task_state"], "active");
    assert_eq!(resumed["session_status"], "active");

    let events = call_tool(
        &ctx,
        "list_task_events",
        &json!({"task_id": task_id, "limit": 200}),
    );
    assert_eq!(events["ok"], true);
    assert!(events["events"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|event| event["kind"] == "task_auto_resumed" && event["tool_name"] == "read_file")
    }));
}

#[test]
fn observation_token_accepts_exact_current_baseline_and_rejects_stale_tokens() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("main.txt"), "one\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "接受当前基线"}));
    let task_id = started["task"]["id"].as_str().expect("task id");
    fs::write(workspace.join("main.txt"), "two\n").expect("外部修改");

    let status = call_tool(&ctx, "harness_status", &json!({}));
    assert_eq!(status["baseline_matches"], false);
    let token = status["observation_token"]
        .as_str()
        .expect("observation token")
        .to_string();
    let accepted = call_tool(
        &ctx,
        "accept_current_baseline",
        &json!({
            "task_id": task_id,
            "observation_token": token,
            "reason": "接受已审阅的外部修改"
        }),
    );
    assert_eq!(accepted["ok"], true);
    assert_eq!(accepted["accepted"], true);
    assert_eq!(accepted["harness"]["baseline_matches"], true);

    let stale_token = accepted["harness"]["observation_token"]
        .as_str()
        .expect("new observation token")
        .to_string();
    fs::write(workspace.join("main.txt"), "three\n").expect("再次修改");
    let stale = call_tool(
        &ctx,
        "accept_current_baseline",
        &json!({
            "task_id": task_id,
            "observation_token": stale_token,
            "reason": "不应接受过期观察"
        }),
    );
    assert_eq!(stale["ok"], false);
    assert_eq!(stale["error"]["code"], "BASELINE_OBSERVATION_STALE");
}

#[test]
fn expected_failure_disposition_allows_audited_task_completion() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "归档已接受的预期失败"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");
    let failed = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "python -c \"import sys; sys.exit(1)\"",
            "verification_kind": "expected-debt",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(failed["command_ok"], false);
    let verification_id = failed["verification"]["verification_id"]
        .as_str()
        .expect("verification id");

    let disposition = call_tool(
        &ctx,
        "update_verification_disposition",
        &json!({
            "task_id": task_id,
            "verification_id": verification_id,
            "disposition": "expected_failure",
            "reason": "该失败用于记录已接受的兼容性债务"
        }),
    );
    assert_eq!(disposition["ok"], true);
    assert_eq!(disposition["effective_disposition"], "expected_failure");
    assert_eq!(
        disposition["verification_status"],
        "verified_with_exceptions"
    );

    let finished = call_tool(&ctx, "finish_task", &json!({"task_id": task_id}));
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["task_status"], "completed");
    assert_eq!(finished["verification_status"], "verified_with_exceptions");
    assert_eq!(finished["closed"], true);
}

#[test]
fn close_work_session_closes_task_and_checkpoints_bound_history() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
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
    assert_eq!(
        closed["work_session"]["task_status"],
        "completed_unverified"
    );
    assert_eq!(
        closed["checkpoint"]["session_key"],
        "close-work-session-contract"
    );
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
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
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
fn operation_log_aggregates_repeated_failures_into_root_causes() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let task = ctx.harness.start_task("失败聚合").expect("task");
    for (tool, code) in [
        ("exec_command", "WORKSPACE_LINK_UNRESOLVED"),
        ("git_stage", "WORKSPACE_LINK_ESCAPE"),
    ] {
        ctx.harness
            .record_operation(
                None,
                Some(&task.id),
                None,
                tool,
                "failed",
                json!({}),
                json!({
                    "ok": false,
                    "error_code": code,
                    "error_message": "broken workspace link",
                    "error_details": {"link_path": "broken-link"}
                }),
            )
            .expect("record failure");
    }

    let log = call_tool(
        &ctx,
        "operation_log",
        &json!({"task_id": task.id, "failures_only": true, "limit": 20}),
    );

    assert_eq!(log["ok"], true);
    assert_eq!(log["diagnostics"].as_array().unwrap().len(), 1);
    let diagnostic = &log["diagnostics"][0];
    assert_eq!(diagnostic["count"], 2);
    assert_eq!(diagnostic["link_path"], "broken-link");
    assert_eq!(diagnostic["recommended_actions"][0]["tool"], "remove_path");
    assert!(diagnostic["affected_tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool == "exec_command")
            && tools.iter().any(|tool| tool == "git_stage")));
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
    assert_eq!(
        finished["change_summary"]["verification"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        finished["change_summary"]["verification_summary"]["total_records"],
        2
    );
    assert_eq!(
        finished["change_summary"]["verification_summary"]["effective_records"],
        1
    );
    assert_eq!(
        finished["change_summary"]["verification_summary"]["historical_failures_collapsed"],
        1
    );

    let expanded = call_tool(
        &ctx,
        "change_summary",
        &json!({
            "task_id": task_id,
            "verification_history_mode": "all",
            "section": "verification"
        }),
    );
    assert_eq!(expanded["verification"].as_array().unwrap().len(), 2);
}

#[test]
fn change_summary_aggregates_every_task_commit_and_file() {
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
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.email", "anchor-tests@example.invalid"]);
    git(&["config", "user.name", "Anchor Tests"]);
    fs::write(workspace.join("README.md"), "baseline\n").expect("baseline");
    git(&["add", "README.md"]);
    git(&["commit", "-m", "baseline"]);

    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("context");
    let task = ctx
        .harness
        .start_task("multi commit summary")
        .expect("task");
    ctx.harness
        .save_change_set(
            &task.id,
            "1111111111111111111111111111111111111111",
            vec!["a.rs".into(), "shared.rs".into()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("first change");
    ctx.harness
        .save_change_set(
            &task.id,
            "2222222222222222222222222222222222222222",
            vec!["b.rs".into(), "shared.rs".into()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("second change");

    let summary = call_tool(&ctx, "change_summary", &json!({"task_id": task.id}));
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["commit_count"], 2);
    assert_eq!(
        summary["first_commit"],
        "1111111111111111111111111111111111111111"
    );
    assert_eq!(
        summary["last_commit"],
        "2222222222222222222222222222222222222222"
    );
    assert_eq!(
        summary["committed_files"],
        json!(["a.rs", "b.rs", "shared.rs"])
    );
    assert_eq!(summary["files_by_commit"].as_array().unwrap().len(), 2);
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

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "python --version", "include_diagnostics": true}),
    );

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
