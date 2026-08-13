use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde_json::{json, Value};
use uuid::Uuid;

use super::model::{StageCommitReceipt, StageCommitStatus};
use crate::tools::command_session as session;
use crate::tools::context::ToolContext;
use crate::tools::policy::validate_tool_arguments_for_workspace;
use crate::tools::workspace::WorkspaceError;
use crate::tools::{exec, session as dev_session, CancellationToken};

const DEFAULT_CHECK_TIMEOUT_MS: u64 = 600_000;
const MAX_STAGE_PATHS: usize = 256;
const MAX_REQUIRED_CHECKS: usize = 16;
const MAX_PROCESS_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Debug)]
struct ProcessOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn ensure_real_index_clean(
    ctx: &ToolContext,
    cancellation: &CancellationToken,
) -> Result<(), WorkspaceError> {
    let real_index = run_git(
        ctx,
        &["diff", "--cached", "--quiet"],
        None,
        Duration::from_secs(10),
        cancellation,
    )?;
    if real_index.exit_code == Some(0) {
        return Ok(());
    }
    Err(stage_error(
        "STAGE_COMMIT_INDEX_NOT_CLEAN",
        "stage_commit requires the real Git index to remain clean so it can safely commit through a temporary index and realign the index afterward.",
        false,
        process_details(&real_index),
    ))
}

fn acquire_stage_commit_lock(ctx: &ToolContext) -> Result<StageCommitLock, WorkspaceError> {
    let path = ctx
        .harness
        .store_root()
        .join("workspaces")
        .join(ctx.harness.workspace_id())
        .join("stage-commit.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            stage_error(
                "STAGE_COMMIT_LOCK_FAILED",
                error.to_string(),
                true,
                json!({}),
            )
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            stage_error(
                "STAGE_COMMIT_LOCK_FAILED",
                error.to_string(),
                true,
                json!({}),
            )
        })?;
    file.try_lock_exclusive().map_err(|_| {
        stage_error(
            "STAGE_COMMIT_BUSY",
            "Another stage_commit workflow is already active for this workspace.",
            true,
            json!({}),
        )
    })?;
    Ok(StageCommitLock { file })
}

impl Drop for StageCommitLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

struct TempIndex {
    path: PathBuf,
}

struct StageCommitLock {
    file: File,
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(PathBuf::from(format!("{}.lock", self.path.display())));
    }
}

