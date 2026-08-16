use std::fs;
use std::process::Command;

use anchor_lib::harness::model::HarnessSessionStatus;
use anchor_lib::harness::TaskRecoveryStatus;
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
fn bound_worktree_session_checkpoint_uses_primary_session_store_without_task_baseline_gate() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "worktree session checkpoint\n").expect("写入文件");
    fs::write(
        workspace.join(".gitignore"),
        "/.anchor/worktrees/\n/docs/session/\n",
    )
    .expect("写入 ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let caller = "worktree-session-checkpoint-caller";

    let started = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "验证 worktree Task 的 Session checkpoint 路由",
            "workspace_root": workspace.to_string_lossy(),
            "workspace_mode": "worktree",
            "worktree_remove_on_close": false
        }),
        caller,
    );
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["work_session"]["workspace_mode"], "worktree");

    let checkpoint = call_tool_for_session(
        &ctx,
        "session_checkpoint",
        &json!({
            "workspace_root": workspace.to_string_lossy(),
            "session_id": started["session"]["session_id"],
            "expected_path": started["session"]["session_path"],
            "turn_id": "worktree-session-checkpoint",
            "user_intent": "persist metadata in the primary Session store",
            "session_status": "active"
        }),
        caller,
    );
    assert_eq!(checkpoint["ok"], true, "{checkpoint}");
    assert_eq!(
        checkpoint["path"], started["session"]["session_path"],
        "Session metadata remains in the primary workspace store"
    );
}

#[test]
fn unbound_session_checkpoint_does_not_inherit_unrelated_default_task_baseline() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join(".gitignore"), "/docs/session/\n").expect("写入 ignore");
    fs::write(workspace.join("README.md"), "initial\n").expect("写入 README");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    // This models the post-close state: the Session remains addressable by its opaque
    // id/path, while the caller has no writable Task binding and another Task is
    // the workspace default writer.
    let session = call_tool(
        &ctx,
        "session_open",
        &json!({
            "workspace_root": workspace.to_string_lossy()
        }),
    );
    assert_eq!(session["ok"], true, "{session}");
    let peer = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "unrelated default peer task"}),
    );
    assert_eq!(peer["ok"], true, "{peer}");
    let peer_id = peer["task"]["id"].as_str().expect("peer task id");
    let expected_before = ctx
        .harness
        .task(peer_id)
        .expect("peer task")
        .expected_state
        .worktree_fingerprint;

    fs::write(workspace.join("README.md"), "external drift\n").expect("制造 peer stale baseline");

    let checkpoint = call_tool(
        &ctx,
        "session_checkpoint",
        &json!({
            "workspace_root": workspace.to_string_lossy(),
            "session_id": session["session_id"],
            "expected_path": session["session_path"],
            "turn_id": "checkpoint-after-close-routing",
            "user_intent": "persist Session metadata without inheriting a peer Task baseline",
            "session_status": "active"
        }),
    );
    assert_eq!(checkpoint["ok"], true, "{checkpoint}");

    let peer_after = ctx
        .harness
        .task(peer_id)
        .expect("peer task after checkpoint");
    assert_eq!(
        peer_after.expected_state.worktree_fingerprint, expected_before,
        "Session checkpoint must not silently accept or refresh the unrelated peer baseline"
    );

    let peer_session_operations = call_tool(
        &ctx,
        "operation_log",
        &json!({
            "task_id": peer_id,
            "tool": "session_checkpoint",
            "limit": 20
        }),
    );
    assert_eq!(
        peer_session_operations["ok"], true,
        "{peer_session_operations}"
    );
    assert_eq!(peer_session_operations["total_matches"], 0);

    let peer_write = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: should-not-write.txt\n+blocked\n*** End Patch\n"
        }),
    );
    assert_eq!(peer_write["ok"], false, "{peer_write}");
    assert!(matches!(
        peer_write["error"]["code"].as_str(),
        Some("FILE_CHANGED_EXTERNALLY" | "BASELINE_STALE")
    ));
}

#[test]
fn legacy_nonmutating_patch_recovery_is_nonblocking_and_resolves_on_completion() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let task = ctx
        .harness
        .start_task("旧版 Patch 预检恢复兼容")
        .expect("task");

    let recovery = ctx
        .harness
        .record_recovery(
            &task.id,
            "apply_patch",
            Some("legacy-patch-fingerprint"),
            "tooling_failure",
            Some("PATCH_CONTEXT_MISMATCH"),
            None,
            false,
            false,
            "not_required",
            vec!["读取当前上下文后重新生成 Patch".into()],
            "ready_to_close",
        )
        .expect("record legacy patch recovery");
    assert_eq!(recovery.status, TaskRecoveryStatus::Open);
    assert!(!recovery.blocks_completion());

    let completed = ctx
        .harness
        .complete_task(&task.id, true, HarnessSessionStatus::Active)
        .expect("complete task");
    let recovery = completed
        .recovery
        .expect("resolved recovery retained for audit");
    assert_eq!(recovery.status, TaskRecoveryStatus::Resolved);
    assert_eq!(
        recovery.resolved_by_step.as_deref(),
        Some("task_completion")
    );
}

#[test]
fn task_facade_can_resolve_recovery_from_explicit_post_failure_evidence() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let mut ctx =
        ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    ctx.tool_profile = "advanced".into();
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "显式证据收口 Recovery"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");
    let rejected = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "python -c \"print('one')\" && python -c \"print('two')\"",
            "recovery_key": "logical-step-that-will-be-proven-elsewhere",
            "yield_time_ms": 30000
        }),
    );
    let recovery_id = rejected["task_recovery"]["recovery"]["id"]
        .as_str()
        .expect("recovery id");

    let resolved = call_tool(
        &ctx,
        "task",
        &json!({
            "operation": "resolve_recovery",
            "task_id": task_id,
            "recovery_id": recovery_id,
            "reason": "A later dedicated step and its verification prove the intended work completed.",
            "evidence": ["commit abc123 contains the intended change", "verification recovery-e2e passed"]
        }),
    );
    assert_eq!(resolved["ok"], true, "{resolved}");
    assert_eq!(resolved["recovery"]["status"], "resolved");
    assert_eq!(resolved["recovery"]["resolved_by_step"], "resolve_recovery");
}

#[test]
fn patch_context_mismatch_without_stable_identity_does_not_open_task_recovery() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("main.txt"), "current\n").expect("写入测试文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "Patch 上下文不匹配不应创建持久恢复"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");

    let rejected = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Update File: main.txt\n@@\n-missing\n+replacement\n*** End Patch"
        }),
    );

    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "PATCH_CONTEXT_MISMATCH");
    assert!(rejected.get("task_recovery").is_none(), "{rejected}");
    assert!(ctx.harness.task(task_id).unwrap().recovery.is_none());
}

#[test]
fn policy_rejection_without_stable_identity_does_not_open_task_recovery() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "普通策略预检拒绝不应创建持久恢复"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");

    let rejected = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "python -c \"print('one')\" && python -c \"print('two')\"",
            "yield_time_ms": 30000
        }),
    );

    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "POLICY_REJECTED");
    assert_eq!(rejected["execution_started"], false);
    assert!(rejected.get("task_recovery").is_none(), "{rejected}");
    assert!(ctx.harness.task(task_id).unwrap().recovery.is_none());
}

