use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};

use crate::tools::workspace::{
    relative_display, tool_ok, Workspace, WorkspaceError, DEFAULT_EXCLUDED_NAMES,
};
use crate::tools::CancellationToken;

const DEFAULT_READ_BUDGET: usize = 131_072;
const MAX_BATCH_READ_FILES: usize = 32;
const DEFAULT_SCAN_MAX_FILES: usize = 100_000;
const MAX_SCAN_MAX_FILES: usize = 250_000;
const DEFAULT_SEARCH_MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_SEARCH_MAX_BYTES: usize = 1024 * 1024 * 1024;
const DEFAULT_SCAN_TIMEOUT_MS: u64 = 30_000;
const MAX_SCAN_TIMEOUT_MS: u64 = 120_000;

static REPOSITORY_SCAN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone)]
struct ReadRequest {
    path: String,
    start_line: usize,
    end_line: Option<usize>,
    start_byte: usize,
}

#[derive(Default)]
struct RepositoryScan {
    files: Vec<PathBuf>,
    skipped_entries: usize,
}

#[derive(Debug, Clone)]
struct RepositoryScanBudget {
    started: Instant,
    max_files: usize,
    max_bytes: Option<usize>,
    timeout: Duration,
}

impl RepositoryScanBudget {
    fn from_args(args: &Value, include_bytes: bool) -> Self {
        let max_files = args
            .get("max_scan_files")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_SCAN_MAX_FILES as u64)
            .clamp(1, MAX_SCAN_MAX_FILES as u64) as usize;
        let max_bytes = include_bytes.then(|| {
            args.get("max_scan_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_SEARCH_MAX_BYTES as u64)
                .clamp(1, MAX_SEARCH_MAX_BYTES as u64) as usize
        });
        let timeout_ms = args
            .get("scan_timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_SCAN_TIMEOUT_MS)
            .clamp(1_000, MAX_SCAN_TIMEOUT_MS);
        Self {
            started: Instant::now(),
            max_files,
            max_bytes,
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    fn checkpoint(
        &self,
        candidate_files: usize,
        bytes_scanned: usize,
    ) -> Result<(), WorkspaceError> {
        if candidate_files > self.max_files {
            return Err(self.exceeded("files", candidate_files, bytes_scanned));
        }
        if self
            .max_bytes
            .is_some_and(|max_bytes| bytes_scanned > max_bytes)
        {
            return Err(self.exceeded("bytes", candidate_files, bytes_scanned));
        }
        if self.started.elapsed() > self.timeout {
            return Err(self.exceeded("time", candidate_files, bytes_scanned));
        }
        Ok(())
    }

    fn exceeded(
        &self,
        dimension: &str,
        candidate_files: usize,
        bytes_scanned: usize,
    ) -> WorkspaceError {
        WorkspaceError::ToolDetails {
            code: "REPOSITORY_SCAN_BUDGET_EXCEEDED",
            message: format!("Repository scan exceeded its {dimension} budget."),
            category: "runtime",
            retryable: true,
            details: json!({
                "dimension": dimension,
                "candidate_files": candidate_files,
                "bytes_scanned": bytes_scanned,
                "elapsed_ms": self.started.elapsed().as_millis(),
                "max_scan_files": self.max_files,
                "max_scan_bytes": self.max_bytes,
                "scan_timeout_ms": self.timeout.as_millis(),
                "workspace_modified": false,
                "suggestion": "Narrow path/globs or retry with a larger bounded scan budget."
            }),
        }
    }

    fn telemetry(&self, candidate_files: usize, bytes_scanned: usize) -> Value {
        json!({
            "candidate_files": candidate_files,
            "bytes_scanned": bytes_scanned,
            "elapsed_ms": self.started.elapsed().as_millis(),
            "max_scan_files": self.max_files,
            "max_scan_bytes": self.max_bytes,
            "scan_timeout_ms": self.timeout.as_millis(),
            "admission": "exclusive_process",
            "budget_complete": true
        })
    }
}

fn acquire_repository_scan(
    budget: &RepositoryScanBudget,
    cancellation: &CancellationToken,
) -> Result<MutexGuard<'static, ()>, WorkspaceError> {
    let lock = REPOSITORY_SCAN_LOCK.get_or_init(|| Mutex::new(()));
    loop {
        ensure_not_cancelled(cancellation)?;
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::WouldBlock) => {
                budget.checkpoint(0, 0)?;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(WorkspaceError::ToolDetails {
                    code: "REPOSITORY_SCAN_ADMISSION_FAILED",
                    message: "Repository scan admission state is unavailable.".into(),
                    category: "runtime",
                    retryable: true,
                    details: json!({
                        "workspace_modified": false,
                        "admission": "exclusive_process"
                    }),
                });
            }
        }
    }
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), WorkspaceError> {
    if cancellation.is_cancelled() {
        return Err(WorkspaceError::ToolDetails {
            code: "REQUEST_CANCELLED",
            message: "Tool request was cancelled".into(),
            category: "runtime",
            retryable: true,
            details: json!({"reason": "client_cancelled", "retryable": true}),
        });
    }
    Ok(())
}

