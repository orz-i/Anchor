use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::tools::workspace::{relative_display, Workspace, WorkspaceError, WorkspaceResult};

use super::markdown;
use super::model::{IndexEntry, ScanReport, SessionDocument, SessionIndex};

pub const SESSION_DIR: &str = "docs/session";
pub const MAX_SESSION_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_SESSION_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_SESSION_DOCUMENTS: usize = 4096;
const MAX_SESSION_INDEX_BYTES: u64 = 1024 * 1024;
const SESSION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);

pub struct SessionLock {
    file: File,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn capacity_error(message: &str, details: serde_json::Value) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "SESSION_CAPACITY_EXCEEDED",
        message: message.into(),
        category: "validation",
        retryable: false,
        details,
    }
}

pub fn resolve_session_dir(
    workspace: &Workspace,
    workspace_root: Option<&str>,
) -> WorkspaceResult<PathBuf> {
    if let Some(requested_root) = workspace_root {
        let requested_path = Path::new(requested_root.trim());
        let candidate = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            workspace.root().join(requested_path)
        };
        let requested = candidate
            .canonicalize()
            .map_err(|_| WorkspaceError::invalid_argument("workspace_root does not exist"))?;
        if requested != workspace.root() {
            return Err(WorkspaceError::path_outside_workspace());
        }
    }

    let candidate = workspace.root().join(SESSION_DIR);
    ensure_safe_candidate(workspace, &candidate)?;
    if candidate.exists() && !candidate.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "session_dir must be a directory",
        ));
    }
    Ok(candidate)
}

fn ensure_safe_candidate(workspace: &Workspace, candidate: &Path) -> WorkspaceResult<()> {
    if candidate.exists() || candidate.is_symlink() {
        let resolved = candidate
            .canonicalize()
            .map_err(|_| WorkspaceError::path_outside_workspace())?;
        if !resolved.starts_with(workspace.root()) {
            return Err(WorkspaceError::path_outside_workspace());
        }
        return Ok(());
    }
    let mut ancestor = candidate.parent();
    while let Some(path) = ancestor {
        if path.exists() || path.is_symlink() {
            let resolved = path
                .canonicalize()
                .map_err(|_| WorkspaceError::path_outside_workspace())?;
            if !resolved.starts_with(workspace.root()) {
                return Err(WorkspaceError::path_outside_workspace());
            }
            return Ok(());
        }
        ancestor = path.parent();
    }
    Err(WorkspaceError::path_outside_workspace())
}

pub fn ensure_directory(path: &Path) -> WorkspaceResult<()> {
    fs::create_dir_all(path).map_err(|error| io_error("SESSION_WRITE_FAILED", error, true))
}

pub fn lock_directory(path: &Path) -> WorkspaceResult<SessionLock> {
    ensure_directory(path)?;
    let lock_path = path.join(".session.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|error| io_error("SESSION_LOCK_FAILED", error, true))?;
    let started = Instant::now();
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => break,
            Err(error) if lock_is_contended(&error) => {
                if started.elapsed() >= SESSION_LOCK_TIMEOUT {
                    return Err(WorkspaceError::ToolDetails {
                        code: "SESSION_LOCK_TIMEOUT",
                        message: format!(
                            "Session directory lock was not available after {} seconds",
                            SESSION_LOCK_TIMEOUT.as_secs()
                        ),
                        category: "runtime",
                        retryable: true,
                        details: serde_json::json!({
                            "termination_reason": "timeout",
                            "timeout_ms": SESSION_LOCK_TIMEOUT.as_millis(),
                            "recoverable": true
                        }),
                    });
                }
                std::thread::sleep(SESSION_LOCK_RETRY_DELAY);
            }
            Err(error) => return Err(io_error("SESSION_LOCK_FAILED", error, true)),
        }
    }
    Ok(SessionLock { file })
}

fn lock_is_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(32 | 33))
}

