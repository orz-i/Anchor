use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::harness::model::BaselineEntry;
use crate::tools::workspace::{tool_ok, WorkspaceError};
use serde_json::{json, Value};

const SESSION_BUFFER_BYTES: usize = 1_048_576;
const DEFAULT_MAX_EXEC_SESSIONS: usize = 64;
const DEFAULT_TERMINAL_RETENTION: Duration = Duration::from_secs(30 * 60);

pub struct SessionStore {
    sessions: Mutex<HashMap<String, Arc<ExecSession>>>,
    max_sessions: usize,
    terminal_retention: Duration,
}

struct StreamDecoder {
    encoding: StreamEncoding,
    pending: Vec<u8>,
}

impl StreamDecoder {
    fn new(encoding: StreamEncoding) -> Self {
        Self {
            encoding,
            pending: Vec::new(),
        }
    }

    fn decode(&mut self, chunk: &[u8], finish: bool) -> Vec<u8> {
        match self.encoding {
            StreamEncoding::Utf8 => chunk.to_vec(),
            #[cfg(windows)]
            StreamEncoding::WindowsOem => {
                self.pending.extend_from_slice(chunk);
                if !finish
                    && self
                        .pending
                        .last()
                        .is_some_and(|byte| windows_oem_is_lead_byte(*byte))
                {
                    let lead = self.pending.pop().expect("pending lead byte");
                    let decoded = decode_windows_oem(&self.pending).into_bytes();
                    self.pending.clear();
                    self.pending.push(lead);
                    decoded
                } else {
                    let decoded = decode_windows_oem(&self.pending).into_bytes();
                    self.pending.clear();
                    decoded
                }
            }
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetOEMCP() -> u32;
    fn IsDBCSLeadByteEx(code_page: u32, test_char: u8) -> i32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        source: *const u8,
        source_len: i32,
        destination: *mut u16,
        destination_len: i32,
    ) -> i32;
}

#[cfg(windows)]
fn windows_oem_is_lead_byte(byte: u8) -> bool {
    unsafe { IsDBCSLeadByteEx(GetOEMCP(), byte) != 0 }
}

#[cfg(windows)]
fn decode_windows_oem(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let Ok(source_len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    let code_page = unsafe { GetOEMCP() };
    let required = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            source_len,
            std::ptr::null_mut(),
            0,
        )
    };
    if required <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; required as usize];
    let written = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            source_len,
            wide.as_mut_ptr(),
            required,
        )
    };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf16_lossy(&wide[..written as usize])
}

#[derive(Debug, Clone, Copy, Default)]
pub enum StreamEncoding {
    #[default]
    Utf8,
    #[cfg(windows)]
    WindowsOem,
}