fn decode_text(data: Vec<u8>) -> Result<(String, &'static str), WorkspaceError> {
    if let Some(payload) = data.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(payload.to_vec())
            .map(|text| (text, "utf-8"))
            .map_err(|_| unsupported_encoding());
    }
    if let Some(payload) = data.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(payload, true).map(|text| (text, "utf-16le"));
    }
    if let Some(payload) = data.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(payload, false).map(|text| (text, "utf-16be"));
    }
    if data.iter().take(4096).any(|byte| *byte == 0) {
        return Err(WorkspaceError::Tool {
            code: "BINARY_FILE",
            message: "Binary file read blocked for text tool.".into(),
            category: "validation",
            retryable: false,
        });
    }
    String::from_utf8(data)
        .map(|text| (text, "utf-8"))
        .map_err(|_| unsupported_encoding())
}

fn decode_utf16(data: &[u8], little_endian: bool) -> Result<String, WorkspaceError> {
    if !data.len().is_multiple_of(2) {
        return Err(unsupported_encoding());
    }
    let units = data
        .chunks_exact(2)
        .map(|chunk| {
            let pair = [chunk[0], chunk[1]];
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| unsupported_encoding())
}

fn unsupported_encoding() -> WorkspaceError {
    WorkspaceError::Tool {
        code: "UNSUPPORTED_ENCODING",
        message: "File must be UTF-8 or BOM-marked UTF-16 text.".into(),
        category: "validation",
        retryable: false,
    }
}

fn repository_files(
    ws: &Workspace,
    root: &Path,
    include_hidden: bool,
    include_ignored: bool,
    budget: &RepositoryScanBudget,
    cancellation: &CancellationToken,
) -> Result<RepositoryScan, WorkspaceError> {
    let skipped_entries = Arc::new(AtomicUsize::new(0));
    let skipped_for_filter = Arc::clone(&skipped_entries);
    let scope_root = root.to_path_buf();
    let walk_root = if root.starts_with(ws.root()) {
        ws.root().to_path_buf()
    } else {
        scope_root.clone()
    };
    let mut builder = WalkBuilder::new(&walk_root);
    builder
        .follow_links(false)
        .hidden(!include_hidden)
        .git_ignore(!include_ignored)
        .git_exclude(!include_ignored)
        .ignore(!include_ignored)
        .parents(false)
        .require_git(false);
    builder.filter_entry(move |entry| {
        let path = entry.path();
        let in_scope =
            entry.depth() == 0 || path.starts_with(&scope_root) || scope_root.starts_with(path);
        if !in_scope {
            return false;
        }
        if include_ignored || entry.depth() == 0 {
            return true;
        }
        let is_default_excluded_dir = entry.file_type().is_some_and(|kind| kind.is_dir())
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| DEFAULT_EXCLUDED_NAMES.contains(&name));
        if is_default_excluded_dir {
            skipped_for_filter.fetch_add(1, Ordering::Relaxed);
            false
        } else {
            true
        }
    });

    let mut files = Vec::new();
    for entry in builder.build() {
        ensure_not_cancelled(cancellation)?;
        budget.checkpoint(files.len(), 0)?;
        let Ok(entry) = entry else {
            skipped_entries.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        if !entry
            .file_type()
            .is_some_and(|kind| kind.is_file() || kind.is_symlink())
        {
            continue;
        }
        let path = entry.into_path();
        if ws.is_safe_read_path(&path) {
            files.push(path);
            budget.checkpoint(files.len(), 0)?;
        }
    }
    budget.checkpoint(files.len(), 0)?;
    files.sort_by_key(|path| relative_display(ws.root(), path));
    budget.checkpoint(files.len(), 0)?;
    Ok(RepositoryScan {
        files,
        skipped_entries: skipped_entries.load(Ordering::Relaxed),
    })
}

pub fn read_file(
    ws: &Workspace,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    ensure_not_cancelled(cancellation)?;
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_READ_BUDGET as u64) as usize;

    let path = args.get("path").and_then(Value::as_str);
    let files = args.get("files").and_then(Value::as_array);
    match (path, files) {
        (Some(path), None) => {
            let request = read_request_from_value(args, Some(path))?;
            let mut result = read_one(ws, &request, max_bytes, cancellation)?;
            if let Some(object) = result.as_object_mut() {
                object.insert("mode".into(), json!("single"));
            }
            Ok(tool_ok(result))
        }
        (None, Some(files)) => {
            if files.is_empty() {
                return Err(WorkspaceError::invalid_argument(
                    "files must contain at least one read request",
                ));
            }
            if files.len() > MAX_BATCH_READ_FILES {
                return Err(WorkspaceError::invalid_argument(format!(
                    "files supports at most {MAX_BATCH_READ_FILES} requests"
                )));
            }
            let requests = files
                .iter()
                .map(|value| read_request_from_value(value, None))
                .collect::<Result<Vec<_>, _>>()?;
            read_batch(ws, &requests, max_bytes, cancellation)
        }
        (Some(_), Some(_)) => Err(WorkspaceError::invalid_argument(
            "provide either path or files, not both",
        )),
        (None, None) => Err(WorkspaceError::invalid_argument(
            "path or files is required",
        )),
    }
}