pub fn new_session_id() -> String {
    format!("ses_{}", uuid::Uuid::new_v4().simple())
}

pub fn valid_session_id(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("ses_") else {
        return false;
    };
    hex.len() == 32 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn scan(workspace: &Workspace, session_dir: &Path) -> WorkspaceResult<ScanReport> {
    if !session_dir.exists() {
        return Ok(ScanReport::default());
    }
    ensure_safe_candidate(workspace, session_dir)?;
    let mut report = ScanReport::default();
    let mut total_bytes = 0_u64;
    let entries =
        fs::read_dir(session_dir).map_err(|error| io_error("SESSION_READ_FAILED", error, true))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error("SESSION_READ_FAILED", error, true))?;
        let file_type = entry
            .file_type()
            .map_err(|error| io_error("SESSION_READ_FAILED", error, true))?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "README.md" | "index.json" | ".session.lock")
            || name.starts_with(".session-tmp-")
        {
            continue;
        }
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            report.invalid_files.push(name);
            continue;
        };
        let is_markdown = path.extension().and_then(|value| value.to_str()) == Some("md");
        if !is_markdown || !valid_session_id(stem) {
            report.invalid_files.push(name);
            continue;
        }
        if report.documents.len() >= MAX_SESSION_DOCUMENTS {
            return Err(capacity_error(
                "Session store contains too many documents.",
                serde_json::json!({
                    "max_documents": MAX_SESSION_DOCUMENTS,
                    "session_dir": relative_display(workspace.root(), session_dir)
                }),
            ));
        }
        let metadata =
            fs::metadata(&path).map_err(|error| io_error("SESSION_READ_FAILED", error, true))?;
        let size_bytes = metadata.len();
        if size_bytes > MAX_SESSION_FILE_BYTES {
            return Err(capacity_error(
                "Session Markdown exceeds the per-session size limit.",
                serde_json::json!({
                    "file": name,
                    "size_bytes": size_bytes,
                    "max_file_bytes": MAX_SESSION_FILE_BYTES
                }),
            ));
        }
        total_bytes = total_bytes.saturating_add(size_bytes);
        if total_bytes > MAX_SESSION_TOTAL_BYTES {
            return Err(capacity_error(
                "Session store exceeds the total size limit.",
                serde_json::json!({
                    "total_bytes": total_bytes,
                    "max_total_bytes": MAX_SESSION_TOTAL_BYTES
                }),
            ));
        }
        let bytes =
            fs::read(&path).map_err(|error| io_error("SESSION_READ_FAILED", error, true))?;
        let content = String::from_utf8(bytes).map_err(|error| WorkspaceError::ToolDetails {
            code: "SESSION_INVALID_UTF8",
            message: "Session Markdown must be UTF-8.".into(),
            category: "validation",
            retryable: false,
            details: serde_json::json!({"file": name, "error": error.to_string()}),
        })?;
        if content.trim().is_empty() {
            report.empty_files.push(name.clone());
        }
        let metadata_session_id = markdown::metadata(&content, "Session id");
        if metadata_session_id.as_deref() != Some(stem) {
            report.invalid_files.push(name);
            continue;
        }
        report.documents.push(SessionDocument {
            session_id: stem.to_string(),
            path: relative_display(workspace.root(), &path),
            title: markdown::document_title(&content),
            size_bytes,
            host_session_scope: markdown::metadata(&content, "Host session scope"),
            parent_session_id: markdown::metadata(&content, "Parent session id"),
            created_at: markdown::metadata(&content, "Created"),
            updated_at: markdown::metadata(&content, "Updated"),
            status: markdown::metadata(&content, "Status"),
        });
    }
    report.documents.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.session_id.cmp(&right.session_id))
    });
    report.invalid_files.sort();
    report.empty_files.sort();

    let mut ids = BTreeMap::<String, usize>::new();
    let mut host_scopes = BTreeMap::<String, usize>::new();
    for document in &report.documents {
        *ids.entry(document.session_id.clone()).or_default() += 1;
        if let Some(scope) = document.host_session_scope.as_ref() {
            *host_scopes.entry(scope.clone()).or_default() += 1;
        }
    }
    report.duplicate_session_ids = ids
        .into_iter()
        .filter_map(|(id, count)| (count > 1).then_some(id))
        .collect();
    report.duplicate_host_session_scopes = host_scopes
        .into_iter()
        .filter_map(|(scope, count)| {
            (count > 1 && !valid_continuation_chain(&report.documents, &scope)).then_some(scope)
        })
        .collect();
    Ok(report)
}