pub fn run(
    ctx: &ToolContext,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    let task_id = required_string(args, "task_id")?;
    let expected_head = required_string(args, "expected_head")?;
    let expected_fingerprint = required_string(args, "expected_fingerprint")?;
    let message = required_string(args, "message")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let paths = parse_paths(args)?;
    let checks = parse_checks(args)?;
    let check_timeout_ms = args
        .get("check_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_CHECK_TIMEOUT_MS)
        .clamp(1_000, 600_000);
    let checkpoint = args.get("session_checkpoint").cloned();
    let deferred = args
        .get("execution_mode")
        .and_then(Value::as_str)
        .is_some_and(|value| value == "deferred");
    let wait_timeout_ms = args
        .get("wait_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(60_000);
    let _workflow_lock = acquire_stage_commit_lock(ctx)?;

    if let Some(mut existing) = ctx
        .harness
        .load_stage_commit_receipt(idempotency_key)
        .map_err(harness_error)?
    {
        return match existing.status {
            StageCommitStatus::Completed => receipt_response(&existing, true, false),
            StageCommitStatus::CommittedCheckpointPending | StageCommitStatus::Committed => {
                resume_checkpoint(ctx, &mut existing, checkpoint.as_ref())
            }
            StageCommitStatus::CommittedRecoveryRequired => {
                receipt_response(&existing, false, false)
            }
            StageCommitStatus::CheckRunning if deferred => advance_deferred_workflow(
                ctx,
                &mut existing,
                cancellation,
                wait_timeout_ms,
                checkpoint.as_ref(),
                false,
            ),
            StageCommitStatus::CheckRunning => receipt_response(&existing, false, true),
            StageCommitStatus::Started if deferred => advance_deferred_workflow(
                ctx,
                &mut existing,
                cancellation,
                wait_timeout_ms,
                checkpoint.as_ref(),
                false,
            ),
            StageCommitStatus::Started | StageCommitStatus::ChecksPassed => {
                let current = super::state::capture_baseline(ctx.workspace.root());
                if current.head.as_deref() != Some(expected_head)
                    || current.worktree_fingerprint != expected_fingerprint
                {
                    Err(stage_error(
                        "STAGE_COMMIT_INCOMPLETE",
                        "A previous stage_commit attempt did not complete and the workspace has changed.",
                        false,
                        json!({
                            "workflow_id": existing.workflow_id,
                            "status": existing.status,
                            "current_head": current.head,
                            "current_fingerprint": current.worktree_fingerprint
                        }),
                    ))
                } else {
                    execute_new_workflow(
                        ctx,
                        args,
                        cancellation,
                        task_id,
                        expected_head,
                        expected_fingerprint,
                        message,
                        idempotency_key,
                        paths,
                        checks,
                        check_timeout_ms,
                        checkpoint.as_ref(),
                        Some(existing),
                        false,
                        0,
                    )
                }
            }
            StageCommitStatus::Failed => Err(stage_error(
                "STAGE_COMMIT_PREVIOUSLY_FAILED",
                "This idempotency key belongs to a failed workflow. Use a new key after resolving the failure.",
                false,
                json!({
                    "workflow_id": existing.workflow_id,
                    "error": existing.error
                }),
            )),
        };
    }

    execute_new_workflow(
        ctx,
        args,
        cancellation,
        task_id,
        expected_head,
        expected_fingerprint,
        message,
        idempotency_key,
        paths,
        checks,
        check_timeout_ms,
        checkpoint.as_ref(),
        None,
        deferred,
        wait_timeout_ms,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_new_workflow(
    ctx: &ToolContext,
    original_args: &Value,
    cancellation: &CancellationToken,
    task_id: &str,
    expected_head: &str,
    expected_fingerprint: &str,
    message: &str,
    idempotency_key: &str,
    paths: Vec<String>,
    checks: Vec<String>,
    check_timeout_ms: u64,
    checkpoint: Option<&Value>,
    existing_receipt: Option<StageCommitReceipt>,
    deferred: bool,
    wait_timeout_ms: u64,
) -> Result<Value, WorkspaceError> {
    let task = ctx.harness.task(task_id).map_err(harness_error)?;
    if !task.status.is_writable() {
        return Err(stage_error(
            "TASK_NOT_WRITABLE",
            "The task is not in a writable state.",
            false,
            json!({"task_id": task_id, "status": task.status}),
        ));
    }
    ensure_real_index_clean(ctx, cancellation)?;
    ctx.harness.check_baseline(task_id).map_err(harness_error)?;
    ensure_repository_root(ctx, cancellation)?;

    let before = super::state::capture_baseline(ctx.workspace.root());
    if before.head.as_deref() != Some(expected_head)
        || before.worktree_fingerprint != expected_fingerprint
    {
        return Err(stage_error(
            "STAGE_COMMIT_CAS_FAILED",
            "The observed HEAD or workspace fingerprint no longer matches.",
            true,
            json!({
                "expected_head": expected_head,
                "current_head": before.head,
                "expected_fingerprint": expected_fingerprint,
                "current_fingerprint": before.worktree_fingerprint
            }),
        ));
    }

    let changed_before = changed_paths(ctx, cancellation)?;
    ensure_changes_are_selected(&changed_before, &paths)?;
    if changed_before.is_empty() {
        return Err(stage_error(
            "STAGE_COMMIT_EMPTY",
            "No Git changes are available to commit.",
            false,
            json!({"paths": paths}),
        ));
    }

    let is_new_receipt = existing_receipt.is_none();
    let now = timestamp();
    let mut receipt = existing_receipt.unwrap_or_else(|| StageCommitReceipt {
        workflow_id: Uuid::new_v4().simple().to_string(),
        idempotency_key: idempotency_key.to_string(),
        task_id: task_id.to_string(),
        status: StageCommitStatus::Started,
        expected_head: expected_head.to_string(),
        expected_fingerprint: expected_fingerprint.to_string(),
        message: message.to_string(),
        reason: original_args
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        required_checks: checks.clone(),
        check_timeout_ms,
        current_check_index: 0,
        current_session_id: None,
        session_checkpoint: checkpoint.cloned(),
        paths: paths.clone(),
        checks: Vec::new(),
        verification_ids: Vec::new(),
        commit_sha: None,
        committed_files: Vec::new(),
        working_tree_files: Vec::new(),
        runtime_artifacts: Vec::new(),
        ignored_files: Vec::new(),
        baseline_refreshed: false,
        checkpoint_hash: None,
        checkpoint_count: None,
        error: None,
        created_at: now.clone(),
        updated_at: now,
    });
    if receipt.message.is_empty() {
        receipt.message = message.to_string();
    }
    if receipt.required_checks.is_empty() {
        receipt.required_checks = checks.clone();
    }
    if receipt.session_checkpoint.is_none() {
        receipt.session_checkpoint = checkpoint.cloned();
    }
    receipt.check_timeout_ms = check_timeout_ms;
    save_receipt(ctx, &receipt)?;
    if is_new_receipt {
        let _ = ctx.harness.record_operation(
            Some(&receipt.workflow_id),
            Some(task_id),
            None,
            "stage_commit",
            "started",
            json!({
                "reason": original_args.get("reason"),
                "paths": paths,
                "required_checks": checks,
                "message": message,
                "execution_mode": if deferred { "deferred" } else { "blocking" }
            }),
            json!({"ok": true}),
        );
    }

    if deferred {
        return advance_deferred_workflow(
            ctx,
            &mut receipt,
            cancellation,
            wait_timeout_ms,
            checkpoint,
            false,
        );
    }

    for command in checks.into_iter().skip(receipt.checks.len()) {
        let mut result = run_required_check(
            ctx,
            &receipt.task_id,
            &command,
            check_timeout_ms,
            cancellation,
        )?;
        let passed = result.get("command_ok").and_then(Value::as_bool) == Some(true);
        let verification = ctx
            .harness
            .record_verification(
                task_id,
                verification_kind(&command),
                &command,
                None,
                None,
                None,
                result
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok()),
                passed,
                result.get("duration_ms").and_then(Value::as_u64),
                None,
                "blocking",
                true,
            )
            .map_err(harness_error)?;
        if let Some(object) = result.as_object_mut() {
            object.insert(
                "verification_id".into(),
                Value::String(verification.id.clone()),
            );
            object.insert("verification_kind".into(), Value::String(verification.kind));
            object.insert(
                "verification_status".into(),
                Value::String(verification.status),
            );
        }
        receipt.verification_ids.push(verification.id);
        receipt.checks.push(result.clone());
        receipt.updated_at = timestamp();
        if !passed {
            receipt.status = StageCommitStatus::Failed;
            receipt.error = Some(json!({
                "code": "STAGE_COMMIT_CHECK_FAILED",
                "command": command,
                "result": result
            }));
            save_receipt(ctx, &receipt)?;
            finish_operation(ctx, &receipt, false);
            return Err(stage_error(
                "STAGE_COMMIT_CHECK_FAILED",
                "A required check failed; no files were staged and HEAD was not changed.",
                false,
                json!({
                    "workflow_id": receipt.workflow_id,
                    "checks": receipt.checks
                }),
            ));
        }
        save_receipt(ctx, &receipt)?;
    }

    let after_checks = super::state::capture_baseline(ctx.workspace.root());
    if after_checks.head.as_deref() != Some(expected_head)
        || after_checks.worktree_fingerprint != expected_fingerprint
    {
        receipt.status = StageCommitStatus::Failed;
        receipt.error = Some(json!({
            "code": "STAGE_COMMIT_CHECK_MODIFIED_WORKSPACE",
            "current_head": after_checks.head,
            "current_fingerprint": after_checks.worktree_fingerprint
        }));
        receipt.updated_at = timestamp();
        save_receipt(ctx, &receipt)?;
        finish_operation(ctx, &receipt, false);
        return Err(stage_error(
            "STAGE_COMMIT_CHECK_MODIFIED_WORKSPACE",
            "A required check modified the workspace or HEAD; inspect the changes before committing.",
            false,
            receipt.error.clone().unwrap_or_else(|| json!({})),
        ));
    }
    ensure_real_index_clean(ctx, cancellation)?;
    receipt.status = StageCommitStatus::ChecksPassed;
    receipt.updated_at = timestamp();
    save_receipt(ctx, &receipt)?;

    let temp_index = create_temp_index(ctx)?;
    git_expect_success(
        ctx,
        &["read-tree", expected_head],
        Some(&temp_index.path),
        Duration::from_secs(30),
        cancellation,
        "STAGE_COMMIT_INDEX_FAILED",
    )?;
    let mut add_args = vec!["add".to_string(), "-A".to_string(), "--".to_string()];
    add_args.extend(paths.iter().cloned());
    git_expect_success_owned(
        ctx,
        &add_args,
        Some(&temp_index.path),
        Duration::from_secs(60),
        cancellation,
        "STAGE_COMMIT_STAGE_FAILED",
    )?;

    let staged = git_paths(
        ctx,
        &["diff", "--cached", "--name-only", "-z"],
        Some(&temp_index.path),
        cancellation,
    )?;
    ensure_changes_are_selected(&staged, &paths)?;
    if staged.is_empty() {
        receipt.status = StageCommitStatus::Failed;
        receipt.error = Some(json!({"code": "STAGE_COMMIT_EMPTY"}));
        receipt.updated_at = timestamp();
        save_receipt(ctx, &receipt)?;
        finish_operation(ctx, &receipt, false);
        return Err(stage_error(
            "STAGE_COMMIT_EMPTY",
            "The selected paths produced an empty staged diff.",
            false,
            json!({"paths": paths}),
        ));
    }

    let pre_commit = super::state::capture_baseline(ctx.workspace.root());
    if pre_commit.head.as_deref() != Some(expected_head)
        || pre_commit.worktree_fingerprint != expected_fingerprint
    {
        return Err(stage_error(
            "STAGE_COMMIT_CAS_FAILED",
            "The workspace changed after checks and before commit.",
            true,
            json!({
                "current_head": pre_commit.head,
                "current_fingerprint": pre_commit.worktree_fingerprint
            }),
        ));
    }

    git_expect_success(
        ctx,
        &["commit", "--no-gpg-sign", "-m", message],
        Some(&temp_index.path),
        Duration::from_millis(check_timeout_ms),
        cancellation,
        "STAGE_COMMIT_GIT_FAILED",
    )?;
    let commit_sha = git_text(
        ctx,
        &["rev-parse", "HEAD"],
        None,
        Duration::from_secs(10),
        cancellation,
    )?;
    receipt.commit_sha = Some(commit_sha.clone());
    receipt.committed_files = git_paths(
        ctx,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            &commit_sha,
        ],
        None,
        cancellation,
    )?;
    receipt.status = StageCommitStatus::Committed;
    receipt.updated_at = timestamp();
    save_receipt(ctx, &receipt)?;

    if let Err(error) = git_expect_success(
        ctx,
        &["reset", "--mixed", "--quiet", "HEAD"],
        None,
        Duration::from_secs(30),
        cancellation,
        "STAGE_COMMIT_INDEX_REALIGN_FAILED",
    ) {
        receipt.status = StageCommitStatus::CommittedRecoveryRequired;
        receipt.error = Some(error.to_error_value());
        receipt.updated_at = timestamp();
        save_receipt(ctx, &receipt)?;
        finish_operation(ctx, &receipt, false);
        return receipt_response(&receipt, false, false);
    }

    let post_commit_changes = changed_paths(ctx, cancellation)?;
    receipt.working_tree_files = post_commit_changes.clone();
    if !post_commit_changes.is_empty() {
        receipt.status = StageCommitStatus::CommittedRecoveryRequired;
        receipt.error = Some(json!({
            "code": "STAGE_COMMIT_POST_COMMIT_DRIFT",
            "changed_paths": post_commit_changes
        }));
        receipt.updated_at = timestamp();
        save_receipt(ctx, &receipt)?;
        finish_operation(ctx, &receipt, false);
        return receipt_response(&receipt, false, false);
    }

    ctx.harness
        .refresh_expected_state_for_operation(task_id, Some(&receipt.workflow_id))
        .map_err(harness_error)?;
    receipt.baseline_refreshed = true;
    let _ = ctx
        .harness
        .set_latest_change(task_id, &commit_sha)
        .map_err(harness_error)?;
    let _ = ctx
        .harness
        .save_change_set(
            task_id,
            &commit_sha,
            receipt.committed_files.clone(),
            receipt.working_tree_files.clone(),
            receipt.runtime_artifacts.clone(),
            receipt.ignored_files.clone(),
            receipt.verification_ids.clone(),
        )
        .map_err(harness_error)?;
    let _ = ctx.harness.record_event(
        task_id,
        "stage_commit_committed",
        Some("stage_commit"),
        json!({"paths": staged, "message": message}),
        json!({
            "ok": true,
            "workflow_id": receipt.workflow_id,
            "commit_sha": commit_sha,
            "checks": receipt.checks
        }),
    );

    if checkpoint.is_some() {
        return resume_checkpoint(ctx, &mut receipt, checkpoint);
    }
    receipt.status = StageCommitStatus::Completed;
    receipt.updated_at = timestamp();
    save_receipt(ctx, &receipt)?;
    finish_operation(ctx, &receipt, true);
    receipt_response(&receipt, true, false)
}

pub fn status(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = required_string(args, "task_id")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let receipt = ctx
        .harness
        .load_stage_commit_receipt(idempotency_key)
        .map_err(harness_error)?
        .ok_or_else(|| {
            stage_error(
                "STAGE_COMMIT_NOT_FOUND",
                "No stage_commit workflow exists for this idempotency key.",
                false,
                json!({"idempotency_key": idempotency_key}),
            )
        })?;
    ensure_receipt_task(&receipt, task_id)?;
    receipt_response(
        &receipt,
        matches!(&receipt.status, StageCommitStatus::Completed),
        stage_status_retryable(&receipt.status),
    )
}

pub fn wait(
    ctx: &ToolContext,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    let task_id = required_string(args, "task_id")?;
    let idempotency_key = required_string(args, "idempotency_key")?;
    let wait_timeout_ms = args
        .get("wait_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(60_000);
    let restart_lost_check = args
        .get("restart_lost_check")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let checkpoint = args.get("session_checkpoint");
    let _workflow_lock = acquire_stage_commit_lock(ctx)?;
    let mut receipt = ctx
        .harness
        .load_stage_commit_receipt(idempotency_key)
        .map_err(harness_error)?
        .ok_or_else(|| {
            stage_error(
                "STAGE_COMMIT_NOT_FOUND",
                "No stage_commit workflow exists for this idempotency key.",
                false,
                json!({"idempotency_key": idempotency_key}),
            )
        })?;
    ensure_receipt_task(&receipt, task_id)?;
    advance_deferred_workflow(
        ctx,
        &mut receipt,
        cancellation,
        wait_timeout_ms,
        checkpoint,
        restart_lost_check,
    )
}

fn ensure_receipt_task(receipt: &StageCommitReceipt, task_id: &str) -> Result<(), WorkspaceError> {
    if receipt.task_id == task_id {
        return Ok(());
    }
    Err(stage_error(
        "STAGE_COMMIT_TASK_MISMATCH",
        "The stage_commit workflow belongs to another task.",
        false,
        json!({
            "expected_task_id": receipt.task_id,
            "observed_task_id": task_id
        }),
    ))
}

fn advance_deferred_workflow(
    ctx: &ToolContext,
    receipt: &mut StageCommitReceipt,
    cancellation: &CancellationToken,
    wait_timeout_ms: u64,
    checkpoint_override: Option<&Value>,
    restart_lost_check: bool,
) -> Result<Value, WorkspaceError> {
    if let Some(checkpoint) = checkpoint_override {
        receipt.session_checkpoint = Some(checkpoint.clone());
        receipt.updated_at = timestamp();
        save_receipt(ctx, receipt)?;
    }
    let deadline = Instant::now() + Duration::from_millis(wait_timeout_ms);
    loop {
        if cancellation.is_cancelled() {
            return Err(stage_error(
                "REQUEST_CANCELLED",
                "Waiting for stage_commit was cancelled; the durable workflow remains available.",
                true,
                json!({"workflow_id": receipt.workflow_id}),
            ));
        }
        match receipt.status.clone() {
            StageCommitStatus::Started => {
                if receipt.current_check_index >= receipt.required_checks.len() {
                    receipt.status = StageCommitStatus::ChecksPassed;
                    receipt.updated_at = timestamp();
                    save_receipt(ctx, receipt)?;
                    continue;
                }
                start_deferred_check(ctx, receipt, cancellation)?;
                if wait_timeout_ms == 0 {
                    return receipt_response(receipt, false, true);
                }
            }
            StageCommitStatus::CheckRunning => {
                let Some(session_id) = receipt.current_session_id.clone() else {
                    if restart_lost_check {
                        receipt.status = StageCommitStatus::Started;
                        receipt.error = None;
                        receipt.updated_at = timestamp();
                        save_receipt(ctx, receipt)?;
                        continue;
                    }
                    receipt.error = Some(json!({
                        "code": "STAGE_COMMIT_CHECK_SESSION_LOST",
                        "message": "The retained check session is unavailable. Pass restart_lost_check=true to explicitly rerun this check."
                    }));
                    receipt.updated_at = timestamp();
                    save_receipt(ctx, receipt)?;
                    return receipt_response(receipt, false, true);
                };
                let remaining_ms = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(60_000) as u64;
                if wait_timeout_ms == 0 || remaining_ms == 0 {
                    return receipt_response(receipt, false, true);
                }
                let waited = match session::wait_command(
                    &ctx.sessions,
                    &json!({
                        "session_id": session_id,
                        "timeout_ms": remaining_ms,
                        "return_incremental_output": false,
                        "limit": 65_536
                    }),
                ) {
                    Ok(value) => value,
                    Err(error) if error.to_error_value()["code"] == "SESSION_NOT_FOUND" => {
                        receipt.current_session_id = None;
                        receipt.error = Some(json!({
                            "code": "STAGE_COMMIT_CHECK_SESSION_LOST",
                            "message": "The retained check session was lost, usually because the service restarted. Pass restart_lost_check=true to explicitly rerun this check."
                        }));
                        receipt.updated_at = timestamp();
                        save_receipt(ctx, receipt)?;
                        if restart_lost_check {
                            receipt.status = StageCommitStatus::Started;
                            receipt.error = None;
                            save_receipt(ctx, receipt)?;
                            continue;
                        }
                        return receipt_response(receipt, false, true);
                    }
                    Err(error) => return Err(error),
                };
                if waited.get("state").and_then(Value::as_str) == Some("running") {
                    return receipt_response(receipt, false, true);
                }
                let result = session::write_stdin(
                    &ctx.sessions,
                    &json!({
                        "session_id": session_id,
                        "chars": "",
                        "yield_time_ms": 0,
                        "max_output_bytes": 65_536
                    }),
                )?;
                persist_deferred_check_result(ctx, receipt, result)?;
                if matches!(&receipt.status, StageCommitStatus::Failed) {
                    return receipt_response(receipt, false, false);
                }
            }
            StageCommitStatus::ChecksPassed => {
                return commit_deferred_receipt(ctx, receipt, cancellation, checkpoint_override);
            }
            StageCommitStatus::Committed | StageCommitStatus::CommittedCheckpointPending => {
                let checkpoint = checkpoint_override
                    .cloned()
                    .or_else(|| receipt.session_checkpoint.clone());
                return resume_checkpoint(ctx, receipt, checkpoint.as_ref());
            }
            StageCommitStatus::Completed => return receipt_response(receipt, true, false),
            StageCommitStatus::CommittedRecoveryRequired | StageCommitStatus::Failed => {
                return receipt_response(receipt, false, stage_status_retryable(&receipt.status));
            }
        }
        if wait_timeout_ms > 0 && Instant::now() >= deadline {
            return receipt_response(receipt, false, true);
        }
    }
}

fn start_deferred_check(
    ctx: &ToolContext,
    receipt: &mut StageCommitReceipt,
    cancellation: &CancellationToken,
) -> Result<(), WorkspaceError> {
    let command = receipt
        .required_checks
        .get(receipt.current_check_index)
        .cloned()
        .ok_or_else(|| invalid_argument("current deferred check is out of range"))?;
    let arguments = json!({
        "cmd": command,
        "timeout_ms": receipt.check_timeout_ms,
        "yield_time_ms": 0,
        "max_output_bytes": 65_536,
        "filesystem_scope": "workspace",
        "reason": "deferred stage_commit required check"
    });
    validate_tool_arguments_for_workspace(
        "exec_command",
        &arguments,
        &ctx.policy,
        Some(&ctx.workspace),
    )
    .map_err(|error| {
        stage_error(
            "STAGE_COMMIT_CHECK_REJECTED",
            error.to_string(),
            false,
            json!({"command": command}),
        )
    })?;
    let result = exec::exec_command_with_cancellation(
        ctx,
        &arguments,
        cancellation,
        Some(&receipt.task_id),
        None,
    )?;
    if result.get("status").and_then(Value::as_str) == Some("running") {
        receipt.current_session_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        receipt.status = StageCommitStatus::CheckRunning;
        receipt.error = None;
        receipt.updated_at = timestamp();
        save_receipt(ctx, receipt)?;
        return Ok(());
    }
    persist_deferred_check_result(ctx, receipt, result)
}

fn persist_deferred_check_result(
    ctx: &ToolContext,
    receipt: &mut StageCommitReceipt,
    mut result: Value,
) -> Result<(), WorkspaceError> {
    let command = receipt
        .required_checks
        .get(receipt.current_check_index)
        .cloned()
        .ok_or_else(|| invalid_argument("completed deferred check is out of range"))?;
    let passed = result.get("command_ok").and_then(Value::as_bool) == Some(true);
    let verification = ctx
        .harness
        .record_verification(
            &receipt.task_id,
            verification_kind(&command),
            &command,
            None,
            None,
            None,
            result
                .get("exit_code")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok()),
            passed,
            result
                .get("duration_ms")
                .or_else(|| result.get("elapsed_ms"))
                .and_then(Value::as_u64),
            None,
            "blocking",
            true,
        )
        .map_err(harness_error)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("command".into(), Value::String(command.clone()));
        object.insert(
            "verification_id".into(),
            Value::String(verification.id.clone()),
        );
        object.insert("verification_kind".into(), Value::String(verification.kind));
        object.insert(
            "verification_status".into(),
            Value::String(verification.status),
        );
    }
    receipt.verification_ids.push(verification.id);
    receipt.checks.push(result.clone());
    receipt.current_check_index = receipt.current_check_index.saturating_add(1);
    receipt.current_session_id = None;
    receipt.updated_at = timestamp();
    if passed {
        receipt.status = StageCommitStatus::Started;
        receipt.error = None;
    } else {
        receipt.status = StageCommitStatus::Failed;
        receipt.error = Some(json!({
            "code": "STAGE_COMMIT_CHECK_FAILED",
            "command": command,
            "result": result
        }));
        finish_operation(ctx, receipt, false);
    }
    save_receipt(ctx, receipt)
}

fn commit_deferred_receipt(
    ctx: &ToolContext,
    receipt: &StageCommitReceipt,
    cancellation: &CancellationToken,
    checkpoint_override: Option<&Value>,
) -> Result<Value, WorkspaceError> {
    let checkpoint = checkpoint_override
        .cloned()
        .or_else(|| receipt.session_checkpoint.clone());
    let args = json!({
        "task_id": receipt.task_id,
        "expected_head": receipt.expected_head,
        "expected_fingerprint": receipt.expected_fingerprint,
        "message": receipt.message,
        "idempotency_key": receipt.idempotency_key,
        "paths": receipt.paths,
        "required_checks": [],
        "check_timeout_ms": receipt.check_timeout_ms,
        "reason": receipt.reason,
        "session_checkpoint": checkpoint.clone()
    });
    execute_new_workflow(
        ctx,
        &args,
        cancellation,
        &receipt.task_id,
        &receipt.expected_head,
        &receipt.expected_fingerprint,
        &receipt.message,
        &receipt.idempotency_key,
        receipt.paths.clone(),
        Vec::new(),
        receipt.check_timeout_ms,
        checkpoint.as_ref(),
        Some(receipt.clone()),
        false,
        0,
    )
}

fn stage_status_retryable(status: &StageCommitStatus) -> bool {
    matches!(
        status,
        StageCommitStatus::Started
            | StageCommitStatus::CheckRunning
            | StageCommitStatus::ChecksPassed
            | StageCommitStatus::Committed
            | StageCommitStatus::CommittedCheckpointPending
    )
}

fn resume_checkpoint(
    ctx: &ToolContext,
    receipt: &mut StageCommitReceipt,
    checkpoint: Option<&Value>,
) -> Result<Value, WorkspaceError> {
    let Some(checkpoint) = checkpoint else {
        receipt.status = StageCommitStatus::CommittedCheckpointPending;
        receipt.updated_at = timestamp();
        save_receipt(ctx, receipt)?;
        finish_operation(ctx, receipt, false);
        return receipt_response(receipt, false, true);
    };
    match dev_session::checkpoint(ctx, checkpoint, None) {
        Ok(result) => {
            receipt.checkpoint_hash = result
                .get("content_hash")
                .and_then(Value::as_str)
                .map(str::to_string);
            receipt.checkpoint_count = result.get("checkpoint_count").and_then(Value::as_u64);
            receipt.status = StageCommitStatus::Completed;
            receipt.error = None;
            receipt.updated_at = timestamp();
            save_receipt(ctx, receipt)?;
            finish_operation(ctx, receipt, true);
            receipt_response(receipt, true, false)
        }
        Err(error) => {
            receipt.status = StageCommitStatus::CommittedCheckpointPending;
            receipt.error = Some(error.to_error_value());
            receipt.updated_at = timestamp();
            save_receipt(ctx, receipt)?;
            finish_operation(ctx, receipt, false);
            receipt_response(receipt, false, true)
        }
    }
}

fn run_required_check(
    ctx: &ToolContext,
    task_id: &str,
    command: &str,
    timeout_ms: u64,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    let arguments = json!({
        "cmd": command,
        "timeout_ms": timeout_ms,
        "yield_time_ms": 30_000,
        "max_output_bytes": 65_536,
        "filesystem_scope": "workspace",
        "reason": "stage_commit required check"
    });
    validate_tool_arguments_for_workspace(
        "exec_command",
        &arguments,
        &ctx.policy,
        Some(&ctx.workspace),
    )
    .map_err(|error| {
        stage_error(
            "STAGE_COMMIT_CHECK_REJECTED",
            error.to_string(),
            false,
            json!({"command": command}),
        )
    })?;
    let mut result =
        exec::exec_command_with_cancellation(ctx, &arguments, cancellation, Some(task_id), None)?;
    while result.get("status").and_then(Value::as_str) == Some("running") {
        let session_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                stage_error(
                    "STAGE_COMMIT_CHECK_SESSION_INVALID",
                    "A running check did not return a session_id.",
                    false,
                    json!({"command": command}),
                )
            })?
            .to_string();
        if cancellation.is_cancelled() {
            let _ = session::kill_session(
                &ctx.sessions,
                &json!({"session_id": session_id, "signal": "TERM", "wait_ms": 5_000}),
            );
            return Err(stage_error(
                "REQUEST_CANCELLED",
                "stage_commit was cancelled while running a required check.",
                true,
                json!({"command": command}),
            ));
        }
        result = session::write_stdin(
            &ctx.sessions,
            &json!({
                "session_id": session_id,
                "chars": "",
                "yield_time_ms": 30_000,
                "max_output_bytes": 65_536
            }),
        )?;
    }
    Ok(json!({
        "command": command,
        "command_ok": result.get("command_ok").cloned().unwrap_or(Value::Null),
        "exit_code": result.get("exit_code").cloned().unwrap_or(Value::Null),
        "termination_reason": result.get("termination_reason").cloned().unwrap_or(Value::Null),
        "duration_ms": result.get("duration_ms").cloned().unwrap_or(Value::Null),
        "stdout": result.get("stdout").cloned().unwrap_or(Value::String(String::new())),
        "stderr": result.get("stderr").cloned().unwrap_or(Value::String(String::new())),
        "stdout_truncated": result.get("stdout_truncated").cloned().unwrap_or(Value::Bool(false)),
        "stderr_truncated": result.get("stderr_truncated").cloned().unwrap_or(Value::Bool(false)),
        "output_refs": result.get("output_refs").cloned().unwrap_or_else(|| json!({}))
    }))
}

