use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::model::{
    ChangeSet, HarnessEvent, OperationRecord, StageCommitReceipt, TaskSession, VerificationRecord,
    WorkspaceHarnessState,
};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct HarnessError {
    code: &'static str,
    message: String,
}

impl HarnessError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

pub type HarnessResult<T> = Result<T, HarnessError>;

#[derive(Debug, Clone)]
pub struct HarnessStore {
    root: PathBuf,
}

impl HarnessStore {
    pub fn new(root: PathBuf) -> HarnessResult<Self> {
        fs::create_dir_all(&root)
            .map_err(|e| HarnessError::new("STORE_UNAVAILABLE", e.to_string()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn workspace_dir(&self, workspace_id: &str) -> PathBuf {
        self.root.join("workspaces").join(workspace_id)
    }

    fn tasks_dir(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("tasks")
    }

    fn events_dir(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("events")
    }

    fn operations_path(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("operations.jsonl")
    }

    fn verifications_dir(&self, workspace_id: &str, task_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id)
            .join("verifications")
            .join(task_id)
    }

    fn changes_dir(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("changes")
    }

    fn stage_commit_path(&self, workspace_id: &str, idempotency_key: &str) -> PathBuf {
        let digest = format!("{:x}", Sha256::digest(idempotency_key.as_bytes()));
        self.workspace_dir(workspace_id)
            .join("stage-commits")
            .join(format!("{digest}.json"))
    }

    pub fn save_task(&self, task: &TaskSession) -> HarnessResult<()> {
        let dir = self.tasks_dir(&task.workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        atomic_write_json(&dir.join(format!("{}.json", task.id)), task)
    }

    pub fn load_task(&self, workspace_id: &str, task_id: &str) -> HarnessResult<TaskSession> {
        read_json(&self.tasks_dir(workspace_id).join(format!("{task_id}.json")))
    }

    pub fn list_tasks(&self, workspace_id: &str) -> HarnessResult<Vec<TaskSession>> {
        let dir = self.tasks_dir(workspace_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut tasks: Vec<TaskSession> = Vec::new();
        for entry in fs::read_dir(dir).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(task) = read_json(&path) {
                tasks.push(task);
            }
        }
        tasks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(tasks)
    }

    pub fn save_workspace_state(
        &self,
        workspace_id: &str,
        state: &WorkspaceHarnessState,
    ) -> HarnessResult<()> {
        let dir = self.workspace_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        atomic_write_json(&dir.join("state.json"), state)
    }

    pub fn load_workspace_state(
        &self,
        workspace_id: &str,
    ) -> HarnessResult<Option<WorkspaceHarnessState>> {
        let path = self.workspace_dir(workspace_id).join("state.json");
        if !path.exists() {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }

    pub fn save_verification(
        &self,
        workspace_id: &str,
        verification: &VerificationRecord,
    ) -> HarnessResult<()> {
        let dir = self.verifications_dir(workspace_id, &verification.task_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        atomic_write_json(&dir.join(format!("{}.json", verification.id)), verification)
    }

    pub fn list_verifications(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> HarnessResult<Vec<VerificationRecord>> {
        let dir = self.verifications_dir(workspace_id, task_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(dir).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if let Ok(record) = read_json(&path) {
                records.push(record);
            }
        }
        records.sort_by(|left: &VerificationRecord, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(records)
    }

    pub fn save_change_set(&self, workspace_id: &str, change: &ChangeSet) -> HarnessResult<()> {
        let dir = self.changes_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        let digest = format!("{:x}", Sha256::digest(change.id.as_bytes()));
        atomic_write_json(&dir.join(format!("{digest}.json")), change)
    }

    pub fn load_change_set(
        &self,
        workspace_id: &str,
        change_id: &str,
    ) -> HarnessResult<Option<ChangeSet>> {
        let digest = format!("{:x}", Sha256::digest(change_id.as_bytes()));
        let path = self
            .changes_dir(workspace_id)
            .join(format!("{digest}.json"));
        if !path.exists() {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }

    pub fn append_event_for_workspace(
        &self,
        workspace_id: &str,
        event: &HarnessEvent,
    ) -> HarnessResult<()> {
        let dir = self.events_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        let path = dir.join(format!("{}.jsonl", event.task_id));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(io_error)?;
        let line = serde_json::to_string(event)
            .map_err(|e| HarnessError::new("STORE_SERIALIZE_FAILED", e.to_string()))?;
        writeln!(file, "{line}").map_err(io_error)
    }

    pub fn append_operation(
        &self,
        workspace_id: &str,
        operation: &OperationRecord,
    ) -> HarnessResult<()> {
        let dir = self.workspace_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.operations_path(workspace_id))
            .map_err(io_error)?;
        let line = serde_json::to_string(operation)
            .map_err(|e| HarnessError::new("STORE_SERIALIZE_FAILED", e.to_string()))?;
        writeln!(file, "{line}").map_err(io_error)
    }

    pub fn list_operations(
        &self,
        workspace_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<OperationRecord>> {
        let path = self.operations_path(workspace_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(path).map_err(io_error)?;
        let mut operations = Vec::new();
        for line in BufReader::new(file).lines().skip(offset).take(limit.max(1)) {
            let line = line.map_err(io_error)?;
            match serde_json::from_str(&line) {
                Ok(operation) => operations.push(operation),
                Err(_) => break,
            }
        }
        Ok(operations)
    }

    pub fn save_stage_commit_receipt(
        &self,
        workspace_id: &str,
        receipt: &StageCommitReceipt,
    ) -> HarnessResult<()> {
        let path = self.stage_commit_path(workspace_id, &receipt.idempotency_key);
        fs::create_dir_all(path.parent().expect("stage commit parent")).map_err(io_error)?;
        atomic_write_json(&path, receipt)
    }

    pub fn load_stage_commit_receipt(
        &self,
        workspace_id: &str,
        idempotency_key: &str,
    ) -> HarnessResult<Option<StageCommitReceipt>> {
        let path = self.stage_commit_path(workspace_id, idempotency_key);
        if !path.exists() {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }

    pub fn list_events(
        &self,
        workspace_id: &str,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        let path = self
            .events_dir(workspace_id)
            .join(format!("{task_id}.jsonl"));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(path).map_err(io_error)?;
        let mut events = Vec::new();
        for line in BufReader::new(file).lines().skip(offset).take(limit.max(1)) {
            let line = line.map_err(io_error)?;
            match serde_json::from_str(&line) {
                Ok(event) => events.push(event),
                Err(_) => break,
            }
        }
        Ok(events)
    }
}

fn io_error(error: std::io::Error) -> HarnessError {
    HarnessError::new("STORE_IO_FAILED", error.to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> HarnessResult<T> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| HarnessError::new("STORE_CORRUPT", format!("{}: {e}", path.display())))
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> HarnessResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| HarnessError::new("STORE_SERIALIZE_FAILED", e.to_string()))?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(io_error)?;
    fs::rename(&temp, path).map_err(io_error)
}