#[test]
fn legacy_nonmutating_policy_recovery_is_nonblocking_and_resolves_on_completion() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let task = ctx
        .harness
        .start_task("旧版策略预检恢复兼容")
        .expect("task");

    let recovery = ctx
        .harness
        .record_recovery(
            &task.id,
            "exec_command",
            Some("legacy-policy-fingerprint"),
            "tooling_failure",
            Some("POLICY_REJECTED"),
            None,
            false,
            false,
            "not_required",
            vec!["修正命令参数后重试".into()],
            "ready_to_close",
        )
        .expect("record legacy recovery");
    assert_eq!(recovery.status, TaskRecoveryStatus::Open);
    assert!(!recovery.blocks_completion());

    let gate = call_tool(&ctx, "task_gate_status", &json!({"task_id": task.id}));
    assert!(!gate["completion_gate"]["missing"]
        .as_array()
        .expect("completion missing")
        .iter()
        .any(|item| item["code"] == "recovery_open"));

    let completed = ctx
        .harness
        .complete_task(&task.id, true, HarnessSessionStatus::Active)
        .expect("complete task");
    let recovery = completed
        .recovery
        .expect("resolved recovery retained for audit");
    assert_eq!(recovery.status, TaskRecoveryStatus::Resolved);
    assert_eq!(
        recovery.resolved_by_step.as_deref(),
        Some("task_completion")
    );
}

#[test]
fn complete_work_session_checkpoints_before_removed_worktree_becomes_unavailable() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "strict cleanup worktree\n").expect("写入文件");
    fs::write(workspace.join(".gitignore"), "/.anchor/worktrees/\n").expect("写入 ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let caller = "strict-worktree-cleanup-session";

    let started = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "严格关闭后清理隔离目录",
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

    let verified = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "git status --porcelain=v1",
            "verification_kind": "test",
            "verification_key": "strict-worktree-close",
            "verification_level": "blocking"
        }),
        caller,
    );
    assert_eq!(verified["ok"], true, "{verified}");

    let closed = call_tool_for_session(
        &ctx,
        "complete_work_session",
        &json!({
            "task_id": task_id,
            "summary": "strict worktree cleanup test"
        }),
        caller,
    );
    assert_eq!(closed["ok"], true, "{closed}");
    assert_eq!(closed["work_session"]["closed"], true, "{closed}");
    assert_eq!(closed["checkpoint"]["ok"], true, "{closed}");
    assert!(!worktree_path.exists());

    let operations = call_tool_for_session(
        &ctx,
        "operation_log",
        &json!({"task_id": task_id, "limit": 20}),
        caller,
    );
    assert_eq!(operations["ok"], true, "{operations}");
    assert_eq!(operations["filters"]["task_id"], task_id);
}

#[test]
fn complete_work_session_checkpoint_failure_keeps_schema_compliant_retry_state() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "checkpoint pending\n").expect("写入文件");
    fs::write(workspace.join(".gitignore"), "/docs/session/\n").expect("写入 ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let caller = "checkpoint-pending-session";

    let started = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "验证 checkpoint pending 输出契约",
            "workspace_root": workspace.to_string_lossy()
        }),
        caller,
    );
    assert_eq!(started["ok"], true, "{started}");
    let task_id = started["task"]["id"].as_str().expect("task id");
    let verified = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "git status --porcelain=v1",
            "verification_kind": "test",
            "verification_key": "checkpoint-pending-close",
            "verification_level": "blocking"
        }),
        caller,
    );
    assert_eq!(verified["ok"], true, "{verified}");

    let missing_root = temp.path().join("missing-workspace-root");
    let pending = call_tool_for_session(
        &ctx,
        "complete_work_session",
        &json!({
            "task_id": task_id,
            "summary": "force checkpoint pending",
            "checkpoint": {
                "workspace_root": missing_root.to_string_lossy()
            }
        }),
        caller,
    );
    assert_eq!(pending["ok"], false, "{pending}");
    assert_eq!(pending["closed"], false, "{pending}");
    assert_eq!(pending["phase"], "session_checkpoint", "{pending}");
    assert_eq!(
        pending["error"]["code"], "WORK_SESSION_CHECKPOINT_PENDING",
        "{pending}"
    );
    assert_eq!(pending["error"]["details"]["task_closed"], true);
    assert_eq!(pending["outbox"]["phase"], "checkpoint_pending");
}

#[test]
fn successful_retained_retry_resolves_superseded_wait_recovery() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "retained 重试恢复"}),
    );
    let task_id = started["task"]["id"].as_str().expect("任务 ID");

    let failed_start = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "import time,sys; time.sleep(0.2); sys.exit(1)"],
            "verification_kind": "test",
            "verification_key": "retained-retry",
            "yield_time_ms": 1
        }),
    );
    assert_eq!(failed_start["execution_status"], "running");
    let failed = call_tool(
        &ctx,
        "wait_command",
        &json!({
            "session_id": failed_start["session_id"],
            "timeout_ms": 30000
        }),
    );
    assert_eq!(failed["command_ok"], false);
    assert_eq!(failed["task_recovery"]["status"], "open");
    let failed_verification_id = failed["verification_id"]
        .as_str()
        .expect("failed verification id")
        .to_string();

    let passed_start = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "import time; time.sleep(0.2)"],
            "verification_kind": "test",
            "verification_key": "retained-retry",
            "yield_time_ms": 1
        }),
    );
    assert_eq!(passed_start["execution_status"], "running");
    let passed = call_tool(
        &ctx,
        "wait_command",
        &json!({
            "session_id": passed_start["session_id"],
            "timeout_ms": 30000
        }),
    );
    assert_eq!(passed["command_ok"], true, "{passed}");
    assert!(passed["supersedes"]
        .as_array()
        .expect("supersedes")
        .iter()
        .any(|id| id == &failed_verification_id));
    assert_eq!(passed["task_recovery"]["status"], "resolved");
    assert_eq!(
        passed["task_recovery"]["resolved_by_superseding_verification"],
        passed["verification_id"]
    );

    let gate = call_tool(&ctx, "task_gate_status", &json!({"task_id": task_id}));
    assert!(!gate["completion_gate"]["missing"]
        .as_array()
        .expect("completion missing")
        .iter()
        .any(|item| item["code"] == "recovery_open"));
}

#[test]
fn configured_contract_and_slice_plan_cannot_be_relaxed_or_replaced() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({
            "objective": "不可放宽的任务契约",
            "contract": {
                "no_early_stop": true,
                "constraints": ["保留既有约束"],
                "required_verifications": [
                    {"id": "required-check", "verification_key": "required-check"}
                ]
            },
            "slices": [
                {"id": "S-fixed", "title": "固定 Slice", "status": "planned"}
            ]
        }),
    );
    assert_eq!(started["ok"], true);
    let task_id = started["task"]["id"].as_str().expect("task id");

    let relaxed = call_tool(
        &ctx,
        "update_task",
        &json!({
            "task_id": task_id,
            "contract": {
                "no_early_stop": false,
                "constraints": [],
                "required_verifications": [],
                "completion_policy": {}
            }
        }),
    );
    assert_eq!(relaxed["ok"], false);
    assert_eq!(
        relaxed["error"]["code"],
        "TASK_CONTRACT_RELAXATION_REJECTED"
    );

    let replaced = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "slices": []}),
    );
    assert_eq!(replaced["ok"], false);
    assert_eq!(replaced["error"]["code"], "TASK_SLICE_UPDATE_TOOL_REQUIRED");

    let task = ctx.harness.task(task_id).expect("task");
    assert!(task.contract.no_early_stop);
    assert_eq!(task.slices.len(), 1);
    assert_eq!(task.slices[0].id, "S-fixed");
}