pub fn finalize_execution_result(mut value: Value) -> Value {
    let transport_ok = value
        .get("transport_ok")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let command_ok = value.get("command_ok").and_then(Value::as_bool);
    let reason = value
        .get("termination_reason")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if value.get("status").and_then(Value::as_str) == Some("running") {
                "running"
            } else {
                "exited"
            }
        })
        .to_string();
    let execution_status = value
        .get("execution_status")
        .and_then(Value::as_str)
        .unwrap_or(match reason.as_str() {
            "running" => "running",
            "exited" if command_ok == Some(true) => "succeeded",
            "exited" => "failed",
            "cancelled" => "cancelled",
            "timeout" => "timed_out",
            "killed" => "killed",
            "spawn_failed" => "spawn_failed",
            "command_rejected" => "rejected",
            "server_restart" => "interrupted",
            _ => "failed",
        })
        .to_string();
    let retryable = value
        .get("retryable")
        .and_then(Value::as_bool)
        .or_else(|| value.get("recoverable").and_then(Value::as_bool))
        .unwrap_or(matches!(
            reason.as_str(),
            "timeout" | "killed" | "spawn_failed" | "server_restart"
        ));

    if let Some(object) = value.as_object_mut() {
        object.entry("session_id").or_insert(Value::Null);
        object.entry("exit_code").or_insert(Value::Null);
        object.insert(
            "transport_status".into(),
            Value::String(if transport_ok { "ok" } else { "error" }.into()),
        );
        object.insert(
            "execution_status".into(),
            Value::String(execution_status.clone()),
        );
        object.insert(
            "success".into(),
            command_ok.map(Value::Bool).unwrap_or(Value::Null),
        );
        object.insert("retryable".into(), Value::Bool(retryable));
        if command_ok == Some(false) || !transport_ok {
            object.insert("ok".into(), Value::Bool(false));
            let exit_code = object.get("exit_code").cloned().unwrap_or(Value::Null);
            let session_id = object.get("session_id").cloned().unwrap_or(Value::Null);
            let error_code = match reason.as_str() {
                "timeout" => "COMMAND_TIMEOUT",
                "killed" => "COMMAND_KILLED",
                "cancelled" => "COMMAND_CANCELLED",
                "spawn_failed" => "COMMAND_SPAWN_FAILED",
                "command_rejected" => "COMMAND_REJECTED",
                _ if !transport_ok => "EXECUTION_TRANSPORT_FAILED",
                _ => "COMMAND_EXIT_NONZERO",
            };
            let summary = match object.get("summary").and_then(Value::as_str) {
                Some(summary) if !summary.trim().is_empty() => summary.to_string(),
                _ => format!("Command execution failed ({execution_status})"),
            };
            object.insert("summary".into(), Value::String(summary.clone()));
            object.entry("status").or_insert_with(|| json!("error"));
            object.entry("error").or_insert_with(|| {
                json!({
                    "code": error_code,
                    "message": summary,
                    "category": "runtime",
                    "retryable": retryable,
                    "details": {
                        "termination_reason": reason,
                        "execution_status": execution_status,
                        "exit_code": exit_code,
                        "session_id": session_id
                    }
                })
            });
        } else {
            object.entry("ok").or_insert(Value::Bool(true));
        }
    }
    value
}

pub fn list_command_sessions(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let include_terminal = args
        .get("include_terminal")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let max_output_bytes = args
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(4_096)
        .clamp(0, 65_536) as usize;
    let sessions = store.list_snapshots(include_terminal, max_output_bytes);
    let running_count = sessions
        .iter()
        .filter(|session| session.get("execution_status") == Some(&json!("running")))
        .count();
    let session_count = sessions.len();
    let terminal_count = session_count.saturating_sub(running_count);
    Ok(tool_ok(json!({
        "sessions": sessions,
        "session_count": session_count,
        "running_count": running_count,
        "terminal_count": terminal_count,
        "process_bound": true,
        "warnings": []
    })))
}