fn read_request_from_value(
    value: &Value,
    fallback_path: Option<&str>,
) -> Result<ReadRequest, WorkspaceError> {
    let path = fallback_path
        .or_else(|| value.get("path").and_then(Value::as_str))
        .ok_or_else(|| WorkspaceError::invalid_argument("read request path is required"))?;
    let start_line = value
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let end_line = value
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|line| line as usize);
    if end_line.is_some_and(|end| end < start_line) {
        return Err(WorkspaceError::invalid_argument(
            "end_line must be greater than or equal to start_line",
        ));
    }
    let start_byte = value.get("start_byte").and_then(Value::as_u64).unwrap_or(0) as usize;
    Ok(ReadRequest {
        path: path.to_string(),
        start_line,
        end_line,
        start_byte,
    })
}

fn read_batch(
    ws: &Workspace,
    requests: &[ReadRequest],
    max_bytes: usize,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    let mut remaining = max_bytes;
    let mut results = Vec::new();
    let mut continuations = Vec::new();
    let mut failed = 0usize;

    for (index, request) in requests.iter().enumerate() {
        ensure_not_cancelled(cancellation)?;
        if remaining == 0 {
            continuations.extend(requests[index..].iter().map(read_request_value));
            break;
        }
        match read_one(ws, request, remaining, cancellation) {
            Ok(result) => {
                remaining = remaining.saturating_sub(
                    result
                        .get("bytes_read")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize,
                );
                let continuation = result.get("next").filter(|next| !next.is_null()).cloned();
                if let Some(next) = continuation {
                    continuations.push(next.clone());
                }
                results.push(json!({"ok": true, "result": result}));
                if !continuations.is_empty() {
                    continuations.extend(requests[index + 1..].iter().map(read_request_value));
                    break;
                }
            }
            Err(error) => {
                failed += 1;
                results.push(json!({
                    "ok": false,
                    "path": request.path,
                    "error": {"message": error.to_string()}
                }));
            }
        }
    }

    let bytes_read = max_bytes.saturating_sub(remaining);
    let truncated = !continuations.is_empty();
    Ok(tool_ok(json!({
        "mode": "batch",
        "files": results,
        "bytes_read": bytes_read,
        "requested_files": requests.len(),
        "failed_files": failed,
        "truncated": truncated,
        "next": if truncated { json!({"files": continuations}) } else { Value::Null },
        "warnings": if truncated { vec!["shared read budget exhausted or one or more files require continuation"] } else { vec![] }
    })))
}

fn read_request_value(request: &ReadRequest) -> Value {
    json!({
        "path": request.path,
        "start_line": request.start_line,
        "start_byte": request.start_byte,
        "end_line": request.end_line
    })
}

fn read_one(
    ws: &Workspace,
    request: &ReadRequest,
    max_bytes: usize,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    let resolved = ws.resolve_read_path(&request.path)?;
    if resolved.path.is_dir() {
        return Err(WorkspaceError::Tool {
            code: "IS_DIRECTORY",
            message: "Path is a directory.".into(),
            category: "validation",
            retryable: false,
        });
    }
    let data = fs::read(&resolved.path).map_err(|_| WorkspaceError::not_found("File not found"))?;
    ensure_not_cancelled(cancellation)?;
    let (text, encoding) = decode_text(data)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total_lines = lines.len();
    let last_line = request.end_line.unwrap_or(total_lines).min(total_lines);
    let mut line_index = request.start_line.saturating_sub(1);
    let mut line_byte = request.start_byte;
    let mut content = String::new();
    let mut remaining = max_bytes;

    if line_index < lines.len() && line_byte > lines[line_index].len() {
        return Err(WorkspaceError::invalid_argument(
            "start_byte exceeds the selected start line length",
        ));
    }
    if line_index < lines.len() && !lines[line_index].is_char_boundary(line_byte) {
        return Err(WorkspaceError::invalid_argument(
            "start_byte must point to a UTF-8 character boundary in the decoded line",
        ));
    }

    let mut next = None;
    while line_index < lines.len() && line_index < last_line && remaining > 0 {
        let line = lines[line_index];
        let available = &line[line_byte..];
        if available.len() <= remaining {
            content.push_str(available);
            remaining -= available.len();
            line_index += 1;
            line_byte = 0;
            continue;
        }

        let mut take = remaining;
        while take > 0 && !available.is_char_boundary(take) {
            take -= 1;
        }
        if take == 0 && !available.is_empty() {
            next = Some(ReadRequest {
                path: request.path.clone(),
                start_line: line_index + 1,
                end_line: request.end_line,
                start_byte: line_byte,
            });
            break;
        }
        content.push_str(&available[..take]);
        line_byte += take;
        next = Some(ReadRequest {
            path: request.path.clone(),
            start_line: line_index + 1,
            end_line: request.end_line,
            start_byte: line_byte,
        });
        break;
    }

    if next.is_none() && line_index < lines.len() && line_index < last_line {
        next = Some(ReadRequest {
            path: request.path.clone(),
            start_line: line_index + 1,
            end_line: request.end_line,
            start_byte: line_byte,
        });
    }
    let truncated = next.is_some();
    let actual_end_line = if content.is_empty() {
        request.start_line.saturating_sub(1)
    } else if line_byte > 0 {
        line_index + 1
    } else {
        line_index.min(last_line)
    };

    Ok(json!({
        "path": resolved.display,
        "content": content,
        "encoding": encoding,
        "start_line": request.start_line,
        "start_byte": request.start_byte,
        "end_line": actual_end_line,
        "total_lines": total_lines,
        "total_bytes": text.len(),
        "bytes_read": content.len(),
        "truncated": truncated,
        "truncated_by": if truncated { json!("bytes") } else { Value::Null },
        "next": next.as_ref().map(read_request_value),
        "warnings": if truncated { vec!["content truncated; use next exactly to continue"] } else { vec![] }
    }))
}

