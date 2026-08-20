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
    "set_frp_profile_token",
    "delete_frp_profile",
    "install_software",
    "uninstall_software",
    "install_windows_service",
    "uninstall_windows_service",
    "start_windows_service",
    "stop_windows_service",
    "restart_windows_service",
    "sync_windows_service_plan",
];

const AVAILABLE_PRIVILEGED_EXECUTORS: &[&str] = &[
    "set_workspace_secret",
    "regenerate_workspace_secret",
    "set_shared_secret",
    "regenerate_shared_secret",
    "set_frp_profile_token",
    "delete_frp_profile",
    "install_software",
    "uninstall_software",
    #[cfg(windows)]
    "install_windows_service",
    #[cfg(windows)]
    "uninstall_windows_service",
    #[cfg(windows)]
    "start_windows_service",
    #[cfg(windows)]
    "stop_windows_service",
    #[cfg(windows)]
    "restart_windows_service",
    #[cfg(windows)]
    "sync_windows_service_plan",
];

const UNAVAILABLE_PRIVILEGED_ACTIONS: &[&str] = &[
    "save_frp_profile",
    #[cfg(not(windows))]
    "install_windows_service",
    #[cfg(not(windows))]
    "uninstall_windows_service",
    #[cfg(not(windows))]
    "start_windows_service",
    #[cfg(not(windows))]
    "stop_windows_service",
    #[cfg(not(windows))]
    "restart_windows_service",
    #[cfg(not(windows))]
    "sync_windows_service_plan",
];

pub(crate) fn privileged_actions() -> &'static [&'static str] {
    PRIVILEGED_ACTIONS
}

pub(crate) fn available_privileged_executors() -> &'static [&'static str] {
    AVAILABLE_PRIVILEGED_EXECUTORS
}

pub(crate) fn unavailable_privileged_actions() -> &'static [&'static str] {
    UNAVAILABLE_PRIVILEGED_ACTIONS
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
    pub target_summary: String,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivilegedActionBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl PrivilegedActionBinding {
    pub fn workspace_secret(id: &str, key: &str) -> Self {
        Self {
            id: Some(id.to_string()),
            key: Some(key.to_string()),
            kind: None,
            version: None,
        }
    }

    pub fn shared_secret(key: &str) -> Self {
        Self {
            id: None,
            key: Some(key.to_string()),
            kind: None,
            version: None,
        }
    }

    pub fn frp_profile(id: &str) -> Self {
        Self {
            id: Some(id.to_string()),
            key: None,
            kind: None,
            version: None,
        }
    }

    pub fn software_install(kind: &str, version: &str) -> Self {
        Self {
            id: None,
            key: None,
            kind: Some(kind.to_string()),
            version: Some(version.to_string()),
        }
    }

    pub fn software_uninstall(kind: &str) -> Self {
        Self {
            id: None,
            key: None,
            kind: Some(kind.to_string()),
            version: None,
        }
    }

    pub fn windows_service(service_name: &str, revision: &str) -> Self {
        Self {
            id: Some(service_name.to_string()),
            key: None,
            kind: None,
            version: Some(revision.to_string()),
        }
    }
}

fn normalize_selector(value: &str, field: &str, max_len: usize) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > max_len || value.chars().any(char::is_control) {
        return Err(AppError::Message(format!(
            "Web Admin privileged binding has invalid {field}"
        )));
    }
    Ok(value.to_string())
}

fn normalize_binding(
    action: &str,
    binding: &PrivilegedActionBinding,
) -> AppResult<PrivilegedActionBinding> {
    validate_privileged_action(action)?;
    let id = binding
        .id
        .as_deref()
        .map(|value| normalize_selector(value, "id", 256))
        .transpose()?;
    let key = binding
        .key
        .as_deref()
        .map(|value| normalize_selector(value, "key", 128))
        .transpose()?;
    let kind = binding
        .kind
        .as_deref()
        .map(|value| normalize_selector(value, "kind", 64))
        .transpose()?;
    let version = binding
        .version
        .as_deref()
        .map(|value| normalize_selector(value, "version", 64))
        .transpose()?;
    let normalized = PrivilegedActionBinding {
        id,
        key,
        kind,
        version,
    };

    let valid_shape = match action {
        "set_workspace_secret" | "regenerate_workspace_secret" => {
            normalized.id.is_some()
                && normalized.key.is_some()
                && normalized.kind.is_none()
                && normalized.version.is_none()
        }
        "set_shared_secret" | "regenerate_shared_secret" => {
            normalized.id.is_none()
                && normalized.key.is_some()
                && normalized.kind.is_none()
                && normalized.version.is_none()
        }
        "save_frp_profile" | "set_frp_profile_token" | "delete_frp_profile" => {
            normalized.id.is_some()
                && normalized.key.is_none()
                && normalized.kind.is_none()
                && normalized.version.is_none()
        }
        "install_software" => {
            normalized.id.is_none()
                && normalized.key.is_none()
                && normalized.kind.is_some()
                && normalized.version.is_some()
        }
        "uninstall_software" => {
            normalized.id.is_none()
                && normalized.key.is_none()
                && normalized.kind.is_some()
                && normalized.version.is_none()
        }
        "install_windows_service"
        | "uninstall_windows_service"
        | "start_windows_service"
        | "stop_windows_service"
        | "restart_windows_service"
        | "sync_windows_service_plan" => {
            normalized.id.is_some()
                && normalized.key.is_none()
                && normalized.kind.is_none()
                && normalized.version.is_some()
        }
        _ => false,
    };
    if !valid_shape {
        return Err(AppError::Message(format!(
            "Web Admin privileged binding does not match action: {action}"
        )));
    }
    Ok(normalized)
}