pub fn wait_command(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    let session = store.get(session_id)?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(60_000);
    let stdout_offset = args
        .get("stdout_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let stderr_offset = args
        .get("stderr_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(65_536)
        .clamp(1, 1_048_576) as usize;
    let return_incremental = args
        .get("return_incremental_output")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let stop_patterns = args
        .get("stop_on_patterns")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .take(16)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let matched_pattern = crate::async_runtime::block_on(async {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            session.refresh_status().await;
            let stdout = session.retained_stream_bytes("stdout").0;
            let stderr = session.retained_stream_bytes("stderr").0;
            if let Some(pattern) = stop_patterns.iter().find(|pattern| {
                String::from_utf8_lossy(&stdout).contains(pattern.as_str())
                    || String::from_utf8_lossy(&stderr).contains(pattern.as_str())
            }) {
                break Some(pattern.clone());
            }
            if session.has_exited() || Instant::now() >= deadline {
                break None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    if session.has_exited() {
        crate::async_runtime::block_on(session.wait_for_readers());
    }
    let snapshot = session.snapshot(limit);
    let termination_reason = snapshot
        .get("termination_reason")
        .and_then(Value::as_str)
        .unwrap_or("running");
    let exit_code = snapshot.get("exit_code").and_then(Value::as_i64);
    let state = match termination_reason {
        "running" => "running",
        "exited" if exit_code == Some(0) => "completed",
        "exited" => "failed",
        "cancelled" | "killed" => "cancelled",
        _ => "failed",
    };
    let stdout = incremental_stream(&session, "stdout", stdout_offset, limit, return_incremental);
    let stderr = incremental_stream(&session, "stderr", stderr_offset, limit, return_incremental);
    let output_refs = json!({
        "stdout": format!("session:{session_id}:stdout"),
        "stderr": format!("session:{session_id}:stderr")
    });

    Ok(finalize_execution_result(json!({
        "session_id": session_id,
        "state": state,
        "status": snapshot["status"],
        "termination_reason": termination_reason,
        "exit_code": snapshot["exit_code"],
        "command_ok": snapshot["command_ok"],
        "transport_status": snapshot["transport_status"],
        "execution_status": snapshot["execution_status"],
        "success": snapshot["success"],
        "retryable": snapshot["retryable"],
        "started_at": snapshot["started_at"],
        "elapsed_ms": snapshot["elapsed_ms"],
        "last_output_at": snapshot["last_output_at"],
        "stdin_open": snapshot["stdin_open"],
        "stdout": stdout,
        "stderr": stderr,
        "stdout_complete": stdout.get("complete").and_then(Value::as_bool).unwrap_or(false),
        "stderr_complete": stderr.get("complete").and_then(Value::as_bool).unwrap_or(false),
        "output_refs": output_refs,
        "stop_pattern_matched": matched_pattern,
        "wait_timeout_ms": timeout_ms,
        "warnings": []
    })))
}

fn incremental_stream(
    session: &ExecSession,
    stream: &str,
    offset: usize,
    limit: usize,
    include_content: bool,
) -> Value {
    let (data, total_stream_bytes) = session.retained_stream_bytes(stream);
    let page = page_retained_output(&data, total_stream_bytes, offset, limit);
    let complete =
        session.has_exited() && !page.evicted_before_offset && page.next_offset.is_none();
    json!({
        "output_ref": format!("session:{}:{stream}", session.session_id),
        "offset": page.effective_offset,
        "requested_offset": offset,
        "retained_start_offset": page.retained_start_offset,
        "content": if include_content { String::from_utf8_lossy(page.content).into_owned() } else { String::new() },
        "next_offset": page.next_offset.unwrap_or(total_stream_bytes as u64),
        "total_stream_bytes": total_stream_bytes,
        "truncated": page.evicted_before_offset || page.next_offset.is_some(),
        "complete": complete
    })
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_EXEC_SESSIONS, DEFAULT_TERMINAL_RETENTION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_result_exposes_unambiguous_failure_contract() {
        let result = finalize_execution_result(json!({
            "status": "exited",
            "termination_reason": "exited",
            "exit_code": 7,
            "transport_ok": true,
            "command_ok": false
        }));

        assert_eq!(result["ok"], false);
        assert_eq!(result["transport_status"], "ok");
        assert_eq!(result["execution_status"], "failed");
        assert_eq!(result["success"], false);
        assert_eq!(result["retryable"], false);
        assert_eq!(result["session_id"], Value::Null);
        assert_eq!(result["error"]["code"], "COMMAND_EXIT_NONZERO");
    }

    #[test]
    fn running_execution_keeps_nullable_success_and_session_identity() {
        let result = finalize_execution_result(json!({
            "session_id": "session-1",
            "status": "running",
            "termination_reason": "running",
            "exit_code": null,
            "transport_ok": true,
            "command_ok": null
        }));

        assert_eq!(result["ok"], true);
        assert_eq!(result["execution_status"], "running");
        assert_eq!(result["success"], Value::Null);
        assert_eq!(result["session_id"], "session-1");
    }

    #[test]
    fn retained_output_offsets_are_absolute_after_ring_buffer_eviction() {
        let retained = b"uvwxyz";
        let page = page_retained_output(retained, 26, 0, 3);
        assert_eq!(page.retained_start_offset, 20);
        assert_eq!(page.effective_offset, 20);
        assert_eq!(page.content, b"uvw");
        assert_eq!(page.next_offset, Some(23));
        assert!(page.evicted_before_offset);

        let next = page_retained_output(retained, 26, 23, 10);
        assert_eq!(next.effective_offset, 23);
        assert_eq!(next.content, b"xyz");
        assert_eq!(next.next_offset, None);
        assert!(!next.evicted_before_offset);
    }

    #[test]
    fn retained_output_offsets_clamp_past_the_end() {
        let page = page_retained_output(b"abc", 3, 99, 10);
        assert_eq!(page.effective_offset, 3);
        assert!(page.content.is_empty());
        assert_eq!(page.next_offset, None);
    }

    #[test]
    fn stream_buffer_keeps_total_and_retained_tail_consistent() {
        let mut buffer = StreamBuffer::default();
        buffer.append(b"abc", 4);
        buffer.append(b"def", 4);
        assert_eq!(buffer.total, 6);
        assert_eq!(buffer.data, b"cdef");
        let page = page_retained_output(&buffer.data, buffer.total, 0, 10);
        assert_eq!(page.retained_start_offset, 2);
        assert_eq!(page.content, b"cdef");
    }
}

fn page_retained_output(
    data: &[u8],
    total_stream_bytes: usize,
    requested_offset: usize,
    limit: usize,
) -> OutputPage<'_> {
    let retained_start_offset = total_stream_bytes.saturating_sub(data.len());
    let effective_offset = requested_offset.clamp(retained_start_offset, total_stream_bytes);
    let buffer_offset = effective_offset.saturating_sub(retained_start_offset);
    let content = &data[buffer_offset..data.len().min(buffer_offset + limit)];
    let next_absolute_offset = effective_offset + content.len();
    OutputPage {
        content,
        effective_offset,
        retained_start_offset,
        next_offset: (next_absolute_offset < total_stream_bytes)
            .then_some(next_absolute_offset as u64),
        evicted_before_offset: requested_offset < retained_start_offset,
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limits(max_sessions: usize, terminal_retention: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            max_sessions: max_sessions.max(1),
            terminal_retention,
        }
    }

    pub fn insert(&self, session: ExecSession) -> Result<Arc<ExecSession>, Box<ExecSession>> {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        prune_terminal_sessions(&mut sessions, self.terminal_retention);
        if sessions.len() >= self.max_sessions {
            return Err(Box::new(session));
        }
        let arc = Arc::new(session);
        sessions.insert(arc.session_id.clone(), arc.clone());
        Ok(arc)
    }

    pub fn get(&self, session_id: &str) -> Result<Arc<ExecSession>, WorkspaceError> {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        prune_terminal_sessions(&mut sessions, self.terminal_retention);
        let session = sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| WorkspaceError::Tool {
                code: "SESSION_NOT_FOUND",
                message: format!("Session not found: {session_id}"),
                category: "not_found",
                retryable: false,
            })?;
        session.touch();
        Ok(session)
    }

    pub fn list_snapshots(&self, include_terminal: bool, max_output_bytes: usize) -> Vec<Value> {
        let sessions = {
            let mut sessions = self.sessions.lock().expect("sessions lock");
            prune_terminal_sessions(&mut sessions, self.terminal_retention);
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut snapshots = sessions
            .into_iter()
            .filter_map(|session| {
                crate::async_runtime::block_on(session.refresh_status());
                if !include_terminal && session.has_exited() {
                    return None;
                }
                Some(session.snapshot(max_output_bytes))
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            right["started_at"]
                .as_str()
                .cmp(&left["started_at"].as_str())
        });
        snapshots
    }

    pub fn running_task_ids(&self) -> Vec<String> {
        let sessions = {
            let mut sessions = self.sessions.lock().expect("sessions lock");
            prune_terminal_sessions(&mut sessions, self.terminal_retention);
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut task_ids = sessions
            .into_iter()
            .filter_map(|session| {
                crate::async_runtime::block_on(session.refresh_status());
                (!session.has_exited())
                    .then(|| session.harness_metadata().map(|metadata| metadata.task_id))
                    .flatten()
            })
            .collect::<Vec<_>>();
        task_ids.sort_unstable();
        task_ids.dedup();
        task_ids
    }

    pub fn remove(&self, session_id: &str) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .remove(session_id);
    }

    pub fn capacity_error(&self) -> WorkspaceError {
        WorkspaceError::ToolDetails {
            code: "SESSION_LIMIT_REACHED",
            message: format!(
                "Too many retained command sessions; the limit is {}",
                self.max_sessions
            ),
            category: "runtime",
            retryable: true,
            details: json!({
                "max_sessions": self.max_sessions,
                "terminal_retention_ms": self.terminal_retention.as_millis(),
                "suggestion": "结束不再需要的运行会话，或等待已结束会话的保留期到期后重试"
            }),
        }
    }
}

fn prune_terminal_sessions(
    sessions: &mut HashMap<String, Arc<ExecSession>>,
    terminal_retention: Duration,
) {
    sessions.retain(|_, session| {
        !session.has_exited()
            || session
                .last_access_elapsed()
                .is_some_and(|elapsed| elapsed <= terminal_retention)
    });
}

pub struct ExecSession {
    pub session_id: String,
    pub(crate) child: AsyncMutex<Child>,
    pub stdin: AsyncMutex<Option<ChildStdin>>,
    stdin_open: Mutex<bool>,
    interactive: bool,
    command: String,
    resolved_cwd: String,
    output_encoding: StreamEncoding,
    stdout: Mutex<StreamBuffer>,
    stderr: Mutex<StreamBuffer>,
    pub started_at: Instant,
    started_at_iso: String,
    last_output_at: Mutex<String>,
    pub exit_code: Mutex<Option<i32>>,
    exited: AtomicBool,
    last_access: Mutex<Instant>,
    termination_reason: Mutex<Option<String>>,
    reader_tasks: AsyncMutex<Vec<crate::async_runtime::JoinHandle<()>>>,
    harness_metadata: Option<SessionHarnessMetadata>,
    harness_finalized: AtomicBool,
}

#[derive(Debug, Clone)]
pub struct SessionHarnessMetadata {
    pub task_id: String,
    pub command: String,
    pub verification_kind: Option<String>,
    pub verification_key: Option<String>,
    pub test_file: Option<String>,
    pub test_name: Option<String>,
    pub workspace_before: Vec<BaselineEntry>,
    pub verification_level: String,
    pub supersede_previous_failures: bool,
}

impl ExecSession {
    pub fn new(child: Child) -> Self {
        Self::new_with_mode(child, false)
    }

    pub fn new_with_mode(child: Child, interactive: bool) -> Self {
        Self::new_with_harness_metadata(child, interactive, None)
    }

    pub fn new_with_harness_metadata(
        child: Child,
        interactive: bool,
        harness_metadata: Option<SessionHarnessMetadata>,
    ) -> Self {
        Self::new_with_details(
            child,
            interactive,
            String::new(),
            String::new(),
            harness_metadata,
        )
    }

    pub fn new_with_details(
        child: Child,
        interactive: bool,
        command: String,
        resolved_cwd: String,
        harness_metadata: Option<SessionHarnessMetadata>,
    ) -> Self {
        Self::new_with_details_and_encoding(
            child,
            interactive,
            command,
            resolved_cwd,
            harness_metadata,
            StreamEncoding::Utf8,
        )
    }

    pub fn new_with_details_and_encoding(
        mut child: Child,
        interactive: bool,
        command: String,
        resolved_cwd: String,
        harness_metadata: Option<SessionHarnessMetadata>,
        output_encoding: StreamEncoding,
    ) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let stdin = child.stdin.take();
        let stdin_open = stdin.is_some();
        let started_at_iso = timestamp();
        Self {
            session_id,
            child: AsyncMutex::new(child),
            stdin: AsyncMutex::new(stdin),
            stdin_open: Mutex::new(stdin_open),
            interactive,
            command,
            resolved_cwd,
            output_encoding,
            stdout: Mutex::new(StreamBuffer::default()),
            stderr: Mutex::new(StreamBuffer::default()),
            started_at: Instant::now(),
            started_at_iso: started_at_iso.clone(),
            last_output_at: Mutex::new(started_at_iso),
            exit_code: Mutex::new(None),
            exited: AtomicBool::new(false),
            last_access: Mutex::new(Instant::now()),
            termination_reason: Mutex::new(None),
            reader_tasks: AsyncMutex::new(Vec::new()),
            harness_metadata,
            harness_finalized: AtomicBool::new(false),
        }
    }

    pub fn harness_metadata(&self) -> Option<SessionHarnessMetadata> {
        self.harness_metadata.clone()
    }

    pub fn mark_harness_finalized(&self) -> bool {
        self.harness_finalized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub async fn spawn_readers(self: &Arc<Self>) {
        let stdout = {
            let mut guard = self.child.lock().await;
            guard.stdout.take()
        };
        let stderr = {
            let mut guard = self.child.lock().await;
            guard.stderr.take()
        };
        if let Some(stream) = stdout {
            let session = Arc::clone(self);
            let task = crate::async_runtime::spawn(async move {
                session.read_stream(stream, true).await;
            });
            self.reader_tasks.lock().await.push(task);
        }
        if let Some(stream) = stderr {
            let session = Arc::clone(self);
            let task = crate::async_runtime::spawn(async move {
                session.read_stream(stream, false).await;
            });
            self.reader_tasks.lock().await.push(task);
        }
    }

    pub async fn wait_for_readers(&self) {
        let mut tasks = self.reader_tasks.lock().await;
        while let Some(task) = tasks.pop() {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), task).await;
        }
    }

    async fn read_stream<T>(&self, mut stream: T, is_stdout: bool)
    where
        T: tokio::io::AsyncRead + Unpin,
    {
        let mut buf = [0u8; 4096];
        let mut decoder = StreamDecoder::new(self.output_encoding);
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let decoded = decoder.decode(&buf[..n], false);
                    let chunk = decoded.as_slice();
                    if is_stdout {
                        self.stdout
                            .lock()
                            .expect("stdout lock")
                            .append(chunk, SESSION_BUFFER_BYTES);
                    } else {
                        self.stderr
                            .lock()
                            .expect("stderr lock")
                            .append(chunk, SESSION_BUFFER_BYTES);
                    }
                    *self.last_output_at.lock().expect("last output lock") = timestamp();
                }
                Err(_) => break,
            }
        }
        let tail = decoder.decode(&[], true);
        if !tail.is_empty() {
            if is_stdout {
                self.stdout
                    .lock()
                    .expect("stdout lock")
                    .append(&tail, SESSION_BUFFER_BYTES);
            } else {
                self.stderr
                    .lock()
                    .expect("stderr lock")
                    .append(&tail, SESSION_BUFFER_BYTES);
            }
            *self.last_output_at.lock().expect("last output lock") = timestamp();
        }
    }

    pub async fn kill_and_wait(&self) {
        let status = {
            let mut child = self.child.lock().await;
            let _ = child.start_kill();
            child.wait().await.ok()
        };
        if let Some(status) = status {
            self.record_exit_status(status);
        }
    }

    pub async fn refresh_status(&self) {
        let mut child = self.child.lock().await;
        if let Ok(Some(status)) = child.try_wait() {
            self.record_exit_status(status);
        }
    }

    fn record_exit_status(&self, status: std::process::ExitStatus) {
        *self.exit_code.lock().expect("exit_code lock") = status.code();
        self.exited.store(true, Ordering::Release);
        *self.stdin_open.lock().expect("stdin_open lock") = false;
        let mut reason = self.termination_reason.lock().expect("termination lock");
        if reason.is_none() {
            *reason = Some("exited".into());
        }
        self.touch();
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    fn touch(&self) {
        *self.last_access.lock().expect("last_access lock") = Instant::now();
    }

    fn last_access_elapsed(&self) -> Option<Duration> {
        self.has_exited()
            .then(|| self.last_access.lock().expect("last_access lock").elapsed())
    }

    pub fn mark_termination_reason(&self, reason: &str) {
        *self.termination_reason.lock().expect("termination lock") = Some(reason.to_string());
    }

    pub(crate) fn mark_stdin_closed(&self) {
        *self.stdin_open.lock().expect("stdin_open lock") = false;
    }

    pub async fn is_running(&self) -> bool {
        self.refresh_status().await;
        !self.has_exited()
    }

    pub fn retained_stream_bytes(&self, stream: &str) -> (Vec<u8>, usize) {
        match stream {
            "stderr" => {
                let buffer = self.stderr.lock().expect("stderr lock");
                (buffer.data.clone(), buffer.total)
            }
            _ => {
                let buffer = self.stdout.lock().expect("stdout lock");
                (buffer.data.clone(), buffer.total)
            }
        }
    }

    pub fn snapshot(&self, max_output_bytes: usize) -> Value {
        let stdout_bytes = self.stdout.lock().expect("stdout lock").data.clone();
        let stderr_bytes = self.stderr.lock().expect("stderr lock").data.clone();
        let stdout = truncate_tail(&stdout_bytes, max_output_bytes);
        let stderr = truncate_tail(&stderr_bytes, max_output_bytes);
        let exit_code = *self.exit_code.lock().expect("exit_code lock");
        let termination_reason = self
            .termination_reason
            .lock()
            .expect("termination lock")
            .clone();
        let status = if self.has_exited() {
            "exited"
        } else {
            "running"
        };
        let reason = termination_reason.as_deref().unwrap_or("running");
        let command_ok = match reason {
            "exited" => Some(exit_code.is_some_and(|code| code == 0)),
            "running" => None,
            _ => Some(false),
        };
        let execution_status = match reason {
            "running" => "running",
            "exited" if command_ok == Some(true) => "succeeded",
            "exited" => "failed",
            "cancelled" => "cancelled",
            "timeout" => "timed_out",
            "killed" => "killed",
            "spawn_failed" => "spawn_failed",
            "server_restart" => "interrupted",
            _ => "failed",
        };
        let retryable = matches!(
            reason,
            "timeout" | "killed" | "spawn_failed" | "server_restart"
        );
        json!({
            "session_id": self.session_id,
            "command": self.command,
            "resolved_cwd": self.resolved_cwd,
            "interactive": self.interactive,
            "stdin_open": *self.stdin_open.lock().expect("stdin_open lock"),
            "status": status,
            "termination_reason": reason,
            "recoverable": retryable,
            "transport_status": "ok",
            "execution_status": execution_status,
            "success": command_ok,
            "retryable": retryable,
            "suggestion": match reason {
                "timeout" => "读取保留输出，调整 timeout_ms 后重试",
                "killed" => "确认终止原因后重新执行命令",
                "exited" => "检查 exit_code 和 stderr",
                "crashed" => "检查 stderr 后重试或恢复工作区",
                _ => "继续读取 session 或等待进程结束",
            },
            "exit_code": exit_code,
            "transport_ok": true,
            "command_ok": command_ok,
            "stdout": stdout.content,
            "stderr": stderr.content,
            "stdout_truncated": stdout.truncated,
            "stderr_truncated": stderr.truncated,
            "stdout_complete": self.has_exited() && !stdout.truncated,
            "stderr_complete": self.has_exited() && !stderr.truncated,
            "elapsed_ms": self.started_at.elapsed().as_millis(),
            "started_at": self.started_at_iso,
            "last_output_at": self.last_output_at.lock().expect("last output lock").clone(),
            "output_refs": {
                "stdout": format!("session:{}:stdout", self.session_id),
                "stderr": format!("session:{}:stderr", self.session_id)
            }
        })
    }
}