#[test]
fn slice_state_machine_and_verification_time_prevent_stale_acceptance() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "拒绝旧验证满足新 Slice"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");

    let stale = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('stale-pass')"],
            "verification_kind": "test",
            "verification_key": "slice-fresh-check",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(stale["command_ok"], true);
    std::thread::sleep(std::time::Duration::from_millis(5));

    let slice = call_tool(
        &ctx,
        "start_slice",
        &json!({
            "task_id": task_id,
            "slice_id": "S-fresh",
            "title": "需要新验证",
            "acceptance_checks": [
                {"id": "fresh", "verification_key": "slice-fresh-check"}
            ]
        }),
    );
    assert_eq!(slice["ok"], true);

    let skipped = call_tool(
        &ctx,
        "update_slice",
        &json!({"task_id": task_id, "slice_id": "S-fresh", "status": "planned"}),
    );
    assert_eq!(skipped["ok"], false);
    assert_eq!(skipped["error"]["code"], "SLICE_STATUS_TRANSITION_INVALID");

    let verifying = call_tool(
        &ctx,
        "update_slice",
        &json!({"task_id": task_id, "slice_id": "S-fresh", "status": "verifying"}),
    );
    assert_eq!(verifying["ok"], true);
    let blocked = call_tool(
        &ctx,
        "complete_slice",
        &json!({"task_id": task_id, "slice_id": "S-fresh"}),
    );
    assert_eq!(blocked["ok"], false);
    assert!(blocked["missing"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["code"] == "slice_acceptance_missing")));

    let fresh = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('fresh-pass')"],
            "verification_kind": "test",
            "verification_key": "slice-fresh-check",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(fresh["command_ok"], true);
    let completed = call_tool(
        &ctx,
        "complete_slice",
        &json!({"task_id": task_id, "slice_id": "S-fresh"}),
    );
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["slice"]["status"], "completed");
}

#[test]
fn policy_rejection_is_recoverable_with_a_stable_logical_step_key() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "策略拒绝后修正原步骤"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");

    let rejected = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "python -c \"print('one')\" && python -c \"print('two')\"",
            "recovery_key": "policy-step-1",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["error"]["code"], "POLICY_REJECTED");
    assert_eq!(rejected["task_recovery"]["status"], "open");
    assert_eq!(
        rejected["task_recovery"]["recovery"]["explicit_retry_identity"],
        true
    );
    assert_eq!(
        rejected["task_recovery"]["recovery"]["workspace_mutated"],
        false
    );
    let server_recovery_key = rejected["task_recovery"]["recovery_key"]
        .as_str()
        .expect("server recovery key")
        .to_string();
    assert_eq!(
        server_recovery_key,
        rejected["task_recovery"]["recovery"]["id"]
    );

    let corrected = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('corrected')"],
            "recovery_key": server_recovery_key,
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(corrected["command_ok"], true);
    assert_eq!(corrected["task_recovery"]["status"], "resolved");
    assert_eq!(
        corrected["task_recovery"]["recovery"]["resolved_by_step"],
        "exec_command"
    );
    assert_eq!(
        ctx.harness
            .task(task_id)
            .unwrap()
            .recovery
            .as_ref()
            .unwrap()
            .status,
        anchor_lib::harness::TaskRecoveryStatus::Resolved
    );
}

#[test]
fn task_phase_state_machine_rejects_skipped_engineering_stages() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "任务阶段状态机", "phase": "planning"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");

    let skipped = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "phase": "ready_to_close"}),
    );
    assert_eq!(skipped["ok"], false);
    assert_eq!(skipped["error"]["code"], "TASK_PHASE_TRANSITION_INVALID");
    assert_eq!(
        ctx.harness.task(task_id).unwrap().phase,
        anchor_lib::harness::TaskPhase::Planning
    );

    let implementing = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "phase": "implementing"}),
    );
    assert_eq!(implementing["ok"], true);
    let verifying = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "phase": "verifying"}),
    );
    assert_eq!(verifying["ok"], true);
    let ready = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "phase": "ready_to_close"}),
    );
    assert_eq!(ready["ok"], true);
    assert_eq!(ready["task"]["phase"], "ready_to_close");

    let forged_complete = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "phase": "completed"}),
    );
    assert_eq!(forged_complete["ok"], false);
    assert_eq!(forged_complete["error"]["code"], "INVALID_TOOL_ARGUMENTS");
    let state_error = ctx
        .harness
        .configure_task(
            task_id,
            Some(anchor_lib::harness::TaskPhase::Completed),
            None,
            None,
            None,
        )
        .expect_err("state layer must reject forged completion");
    assert_eq!(state_error.code(), "TASK_COMPLETION_TOOL_REQUIRED");
}

#[test]
fn no_early_stop_forces_strict_completion_policy() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({
            "objective": "禁止提前结束",
            "contract": {
                "no_early_stop": true,
                "completion_policy": {
                    "require_pending_steps_empty": false,
                    "require_all_slices_completed": false,
                    "require_no_open_recovery": false,
                    "require_ready_to_close": false,
                    "require_complete_work_session": false,
                    "disallow_unverified_completion": false
                }
            }
        }),
    );
    assert_eq!(started["ok"], true);
    let policy = &started["task"]["contract"]["completion_policy"];
    for key in [
        "require_pending_steps_empty",
        "require_all_slices_completed",
        "require_no_open_recovery",
        "require_ready_to_close",
        "require_complete_work_session",
        "disallow_unverified_completion",
    ] {
        assert_eq!(policy[key], true, "no_early_stop 未强制启用 {key}");
    }
}

#[test]
fn strict_verifying_task_can_pause_and_abort_without_fake_completion() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let pause_started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "verifying pause regression"}),
    );
    let pause_task_id = pause_started["task"]["id"].as_str().expect("task id");
    ctx.harness
        .transition(pause_task_id, anchor_lib::harness::TaskStatus::Verifying)
        .expect("enter verifying");
    let paused = call_tool(&ctx, "pause_task", &json!({"task_id": pause_task_id}));
    assert_eq!(paused["ok"], true);
    assert_eq!(paused["task"]["status"], "paused");

    let started = call_tool(
        &ctx,
        "start_task",
        &json!({
            "objective": "strict user termination",
            "pending_steps": ["unfinished verification"],
            "contract": {"no_early_stop": true}
        }),
    );
    assert_eq!(started["ok"], true);
    let task_id = started["task"]["id"].as_str().expect("task id");
    let blocked = call_tool(&ctx, "finish_task", &json!({"task_id": task_id}));
    assert_eq!(blocked["ok"], false);
    assert_eq!(blocked["task_status"], "verifying");

    let aborted = call_tool(
        &ctx,
        "abort_task",
        &json!({
            "task_id": task_id,
            "reason": "user explicitly terminated the unfinished task",
            "session_status": "paused"
        }),
    );
    assert_eq!(aborted["ok"], true);
    assert_eq!(aborted["task_status"], "incomplete");
    assert_eq!(aborted["outcome"], "incomplete");
    assert_eq!(aborted["closed"], true);
    assert_eq!(aborted["task"]["phase"], "aborted");
    assert_eq!(aborted["task"]["contract"]["no_early_stop"], true);
    assert_eq!(
        aborted["task"]["termination"]["reason"],
        "user explicitly terminated the unfinished task"
    );
    assert_eq!(aborted["completion_gate"]["ready"], false);
    assert_eq!(
        ctx.harness.task(task_id).expect("persisted task").status,
        anchor_lib::harness::TaskStatus::Incomplete
    );
}

