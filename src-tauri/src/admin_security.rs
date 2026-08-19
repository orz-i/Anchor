use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::platform::platform;

const ADMIN_AUDIT_MAX_BYTES: u64 = 512 * 1024;
const ADMIN_AUDIT_MAX_LINE_BYTES: usize = 8 * 1024;
const ADMIN_AUDIT_MAX_READ_EVENTS: usize = 200;
const PRIVILEGED_CONFIRMATION_TTL: Duration = Duration::from_secs(5 * 60);
const PRIVILEGED_GRANT_TTL: Duration = Duration::from_secs(2 * 60);
const MAX_PRIVILEGED_CONFIRMATIONS: usize = 64;

const PRIVILEGED_ACTIONS: &[&str] = &[
    "set_workspace_secret",
    "regenerate_workspace_secret",
    "set_shared_secret",
    "regenerate_shared_secret",
    "save_frp_profile",
    "install_software",
    "uninstall_software",
    "install_windows_service",
    "uninstall_windows_service",
    "start_windows_service",
    "stop_windows_service",
    "restart_windows_service",
    "sync_windows_service_plan",
];

pub(crate) fn privileged_actions() -> &'static [&'static str] {
    PRIVILEGED_ACTIONS
}

static AUDIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditEvent {
    pub timestamp_unix_ms: u64,
    pub session_fingerprint: String,
    pub action: String,
    pub phase: String,
    pub outcome: String,
}

pub fn session_fingerprint(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..12])
}

fn validate_privileged_action(action: &str) -> AppResult<()> {
    if PRIVILEGED_ACTIONS.contains(&action) {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "Web Admin privileged action is not allowlisted: {action}"
        )))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedPrivilegedAction {
    pub confirmation_id: String,
    pub action: String,
    pub confirmation_text: String,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovedPrivilegedGrant {
    pub grant_id: String,
    pub action: String,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone)]
struct PrivilegedConfirmation {
    session_id: String,
    action: String,
    confirmation_text: String,
    created_at: Instant,
    approved_at: Option<Instant>,
    consumed: bool,
}

#[derive(Default)]
pub struct PrivilegedConfirmationStore {
    records: HashMap<String, PrivilegedConfirmation>,
    audit_root: Option<PathBuf>,
}

impl PrivilegedConfirmationStore {
    #[cfg(test)]
    fn for_test(audit_root: PathBuf) -> Self {
        Self {
            records: HashMap::new(),
            audit_root: Some(audit_root),
        }
    }

    fn append_audit(&self, event: &AdminAuditEvent) -> AppResult<()> {
        append_confirmation_audit(self.audit_root.as_deref(), event)
    }

    fn purge_expired(&mut self, now: Instant) {
        self.records.retain(|_, record| {
            if record.consumed {
                return false;
            }
            match record.approved_at {
                Some(approved_at) => now.duration_since(approved_at) <= PRIVILEGED_GRANT_TTL,
                None => now.duration_since(record.created_at) <= PRIVILEGED_CONFIRMATION_TTL,
            }
        });
    }

    fn make_room(&mut self) {
        if self.records.len() < MAX_PRIVILEGED_CONFIRMATIONS {
            return;
        }
        if let Some(oldest) = self
            .records
            .iter()
            .min_by_key(|(_, record)| record.created_at)
            .map(|(id, _)| id.clone())
        {
            self.records.remove(&oldest);
        }
    }

    pub fn prepare(
        &mut self,
        session_id: &str,
        action: &str,
    ) -> AppResult<PreparedPrivilegedAction> {
        validate_privileged_action(action)?;
        let now = Instant::now();
        self.purge_expired(now);
        self.make_room();
        let confirmation_id = uuid::Uuid::new_v4().to_string();
        let confirmation_text = format!("CONFIRM {action}");
        self.append_audit(&AdminAuditEvent::new(
            session_fingerprint(session_id),
            action,
            "confirmation_prepared",
            "pending",
        ))?;
        self.records.insert(
            confirmation_id.clone(),
            PrivilegedConfirmation {
                session_id: session_id.to_string(),
                action: action.to_string(),
                confirmation_text: confirmation_text.clone(),
                created_at: now,
                approved_at: None,
                consumed: false,
            },
        );
        Ok(PreparedPrivilegedAction {
            confirmation_id,
            action: action.to_string(),
            confirmation_text,
            expires_in_seconds: PRIVILEGED_CONFIRMATION_TTL.as_secs(),
        })
    }