#[derive(Default)]
struct StreamBuffer {
    data: Vec<u8>,
    total: usize,
}

impl StreamBuffer {
    fn append(&mut self, chunk: &[u8], limit: usize) {
        self.data.extend_from_slice(chunk);
        self.total = self.total.saturating_add(chunk.len());
        if self.data.len() > limit {
            let drop = self.data.len() - limit;
            self.data.drain(..drop);
        }
    }
}

struct Truncated {
    content: String,
    truncated: bool,
}

struct OutputPage<'a> {
    content: &'a [u8],
    effective_offset: usize,
    retained_start_offset: usize,
    next_offset: Option<u64>,
    evicted_before_offset: bool,
}

fn truncate_tail(bytes: &[u8], max_bytes: usize) -> Truncated {
    let truncated = bytes.len() > max_bytes;
    let take = bytes.len().min(max_bytes);
    Truncated {
        content: String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(take)..]).into_owned(),
        truncated,
    }
}

pub fn read_output(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let output_ref = args
        .get("output_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("output_ref is required"))?;
    let parts: Vec<&str> = output_ref.split(':').collect();
    if parts.len() != 3 || parts[0] != "session" {
        return Err(WorkspaceError::invalid_argument(
            "output_ref must look like session:<id>:stdout or session:<id>:stderr",
        ));
    }
    let session_id = parts[1];
    let ref_stream = parts[2];
    if ref_stream != "stdout" && ref_stream != "stderr" {
        return Err(WorkspaceError::invalid_argument(
            "output_ref stream must be stdout or stderr",
        ));
    }
    let session = store.get(session_id)?;
    crate::async_runtime::block_on(session.refresh_status());

    let stream = ref_stream;

    let (data, total_stream_bytes) = session.retained_stream_bytes(stream);
    let requested_offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(4096)
        .clamp(1, 1_048_576) as usize;
    let page = page_retained_output(&data, total_stream_bytes, requested_offset, limit);
    let mut warnings = Vec::<&str>::new();
    if page.evicted_before_offset {
        warnings.push(
            "requested offset is no longer retained; response starts at retained_start_offset",
        );
    }
    Ok(tool_ok(json!({
        "output_ref": output_ref,
        "stream_output_ref": format!("session:{session_id}:{stream}"),
        "stream": stream,
        "offset": page.effective_offset,
        "requested_offset": requested_offset,
        "retained_start_offset": page.retained_start_offset,
        "limit": limit,
        "content": String::from_utf8_lossy(page.content),
        "next_offset": page.next_offset,
        "total_retained_bytes": data.len(),
        "total_stream_bytes": total_stream_bytes,
        "truncated": page.evicted_before_offset || page.next_offset.is_some(),
        "warnings": warnings
    })))
}

pub fn write_stdin(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    let session = store.get(session_id)?;
    let chars = args.get("chars").and_then(Value::as_str).unwrap_or("");
    let max_output_bytes = args
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65_536) as usize;

    let running = crate::async_runtime::block_on(session.is_running());
    if !running {
        if !chars.is_empty() {
            return Err(WorkspaceError::Tool {
                code: "SESSION_CLOSED",
                message: "Session is closed; stdin write blocked.".into(),
                category: "runtime",
                retryable: false,
            });
        }
        return Ok(finalize_execution_result(
            session.snapshot(max_output_bytes),
        ));
    }

    if !chars.is_empty() {
        let mut stdin_guard = crate::async_runtime::block_on(session.stdin.lock());
        let stdin = stdin_guard.as_mut().ok_or_else(|| WorkspaceError::Tool {
            code: "SESSION_CLOSED",
            message: "Session stdin is closed.".into(),
            category: "runtime",
            retryable: false,
        })?;
        use tokio::io::AsyncWriteExt;
        crate::async_runtime::block_on(async {
            stdin
                .write_all(chars.as_bytes())
                .await
                .map_err(|_| WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: "Session stdin is closed.".into(),
                    category: "runtime",
                    retryable: false,
                })
        })?;
        let _ = crate::async_runtime::block_on(stdin.flush());
    }

    let yield_ms = args
        .get("yield_time_ms")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .min(30_000);
    std::thread::sleep(std::time::Duration::from_millis(yield_ms));
    crate::async_runtime::block_on(session.refresh_status());
    Ok(finalize_execution_result(
        session.snapshot(max_output_bytes),
    ))
}