fn verification_kind(command: &str) -> &str {
    let normalized = command.to_ascii_lowercase();
    if normalized.contains("diff --check") {
        "diff_check"
    } else if normalized.contains("lint") || normalized.contains("clippy") {
        "lint"
    } else if normalized.contains("test") {
        "test"
    } else if normalized.contains("check") {
        "check"
    } else if normalized.contains("build") {
        "build"
    } else {
        "command"
    }
}

fn ensure_repository_root(
    ctx: &ToolContext,
    cancellation: &CancellationToken,
) -> Result<(), WorkspaceError> {
    let top = git_text(
        ctx,
        &["rev-parse", "--show-toplevel"],
        None,
        Duration::from_secs(10),
        cancellation,
    )?;
    let top = PathBuf::from(top).canonicalize().map_err(|error| {
        stage_error(
            "STAGE_COMMIT_REPOSITORY_INVALID",
            error.to_string(),
            false,
            json!({}),
        )
    })?;
    if top != ctx.workspace.root() {
        return Err(stage_error(
            "STAGE_COMMIT_REPOSITORY_SCOPE",
            "stage_commit requires the configured workspace to be the Git repository root.",
            false,
            json!({"repository_root": top.display().to_string()}),
        ));
    }
    Ok(())
}

fn create_temp_index(ctx: &ToolContext) -> Result<TempIndex, WorkspaceError> {
    let dir = ctx.harness.store_root().join("tmp");
    fs::create_dir_all(&dir).map_err(|error| {
        stage_error(
            "STAGE_COMMIT_INDEX_FAILED",
            error.to_string(),
            true,
            json!({}),
        )
    })?;
    Ok(TempIndex {
        path: dir.join(format!("stage-{}.index", Uuid::new_v4().simple())),
    })
}