#[test]
fn close_work_session_can_checkpoint_a_no_early_stop_task_as_incomplete() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "用户终止严格 Work Session",
            "workspace_root": workspace.to_string_lossy(),
            "phase": "verifying",
            "pending_steps": ["unfinished"],
            "contract": {"no_early_stop": true}
        }),
    );
    assert_eq!(started["ok"], true);
    let task_id = started["work_session"]["task_id"]
        .as_str()
        .expect("task id");

    let missing_reason = call_tool(
        &ctx,
        "close_work_session",
        &json!({
            "task_id": task_id,
            "outcome": "incomplete",
            "session_status": "paused"
        }),
    );
    assert_eq!(missing_reason["ok"], false);
    assert_eq!(missing_reason["error"]["code"], "INVALID_ARGUMENT");

    let invalid_completed_session = call_tool(
        &ctx,
        "close_work_session",
        &json!({
            "task_id": task_id,
            "outcome": "incomplete",
            "reason": "user requested termination",
            "session_status": "completed"
        }),
    );
    assert_eq!(invalid_completed_session["ok"], false);
    assert_eq!(
        invalid_completed_session["error"]["code"],
        "INVALID_ARGUMENT"
    );

    let closed = call_tool(
        &ctx,
        "close_work_session",
        &json!({
            "task_id": task_id,
            "outcome": "incomplete",
            "reason": "user requested termination before task completion",
            "session_status": "paused",
            "summary": "terminated by user; unfinished work preserved",
            "checkpoint": {
                "remaining_issues": ["unfinished"],
                "runtime_state": ["termination=terminated-by-user"]
            }
        }),
    );
    assert_eq!(closed["ok"], true);
    assert_eq!(closed["work_session"]["closed"], true);
    assert_eq!(closed["work_session"]["outcome"], "incomplete");
    assert_eq!(closed["work_session"]["task_status"], "incomplete");
    assert_eq!(closed["work_session"]["status"], "paused");
    assert_eq!(closed["finish"]["outcome"], "incomplete");
    assert_eq!(closed["task"]["phase"], "aborted");
    assert_eq!(closed["task"]["contract"]["no_early_stop"], true);
    assert_eq!(
        closed["task"]["termination"]["reason"],
        "user requested termination before task completion"
    );

    let strict_complete = call_tool(
        &ctx,
        "complete_work_session",
        &json!({"task_id": task_id, "summary": "must not rewrite incomplete"}),
    );
    assert_eq!(strict_complete["ok"], false);
    assert_eq!(
        strict_complete["error"]["code"],
        "WORK_SESSION_ALREADY_ABORTING"
    );
}

#[test]
fn begin_work_session_binds_session_and_task_idempotently() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "initial\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let initial_arguments = json!({
        "objective": "统一工作会话测试",
        "workspace_root": workspace.to_string_lossy()
    });
    let mcp_session = "work-session-a";

    let first = call_tool_for_session(&ctx, "begin_work_session", &initial_arguments, mcp_session);
    assert_eq!(first["ok"], true);
    assert_eq!(first["work_session"]["status"], "active");
    assert_eq!(first["work_session"]["task_created"], true);
    let session_id = first["work_session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    assert!(session_id.starts_with("ses_"));
    let task_id = first["work_session"]["task_id"]
        .as_str()
        .expect("task id")
        .to_string();
    let session_path = first["work_session"]["session_path"]
        .as_str()
        .expect("session path");
    assert!(workspace.join(session_path).exists());
    assert_eq!(first["task"]["session_id"], session_id);
    let resume_arguments = json!({
        "objective": "统一工作会话测试",
        "session_id": session_id,
        "workspace_root": workspace.to_string_lossy()
    });

    let second = call_tool_for_session(&ctx, "begin_work_session", &resume_arguments, mcp_session);
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
    let reconnected =
        call_tool_for_session(&ctx, "begin_work_session", &resume_arguments, mcp_session);
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
fn begin_work_session_handoff_pauses_previous_same_session_shared_writer() {
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
            "workspace_root": workspace.to_string_lossy()
        }),
        mcp_session,
    );
    assert_eq!(first["ok"], true);
    let first_id = first["task"]["id"].as_str().expect("first id").to_string();
    let session_id = first["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();

    let second = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "任务 B",
            "session_id": session_id,
            "workspace_root": workspace.to_string_lossy()
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
    assert_eq!(
        second["work_session"]["auto_paused_previous_task_ids"][0],
        first_id
    );
    assert_eq!(second["harness"]["task_id"], second_id);
    assert_eq!(second["harness"]["active_task_count"], 1);

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
        anchor_lib::harness::TaskStatus::Active
    );
    assert_eq!(switched["harness"]["default_task_id"], first_id);
    assert_eq!(switched["harness"]["active_task_count"], 2);
}

#[test]
fn direct_finish_protocol_gate_does_not_downgrade_ready_task_to_verifying() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let mut ctx =
        ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    ctx.tool_profile = "advanced".into();
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({
            "objective": "只能通过 complete_work_session 关闭",
            "phase": "verifying",
            "contract": {
                "completion_policy": {
                    "require_complete_work_session": true
                }
            }
        }),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");
    assert_eq!(started["task"]["status"], "active");
    let ready = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "phase": "ready_to_close"}),
    );
    assert_eq!(ready["ok"], true, "{ready}");
    assert_eq!(ready["task"]["phase"], "ready_to_close");
    let verified = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('ready-to-close verified')"],
            "verification_kind": "check",
            "verification_key": "ready-to-close-protocol-only",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(verified["command_ok"], true, "{verified}");

    let rejected = call_tool(
        &ctx,
        "task",
        &json!({"operation": "finish", "task_id": task_id}),
    );
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["task_status"], "active");
    assert_eq!(
        ctx.harness.task(task_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Active
    );
}

#[test]
fn shared_tasks_remain_active_and_serialize_workspace_writes() {
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
    assert_eq!(first["ok"], true, "{first}");
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
    assert_eq!(first_status["active_task_count"], 2);
    assert_eq!(second_status["active_task_count"], 2);
    assert_eq!(
        ctx.harness.task(&first_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Active
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
    assert_eq!(first_after["active_task_count"], 2);
    assert_eq!(second_after["active_task_count"], 2);
    assert_eq!(ctx.harness.active_tasks().unwrap().len(), 2);

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
fn unbound_transport_never_adopts_workspace_default_task() {
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
    assert_eq!(first["ok"], true);
    let unique_rejected = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: unique-forbidden.txt\n+forbidden\n*** End Patch\n"
        }),
        "unbound-unique-session",
    );
    assert_eq!(unique_rejected["ok"], false, "{unique_rejected}");
    assert_eq!(unique_rejected["error"]["code"], "TASK_BINDING_REQUIRED");
    assert!(!workspace.join("unique-forbidden.txt").exists());

    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "绑定任务 B"}),
        "binding-session-b",
    );
    assert_eq!(second["ok"], true);
    assert_eq!(ctx.harness.active_tasks().unwrap().len(), 2);

    let ambiguous_rejected = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: ambiguous-forbidden.txt\n+forbidden\n*** End Patch\n"
        }),
        "unbound-third-session",
    );
    assert_eq!(ambiguous_rejected["ok"], false, "{ambiguous_rejected}");
    assert_eq!(ambiguous_rejected["error"]["code"], "TASK_BINDING_REQUIRED");
    assert!(!workspace.join("ambiguous-forbidden.txt").exists());

    let paused_second = call_tool_for_session(
        &ctx,
        "pause_task",
        &json!({"task_id": second["task"]["id"]}),
        "binding-session-b",
    );
    assert_eq!(paused_second["ok"], true, "{paused_second}");
    let paused_first = call_tool_for_session(
        &ctx,
        "pause_task",
        &json!({"task_id": first["task"]["id"]}),
        "binding-session-a",
    );
    assert_eq!(paused_first["ok"], true, "{paused_first}");
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
fn one_transport_restores_each_tasks_last_cwd_when_switching() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join("task-a-dir")).expect("task a dir");
    fs::create_dir_all(workspace.join("task-b-dir")).expect("task b dir");
    fs::write(workspace.join("README.md"), "task cwd\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let transport = "task-cwd-switch-session";

    let first = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "cwd task A"}),
        transport,
    );
    assert_eq!(first["ok"], true, "{first}");
    let first_id = first["task"]["id"].as_str().expect("first id").to_string();
    let set_a = call_tool_for_session(
        &ctx,
        "set_default_cwd",
        &json!({"path": "task-a-dir"}),
        transport,
    );
    assert_eq!(set_a["ok"], true, "{set_a}");
    assert_eq!(set_a["default_cwd"], "task-a-dir");

    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "cwd task B"}),
        transport,
    );
    assert_eq!(second["ok"], true, "{second}");
    let second_id = second["task"]["id"]
        .as_str()
        .expect("second id")
        .to_string();
    let set_b = call_tool_for_session(
        &ctx,
        "set_default_cwd",
        &json!({"path": "task-b-dir"}),
        transport,
    );
    assert_eq!(set_b["ok"], true, "{set_b}");
    assert_eq!(set_b["default_cwd"], "task-b-dir");

    let back_to_a = call_tool_for_session(
        &ctx,
        "switch_task",
        &json!({"task_id": first_id}),
        transport,
    );
    assert_eq!(back_to_a["ok"], true, "{back_to_a}");
    let cwd_a = call_tool_for_session(&ctx, "get_default_cwd", &json!({}), transport);
    assert_eq!(cwd_a["ok"], true, "{cwd_a}");
    assert_eq!(cwd_a["default_cwd"], "task-a-dir");

    let back_to_b = call_tool_for_session(
        &ctx,
        "switch_task",
        &json!({"task_id": second_id}),
        transport,
    );
    assert_eq!(back_to_b["ok"], true, "{back_to_b}");
    let cwd_b = call_tool_for_session(&ctx, "get_default_cwd", &json!({}), transport);
    assert_eq!(cwd_b["ok"], true, "{cwd_b}");
    assert_eq!(cwd_b["default_cwd"], "task-b-dir");
}

