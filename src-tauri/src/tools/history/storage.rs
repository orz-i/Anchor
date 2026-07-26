use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::tools::workspace::{relative_display, Workspace, WorkspaceError, WorkspaceResult};

use super::markdown;
use super::model::{HistoryDocument, HistoryIndex, IndexEntry, ScanReport};

pub const DEFAULT_HISTORY_DIR: &str = "docs/history-session";
pub const MAX_HISTORY_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_HISTORY_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_HISTORY_DOCUMENTS: usize = 4096;
const MAX_HISTORY_INDEX_BYTES: u64 = 1024 * 1024;
const HISTORY_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const HISTORY_LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);

pub struct HistoryLock {
    file: File,
}

impl Drop for HistoryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn history_capacity_error(message: &str, details: serde_json::Value) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "HISTORY_CAPACITY_EXCEEDED",
        message: message.into(),
        category: "validation",
        retryable: false,
        details,
    }
}

pub fn resolve_history_dir(
    workspace: &Workspace,
    workspace_root: Option<&str>,
    history_dir: Option<&str>,
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

    let raw = history_dir.unwrap_or(DEFAULT_HISTORY_DIR).trim();
    if raw.is_empty() || workspace.reject_unsafe_text(raw).is_err() {
        return Err(WorkspaceError::path_outside_workspace());
    }
    let candidate = workspace
        .root()
        .join(raw.replace('/', std::path::MAIN_SEPARATOR_STR));
    ensure_safe_candidate(workspace, &candidate)?;
    if candidate.exists() && !candidate.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "history_dir must be a directory",
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
    fs::create_dir_all(path).map_err(|error| io_error("HISTORY_WRITE_FAILED", error, true))
}

pub fn lock_directory(path: &Path) -> WorkspaceResult<HistoryLock> {
    ensure_directory(path)?;
    let lock_path = path.join(".history.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|error| io_error("HISTORY_LOCK_FAILED", error, true))?;
    let started = Instant::now();
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => break,
            Err(error) if lock_is_contended(&error) => {
                if started.elapsed() >= HISTORY_LOCK_TIMEOUT {
                    return Err(WorkspaceError::ToolDetails {
                        code: "HISTORY_LOCK_TIMEOUT",
                        message: format!(
                            "History directory lock was not available after {} seconds",
                            HISTORY_LOCK_TIMEOUT.as_secs()
                        ),
                        category: "runtime",
                        retryable: true,
                        details: serde_json::json!({
                            "termination_reason": "timeout",
                            "timeout_ms": HISTORY_LOCK_TIMEOUT.as_millis(),
                            "recoverable": true
                        }),
                    });
                }
                std::thread::sleep(HISTORY_LOCK_RETRY_DELAY);
            }
            Err(error) => {
                return Err(io_error("HISTORY_LOCK_FAILED", error, true));
            }
        }
    }
    Ok(HistoryLock { file })
}

fn lock_is_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(32 | 33))
}

pub fn scan(workspace: &Workspace, history_dir: &Path) -> WorkspaceResult<ScanReport> {
    if !history_dir.exists() {
        return Ok(ScanReport::default());
    }
    ensure_safe_candidate(workspace, history_dir)?;
    let mut report = ScanReport::default();
    let mut total_bytes = 0_u64;
    let entries =
        fs::read_dir(history_dir).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
        let file_type = entry
            .file_type()
            .map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "README.md" | "index.json" | ".history.lock")
            || name.starts_with(".history-tmp-")
        {
            continue;
        }
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            report.invalid_files.push(name);
            continue;
        };
        let is_markdown = path.extension().and_then(|value| value.to_str()) == Some("md");
        let number = stem.parse::<u64>().ok();
        if !is_markdown
            || number.is_none()
            || number == Some(0)
            || number.map(|value| value.to_string()) != Some(stem.to_string())
        {
            report.invalid_files.push(name);
            continue;
        }
        let number = number.expect("validated number");
        if report.documents.len() >= MAX_HISTORY_DOCUMENTS {
            return Err(history_capacity_error(
                "History archive contains too many session documents.",
                serde_json::json!({
                    "max_documents": MAX_HISTORY_DOCUMENTS,
                    "history_dir": relative_display(workspace.root(), history_dir)
                }),
            ));
        }
        let metadata =
            fs::metadata(&path).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
        let size_bytes = metadata.len();
        if size_bytes > MAX_HISTORY_FILE_BYTES {
            return Err(history_capacity_error(
                "History Markdown exceeds the per-session size limit.",
                serde_json::json!({
                    "file": name,
                    "size_bytes": size_bytes,
                    "max_file_bytes": MAX_HISTORY_FILE_BYTES
                }),
            ));
        }
        total_bytes = total_bytes.saturating_add(size_bytes);
        if total_bytes > MAX_HISTORY_TOTAL_BYTES {
            return Err(history_capacity_error(
                "History archive exceeds the total size limit.",
                serde_json::json!({
                    "total_bytes": total_bytes,
                    "max_total_bytes": MAX_HISTORY_TOTAL_BYTES
                }),
            ));
        }
        let bytes =
            fs::read(&path).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
        let content = String::from_utf8(bytes).map_err(|error| WorkspaceError::ToolDetails {
            code: "HISTORY_INVALID_UTF8",
            message: "History Markdown must be UTF-8.".into(),
            category: "validation",
            retryable: false,
            details: serde_json::json!({"file": name, "error": error.to_string()}),
        })?;
        if content.trim().is_empty() {
            report.empty_files.push(name.clone());
        }
        report.documents.push(HistoryDocument {
            number,
            path: relative_display(workspace.root(), &path),
            size_bytes,
            session_key: markdown::metadata(&content, "Session key"),
            created_at: markdown::metadata(&content, "Created"),
            updated_at: markdown::metadata(&content, "Updated"),
            status: markdown::metadata(&content, "Status"),
            content,
        });
    }
    report.documents.sort_by_key(|document| document.number);
    report.invalid_files.sort();
    report.empty_files.sort();
    report.numbers = report
        .documents
        .iter()
        .map(|document| document.number)
        .collect();
    if let Some(latest) = report.latest_number() {
        let present = report.numbers.iter().copied().collect::<BTreeSet<_>>();
        report.missing_numbers = (1..=latest)
            .filter(|number| !present.contains(number))
            .collect();
    }
    let mut keys = BTreeMap::<String, usize>::new();
    for key in report
        .documents
        .iter()
        .filter_map(|document| document.session_key.as_ref())
    {
        *keys.entry(key.clone()).or_default() += 1;
    }
    report.duplicate_session_keys = keys
        .into_iter()
        .filter_map(|(key, count)| (count > 1).then_some(key))
        .collect();
    Ok(report)
}