fn binding_fingerprint(action: &str, binding: &PrivilegedActionBinding) -> AppResult<String> {
    let normalized = normalize_binding(action, binding)?;
    let bytes = serde_json::to_vec(&(action, normalized))?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}

fn binding_target_summary(action: &str, binding: &PrivilegedActionBinding) -> AppResult<String> {
    let binding = normalize_binding(action, binding)?;
    Ok(match action {
        "set_workspace_secret" | "regenerate_workspace_secret" => format!(
            "Workspace {} · {}",
            binding.id.as_deref().unwrap_or_default(),
            binding.key.as_deref().unwrap_or_default()
        ),
        "set_shared_secret" | "regenerate_shared_secret" => {
            format!("共享密钥 {}", binding.key.as_deref().unwrap_or_default())
        }
        "save_frp_profile" | "set_frp_profile_token" | "delete_frp_profile" => {
            format!("FRP profile {}", binding.id.as_deref().unwrap_or_default())
        }
        "install_software" => format!(
            "软件 {} · 版本 {}",
            binding.kind.as_deref().unwrap_or_default(),
            binding.version.as_deref().unwrap_or_default()
        ),
        "uninstall_software" => {
            format!("软件 {}", binding.kind.as_deref().unwrap_or_default())
        }
        "install_windows_service"
        | "uninstall_windows_service"
        | "start_windows_service"
        | "stop_windows_service"
        | "restart_windows_service"
        | "sync_windows_service_plan" => format!(
            "Windows Service {}",
            binding.id.as_deref().unwrap_or_default()
        ),
        _ => unreachable!("validated privileged action must have a target summary"),
    })
}