#[test]
fn one_transport_restores_cwd_across_two_real_worktrees() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join("task-subdir")).expect("task subdir");
    fs::write(workspace.join("README.md"), "worktree cwd\n").expect("README");
    fs::write(workspace.join("task-subdir/marker.txt"), "tracked\n").expect("marker");
    fs::write(workspace.join(".gitignore"), "/.anchor/worktrees/\n").expect("ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let transport = "real-worktree-cwd-switch";

    let first = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "real worktree A", "workspace_mode": "worktree"}),
        transport,
    );
    assert_eq!(first["ok"], true, "{first}");
    let first_id = first["task"]["id"].as_str().expect("first id").to_string();
    let first_root = std::path::PathBuf::from(
        first["task"]["git_worktree"]["path"]
            .as_str()
            .expect("first root"),
    );
    let first_cwd = call_tool_for_session(
        &ctx,
        "set_default_cwd",
        &json!({"path": "task-subdir"}),
        transport,
    );
    assert_eq!(first_cwd["ok"], true, "{first_cwd}");
    let first_resolved = std::path::PathBuf::from(
        first_cwd["resolved_cwd"]
            .as_str()
            .expect("first resolved cwd"),
    );
    assert!(first_resolved.starts_with(&first_root));

    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "real worktree B", "workspace_mode": "worktree"}),
        transport,
    );
    assert_eq!(second["ok"], true, "{second}");
    let second_root = std::path::PathBuf::from(
        second["task"]["git_worktree"]["path"]
            .as_str()
            .expect("second root"),
    );
    let second_cwd = call_tool_for_session(
        &ctx,
        "set_default_cwd",
        &json!({"path": "task-subdir"}),
        transport,
    );
    assert_eq!(second_cwd["ok"], true, "{second_cwd}");
    let second_resolved = std::path::PathBuf::from(
        second_cwd["resolved_cwd"]
            .as_str()
            .expect("second resolved cwd"),
    );
    assert!(second_resolved.starts_with(&second_root));
    assert_ne!(first_resolved, second_resolved);

    let back_to_a = call_tool_for_session(
        &ctx,
        "switch_task",
        &json!({"task_id": first_id}),
        transport,
    );
    assert_eq!(back_to_a["ok"], true, "{back_to_a}");
    let restored = call_tool_for_session(&ctx, "get_default_cwd", &json!({}), transport);
    assert_eq!(restored["ok"], true, "{restored}");
    let restored = std::path::PathBuf::from(
        restored["resolved_cwd"]
            .as_str()
            .expect("restored resolved cwd"),
    );
    assert_eq!(restored, first_resolved);
}