pub fn list_dir(
    ws: &Workspace,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    ensure_not_cancelled(cancellation)?;
    let budget = RepositoryScanBudget::from_args(args, false);
    let _scan_guard = acquire_repository_scan(&budget, cancellation)?;
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_read_path(path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory("Path is not a directory"));
    }
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_depth = args
        .get("max_depth")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let max_entries = args
        .get("max_entries")
        .and_then(Value::as_u64)
        .unwrap_or(1000) as usize;
    let include_hidden = args
        .get("include_hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_ignored = args
        .get("include_ignored")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut entries = Vec::new();
    let mut truncated = false;
    collect_dir_entries(
        ws,
        &resolved.path,
        &resolved.display,
        1,
        max_depth,
        recursive,
        include_hidden,
        include_ignored,
        max_entries,
        &mut entries,
        &mut truncated,
        cancellation,
    )?;
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(tool_ok(json!({
        "path": resolved.display,
        "entries": entries,
        "truncated": truncated,
        "warnings": if truncated { vec!["entry limit reached"] } else { vec![] }
    })))
}

pub fn list_files(
    ws: &Workspace,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    ensure_not_cancelled(cancellation)?;
    let budget = RepositoryScanBudget::from_args(args, false);
    let _scan_guard = acquire_repository_scan(&budget, cancellation)?;
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_read_path(path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory("Path is not a directory"));
    }
    let patterns = list_files_patterns(args);
    let exclude_patterns = string_list_arg(args, "exclude_patterns");
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(5000) as usize;
    let cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let include_hidden = args
        .get("include_hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_ignored = args
        .get("include_ignored")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let scan = repository_files(
        ws,
        &resolved.path,
        include_hidden,
        include_ignored,
        &budget,
        cancellation,
    )?;
    let candidate_files = scan.files.len();
    let skipped_entries = scan.skipped_entries;
    let mut all_files = Vec::new();
    for (index, p) in scan.files.into_iter().enumerate() {
        if index % 256 == 0 {
            ensure_not_cancelled(cancellation)?;
            budget.checkpoint(candidate_files, 0)?;
        }
        let rel = relative_display(ws.root(), &p);
        if !patterns.iter().any(|pat| glob_match(pat, &rel)) {
            continue;
        }
        if exclude_patterns.iter().any(|pat| glob_match(pat, &rel)) {
            continue;
        }
        let meta = p.symlink_metadata().ok();
        all_files.push(json!({
            "path": rel,
            "type": if p.is_symlink() { "symlink" } else { "file" },
            "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            "modified": meta.and_then(|m| format_mtime(m.modified().ok()))
        }));
    }
    all_files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let total_files = all_files.len();
    let start = cursor.min(total_files);
    let end = start.saturating_add(max_results).min(total_files);
    let files = all_files.drain(start..end).collect::<Vec<_>>();
    let next_cursor = (end < total_files).then_some(end);
    let mut scan_telemetry = budget.telemetry(candidate_files, 0);
    if let Some(object) = scan_telemetry.as_object_mut() {
        object.insert("skipped_entries".into(), json!(skipped_entries));
        object.insert(
            "ignore_rules".into(),
            json!(if include_ignored {
                "disabled"
            } else {
                "project_and_default"
            }),
        );
    }
    Ok(tool_ok(json!({
        "path": resolved.display,
        "files": files,
        "cursor": cursor,
        "next_cursor": next_cursor,
        "total_files": total_files,
        "truncated": next_cursor.is_some(),
        "scan": scan_telemetry,
        "warnings": if next_cursor.is_some() { vec!["result page limit reached; continue with next_cursor"] } else { vec![] }
    })))
}