pub fn rebuild_index(report: &ScanReport) -> HistoryIndex {
    let duplicates = report
        .duplicate_session_keys
        .iter()
        .collect::<BTreeSet<_>>();
    let mut index = HistoryIndex {
        latest_number: report.latest_number().unwrap_or(0),
        ..HistoryIndex::default()
    };
    for document in &report.documents {
        let Some(session_key) = document.session_key.as_ref() else {
            continue;
        };
        if duplicates.contains(session_key) {
            continue;
        }
        index.sessions.insert(
            session_key.clone(),
            IndexEntry {
                number: document.number,
                path: document.path.clone(),
                created_at: document.created_at.clone().unwrap_or_default(),
                updated_at: document.updated_at.clone().unwrap_or_default(),
            },
        );
    }
    index
}

pub fn read_index(history_dir: &Path) -> WorkspaceResult<Option<HistoryIndex>> {
    let path = history_dir.join("index.json");
    if !path.exists() {
        return Ok(None);
    }
    let size_bytes = fs::metadata(&path)
        .map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?
        .len();
    if size_bytes > MAX_HISTORY_INDEX_BYTES {
        return Err(history_capacity_error(
            "History index exceeds its size limit.",
            serde_json::json!({
                "size_bytes": size_bytes,
                "max_index_bytes": MAX_HISTORY_INDEX_BYTES
            }),
        ));
    }
    let content =
        fs::read_to_string(&path).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|error| WorkspaceError::ToolDetails {
            code: "HISTORY_INDEX_INVALID",
            message: "History index is not valid JSON.".into(),
            category: "validation",
            retryable: true,
            details: serde_json::json!({"error": error.to_string()}),
        })
}

pub fn write_index(history_dir: &Path, index: &HistoryIndex) -> WorkspaceResult<()> {
    let content =
        serde_json::to_vec_pretty(index).map_err(|error| WorkspaceError::ToolDetails {
            code: "HISTORY_WRITE_FAILED",
            message: "Unable to serialize history index.".into(),
            category: "internal",
            retryable: true,
            details: serde_json::json!({"error": error.to_string()}),
        })?;
    if content.len() as u64 > MAX_HISTORY_INDEX_BYTES {
        return Err(history_capacity_error(
            "History index would exceed its size limit.",
            serde_json::json!({
                "size_bytes": content.len(),
                "max_index_bytes": MAX_HISTORY_INDEX_BYTES
            }),
        ));
    }
    atomic_write(&history_dir.join("index.json"), &content)
}

pub fn write_markdown(path: &Path, content: &str) -> WorkspaceResult<()> {
    ensure_history_document_capacity(content)?;
    atomic_write(path, content.as_bytes())
}

pub fn ensure_history_document_capacity(content: &str) -> WorkspaceResult<()> {
    let size_bytes = content.len() as u64;
    if size_bytes <= MAX_HISTORY_FILE_BYTES {
        return Ok(());
    }
    Err(history_capacity_error(
        "History Markdown would exceed the per-session size limit.",
        serde_json::json!({
            "size_bytes": size_bytes,
            "max_file_bytes": MAX_HISTORY_FILE_BYTES,
            "suggestion": "减少单次 checkpoint 内容，或使用新的显式 session_key 开始后续归档"
        }),
    ))
}

pub fn ensure_history_archive_capacity(
    current_total_bytes: u64,
    previous_document_bytes: u64,
    new_document_bytes: u64,
) -> WorkspaceResult<()> {
    let projected = current_total_bytes
        .saturating_sub(previous_document_bytes)
        .saturating_add(new_document_bytes);
    if projected <= MAX_HISTORY_TOTAL_BYTES {
        return Ok(());
    }
    Err(history_capacity_error(
        "History archive write would exceed the total size limit.",
        serde_json::json!({
            "current_total_bytes": current_total_bytes,
            "previous_document_bytes": previous_document_bytes,
            "new_document_bytes": new_document_bytes,
            "projected_total_bytes": projected,
            "max_total_bytes": MAX_HISTORY_TOTAL_BYTES
        }),
    ))
}

pub fn sha256(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn atomic_write(target: &Path, content: &[u8]) -> WorkspaceResult<()> {
    let parent = target
        .parent()
        .ok_or_else(|| WorkspaceError::invalid_argument("History target has no parent"))?;
    ensure_directory(parent)?;
    let temp = parent.join(format!(".history-tmp-{}", uuid::Uuid::new_v4()));
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
    result.map_err(|error| io_error("HISTORY_WRITE_FAILED", error, true))
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