#[test]
fn running_command_allows_peer_task_creation_but_blocks_peer_writes() {
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

    let second = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "竞争写租约的任务"}),
        "writer-busy-session-b",
    );
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(ctx.harness.active_tasks().unwrap().len(), 2);
    let blocked_write = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: peer-after-command.txt\n+peer\n*** End Patch\n"
        }),
        "writer-busy-session-b",
    );
    assert_eq!(blocked_write["ok"], false, "{blocked_write}");
    assert_eq!(blocked_write["error"]["code"], "WORKSPACE_WRITER_BUSY");

    let killed = call_tool_for_session(
        &ctx,
        "kill_session",
        &json!({"session_id": command_session_id}),
        "writer-busy-session-a",
    );
    assert_eq!(killed["ok"], false, "{killed}");
    assert_eq!(killed["killed"], true, "{killed}");
    assert_eq!(killed["execution_status"], "killed", "{killed}");

    let resumed_write = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: peer-after-command.txt\n+peer\n*** End Patch\n"
        }),
        "writer-busy-session-b",
    );
    assert_eq!(resumed_write["ok"], true, "{resumed_write}");
    assert!(ctx.workspace.root().join("peer-after-command.txt").exists());
    assert_eq!(ctx.harness.active_tasks().unwrap().len(), 2);
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
fn session_checkpoint_cannot_complete_an_active_bound_task() {
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
            "workspace_root": workspace.to_string_lossy()
        }),
        "session-completion-caller",
    );
    assert_eq!(started["ok"], true, "{started}");
    let session_id = started["session"]["session_id"]
        .as_str()
        .expect("session id");
    let expected_path = started["session"]["session_path"]
        .as_str()
        .expect("session path");

    let rejected = call_tool_for_session(
        &ctx,
        "session_checkpoint",
        &json!({
            "session_id": session_id,
            "expected_path": expected_path,
            "turn_id": "illegal-completion",
            "user_intent": "绕过关闭流程",
            "session_status": "completed"
        }),
        "session-completion-caller",
    );
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "SESSION_TASK_STILL_ACTIVE");
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
    assert_eq!(
        document["plugin"]["catalog_version"],
        anchor_lib::tools::registry::CATALOG_VERSION
    );
    assert_eq!(document["task"]["id"], task_id);
    assert_eq!(
        document["session"]["session_id"],
        started["session"]["session_id"]
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
        "/.anchor/worktrees/\n/.anchor/handoffs/\n/docs/session/\n",
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

    let unbound_read = call_tool_for_session(
        &ctx,
        "read_file",
        &json!({"path": "isolated.txt"}),
        "fresh-unbound-worktree-reader",
    );
    assert_eq!(unbound_read["ok"], false, "{unbound_read}");
    assert_eq!(unbound_read["error"]["code"], "NOT_FOUND");

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
fn unbound_transports_stay_primary_until_explicitly_bound_and_cleanup_on_finish() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "stateless task routing\n").expect("写入文件");
    fs::write(workspace.join(".gitignore"), "/.anchor/worktrees/\n").expect("写入 ignore");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let shared = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({"objective": "共享任务"}),
        "stateless-start-shared",
    );
    assert_eq!(shared["ok"], true, "{shared}");
    let shared_branch = shared["task"]["expected_state"]["branch"]
        .as_str()
        .expect("shared branch")
        .to_string();

    let isolated = call_tool_for_session(
        &ctx,
        "start_task",
        &json!({
            "objective": "隔离任务",
            "workspace_mode": "worktree",
            "worktree_remove_on_close": true
        }),
        "stateless-start-worktree",
    );
    assert_eq!(isolated["ok"], true, "{isolated}");
    let task_id = isolated["task"]["id"].as_str().expect("task id");
    let worktree_path = std::path::PathBuf::from(
        isolated["task"]["git_worktree"]["path"]
            .as_str()
            .expect("worktree path"),
    );
    let cwd = call_tool_for_session(
        &ctx,
        "get_default_cwd",
        &json!({}),
        "stateless-cwd-after-start",
    );
    assert_eq!(cwd["ok"], true, "{cwd}");
    assert_eq!(cwd["default_cwd"], ".");
    assert_eq!(
        std::path::PathBuf::from(cwd["resolved_cwd"].as_str().expect("resolved cwd"))
            .canonicalize()
            .expect("canonical cwd"),
        workspace
            .canonicalize()
            .expect("canonical primary workspace")
    );

    let status = call_tool_for_session(
        &ctx,
        "git_status",
        &json!({}),
        "stateless-status-after-start",
    );
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["branch"], shared_branch);

    let unbound_patch = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: stateless-route.txt\n+worktree\n*** End Patch\n"
        }),
        "stateless-patch-after-start",
    );
    assert_eq!(unbound_patch["ok"], false, "{unbound_patch}");
    assert_eq!(unbound_patch["error"]["code"], "TASK_BINDING_REQUIRED");
    assert!(!worktree_path.join("stateless-route.txt").exists());
    assert!(!workspace.join("stateless-route.txt").exists());

    let patched = call_tool_for_session(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: stateless-route.txt\n+worktree\n*** End Patch\n"
        }),
        "stateless-start-worktree",
    );
    assert_eq!(patched["ok"], true, "{patched}");
    assert!(worktree_path.join("stateless-route.txt").exists());
    assert!(!workspace.join("stateless-route.txt").exists());

    let removed = call_tool_for_session(
        &ctx,
        "remove_path",
        &json!({"path": "stateless-route.txt"}),
        "stateless-start-worktree",
    );
    assert_eq!(removed["ok"], true, "{removed}");

    let finished = call_tool_for_session(
        &ctx,
        "finish_task",
        &json!({
            "task_id": task_id,
            "allow_unverified": true,
            "session_status": "active"
        }),
        "stateless-start-worktree",
    );
    assert_eq!(finished["ok"], true, "{finished}");
    assert_eq!(finished["worktree_cleanup"]["requested"], true);
    assert_eq!(finished["worktree_cleanup"]["removed"], true);
    assert!(!worktree_path.exists());

    let primary_status = call_tool_for_session(
        &ctx,
        "git_status",
        &json!({}),
        "stateless-status-after-finish",
    );
    assert_eq!(primary_status["ok"], true, "{primary_status}");
    assert_eq!(primary_status["branch"], shared_branch);
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
fn reconnected_transport_recovers_task_by_explicit_session_not_workspace_default() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "parallel reconnect\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let first_args = json!({
        "objective": "可恢复并行任务 A",
        "workspace_root": workspace.to_string_lossy()
    });
    let first = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &first_args,
        "transport-a-original",
    );
    assert_eq!(first["ok"], true, "{first}");
    let first_id = first["task"]["id"].as_str().expect("first id").to_string();
    let first_session_id = first["session"]["session_id"]
        .as_str()
        .expect("first session id")
        .to_string();
    let reconnect_args = json!({
        "objective": "可恢复并行任务 A",
        "session_id": first_session_id,
        "workspace_root": workspace.to_string_lossy()
    });

    let second = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "并行任务 B 成为工作区默认",
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
    assert!(renewed_status["task_id"].is_null(), "{renewed_status}");
    assert_eq!(renewed_status["active_task_count"], 2);
    assert_eq!(renewed_status["writable"], false);

    let reconnected = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &reconnect_args,
        "transport-a-reconnected",
    );
    assert_eq!(reconnected["ok"], true, "{reconnected}");
    assert_eq!(reconnected["task"]["id"], first_id);
    assert_eq!(reconnected["work_session"]["task_created"], false);
    assert_eq!(reconnected["harness"]["task_id"], first_id);
    assert_eq!(reconnected["harness"]["active_task_count"], 2);
    assert_eq!(
        ctx.harness.task(second_id).unwrap().status,
        anchor_lib::harness::TaskStatus::Active
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
fn close_work_session_closes_task_and_checkpoints_bound_session() {
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
            "workspace_root": workspace.to_string_lossy()
        }),
    );
    let task_id = started["work_session"]["task_id"]
        .as_str()
        .expect("task id");
    let session_id = started["work_session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    let expected_path = started["work_session"]["session_path"]
        .as_str()
        .expect("session path")
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
    assert_eq!(closed["checkpoint"]["session_id"], session_id);
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
    let caller = "operation-scope-caller";
    let started = call_tool_for_session(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "作用域操作日志",
            "workspace_root": workspace.to_string_lossy()
        }),
        caller,
    );
    let task_id = started["work_session"]["task_id"]
        .as_str()
        .expect("task id");
    let session_id = started["work_session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    let executed = call_tool_for_session(
        &ctx,
        "exec_command",
        &json!({"cmd": "python --version", "yield_time_ms": 30000}),
        caller,
    );
    assert_eq!(executed["command_ok"], true);

    let log = call_tool_for_session(
        &ctx,
        "operation_log",
        &json!({
            "task_id": task_id,
            "session_id": session_id,
            "tool": "exec_command",
            "collapse": true,
            "limit": 20
        }),
        caller,
    );
    assert_eq!(log["ok"], true);
    assert_eq!(log["total_matches"], 1);
    assert_eq!(log["operations"].as_array().unwrap().len(), 1);
    let operation = &log["operations"][0];
    assert_eq!(operation["task_id"], task_id);
    assert_eq!(operation["session_id"], session_id);
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
fn ordinary_git_commit_is_persisted_as_a_task_change_set() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "baseline\n").expect("baseline");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("context");
    let task = ctx
        .harness
        .start_task("ordinary commit summary")
        .expect("task");
    let patched = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "*** Begin Patch\n*** Add File: feature.txt\n+feature\n*** End Patch\n"
        }),
    );
    assert_eq!(patched["ok"], true, "{patched}");

    let staged = call_tool(&ctx, "git_stage", &json!({"paths": ["feature.txt"]}));
    assert_eq!(staged["ok"], true, "{staged}");
    let committed = call_tool(
        &ctx,
        "git_commit",
        &json!({"message": "test: ordinary task commit"}),
    );
    assert_eq!(committed["ok"], true, "{committed}");

    let summary = call_tool(&ctx, "change_summary", &json!({"task_id": task.id}));
    assert_eq!(summary["commit_count"], 1, "{summary}");
    assert_eq!(summary["commits"][0]["commit_sha"], committed["commit_sha"]);
    assert_eq!(summary["committed_files"], json!(["feature.txt"]));
    assert_eq!(summary["rollback_capability"], "git_commit_range");
}

#[test]
fn change_summary_reconstructs_missing_change_sets_from_the_git_range() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "baseline\n").expect("baseline");
    initialize_git(&workspace);
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("context");
    let task = ctx.harness.start_task("range fallback").expect("task");

    for (path, content, message) in [
        ("first.txt", "first\n", "first external commit"),
        ("second.txt", "second\n", "second external commit"),
    ] {
        fs::write(workspace.join(path), content).expect("commit file");
        let output = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(["add", path])
            .output()
            .expect("git add");
        assert!(output.status.success());
        let output = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(["commit", "--no-gpg-sign", "--no-verify", "-m", message])
            .output()
            .expect("git commit");
        assert!(output.status.success());
    }
    ctx.harness
        .refresh_expected_state_for_operation(&task.id, None)
        .expect("refresh expected state");

    let summary = call_tool(&ctx, "change_summary", &json!({"task_id": task.id}));
    assert_eq!(summary["commit_count"], 2, "{summary}");
    assert_eq!(
        summary["committed_files"],
        json!(["first.txt", "second.txt"])
    );
    assert_eq!(summary["commits"][0]["source"], "git_commit_range_fallback");
    assert_eq!(summary["commits"][1]["source"], "git_commit_range_fallback");
    assert_eq!(summary["rollback_capability"], "git_commit_range");

    let missing = call_tool(
        &ctx,
        "change_summary",
        &json!({
            "task_id": task.id,
            "change_id": "0000000000000000000000000000000000000000"
        }),
    );
    assert_eq!(missing["commit_count"], 0, "{missing}");
    assert_eq!(missing["commits"], json!([]));
    assert_eq!(missing["rollback_capability"], "not_available");
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
fn finish_task_rejects_running_and_terminal_unobserved_commands() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "消费后台命令后再结束"}),
    );
    let task_id = started["task"]["id"].as_str().expect("任务 ID");
    let command = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-u", "-c", "import time; print('pending', flush=True); time.sleep(0.4)"],
            "yield_time_ms": 0,
            "timeout_ms": 5_000
        }),
    );
    let command_session = command["session_id"].as_str().expect("命令 Session");

    let running_blocked = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "allow_unverified": true}),
    );
    assert_eq!(running_blocked["ok"], false);
    assert_eq!(
        running_blocked["error"]["code"],
        "TASK_COMMAND_RESULTS_PENDING"
    );
    assert_eq!(
        running_blocked["running_sessions"][0]["session_id"],
        command_session
    );

    std::thread::sleep(std::time::Duration::from_millis(650));
    let terminal_blocked = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "allow_unverified": true}),
    );
    assert_eq!(terminal_blocked["ok"], false);
    assert_eq!(
        terminal_blocked["error"]["code"],
        "TASK_COMMAND_RESULTS_PENDING"
    );
    assert_eq!(
        terminal_blocked["unobserved_terminal_sessions"][0]["session_id"],
        command_session
    );

    let observed = call_tool(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 0}),
    );
    assert_eq!(observed["ok"], true);
    assert_eq!(observed["result_observed"], true);

    let finished = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "allow_unverified": true}),
    );
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["closed"], true);
}