pub fn search_text(
    ws: &Workspace,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    ensure_not_cancelled(cancellation)?;
    let budget = RepositoryScanBudget::from_args(args, true);
    let _scan_guard = acquire_repository_scan(&budget, cancellation)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("query is required"))?;
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_read_path(path)?;
    let use_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(1000) as usize;
    let cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let output_mode = args
        .get("output_mode")
        .and_then(Value::as_str)
        .unwrap_or("matches");
    let max_preview = args
        .get("max_preview_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(512) as usize;

    let (include_globs, exclude_globs) = search_globs(args);
    let context_lines = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let matcher = build_matcher(query, use_regex, case_sensitive)?;
    if !matches!(output_mode, "matches" | "files" | "count" | "summary") {
        return Err(WorkspaceError::invalid_argument(
            "output_mode must be matches, files, count, or summary",
        ));
    }

    let scan = if resolved.path.is_file() {
        RepositoryScan {
            files: vec![resolved.path.clone()],
            skipped_entries: 0,
        }
    } else {
        repository_files(ws, &resolved.path, false, false, &budget, cancellation)?
    };
    budget.checkpoint(scan.files.len(), 0)?;

    let mut page_matches = Vec::new();
    let mut file_summaries = Vec::new();
    let mut total = 0usize;
    let mut bytes_scanned = 0usize;
    let mut skipped_files = 0usize;
    for p in &scan.files {
        ensure_not_cancelled(cancellation)?;
        if p.is_symlink() {
            skipped_files += 1;
            continue;
        }
        if !ws.is_safe_read_path(p) {
            continue;
        }
        let rel = relative_display(ws.root(), p);
        if !passes_glob_filters(&rel, &include_globs, &exclude_globs) {
            continue;
        }
        let anticipated_bytes = p
            .metadata()
            .ok()
            .and_then(|metadata| usize::try_from(metadata.len()).ok())
            .map(|size| bytes_scanned.saturating_add(size))
            .unwrap_or(bytes_scanned);
        budget.checkpoint(scan.files.len(), anticipated_bytes)?;
        let data = match fs::read(p) {
            Ok(data) => data,
            Err(_) => {
                skipped_files += 1;
                continue;
            }
        };
        bytes_scanned = bytes_scanned.saturating_add(data.len());
        budget.checkpoint(scan.files.len(), bytes_scanned)?;
        let content = match decode_text(data) {
            Ok((text, _)) if !text.contains('\0') => text,
            _ => {
                skipped_files += 1;
                continue;
            }
        };
        let lines: Vec<&str> = content.lines().collect();
        let mut file_match_count = 0usize;
        for (idx, line) in lines.iter().enumerate() {
            if idx % 256 == 0 {
                ensure_not_cancelled(cancellation)?;
                budget.checkpoint(scan.files.len(), bytes_scanned)?;
            }
            let Some(match_byte) = matcher.find(line) else {
                continue;
            };
            let match_index = total;
            total += 1;
            file_match_count += 1;
            if output_mode != "matches" || match_index < cursor || page_matches.len() >= max_results
            {
                continue;
            }
            let (preview, truncated, _) = truncate_bytes(line, max_preview);
            let preview = if truncated {
                format!("{preview}...")
            } else {
                preview
            };
            let mut item = json!({
                "path": rel,
                "line": idx + 1,
                "column": line[..match_byte].chars().count() + 1,
                "preview": preview
            });
            if context_lines > 0 {
                let start = idx.saturating_sub(context_lines);
                let end = (idx + 1 + context_lines).min(lines.len());
                item["before"] = json!(&lines[start..idx]);
                item["after"] = json!(&lines[idx + 1..end]);
            }
            page_matches.push(item);
        }
        if file_match_count > 0 {
            file_summaries.push(json!({"path": rel, "match_count": file_match_count}));
        }
    }

    let matched_files = file_summaries.len();
    let (matches, files, summary, next_cursor) = match output_mode {
        "matches" => {
            let next = (cursor.saturating_add(page_matches.len()) < total)
                .then_some(cursor.saturating_add(page_matches.len()));
            (page_matches, Vec::new(), Vec::new(), next)
        }
        "files" => {
            let start = cursor.min(matched_files);
            let end = start.saturating_add(max_results).min(matched_files);
            let page = file_summaries[start..end]
                .iter()
                .filter_map(|item| item.get("path").cloned())
                .collect::<Vec<_>>();
            let next = (end < matched_files).then_some(end);
            (Vec::new(), page, Vec::new(), next)
        }
        "summary" => {
            let start = cursor.min(matched_files);
            let end = start.saturating_add(max_results).min(matched_files);
            let page = file_summaries[start..end].to_vec();
            let next = (end < matched_files).then_some(end);
            (Vec::new(), Vec::new(), page, next)
        }
        "count" => (Vec::new(), Vec::new(), Vec::new(), None),
        _ => unreachable!(),
    };

    let mut scan_telemetry = budget.telemetry(scan.files.len(), bytes_scanned);
    if let Some(object) = scan_telemetry.as_object_mut() {
        object.insert("skipped_entries".into(), json!(scan.skipped_entries));
        object.insert("skipped_files".into(), json!(skipped_files));
        object.insert("ignore_rules".into(), json!("project_and_default"));
    }
    Ok(tool_ok(json!({
        "query": query,
        "output_mode": output_mode,
        "matches": matches,
        "files": files,
        "summary": summary,
        "total_matches": total,
        "matched_files": matched_files,
        "cursor": cursor,
        "next_cursor": next_cursor,
        "truncated": next_cursor.is_some(),
        "scan": scan_telemetry,
        "warnings": if next_cursor.is_some() { vec!["result page limit reached; continue with next_cursor"] } else { vec![] }
    })))
}

