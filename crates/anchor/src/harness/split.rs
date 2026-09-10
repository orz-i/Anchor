use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use super::model::{
    ChangeSet, HarnessEvent, HarnessSessionStatus, HarnessStatus, OperationRecord, ProjectBaseline,
    ProjectState, StageCommitReceipt, TaskContract, TaskGitWorktree, TaskPhase, TaskRecoveryState,
    TaskSession, TaskSlice, TaskSliceStatus, TaskStatus, VerificationRecord,
    WorkSessionCloseOutbox,
};
use super::state::HarnessKernel;
use super::store::HarnessResult;

/// Task-domain authority.
///
/// This facade owns task identity, lifecycle, plan/phase, Slice state and
/// Development Session association. Coding-specific safety and evidence APIs
/// are intentionally absent; callers must use [`CodingHarness`] for those.
#[derive(Clone)]
pub struct TaskHarness {
    core: Arc<HarnessKernel>,
}

/// Coding-domain authority.
///
/// This facade owns workspace/worktree state, baselines, verification,
/// recovery, change evidence, durable commit receipts and completion
/// diagnostics. It can inspect task state by id when coding policy needs it,
/// but it does not expose task-plan mutation APIs.
#[derive(Clone)]
pub struct CodingHarness {
    core: Arc<HarnessKernel>,
}

/// Construct the two Harness authorities over one durable storage namespace.
/// The shared core is a storage/transaction substrate, not a public domain API.
pub fn split_harness(
    workspace_root: PathBuf,
    harness_root: PathBuf,
) -> HarnessResult<(TaskHarness, CodingHarness)> {
    let core = Arc::new(HarnessKernel::new(workspace_root, harness_root)?);
    Ok((TaskHarness { core: core.clone() }, CodingHarness { core }))
}

impl TaskHarness {
    pub fn workspace_id(&self) -> &str {
        self.core.workspace_id()
    }

    pub fn start_task(&self, objective: &str) -> HarnessResult<TaskSession> {
        self.core.start_task(objective)
    }