    pub fn confirm(
        &mut self,
        session_id: &str,
        confirmation_id: &str,
        confirmation_text: &str,
    ) -> AppResult<ApprovedPrivilegedGrant> {
        let now = Instant::now();
        self.purge_expired(now);
        let audit_root = self.audit_root.clone();
        let record = self.records.get_mut(confirmation_id).ok_or_else(|| {
            AppError::Message("Web Admin privileged confirmation is missing or expired".into())
        })?;
        if record.session_id != session_id {
            return Err(AppError::Message(
                "Web Admin privileged confirmation belongs to another session".into(),
            ));
        }
        if record.approved_at.is_some() {
            return Err(AppError::Message(
                "Web Admin privileged confirmation has already been approved".into(),
            ));
        }
        if record.confirmation_text != confirmation_text {
            append_confirmation_audit(
                audit_root.as_deref(),
                &AdminAuditEvent::new(
                    session_fingerprint(session_id),
                    &record.action,
                    "confirmation_rejected",
                    "rejected",
                ),
            )?;
            return Err(AppError::Message(
                "Web Admin privileged confirmation text does not match".into(),
            ));
        }
        append_confirmation_audit(
            audit_root.as_deref(),
            &AdminAuditEvent::new(
                session_fingerprint(session_id),
                &record.action,
                "confirmation_approved",
                "succeeded",
            ),
        )?;
        record.approved_at = Some(now);
        Ok(ApprovedPrivilegedGrant {
            grant_id: confirmation_id.to_string(),
            action: record.action.clone(),
            expires_in_seconds: PRIVILEGED_GRANT_TTL.as_secs(),
        })
    }

    /// Reserved for the future privileged command dispatcher. No current Web
    /// Admin command consumes a grant, so creating/approving a ticket cannot
    /// itself unlock secret/software/service mutations.
    #[allow(dead_code)]
    pub fn consume_grant(
        &mut self,
        session_id: &str,
        grant_id: &str,
        action: &str,
    ) -> AppResult<()> {
        validate_privileged_action(action)?;
        let now = Instant::now();
        self.purge_expired(now);
        let audit_root = self.audit_root.clone();
        let record = self.records.get_mut(grant_id).ok_or_else(|| {
            AppError::Message("Web Admin privileged grant is missing or expired".into())
        })?;
        if record.session_id != session_id || record.action != action {
            return Err(AppError::Message(
                "Web Admin privileged grant does not match this session/action".into(),
            ));
        }
        let approved_at = record.approved_at.ok_or_else(|| {
            AppError::Message("Web Admin privileged grant has not been approved".into())
        })?;
        if now.duration_since(approved_at) > PRIVILEGED_GRANT_TTL || record.consumed {
            return Err(AppError::Message(
                "Web Admin privileged grant is expired or already consumed".into(),
            ));
        }
        append_confirmation_audit(
            audit_root.as_deref(),
            &AdminAuditEvent::new(
                session_fingerprint(session_id),
                action,
                "grant_consumed",
                "succeeded",
            ),
        )?;
        record.consumed = true;
        Ok(())
    }
}

fn append_confirmation_audit(audit_root: Option<&Path>, event: &AdminAuditEvent) -> AppResult<()> {
    match audit_root {
        Some(root) => append_event_at(root, event),
        None => append_admin_audit_event(event),
    }
}

impl AdminAuditEvent {
    pub fn new(
        session_fingerprint: impl Into<String>,
        action: impl Into<String>,
        phase: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        Self {
            timestamp_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            session_fingerprint: session_fingerprint.into(),
            action: action.into(),
            phase: phase.into(),
            outcome: outcome.into(),
        }
    }
}

fn audit_paths(root: &Path) -> (PathBuf, PathBuf) {
    (
        root.join("admin-audit.jsonl"),
        root.join("admin-audit.1.jsonl"),
    )
}