fn valid_continuation_chain(documents: &[SessionDocument], host_scope: &str) -> bool {
    let members = documents
        .iter()
        .filter(|document| document.host_session_scope.as_deref() == Some(host_scope))
        .collect::<Vec<_>>();
    if members.len() <= 1 {
        return true;
    }
    let ids = members
        .iter()
        .map(|document| document.session_id.as_str())
        .collect::<BTreeSet<_>>();
    let roots = members
        .iter()
        .filter(|document| document.parent_session_id.is_none())
        .count();
    if roots != 1 {
        return false;
    }
    let mut children = BTreeMap::<&str, usize>::new();
    for document in members {
        let Some(parent) = document.parent_session_id.as_deref() else {
            continue;
        };
        if parent == document.session_id || !ids.contains(parent) {
            return false;
        }
        let count = children.entry(parent).or_default();
        *count += 1;
        if *count > 1 {
            return false;
        }
    }
    true
}

pub fn rebuild_index(report: &ScanReport) -> SessionIndex {
    let duplicate_ids = report.duplicate_session_ids.iter().collect::<BTreeSet<_>>();
    let duplicate_hosts = report
        .duplicate_host_session_scopes
        .iter()
        .collect::<BTreeSet<_>>();
    let mut index = SessionIndex::default();
    for document in &report.documents {
        if duplicate_ids.contains(&document.session_id) {
            continue;
        }
        index.sessions.insert(
            document.session_id.clone(),
            IndexEntry {
                path: document.path.clone(),
                title: document.title.clone(),
                status: document.status.clone().unwrap_or_else(|| "active".into()),
                created_at: document.created_at.clone().unwrap_or_default(),
                updated_at: document.updated_at.clone().unwrap_or_default(),
                parent_session_id: document.parent_session_id.clone(),
            },
        );
        if let Some(host_scope) = document.host_session_scope.as_ref() {
            if !duplicate_hosts.contains(host_scope) {
                // report.documents is ordered oldest -> newest, so a valid continuation
                // chain naturally leaves the current leaf bound to the host conversation.
                index
                    .host_scopes
                    .insert(host_scope.clone(), document.session_id.clone());
            }
        }
    }
    index
}

pub fn read_index(session_dir: &Path) -> WorkspaceResult<Option<SessionIndex>> {
    let path = session_dir.join("index.json");
    if !path.exists() {
        return Ok(None);
    }
    let size_bytes = fs::metadata(&path)
        .map_err(|error| io_error("SESSION_READ_FAILED", error, true))?
        .len();
    if size_bytes > MAX_SESSION_INDEX_BYTES {
        return Err(capacity_error(
            "Session index exceeds its size limit.",
            serde_json::json!({
                "size_bytes": size_bytes,
                "max_index_bytes": MAX_SESSION_INDEX_BYTES
            }),
        ));
    }
    let content =
        fs::read_to_string(&path).map_err(|error| io_error("SESSION_READ_FAILED", error, true))?;
    let index = serde_json::from_str::<SessionIndex>(&content).map_err(|error| {
        WorkspaceError::ToolDetails {
            code: "SESSION_INDEX_INVALID",
            message: "Session index does not match the current schema.".into(),
            category: "validation",
            retryable: true,
            details: serde_json::json!({
                "error": error.to_string(),
                "required_version": super::model::SESSION_INDEX_VERSION,
                "suggestion": "Run session operation=validate with repair=true to rebuild the current index from Session documents."
            }),
        }
    })?;
    if index.version != super::model::SESSION_INDEX_VERSION {
        return Err(WorkspaceError::ToolDetails {
            code: "SESSION_INDEX_VERSION_UNSUPPORTED",
            message: "Session index version is not supported by this Anchor build.".into(),
            category: "validation",
            retryable: true,
            details: serde_json::json!({
                "version": index.version,
                "required_version": super::model::SESSION_INDEX_VERSION,
                "suggestion": "Run session operation=validate with repair=true to rebuild the current index from Session documents."
            }),
        });
    }
    Ok(Some(index))
}