fn changed_paths(
    ctx: &ToolContext,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, WorkspaceError> {
    let mut paths = git_paths(
        ctx,
        &["diff", "HEAD", "--name-only", "-z"],
        None,
        cancellation,
    )?;
    paths.extend(git_paths(
        ctx,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        None,
        cancellation,
    )?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn git_paths(
    ctx: &ToolContext,
    args: &[&str],
    index: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, WorkspaceError> {
    let output = run_git(ctx, args, index, Duration::from_secs(30), cancellation)?;
    if output.exit_code != Some(0) {
        return Err(stage_error(
            "STAGE_COMMIT_GIT_FAILED",
            "Git failed while inspecting changed paths.",
            true,
            process_details(&output),
        ));
    }
    Ok(output
        .stdout
        .as_bytes()
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok())
        .map(|path| path.replace('\\', "/"))
        .collect())
}

fn git_text(
    ctx: &ToolContext,
    args: &[&str],
    index: Option<&Path>,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<String, WorkspaceError> {
    let output = run_git(ctx, args, index, timeout, cancellation)?;
    if output.exit_code != Some(0) {
        return Err(stage_error(
            "STAGE_COMMIT_GIT_FAILED",
            "Git command failed.",
            true,
            process_details(&output),
        ));
    }
    Ok(output.stdout.trim().to_string())
}

fn git_expect_success(
    ctx: &ToolContext,
    args: &[&str],
    index: Option<&Path>,
    timeout: Duration,
    cancellation: &CancellationToken,
    code: &'static str,
) -> Result<(), WorkspaceError> {
    let output = run_git(ctx, args, index, timeout, cancellation)?;
    if output.exit_code == Some(0) {
        return Ok(());
    }
    Err(stage_error(
        code,
        "Git command failed during stage_commit.",
        false,
        process_details(&output),
    ))
}

fn git_expect_success_owned(
    ctx: &ToolContext,
    args: &[String],
    index: Option<&Path>,
    timeout: Duration,
    cancellation: &CancellationToken,
    code: &'static str,
) -> Result<(), WorkspaceError> {
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    git_expect_success(ctx, &borrowed, index, timeout, cancellation, code)
}

fn run_git(
    ctx: &ToolContext,
    args: &[&str],
    index: Option<&Path>,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, WorkspaceError> {
    let mut command = Command::new("git");
    crate::platform::hide_std_console(&mut command);
    command
        .arg("-C")
        .arg(ctx.workspace.root())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    run_process(command, timeout, cancellation)
}

fn run_process(
    mut command: Command,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, WorkspaceError> {
    let mut child = command.spawn().map_err(|error| {
        stage_error(
            "STAGE_COMMIT_PROCESS_FAILED",
            error.to_string(),
            true,
            json!({}),
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        stage_error(
            "STAGE_COMMIT_PROCESS_FAILED",
            "Process stdout is unavailable.",
            true,
            json!({}),
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        stage_error(
            "STAGE_COMMIT_PROCESS_FAILED",
            "Process stderr is unavailable.",
            true,
            json!({}),
        )
    })?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let started = Instant::now();
    let exit_status = loop {
        if cancellation.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(stage_error(
                "REQUEST_CANCELLED",
                "stage_commit process was cancelled.",
                true,
                json!({}),
            ));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(stage_error(
                "STAGE_COMMIT_PROCESS_TIMEOUT",
                "stage_commit process timed out.",
                true,
                json!({"timeout_ms": timeout.as_millis()}),
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(stage_error(
                    "STAGE_COMMIT_PROCESS_FAILED",
                    error.to_string(),
                    true,
                    json!({}),
                ));
            }
        }
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok(ProcessOutput {
        exit_code: exit_status.code(),
        stdout,
        stderr,
    })
}

fn read_bounded(mut reader: impl Read) -> String {
    let mut retained = Vec::new();
    let mut buffer = [0u8; 8192];
    while let Ok(read) = reader.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let remaining = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(retained.len());
        if remaining > 0 {
            retained.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    String::from_utf8_lossy(&retained).into_owned()
}

fn parse_paths(args: &Value) -> Result<Vec<String>, WorkspaceError> {
    let values = args
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_argument("paths must be a non-empty array"))?;
    if values.is_empty() || values.len() > MAX_STAGE_PATHS {
        return Err(invalid_argument(format!(
            "paths must contain between 1 and {MAX_STAGE_PATHS} entries"
        )));
    }
    let mut paths = BTreeSet::new();
    for value in values {
        let raw = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_argument("paths contains an invalid entry"))?;
        let path = Path::new(raw);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
            || path
                .components()
                .any(|component| component.as_os_str() == ".git")
        {
            return Err(stage_error(
                "STAGE_COMMIT_PATH_REJECTED",
                "stage_commit paths must be relative workspace paths and cannot target .git.",
                false,
                json!({"path": raw}),
            ));
        }
        let normalized = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => value.to_str(),
                Component::CurDir => None,
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");
        if normalized.is_empty() {
            return Err(invalid_argument("paths cannot contain the workspace root"));
        }
        paths.insert(normalized);
    }
    Ok(paths.into_iter().collect())
}

fn parse_checks(args: &Value) -> Result<Vec<String>, WorkspaceError> {
    let Some(values) = args.get("required_checks") else {
        return Ok(Vec::new());
    };
    let values = values
        .as_array()
        .ok_or_else(|| invalid_argument("required_checks must be an array"))?;
    if values.len() > MAX_REQUIRED_CHECKS {
        return Err(invalid_argument(format!(
            "required_checks supports at most {MAX_REQUIRED_CHECKS} commands"
        )));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| invalid_argument("required_checks contains an invalid command"))
        })
        .collect()
}

fn ensure_changes_are_selected(
    changed: &[String],
    selected: &[String],
) -> Result<(), WorkspaceError> {
    let outside = changed
        .iter()
        .filter(|path| {
            !selected
                .iter()
                .any(|selected| *path == selected || path.starts_with(&format!("{selected}/")))
        })
        .cloned()
        .collect::<Vec<_>>();
    if outside.is_empty() {
        return Ok(());
    }
    Err(stage_error(
        "STAGE_COMMIT_SCOPE_MISMATCH",
        "Workspace changes exist outside the selected stage_commit paths.",
        false,
        json!({"outside_paths": outside, "selected_paths": selected}),
    ))
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, WorkspaceError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_argument(format!("{key} is required")))
}

fn save_receipt(ctx: &ToolContext, receipt: &StageCommitReceipt) -> Result<(), WorkspaceError> {
    ctx.harness
        .save_stage_commit_receipt(receipt)
        .map_err(harness_error)
}

fn finish_operation(ctx: &ToolContext, receipt: &StageCommitReceipt, complete: bool) {
    let _ = ctx.harness.record_operation(
        Some(&receipt.workflow_id),
        Some(&receipt.task_id),
        None,
        "stage_commit",
        if complete { "completed" } else { "incomplete" },
        json!({"idempotency_key": receipt.idempotency_key}),
        json!({
            "ok": complete,
            "status": receipt.status,
            "commit_sha": receipt.commit_sha,
            "checkpoint_hash": receipt.checkpoint_hash,
            "error": receipt.error
        }),
    );
}

fn receipt_response(
    receipt: &StageCommitReceipt,
    complete: bool,
    retryable: bool,
) -> Result<Value, WorkspaceError> {
    let state = match &receipt.status {
        StageCommitStatus::Started
        | StageCommitStatus::CheckRunning
        | StageCommitStatus::ChecksPassed => "running",
        StageCommitStatus::Committed | StageCommitStatus::CommittedCheckpointPending => {
            "checkpoint_pending"
        }
        StageCommitStatus::Completed => "completed",
        StageCommitStatus::CommittedRecoveryRequired | StageCommitStatus::Failed => "failed",
    };
    let current_check = receipt
        .required_checks
        .get(receipt.current_check_index)
        .cloned();
    let mut response = json!({
        "workflow_id": receipt.workflow_id,
        "idempotency_key": receipt.idempotency_key,
        "workflow_status": receipt.status,
        "state": state,
        "complete": complete,
        "retryable": retryable,
        "task_id": receipt.task_id,
        "paths": receipt.paths,
        "checks": receipt.checks,
        "required_check_count": receipt.required_checks.len(),
        "current_check_index": receipt.current_check_index,
        "current_check": current_check,
        "current_session_id": receipt.current_session_id,
        "verification_ids": receipt.verification_ids,
        "commit_sha": receipt.commit_sha,
        "committed_files": receipt.committed_files,
        "working_tree_files": receipt.working_tree_files,
        "runtime_artifacts": receipt.runtime_artifacts,
        "ignored_files": receipt.ignored_files,
        "baseline_refreshed": receipt.baseline_refreshed,
        "checkpoint_hash": receipt.checkpoint_hash,
        "checkpoint_count": receipt.checkpoint_count,
        "next_actions": match state {
            "running" => vec!["stage_commit_status", "wait_stage_commit"],
            "checkpoint_pending" => vec!["wait_stage_commit"],
            _ => Vec::<&str>::new(),
        }
    });
    if let (Some(object), Some(error)) = (response.as_object_mut(), receipt.error.clone()) {
        object.insert("error".into(), error);
    }
    Ok(response)
}

fn process_details(output: &ProcessOutput) -> Value {
    json!({
        "exit_code": output.exit_code,
        "stdout": output.stdout,
        "stderr": output.stderr
    })
}

fn harness_error(error: super::store::HarnessError) -> WorkspaceError {
    stage_error(error.code(), error.to_string(), false, json!({}))
}

fn invalid_argument(message: impl Into<String>) -> WorkspaceError {
    stage_error("INVALID_ARGUMENT", message.into(), false, json!({}))
}

fn stage_error(
    code: &'static str,
    message: impl Into<String>,
    retryable: bool,
    details: Value,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: message.into(),
        category: "runtime",
        retryable,
        details,
    }
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use tempfile::tempdir;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn fixture() -> Option<(tempfile::TempDir, tempfile::TempDir, ToolContext)> {
        which::which("python").ok()?;
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        git(workspace.path(), &["init"]);
        git(
            workspace.path(),
            &["config", "user.email", "anchor-tests@example.invalid"],
        );
        git(workspace.path(), &["config", "user.name", "Anchor Tests"]);
        fs::write(workspace.path().join(".gitignore"), "docs/session/\n").expect("gitignore");
        fs::write(workspace.path().join("main.txt"), "before\n").expect("file");
        git(workspace.path(), &["add", ".gitignore", "main.txt"]);
        git(workspace.path(), &["commit", "-m", "initial"]);
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        Some((workspace, harness, ctx))
    }

    fn prepare_change(ctx: &ToolContext, path: &Path) -> (String, String, String) {
        let task = ctx.harness.start_task("stage commit test").expect("task");
        fs::write(path.join("main.txt"), "after\n").expect("change");
        let refreshed = ctx
            .harness
            .refresh_expected_state_for_operation(&task.id, Some("test-change"))
            .expect("refresh");
        let expected = refreshed.expected_state;
        (
            task.id,
            expected.head.expect("head"),
            expected.worktree_fingerprint,
        )
    }

    #[test]
    fn failed_check_leaves_head_and_index_unchanged() {
        let Some((workspace, _harness, ctx)) = fixture() else {
            return;
        };
        let initial_head = git(workspace.path(), &["rev-parse", "HEAD"]);
        let (task_id, expected_head, expected_fingerprint) = prepare_change(&ctx, workspace.path());
        let command = "python -c \"import sys; sys.exit(3)\"";

        let error = run(
            &ctx,
            &json!({
                "task_id": task_id,
                "expected_head": expected_head,
                "expected_fingerprint": expected_fingerprint,
                "paths": ["main.txt"],
                "message": "must not commit",
                "required_checks": [command],
                "idempotency_key": "failed-check"
            }),
            &CancellationToken::default(),
        )
        .expect_err("check failure");

        assert_eq!(error.to_error_value()["code"], "STAGE_COMMIT_CHECK_FAILED");
        assert_eq!(git(workspace.path(), &["rev-parse", "HEAD"]), initial_head);
        let staged = Command::new("git")
            .arg("-C")
            .arg(workspace.path())
            .args(["diff", "--cached", "--quiet"])
            .status()
            .expect("git diff");
        assert!(staged.success(), "real Git index must remain unchanged");
    }

    #[test]
    fn check_that_mutates_real_index_is_rejected_before_commit() {
        let Some((workspace, _harness, ctx)) = fixture() else {
            return;
        };
        let initial_head = git(workspace.path(), &["rev-parse", "HEAD"]);
        let (task_id, expected_head, expected_fingerprint) = prepare_change(&ctx, workspace.path());

        let error = run(
            &ctx,
            &json!({
                "task_id": task_id,
                "expected_head": expected_head,
                "expected_fingerprint": expected_fingerprint,
                "paths": ["main.txt"],
                "message": "must not commit staged side effect",
                "required_checks": ["git add main.txt"],
                "idempotency_key": "index-mutating-check"
            }),
            &CancellationToken::default(),
        )
        .expect_err("dirty real index must fail");

        assert_eq!(
            error.to_error_value()["code"],
            "STAGE_COMMIT_INDEX_NOT_CLEAN"
        );
        assert_eq!(git(workspace.path(), &["rev-parse", "HEAD"]), initial_head);
    }

    #[test]
    fn successful_stage_commit_is_idempotent_and_preserves_initial_baseline() {
        let Some((workspace, _harness, ctx)) = fixture() else {
            return;
        };
        let (task_id, expected_head, expected_fingerprint) = prepare_change(&ctx, workspace.path());
        let initial_baseline_head = ctx.harness.task(&task_id).expect("task").baseline.head;
        let command = "python -c \"print('ok')\"";
        let arguments = json!({
            "task_id": task_id,
            "expected_head": expected_head,
            "expected_fingerprint": expected_fingerprint,
            "paths": ["main.txt"],
            "message": "commit selected stage",
            "required_checks": [command],
            "idempotency_key": "successful-stage"
        });

        let first = run(&ctx, &arguments, &CancellationToken::default()).expect("commit");
        assert_eq!(first["complete"], true);
        assert_eq!(first["committed_files"], json!(["main.txt"]));
        assert_eq!(first["working_tree_files"], json!([]));
        assert_eq!(first["runtime_artifacts"], json!([]));
        assert_eq!(first["baseline_refreshed"], true);
        assert_eq!(first["verification_ids"].as_array().unwrap().len(), 1);
        let commit_sha = first["commit_sha"]
            .as_str()
            .expect("commit sha")
            .to_string();
        assert_eq!(git(workspace.path(), &["rev-parse", "HEAD"]), commit_sha);
        assert!(git(workspace.path(), &["status", "--porcelain"]).is_empty());
        ctx.harness
            .check_baseline(&task_id)
            .expect("baseline valid");
        assert_eq!(
            ctx.harness.task(&task_id).expect("task").baseline.head,
            initial_baseline_head
        );
        let change = ctx
            .harness
            .load_change_set(&commit_sha)
            .expect("load change set")
            .expect("persisted change set");
        assert_eq!(change.committed_files, vec!["main.txt"]);
        assert_eq!(change.working_tree_files, Vec::<String>::new());
        assert_eq!(change.verification_ids.len(), 1);

        let second = run(&ctx, &arguments, &CancellationToken::default()).expect("idempotent");
        assert_eq!(second["commit_sha"], first["commit_sha"]);
        assert_eq!(git(workspace.path(), &["rev-list", "--count", "HEAD"]), "2");
    }

    #[test]
    fn deferred_stage_commit_is_waitable_and_does_not_replay_completed_checks() {
        let Some((workspace, _harness, ctx)) = fixture() else {
            return;
        };
        let (task_id, expected_head, expected_fingerprint) = prepare_change(&ctx, workspace.path());
        let arguments = json!({
            "task_id": task_id,
            "expected_head": expected_head,
            "expected_fingerprint": expected_fingerprint,
            "paths": ["main.txt"],
            "message": "deferred commit",
            "required_checks": ["python -c \"import time; time.sleep(0.2); print('ok')\""],
            "idempotency_key": "deferred-stage",
            "execution_mode": "deferred",
            "wait_timeout_ms": 0
        });

        let started = run(&ctx, &arguments, &CancellationToken::default()).expect("started");
        assert_eq!(started["state"], "running");
        assert_eq!(started["complete"], false);
        assert_eq!(started["current_check_index"], 0);
        assert!(started["current_session_id"].as_str().is_some());

        let observed = status(
            &ctx,
            &json!({"task_id": task_id, "idempotency_key": "deferred-stage"}),
        )
        .expect("status");
        assert_eq!(observed["state"], "running");

        let completed = wait(
            &ctx,
            &json!({
                "task_id": task_id,
                "idempotency_key": "deferred-stage",
                "wait_timeout_ms": 60_000
            }),
            &CancellationToken::default(),
        )
        .expect("completed");
        assert_eq!(completed["state"], "completed");
        assert_eq!(completed["complete"], true);
        assert_eq!(completed["checks"].as_array().unwrap().len(), 1);
        let commit_sha = completed["commit_sha"].clone();

        let repeated = wait(
            &ctx,
            &json!({
                "task_id": task_id,
                "idempotency_key": "deferred-stage",
                "wait_timeout_ms": 1
            }),
            &CancellationToken::default(),
        )
        .expect("idempotent wait");
        assert_eq!(repeated["commit_sha"], commit_sha);
        assert_eq!(repeated["checks"].as_array().unwrap().len(), 1);
        assert_eq!(git(workspace.path(), &["rev-list", "--count", "HEAD"]), "2");
    }

    #[test]
    fn checkpoint_failure_resumes_without_repeating_commit() {
        let Some((workspace, _harness, ctx)) = fixture() else {
            return;
        };
        let opened =
            dev_session::open(&ctx, &json!({"create_if_missing": true})).expect("open session");
        let session_id = opened["session_id"]
            .as_str()
            .expect("session id")
            .to_string();
        let session_path = opened["session_path"]
            .as_str()
            .expect("session path")
            .to_string();
        let (task_id, expected_head, expected_fingerprint) = prepare_change(&ctx, workspace.path());
        let base = json!({
            "task_id": task_id,
            "expected_head": expected_head,
            "expected_fingerprint": expected_fingerprint,
            "paths": ["main.txt"],
            "message": "commit before checkpoint retry",
            "required_checks": ["python -c \"print('ok')\""],
            "idempotency_key": "checkpoint-retry"
        });
        let mut first_args = base.clone();
        first_args["session_checkpoint"] = json!({
            "session_id": session_id,
            "expected_path": "docs/session/ses_not-the-current-session.md",
            "user_intent": "checkpoint retry test"
        });

        let first = run(&ctx, &first_args, &CancellationToken::default())
            .expect("commit with pending checkpoint");
        assert_eq!(first["complete"], false);
        assert_eq!(first["retryable"], true);
        assert_eq!(first["workflow_status"], "committed_checkpoint_pending");
        let commit_sha = first["commit_sha"].as_str().expect("commit").to_string();
        assert_eq!(git(workspace.path(), &["rev-list", "--count", "HEAD"]), "2");
        ctx.harness
            .check_baseline(&task_id)
            .expect("baseline valid");

        let mut retry_args = base;
        retry_args["session_checkpoint"] = json!({
            "session_id": opened["session_id"],
            "expected_path": session_path,
            "user_intent": "checkpoint retry test",
            "findings": ["commit already exists; checkpoint resumed"]
        });
        let retry =
            run(&ctx, &retry_args, &CancellationToken::default()).expect("checkpoint resumed");
        assert_eq!(retry["complete"], true);
        assert_eq!(retry["commit_sha"], commit_sha);
        assert!(retry["checkpoint_hash"].as_str().is_some());
        assert_eq!(git(workspace.path(), &["rev-list", "--count", "HEAD"]), "2");
    }
}