fn ensure_private_dir(path: &Path) -> AppResult<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn ensure_private_file(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn rotate_if_needed(path: &Path, rotated: &Path) -> AppResult<()> {
    let len = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if len <= ADMIN_AUDIT_MAX_BYTES {
        return Ok(());
    }
    if rotated.exists() {
        fs::remove_file(rotated)?;
    }
    fs::rename(path, rotated)?;
    ensure_private_file(rotated)
}

fn append_event_at(root: &Path, event: &AdminAuditEvent) -> AppResult<()> {
    ensure_private_dir(root)?;
    let (path, rotated) = audit_paths(root);
    rotate_if_needed(&path, &rotated)?;
    let mut bytes = serde_json::to_vec(event)?;
    if bytes.len() > ADMIN_AUDIT_MAX_LINE_BYTES {
        return Err(AppError::Message(
            "Web Admin audit event exceeds the bounded line size".into(),
        ));
    }
    bytes.push(b'\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    ensure_private_file(&path)?;
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}

fn read_events_from(path: &Path, output: &mut Vec<AdminAuditEvent>) -> AppResult<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > ADMIN_AUDIT_MAX_BYTES + ADMIN_AUDIT_MAX_LINE_BYTES as u64 {
        return Err(AppError::Message(format!(
            "Web Admin audit file exceeds safety bound: {} bytes",
            metadata.len()
        )));
    }
    let bytes = fs::read(path)?;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if line.len() > ADMIN_AUDIT_MAX_LINE_BYTES {
            return Err(AppError::Message(
                "Web Admin audit line exceeds safety bound".into(),
            ));
        }
        output.push(serde_json::from_slice(line).map_err(|error| {
            AppError::Message(format!("Web Admin audit log is corrupted: {error}"))
        })?);
    }
    Ok(())
}

pub fn append_admin_audit_event(event: &AdminAuditEvent) -> AppResult<()> {
    let _guard = AUDIT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = platform().app_config_dir()?.join("admin");
    append_event_at(&root, event)
}

pub fn read_admin_audit_events(limit: usize) -> AppResult<Vec<AdminAuditEvent>> {
    let _guard = AUDIT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = platform().app_config_dir()?.join("admin");
    let (path, rotated) = audit_paths(&root);
    let mut events = Vec::new();
    read_events_from(&rotated, &mut events)?;
    read_events_from(&path, &mut events)?;
    let limit = limit.clamp(1, ADMIN_AUDIT_MAX_READ_EVENTS);
    if events.len() > limit {
        events.drain(..events.len() - limit);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_round_trip_is_bounded_and_contains_no_argument_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let event = AdminAuditEvent::new(
            "session-fingerprint",
            "set_workspace_secret",
            "confirmation_prepared",
            "pending",
        );
        append_event_at(temp.path(), &event).expect("append audit");
        let (path, _) = audit_paths(temp.path());
        let raw = fs::read_to_string(path).expect("audit file");
        assert!(!raw.contains("secret-value"));
        assert!(!raw.contains("\"args\""));
        assert!(!raw.contains("\"payload\""));

        let mut events = Vec::new();
        read_events_from(&audit_paths(temp.path()).0, &mut events).expect("read audit");
        assert_eq!(events, vec![event]);
    }

    #[test]
    fn confirmation_is_session_bound_exact_and_one_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut store = PrivilegedConfirmationStore::for_test(temp.path().to_path_buf());
        let prepared = store
            .prepare("session-a", "set_workspace_secret")
            .expect("prepare");
        assert!(store
            .confirm(
                "session-b",
                &prepared.confirmation_id,
                &prepared.confirmation_text,
            )
            .is_err());
        assert!(store
            .confirm(
                "session-a",
                &prepared.confirmation_id,
                "CONFIRM something_else"
            )
            .is_err());
        let grant = store
            .confirm(
                "session-a",
                &prepared.confirmation_id,
                &prepared.confirmation_text,
            )
            .expect("confirm");
        store
            .consume_grant("session-a", &grant.grant_id, "set_workspace_secret")
            .expect("consume once");
        assert!(store
            .consume_grant("session-a", &grant.grant_id, "set_workspace_secret")
            .is_err());
    }
}