#[test]
fn abort_task_rejects_pending_command_results_then_closes_incomplete() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "用户终止仍需先消费后台命令"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");
    let command = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-u", "-c", "import time; print('pending', flush=True); time.sleep(0.3)"],
            "yield_time_ms": 0,
            "timeout_ms": 5_000
        }),
    );
    let command_session = command["session_id"].as_str().expect("command session");

    let running = call_tool(
        &ctx,
        "abort_task",
        &json!({"task_id": task_id, "reason": "user stop", "session_status": "paused"}),
    );
    assert_eq!(running["ok"], false);
    assert_eq!(
        running["error"]["code"],
        "TASK_ABORT_COMMAND_RESULTS_PENDING"
    );
    assert_eq!(running["closed"], false);

    std::thread::sleep(std::time::Duration::from_millis(500));
    let terminal = call_tool(
        &ctx,
        "abort_task",
        &json!({"task_id": task_id, "reason": "user stop", "session_status": "paused"}),
    );
    assert_eq!(terminal["ok"], false);
    assert_eq!(
        terminal["unobserved_terminal_sessions"][0]["session_id"],
        command_session
    );

    let observed = call_tool(
        &ctx,
        "wait_command",
        &json!({"session_id": command_session, "timeout_ms": 0}),
    );
    assert_eq!(observed["ok"], true);
    assert_eq!(observed["result_observed"], true);

    let aborted = call_tool(
        &ctx,
        "abort_task",
        &json!({"task_id": task_id, "reason": "user stop", "session_status": "paused"}),
    );
    assert_eq!(aborted["ok"], true);
    assert_eq!(aborted["task_status"], "incomplete");
    assert_eq!(aborted["closed"], true);
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
    assert!(names.contains(&"task"), "缺少 task facade");
    for internal in [
        "project_state",
        "start_task",
        "task_context",
        "list_task_events",
        "change_summary",
    ] {
        assert!(!names.contains(&internal), "内部工具泄漏 {internal}");
    }
    let task = tools
        .iter()
        .find(|tool| tool["name"] == "task")
        .expect("task facade");
    let operations = task["inputSchema"]["properties"]["operation"]["enum"]
        .as_array()
        .expect("task operations");
    for expected in [
        "project_state",
        "start",
        "context",
        "events",
        "change_summary",
    ] {
        assert!(
            operations.iter().any(|operation| operation == expected),
            "task facade 缺少 operation {expected}"
        );
    }
    assert!(!names.contains(&"undo_last_patch"));
}

#[test]
fn task_contract_blocks_early_finish_until_every_declared_gate_passes() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "contract\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "begin_work_session",
        &json!({
            "objective": "严格任务契约",
            "workspace_root": workspace.to_string_lossy(),
            "phase": "planning",
            "pending_steps": ["final review"],
            "contract": {
                "no_early_stop": true,
                "constraints": ["每个 Slice 必须通过声明的验收检查"],
                "required_verifications": [
                    {"id": "final-lint", "verification_key": "final-lint"}
                ],
                "completion_policy": {
                    "require_pending_steps_empty": true,
                    "require_all_slices_completed": true,
                    "require_slice_commits": true,
                    "require_no_open_recovery": true,
                    "require_ready_to_close": true,
                    "require_complete_work_session": true,
                    "disallow_unverified_completion": true
                }
            },
            "slices": [
                {
                    "id": "S1",
                    "title": "实现任务闭环",
                    "status": "planned",
                    "acceptance_checks": [
                        {"id": "slice-test", "verification_key": "slice-test"}
                    ]
                }
            ],
            "working_set": {
                "primary": ["src-tauri/src/harness/tools.rs"],
                "tests": ["src-tauri/tests/harness_tool_contract.rs"]
            }
        }),
    );
    assert_eq!(started["ok"], true);
    let task_id = started["work_session"]["task_id"]
        .as_str()
        .expect("task id");
    assert_eq!(started["task"]["contract"]["no_early_stop"], true);
    assert_eq!(started["task"]["slices"][0]["id"], "S1");
    assert_eq!(
        started["task"]["working_set"]["primary"][0],
        "src-tauri/src/harness/tools.rs"
    );

    let initial_gate = call_tool(&ctx, "task_gate_status", &json!({"task_id": task_id}));
    assert_eq!(initial_gate["ok"], true);
    assert_eq!(initial_gate["ready"], false);
    assert_eq!(initial_gate["detail"], "compact");
    assert!(initial_gate.get("task").is_none());
    assert!(initial_gate.get("verification").is_none());
    let full_gate = call_tool(
        &ctx,
        "task_gate_status",
        &json!({"task_id": task_id, "detail": "full"}),
    );
    assert_eq!(full_gate["detail"], "full");
    assert!(full_gate["task"].is_object());
    assert!(full_gate["verification"].is_array());
    let initial_codes = initial_gate["completion_gate"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value["code"].as_str())
        .collect::<Vec<_>>();
    for code in [
        "pending_steps_remaining",
        "slices_incomplete",
        "ready_to_close_phase_missing",
        "complete_work_session_required",
        "required_verification_missing",
        "slice_acceptance_missing",
    ] {
        assert!(
            initial_codes.contains(&code),
            "completion gate missing {code}"
        );
    }

    let early_finish = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "allow_unverified": true}),
    );
    assert_eq!(early_finish["ok"], false);
    assert_eq!(early_finish["error"]["code"], "TASK_VERIFICATION_MISSING");
    assert_eq!(early_finish["completion_gate"]["ready"], false);

    let implementing = call_tool(
        &ctx,
        "update_slice",
        &json!({"task_id": task_id, "slice_id": "S1", "status": "in_progress"}),
    );
    assert_eq!(implementing["ok"], true);
    assert_eq!(implementing["slice"]["status"], "in_progress");
    let verifying = call_tool(
        &ctx,
        "update_slice",
        &json!({"task_id": task_id, "slice_id": "S1", "status": "verifying"}),
    );
    assert_eq!(verifying["ok"], true);
    assert_eq!(verifying["slice"]["status"], "verifying");

    let slice_test = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('slice-ok')"],
            "verification_kind": "test",
            "verification_key": "slice-test",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(slice_test["command_ok"], true);
    let completed_slice = call_tool(
        &ctx,
        "complete_slice",
        &json!({"task_id": task_id, "slice_id": "S1", "commit_sha": "slice-commit-1"}),
    );
    assert_eq!(completed_slice["ok"], true);
    assert_eq!(completed_slice["completed"], true);
    assert_eq!(completed_slice["slice"]["status"], "completed");

    let final_lint = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('lint-ok')"],
            "verification_kind": "lint",
            "verification_key": "final-lint",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(final_lint["command_ok"], true);
    let updated = call_tool(
        &ctx,
        "update_task",
        &json!({
            "task_id": task_id,
            "pending_steps": [],
            "phase": "ready_to_close"
        }),
    );
    assert_eq!(updated["task"]["phase"], "ready_to_close");

    let direct_finish = call_tool(&ctx, "finish_task", &json!({"task_id": task_id}));
    assert_eq!(direct_finish["ok"], false);
    assert_eq!(
        direct_finish["error"]["code"],
        "TASK_COMPLETE_WORK_SESSION_REQUIRED"
    );
    let direct_close = call_tool(
        &ctx,
        "close_work_session",
        &json!({
            "task_id": task_id,
            "session_status": "completed",
            "summary": "不得绕过严格入口"
        }),
    );
    assert_eq!(direct_close["ok"], false);
    assert_eq!(
        direct_close["finish"]["error"]["code"],
        "TASK_COMPLETE_WORK_SESSION_REQUIRED"
    );
    let closed = call_tool(
        &ctx,
        "complete_work_session",
        &json!({
            "task_id": task_id,
            "summary": "严格契约全部通过",
            "checkpoint": {
                "findings": ["task contract completion gate passed"],
                "tests": ["slice-test", "final-lint"]
            }
        }),
    );
    assert_eq!(closed["ok"], true);
    assert_eq!(closed["work_session"]["closed"], true);
    assert_eq!(closed["work_session"]["status"], "completed");
    assert_eq!(closed["task"]["phase"], "completed");

    let completed_gate = call_tool(&ctx, "task_gate_status", &json!({"task_id": task_id}));
    assert_eq!(completed_gate["ok"], true);
    assert_eq!(completed_gate["ready"], true);
    assert_eq!(completed_gate["completion_gate"]["ready"], true);
    assert!(completed_gate["completion_gate"]["missing"]
        .as_array()
        .expect("completed task missing list")
        .is_empty());
    assert!(completed_gate["completion_gate"]["next_actions"]
        .as_array()
        .expect("completed task next actions")
        .is_empty());

    let completed_context = call_tool(&ctx, "task_context", &json!({"task_id": task_id}));
    assert_eq!(completed_context["ok"], true);
    assert_eq!(completed_context["task"]["status"], "completed");
    assert_eq!(completed_context["completion_gate"]["ready"], true);
}

