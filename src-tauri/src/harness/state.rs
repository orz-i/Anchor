use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use super::model::{
    BaselineEntry, BaselineObject, CapabilityStatus, ChangeSet, ExpectedWorkspaceState,
    FileChangeRecord, HarnessEvent, HarnessSessionStatus, HarnessStatus, OperationRecord,
    ProjectBaseline, ProjectFileState, ProjectState, ReasonRecord, StageCommitReceipt, TaskSession,
    TaskStatus, VerificationDispositionRecord, VerificationRecord, WorkSessionCloseOutbox,
    WorkspaceHarnessState, SCHEMA_VERSION,
};
use super::store::{baseline_object_id, HarnessError, HarnessResult, HarnessStore};

#[derive(Debug, Clone)]
pub struct Harness {
    workspace_root: PathBuf,
    workspace_id: String,
    store: HarnessStore,
}

fn tool_activity_resumes_paused_task(tool: &str) -> bool {
    !matches!(
        tool,
        "server_info"
            | "harness_status"
            | "operation_log"
            | "project_state"
            | "task_context"
            | "list_task_events"
            | "change_summary"
            | "history_session_bootstrap"
            | "history_session_checkpoint"
            | "history_session_validate"
            | "check_exec_environment"
            | "exec_health_check"
            | "command_cost_explain"
            | "get_default_cwd"
            | "list_command_sessions"
            | "pause_task"
            | "resume_task"
            | "switch_task"
            | "finish_task"
            | "close_work_session"
            | "start_task"
            | "stage_commit_status"
    )
}

fn verification_identity_matches(
    previous: &VerificationRecord,
    kind: &str,
    command: &str,
    verification_key: Option<&str>,
    test_file: Option<&str>,
    test_name: Option<&str>,
) -> bool {
    if previous.kind != kind {
        return false;
    }
    if let Some(key) = verification_key {
        return previous.verification_key.as_deref() == Some(key);
    }
    if test_file.is_some() || test_name.is_some() {
        return test_file.is_none_or(|value| previous.test_file.as_deref() == Some(value))
            && test_name.is_none_or(|value| previous.test_name.as_deref() == Some(value));
    }
    previous.command == command || previous.verification_key.is_none()
}

fn normalize_verification_level(level: &str) -> HarnessResult<&str> {
    let normalized = if level.trim().is_empty() {
        "blocking"
    } else {
        level.trim()
    };
    if matches!(
        normalized,
        "diagnostic" | "informational" | "required" | "blocking"
    ) {
        Ok(normalized)
    } else {
        Err(HarnessError::new(
            "INVALID_ARGUMENT",
            "verification level must be diagnostic, informational, required, or blocking",
        ))
    }
}

fn verification_effective_disposition(record: &VerificationRecord) -> &str {
    record
        .dispositions
        .last()
        .map(|entry| entry.disposition.as_str())
        .unwrap_or(if record.passed {
            "passed"
        } else {
            "active_failure"
        })
}

fn baseline_observation_token(
    workspace_id: &str,
    task_id: &str,
    baseline: &ProjectBaseline,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"anchor-baseline-observation-v1\0");
    hasher.update(workspace_id.as_bytes());
    hasher.update([0]);
    hasher.update(task_id.as_bytes());
    hasher.update([0]);
    hasher.update(baseline.branch.as_deref().unwrap_or_default().as_bytes());
    hasher.update([0]);
    hasher.update(baseline.head.as_deref().unwrap_or_default().as_bytes());
    hasher.update([0]);
    hasher.update(baseline.worktree_fingerprint.as_bytes());
    format!("anchor-observation-v1:{:x}", hasher.finalize())
}

impl Harness {
    pub fn new(workspace_root: PathBuf, harness_root: PathBuf) -> HarnessResult<Self> {
        let workspace_root = workspace_root
            .canonicalize()
            .map_err(|e| HarnessError::new("WORKSPACE_UNAVAILABLE", e.to_string()))?;
        let store = HarnessStore::new(harness_root)?;
        let workspace_id = store
            .resolve_workspace_identity(&workspace_root)?
            .workspace_id;
        Ok(Self {
            workspace_root,
            workspace_id,
            store,
        })
    }

    pub fn all_operations(&self, limit: usize) -> HarnessResult<Vec<OperationRecord>> {
        self.store
            .list_operations(&self.workspace_id, 0, limit.clamp(1, 20_000))
    }