fn build_matcher(
    query: &str,
    use_regex: bool,
    case_sensitive: bool,
) -> Result<Matcher, WorkspaceError> {
    if use_regex {
        let pattern = if case_sensitive {
            Regex::new(query)
        } else {
            Regex::new(&format!("(?i:{query})"))
        }
        .map_err(|e| WorkspaceError::invalid_argument(format!("Invalid regex: {e}")))?;
        Ok(Matcher::Regex(pattern))
    } else if case_sensitive {
        Ok(Matcher::Literal(query.to_string()))
    } else {
        let pattern = RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()
            .map_err(|e| WorkspaceError::invalid_argument(format!("Invalid query: {e}")))?;
        Ok(Matcher::Regex(pattern))
    }
}

enum Matcher {
    Regex(Regex),
    Literal(String),
}

impl Matcher {
    fn find(&self, line: &str) -> Option<usize> {
        match self {
            Matcher::Regex(re) => re.find(line).map(|matched| matched.start()),
            Matcher::Literal(lit) => line.find(lit),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_dir_entries(
    ws: &Workspace,
    dir: &Path,
    display: &str,
    depth: usize,
    max_depth: usize,
    recursive: bool,
    include_hidden: bool,
    include_ignored: bool,
    max_entries: usize,
    entries: &mut Vec<Value>,
    truncated: &mut bool,
    cancellation: &CancellationToken,
) -> Result<(), WorkspaceError> {
    ensure_not_cancelled(cancellation)?;
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for item in read_dir.flatten() {
        ensure_not_cancelled(cancellation)?;
        if *truncated {
            return Ok(());
        }
        let p = item.path();
        if ws.is_ignored_path(&p, include_hidden, include_ignored) {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        let rel = if display == "." {
            name.clone()
        } else {
            format!("{display}/{name}")
        };
        let ft = item.file_type().ok();
        let entry_type = if ft.as_ref().map(|t| t.is_symlink()).unwrap_or(false) {
            "symlink"
        } else if ft.as_ref().map(|t| t.is_dir()).unwrap_or(false) {
            "directory"
        } else if ft.as_ref().map(|t| t.is_file()).unwrap_or(false) {
            "file"
        } else {
            "other"
        };
        let meta = item.metadata().ok();
        let is_hidden = ws.is_hidden_path(&p);
        let is_ignored = ws.is_default_ignored_path(&p);
        entries.push(json!({
            "name": name,
            "path": rel.replace('\\', "/"),
            "type": entry_type,
            "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            "modified": meta.and_then(|m| format_mtime(m.modified().ok())),
            "is_hidden": is_hidden,
            "is_ignored": is_ignored
        }));
        if entries.len() >= max_entries {
            *truncated = true;
            return Ok(());
        }
        if recursive && depth < max_depth && entry_type == "directory" && !p.is_symlink() {
            collect_dir_entries(
                ws,
                &p,
                &rel.replace('\\', "/"),
                depth + 1,
                max_depth,
                recursive,
                include_hidden,
                include_ignored,
                max_entries,
                entries,
                truncated,
                cancellation,
            )?;
        }
    }
    Ok(())
}

fn truncate_bytes(text: &str, max_bytes: usize) -> (String, bool, Option<&'static str>) {
    let bytes = text.as_bytes();
    if bytes.len() <= max_bytes {
        return (text.to_string(), false, None);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true, Some("bytes"))
}

fn string_list_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn list_files_patterns(args: &Value) -> Vec<String> {
    let patterns = string_list_arg(args, "patterns");
    if !patterns.is_empty() {
        return patterns;
    }
    vec!["**/*".to_string()]
}

fn search_globs(args: &Value) -> (Vec<String>, Vec<String>) {
    (
        string_list_arg(args, "include_globs"),
        string_list_arg(args, "exclude_globs"),
    )
}

fn passes_glob_filters(rel: &str, include: &[String], exclude: &[String]) -> bool {
    if !include.is_empty() && !include.iter().any(|pat| glob_match(pat, rel)) {
        return false;
    }
    !exclude.iter().any(|pat| glob_match(pat, rel))
}

fn glob_match(pattern: &str, path: &str) -> bool {
    let pat = pattern.replace('\\', "/");
    let p = path.replace('\\', "/");
    if pat == "**/*" || pat == "*" {
        return true;
    }
    if let Some(suffix) = pat.strip_prefix("**/") {
        return simple_glob(suffix, &p) || p.split('/').any(|part| simple_glob(suffix, part));
    }
    simple_glob(&pat, &p)
}

fn simple_glob(pattern: &str, text: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| p.matches(text))
        .unwrap_or(false)
}

fn format_mtime(st: Option<SystemTime>) -> Option<String> {
    st.map(|t| {
        let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{}.{:03}Z", d.as_secs(), d.subsec_millis())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reads_and_searches_bom_marked_utf16_text() {
        let root = tempdir().expect("workspace");
        let text = "第一行\n包含 needle 的第二行\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(root.path().join("utf16.txt"), bytes).expect("utf16 file");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let cancellation = CancellationToken::default();

        let read =
            read_file(&workspace, &json!({"path": "utf16.txt"}), &cancellation).expect("read");
        assert_eq!(read["encoding"], "utf-16le");
        assert_eq!(read["content"], text);

        let searched = search_text(
            &workspace,
            &json!({"path": ".", "query": "needle"}),
            &cancellation,
        )
        .expect("search");
        assert_eq!(searched["total_matches"], 1);
        assert_eq!(searched["matches"][0]["path"], "utf16.txt");
    }

    #[test]
    fn list_dir_reports_hidden_and_default_ignored_entries() {
        let root = tempdir().expect("workspace");
        std::fs::write(root.path().join(".hidden"), "hidden").expect("hidden");
        std::fs::create_dir_all(root.path().join("node_modules")).expect("ignored");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");

        let listed = list_dir(
            &workspace,
            &json!({"path": ".", "include_hidden": true, "include_ignored": true}),
            &CancellationToken::default(),
        )
        .expect("list");
        let entries = listed["entries"].as_array().expect("entries");
        let hidden = entries
            .iter()
            .find(|entry| entry["name"] == ".hidden")
            .expect("hidden entry");
        let ignored = entries
            .iter()
            .find(|entry| entry["name"] == "node_modules")
            .expect("ignored entry");
        assert_eq!(hidden["is_hidden"], true);
        assert_eq!(ignored["is_ignored"], true);
    }

    #[test]
    fn list_files_respects_project_ignore_rules_and_stable_paging() {
        let root = tempdir().expect("workspace");
        std::fs::write(root.path().join(".gitignore"), "ignored/\n").expect("ignore file");
        std::fs::create_dir_all(root.path().join("ignored")).expect("ignored dir");
        std::fs::write(root.path().join("ignored/hidden.txt"), "hidden").expect("ignored file");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.path().join(name), name).expect("fixture file");
        }
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let cancellation = CancellationToken::default();

        let first = list_files(
            &workspace,
            &json!({"path": ".", "patterns": ["*.txt"], "max_results": 2}),
            &cancellation,
        )
        .expect("first page");
        assert_eq!(first["total_files"], 3);
        assert_eq!(first["next_cursor"], 2);
        assert_eq!(first["files"][0]["path"], "a.txt");
        assert_eq!(first["files"][1]["path"], "b.txt");

        let second = list_files(
            &workspace,
            &json!({"path": ".", "patterns": ["*.txt"], "max_results": 2, "cursor": 2}),
            &cancellation,
        )
        .expect("second page");
        assert_eq!(second["files"].as_array().expect("files").len(), 1);
        assert_eq!(second["files"][0]["path"], "c.txt");
        assert!(second["next_cursor"].is_null());

        let including_ignored = list_files(
            &workspace,
            &json!({"path": ".", "patterns": ["*.txt"], "include_ignored": true}),
            &cancellation,
        )
        .expect("include ignored");
        assert_eq!(including_ignored["total_files"], 4);
    }

    #[test]
    fn subdirectory_scan_inherits_workspace_root_ignore_without_parent_escape() {
        let root = tempdir().expect("workspace");
        std::fs::write(root.path().join(".gitignore"), "sub/ignored.txt\n").expect("ignore file");
        std::fs::create_dir_all(root.path().join("sub")).expect("subdir");
        std::fs::write(root.path().join("sub/ignored.txt"), "ignored").expect("ignored file");
        std::fs::write(root.path().join("sub/kept.txt"), "kept").expect("kept file");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");

        let listed = list_files(
            &workspace,
            &json!({"path": "sub", "patterns": ["*.txt"]}),
            &CancellationToken::default(),
        )
        .expect("subdirectory scan");
        assert_eq!(listed["total_files"], 1);
        assert_eq!(listed["files"][0]["path"], "sub/kept.txt");
    }

    #[test]
    fn repository_scan_file_budget_fails_before_returning_an_unstable_partial_page() {
        let root = tempdir().expect("workspace");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.path().join(name), name).expect("fixture file");
        }
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let error = list_files(
            &workspace,
            &json!({"path": ".", "max_scan_files": 2, "max_results": 1}),
            &CancellationToken::default(),
        )
        .expect_err("hard scan budget must reject the whole scan");
        assert!(matches!(
            error,
            WorkspaceError::ToolDetails {
                code: "REPOSITORY_SCAN_BUDGET_EXCEEDED",
                ..
            }
        ));
    }