#[derive(Debug, Clone)]
struct PrivilegedConfirmation {
    session_id: String,
    action: String,
    binding_fingerprint: String,
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
        binding: &PrivilegedActionBinding,
    ) -> AppResult<PreparedPrivilegedAction> {
        validate_privileged_action(action)?;
        let binding_fingerprint = binding_fingerprint(action, binding)?;
        let target_summary = binding_target_summary(action, binding)?;
        let now = Instant::now();
        self.purge_expired(now);
        self.make_room();
        let confirmation_id = uuid::Uuid::new_v4().to_string();
        let binding_tag = binding_fingerprint.chars().take(12).collect::<String>();
        let confirmation_text = format!("CONFIRM {action} {binding_tag}");
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
                binding_fingerprint,
                confirmation_text: confirmation_text.clone(),
                created_at: now,
                approved_at: None,
                consumed: false,
            },
        );
        Ok(PreparedPrivilegedAction {
            confirmation_id,
            action: action.to_string(),
            target_summary,
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

    pub fn consume_grant(
        &mut self,
        session_id: &str,
        grant_id: &str,
        action: &str,
        binding: &PrivilegedActionBinding,
    ) -> AppResult<()> {
        validate_privileged_action(action)?;
        let actual_binding_fingerprint = binding_fingerprint(action, binding)?;
        let now = Instant::now();
        self.purge_expired(now);
        let audit_root = self.audit_root.clone();
        let record = self.records.get_mut(grant_id).ok_or_else(|| {
            AppError::Message("Web Admin privileged grant is missing or expired".into())
        })?;
        if record.session_id != session_id
            || record.action != action
            || record.binding_fingerprint != actual_binding_fingerprint
        {
            append_confirmation_audit(
                audit_root.as_deref(),
                &AdminAuditEvent::new(
                    session_fingerprint(session_id),
                    action,
                    "grant_rejected",
                    "rejected",
                ),
            )?;
            return Err(AppError::Message(
                "Web Admin privileged grant does not match this session/action/target".into(),
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

    pub fn record_execution_outcome(
        &self,
        session_id: &str,
        action: &str,
        succeeded: bool,
    ) -> AppResult<()> {
        validate_privileged_action(action)?;
        self.append_audit(&AdminAuditEvent::new(
            session_fingerprint(session_id),
            action,
            "execution_completed",
            if succeeded { "succeeded" } else { "failed" },
        ))
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
    use std::collections::HashSet;

    #[test]
    fn privileged_manifests_are_complete_and_disjoint() {
        let all = PRIVILEGED_ACTIONS.iter().copied().collect::<HashSet<_>>();
        let available = AVAILABLE_PRIVILEGED_EXECUTORS
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let unavailable = UNAVAILABLE_PRIVILEGED_ACTIONS
            .iter()
            .copied()
            .collect::<HashSet<_>>();

        assert!(available.is_disjoint(&unavailable));
        assert_eq!(
            all,
            available
                .union(&unavailable)
                .copied()
                .collect::<HashSet<_>>()
        );
        #[cfg(windows)]
        assert!(available.contains("install_windows_service"));
        #[cfg(not(windows))]
        assert!(unavailable.contains("install_windows_service"));
    }

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
        let binding = PrivilegedActionBinding::workspace_secret("workspace-a", "bearer_token");
        let prepared = store
            .prepare("session-a", "set_workspace_secret", &binding)
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
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "set_workspace_secret",
                &binding,
            )
            .expect("consume once");
        assert!(store
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "set_workspace_secret",
                &binding,
            )
            .is_err());
    }

    #[test]
    fn grant_is_bound_to_the_exact_non_sensitive_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut store = PrivilegedConfirmationStore::for_test(temp.path().to_path_buf());
        let original = PrivilegedActionBinding::workspace_secret("workspace-a", "bearer_token");
        let different_workspace =
            PrivilegedActionBinding::workspace_secret("workspace-b", "bearer_token");
        let different_key =
            PrivilegedActionBinding::workspace_secret("workspace-a", "oauth_password");

        let prepared = store
            .prepare("session-a", "set_workspace_secret", &original)
            .expect("prepare");
        let grant = store
            .confirm(
                "session-a",
                &prepared.confirmation_id,
                &prepared.confirmation_text,
            )
            .expect("confirm");

        assert!(store
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "set_workspace_secret",
                &different_workspace,
            )
            .is_err());
        assert!(store
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "set_workspace_secret",
                &different_key,
            )
            .is_err());
        store
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "set_workspace_secret",
                &original,
            )
            .expect("matching target remains consumable");
    }

    #[test]
    fn confirmation_text_changes_with_target_binding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut store = PrivilegedConfirmationStore::for_test(temp.path().to_path_buf());
        let first = store
            .prepare(
                "session-a",
                "set_shared_secret",
                &PrivilegedActionBinding::shared_secret("bearer_token"),
            )
            .expect("first prepare");
        let second = store
            .prepare(
                "session-a",
                "set_shared_secret",
                &PrivilegedActionBinding::shared_secret("oauth_password"),
            )
            .expect("second prepare");

        assert_ne!(first.confirmation_text, second.confirmation_text);
        assert!(!first.confirmation_text.contains("secret-value"));
    }

    #[test]
    fn software_install_grant_is_bound_to_the_pinned_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut store = PrivilegedConfirmationStore::for_test(temp.path().to_path_buf());
        let target = PrivilegedActionBinding::software_install("frpc", "0.61.2");
        let prepared = store
            .prepare("session-a", "install_software", &target)
            .expect("prepare");
        assert!(prepared.target_summary.contains("0.61.2"));
        let grant = store
            .confirm(
                "session-a",
                &prepared.confirmation_id,
                &prepared.confirmation_text,
            )
            .expect("confirm");

        assert!(store
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "install_software",
                &PrivilegedActionBinding::software_install("frpc", "0.61.3"),
            )
            .is_err());
        store
            .consume_grant("session-a", &grant.grant_id, "install_software", &target)
            .expect("matching version remains consumable");
    }

    #[test]
    fn windows_service_grant_is_bound_to_service_scope_and_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut store = PrivilegedConfirmationStore::for_test(temp.path().to_path_buf());
        let original =
            PrivilegedActionBinding::windows_service("AnchorControlPlane-abcd1234", "revision-a");
        let different_revision =
            PrivilegedActionBinding::windows_service("AnchorControlPlane-abcd1234", "revision-b");
        let different_service =
            PrivilegedActionBinding::windows_service("AnchorControlPlane-efgh5678", "revision-a");

        let prepared = store
            .prepare("session-a", "restart_windows_service", &original)
            .expect("prepare");
        assert_eq!(
            prepared.target_summary,
            "Windows Service AnchorControlPlane-abcd1234"
        );
        assert!(!prepared.target_summary.contains("revision-a"));
        let grant = store
            .confirm(
                "session-a",
                &prepared.confirmation_id,
                &prepared.confirmation_text,
            )
            .expect("confirm");

        assert!(store
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "restart_windows_service",
                &different_revision,
            )
            .is_err());
        assert!(store
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "restart_windows_service",
                &different_service,
            )
            .is_err());
        store
            .consume_grant(
                "session-a",
                &grant.grant_id,
                "restart_windows_service",
                &original,
            )
            .expect("matching service target remains consumable");
    }
}
