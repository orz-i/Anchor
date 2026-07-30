use std::collections::HashMap;
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
    BaselineEntry, CapabilityStatus, ChangeSet, ExpectedWorkspaceState, FileChangeRecord,
    HarnessEvent, HarnessSessionStatus, HarnessStatus, OperationRecord, ProjectBaseline,
    ProjectFileState, ProjectState, ReasonRecord, StageCommitReceipt, TaskSession, TaskStatus,
    VerificationRecord, WorkspaceHarnessState, SCHEMA_VERSION,
};
use super::store::{HarnessError, HarnessResult, HarnessStore};

#[derive(Debug, Clone)]
pub struct Harness {
    workspace_root: PathBuf,
    workspace_id: String,
    store: HarnessStore,
}

impl Harness {
    pub fn new(workspace_root: PathBuf, harness_root: PathBuf) -> HarnessResult<Self> {
        let workspace_root = workspace_root
            .canonicalize()
            .map_err(|e| HarnessError::new("WORKSPACE_UNAVAILABLE", e.to_string()))?;
        let workspace_id = workspace_id(&workspace_root);
        Ok(Self {
            workspace_root,
            workspace_id,
            store: HarnessStore::new(harness_root)?,
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
        let mut task = self.task(task_id)?;
        if let Some(existing) = task.history_session_key.as_deref() {
            if existing != session_key || task.history_session_path.as_deref() != Some(path) {
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
        self.store.save_task(&task)?;
        self.record_event(
            task_id,
            "history_session_bound",
            Some("begin_work_session"),
            json!({"session_key": session_key, "path": path}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn default_root() -> HarnessResult<PathBuf> {
        let root = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| HarnessError::new("STORE_UNAVAILABLE", "无法确定应用数据目录"))?;
        Ok(root.join("anchor").join("harness"))
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn store_root(&self) -> &Path {
        self.store.root()
    }

    pub fn start_task(&self, objective: &str) -> HarnessResult<TaskSession> {
        if objective.trim().is_empty() {
            return Err(HarnessError::new("INVALID_ARGUMENT", "任务目标不能为空"));
        }
        if let Some(task) = self.current_task()? {
            return Err(HarnessError::new(
                "TASK_ALREADY_ACTIVE",
                format!("工作区已有活动任务 {}", task.id),
            ));
        }
        let baseline = capture_baseline(&self.workspace_root);
        let now = timestamp();
        let task = TaskSession {
            id: Uuid::new_v4().simple().to_string(),
            workspace_id: self.workspace_id.clone(),
            objective: objective.trim().to_string(),
            status: TaskStatus::Active,
            expected_fingerprint: baseline.worktree_fingerprint.clone(),
            expected_state: Some(expected_state_from_baseline(&baseline, None)),
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
        self.store.save_task(&task)?;
        self.save_workspace_state(
            Some(&task.id),
            HarnessSessionStatus::Active,
            &task.updated_at,
        )?;
        self.record_event(
            &task.id,
            "task_started",
            None,
            json!({}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn mark_verifying(&self, task_id: &str) -> HarnessResult<TaskSession> {
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
            self.store.save_task(&task)?;
            self.save_workspace_state(
                Some(&task.id),
                HarnessSessionStatus::Active,
                &task.updated_at,
            )?;
            self.record_event(
                task_id,
                "task_verification_required",
                Some("finish_task"),
                json!({}),
                json!({"ok": false, "task_status": "verifying"}),
            )?;
        }
        Ok(task)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_verification(
        &self,
        task_id: &str,
        kind: &str,
        command: &str,
        exit_code: Option<i32>,
        passed: bool,
        duration_ms: Option<u64>,
        change_id: Option<&str>,
    ) -> HarnessResult<VerificationRecord> {
        let mut task = self.task(task_id)?;
        let verification = VerificationRecord {
            id: Uuid::new_v4().simple().to_string(),
            task_id: task_id.to_string(),
            command: command.to_string(),
            kind: kind.trim().to_string(),
            status: if passed { "passed" } else { "failed" }.into(),
            exit_code,
            passed,
            duration_ms,
            change_id: change_id.map(str::to_string),
            created_at: timestamp(),
        };
        self.store
            .save_verification(&self.workspace_id, &verification)?;
        task.latest_verification_id = Some(verification.id.clone());
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        self.record_event(
            task_id,
            "verification_recorded",
            Some("exec_command"),
            json!({
                "verification_id": verification.id,
                "kind": verification.kind,
                "command": verification.command
            }),
            json!({
                "ok": passed,
                "status": verification.status,
                "exit_code": verification.exit_code,
                "duration_ms": verification.duration_ms
            }),
        )?;
        Ok(verification)
    }

    pub fn list_verifications(&self, task_id: &str) -> HarnessResult<Vec<VerificationRecord>> {
        self.store.list_verifications(&self.workspace_id, task_id)
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

    pub fn complete_task(
        &self,
        task_id: &str,
        verified: bool,
        session_status: HarnessSessionStatus,
    ) -> HarnessResult<TaskSession> {
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
        self.store.save_task(&task)?;
        self.save_workspace_state(None, session_status, &task.updated_at)?;
        self.record_event(
            task_id,
            "task_completed",
            Some("finish_task"),
            json!({
                "verification_status": if verified { "verified" } else { "unverified" },
                "session_status": session_status,
                "next_stage_started": false
            }),
            json!({"ok": true, "closed": true}),
        )?;
        Ok(task)
    }

    pub fn current_task(&self) -> HarnessResult<Option<TaskSession>> {
        Ok(self
            .store
            .list_tasks(&self.workspace_id)?
            .into_iter()
            .find(|task| task.status.is_writable()))
    }

    pub fn task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.store.load_task(&self.workspace_id, task_id)
    }

    pub fn transition(&self, task_id: &str, next: TaskStatus) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        if !task.status.can_transition_to(next) {
            return Err(HarnessError::new(
                "INVALID_TASK_TRANSITION",
                format!("不允许从 {:?} 转换到 {:?}", task.status, next),
            ));
        }
        task.status = next;
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        let (active_task_id, session_status) = match task.status {
            TaskStatus::Active | TaskStatus::Verifying => {
                (Some(task.id.as_str()), HarnessSessionStatus::Active)
            }
            TaskStatus::Paused | TaskStatus::Failed => {
                (Some(task.id.as_str()), HarnessSessionStatus::Paused)
            }
            TaskStatus::Completed | TaskStatus::CompletedUnverified | TaskStatus::RolledBack => {
                (None, HarnessSessionStatus::Paused)
            }
        };
        self.save_workspace_state(active_task_id, session_status, &task.updated_at)?;
        self.record_event(
            task_id,
            "task_status_changed",
            None,
            json!({"status": next}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn update_steps(
        &self,
        task_id: &str,
        completed_steps: Option<Vec<String>>,
        pending_steps: Option<Vec<String>>,
    ) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        if let Some(steps) = completed_steps {
            task.completed_steps = steps;
        }
        if let Some(steps) = pending_steps {
            task.pending_steps = steps;
        }
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        self.record_event(
            task_id,
            "task_updated",
            None,
            json!({
                "completed_steps": task.completed_steps,
                "pending_steps": task.pending_steps
            }),
            json!({"ok": true}),
        )?;
        Ok(task)
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
        let mut task = self.task(task_id)?;
        let current = capture_baseline(&self.workspace_root);
        task.expected_fingerprint = current.worktree_fingerprint.clone();
        task.expected_state = Some(expected_state_from_baseline(&current, operation_id));
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        Ok(task)
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
        let mut task = self.task(task_id)?;
        if !task.status.is_writable() {
            return Err(HarnessError::new(
                "TASK_NOT_WRITABLE",
                "当前任务状态不允许刷新基线",
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
        task.expected_fingerprint = current.worktree_fingerprint.clone();
        task.expected_state = Some(expected_state_from_baseline(&current, Some(&operation_id)));
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        self.record_event(
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
        )?;
        Ok(task)
    }

    pub fn record_event(
        &self,
        task_id: &str,
        kind: &str,
        tool_name: Option<&str>,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<HarnessEvent> {
        let event = HarnessEvent {
            id: Uuid::new_v4().simple().to_string(),
            task_id: task_id.to_string(),
            operation_id: Uuid::new_v4().simple().to_string(),
            kind: kind.to_string(),
            tool_name: tool_name.map(str::to_string),
            input_summary: json!({"workspace_id": self.workspace_id, "payload": input_summary}),
            result_summary,
            reason: None,
            affected_files: Vec::<FileChangeRecord>::new(),
            created_at: timestamp(),
        };
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

    pub fn set_latest_change(&self, task_id: &str, change_id: &str) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        task.latest_change_id = Some(change_id.to_string());
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn project_state(&self, max_files: usize) -> HarnessResult<ProjectState> {
        let current = capture_baseline(&self.workspace_root);
        let task = self.current_task()?;
        let baseline_map = task
            .as_ref()
            .map(|t| {
                t.baseline
                    .entries
                    .iter()
                    .map(|e| (e.path.clone(), e))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let current_map: HashMap<_, _> = current
            .entries
            .iter()
            .map(|e| (e.path.clone(), e))
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
        let recent_events = task
            .as_ref()
            .and_then(|t| self.list_events(&t.id, 0, 100).ok())
            .map(|events| events.len())
            .unwrap_or(0);
        Ok(ProjectState {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            branch: current.branch,
            head: current.head,
            clean,
            files,
            total_files,
            truncated,
            active_task_id,
            task,
            recent_events,
        })
    }

    pub fn status(&self) -> HarnessResult<HarnessStatus> {
        let current = capture_baseline(&self.workspace_root);
        let task = self.current_task()?;
        let workspace_state = self.store.load_workspace_state(&self.workspace_id)?;
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
                    let reason = if matches {
                        "任务可继续执行"
                    } else {
                        "工作区基线已变化，写入和执行已暂停"
                    };
                    (
                        Some(task.id.clone()),
                        Some(task.status),
                        Some(task.updated_at.clone()),
                        matches && task.status.is_writable(),
                        Some(matches),
                        reason.to_string(),
                    )
                }
                None => (
                    None,
                    None,
                    None,
                    true,
                    None,
                    "当前没有活动任务，工作区采用无任务模式；修改不会进入任务事件流".to_string(),
                ),
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
            next_actions.push("start_task".into());
        } else if baseline_matches == Some(false) {
            next_actions.push("project_state".into());
            next_actions.push("git_diff".into());
            next_actions.push("refresh_baseline".into());
        } else if !writable {
            next_actions.push("resume_task".into());
        }
        next_actions.push("read_file".into());
        next_actions.push("git_status".into());

        Ok(HarnessStatus {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
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
            baseline_matches,
            capabilities,
            next_actions,
        })
    }

    fn save_workspace_state(
        &self,
        active_task_id: Option<&str>,
        session_status: HarnessSessionStatus,
        updated_at: &str,
    ) -> HarnessResult<()> {
        self.store.save_workspace_state(
            &self.workspace_id,
            &WorkspaceHarnessState {
                schema_version: SCHEMA_VERSION,
                active_task_id: active_task_id.map(str::to_string),
                session_status,
                recent_task_ids: self
                    .store
                    .list_tasks(&self.workspace_id)?
                    .into_iter()
                    .take(20)
                    .map(|t| t.id)
                    .collect(),
                updated_at: updated_at.to_string(),
            },
        )
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
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Some(paths)
}

fn expected_state(task: &TaskSession) -> ExpectedWorkspaceState {
    task.expected_state
        .clone()
        .unwrap_or_else(|| ExpectedWorkspaceState {
            branch: task.baseline.branch.clone(),
            head: task.baseline.head.clone(),
            worktree_fingerprint: task.expected_fingerprint.clone(),
            accepted_at: task.updated_at.clone(),
            accepted_by_operation_id: None,
        })
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

pub fn capture_baseline(root: &Path) -> ProjectBaseline {
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
    let mut fingerprint = Sha256::new();
    for entry in &entries {
        fingerprint.update(entry.path.as_bytes());
        fingerprint.update(entry.sha256.as_bytes());
        fingerprint.update(entry.bytes.to_le_bytes());
    }
    ProjectBaseline {
        branch: git_value(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        head: git_value(root, &["rev-parse", "HEAD"]),
        worktree_fingerprint: format!("{:x}", fingerprint.finalize()),
        entries,
        captured_at: timestamp(),
    }
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

fn workspace_id(root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..32].to_string()
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
        let after = capture_baseline(workspace.path());

        assert_eq!(before.worktree_fingerprint, after.worktree_fingerprint);
        assert!(!after
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
        assert_ne!(
            refreshed
                .expected_state
                .as_ref()
                .and_then(|state| state.head.clone()),
            initial_head
        );
        assert_eq!(
            refreshed
                .expected_state
                .as_ref()
                .and_then(|state| state.accepted_by_operation_id.as_deref()),
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
}