    #[test]
    fn search_byte_budget_is_checked_before_reading_an_oversized_candidate() {
        let root = tempdir().expect("workspace");
        std::fs::write(root.path().join("large.txt"), "needle and more data\n").expect("fixture");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let error = search_text(
            &workspace,
            &json!({"query": "needle", "max_scan_bytes": 4}),
            &CancellationToken::default(),
        )
        .expect_err("byte budget must reject the whole scan");
        assert!(matches!(
            error,
            WorkspaceError::ToolDetails {
                code: "REPOSITORY_SCAN_BUDGET_EXCEEDED",
                ..
            }
        ));
    }

    #[test]
    fn search_text_reports_real_columns_and_compact_modes() {
        let root = tempdir().expect("workspace");
        std::fs::write(root.path().join("a.txt"), "prefix Needle suffix\n").expect("a");
        std::fs::write(root.path().join("b.txt"), "needle again\n").expect("b");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let cancellation = CancellationToken::default();

        let matches = search_text(
            &workspace,
            &json!({"query": "needle", "max_results": 1}),
            &cancellation,
        )
        .expect("matches");
        assert_eq!(matches["total_matches"], 2);
        assert_eq!(matches["matches"][0]["path"], "a.txt");
        assert_eq!(matches["matches"][0]["column"], 8);
        assert_eq!(matches["next_cursor"], 1);

        let files = search_text(
            &workspace,
            &json!({"query": "needle", "output_mode": "files"}),
            &cancellation,
        )
        .expect("files mode");
        assert_eq!(files["files"], json!(["a.txt", "b.txt"]));
        assert!(files["matches"].as_array().expect("matches").is_empty());

        let count = search_text(
            &workspace,
            &json!({"query": "needle", "output_mode": "count"}),
            &cancellation,
        )
        .expect("count mode");
        assert_eq!(count["total_matches"], 2);
        assert_eq!(count["matched_files"], 2);
        assert!(count["summary"].as_array().expect("summary").is_empty());
    }