pub fn kill_session(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    let session = store.get(session_id)?;
    let max_output_bytes = args
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65_536) as usize;
    let wait_ms = args
        .get("wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5000)
        .min(30_000);
    let signal = args.get("signal").and_then(Value::as_str).unwrap_or("TERM");

    let running = crate::async_runtime::block_on(session.is_running());
    let mut killed = false;
    let mut status = "exited";
    let mut evicted = true;

    if running {
        session.mark_termination_reason("killed");
        crate::async_runtime::block_on(async {
            let pid = {
                let child = session.child.lock().await;
                child.id()
            };
            if let Some(pid) = pid {
                send_session_signal(pid, signal);
            } else {
                let mut child = session.child.lock().await;
                let _ = child.start_kill();
            }
            let _ = tokio::time::timeout(std::time::Duration::from_millis(wait_ms), async {
                let mut child = session.child.lock().await;
                let _ = child.wait().await;
            })
            .await;
        });
        crate::async_runtime::block_on(session.refresh_status());
        if crate::async_runtime::block_on(session.is_running()) {
            status = "terminating";
            evicted = false;
        } else {
            killed = true;
            status = "killed";
        }
    }

    let mut payload = session.snapshot(max_output_bytes);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("killed".into(), json!(killed));
        obj.insert("status".into(), json!(status));
        obj.insert("evicted".into(), json!(evicted));
        if status == "terminating" {
            obj.insert(
                "warnings".into(),
                json!(["Process did not exit after kill; session retained for retry"]),
            );
        }
    }

    if evicted {
        store.remove(session_id);
    }

    Ok(finalize_execution_result(payload))
}

#[cfg(unix)]
fn send_session_signal(pid: u32, signal: &str) {
    let sig = match signal {
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        _ => libc::SIGTERM,
    };
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

#[cfg(windows)]
fn send_session_signal(pid: u32, _signal: &str) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(handle, 1);
            let _ = CloseHandle(handle);
        }
    }
}