pub fn write_index(session_dir: &Path, index: &SessionIndex) -> WorkspaceResult<()> {
    let content =
        serde_json::to_vec_pretty(index).map_err(|error| WorkspaceError::ToolDetails {
            code: "SESSION_WRITE_FAILED",
            message: "Unable to serialize Session index.".into(),
            category: "internal",
            retryable: true,
            details: serde_json::json!({"error": error.to_string()}),
        })?;
    if content.len() as u64 > MAX_SESSION_INDEX_BYTES {
        return Err(capacity_error(
            "Session index would exceed its size limit.",
            serde_json::json!({
                "size_bytes": content.len(),
                "max_index_bytes": MAX_SESSION_INDEX_BYTES
            }),
        ));
    }
    atomic_write(&session_dir.join("index.json"), &content)
}

pub fn write_markdown(path: &Path, content: &str) -> WorkspaceResult<()> {
    ensure_session_document_capacity(content)?;
    atomic_write(path, content.as_bytes())
}

pub fn ensure_session_document_capacity(content: &str) -> WorkspaceResult<()> {
    let size_bytes = content.len() as u64;
    if size_bytes <= MAX_SESSION_FILE_BYTES {
        return Ok(());
    }
    Err(capacity_error(
        "Session Markdown would exceed the per-session size limit.",
        serde_json::json!({
            "size_bytes": size_bytes,
            "max_file_bytes": MAX_SESSION_FILE_BYTES,
            "suggestion": "减少单次 checkpoint 内容，或开始新的 Session"
        }),
    ))
}

pub fn ensure_session_store_capacity(
    current_total_bytes: u64,
    previous_document_bytes: u64,
    new_document_bytes: u64,
) -> WorkspaceResult<()> {
    let projected = current_total_bytes
        .saturating_sub(previous_document_bytes)
        .saturating_add(new_document_bytes);
    if projected <= MAX_SESSION_TOTAL_BYTES {
        return Ok(());
    }
    Err(capacity_error(
        "Session store write would exceed the total size limit.",
        serde_json::json!({
            "current_total_bytes": current_total_bytes,
            "previous_document_bytes": previous_document_bytes,
            "new_document_bytes": new_document_bytes,
            "projected_total_bytes": projected,
            "max_total_bytes": MAX_SESSION_TOTAL_BYTES
        }),
    ))
}

pub fn sha256(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn atomic_write(target: &Path, content: &[u8]) -> WorkspaceResult<()> {
    let parent = target
        .parent()
        .ok_or_else(|| WorkspaceError::invalid_argument("Session target has no parent"))?;
    ensure_directory(parent)?;
    let temp = parent.join(format!(".session-tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(content)?;
        file.sync_all()?;
        atomic_replace(&temp, target)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|error| io_error("SESSION_WRITE_FAILED", error, true))
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, target: &Path) -> io::Result<()> {
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
        .map_err(|error| io::Error::other(error.to_string()))
    }
}

fn io_error(code: &'static str, error: io::Error, retryable: bool) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: error.to_string(),
        category: "filesystem",
        retryable,
        details: serde_json::json!({"kind": format!("{:?}", error.kind())}),
    }
}