    #[test]
    fn batch_read_uses_shared_budget_and_exact_continuation() {
        let root = tempdir().expect("workspace");
        std::fs::write(root.path().join("unicode.txt"), "αβγ\nsecond\n").expect("unicode");
        std::fs::write(root.path().join("other.txt"), "other\n").expect("other");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let cancellation = CancellationToken::default();

        let first = read_file(
            &workspace,
            &json!({
                "files": [{"path": "unicode.txt"}, {"path": "other.txt"}],
                "max_bytes": 3
            }),
            &cancellation,
        )
        .expect("batch read");
        assert_eq!(first["mode"], "batch");
        assert_eq!(first["files"][0]["result"]["content"], "α");
        assert_eq!(first["next"]["files"][0]["path"], "unicode.txt");
        assert_eq!(first["next"]["files"][0]["start_line"], 1);
        assert_eq!(first["next"]["files"][0]["start_byte"], 2);
        assert_eq!(first["next"]["files"][1]["path"], "other.txt");

        let continuation = read_file(
            &workspace,
            &json!({
                "files": first["next"]["files"].clone(),
                "max_bytes": 64
            }),
            &cancellation,
        )
        .expect("continuation");
        assert_eq!(
            continuation["files"][0]["result"]["content"],
            "βγ\nsecond\n"
        );
        assert_eq!(continuation["files"][1]["result"]["content"], "other\n");
        assert!(continuation["next"].is_null());
    }

    #[test]
    fn batch_read_isolates_per_file_failures() {
        let root = tempdir().expect("workspace");
        std::fs::write(root.path().join("present.txt"), "present\n").expect("fixture");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");

        let result = read_file(
            &workspace,
            &json!({"files": [{"path": "missing.txt"}, {"path": "present.txt"}]}),
            &CancellationToken::default(),
        )
        .expect("batch result");
        assert_eq!(result["failed_files"], 1);
        assert_eq!(result["files"][0]["ok"], false);
        assert_eq!(result["files"][1]["ok"], true);
        assert_eq!(result["files"][1]["result"]["content"], "present\n");
    }
}
