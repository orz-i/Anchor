use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::model::{
    BaselineObject, ChangeSet, HarnessEvent, JournalHealth, OperationRecord, StageCommitReceipt,
    TaskSession, VerificationRecord, WorkSessionCloseOutbox, WorkspaceHarnessState,
    WorkspaceIdentity, SCHEMA_VERSION,
};

const WORKSPACE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const WORKSPACE_LOCK_RETRY: Duration = Duration::from_millis(25);
const JOURNAL_SEGMENT_MAX_BYTES: u64 = 4 * 1024 * 1024;
const JOURNAL_RETAINED_SEGMENTS: usize = 8;

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

fn lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(32 | 33))
}

pub type HarnessResult<T> = Result<T, HarnessError>;

#[derive(Debug, Clone)]
pub struct HarnessStore {
    root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreMetadata {
    schema_version: u32,
    created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceMarker {
    schema_version: u32,
    workspace_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalEnvelope<T> {
    schema_version: u32,
    sequence: u64,
    checksum: String,
    record: T,
}

struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub struct WorkspaceTransaction<'a> {
    store: &'a HarnessStore,
    workspace_id: &'a str,
    _lock: FileLock,
}

#[derive(Debug)]
struct JournalRead<T> {
    records: Vec<T>,
    health: JournalHealth,
}

impl HarnessStore {
    pub fn new(root: PathBuf) -> HarnessResult<Self> {
        fs::create_dir_all(&root)
            .map_err(|error| HarnessError::new("STORE_UNAVAILABLE", error.to_string()))?;
        let store = Self { root };
        store.ensure_schema()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_workspace_identity(
        &self,
        workspace_root: &Path,
    ) -> HarnessResult<WorkspaceIdentity> {
        let marker_path = workspace_marker_path(workspace_root);
        let marker_dir = marker_path.parent().ok_or_else(|| {
            HarnessError::new(
                "STORE_IO_FAILED",
                "Workspace marker has no parent directory",
            )
        })?;
        fs::create_dir_all(marker_dir).map_err(io_error)?;
        let _marker_lock = acquire_lock(&marker_path.with_extension("lock"))?;
        let marker = if marker_path.exists() {
            let marker: WorkspaceMarker = read_json(&marker_path)?;
            ensure_schema(marker.schema_version, "workspace marker")?;
            Uuid::parse_str(&marker.workspace_id).map_err(|error| {
                HarnessError::new(
                    "STORE_CORRUPT",
                    format!("{}: invalid workspace UUID: {error}", marker_path.display()),
                )
            })?;
            marker
        } else {
            let marker = WorkspaceMarker {
                schema_version: SCHEMA_VERSION,
                workspace_id: Uuid::new_v4().simple().to_string(),
            };
            atomic_write_json(&marker_path, &marker)?;
            marker
        };

        let canonical_path = canonical_path_string(workspace_root);
        self.with_workspace_transaction(&marker.workspace_id, |transaction| {
            let identity_path = transaction.store.identity_path(transaction.workspace_id);
            let mut identity = if identity_path.exists() {
                let identity: WorkspaceIdentity = read_json(&identity_path)?;
                ensure_schema(identity.schema_version, "workspace identity")?;
                if identity.workspace_id != transaction.workspace_id {
                    return Err(HarnessError::new(
                        "STORE_CORRUPT",
                        "Workspace identity does not match its storage namespace",
                    ));
                }
                identity
            } else {
                WorkspaceIdentity {
                    schema_version: SCHEMA_VERSION,
                    workspace_id: transaction.workspace_id.to_string(),
                    primary_path: canonical_path.clone(),
                    aliases: Vec::new(),
                    created_at: timestamp(),
                    updated_at: timestamp(),
                }
            };
            if identity.primary_path != canonical_path {
                if !identity.aliases.contains(&identity.primary_path) {
                    identity.aliases.push(identity.primary_path.clone());
                }
                identity.primary_path = canonical_path.clone();
            }
            if !identity.aliases.contains(&canonical_path) {
                identity.aliases.push(canonical_path.clone());
            }
            identity.aliases.sort();
            identity.aliases.dedup();
            identity.updated_at = timestamp();
            transaction.save_identity(&identity)?;
            Ok(identity)
        })
    }

    pub fn with_workspace_transaction<T>(
        &self,
        workspace_id: &str,
        operation: impl FnOnce(&WorkspaceTransaction<'_>) -> HarnessResult<T>,
    ) -> HarnessResult<T> {
        let workspace_dir = self.workspace_dir(workspace_id);
        fs::create_dir_all(&workspace_dir).map_err(io_error)?;
        let transaction = WorkspaceTransaction {
            store: self,
            workspace_id,
            _lock: acquire_lock(&workspace_dir.join("workspace.lock"))?,
        };
        operation(&transaction)
    }

    fn ensure_schema(&self) -> HarnessResult<()> {
        let path = self.root.join("store.json");
        if path.exists() {
            let metadata: StoreMetadata = read_json(&path)?;
            return ensure_schema(metadata.schema_version, "Harness store");
        }
        let has_existing_entries = fs::read_dir(&self.root).map_err(io_error)?.next().is_some();
        if has_existing_entries {
            return Err(HarnessError::new(
                "STORE_SCHEMA_INCOMPATIBLE",
                "Harness store has no Schema 5 marker. Remove or archive the old store before starting this build.",
            ));
        }
        atomic_write_json(
            &path,
            &StoreMetadata {
                schema_version: SCHEMA_VERSION,
                created_at: timestamp(),
            },
        )
    }

    fn workspace_dir(&self, workspace_id: &str) -> PathBuf {
        self.root.join("workspaces").join(workspace_id)
    }

    fn identity_path(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("identity.json")
    }

    fn tasks_dir(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("tasks")
    }

    fn baseline_path(&self, workspace_id: &str, object_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id)
            .join("baselines")
            .join(format!("{object_id}.json"))
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

    fn close_outbox_path(&self, workspace_id: &str, task_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id)
            .join("outbox")
            .join("close-work-session")
            .join(format!("{task_id}.json"))
    }

    fn event_journal_dir(&self, workspace_id: &str, task_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id)
            .join("journals")
            .join("events")
            .join(task_id)
    }

    fn operation_journal_dir(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id)
            .join("journals")
            .join("operations")
    }

    pub fn save_task(&self, task: &TaskSession) -> HarnessResult<()> {
        self.with_workspace_transaction(&task.workspace_id, |transaction| {
            transaction.save_task(task)
        })
    }

    pub fn load_task(&self, workspace_id: &str, task_id: &str) -> HarnessResult<TaskSession> {
        let task: TaskSession =
            read_json(&self.tasks_dir(workspace_id).join(format!("{task_id}.json")))?;
        validate_task_schema(&task)?;
        Ok(task)
    }

    pub fn list_tasks(&self, workspace_id: &str) -> HarnessResult<Vec<TaskSession>> {
        let dir = self.tasks_dir(workspace_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut tasks = Vec::new();
        for entry in fs::read_dir(dir).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let task: TaskSession = read_json(&path)?;
            validate_task_schema(&task)?;
            tasks.push(task);
        }
        tasks.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(tasks)
    }

    pub fn save_workspace_state(
        &self,
        workspace_id: &str,
        state: &WorkspaceHarnessState,
    ) -> HarnessResult<()> {
        self.with_workspace_transaction(workspace_id, |transaction| {
            transaction.save_workspace_state(state)
        })
    }

    pub fn load_workspace_state(
        &self,
        workspace_id: &str,
    ) -> HarnessResult<Option<WorkspaceHarnessState>> {
        let path = self.workspace_dir(workspace_id).join("state.json");
        if !path.exists() {
            return Ok(None);
        }
        let state: WorkspaceHarnessState = read_json(&path)?;
        ensure_schema(state.schema_version, "workspace state")?;
        Ok(Some(state))
    }

    pub fn save_baseline_object(
        &self,
        workspace_id: &str,
        baseline: &BaselineObject,
    ) -> HarnessResult<()> {
        self.with_workspace_transaction(workspace_id, |transaction| {
            transaction.save_baseline_object(baseline)
        })
    }

    pub fn load_baseline_object(
        &self,
        workspace_id: &str,
        object_id: &str,
    ) -> HarnessResult<BaselineObject> {
        let baseline: BaselineObject = read_json(&self.baseline_path(workspace_id, object_id))?;
        ensure_schema(baseline.schema_version, "baseline object")?;
        if baseline.id != object_id {
            return Err(HarnessError::new(
                "STORE_CORRUPT",
                "Baseline object ID does not match its content-addressed path",
            ));
        }
        Ok(baseline)
    }

    pub fn save_verification(
        &self,
        workspace_id: &str,
        verification: &VerificationRecord,
    ) -> HarnessResult<()> {
        self.with_workspace_transaction(workspace_id, |transaction| {
            transaction.save_verification(verification)
        })
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
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                records.push(read_json(&path)?);
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
        self.with_workspace_transaction(workspace_id, |transaction| {
            transaction.save_change_set(change)
        })
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

    pub fn list_change_sets(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> HarnessResult<Vec<ChangeSet>> {
        let dir = self.changes_dir(workspace_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut changes = Vec::new();
        for entry in fs::read_dir(dir).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let change: ChangeSet = read_json(&path)?;
            if change.task_id == task_id {
                changes.push(change);
            }
        }
        changes.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(changes)
    }

    pub fn append_event_for_workspace(
        &self,
        workspace_id: &str,
        event: &HarnessEvent,
    ) -> HarnessResult<()> {
        self.with_workspace_transaction(workspace_id, |transaction| transaction.append_event(event))
    }

    pub fn list_events(
        &self,
        workspace_id: &str,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        Ok(
            read_journal::<HarnessEvent>(&self.event_journal_dir(workspace_id, task_id))?
                .records
                .into_iter()
                .skip(offset)
                .take(limit.max(1))
                .collect(),
        )
    }

    pub fn append_operation(
        &self,
        workspace_id: &str,
        operation: &OperationRecord,
    ) -> HarnessResult<()> {
        self.with_workspace_transaction(workspace_id, |transaction| {
            transaction.append_operation(operation)
        })
    }

    pub fn list_operations(
        &self,
        workspace_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<OperationRecord>> {
        Ok(
            read_journal::<OperationRecord>(&self.operation_journal_dir(workspace_id))?
                .records
                .into_iter()
                .skip(offset)
                .take(limit.max(1))
                .collect(),
        )
    }

    pub fn journal_health(&self, workspace_id: &str) -> HarnessResult<JournalHealth> {
        let mut health =
            read_journal::<serde_json::Value>(&self.operation_journal_dir(workspace_id))?.health;
        let event_root = self
            .workspace_dir(workspace_id)
            .join("journals")
            .join("events");
        if event_root.exists() {
            for entry in fs::read_dir(event_root).map_err(io_error)? {
                let path = entry.map_err(io_error)?.path();
                if path.is_dir() {
                    merge_health(
                        &mut health,
                        read_journal::<serde_json::Value>(&path)?.health,
                    );
                }
            }
        }
        Ok(health)
    }

    pub fn save_stage_commit_receipt(
        &self,
        workspace_id: &str,
        receipt: &StageCommitReceipt,
    ) -> HarnessResult<()> {
        self.with_workspace_transaction(workspace_id, |transaction| {
            transaction.save_stage_commit_receipt(receipt)
        })
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

    pub fn save_close_outbox(
        &self,
        workspace_id: &str,
        outbox: &WorkSessionCloseOutbox,
    ) -> HarnessResult<()> {
        self.with_workspace_transaction(workspace_id, |transaction| {
            transaction.save_close_outbox(outbox)
        })
    }

    pub fn load_close_outbox(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> HarnessResult<Option<WorkSessionCloseOutbox>> {
        let path = self.close_outbox_path(workspace_id, task_id);
        if !path.exists() {
            return Ok(None);
        }
        let outbox: WorkSessionCloseOutbox = read_json(&path)?;
        ensure_schema(outbox.schema_version, "work session close outbox")?;
        Ok(Some(outbox))
    }

    pub fn list_close_outboxes(
        &self,
        workspace_id: &str,
    ) -> HarnessResult<Vec<WorkSessionCloseOutbox>> {
        let dir = self
            .workspace_dir(workspace_id)
            .join("outbox")
            .join("close-work-session");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut outboxes = Vec::new();
        for entry in fs::read_dir(dir).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                let outbox: WorkSessionCloseOutbox = read_json(&path)?;
                ensure_schema(outbox.schema_version, "work session close outbox")?;
                outboxes.push(outbox);
            }
        }
        outboxes.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(outboxes)
    }

    pub fn delete_close_outbox(&self, workspace_id: &str, task_id: &str) -> HarnessResult<()> {
        self.with_workspace_transaction(workspace_id, |transaction| {
            transaction.delete_close_outbox(task_id)
        })
    }
}

impl WorkspaceTransaction<'_> {
    pub fn save_identity(&self, identity: &WorkspaceIdentity) -> HarnessResult<()> {
        ensure_schema(identity.schema_version, "workspace identity")?;
        atomic_write_json(&self.store.identity_path(self.workspace_id), identity)
    }

    pub fn save_task(&self, task: &TaskSession) -> HarnessResult<()> {
        validate_task_schema(task)?;
        let dir = self.store.tasks_dir(self.workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        atomic_write_json(&dir.join(format!("{}.json", task.id)), task)
    }

    pub fn save_workspace_state(&self, state: &WorkspaceHarnessState) -> HarnessResult<()> {
        ensure_schema(state.schema_version, "workspace state")?;
        atomic_write_json(
            &self
                .store
                .workspace_dir(self.workspace_id)
                .join("state.json"),
            state,
        )
    }

    pub fn save_baseline_object(&self, baseline: &BaselineObject) -> HarnessResult<()> {
        ensure_schema(baseline.schema_version, "baseline object")?;
        let expected_id = baseline_object_id(&baseline.entries)?;
        if baseline.id != expected_id {
            return Err(HarnessError::new(
                "STORE_SERIALIZE_FAILED",
                "Baseline object ID does not match its entries",
            ));
        }
        let path = self.store.baseline_path(self.workspace_id, &baseline.id);
        if path.exists() {
            let existing: BaselineObject = read_json(&path)?;
            if existing.id != baseline.id || existing.entries != baseline.entries {
                return Err(HarnessError::new(
                    "STORE_CORRUPT",
                    "Content-addressed baseline collision detected",
                ));
            }
            return Ok(());
        }
        atomic_write_json(&path, baseline)
    }

    pub fn save_verification(&self, verification: &VerificationRecord) -> HarnessResult<()> {
        let dir = self
            .store
            .verifications_dir(self.workspace_id, &verification.task_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        atomic_write_json(&dir.join(format!("{}.json", verification.id)), verification)
    }

    pub fn save_change_set(&self, change: &ChangeSet) -> HarnessResult<()> {
        let dir = self.store.changes_dir(self.workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        let digest = format!("{:x}", Sha256::digest(change.id.as_bytes()));
        atomic_write_json(&dir.join(format!("{digest}.json")), change)
    }

    pub fn append_event(&self, event: &HarnessEvent) -> HarnessResult<()> {
        append_journal(
            &self
                .store
                .event_journal_dir(self.workspace_id, &event.task_id),
            event,
            JOURNAL_SEGMENT_MAX_BYTES,
            JOURNAL_RETAINED_SEGMENTS,
        )
    }

    pub fn append_operation(&self, operation: &OperationRecord) -> HarnessResult<()> {
        append_journal(
            &self.store.operation_journal_dir(self.workspace_id),
            operation,
            JOURNAL_SEGMENT_MAX_BYTES,
            JOURNAL_RETAINED_SEGMENTS,
        )
    }

    pub fn save_stage_commit_receipt(&self, receipt: &StageCommitReceipt) -> HarnessResult<()> {
        let path = self
            .store
            .stage_commit_path(self.workspace_id, &receipt.idempotency_key);
        atomic_write_json(&path, receipt)
    }

    pub fn save_close_outbox(&self, outbox: &WorkSessionCloseOutbox) -> HarnessResult<()> {
        ensure_schema(outbox.schema_version, "work session close outbox")?;
        atomic_write_json(
            &self
                .store
                .close_outbox_path(self.workspace_id, &outbox.task_id),
            outbox,
        )
    }

    pub fn delete_close_outbox(&self, task_id: &str) -> HarnessResult<()> {
        let path = self.store.close_outbox_path(self.workspace_id, task_id);
        if path.exists() {
            fs::remove_file(path).map_err(io_error)?;
        }
        Ok(())
    }
}

pub fn baseline_object_id(entries: &[super::model::BaselineEntry]) -> HarnessResult<String> {
    let bytes = serde_json::to_vec(entries)
        .map_err(|error| HarnessError::new("STORE_SERIALIZE_FAILED", error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(b"anchor-baseline-v5\0");
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn append_journal<T: Serialize>(
    dir: &Path,
    record: &T,
    segment_max_bytes: u64,
    retained_segments: usize,
) -> HarnessResult<()> {
    fs::create_dir_all(dir).map_err(io_error)?;
    let mut segments = journal_segments(dir)?;
    let mut segment_number = segments.last().map(|entry| entry.0).unwrap_or(1);
    let mut path = segment_path(dir, segment_number);
    if path.exists() && fs::metadata(&path).map_err(io_error)?.len() >= segment_max_bytes {
        segment_number += 1;
        path = segment_path(dir, segment_number);
        segments.push((segment_number, path.clone()));
        while segments.len() > retained_segments.max(1) {
            let (_, stale) = segments.remove(0);
            fs::remove_file(stale).map_err(io_error)?;
        }
    }
    let sequence = last_valid_sequence(dir)?.saturating_add(1);
    let record_value = serde_json::to_value(record)
        .map_err(|error| HarnessError::new("STORE_SERIALIZE_FAILED", error.to_string()))?;
    let checksum = journal_checksum(sequence, &record_value)?;
    let envelope = JournalEnvelope {
        schema_version: SCHEMA_VERSION,
        sequence,
        checksum,
        record: record_value,
    };
    let line = serde_json::to_vec(&envelope)
        .map_err(|error| HarnessError::new("STORE_SERIALIZE_FAILED", error.to_string()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(&line).map_err(io_error)?;
    file.write_all(b"\n").map_err(io_error)?;
    file.sync_data().map_err(io_error)
}

fn read_journal<T: DeserializeOwned>(dir: &Path) -> HarnessResult<JournalRead<T>> {
    let segments = journal_segments(dir)?;
    let mut health = JournalHealth {
        segment_count: segments.len(),
        rotations: segments.len().saturating_sub(1),
        ..JournalHealth::default()
    };
    let mut records = Vec::new();
    let mut previous_sequence = None;
    for (_, path) in segments {
        health.retained_bytes = health
            .retained_bytes
            .saturating_add(fs::metadata(&path).map_err(io_error)?.len());
        let file = File::open(path).map_err(io_error)?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(io_error)?;
            let envelope: JournalEnvelope<serde_json::Value> = match serde_json::from_str(&line) {
                Ok(envelope) => envelope,
                Err(_) => {
                    health.corrupt_lines += 1;
                    continue;
                }
            };
            if envelope.schema_version != SCHEMA_VERSION {
                health.schema_mismatches += 1;
                continue;
            }
            if journal_checksum(envelope.sequence, &envelope.record)? != envelope.checksum {
                health.checksum_failures += 1;
                continue;
            }
            if let Some(previous) = previous_sequence {
                if envelope.sequence <= previous {
                    health.sequence_anomalies += 1;
                    continue;
                }
                if envelope.sequence != previous + 1 {
                    health.sequence_anomalies += 1;
                }
            }
            previous_sequence = Some(envelope.sequence);
            match serde_json::from_value(envelope.record) {
                Ok(record) => {
                    records.push(record);
                    health.valid_records += 1;
                }
                Err(_) => health.corrupt_lines += 1,
            }
        }
    }
    Ok(JournalRead { records, health })
}

fn last_valid_sequence(dir: &Path) -> HarnessResult<u64> {
    let mut last = 0;
    for (_, path) in journal_segments(dir)? {
        let file = File::open(path).map_err(io_error)?;
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                continue;
            };
            let Ok(envelope) = serde_json::from_str::<JournalEnvelope<serde_json::Value>>(&line)
            else {
                continue;
            };
            if envelope.schema_version == SCHEMA_VERSION
                && journal_checksum(envelope.sequence, &envelope.record)? == envelope.checksum
            {
                last = last.max(envelope.sequence);
            }
        }
    }
    Ok(last)
}

fn journal_checksum(sequence: u64, record: &serde_json::Value) -> HarnessResult<String> {
    let bytes = serde_json::to_vec(record)
        .map_err(|error| HarnessError::new("STORE_SERIALIZE_FAILED", error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(b"anchor-journal-v5\0");
    hasher.update(SCHEMA_VERSION.to_le_bytes());
    hasher.update(sequence.to_le_bytes());
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn journal_segments(dir: &Path) -> HarnessResult<Vec<(u64, PathBuf)>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(number) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        segments.push((number, path));
    }
    segments.sort_by_key(|entry| entry.0);
    Ok(segments)
}

fn segment_path(dir: &Path, number: u64) -> PathBuf {
    dir.join(format!("{number:08}.jsonl"))
}

fn merge_health(target: &mut JournalHealth, source: JournalHealth) {
    target.segment_count += source.segment_count;
    target.retained_bytes = target.retained_bytes.saturating_add(source.retained_bytes);
    target.valid_records += source.valid_records;
    target.corrupt_lines += source.corrupt_lines;
    target.checksum_failures += source.checksum_failures;
    target.schema_mismatches += source.schema_mismatches;
    target.sequence_anomalies += source.sequence_anomalies;
    target.rotations += source.rotations;
}

fn acquire_lock(path: &Path) -> HarnessResult<FileLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(io_error)?;
    let started = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(FileLock { file }),
            Err(error) if lock_is_contended(&error) => {
                if started.elapsed() >= WORKSPACE_LOCK_TIMEOUT {
                    return Err(HarnessError::new(
                        "STORE_LOCK_TIMEOUT",
                        format!(
                            "Harness workspace lock was unavailable after {} ms",
                            WORKSPACE_LOCK_TIMEOUT.as_millis()
                        ),
                    ));
                }
                std::thread::sleep(WORKSPACE_LOCK_RETRY);
            }
            Err(error) => return Err(io_error(error)),
        }
    }
}

fn validate_task_schema(task: &TaskSession) -> HarnessResult<()> {
    ensure_schema(task.schema_version, "task")?;
    ensure_schema(task.baseline.schema_version, "task baseline")
}

fn ensure_schema(actual: u32, subject: &str) -> HarnessResult<()> {
    if actual == SCHEMA_VERSION {
        return Ok(());
    }
    Err(HarnessError::new(
        "STORE_SCHEMA_INCOMPATIBLE",
        format!(
            "{subject} uses Schema {actual}; this build requires Schema {SCHEMA_VERSION}. No compatibility bridge is provided."
        ),
    ))
}

fn canonical_path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn workspace_marker_path(workspace_root: &Path) -> PathBuf {
    let mut command = std::process::Command::new("git");
    crate::platform::hide_std_console(&mut command);
    if let Ok(output) = command
        .arg("-C")
        .arg(workspace_root)
        .args(["rev-parse", "--git-path", "anchor-workspace-id.json"])
        .output()
    {
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !raw.is_empty() {
                let path = PathBuf::from(raw);
                return if path.is_absolute() {
                    path
                } else {
                    workspace_root.join(path)
                };
            }
        }
    }
    workspace_root.join(".anchor").join("workspace-id.json")
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

fn io_error(error: std::io::Error) -> HarnessError {
    HarnessError::new("STORE_IO_FAILED", error.to_string())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> HarnessResult<T> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| HarnessError::new("STORE_CORRUPT", format!("{}: {error}", path.display())))
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> HarnessResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| HarnessError::new("STORE_SERIALIZE_FAILED", error.to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| HarnessError::new("STORE_IO_FAILED", "Store path has no parent"))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let temp = parent.join(format!(".harness-tmp-{}", Uuid::new_v4().simple()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        atomic_replace(&temp, path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(io_error)
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::harness::model::{
        ProjectBaseline, TaskContract, TaskPhase, TaskStatus, TaskWorkingSet,
    };

    #[test]
    fn journal_skips_corruption_and_reports_health() {
        let root = tempdir().expect("root");
        let journal = root.path().join("journal");
        append_journal(&journal, &json!({"value": 1}), 1024, 4).expect("append");
        let path = segment_path(&journal, 1);
        let mut file = OpenOptions::new().append(true).open(path).expect("open");
        writeln!(file, "not-json").expect("corrupt");
        append_journal(&journal, &json!({"value": 2}), 1024, 4).expect("append");

        let read = read_journal::<serde_json::Value>(&journal).expect("read");
        assert_eq!(read.records.len(), 2);
        assert_eq!(read.health.corrupt_lines, 1);
    }

    #[test]
    fn journal_rejects_tampered_checksum_but_keeps_later_records() {
        let root = tempdir().expect("root");
        let journal = root.path().join("journal");
        append_journal(&journal, &json!({"value": 1}), 1024, 4).expect("append");
        append_journal(&journal, &json!({"value": 2}), 1024, 4).expect("append");
        let path = segment_path(&journal, 1);
        let content = fs::read_to_string(&path).expect("content");
        let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
        let mut first: serde_json::Value = serde_json::from_str(&lines[0]).expect("first envelope");
        first["record"]["value"] = json!(999);
        lines[0] = serde_json::to_string(&first).expect("tampered envelope");
        fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write tampered");

        let read = read_journal::<serde_json::Value>(&journal).expect("read");
        assert_eq!(read.records, vec![json!({"value": 2})]);
        assert_eq!(read.health.checksum_failures, 1);
        assert_eq!(read.health.valid_records, 1);
    }

    #[test]
    fn journal_rotates_and_prunes_old_segments() {
        let root = tempdir().expect("root");
        let journal = root.path().join("journal");
        for value in 0..12 {
            append_journal(&journal, &json!({"value": value}), 1, 3).expect("append");
        }
        let segments = journal_segments(&journal).expect("segments");
        assert_eq!(segments.len(), 3);
        let read = read_journal::<serde_json::Value>(&journal).expect("read");
        assert_eq!(read.health.segment_count, 3);
        assert!(read.health.valid_records <= 3);
    }

    #[test]
    fn store_rejects_unmarked_legacy_directory() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("legacy.json"), b"{}").expect("legacy");
        let error = HarnessStore::new(root.path().to_path_buf()).expect_err("reject");
        assert_eq!(error.code(), "STORE_SCHEMA_INCOMPATIBLE");
    }

    #[test]
    fn schema5_store_rejects_a_schema4_task_file() {
        let root = tempdir().expect("root");
        let store = HarnessStore::new(root.path().to_path_buf()).expect("store");
        let task = TaskSession {
            schema_version: 4,
            id: "legacy-task".into(),
            workspace_id: "workspace".into(),
            objective: "legacy task".into(),
            status: TaskStatus::Active,
            phase: TaskPhase::Unspecified,
            contract: TaskContract::default(),
            slices: Vec::new(),
            current_slice_id: None,
            working_set: TaskWorkingSet::default(),
            recovery: None,
            baseline: ProjectBaseline {
                schema_version: SCHEMA_VERSION,
                branch: None,
                head: None,
                worktree_fingerprint: "0".repeat(64),
                object_id: "1".repeat(64),
                file_count: 0,
                captured_at: "0".into(),
            },
            expected_state: super::super::model::ExpectedWorkspaceState {
                branch: None,
                head: None,
                worktree_fingerprint: "0".repeat(64),
                accepted_at: "0".into(),
                accepted_by_operation_id: None,
            },
            completed_steps: Vec::new(),
            pending_steps: Vec::new(),
            latest_change_id: None,
            latest_verification_id: None,
            history_session_key: None,
            history_session_path: None,
            git_worktree: None,
            created_at: "0".into(),
            updated_at: "0".into(),
            last_activity_at: None,
        };
        let task_dir = store.tasks_dir("workspace");
        fs::create_dir_all(&task_dir).expect("task dir");
        fs::write(
            task_dir.join("legacy-task.json"),
            serde_json::to_vec_pretty(&task).expect("serialize"),
        )
        .expect("write task");

        let error = store.list_tasks("workspace").expect_err("reject task");
        assert_eq!(error.code(), "STORE_SCHEMA_INCOMPATIBLE");
    }
}