    pub fn bind_history_session(
        &self,
        task_id: &str,
        session_key: &str,
        path: &str,
    ) -> HarnessResult<TaskSession> {
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(task_id)?;
                if let Some(existing) = task.history_session_key.as_deref() {
                    if existing != session_key || task.history_session_path.as_deref() != Some(path)
                    {
                        return Err(HarnessError::new(
                            "WORK_SESSION_CONFLICT",
                            "当前任务已绑定到另一个 History Session",
                        ));
                    }
                    return Ok(task);
                }
                task.history_session_key = Some(session_key.to_string());
                task.history_session_path = Some(path.to_string());
                task.updated_at = timestamp();
                transaction.save_task(&task)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    task_id,
                    "history_session_bound",
                    Some("begin_work_session"),
                    json!({"session_key": session_key, "path": path}),
                    json!({"ok": true}),
                ))?;
                Ok(task)
            })
    }

    pub fn switch_task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut target = self.task(task_id)?;
                if !matches!(target.status, TaskStatus::Active | TaskStatus::Paused) {
                    return Err(HarnessError::new(
                        "TASK_SWITCH_BLOCKED",
                        format!("任务 {} 当前状态 {:?} 不允许切换", target.id, target.status),
                    ));
                }
                let mut paused_task_ids = Vec::new();
                for mut peer in self.store.list_tasks(&self.workspace_id)? {
                    if peer.id == target.id
                        || !matches!(peer.status, TaskStatus::Active | TaskStatus::Verifying)
                    {
                        continue;
                    }
                    peer.status = TaskStatus::Paused;
                    peer.updated_at = timestamp();
                    transaction.save_task(&peer)?;
                    transaction.append_event(&harness_event(
                        &self.workspace_id,
                        &peer.id,
                        "task_paused_for_writer_handoff",
                        Some("switch_task"),
                        json!({"next_writer_task_id": target.id.clone()}),
                        json!({"ok": true, "status": "paused"}),
                    ))?;
                    paused_task_ids.push(peer.id);
                }
                if target.status == TaskStatus::Paused {
                    target.status = TaskStatus::Active;
                    target.updated_at = timestamp();
                    transaction.save_task(&target)?;
                }
                transaction.save_workspace_state(&self.workspace_state(
                    Some(&target.id),
                    HarnessSessionStatus::Active,
                    &target.updated_at,
                )?)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    &target.id,
                    "task_selected",
                    Some("switch_task"),
                    json!({"single_writer": true, "paused_task_ids": paused_task_ids}),
                    json!({"ok": true, "status": "active", "single_writer": true}),
                ))?;
                Ok(target)
            })
    }

    pub fn resume_paused_task_for_activity(
        &self,
        tool: &str,
        mcp_session_id: Option<&str>,
    ) -> HarnessResult<Option<TaskSession>> {
        if !tool_activity_resumes_paused_task(tool) {
            return self.current_task();
        }
        let Some(current) = self.current_task()? else {
            return Ok(None);
        };
        self.resume_task_for_activity(&current.id, tool, mcp_session_id)
            .map(Some)
    }

    pub fn resume_task_for_activity(
        &self,
        task_id: &str,
        tool: &str,
        mcp_session_id: Option<&str>,
    ) -> HarnessResult<TaskSession> {
        let current = self.task(task_id)?;
        if !tool_activity_resumes_paused_task(tool) {
            return Ok(current);
        }
        if current.status != TaskStatus::Paused {
            return Ok(current);
        }
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(&current.id)?;
                if task.status != TaskStatus::Paused {
                    return Ok(task);
                }
                let mut paused_task_ids = Vec::new();
                for mut peer in self.store.list_tasks(&self.workspace_id)? {
                    if peer.id == task.id
                        || !matches!(peer.status, TaskStatus::Active | TaskStatus::Verifying)
                    {
                        continue;
                    }
                    peer.status = TaskStatus::Paused;
                    peer.updated_at = timestamp();
                    transaction.save_task(&peer)?;
                    transaction.append_event(&harness_event(
                        &self.workspace_id,
                        &peer.id,
                        "task_paused_for_writer_handoff",
                        Some(tool),
                        json!({"next_writer_task_id": task.id.clone()}),
                        json!({"ok": true, "status": "paused"}),
                    ))?;
                    paused_task_ids.push(peer.id);
                }
                task.status = TaskStatus::Active;
                task.updated_at = timestamp();
                transaction.save_task(&task)?;
                transaction.save_workspace_state(&self.workspace_state(
                    Some(task.id.as_str()),
                    HarnessSessionStatus::Active,
                    &task.updated_at,
                )?)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    &task.id,
                    "task_auto_resumed",
                    Some(tool),
                    json!({
                        "previous_status": "paused",
                        "trigger": "tool_activity",
                        "tool": tool,
                        "mcp_session_id": mcp_session_id,
                        "single_writer": true,
                        "paused_task_ids": paused_task_ids,
                    }),
                    json!({
                        "ok": true,
                        "status": "active",
                        "auto_resumed": true,
                    }),
                ))?;
                Ok(task)
            })
    }

    pub fn accept_current_baseline(
        &self,
        task_id: &str,
        observation_token: &str,
        reason: &str,
    ) -> HarnessResult<TaskSession> {
        let current = capture_baseline(&self.workspace_root);
        let expected_token = baseline_observation_token(&self.workspace_id, task_id, &current);
        if observation_token != expected_token {
            return Err(HarnessError::new(
                "BASELINE_OBSERVATION_STALE",
                "工作区状态已变化或 observation_token 不属于当前任务；请重新读取 harness_status",
            ));
        }
        self.refresh_baseline(
            task_id,
            current.head.as_deref(),
            &current.worktree_fingerprint,
            reason,
        )
    }

    pub fn accept_latest_baseline(
        &self,
        task_id: &str,
        reason: &str,
        max_attempts: u8,
    ) -> HarnessResult<(TaskSession, u8, ProjectBaseline)> {
        if reason.trim().is_empty() {
            return Err(HarnessError::new(
                "INVALID_ARGUMENT",
                "accept_latest_baseline 必须提供接受当前状态的原因",
            ));
        }
        let max_attempts = max_attempts.clamp(1, 10);
        for attempt in 1..=max_attempts {
            let observed = capture_baseline(&self.workspace_root);
            let confirmed = capture_baseline(&self.workspace_root);
            if observed.branch != confirmed.branch
                || observed.head != confirmed.head
                || observed.worktree_fingerprint != confirmed.worktree_fingerprint
            {
                continue;
            }
            let operation_id = Uuid::new_v4().simple().to_string();
            let task =
                self.store
                    .with_workspace_transaction(&self.workspace_id, |transaction| {
                        let mut task = self.task(task_id)?;
                        if !task.status.is_writable() {
                            return Err(HarnessError::new(
                                "TASK_NOT_WRITABLE",
                                "当前任务状态不允许接受最新基线",
                            ));
                        }
                        task.expected_state =
                            expected_state_from_baseline(&confirmed, Some(&operation_id));
                        task.updated_at = timestamp();
                        transaction.save_task(&task)?;
                        transaction.append_event(&harness_event(
                            &self.workspace_id,
                            task_id,
                            "baseline_latest_accepted",
                            Some("accept_latest_baseline"),
                            json!({
                                "reason": reason,
                                "attempt": attempt,
                                "max_attempts": max_attempts
                            }),
                            json!({
                                "ok": true,
                                "branch": confirmed.branch,
                                "head": confirmed.head,
                                "worktree_fingerprint": confirmed.worktree_fingerprint,
                                "operation_id": operation_id
                            }),
                        ))?;
                        Ok(task)
                    })?;
            return Ok((task, attempt, confirmed));
        }
        Err(HarnessError::new(
            "BASELINE_UNSTABLE",
            format!("工作区在连续 {max_attempts} 次稳定性检查中持续变化；请停止并发写入后重试"),
        ))
    }

    pub fn default_root() -> HarnessResult<PathBuf> {
        let root = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| HarnessError::new("STORE_UNAVAILABLE", "无法确定应用数据目录"))?;
        Ok(root.join("anchor").join("harness-v5"))
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn store_root(&self) -> &Path {
        self.store.root()
    }

    pub fn start_task(&self, objective: &str) -> HarnessResult<TaskSession> {
        self.start_task_with_handoff(objective, false)
    }

    pub fn start_task_with_handoff(
        &self,
        objective: &str,
        _pause_current: bool,
    ) -> HarnessResult<TaskSession> {
        if objective.trim().is_empty() {
            return Err(HarnessError::new("INVALID_ARGUMENT", "任务目标不能为空"));
        }
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut paused_task_ids = Vec::new();
                for mut task in self.store.list_tasks(&self.workspace_id)? {
                    if !matches!(task.status, TaskStatus::Active | TaskStatus::Verifying) {
                        continue;
                    }
                    task.status = TaskStatus::Paused;
                    task.updated_at = timestamp();
                    transaction.save_task(&task)?;
                    transaction.append_event(&harness_event(
                        &self.workspace_id,
                        &task.id,
                        "task_paused_for_writer_handoff",
                        Some("start_task"),
                        json!({"next_objective": objective.trim()}),
                        json!({"ok": true, "status": "paused"}),
                    ))?;
                    paused_task_ids.push(task.id);
                }
                let captured = capture_baseline_snapshot(&self.workspace_root);
                let baseline = captured.baseline;
                let now = timestamp();
                let task = TaskSession {
                    schema_version: SCHEMA_VERSION,
                    id: Uuid::new_v4().simple().to_string(),
                    workspace_id: self.workspace_id.clone(),
                    objective: objective.trim().to_string(),
                    status: TaskStatus::Active,
                    expected_state: expected_state_from_baseline(&baseline, None),
                    baseline,
                    completed_steps: Vec::new(),
                    pending_steps: Vec::new(),
                    latest_change_id: None,
                    latest_verification_id: None,
                    history_session_key: None,
                    history_session_path: None,
                    created_at: now.clone(),
                    updated_at: now,
                };
                transaction.save_baseline_object(&captured.object)?;
                transaction.save_task(&task)?;
                transaction.save_workspace_state(&self.workspace_state(
                    Some(&task.id),
                    HarnessSessionStatus::Active,
                    &task.updated_at,
                )?)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    &task.id,
                    "task_started",
                    None,
                    json!({"single_writer": true, "paused_task_ids": paused_task_ids}),
                    json!({"ok": true, "single_writer": true}),
                ))?;
                Ok(task)
            })
    }

    pub fn mark_verifying(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(task_id)?;
                if !task.status.is_writable() {
                    return Err(HarnessError::new(
                        "TASK_NOT_WRITABLE",
                        "当前任务已经关闭，不能进入验证状态",
                    ));
                }
                if task.status != TaskStatus::Verifying {
                    task.status = TaskStatus::Verifying;
                    task.updated_at = timestamp();
                    transaction.save_task(&task)?;
                    transaction.save_workspace_state(&self.workspace_state(
                        Some(&task.id),
                        HarnessSessionStatus::Active,
                        &task.updated_at,
                    )?)?;
                    transaction.append_event(&harness_event(
                        &self.workspace_id,
                        task_id,
                        "task_verification_required",
                        Some("finish_task"),
                        json!({}),
                        json!({"ok": false, "task_status": "verifying"}),
                    ))?;
                }
                Ok(task)
            })
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
        change_id: Option<&str>,
        level: &str,
        supersede_previous_failures: bool,
    ) -> HarnessResult<VerificationRecord> {
        let level = normalize_verification_level(level)?;
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(task_id)?;
                let verification_id = Uuid::new_v4().simple().to_string();
                let mut supersedes = Vec::new();
                if passed && supersede_previous_failures {
                    for mut previous in self.list_verifications(task_id)? {
                        if !verification_identity_matches(
                            &previous,
                            kind,
                            command,
                            verification_key,
                            test_file,
                            test_name,
                        )
                            || previous.passed
                            || verification_effective_disposition(&previous) != "active_failure"
                        {
                            continue;
                        }
                        previous.dispositions.push(VerificationDispositionRecord {
                            id: Uuid::new_v4().simple().to_string(),
                            disposition: "superseded".into(),
                            reason: format!(
                                "Superseded by later successful verification {verification_id} for kind {kind}"
                            ),
                            source: "automatic_later_success".into(),
                            created_at: timestamp(),
                        });
                        transaction.save_verification(&previous)?;
                        supersedes.push(previous.id);
                    }
                }
                let mut verification = VerificationRecord {
                    id: verification_id,
                    task_id: task_id.to_string(),
                    command: command.to_string(),
                    kind: kind.trim().to_string(),
                    verification_key: verification_key.map(str::to_string),
                    test_file: test_file.map(str::to_string),
                    test_name: test_name.map(str::to_string),
                    status: if passed { "passed" } else { "failed" }.into(),
                    level: level.to_string(),
                    exit_code,
                    passed,
                    duration_ms,
                    change_id: change_id.map(str::to_string),
                    dispositions: Vec::new(),
                    supersedes,
                    created_at: timestamp(),
                };
                if !passed {
                    let initial_disposition = match level {
                        "diagnostic" => Some((
                            "diagnostic_only",
                            "Diagnostic verification failures are recorded but do not block task completion",
                        )),
                        "informational" => Some((
                            "expected_failure",
                            "Informational verification failures are retained as non-blocking evidence",
                        )),
                        _ => None,
                    };
                    if let Some((disposition, reason)) = initial_disposition {
                        verification
                            .dispositions
                            .push(VerificationDispositionRecord {
                                id: Uuid::new_v4().simple().to_string(),
                                disposition: disposition.into(),
                                reason: reason.into(),
                                source: "verification_level".into(),
                                created_at: timestamp(),
                            });
                    }
                }
                transaction.save_verification(&verification)?;
                task.latest_verification_id = Some(verification.id.clone());
                task.updated_at = timestamp();
                transaction.save_task(&task)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    task_id,
                    "verification_recorded",
                    Some("exec_command"),
                    json!({
                        "verification_id": verification.id,
                        "kind": verification.kind,
                        "command": verification.command,
                        "level": verification.level,
                        "supersedes": verification.supersedes
                    }),
                    json!({
                        "ok": passed,
                        "status": verification.status,
                        "exit_code": verification.exit_code,
                        "duration_ms": verification.duration_ms
                    }),
                ))?;
                Ok(verification)
            })
    }

    pub fn list_verifications(&self, task_id: &str) -> HarnessResult<Vec<VerificationRecord>> {
        self.store.list_verifications(&self.workspace_id, task_id)
    }

    pub fn update_verification_disposition(
        &self,
        task_id: &str,
        verification_id: &str,
        disposition: &str,
        reason: &str,
        source: &str,
    ) -> HarnessResult<VerificationRecord> {
        if reason.trim().is_empty() {
            return Err(HarnessError::new(
                "INVALID_ARGUMENT",
                "verification disposition 必须提供理由",
            ));
        }
        let allowed = [
            "active_failure",
            "expected_failure",
            "diagnostic_only",
            "superseded",
            "waived",
            "passed",
        ];
        if !allowed.contains(&disposition) {
            return Err(HarnessError::new(
                "INVALID_ARGUMENT",
                "不支持的 verification disposition",
            ));
        }
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut verification = self
                    .list_verifications(task_id)?
                    .into_iter()
                    .find(|record| record.id == verification_id)
                    .ok_or_else(|| {
                        HarnessError::new(
                            "VERIFICATION_NOT_FOUND",
                            format!("Verification not found: {verification_id}"),
                        )
                    })?;
                let entry = VerificationDispositionRecord {
                    id: Uuid::new_v4().simple().to_string(),
                    disposition: disposition.to_string(),
                    reason: reason.trim().to_string(),
                    source: source.trim().to_string(),
                    created_at: timestamp(),
                };
                verification.dispositions.push(entry.clone());
                transaction.save_verification(&verification)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    task_id,
                    "verification_disposition_updated",
                    Some("update_verification_disposition"),
                    json!({
                        "verification_id": verification_id,
                        "disposition": disposition,
                        "reason": reason,
                        "source": source
                    }),
                    json!({"ok": true, "disposition_id": entry.id}),
                ))?;
                Ok(verification)
            })
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
        let task = self.task(task_id)?;
        let files = committed_files
            .iter()
            .map(|path| FileChangeRecord {
                path: path.clone(),
                status: "committed".into(),
                before_sha256: None,
                after_sha256: None,
            })
            .collect();
        let change = ChangeSet {
            id: change_id.to_string(),
            task_id: task_id.to_string(),
            objective: task.objective.clone(),
            reason: ReasonRecord {
                text: task.objective,
                source: "task_objective".into(),
            },
            files,
            commit_sha: Some(change_id.to_string()),
            committed_files,
            working_tree_files,
            runtime_artifacts,
            ignored_files,
            command_ids: Vec::new(),
            verification_ids,
            risks: Vec::new(),
            created_at: timestamp(),
        };
        self.store.save_change_set(&self.workspace_id, &change)?;
        Ok(change)
    }

    pub fn load_change_set(&self, change_id: &str) -> HarnessResult<Option<ChangeSet>> {
        self.store.load_change_set(&self.workspace_id, change_id)
    }

    pub fn list_change_sets(&self, task_id: &str) -> HarnessResult<Vec<ChangeSet>> {
        self.store.list_change_sets(&self.workspace_id, task_id)
    }

    pub fn complete_task(
        &self,
        task_id: &str,
        verified: bool,
        session_status: HarnessSessionStatus,
    ) -> HarnessResult<TaskSession> {
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(task_id)?;
                if !task.status.is_writable() {
                    return Err(HarnessError::new(
                        "TASK_NOT_WRITABLE",
                        "当前任务已经关闭，不能重复完成",
                    ));
                }
                task.status = if verified {
                    TaskStatus::Completed
                } else {
                    TaskStatus::CompletedUnverified
                };
                task.updated_at = timestamp();
                transaction.save_task(&task)?;
                let default_task_id = self
                    .store
                    .load_workspace_state(&self.workspace_id)?
                    .and_then(|state| state.active_task_id)
                    .filter(|current| current != task_id)
                    .or(self.preferred_default_task_id(Some(task_id))?);
                let workspace_state = self.workspace_state(
                    default_task_id.as_deref(),
                    session_status,
                    &task.updated_at,
                )?;
                let actual_session_status = workspace_state.session_status;
                transaction.save_workspace_state(&workspace_state)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    task_id,
                    "task_completed",
                    Some("finish_task"),
                    json!({
                        "verification_status": if verified { "verified" } else { "unverified" },
                        "requested_session_status": session_status,
                        "session_status": actual_session_status,
                        "next_stage_started": false
                    }),
                    json!({"ok": true, "closed": true}),
                ))?;
                Ok(task)
            })
    }

    pub fn current_task(&self) -> HarnessResult<Option<TaskSession>> {
        if let Some(state) = self.store.load_workspace_state(&self.workspace_id)? {
            if let Some(task_id) = state.active_task_id.as_deref() {
                let task = self.task(task_id)?;
                if task.status.is_writable() {
                    return Ok(Some(task));
                }
            }
        }
        self.preferred_default_task_id(None)?
            .map(|task_id| self.task(&task_id))
            .transpose()
    }

    pub fn active_tasks(&self) -> HarnessResult<Vec<TaskSession>> {
        Ok(self
            .store
            .list_tasks(&self.workspace_id)?
            .into_iter()
            .filter(|task| matches!(task.status, TaskStatus::Active | TaskStatus::Verifying))
            .collect())
    }

    fn preferred_default_task_id(&self, exclude: Option<&str>) -> HarnessResult<Option<String>> {
        let tasks = self.store.list_tasks(&self.workspace_id)?;
        Ok(tasks
            .iter()
            .find(|task| {
                exclude != Some(task.id.as_str())
                    && matches!(task.status, TaskStatus::Active | TaskStatus::Verifying)
            })
            .or_else(|| {
                tasks
                    .iter()
                    .find(|task| exclude != Some(task.id.as_str()) && task.status.is_writable())
            })
            .map(|task| task.id.clone()))
    }

    pub fn list_tasks(&self) -> HarnessResult<Vec<TaskSession>> {
        self.store.list_tasks(&self.workspace_id)
    }

    pub fn task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.store.load_task(&self.workspace_id, task_id)
    }

    pub fn transition(&self, task_id: &str, next: TaskStatus) -> HarnessResult<TaskSession> {
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(task_id)?;
                if !task.status.can_transition_to(next) {
                    return Err(HarnessError::new(
                        "INVALID_TASK_TRANSITION",
                        format!("不允许从 {:?} 转换到 {:?}", task.status, next),
                    ));
                }
                task.status = next;
                task.updated_at = timestamp();
                transaction.save_task(&task)?;
                let existing_default = self
                    .store
                    .load_workspace_state(&self.workspace_id)?
                    .and_then(|state| state.active_task_id);
                let active_task_id =
                    if matches!(task.status, TaskStatus::Active | TaskStatus::Verifying) {
                        Some(task.id.clone())
                    } else if existing_default.as_deref() == Some(task.id.as_str()) {
                        self.preferred_default_task_id(Some(task.id.as_str()))?
                    } else {
                        existing_default
                    };
                transaction.save_workspace_state(&self.workspace_state(
                    active_task_id.as_deref(),
                    HarnessSessionStatus::Paused,
                    &task.updated_at,
                )?)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    task_id,
                    "task_status_changed",
                    None,
                    json!({"status": next}),
                    json!({"ok": true}),
                ))?;
                Ok(task)
            })
    }

    pub fn update_steps(
        &self,
        task_id: &str,
        completed_steps: Option<Vec<String>>,
        pending_steps: Option<Vec<String>>,
    ) -> HarnessResult<TaskSession> {
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(task_id)?;
                if let Some(steps) = completed_steps {
                    task.completed_steps = steps;
                }
                if let Some(steps) = pending_steps {
                    task.pending_steps = steps;
                }
                task.updated_at = timestamp();
                transaction.save_task(&task)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    task_id,
                    "task_updated",
                    None,
                    json!({
                        "completed_steps": task.completed_steps,
                        "pending_steps": task.pending_steps
                    }),
                    json!({"ok": true}),
                ))?;
                Ok(task)
            })
    }

    pub fn check_baseline(&self, task_id: &str) -> HarnessResult<()> {
        let task = self.task(task_id)?;
        let current = capture_baseline(&self.workspace_root);
        let expected = expected_state(&task);
        if current.branch != expected.branch || current.head != expected.head {
            return Err(HarnessError::new(
                "BASELINE_STALE",
                "Git 分支或 HEAD 已发生变化",
            ));
        }
        if current.worktree_fingerprint != expected.worktree_fingerprint {
            return Err(HarnessError::new(
                "FILE_CHANGED_EXTERNALLY",
                "工作区存在 Harness 未记录的外部文件变化",
            ));
        }
        Ok(())
    }

    pub fn refresh_expected_state(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.refresh_expected_state_for_operation(task_id, None)
    }

    pub fn refresh_expected_state_for_operation(
        &self,
        task_id: &str,
        operation_id: Option<&str>,
    ) -> HarnessResult<TaskSession> {
        let current = capture_baseline(&self.workspace_root);
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let now = timestamp();
                let mut selected = None;
                let mut synchronized_task_ids = Vec::new();
                for mut task in self.store.list_tasks(&self.workspace_id)? {
                    if !task.status.is_writable() {
                        continue;
                    }
                    task.expected_state = expected_state_from_baseline(&current, operation_id);
                    if task.id == task_id {
                        task.updated_at = now.clone();
                        selected = Some(task.clone());
                    }
                    synchronized_task_ids.push(task.id.clone());
                    transaction.save_task(&task)?;
                }
                let selected = selected.ok_or_else(|| {
                    HarnessError::new(
                        "TASK_NOT_FOUND",
                        format!("Task not found or not writable: {task_id}"),
                    )
                })?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    task_id,
                    "workspace_baseline_synchronized",
                    None,
                    json!({
                        "operation_id": operation_id,
                        "synchronized_task_ids": synchronized_task_ids
                    }),
                    json!({"ok": true}),
                ))?;
                Ok(selected)
            })
    }

    pub fn refresh_baseline(
        &self,
        task_id: &str,
        observed_head: Option<&str>,
        observed_fingerprint: &str,
        reason: &str,
    ) -> HarnessResult<TaskSession> {
        if reason.trim().is_empty() {
            return Err(HarnessError::new(
                "INVALID_ARGUMENT",
                "refresh_baseline 必须提供接受当前状态的原因",
            ));
        }
        let current = capture_baseline(&self.workspace_root);
        if current.head.as_deref() != observed_head
            || current.worktree_fingerprint != observed_fingerprint
        {
            return Err(HarnessError::new(
                "BASELINE_REFRESH_CAS_FAILED",
                "工作区状态已再次变化；请重新读取 harness_status/project_state 后重试",
            ));
        }
        let operation_id = Uuid::new_v4().simple().to_string();
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(task_id)?;
                if !task.status.is_writable() {
                    return Err(HarnessError::new(
                        "TASK_NOT_WRITABLE",
                        "当前任务状态不允许刷新基线",
                    ));
                }
                task.expected_state = expected_state_from_baseline(&current, Some(&operation_id));
                task.updated_at = timestamp();
                transaction.save_task(&task)?;
                transaction.append_event(&harness_event(
                    &self.workspace_id,
                    task_id,
                    "baseline_refreshed",
                    Some("refresh_baseline"),
                    json!({
                        "observed_head": observed_head,
                        "observed_fingerprint": observed_fingerprint,
                        "reason": reason
                    }),
                    json!({
                        "ok": true,
                        "branch": current.branch,
                        "head": current.head,
                        "worktree_fingerprint": current.worktree_fingerprint,
                        "operation_id": operation_id
                    }),
                ))?;
                Ok(task)
            })
    }

    pub fn record_event(
        &self,
        task_id: &str,
        kind: &str,
        tool_name: Option<&str>,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<HarnessEvent> {
        let event = harness_event(
            &self.workspace_id,
            task_id,
            kind,
            tool_name,
            input_summary,
            result_summary,
        );
        self.store
            .append_event_for_workspace(&self.workspace_id, &event)?;
        Ok(event)
    }

    pub fn list_events(
        &self,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        self.store
            .list_events(&self.workspace_id, task_id, offset, limit)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_operation(
        &self,
        operation_id: Option<&str>,
        task_id: Option<&str>,
        mcp_session_id: Option<&str>,
        tool: &str,
        kind: &str,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<OperationRecord> {
        let reason = input_summary
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let history_session_key = task_id
            .and_then(|task_id| self.task(task_id).ok())
            .and_then(|task| task.history_session_key);
        let affected_files = result_summary
            .get("affected_files")
            .and_then(serde_json::Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(|file| {
                        let path = file.get("path")?.as_str()?.to_string();
                        let status = file
                            .get("operation")
                            .or_else(|| file.get("status"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("changed")
                            .to_string();
                        Some(FileChangeRecord {
                            path,
                            status,
                            before_sha256: None,
                            after_sha256: None,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let operation = OperationRecord {
            id: operation_id
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
            workspace_id: self.workspace_id.clone(),
            task_id: task_id.map(str::to_string),
            history_session_key,
            mcp_session_id: mcp_session_id.map(str::to_string),
            tool: tool.to_string(),
            kind: kind.to_string(),
            input_summary,
            result_summary,
            reason,
            affected_files,
            created_at: timestamp(),
            created_at_iso: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
        self.store
            .append_operation(&self.workspace_id, &operation)?;
        Ok(operation)
    }

    pub fn list_operations(
        &self,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<OperationRecord>> {
        self.store
            .list_operations(&self.workspace_id, offset, limit)
    }

    pub fn load_stage_commit_receipt(
        &self,
        idempotency_key: &str,
    ) -> HarnessResult<Option<StageCommitReceipt>> {
        self.store
            .load_stage_commit_receipt(&self.workspace_id, idempotency_key)
    }

    pub fn save_stage_commit_receipt(&self, receipt: &StageCommitReceipt) -> HarnessResult<()> {
        self.store
            .save_stage_commit_receipt(&self.workspace_id, receipt)
    }

    pub fn save_close_outbox(&self, outbox: &WorkSessionCloseOutbox) -> HarnessResult<()> {
        self.store.save_close_outbox(&self.workspace_id, outbox)
    }

    pub fn load_close_outbox(
        &self,
        task_id: &str,
    ) -> HarnessResult<Option<WorkSessionCloseOutbox>> {
        self.store.load_close_outbox(&self.workspace_id, task_id)
    }

    pub fn list_close_outboxes(&self) -> HarnessResult<Vec<WorkSessionCloseOutbox>> {
        self.store.list_close_outboxes(&self.workspace_id)
    }

    pub fn delete_close_outbox(&self, task_id: &str) -> HarnessResult<()> {
        self.store.delete_close_outbox(&self.workspace_id, task_id)
    }

    pub fn set_latest_change(&self, task_id: &str, change_id: &str) -> HarnessResult<TaskSession> {
        self.store
            .with_workspace_transaction(&self.workspace_id, |transaction| {
                let mut task = self.task(task_id)?;
                task.latest_change_id = Some(change_id.to_string());
                task.updated_at = timestamp();
                transaction.save_task(&task)?;
                Ok(task)
            })
    }

    pub fn project_state(&self, max_files: usize) -> HarnessResult<ProjectState> {
        let task_id = self.current_task()?.map(|task| task.id);
        self.project_state_for_task(max_files, task_id.as_deref())
    }

    pub fn project_state_for_task(
        &self,
        max_files: usize,
        selected_task_id: Option<&str>,
    ) -> HarnessResult<ProjectState> {
        let current = capture_baseline_snapshot(&self.workspace_root);
        let task = selected_task_id
            .map(|task_id| self.task(task_id))
            .transpose()?
            .filter(|task| task.status.is_writable());
        let baseline_object = task
            .as_ref()
            .map(|task| {
                self.store
                    .load_baseline_object(&self.workspace_id, &task.baseline.object_id)
            })
            .transpose()?;
        let baseline_map = baseline_object
            .as_ref()
            .map(|baseline| {
                baseline
                    .entries
                    .iter()
                    .map(|entry| (entry.path.clone(), entry))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let current_map: HashMap<_, _> = current
            .object
            .entries
            .iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let mut paths: Vec<String> = baseline_map
            .keys()
            .chain(current_map.keys())
            .cloned()
            .collect();
        paths.sort();
        paths.dedup();
        let total_files = paths.len();
        let files = paths
            .into_iter()
            .map(|path| {
                let before = baseline_map.get(&path).map(|e| e.sha256.clone());
                let entry = current_map.get(&path);
                let status = match (before, entry) {
                    (Some(before), Some(entry)) if before == entry.sha256 => "unchanged",
                    (Some(_), Some(_)) => "modified",
                    (Some(_), None) => "deleted",
                    (None, Some(_)) => "added",
                    (None, None) => "unknown",
                };
                ProjectFileState {
                    path,
                    status: status.to_string(),
                    sha256: entry.map(|e| e.sha256.clone()).unwrap_or_default(),
                    bytes: entry.map(|e| e.bytes).unwrap_or(0),
                }
            })
            .collect::<Vec<_>>();
        let clean = files.iter().all(|file| file.status == "unchanged");
        let truncated = files.len() > max_files.max(1);
        let files = files.into_iter().take(max_files.max(1)).collect::<Vec<_>>();
        let active_task_id = task.as_ref().map(|t| t.id.clone());
        let active_task_ids = self
            .active_tasks()?
            .into_iter()
            .map(|task| task.id)
            .collect();
        let recent_events = task
            .as_ref()
            .and_then(|t| self.list_events(&t.id, 0, 100).ok())
            .map(|events| events.len())
            .unwrap_or(0);
        Ok(ProjectState {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            branch: current.baseline.branch,
            head: current.baseline.head,
            clean,
            files,
            total_files,
            truncated,
            active_task_id,
            active_task_ids,
            task,
            recent_events,
        })
    }

    pub fn status(&self) -> HarnessResult<HarnessStatus> {
        let task_id = self.current_task()?.map(|task| task.id);
        self.status_for_task(task_id.as_deref())
    }

    pub fn status_for_task(&self, selected_task_id: Option<&str>) -> HarnessResult<HarnessStatus> {
        let current = capture_baseline(&self.workspace_root);
        let workspace_state = self.store.load_workspace_state(&self.workspace_id)?;
        let default_task_id = workspace_state
            .as_ref()
            .and_then(|state| state.active_task_id.clone());
        let task = selected_task_id
            .map(|task_id| self.task(task_id))
            .transpose()?
            .filter(|task| task.status.is_writable());
        let active_task_ids = self
            .active_tasks()?
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>();
        let active_task_count = active_task_ids.len();
        let session_status = workspace_state
            .as_ref()
            .map(|state| state.session_status)
            .unwrap_or_else(|| {
                if task.is_some() {
                    HarnessSessionStatus::Active
                } else {
                    HarnessSessionStatus::Paused
                }
            });
        let expected = task.as_ref().map(expected_state);
        let (task_id, task_state, task_updated_at, writable, baseline_matches, reason) =
            match task.as_ref() {
                Some(task) => {
                    let expected = expected_state(task);
                    let matches = expected.branch == current.branch
                        && expected.head == current.head
                        && expected.worktree_fingerprint == current.worktree_fingerprint;
                    let reason = if matches && active_task_count > 1 {
                        format!("任务可继续执行；当前有 {active_task_count} 个并行活动任务")
                    } else if matches {
                        "任务可继续执行".to_string()
                    } else {
                        "工作区基线已变化，写入和执行已暂停".to_string()
                    };
                    (
                        Some(task.id.clone()),
                        Some(task.status),
                        Some(task.updated_at.clone()),
                        matches && task.status.is_writable(),
                        Some(matches),
                        reason,
                    )
                }
                None => {
                    let writable = active_task_count == 0;
                    let reason = if !writable {
                        "当前 MCP 会话未绑定 Harness Task；工作区已有活动写任务，必须先绑定该任务"
                            .to_string()
                    } else {
                        "当前没有活动任务，工作区采用无任务模式；修改不会进入任务事件流".to_string()
                    };
                    (None, None, None, writable, None, reason)
                }
            };

        let mut capabilities = HashMap::new();
        capabilities.insert(
            "read".into(),
            CapabilityStatus {
                status: "available".into(),
                reason: "工作区读取不依赖活动任务".into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "write".into(),
            CapabilityStatus {
                status: if writable { "available" } else { "denied" }.into(),
                reason: if writable {
                    if task_id.is_some() {
                        "活动任务和工作区基线有效"
                    } else if active_task_count > 0 {
                        "当前会话未绑定活动写任务；必须使用 switch_task 或 begin_work_session 建立绑定"
                    } else {
                        "无任务模式允许直接修改，建议需要长期追踪时调用 start_task"
                    }
                } else {
                    "需要活动任务且工作区基线必须匹配"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "exec".into(),
            CapabilityStatus {
                status: if writable { "available" } else { "denied" }.into(),
                reason: if writable {
                    if task_id.is_some() {
                        "活动任务和工作区基线有效"
                    } else if active_task_count > 0 {
                        "当前会话未绑定活动写任务；必须使用 switch_task 或 begin_work_session 建立绑定"
                    } else {
                        "无任务模式允许直接执行，建议需要长期追踪时调用 start_task"
                    }
                } else {
                    "需要活动任务且工作区基线必须匹配"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "git".into(),
            CapabilityStatus {
                status: if current.branch.is_some() && current.head.is_some() {
                    "available"
                } else {
                    "degraded"
                }
                .into(),
                reason: if current.branch.is_some() && current.head.is_some() {
                    "已读取当前分支和 HEAD"
                } else {
                    "当前工作区不是可读取 Git 状态的仓库"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "network".into(),
            CapabilityStatus {
                status: "managed_by_policy".into(),
                reason: "网络权限由工具策略控制，不由 Harness 任务状态决定".into(),
                recoverable: true,
            },
        );

        let mut next_actions = Vec::new();
        if task_id.is_none() {
            if active_task_count > 0 {
                next_actions.push("switch_task".into());
                next_actions.push("project_state".into());
            }
            next_actions.push("start_task".into());
        } else if baseline_matches == Some(false) {
            next_actions.push("project_state".into());
            next_actions.push("git_diff".into());
            next_actions.push("accept_current_baseline".into());
            next_actions.push("refresh_baseline".into());
        } else if !writable {
            next_actions.push("resume_task".into());
        }
        next_actions.push("read_file".into());
        next_actions.push("git_status".into());

        Ok(HarnessStatus {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            default_task_id,
            active_task_ids,
            active_task_count,
            task_id,
            task_state,
            task_updated_at,
            session_status,
            next_stage_started: false,
            writable,
            reason,
            recoverable: true,
            branch: current.branch.clone(),
            head: current.head.clone(),
            worktree_fingerprint: current.worktree_fingerprint.clone(),
            expected_branch: expected.as_ref().and_then(|state| state.branch.clone()),
            expected_head: expected.as_ref().and_then(|state| state.head.clone()),
            expected_fingerprint: expected
                .as_ref()
                .map(|state| state.worktree_fingerprint.clone()),
            observation_token: task
                .as_ref()
                .map(|task| baseline_observation_token(&self.workspace_id, &task.id, &current)),
            baseline_matches,
            capabilities,
            next_actions,
            journal_health: self.store.journal_health(&self.workspace_id)?,
        })
    }

    fn workspace_state(
        &self,
        active_task_id: Option<&str>,
        session_status: HarnessSessionStatus,
        updated_at: &str,
    ) -> HarnessResult<WorkspaceHarnessState> {
        let tasks = self.store.list_tasks(&self.workspace_id)?;
        let active_task_ids = tasks
            .iter()
            .filter(|task| matches!(task.status, TaskStatus::Active | TaskStatus::Verifying))
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let writable_task_ids = tasks
            .iter()
            .filter(|task| task.status.is_writable())
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let default_task_id = active_task_id
            .filter(|task_id| {
                writable_task_ids
                    .iter()
                    .any(|candidate| candidate == task_id)
            })
            .map(str::to_string)
            .or_else(|| active_task_ids.first().cloned())
            .or_else(|| writable_task_ids.first().cloned());
        let session_status = if !active_task_ids.is_empty() {
            HarnessSessionStatus::Active
        } else if !writable_task_ids.is_empty() {
            HarnessSessionStatus::Paused
        } else {
            session_status
        };
        Ok(WorkspaceHarnessState {
            schema_version: SCHEMA_VERSION,
            active_task_id: default_task_id,
            active_task_ids,
            session_status,
            recent_task_ids: tasks.into_iter().take(20).map(|task| task.id).collect(),
            updated_at: updated_at.to_string(),
        })
    }
}

fn harness_event(
    workspace_id: &str,
    task_id: &str,
    kind: &str,
    tool_name: Option<&str>,
    input_summary: serde_json::Value,
    result_summary: serde_json::Value,
) -> HarnessEvent {
    HarnessEvent {
        id: Uuid::new_v4().simple().to_string(),
        task_id: task_id.to_string(),
        operation_id: Uuid::new_v4().simple().to_string(),
        kind: kind.to_string(),
        tool_name: tool_name.map(str::to_string),
        input_summary: json!({"workspace_id": workspace_id, "payload": input_summary}),
        result_summary,
        reason: None,
        affected_files: Vec::new(),
        created_at: timestamp(),
    }
}

fn baseline_paths(root: &Path) -> Vec<PathBuf> {
    if let Some(paths) = git_file_paths(root) {
        return paths;
    }
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|item| {
            let path = item.path();
            path != root && !should_skip(path, root) && item.file_type().is_file()
        })
        .map(|item| item.into_path())
        .collect()
}

fn git_file_paths(root: &Path) -> Option<Vec<PathBuf>> {
    let mut command = Command::new("git");
    crate::platform::hide_std_console(&mut command);
    let output = command
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
        .filter_map(|value| std::str::from_utf8(value).ok())
        .map(|relative| root.join(relative))
        .filter(|path| path.exists())
        .filter(|path| !should_skip(path, root))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Some(paths)
}

fn expected_state(task: &TaskSession) -> ExpectedWorkspaceState {
    task.expected_state.clone()
}

fn expected_state_from_baseline(
    baseline: &ProjectBaseline,
    operation_id: Option<&str>,
) -> ExpectedWorkspaceState {
    ExpectedWorkspaceState {
        branch: baseline.branch.clone(),
        head: baseline.head.clone(),
        worktree_fingerprint: baseline.worktree_fingerprint.clone(),
        accepted_at: timestamp(),
        accepted_by_operation_id: operation_id.map(str::to_string),
    }
}

struct CapturedBaseline {
    baseline: ProjectBaseline,
    object: BaselineObject,
}

pub(crate) fn capture_baseline(root: &Path) -> ProjectBaseline {
    capture_baseline_snapshot(root).baseline
}

fn capture_baseline_snapshot(root: &Path) -> CapturedBaseline {
    let entries = capture_baseline_entries(root);
    let mut fingerprint = Sha256::new();
    for entry in &entries {
        fingerprint.update(entry.path.as_bytes());
        fingerprint.update(entry.sha256.as_bytes());
        fingerprint.update(entry.bytes.to_le_bytes());
    }
    let worktree_fingerprint = format!("{:x}", fingerprint.finalize());
    let object_id = baseline_object_id(&entries).expect("baseline entries are serializable");
    CapturedBaseline {
        baseline: ProjectBaseline {
            schema_version: SCHEMA_VERSION,
            branch: git_value(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
            head: git_value(root, &["rev-parse", "HEAD"]),
            worktree_fingerprint,
            object_id: object_id.clone(),
            file_count: entries.len(),
            captured_at: timestamp(),
        },
        object: BaselineObject {
            schema_version: SCHEMA_VERSION,
            id: object_id,
            entries,
        },
    }
}

pub(crate) fn capture_baseline_entries(root: &Path) -> Vec<BaselineEntry> {
    let mut entries = Vec::new();
    for path in baseline_paths(root) {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(root) {
            continue;
        }
        let Ok(bytes) = fs::read(&canonical) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        entries.push(BaselineEntry {
            path: rel,
            exists: true,
            is_binary: bytes.contains(&0),
            sha256: format!("{:x}", hasher.finalize()),
            bytes: bytes.len() as u64,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

pub(crate) fn diff_baseline_entries(
    before: &[BaselineEntry],
    after: &[BaselineEntry],
) -> Vec<FileChangeRecord> {
    let before = before
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let after = after
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut paths = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|path| match (before.get(path), after.get(path)) {
            (None, Some(current)) => Some(FileChangeRecord {
                path: path.to_string(),
                status: "added".into(),
                before_sha256: None,
                after_sha256: Some(current.sha256.clone()),
            }),
            (Some(previous), None) => Some(FileChangeRecord {
                path: path.to_string(),
                status: "deleted".into(),
                before_sha256: Some(previous.sha256.clone()),
                after_sha256: None,
            }),
            (Some(previous), Some(current)) if previous.sha256 != current.sha256 => {
                Some(FileChangeRecord {
                    path: path.to_string(),
                    status: "modified".into(),
                    before_sha256: Some(previous.sha256.clone()),
                    after_sha256: Some(current.sha256.clone()),
                })
            }
            _ => None,
        })
        .collect()
}

fn should_skip(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .into_iter()
        .flat_map(|p| p.components())
        .filter_map(|component| component.as_os_str().to_str())
        .any(|name| {
            matches!(
                name,
                ".git"
                    | "workspace-id.json"
                    | "workspace-id.lock"
                    | ".mcp-probe-kit"
                    | "node_modules"
                    | "target"
                    | "dist"
                    | "build"
                    | ".svelte-kit"
            )
        })
}

fn git_value(root: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    crate::platform::hide_std_console(&mut command);
    let output = command.arg("-C").arg(root).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
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
    use tempfile::tempdir;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn initialize_git(root: &Path) {
        git(root, &["init"]);
        git(
            root,
            &["config", "user.email", "anchor-tests@example.invalid"],
        );
        git(root, &["config", "user.name", "Anchor Tests"]);
    }

    #[test]
    fn status_keeps_read_available_without_task() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");

        let status = harness.status().expect("status");
        assert!(status.writable);
        assert_eq!(status.capabilities["read"].status, "available");
        assert_eq!(status.capabilities["write"].status, "available");
        assert!(status.next_actions.contains(&"start_task".to_string()));
    }

    #[test]
    fn starting_task_does_not_create_workspace_copies() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");

        harness.start_task("测试任务").expect("start task");
        assert!(!harness
            .store_root()
            .join("workspaces")
            .join(harness.workspace_id())
            .join("snapshots")
            .exists());
    }

    #[test]
    fn meaningful_tool_activity_auto_resumes_a_paused_task_once() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let task = harness.start_task("resume activity").expect("start task");
        harness
            .transition(&task.id, TaskStatus::Paused)
            .expect("pause task");

        let resumed = harness
            .resume_paused_task_for_activity("read_file", Some("mcp-session"))
            .expect("auto resume")
            .expect("current task");
        assert_eq!(resumed.status, TaskStatus::Active);
        let status = harness.status().expect("status");
        assert_eq!(status.task_state, Some(TaskStatus::Active));
        assert_eq!(status.session_status, HarnessSessionStatus::Active);

        let events = harness
            .list_events(&task.id, 0, usize::MAX)
            .expect("events");
        let auto_resumed = events
            .iter()
            .filter(|event| event.kind == "task_auto_resumed")
            .collect::<Vec<_>>();
        assert_eq!(auto_resumed.len(), 1);
        assert_eq!(auto_resumed[0].tool_name.as_deref(), Some("read_file"));
        assert_eq!(
            auto_resumed[0].input_summary["payload"]["mcp_session_id"],
            "mcp-session"
        );

        harness
            .resume_paused_task_for_activity("git_status", Some("mcp-session"))
            .expect("already active");
        let events = harness
            .list_events(&task.id, 0, usize::MAX)
            .expect("events");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "task_auto_resumed")
                .count(),
            1
        );
    }

    #[test]
    fn status_and_lifecycle_tools_do_not_auto_resume_paused_tasks() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let task = harness.start_task("stay paused").expect("start task");
        harness
            .transition(&task.id, TaskStatus::Paused)
            .expect("pause task");

        for tool in [
            "harness_status",
            "task_context",
            "pause_task",
            "resume_task",
            "finish_task",
            "close_work_session",
        ] {
            let current = harness
                .resume_paused_task_for_activity(tool, None)
                .expect("inspect paused task")
                .expect("current task");
            assert_eq!(current.status, TaskStatus::Paused, "tool={tool}");
        }
        assert_eq!(
            harness.task(&task.id).expect("task").status,
            TaskStatus::Paused
        );
    }

    #[test]
    fn auto_resume_does_not_override_verifying_or_failed_states() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let task = harness.start_task("preserve status").expect("start task");
        harness
            .transition(&task.id, TaskStatus::Verifying)
            .expect("verifying");
        let verifying = harness
            .resume_paused_task_for_activity("read_file", None)
            .expect("activity")
            .expect("current task");
        assert_eq!(verifying.status, TaskStatus::Verifying);

        harness
            .transition(&task.id, TaskStatus::Failed)
            .expect("failed");
        let failed = harness
            .resume_paused_task_for_activity("read_file", None)
            .expect("activity")
            .expect("current task");
        assert_eq!(failed.status, TaskStatus::Failed);
    }

    #[test]
    fn git_baseline_ignores_history_metadata() {
        let workspace = tempdir().expect("workspace");
        initialize_git(workspace.path());
        fs::write(
            workspace.path().join(".gitignore"),
            "docs/history-session/\n",
        )
        .expect("gitignore");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        git(workspace.path(), &["add", ".gitignore", "main.rs"]);
        git(workspace.path(), &["commit", "-m", "initial"]);

        let before = capture_baseline(workspace.path());
        let history = workspace.path().join("docs/history-session");
        fs::create_dir_all(&history).expect("history dir");
        fs::write(history.join("1.md"), "checkpoint\n").expect("history");
        let after = capture_baseline_snapshot(workspace.path());

        assert_eq!(
            before.worktree_fingerprint,
            after.baseline.worktree_fingerprint
        );
        assert!(!after
            .object
            .entries
            .iter()
            .any(|entry| entry.path.starts_with("docs/history-session/")));
    }

    #[test]
    fn controlled_commit_advances_expected_head_without_replacing_baseline() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        initialize_git(workspace.path());
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        git(workspace.path(), &["add", "main.rs"]);
        git(workspace.path(), &["commit", "-m", "initial"]);
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let started = harness.start_task("commit test").expect("start");
        let initial_head = started.baseline.head.clone();

        fs::write(
            workspace.path().join("main.rs"),
            "fn main() { println!(\"ok\"); }\n",
        )
        .expect("change");
        git(workspace.path(), &["add", "main.rs"]);
        git(workspace.path(), &["commit", "-m", "change"]);
        assert!(harness.check_baseline(&started.id).is_err());

        let refreshed = harness
            .refresh_expected_state_for_operation(&started.id, Some("commit-operation"))
            .expect("refresh");
        harness.check_baseline(&started.id).expect("baseline valid");
        assert_eq!(refreshed.baseline.head, initial_head);
        assert_ne!(refreshed.expected_state.head, initial_head);
        assert_eq!(
            refreshed.expected_state.accepted_by_operation_id.as_deref(),
            Some("commit-operation")
        );
    }

    #[test]
    fn refresh_baseline_rejects_stale_observation() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "one\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let task = harness.start_task("refresh test").expect("start");
        fs::write(workspace.path().join("main.rs"), "two\n").expect("first change");
        let observed = capture_baseline(workspace.path());
        fs::write(workspace.path().join("main.rs"), "three\n").expect("second change");

        let error = harness
            .refresh_baseline(
                &task.id,
                observed.head.as_deref(),
                &observed.worktree_fingerprint,
                "accept known external change",
            )
            .expect_err("stale observation must fail");
        assert_eq!(error.code(), "BASELINE_REFRESH_CAS_FAILED");
    }

    #[test]
    fn stable_workspace_id_survives_directory_move_and_records_aliases() {
        let root = tempdir().expect("root");
        let first = root.path().join("workspace-first");
        let second = root.path().join("workspace-second");
        let harness_root = root.path().join("harness-store");
        fs::create_dir_all(&first).expect("workspace");
        initialize_git(&first);
        fs::write(first.join("main.rs"), "fn main() {}\n").expect("file");

        let first_harness =
            Harness::new(first.clone(), harness_root.clone()).expect("first harness");
        let workspace_id = first_harness.workspace_id().to_string();
        drop(first_harness);
        fs::rename(&first, &second).expect("move workspace");

        let second_harness =
            Harness::new(second.clone(), harness_root.clone()).expect("second harness");
        assert_eq!(second_harness.workspace_id(), workspace_id);
        let identity_path = harness_root
            .join("workspaces")
            .join(&workspace_id)
            .join("identity.json");
        let identity: super::super::model::WorkspaceIdentity =
            serde_json::from_slice(&fs::read(identity_path).expect("identity bytes"))
                .expect("identity");
        assert!(identity
            .aliases
            .iter()
            .any(|path| path.ends_with("workspace-first")));
        assert!(identity
            .aliases
            .iter()
            .any(|path| path.ends_with("workspace-second")));
    }

    #[test]
    fn task_references_content_addressed_baseline_without_inline_entries() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let task = harness.start_task("baseline object").expect("start");
        assert_eq!(task.baseline.file_count, 1);
        assert_eq!(task.baseline.object_id.len(), 64);

        let task_path = harness
            .store_root()
            .join("workspaces")
            .join(harness.workspace_id())
            .join("tasks")
            .join(format!("{}.json", task.id));
        let task_json: serde_json::Value =
            serde_json::from_slice(&fs::read(task_path).expect("task bytes")).expect("task json");
        assert!(task_json["baseline"].get("entries").is_none());
        assert_eq!(task_json["baseline"]["object_id"], task.baseline.object_id);
        let baseline = harness
            .store
            .load_baseline_object(harness.workspace_id(), &task.baseline.object_id)
            .expect("baseline object");
        assert_eq!(baseline.entries.len(), task.baseline.file_count);
    }

    #[test]
    fn concurrent_operation_appends_keep_sequences_and_checksums_valid() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let mut handles = Vec::new();
        for index in 0..12 {
            let harness = harness.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                harness
                    .record_operation(
                        None,
                        None,
                        None,
                        "concurrency_probe",
                        "completed",
                        json!({"index": index}),
                        json!({"ok": true}),
                    )
                    .expect("operation");
            }));
        }
        for handle in handles {
            handle.join().expect("thread");
        }
        assert_eq!(
            harness.list_operations(0, 100).expect("operations").len(),
            12
        );
        let health = harness
            .store
            .journal_health(harness.workspace_id())
            .expect("journal health");
        assert_eq!(health.valid_records, 12);
        assert_eq!(health.corrupt_lines, 0);
        assert_eq!(health.checksum_failures, 0);
        assert_eq!(health.sequence_anomalies, 0);
    }

    #[test]
    fn diagnostic_failure_is_non_blocking_and_later_success_supersedes_blocking_failure() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let task = harness.start_task("verification levels").expect("start");

        let diagnostic = harness
            .record_verification(
                &task.id,
                "environment_probe",
                "pnpm --version",
                None,
                None,
                None,
                Some(1),
                false,
                Some(10),
                None,
                "diagnostic",
                true,
            )
            .expect("diagnostic");
        assert_eq!(diagnostic.level, "diagnostic");
        assert_eq!(
            diagnostic
                .dispositions
                .last()
                .map(|entry| entry.disposition.as_str()),
            Some("diagnostic_only")
        );

        let failed = harness
            .record_verification(
                &task.id,
                "lint",
                "make lint",
                None,
                None,
                None,
                Some(1),
                false,
                Some(20),
                None,
                "blocking",
                true,
            )
            .expect("failed lint");
        let passed = harness
            .record_verification(
                &task.id,
                "lint",
                "make lint",
                None,
                None,
                None,
                Some(0),
                true,
                Some(30),
                None,
                "blocking",
                true,
            )
            .expect("passed lint");
        assert_eq!(passed.supersedes, vec![failed.id.clone()]);

        let records = harness.list_verifications(&task.id).expect("verifications");
        let superseded = records
            .iter()
            .find(|record| record.id == failed.id)
            .expect("failed record");
        assert_eq!(
            superseded
                .dispositions
                .last()
                .map(|entry| entry.disposition.as_str()),
            Some("superseded")
        );
    }

    #[test]
    fn verification_key_supersedes_a_prior_failure_even_when_command_changes() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let task = harness.start_task("verification identity").expect("start");

        let failed = harness
            .record_verification(
                &task.id,
                "test",
                "pnpm vitest story-old",
                Some("story-live"),
                Some("tests/story-live.test.ts"),
                Some("Story live integration"),
                Some(1),
                false,
                Some(20),
                None,
                "blocking",
                true,
            )
            .expect("failed test");
        let passed = harness
            .record_verification(
                &task.id,
                "test",
                "pnpm vitest story-new --runInBand",
                Some("story-live"),
                Some("tests/story-live.test.ts"),
                Some("Story live integration"),
                Some(0),
                true,
                Some(30),
                None,
                "blocking",
                true,
            )
            .expect("passed test");

        assert_eq!(passed.supersedes, vec![failed.id.clone()]);
        let records = harness.list_verifications(&task.id).expect("records");
        let previous = records
            .iter()
            .find(|record| record.id == failed.id)
            .expect("previous verification");
        assert_eq!(verification_effective_disposition(previous), "superseded");
    }
}