#[test]
fn slice_completion_gate_reports_missing_acceptance_without_changing_slice_state() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({
            "objective": "Slice completion gate",
            "contract": {"completion_policy": {"require_slice_commits": true}}
        }),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");
    let slice = call_tool(
        &ctx,
        "start_slice",
        &json!({
            "task_id": task_id,
            "slice_id": "S-gated",
            "title": "受门禁保护的 Slice",
            "acceptance_checks": [
                {"id": "focused-e2e", "verification_key": "focused-e2e"}
            ]
        }),
    );
    assert_eq!(slice["ok"], true);

    let blocked = call_tool(
        &ctx,
        "complete_slice",
        &json!({"task_id": task_id, "slice_id": "S-gated"}),
    );
    assert_eq!(blocked["ok"], false);
    assert_eq!(blocked["error"]["code"], "SLICE_COMPLETION_GATE_FAILED");
    assert_eq!(blocked["task"]["slices"][0]["status"], "in_progress");
    let missing_codes = blocked["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value["code"].as_str())
        .collect::<Vec<_>>();
    assert!(missing_codes.contains(&"slice_acceptance_missing"));
    assert!(missing_codes.contains(&"slice_commit_missing"));
}

#[test]
fn failed_tool_opens_recovery_and_same_step_success_resolves_it() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "工具失败恢复到原步骤"}),
    );
    let task_id = started["task"]["id"].as_str().expect("task id");
    let script = "import pathlib,sys; p=pathlib.Path('recovery.flag'); existed=p.exists(); p.write_text('ready'); sys.exit(0 if existed else 1)";

    let failed = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", script],
            "verification_kind": "test",
            "verification_key": "recovery-retry",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(failed["command_ok"], false);
    assert_eq!(failed["task_recovery"]["status"], "open");
    assert_eq!(
        failed["task_recovery"]["recovery"]["failed_step"],
        "exec_command"
    );
    assert_eq!(
        failed["task_recovery"]["recovery"]["workspace_mutated"],
        true
    );

    let blocked = call_tool(&ctx, "task_gate_status", &json!({"task_id": task_id}));
    let has_recovery = blocked["completion_gate"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value["code"] == "recovery_open");
    assert!(has_recovery);

    let unrelated = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", "print('unrelated-success')"],
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(unrelated["command_ok"], true);
    assert!(unrelated.get("task_recovery").is_none());
    assert_eq!(
        ctx.harness
            .task(task_id)
            .unwrap()
            .recovery
            .as_ref()
            .unwrap()
            .status,
        anchor_lib::harness::TaskRecoveryStatus::Open
    );

    let passed = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "executable": TEST_PYTHON,
            "args": ["-c", script],
            "verification_kind": "test",
            "verification_key": "recovery-retry",
            "yield_time_ms": 30000
        }),
    );
    assert_eq!(passed["command_ok"], true);
    assert_eq!(passed["task_recovery"]["status"], "resolved");
    assert_eq!(
        passed["task_recovery"]["recovery"]["resolved_by_step"],
        "exec_command"
    );
    assert!(passed["supersedes"]
        .as_array()
        .is_some_and(|items| !items.is_empty()));

    let recovered = call_tool(
        &ctx,
        "task_gate_status",
        &json!({"task_id": task_id, "detail": "full"}),
    );
    let still_open = recovered["completion_gate"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value["code"] == "recovery_open");
    assert!(!still_open);
    assert_eq!(recovered["task"]["recovery"]["status"], "resolved");
}

#[test]
fn catalog_v34_exposes_task_governance_facades_and_schemas() {
    let tools = anchor_lib::tools::list_tools_for_profile("advanced");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in ["complete_work_session", "task", "slice"] {
        assert!(names.contains(&expected), "缺少工具 {expected}");
    }
    for internal in [
        "task_gate_status",
        "start_slice",
        "update_slice",
        "complete_slice",
    ] {
        assert!(!names.contains(&internal), "内部工具泄漏 {internal}");
    }
    let task = tools
        .iter()
        .find(|tool| tool["name"] == "task")
        .expect("task facade");
    assert!(task["inputSchema"]["properties"]["operation"]["enum"]
        .as_array()
        .expect("task operations")
        .iter()
        .any(|operation| operation == "gate_status"));
    let slice = tools
        .iter()
        .find(|tool| tool["name"] == "slice")
        .expect("slice facade");
    let slice_operations = slice["inputSchema"]["properties"]["operation"]["enum"]
        .as_array()
        .expect("slice operations");
    for expected in ["start", "update", "complete"] {
        assert!(slice_operations
            .iter()
            .any(|operation| operation == expected));
    }
    let begin = tools
        .iter()
        .find(|tool| tool["name"] == "begin_work_session")
        .expect("begin_work_session schema");
    for property in ["contract", "phase", "slices", "working_set"] {
        assert!(
            begin["inputSchema"]["properties"].get(property).is_some(),
            "begin_work_session 缺少 {property} schema"
        );
    }
    let verification_branches = begin["inputSchema"]["properties"]["contract"]["properties"]
        ["required_verifications"]["items"]["anyOf"]
        .as_array()
        .expect("verification requirement anyOf");
    assert_eq!(verification_branches.len(), 4);
    for branch in verification_branches {
        let required = branch["required"]
            .as_array()
            .expect("verification requirement branch required");
        assert!(
            required.iter().any(|value| value == "id"),
            "verification requirement 分支必须显式要求 id: {branch}"
        );
    }
}