    pub fn task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.core.task(task_id)
    }

    pub fn list_tasks(&self) -> HarnessResult<Vec<TaskSession>> {
        self.core.list_tasks()
    }

    pub fn current_task(&self) -> HarnessResult<Option<TaskSession>> {
        self.core.current_task()
    }

    pub fn active_tasks(&self) -> HarnessResult<Vec<TaskSession>> {
        self.core.active_tasks()
    }

    pub fn bind_session(
        &self,
        task_id: &str,
        session_id: &str,
        path: &str,
    ) -> HarnessResult<TaskSession> {
        self.core.bind_session(task_id, session_id, path)
    }

    pub fn reclaim_session(
        &self,
        task_id: &str,
        session_id: &str,
        path: &str,
    ) -> HarnessResult<TaskSession> {
        self.core.reclaim_session(task_id, session_id, path)
    }

    pub fn pause_stale_active_tasks(
        &self,
        protected_task_ids: &std::collections::HashSet<String>,
    ) -> HarnessResult<Vec<String>> {
        self.core.pause_stale_active_tasks(protected_task_ids)
    }

    pub fn switch_task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.core.switch_task(task_id)
    }

    pub fn resume_paused_task_for_activity(
        &self,
        tool: &str,
        mcp_session_id: Option<&str>,
    ) -> HarnessResult<Option<TaskSession>> {
        self.core
            .resume_paused_task_for_activity(tool, mcp_session_id)
    }

    pub fn resume_task_for_activity(
        &self,
        task_id: &str,
        tool: &str,
        mcp_session_id: Option<&str>,
    ) -> HarnessResult<TaskSession> {
        self.core
            .resume_task_for_activity(task_id, tool, mcp_session_id)
    }

    pub fn mark_verifying(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.core.mark_verifying(task_id)
    }

    pub fn complete_task(
        &self,
        task_id: &str,
        verified: bool,
        session_status: HarnessSessionStatus,
    ) -> HarnessResult<TaskSession> {
        self.core.complete_task(task_id, verified, session_status)
    }

    pub fn abort_task(
        &self,
        task_id: &str,
        reason: &str,
        session_status: HarnessSessionStatus,
    ) -> HarnessResult<TaskSession> {
        self.core.abort_task(task_id, reason, session_status)
    }

    pub fn transition(&self, task_id: &str, next: TaskStatus) -> HarnessResult<TaskSession> {
        self.core.transition(task_id, next)
    }

    pub fn update_steps(
        &self,
        task_id: &str,
        completed_steps: Option<Vec<String>>,
        pending_steps: Option<Vec<String>>,
    ) -> HarnessResult<TaskSession> {
        self.core
            .update_steps(task_id, completed_steps, pending_steps)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn revise_plan(
        &self,
        task_id: &str,
        objective: &str,
        phase: Option<TaskPhase>,
        contract: Option<TaskContract>,
        slices: Option<Vec<TaskSlice>>,
        working_set: Option<super::model::TaskWorkingSet>,
        completed_steps: Option<Vec<String>>,
        pending_steps: Option<Vec<String>>,
    ) -> HarnessResult<TaskSession> {
        self.core.revise_plan(
            task_id,
            objective,
            phase,
            contract,
            slices,
            working_set,
            completed_steps,
            pending_steps,
        )
    }

    pub fn configure_task(
        &self,
        task_id: &str,
        phase: Option<TaskPhase>,
        contract: Option<TaskContract>,
        slices: Option<Vec<TaskSlice>>,
        working_set: Option<super::model::TaskWorkingSet>,
    ) -> HarnessResult<TaskSession> {
        self.core
            .configure_task(task_id, phase, contract, slices, working_set)
    }

    pub fn start_slice(&self, task_id: &str, slice: TaskSlice) -> HarnessResult<TaskSession> {
        self.core.start_slice(task_id, slice)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_slice(
        &self,
        task_id: &str,
        slice_id: &str,
        status: Option<TaskSliceStatus>,
        title: Option<String>,
        files: Option<Vec<String>>,
        acceptance_checks: Option<Vec<super::model::VerificationRequirement>>,
        commit_sha: Option<Option<String>>,
        blocker: Option<Option<String>>,
    ) -> HarnessResult<TaskSession> {
        self.core.update_slice(
            task_id,
            slice_id,
            status,
            title,
            files,
            acceptance_checks,
            commit_sha,
            blocker,
        )
    }

    pub fn complete_slice(
        &self,
        task_id: &str,
        slice_id: &str,
        commit_sha: Option<String>,
    ) -> HarnessResult<TaskSession> {
        self.core.complete_slice(task_id, slice_id, commit_sha)
    }
}

impl CodingHarness {
    pub fn default_root() -> HarnessResult<PathBuf> {
        HarnessKernel::default_root()
    }

    pub fn workspace_root(&self) -> &Path {
        self.core.workspace_root()
    }

    pub fn workspace_id(&self) -> &str {
        self.core.workspace_id()
    }

    pub fn store_root(&self) -> &Path {
        self.core.store_root()
    }

    pub fn scoped_to_workspace_root(&self, root: PathBuf) -> HarnessResult<Self> {
        Ok(Self {
            core: Arc::new(self.core.with_workspace_root(root)?),
        })
    }

    pub fn task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.core.task(task_id)
    }

    /// Create a task whose coding execution root is an Anchor-managed Git
    /// worktree. Task identity remains shared with Task Harness, while
    /// worktree ownership and baseline initialization stay here.
    pub fn start_task_in_git_worktree(
        &self,
        objective: &str,
        task_id: String,
        worktree: TaskGitWorktree,
    ) -> HarnessResult<TaskSession> {
        self.core
            .start_task_in_git_worktree(objective, task_id, worktree)
    }

    pub fn attach_git_worktree(
        &self,
        task_id: &str,
        worktree: TaskGitWorktree,
    ) -> HarnessResult<TaskSession> {
        self.core.attach_git_worktree(task_id, worktree)
    }

    pub fn accept_current_baseline(
        &self,
        task_id: &str,
        observation_token: &str,
        reason: &str,
    ) -> HarnessResult<TaskSession> {
        self.core
            .accept_current_baseline(task_id, observation_token, reason)
    }

    pub fn accept_latest_baseline(
        &self,
        task_id: &str,
        reason: &str,
        max_attempts: u8,
    ) -> HarnessResult<(TaskSession, u8, ProjectBaseline)> {
        self.core
            .accept_latest_baseline(task_id, reason, max_attempts)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_verification(
        &self,
        task_id: &str,
        kind: &str,
        command: &str,
        verification_key: Option<&str>,
        test_file: Option<&str>,
        test_name: Option<&str>,
        exit_code: Option<i32>,
        passed: bool,
        duration_ms: Option<u64>,
        terminal_at: Option<&str>,
        output_refs: Option<Value>,
        change_id: Option<&str>,
        level: &str,
        supersede_previous_failures: bool,
    ) -> HarnessResult<VerificationRecord> {
        self.core.record_verification(
            task_id,
            kind,
            command,
            verification_key,
            test_file,
            test_name,
            exit_code,
            passed,
            duration_ms,
            terminal_at,
            output_refs,
            change_id,
            level,
            supersede_previous_failures,
        )
    }

    pub fn list_verifications(&self, task_id: &str) -> HarnessResult<Vec<VerificationRecord>> {
        self.core.list_verifications(task_id)
    }

    pub fn update_verification_disposition(
        &self,
        task_id: &str,
        verification_id: &str,
        disposition: &str,
        reason: &str,
        source: &str,
    ) -> HarnessResult<VerificationRecord> {
        self.core.update_verification_disposition(
            task_id,
            verification_id,
            disposition,
            reason,
            source,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_change_set(
        &self,
        task_id: &str,
        change_id: &str,
        committed_files: Vec<String>,
        working_tree_files: Vec<String>,
        runtime_artifacts: Vec<String>,
        ignored_files: Vec<String>,
        verification_ids: Vec<String>,
    ) -> HarnessResult<ChangeSet> {
        self.core.save_change_set(
            task_id,
            change_id,
            committed_files,
            working_tree_files,
            runtime_artifacts,
            ignored_files,
            verification_ids,
        )
    }

    pub fn load_change_set(&self, change_id: &str) -> HarnessResult<Option<ChangeSet>> {
        self.core.load_change_set(change_id)
    }

    pub fn list_change_sets(&self, task_id: &str) -> HarnessResult<Vec<ChangeSet>> {
        self.core.list_change_sets(task_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_recovery(
        &self,
        task_id: &str,
        failed_step: &str,
        step_fingerprint: Option<&str>,
        failure_type: &str,
        error_code: Option<&str>,
        related_verification_id: Option<&str>,
        explicit_retry_identity: bool,
        workspace_mutated: bool,
        rollback_status: &str,
        recommended_recovery: Vec<String>,
        resume_target: &str,
    ) -> HarnessResult<TaskRecoveryState> {
        self.core.record_recovery(
            task_id,
            failed_step,
            step_fingerprint,
            failure_type,
            error_code,
            related_verification_id,
            explicit_retry_identity,
            workspace_mutated,
            rollback_status,
            recommended_recovery,
            resume_target,
        )
    }

    pub fn resolve_recovery_for_attempt(
        &self,
        task_id: &str,
        failed_step: &str,
        step_fingerprint: Option<&str>,
        recovery_key: Option<&str>,
    ) -> HarnessResult<Option<TaskRecoveryState>> {
        self.core
            .resolve_recovery_for_attempt(task_id, failed_step, step_fingerprint, recovery_key)
    }

    pub fn resolve_recovery_for_step(
        &self,
        task_id: &str,
        failed_step: &str,
        step_fingerprint: Option<&str>,
    ) -> HarnessResult<Option<TaskRecoveryState>> {
        self.core
            .resolve_recovery_for_step(task_id, failed_step, step_fingerprint)
    }

    pub fn resolve_recovery(
        &self,
        task_id: &str,
        recovery_id: &str,
        reason: &str,
        evidence: &[String],
    ) -> HarnessResult<TaskRecoveryState> {
        self.core
            .resolve_recovery(task_id, recovery_id, reason, evidence)
    }

    pub fn check_baseline(&self, task_id: &str) -> HarnessResult<()> {
        self.core.check_baseline(task_id)
    }

    pub fn refresh_expected_state(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.core.refresh_expected_state(task_id)
    }

    pub fn refresh_expected_state_for_operation(
        &self,
        task_id: &str,
        operation_id: Option<&str>,
    ) -> HarnessResult<TaskSession> {
        self.core
            .refresh_expected_state_for_operation(task_id, operation_id)
    }

    pub fn refresh_baseline(
        &self,
        task_id: &str,
        observed_head: Option<&str>,
        observed_fingerprint: &str,
        reason: &str,
    ) -> HarnessResult<TaskSession> {
        self.core
            .refresh_baseline(task_id, observed_head, observed_fingerprint, reason)
    }

    pub fn record_event(
        &self,
        task_id: &str,
        kind: &str,
        tool_name: Option<&str>,
        input_summary: Value,
        result_summary: Value,
    ) -> HarnessResult<HarnessEvent> {
        self.core
            .record_event(task_id, kind, tool_name, input_summary, result_summary)
    }

    pub fn list_events(
        &self,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        self.core.list_events(task_id, offset, limit)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_operation(
        &self,
        operation_id: Option<&str>,
        task_id: Option<&str>,
        mcp_session_id: Option<&str>,
        tool: &str,
        kind: &str,
        input_summary: Value,
        result_summary: Value,
    ) -> HarnessResult<OperationRecord> {
        self.core.record_operation(
            operation_id,
            task_id,
            mcp_session_id,
            tool,
            kind,
            input_summary,
            result_summary,
        )
    }

    pub fn list_operations(
        &self,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<OperationRecord>> {
        self.core.list_operations(offset, limit)
    }

    pub fn all_operations(&self, limit: usize) -> HarnessResult<Vec<OperationRecord>> {
        self.core.all_operations(limit)
    }

    pub fn load_stage_commit_receipt(
        &self,
        idempotency_key: &str,
    ) -> HarnessResult<Option<StageCommitReceipt>> {
        self.core.load_stage_commit_receipt(idempotency_key)
    }

    pub fn save_stage_commit_receipt(&self, receipt: &StageCommitReceipt) -> HarnessResult<()> {
        self.core.save_stage_commit_receipt(receipt)
    }

    pub fn save_close_outbox(&self, outbox: &WorkSessionCloseOutbox) -> HarnessResult<()> {
        self.core.save_close_outbox(outbox)
    }

    pub fn load_close_outbox(
        &self,
        task_id: &str,
    ) -> HarnessResult<Option<WorkSessionCloseOutbox>> {
        self.core.load_close_outbox(task_id)
    }

    pub fn list_close_outboxes(&self) -> HarnessResult<Vec<WorkSessionCloseOutbox>> {
        self.core.list_close_outboxes()
    }

    pub fn delete_close_outbox(&self, task_id: &str) -> HarnessResult<()> {
        self.core.delete_close_outbox(task_id)
    }

    pub fn set_latest_change(&self, task_id: &str, change_id: &str) -> HarnessResult<TaskSession> {
        self.core.set_latest_change(task_id, change_id)
    }

    pub fn project_state(&self, max_files: usize) -> HarnessResult<ProjectState> {
        self.core.project_state(max_files)
    }

    pub fn project_state_for_task(
        &self,
        max_files: usize,
        selected_task_id: Option<&str>,
    ) -> HarnessResult<ProjectState> {
        self.core
            .project_state_for_task(max_files, selected_task_id)
    }

    pub fn status(&self) -> HarnessResult<HarnessStatus> {
        self.core.status()
    }

    pub fn status_for_task(&self, selected_task_id: Option<&str>) -> HarnessResult<HarnessStatus> {
        self.core.status_for_task(selected_task_id)
    }
}
