use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::harness::model::BaselineEntry;
use crate::tools::resource_budget::ExecutionLease;
use crate::tools::workspace::{tool_ok, WorkspaceError};
use serde_json::{json, Value};

const SESSION_BUFFER_BYTES: usize = 1_048_576;
const DEFAULT_MAX_EXEC_SESSIONS: usize = 64;
// Once a terminal result has been explicitly consumed it must stop counting
// against command concurrency immediately. The session can still remain in
// the store for terminal_log_retention so output refs stay readable.
const DEFAULT_TERMINAL_SLOT_RETENTION: Duration = Duration::ZERO;
const DEFAULT_TERMINAL_LOG_RETENTION: Duration = Duration::from_secs(30 * 60);
const DURABLE_COMMAND_SCHEMA_VERSION: u32 = 1;
const TERMINAL_RECONCILIATION_GRACE: Duration = Duration::from_millis(250);
const TERMINAL_RECONCILIATION_POLL: Duration = Duration::from_millis(10);
const DURABLE_LOG_MAX_BYTES: u64 = 16 * 1024 * 1024;
const DURABLE_LOG_RETAIN_BYTES: u64 = 8 * 1024 * 1024;
const DURABLE_JSON_MAX_BYTES: u64 = 4 * 1024 * 1024;

pub struct CommandSessionStore {
    sessions: Mutex<HashMap<String, Arc<ExecSession>>>,
    execution_admission: Mutex<()>,
    max_sessions: usize,
    terminal_slot_retention: Duration,
    terminal_log_retention: Duration,
    durable_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct DurableJobPaths {
    dir: PathBuf,
    spec: PathBuf,
    state: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
    control: PathBuf,
    spawned: PathBuf,
    observed: PathBuf,
    harness_finalized: PathBuf,
}

impl DurableJobPaths {
    fn new(root: &Path, session_id: &str) -> Self {
        let dir = root.join(session_id);
        Self {
            spec: dir.join("spec.json"),
            state: dir.join("state.json"),
            stdout: dir.join("stdout.log"),
            stderr: dir.join("stderr.log"),
            control: dir.join("control"),
            spawned: dir.join("spawned.marker"),
            observed: dir.join("observed.marker"),
            harness_finalized: dir.join("harness-finalized.marker"),
            dir,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DurableCommandSpec {
    pub(crate) schema_version: u32,
    pub(crate) session_id: String,
    pub(crate) command: String,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: String,
    pub(crate) timeout_ms: u64,
    pub(crate) initial_stdin: String,
    pub(crate) output_encoding: StreamEncoding,
    pub(crate) expected_exit_codes: Vec<i32>,
    pub(crate) harness_metadata: Option<SessionHarnessMetadata>,
    pub(crate) owner_scope: Option<String>,
    pub(crate) transport_session_id: Option<String>,
    pub(crate) execution_resources: Option<Value>,
    pub(crate) started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableCommandState {
    schema_version: u32,
    session_id: String,
    status: String,
    termination_reason: String,
    supervisor_pid: Option<u32>,
    child_pid: Option<u32>,
    exit_code: Option<i32>,
    stdin_open: bool,
    started_at: String,
    finished_at: Option<String>,
    last_output_at: String,
    stdout_total_bytes: u64,
    stderr_total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableControl {
    action: String,
    chars: Option<String>,
    signal: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DurableJobHandle {
    paths: DurableJobPaths,
    spec: DurableCommandSpec,
}

impl DurableJobHandle {
    pub(crate) fn spec_path(&self) -> &Path {
        &self.paths.spec
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.spec.session_id
    }

    pub(crate) fn mark_launcher_failed(&self, message: &str) {
        if let Ok(mut state) = self.read_state() {
            state.status = "spawn_failed".into();
            state.termination_reason = "spawn_failed".into();
            state.stdin_open = false;
            state.finished_at = Some(timestamp());
            persist_durable_state(&self.paths, &state);
        }
        let _ = append_bounded_log(&self.paths.stderr, message.as_bytes());
    }

    fn read_state(&self) -> Result<DurableCommandState, WorkspaceError> {
        read_bounded_json(&self.paths.state).map_err(|error| WorkspaceError::ToolDetails {
            code: "DURABLE_COMMAND_STATE_UNAVAILABLE",
            message: format!("Durable command state is unavailable: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({
                "session_id": self.spec.session_id,
                "state_path": self.paths.state.display().to_string()
            }),
        })
    }

    fn terminal_observed(&self) -> bool {
        self.paths.observed.is_file()
    }

    fn mark_terminal_observed(&self) {
        let _ = create_marker(&self.paths.observed);
    }

    fn mark_harness_finalized(&self) -> bool {
        create_marker(&self.paths.harness_finalized).unwrap_or(false)
    }

    fn retained_stream_bytes(&self, stream: &str) -> (Vec<u8>, usize) {
        let state = self.read_state().ok();
        let (path, total) = if stream == "stderr" {
            (
                &self.paths.stderr,
                state.as_ref().map(|state| state.stderr_total_bytes),
            )
        } else {
            (
                &self.paths.stdout,
                state.as_ref().map(|state| state.stdout_total_bytes),
            )
        };
        let data = fs::read(path).unwrap_or_default();
        let total = total
            .unwrap_or(data.len() as u64)
            .max(data.len() as u64)
            .min(usize::MAX as u64) as usize;
        (data, total)
    }

    fn enqueue_control(&self, control: &DurableControl) -> Result<PathBuf, WorkspaceError> {
        fs::create_dir_all(&self.paths.control).map_err(|error| WorkspaceError::ToolDetails {
            code: "DURABLE_COMMAND_CONTROL_FAILED",
            message: format!("Failed to create durable command control directory: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({"session_id": self.spec.session_id}),
        })?;
        let sequence = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = self
            .paths
            .control
            .join(format!("{sequence:039}-{}.json", Uuid::new_v4().simple()));
        write_json_atomic(&path, control).map_err(|error| WorkspaceError::ToolDetails {
            code: "DURABLE_COMMAND_CONTROL_FAILED",
            message: format!("Failed to queue durable command control: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({"session_id": self.spec.session_id}),
        })?;
        Ok(path)
    }
}

fn create_marker(path: &Path) -> std::io::Result<bool> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(timestamp().as_bytes())?;
            file.sync_all()?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_bounded_json<T: for<'de> Deserialize<'de>>(path: &Path) -> std::io::Result<T> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > DURABLE_JSON_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable command JSON exceeds size limit",
        ));
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "state path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let payload = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if payload.len() as u64 > DURABLE_JSON_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable command JSON exceeds size limit",
        ));
    }
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&payload)?;
        file.sync_all()?;
    }
    #[cfg(windows)]
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&temp, path)?;
    Ok(())
}

fn append_bounded_log(path: &Path, chunk: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(chunk)?;
    file.flush()?;
    let len = file.metadata()?.len();
    drop(file);
    if len <= DURABLE_LOG_MAX_BYTES {
        return Ok(());
    }
    let retain = DURABLE_LOG_RETAIN_BYTES.min(len);
    let mut source = File::open(path)?;
    source.seek(SeekFrom::Start(len - retain))?;
    let mut tail = Vec::with_capacity(retain as usize);
    source.read_to_end(&mut tail)?;
    let temp = path.with_extension(format!("log-tmp-{}", Uuid::new_v4().simple()));
    fs::write(&temp, tail)?;
    #[cfg(windows)]
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(temp, path)?;
    Ok(())
}

fn persist_durable_state(paths: &DurableJobPaths, state: &DurableCommandState) {
    let _ = write_json_atomic(&paths.state, state);
}

async fn durable_log_reader<T>(
    mut stream: T,
    path: PathBuf,
    paths: DurableJobPaths,
    state: Arc<Mutex<DurableCommandState>>,
    is_stdout: bool,
    encoding: StreamEncoding,
) where
    T: tokio::io::AsyncRead + Unpin,
{
    let mut decoder = StreamDecoder::new(encoding);
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let decoded = decoder.decode(&buffer[..read], false);
                if decoded.is_empty() {
                    continue;
                }
                let _ = append_bounded_log(&path, &decoded);
                let mut current = state.lock().expect("durable state lock");
                if is_stdout {
                    current.stdout_total_bytes = current
                        .stdout_total_bytes
                        .saturating_add(decoded.len() as u64);
                } else {
                    current.stderr_total_bytes = current
                        .stderr_total_bytes
                        .saturating_add(decoded.len() as u64);
                }
                current.last_output_at = timestamp();
                persist_durable_state(&paths, &current);
            }
            Err(_) => break,
        }
    }
    let tail = decoder.decode(&[], true);
    if !tail.is_empty() {
        let _ = append_bounded_log(&path, &tail);
        let mut current = state.lock().expect("durable state lock");
        if is_stdout {
            current.stdout_total_bytes =
                current.stdout_total_bytes.saturating_add(tail.len() as u64);
        } else {
            current.stderr_total_bytes =
                current.stderr_total_bytes.saturating_add(tail.len() as u64);
        }
        current.last_output_at = timestamp();
        persist_durable_state(&paths, &current);
    }
}

fn durable_control_files(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .is_some_and(|kind| kind.is_file())
                .then_some(entry.path())
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn validate_durable_spec_location(spec_path: &Path) -> Result<(), String> {
    #[cfg(test)]
    {
        let _ = spec_path;
        Ok(())
    }

    #[cfg(not(test))]
    {
        let harness_root = crate::harness::Harness::default_root()
            .map_err(|error| format!("resolve Harness root failed: {error}"))?;
        let harness_root = fs::canonicalize(&harness_root)
            .map_err(|error| format!("canonicalize Harness root failed: {error}"))?;
        let spec_path = fs::canonicalize(spec_path)
            .map_err(|error| format!("canonicalize durable spec failed: {error}"))?;
        if !spec_path.starts_with(harness_root.join("workspaces")) {
            return Err("durable command spec is outside the configured Harness store".into());
        }
        Ok(())
    }
}

/// Internal one-shot entrypoint used by the CLI. Recovery code never calls
/// this function: it only reads the persisted job state, so a daemon restart
/// cannot replay a command.
pub(crate) async fn run_durable_command_supervisor(spec_path: PathBuf) -> Result<i32, String> {
    validate_durable_spec_location(&spec_path)?;
    let spec = read_bounded_json::<DurableCommandSpec>(&spec_path)
        .map_err(|error| format!("read durable command spec failed: {error}"))?;
    if spec.schema_version != DURABLE_COMMAND_SCHEMA_VERSION {
        return Err("unsupported durable command spec version".into());
    }
    if Uuid::parse_str(&spec.session_id).is_err() {
        return Err("invalid durable command session id".into());
    }
    let root = spec_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "durable command spec path has no job root".to_string())?;
    let paths = DurableJobPaths::new(root, &spec.session_id);
    if paths.spec != spec_path {
        return Err("durable command spec path does not match session id".into());
    }
    if !create_marker(&paths.spawned)
        .map_err(|error| format!("claim durable command failed: {error}"))?
    {
        return Err("durable command was already claimed; replay refused".into());
    }
    let mut state = read_bounded_json::<DurableCommandState>(&paths.state)
        .map_err(|error| format!("read durable command state failed: {error}"))?;
    if state.schema_version != DURABLE_COMMAND_SCHEMA_VERSION || state.session_id != spec.session_id
    {
        return Err("durable command state does not match spec".into());
    }
    state.supervisor_pid = Some(std::process::id());
    state.status = "starting".into();
    state.termination_reason = "running".into();
    persist_durable_state(&paths, &state);

    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::platform::configure_exec_tokio_process(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            state.status = "spawn_failed".into();
            state.termination_reason = "spawn_failed".into();
            state.stdin_open = false;
            state.finished_at = Some(timestamp());
            persist_durable_state(&paths, &state);
            return Err(format!("spawn durable workspace command failed: {error}"));
        }
    };
    crate::platform::lower_exec_child_priority(&child);
    let child_pid = child.id();
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let state = Arc::new(Mutex::new({
        state.status = "running".into();
        state.child_pid = child_pid;
        state.stdin_open = stdin.is_some();
        persist_durable_state(&paths, &state);
        state
    }));
    let mut readers = Vec::new();
    if let Some(stdout) = stdout {
        readers.push(crate::async_runtime::spawn(durable_log_reader(
            stdout,
            paths.stdout.clone(),
            paths.clone(),
            Arc::clone(&state),
            true,
            spec.output_encoding,
        )));
    }
    if let Some(stderr) = stderr {
        readers.push(crate::async_runtime::spawn(durable_log_reader(
            stderr,
            paths.stderr.clone(),
            paths.clone(),
            Arc::clone(&state),
            false,
            spec.output_encoding,
        )));
    }

    if !spec.initial_stdin.is_empty() {
        if let Some(input) = stdin.as_mut() {
            use tokio::io::AsyncWriteExt;
            let _ = input.write_all(spec.initial_stdin.as_bytes()).await;
            let _ = input.shutdown().await;
        }
        stdin = None;
        let mut current = state.lock().expect("durable state lock");
        current.stdin_open = false;
        persist_durable_state(&paths, &current);
    }

    let deadline = Instant::now() + Duration::from_millis(spec.timeout_ms.max(1));
    let exit_status = 'wait: loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if Instant::now() >= deadline {
            let grace_deadline = Instant::now() + TERMINAL_RECONCILIATION_GRACE;
            while Instant::now() < grace_deadline {
                if let Ok(Some(status)) = child.try_wait() {
                    if status.success() {
                        let mut current = state.lock().expect("durable state lock");
                        current.termination_reason = "late_success".into();
                        persist_durable_state(&paths, &current);
                    }
                    break 'wait status;
                }
                tokio::time::sleep(TERMINAL_RECONCILIATION_POLL).await;
            }
            {
                let mut current = state.lock().expect("durable state lock");
                current.termination_reason = "timeout".into();
                persist_durable_state(&paths, &current);
            }
            if let Some(pid) = child.id() {
                send_session_signal(pid, "KILL");
            } else {
                let _ = child.start_kill();
            }
            break child
                .wait()
                .await
                .map_err(|error| format!("wait after durable timeout failed: {error}"))?;
        }

        for control_path in durable_control_files(&paths.control) {
            let control = read_bounded_json::<DurableControl>(&control_path);
            if let Ok(control) = control {
                match control.action.as_str() {
                    "stdin" => {
                        if let (Some(input), Some(chars)) =
                            (stdin.as_mut(), control.chars.as_deref())
                        {
                            use tokio::io::AsyncWriteExt;
                            let _ = input.write_all(chars.as_bytes()).await;
                            let _ = input.flush().await;
                        }
                    }
                    "signal" => {
                        let signal = control.signal.as_deref().unwrap_or("TERM");
                        {
                            let mut current = state.lock().expect("durable state lock");
                            current.termination_reason = "killed".into();
                            persist_durable_state(&paths, &current);
                        }
                        if let Some(pid) = child.id() {
                            send_session_signal(pid, signal);
                        } else {
                            let _ = child.start_kill();
                        }
                    }
                    _ => {}
                }
            }
            let _ = fs::remove_file(control_path);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    for reader in readers {
        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
    }
    let mut current = state.lock().expect("durable state lock");
    current.status = "exited".into();
    if current.termination_reason == "running" {
        current.termination_reason = "exited".into();
    }
    current.exit_code = exit_status.code();
    current.stdin_open = false;
    current.finished_at = Some(timestamp());
    persist_durable_state(&paths, &current);
    Ok(exit_status.code().unwrap_or(1))
}

struct StreamDecoder {
    encoding: StreamEncoding,
    #[cfg(windows)]
    pending: Vec<u8>,
}

impl StreamDecoder {
    fn new(encoding: StreamEncoding) -> Self {
        Self {
            encoding,
            #[cfg(windows)]
            pending: Vec::new(),
        }
    }

    fn decode(&mut self, chunk: &[u8], finish: bool) -> Vec<u8> {
        #[cfg(not(windows))]
        let _ = finish;
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEncoding {
    #[default]
    Utf8,
    #[cfg(windows)]
    WindowsOem,
}

fn non_blocking_stderr_warnings(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("warning:")
                && (lower.contains("lf will be replaced by crlf")
                    || lower.contains("crlf will be replaced by lf"))
        })
        .map(str::to_string)
        .collect()
}

fn attach_stderr_classification(
    object: &mut serde_json::Map<String, Value>,
    command_ok: Option<bool>,
) {
    let stderr = object
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let non_blocking = non_blocking_stderr_warnings(stderr);
    let non_empty_line_count = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let classification = if stderr.trim().is_empty() {
        "empty"
    } else if command_ok == Some(false) {
        "error"
    } else if !non_blocking.is_empty() && non_blocking.len() == non_empty_line_count {
        "non_blocking_warning"
    } else {
        "diagnostic"
    };
    object.insert(
        "stderr_classification".into(),
        Value::String(classification.into()),
    );
    object.insert(
        "non_blocking_warnings".into(),
        serde_json::to_value(&non_blocking).unwrap_or_else(|_| json!([])),
    );
    if !non_blocking.is_empty() {
        let warnings = object
            .entry("warnings")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(warnings) = warnings.as_array_mut() {
            for warning in non_blocking {
                if !warnings
                    .iter()
                    .any(|value| value.as_str() == Some(&warning))
                {
                    warnings.push(Value::String(warning));
                }
            }
        }
    }
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
            "exited" | "late_success" if command_ok == Some(true) => "succeeded",
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
        attach_stderr_classification(object, command_ok);
        if command_ok == Some(false) || !transport_ok {
            let execution_started = object
                .get("execution_started")
                .and_then(Value::as_bool)
                .unwrap_or(!matches!(
                    reason.as_str(),
                    "spawn_failed" | "command_rejected"
                ));
            let failure_stage = object
                .get("failure_stage")
                .and_then(Value::as_str)
                .unwrap_or(match reason.as_str() {
                    "command_rejected" => "pre_spawn",
                    "spawn_failed" => "spawn",
                    _ if !execution_started => "pre_spawn",
                    _ => "process",
                })
                .to_string();
            let duration_ms = object
                .get("duration_ms")
                .and_then(Value::as_u64)
                .or_else(|| object.get("execution_duration_ms").and_then(Value::as_u64))
                .or_else(|| object.get("elapsed_ms").and_then(Value::as_u64))
                .unwrap_or(0);
            let stdout_empty = object
                .get("stdout")
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            let stderr_empty = object
                .get("stderr")
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            let short_empty_process_failure =
                failure_stage == "process" && duration_ms <= 250 && stdout_empty && stderr_empty;
            let failure_classification = object
                .get("failure_classification")
                .and_then(Value::as_str)
                .unwrap_or(
                    if matches!(failure_stage.as_str(), "pre_spawn" | "spawn")
                        || short_empty_process_failure
                        || matches!(reason.as_str(), "timeout" | "killed" | "server_restart")
                    {
                        "infrastructure"
                    } else {
                        "command"
                    },
                )
                .to_string();
            object.insert("ok".into(), Value::Bool(false));
            object.insert("failure_stage".into(), Value::String(failure_stage.clone()));
            object.insert(
                "failure_classification".into(),
                Value::String(failure_classification.clone()),
            );
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
                        "failure_stage": failure_stage,
                        "failure_classification": failure_classification,
                        "exit_code": exit_code,
                        "session_id": session_id
                    }
                })
            });
            if short_empty_process_failure {
                let warnings = object
                    .entry("warnings")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(warnings) = warnings.as_array_mut() {
                    warnings.push(Value::String(
                        "Process exited almost immediately without stdout/stderr; classify this as an infrastructure/startup failure before treating it as a test failure."
                            .into(),
                    ));
                }
            }
        } else {
            object.entry("ok").or_insert(Value::Bool(true));
        }
    }
    value
}

pub fn list_command_sessions(
    store: &CommandSessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let include_history = args
        .get("include_history")
        .and_then(Value::as_bool)
        .or_else(|| args.get("include_terminal").and_then(Value::as_bool))
        .unwrap_or(false);
    let running_only_compat = args.get("include_history").is_none()
        && args.get("include_terminal").and_then(Value::as_bool) == Some(false);
    let max_output_bytes = args
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(4_096)
        .clamp(0, 65_536) as usize;
    let all_sessions = store.list_snapshots(true, max_output_bytes);
    let unobserved_terminal_session_ids = all_sessions
        .iter()
        .filter(|session| session.get("execution_status") != Some(&json!("running")))
        .filter(|session| session.get("result_observed") == Some(&Value::Bool(false)))
        .filter_map(|session| session.get("session_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let running_session_ids = all_sessions
        .iter()
        .filter(|session| session.get("execution_status") == Some(&json!("running")))
        .filter_map(|session| session.get("session_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let running_count = running_session_ids.len();
    let retained_total_count = all_sessions.len();
    let sessions = if include_history {
        all_sessions
    } else if running_only_compat {
        all_sessions
            .into_iter()
            .filter(|session| session.get("execution_status") == Some(&json!("running")))
            .collect::<Vec<_>>()
    } else {
        all_sessions
            .into_iter()
            .filter(|session| {
                session.get("execution_status") == Some(&json!("running"))
                    || session.get("result_observed") == Some(&Value::Bool(false))
            })
            .collect::<Vec<_>>()
    };
    let session_count = sessions.len();
    let terminal_count = session_count.saturating_sub(running_count);
    let unobserved_terminal_count = unobserved_terminal_session_ids.len();
    let pending_result_count = running_count.saturating_add(unobserved_terminal_count);
    let durable_session_count = sessions
        .iter()
        .filter(|session| session.get("durable") == Some(&Value::Bool(true)))
        .count();
    let process_bound_session_count = sessions
        .iter()
        .filter(|session| session.get("process_bound") == Some(&Value::Bool(true)))
        .count();
    Ok(tool_ok(json!({
        "sessions": sessions,
        "scope": if include_history { "history" } else if running_only_compat { "running" } else { "pending" },
        "history_included": include_history,
        "retained_total_count": retained_total_count,
        "session_count": session_count,
        "running_count": running_count,
        "terminal_count": terminal_count,
        "unobserved_terminal_count": unobserved_terminal_count,
        "pending_result_count": pending_result_count,
        "running_session_ids": running_session_ids,
        "unobserved_terminal_session_ids": unobserved_terminal_session_ids,
        "requires_followup": pending_result_count > 0,
        "next_actions": if pending_result_count > 0 {
            json!(["wait_command", "kill_session"])
        } else {
            json!([])
        },
        "process_bound": process_bound_session_count > 0,
        "process_bound_session_count": process_bound_session_count,
        "durable_session_count": durable_session_count,
        "durable_supervisor_available": store.durable_enabled(),
        "warnings": []
    })))
}

pub fn wait_command(store: &CommandSessionStore, args: &Value) -> Result<Value, WorkspaceError> {
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
        session.mark_terminal_observed();
    }
    let snapshot = session.snapshot(limit);
    let termination_reason = snapshot
        .get("termination_reason")
        .and_then(Value::as_str)
        .unwrap_or("running");
    let exit_code = snapshot.get("exit_code").and_then(Value::as_i64);
    let state = match termination_reason {
        "running" => "running",
        "exited" | "late_success" if exit_code == Some(0) => "completed",
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
        "command": snapshot["command"],
        "resolved_cwd": snapshot["resolved_cwd"],
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
        "execution_duration_ms": snapshot["execution_duration_ms"],
        "session_age_ms": snapshot["session_age_ms"],
        "retained_ms": snapshot["retained_ms"],
        "finished_at": snapshot["finished_at"],
        "result_observed": snapshot["result_observed"],
        "durable": snapshot["durable"],
        "process_bound": snapshot["process_bound"],
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

impl Default for CommandSessionStore {
    fn default() -> Self {
        Self::with_retention_limits(
            DEFAULT_MAX_EXEC_SESSIONS,
            DEFAULT_TERMINAL_SLOT_RETENTION,
            DEFAULT_TERMINAL_LOG_RETENTION,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_success_is_a_successful_terminal_result() {
        let result = finalize_execution_result(json!({
            "status": "exited",
            "termination_reason": "late_success",
            "exit_code": 0,
            "transport_ok": true,
            "command_ok": true
        }));
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["success"], true, "{result}");
        assert_eq!(result["execution_status"], "succeeded", "{result}");
        assert_eq!(result["termination_reason"], "late_success", "{result}");
    }

    #[test]
    #[ignore = "invoked as a child process by durable supervisor tests"]
    fn durable_supervisor_output_child() {
        println!("durable-child-output");
        std::thread::sleep(Duration::from_millis(150));
    }

    #[test]
    #[ignore = "invoked as a child process by durable supervisor tests"]
    fn durable_supervisor_stdin_child() {
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .expect("read durable stdin");
        println!("durable-stdin:{}", input.trim());
    }

    fn durable_child_args(test_name: &str) -> Vec<String> {
        vec![
            test_name.to_string(),
            "--ignored".to_string(),
            "--exact".to_string(),
            "--nocapture".to_string(),
        ]
    }

    fn durable_test_spec(
        session_id: String,
        test_name: &str,
        timeout_ms: u64,
    ) -> DurableCommandSpec {
        DurableCommandSpec {
            schema_version: DURABLE_COMMAND_SCHEMA_VERSION,
            session_id,
            command: format!("test-child {test_name}"),
            program: std::env::current_exe()
                .expect("current test exe")
                .display()
                .to_string(),
            args: durable_child_args(test_name),
            cwd: std::env::current_dir()
                .expect("current dir")
                .display()
                .to_string(),
            timeout_ms,
            initial_stdin: String::new(),
            output_encoding: StreamEncoding::Utf8,
            expected_exit_codes: vec![0],
            harness_metadata: None,
            owner_scope: Some("durable-test".into()),
            transport_session_id: None,
            execution_resources: None,
            started_at: timestamp(),
        }
    }

    fn wait_for_durable_status(path: &Path, expected: &str) {
        for _ in 0..200 {
            if read_bounded_json::<DurableCommandState>(path)
                .ok()
                .is_some_and(|state| state.status == expected)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("durable state never reached {expected}");
    }

    #[test]
    fn durable_job_recovery_keeps_output_and_refuses_replay() {
        let temp = tempfile::tempdir().expect("durable root");
        let root = temp.path().join("jobs");
        let store = CommandSessionStore::with_durable_root(root.clone());
        let session_id = Uuid::new_v4().to_string();
        let job = store
            .create_durable_job(durable_test_spec(
                session_id.clone(),
                "tools::command_session::tests::durable_supervisor_output_child",
                5_000,
            ))
            .expect("create durable job");
        let spec_path = job.spec_path().to_path_buf();
        let state_path = job.paths.state.clone();
        let supervisor = std::thread::spawn({
            let spec_path = spec_path.clone();
            move || crate::async_runtime::block_on(run_durable_command_supervisor(spec_path))
        });
        for _ in 0..200 {
            if job.paths.spawned.is_file() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            job.paths.spawned.is_file(),
            "supervisor must claim the spec"
        );
        let replay = crate::async_runtime::block_on(run_durable_command_supervisor(spec_path));
        assert!(replay
            .expect_err("second supervisor must refuse replay")
            .contains("replay refused"));

        // Rebuild the store while the original supervisor still owns the job.
        let recovered = CommandSessionStore::with_durable_root(root);
        let recovered_session = recovered.get(&session_id).expect("recovered session");
        assert!(recovered_session.is_durable());
        assert!(!recovered_session.terminal_observed());

        let result = supervisor
            .join()
            .expect("supervisor thread")
            .expect("child exit");
        assert_eq!(result, 0);
        wait_for_durable_status(&state_path, "exited");
        crate::async_runtime::block_on(recovered_session.refresh_status());
        let (stdout, total) = recovered_session.retained_stream_bytes("stdout");
        assert!(total >= stdout.len());
        assert!(String::from_utf8_lossy(&stdout).contains("durable-child-output"));
        recovered_session.mark_terminal_observed();
        assert!(job.paths.observed.is_file());
    }

    #[test]
    fn recovered_durable_session_accepts_stdin_control() {
        let temp = tempfile::tempdir().expect("durable root");
        let root = temp.path().join("jobs");
        let store = CommandSessionStore::with_durable_root(root.clone());
        let session_id = Uuid::new_v4().to_string();
        let job = store
            .create_durable_job(durable_test_spec(
                session_id.clone(),
                "tools::command_session::tests::durable_supervisor_stdin_child",
                5_000,
            ))
            .expect("create durable job");
        let state_path = job.paths.state.clone();
        let supervisor = std::thread::spawn({
            let spec_path = job.spec_path().to_path_buf();
            move || crate::async_runtime::block_on(run_durable_command_supervisor(spec_path))
        });
        wait_for_durable_status(&state_path, "running");

        let recovered = CommandSessionStore::with_durable_root(root);
        let write = write_stdin(
            &recovered,
            &json!({
                "session_id": session_id,
                "chars": "hello-after-reconnect\n",
                "yield_time_ms": 50,
                "max_output_bytes": 4096
            }),
        )
        .expect("queue durable stdin");
        assert_eq!(write["durable"], true);
        // The child reads until EOF. Terminate the session after proving the
        // control file was consumed; its stdout must include the input before
        // the supervisor exits on timeout/kill.
        for _ in 0..100 {
            if durable_control_files(&job.paths.control).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(durable_control_files(&job.paths.control).is_empty());
        assert_eq!(supervisor.join().expect("supervisor thread").unwrap(), 0);
        wait_for_durable_status(&state_path, "exited");
        let recovered_session = recovered.get(&session_id).expect("recovered session");
        crate::async_runtime::block_on(recovered_session.refresh_status());
        let (stdout, _) = recovered_session.retained_stream_bytes("stdout");
        assert!(String::from_utf8_lossy(&stdout).contains("durable-stdin:hello-after-reconnect"));
    }

    #[test]
    fn durable_session_does_not_block_daemon_handoff() {
        let temp = tempfile::tempdir().expect("durable root");
        let root = temp.path().join("jobs");
        let creator = CommandSessionStore::with_durable_root(root.clone());
        let session_id = Uuid::new_v4().to_string();
        let _job = creator
            .create_durable_job(durable_test_spec(
                session_id,
                "tools::command_session::tests::durable_supervisor_output_child",
                5_000,
            ))
            .expect("durable job");
        let recovered = CommandSessionStore::with_durable_root(root);
        assert_eq!(recovered.prepare_daemon_handoff(u32::MAX).unwrap(), None);
    }

    #[test]
    fn stale_unclaimed_durable_job_is_interrupted_without_replay() {
        let temp = tempfile::tempdir().expect("durable root");
        let root = temp.path().join("jobs");
        let creator = CommandSessionStore::with_durable_root(root.clone());
        let session_id = Uuid::new_v4().to_string();
        let mut spec = durable_test_spec(
            session_id.clone(),
            "tools::command_session::tests::durable_supervisor_output_child",
            5_000,
        );
        spec.started_at = (Utc::now() - chrono::Duration::seconds(10))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let job = creator.create_durable_job(spec).expect("durable job");
        let recovered = CommandSessionStore::with_durable_root(root);
        let session = recovered.get(&session_id).expect("recovered session");
        crate::async_runtime::block_on(session.refresh_status());
        let snapshot = session.snapshot(0);
        assert_eq!(snapshot["execution_status"], "interrupted", "{snapshot}");
        assert_eq!(snapshot["termination_reason"], "launch_interrupted");
        assert_eq!(snapshot["retryable"], true);
        assert!(
            !job.paths.spawned.exists(),
            "recovery must never replay the job"
        );
    }

    #[test]
    fn recovered_running_durable_job_holds_execution_capacity() {
        let temp = tempfile::tempdir().expect("durable root");
        let root = temp.path().join("jobs");
        let creator = CommandSessionStore::with_durable_root(root.clone());
        let session_id = Uuid::new_v4().to_string();
        let job = creator
            .create_durable_job(durable_test_spec(
                session_id,
                "tools::command_session::tests::durable_supervisor_output_child",
                5_000,
            ))
            .expect("durable job");
        let mut state = job.read_state().expect("state");
        state.status = "running".into();
        state.supervisor_pid = Some(std::process::id());
        persist_durable_state(&job.paths, &state);
        let recovered = CommandSessionStore::with_durable_root(root);
        let error = recovered
            .ensure_execution_capacity(1)
            .expect_err("recovered running durable job must consume capacity");
        assert!(matches!(
            error,
            WorkspaceError::ToolDetails {
                code: "EXECUTION_CAPACITY_HELD",
                ..
            }
        ));
    }

    fn spawn_test_session() -> ExecSession {
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let child = crate::async_runtime::block_on(async {
            tokio::process::Command::new(rustc)
                .arg("--version")
                .spawn()
                .expect("spawn rustc --version")
        });
        ExecSession::new(child)
    }

    fn wait_until_exited(session: &ExecSession) {
        crate::async_runtime::block_on(async {
            for _ in 0..100 {
                session.refresh_status().await;
                if session.has_exited() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("test command did not exit");
        });
    }

    #[cfg(unix)]
    fn spawn_handoff_session(transport_session_id: &str) -> (u32, ExecSession) {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = crate::async_runtime::block_on(async {
            command.spawn().expect("spawn handoff test child")
        });
        let pid = child.id().expect("handoff test child pid");
        (
            pid,
            ExecSession::new_with_details_encoding_and_resources(
                child,
                false,
                "sleep 30".into(),
                ".".into(),
                None,
                None,
                Some(transport_session_id.to_string()),
                StreamEncoding::Utf8,
                None,
            ),
        )
    }

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
    fn git_line_ending_stderr_is_classified_as_non_blocking_warning() {
        let warning = "warning: in the working copy of 'src/main.rs', LF will be replaced by CRLF the next time Git touches it";
        let result = finalize_execution_result(json!({
            "status": "exited",
            "termination_reason": "exited",
            "exit_code": 0,
            "transport_ok": true,
            "command_ok": true,
            "stderr": warning,
            "warnings": []
        }));

        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["stderr_classification"], "non_blocking_warning");
        assert_eq!(result["non_blocking_warnings"], json!([warning]));
        assert_eq!(result["warnings"], json!([warning]));
    }

    #[test]
    fn successful_non_warning_stderr_remains_diagnostic() {
        let result = finalize_execution_result(json!({
            "status": "exited",
            "termination_reason": "exited",
            "exit_code": 0,
            "transport_ok": true,
            "command_ok": true,
            "stderr": "compiler emitted progress",
            "warnings": []
        }));

        assert_eq!(result["stderr_classification"], "diagnostic");
        assert_eq!(result["non_blocking_warnings"], json!([]));
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

    #[cfg(unix)]
    #[test]
    fn daemon_handoff_preflight_only_allows_the_unexposed_initiator_command() {
        let store = CommandSessionStore::with_limits(4, Duration::from_secs(60));
        let (pid, exec_session) = spawn_handoff_session("mcp-session-1");
        let session = store
            .insert(exec_session)
            .unwrap_or_else(|_| panic!("insert handoff session"));

        assert_eq!(
            store.prepare_daemon_handoff(pid),
            Ok(Some("mcp-session-1".into()))
        );

        session.mark_externally_retained();
        let error = store
            .prepare_daemon_handoff(pid)
            .expect_err("externally retained initiator must block handoff");
        assert!(error.contains(&session.session_id));

        crate::async_runtime::block_on(session.kill_and_wait());
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

    #[test]
    fn background_terminal_refresh_does_not_extend_retention() {
        let store = CommandSessionStore::with_limits(4, Duration::from_millis(100));
        let session = store
            .insert(spawn_test_session())
            .unwrap_or_else(|_| panic!("insert session"));
        wait_until_exited(&session);
        session.mark_terminal_observed();

        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(store.list_snapshots(true, 0).len(), 1);
        std::thread::sleep(Duration::from_millis(60));

        assert!(store.list_snapshots(true, 0).is_empty());
    }

    #[test]
    fn unobserved_terminal_session_is_never_pruned_by_retention() {
        let store = CommandSessionStore::with_limits(4, Duration::ZERO);
        let session = store
            .insert(spawn_test_session())
            .unwrap_or_else(|_| panic!("insert session"));
        wait_until_exited(&session);
        std::thread::sleep(Duration::from_millis(2));

        let snapshots = store.list_snapshots(true, 0);
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["result_observed"], false);
    }

    #[test]
    fn capacity_prunes_expired_observed_terminal_before_insert() {
        let store = CommandSessionStore::with_limits(1, Duration::from_millis(100));
        let first = store
            .insert(spawn_test_session())
            .unwrap_or_else(|_| panic!("insert first"));
        wait_until_exited(&first);
        first.mark_terminal_observed();

        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(store.list_snapshots(true, 0).len(), 1);
        std::thread::sleep(Duration::from_millis(60));

        let second = store
            .insert(spawn_test_session())
            .unwrap_or_else(|_| panic!("expired slot reclaimed"));
        wait_until_exited(&second);
    }

    #[test]
    fn observed_terminal_releases_slot_before_retained_output_expires() {
        let store = CommandSessionStore::with_retention_limits(
            1,
            Duration::from_millis(40),
            Duration::from_millis(500),
        );
        let first = store
            .insert(spawn_test_session())
            .unwrap_or_else(|_| panic!("insert first"));
        let first_id = first.session_id.clone();
        wait_until_exited(&first);
        first.mark_terminal_observed();

        std::thread::sleep(Duration::from_millis(60));
        let second = store
            .insert(spawn_test_session())
            .unwrap_or_else(|_| panic!("observed terminal should no longer consume capacity"));
        assert!(
            store.get(&first_id).is_ok(),
            "retained output remains readable"
        );
        wait_until_exited(&second);
    }

    #[test]
    fn capacity_never_evicts_unobserved_terminal_result() {
        let store = CommandSessionStore::with_limits(1, Duration::ZERO);
        let first = store
            .insert(spawn_test_session())
            .unwrap_or_else(|_| panic!("insert first"));
        wait_until_exited(&first);
        std::thread::sleep(Duration::from_millis(2));

        let rejected = match store.insert(spawn_test_session()) {
            Err(rejected) => rejected,
            Ok(session) => {
                crate::async_runtime::block_on(session.kill_and_wait());
                panic!("capacity must remain full");
            }
        };
        crate::async_runtime::block_on(rejected.kill_and_wait());
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

impl CommandSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limits(max_sessions: usize, terminal_retention: Duration) -> Self {
        Self::with_retention_limits(max_sessions, terminal_retention, terminal_retention)
    }

    pub fn with_retention_limits(
        max_sessions: usize,
        terminal_slot_retention: Duration,
        terminal_log_retention: Duration,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            execution_admission: Mutex::new(()),
            max_sessions: max_sessions.max(1),
            terminal_slot_retention,
            terminal_log_retention: terminal_log_retention.max(terminal_slot_retention),
            durable_root: None,
        }
    }

    pub fn with_durable_root(root: PathBuf) -> Self {
        let store = Self {
            sessions: Mutex::new(HashMap::new()),
            execution_admission: Mutex::new(()),
            max_sessions: DEFAULT_MAX_EXEC_SESSIONS,
            terminal_slot_retention: DEFAULT_TERMINAL_SLOT_RETENTION,
            terminal_log_retention: DEFAULT_TERMINAL_LOG_RETENTION,
            durable_root: Some(root),
        };
        store.recover_durable_sessions();
        store
    }

    pub fn durable_enabled(&self) -> bool {
        self.durable_root.is_some()
    }

    pub fn execution_start_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.execution_admission
            .lock()
            .expect("execution admission lock")
    }

    pub fn ensure_execution_capacity(&self, max_running: usize) -> Result<(), WorkspaceError> {
        let sessions = {
            let mut sessions = self.sessions.lock().expect("sessions lock");
            prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let running = sessions
            .into_iter()
            .filter(|session| {
                crate::async_runtime::block_on(session.refresh_status());
                !session.has_exited()
            })
            .count();
        if running >= max_running.max(1) {
            return Err(WorkspaceError::ToolDetails {
                code: "EXECUTION_CAPACITY_HELD",
                message: "Execution capacity is currently held by running command sessions.".into(),
                category: "runtime",
                retryable: true,
                details: json!({
                    "running_command_sessions": running,
                    "max_running_commands": max_running.max(1),
                    "durable_sessions_included": true,
                    "suggestion": "Wait for or terminate an existing command session before starting another."
                }),
            });
        }
        Ok(())
    }

    pub(crate) fn create_durable_job(
        &self,
        spec: DurableCommandSpec,
    ) -> Result<DurableJobHandle, WorkspaceError> {
        let root = self
            .durable_root
            .as_ref()
            .ok_or_else(|| WorkspaceError::Tool {
                code: "DURABLE_COMMAND_UNAVAILABLE",
                message: "Durable command storage is not configured.".into(),
                category: "runtime",
                retryable: false,
            })?;
        if Uuid::parse_str(&spec.session_id).is_err() {
            return Err(WorkspaceError::invalid_argument(
                "durable command session_id must be a UUID",
            ));
        }
        let paths = DurableJobPaths::new(root, &spec.session_id);
        fs::create_dir_all(&paths.control).map_err(|error| WorkspaceError::ToolDetails {
            code: "DURABLE_COMMAND_CREATE_FAILED",
            message: format!("Failed to create durable command job directory: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({"session_id": spec.session_id}),
        })?;
        if paths.spec.exists() || paths.spawned.exists() {
            return Err(WorkspaceError::ToolDetails {
                code: "DURABLE_COMMAND_ID_CONFLICT",
                message: "Durable command session already exists; refusing to replay it.".into(),
                category: "conflict",
                retryable: false,
                details: json!({"session_id": spec.session_id}),
            });
        }
        write_json_atomic(&paths.spec, &spec).map_err(|error| WorkspaceError::ToolDetails {
            code: "DURABLE_COMMAND_CREATE_FAILED",
            message: format!("Failed to persist durable command spec: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({"session_id": spec.session_id}),
        })?;
        let state = DurableCommandState {
            schema_version: DURABLE_COMMAND_SCHEMA_VERSION,
            session_id: spec.session_id.clone(),
            status: "starting".into(),
            termination_reason: "running".into(),
            supervisor_pid: None,
            child_pid: None,
            exit_code: None,
            stdin_open: true,
            started_at: spec.started_at.clone(),
            finished_at: None,
            last_output_at: spec.started_at.clone(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
        };
        write_json_atomic(&paths.state, &state).map_err(|error| WorkspaceError::ToolDetails {
            code: "DURABLE_COMMAND_CREATE_FAILED",
            message: format!("Failed to persist durable command state: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({"session_id": spec.session_id}),
        })?;
        Ok(DurableJobHandle { paths, spec })
    }

    fn recover_durable_sessions(&self) {
        let Some(root) = self.durable_root.as_ref() else {
            return;
        };
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        let mut recovered = Vec::new();
        for entry in entries.flatten().take(self.max_sessions.saturating_mul(4)) {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let Some(session_id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if Uuid::parse_str(&session_id).is_err() {
                continue;
            }
            let paths = DurableJobPaths::new(root, &session_id);
            let Ok(spec) = read_bounded_json::<DurableCommandSpec>(&paths.spec) else {
                continue;
            };
            let Ok(state) = read_bounded_json::<DurableCommandState>(&paths.state) else {
                continue;
            };
            if spec.schema_version != DURABLE_COMMAND_SCHEMA_VERSION
                || state.schema_version != DURABLE_COMMAND_SCHEMA_VERSION
                || spec.session_id != session_id
                || state.session_id != session_id
            {
                continue;
            }
            if state.status == "exited" && paths.observed.is_file() {
                if let Some(finished) = state
                    .finished_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                {
                    let elapsed = Utc::now().signed_duration_since(finished.with_timezone(&Utc));
                    if elapsed
                        .to_std()
                        .ok()
                        .is_some_and(|elapsed| elapsed > self.terminal_log_retention)
                    {
                        let _ = fs::remove_dir_all(&paths.dir);
                        continue;
                    }
                }
            }
            recovered.push(ExecSession::from_durable_job(
                DurableJobHandle { paths, spec },
                state,
            ));
        }
        let mut sessions = self.sessions.lock().expect("sessions lock");
        for session in recovered {
            sessions.insert(session.session_id.clone(), Arc::new(session));
        }
    }

    pub fn insert(&self, session: ExecSession) -> Result<Arc<ExecSession>, Box<ExecSession>> {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
        if sessions
            .values()
            .filter(|session| session_consumes_slot(session, self.terminal_slot_retention))
            .count()
            >= self.max_sessions
        {
            return Err(Box::new(session));
        }
        let arc = Arc::new(session);
        sessions.insert(arc.session_id.clone(), arc.clone());
        Ok(arc)
    }

    pub fn ensure_capacity(&self) -> Result<(), WorkspaceError> {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
        let consumed = sessions
            .values()
            .filter(|session| session_consumes_slot(session, self.terminal_slot_retention))
            .count();
        if consumed >= self.max_sessions {
            Err(self.capacity_error())
        } else {
            Ok(())
        }
    }

    pub fn get(&self, session_id: &str) -> Result<Arc<ExecSession>, WorkspaceError> {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
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
            prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
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
            prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
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

    pub fn task_ids_requiring_followup(&self) -> Vec<String> {
        let sessions = {
            let mut sessions = self.sessions.lock().expect("sessions lock");
            prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut task_ids = sessions
            .into_iter()
            .filter_map(|session| {
                crate::async_runtime::block_on(session.refresh_status());
                let requires_followup = !session.has_exited() || !session.terminal_observed();
                requires_followup
                    .then(|| session.harness_metadata().map(|metadata| metadata.task_id))
                    .flatten()
            })
            .collect::<Vec<_>>();
        task_ids.sort_unstable();
        task_ids.dedup();
        task_ids
    }

    pub(crate) fn prepare_daemon_handoff(
        &self,
        initiator_pid: u32,
    ) -> Result<Option<String>, String> {
        let sessions = {
            let mut sessions = self.sessions.lock().expect("sessions lock");
            prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut initiator_transport_session = None;
        let mut blockers = Vec::new();
        for session in sessions {
            crate::async_runtime::block_on(session.refresh_status());
            if session.is_durable() {
                // The detached supervisor owns this command; MCP daemon handoff
                // does not affect its process or output state.
                continue;
            }
            let child_pid = if session.has_exited() {
                None
            } else {
                crate::async_runtime::block_on(async {
                    let child = session.child.lock().await;
                    child.as_ref().and_then(|child| child.id())
                })
            };
            let is_blocking_initiator = child_pid == Some(initiator_pid)
                && !session.externally_retained()
                && initiator_transport_session.is_none();
            if is_blocking_initiator {
                initiator_transport_session = session.transport_session_id.clone();
                continue;
            }
            blockers.push(session.session_id.clone());
        }
        if blockers.is_empty() {
            Ok(initiator_transport_session)
        } else {
            Err(format!(
                "zero-downtime handoff is blocked by retained process-bound command sessions: {}",
                blockers.join(",")
            ))
        }
    }

    pub fn pending_for_task(
        &self,
        task_id: &str,
        max_output_bytes: usize,
    ) -> (Vec<Value>, Vec<Value>) {
        self.pending_matching(
            |session| {
                session
                    .harness_metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.task_id == task_id)
            },
            max_output_bytes,
        )
    }

    pub fn pending_for_owner(
        &self,
        owner_scope: &str,
        max_output_bytes: usize,
    ) -> (Vec<Value>, Vec<Value>) {
        self.pending_matching(
            |session| {
                session.harness_metadata.is_none() && session.owner_scope() == Some(owner_scope)
            },
            max_output_bytes,
        )
    }

    fn pending_matching<F>(
        &self,
        matches_scope: F,
        max_output_bytes: usize,
    ) -> (Vec<Value>, Vec<Value>)
    where
        F: Fn(&ExecSession) -> bool,
    {
        let sessions = {
            let mut sessions = self.sessions.lock().expect("sessions lock");
            prune_terminal_sessions(&mut sessions, self.terminal_log_retention);
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut running = Vec::new();
        let mut unobserved_terminal = Vec::new();
        for session in sessions {
            if !matches_scope(&session) {
                continue;
            }
            crate::async_runtime::block_on(session.refresh_status());
            if !session.has_exited() {
                running.push(session.snapshot(max_output_bytes));
            } else if !session.terminal_observed() {
                unobserved_terminal.push(session.snapshot(max_output_bytes));
            }
        }
        (running, unobserved_terminal)
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
                "terminal_slot_retention_ms": self.terminal_slot_retention.as_millis(),
                "terminal_log_retention_ms": self.terminal_log_retention.as_millis(),
                "suggestion": "结束运行中会话或消费未观察的终态结果；已消费终态只在短暂 slot retention 内占用并发槽"
            }),
        }
    }
}

fn session_consumes_slot(session: &ExecSession, terminal_slot_retention: Duration) -> bool {
    !session.has_exited()
        || !session.terminal_observed()
        || session
            .last_access_elapsed()
            .is_some_and(|elapsed| elapsed <= terminal_slot_retention)
}

fn prune_terminal_sessions(
    sessions: &mut HashMap<String, Arc<ExecSession>>,
    terminal_retention: Duration,
) {
    sessions.retain(|_, session| {
        !session.has_exited()
            || !session.terminal_observed()
            || session
                .last_access_elapsed()
                .is_some_and(|elapsed| elapsed <= terminal_retention)
    });
}

pub struct ExecSession {
    pub session_id: String,
    pub(crate) child: AsyncMutex<Option<Child>>,
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
    finished_at: Mutex<Option<Instant>>,
    finished_at_iso: Mutex<Option<String>>,
    last_output_at: Mutex<String>,
    pub exit_code: Mutex<Option<i32>>,
    exited: AtomicBool,
    last_access: Mutex<Instant>,
    termination_reason: Mutex<Option<String>>,
    reader_tasks: AsyncMutex<Vec<crate::async_runtime::JoinHandle<()>>>,
    harness_metadata: Option<SessionHarnessMetadata>,
    owner_scope: Option<String>,
    transport_session_id: Option<String>,
    harness_finalized: AtomicBool,
    terminal_observed: AtomicBool,
    externally_retained: AtomicBool,
    resource_lease: Mutex<Option<ExecutionLease>>,
    execution_resources: Option<Value>,
    expected_exit_codes: Vec<i32>,
    durable_job: Option<DurableJobHandle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHarnessMetadata {
    pub task_id: String,
    pub command: String,
    pub recovery_key: Option<String>,
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
            None,
        )
    }

    pub fn new_with_details(
        child: Child,
        interactive: bool,
        command: String,
        resolved_cwd: String,
        harness_metadata: Option<SessionHarnessMetadata>,
        owner_scope: Option<String>,
    ) -> Self {
        Self::new_with_details_and_encoding(
            child,
            interactive,
            command,
            resolved_cwd,
            harness_metadata,
            owner_scope,
            StreamEncoding::Utf8,
        )
    }

    pub fn new_with_details_and_encoding(
        child: Child,
        interactive: bool,
        command: String,
        resolved_cwd: String,
        harness_metadata: Option<SessionHarnessMetadata>,
        owner_scope: Option<String>,
        output_encoding: StreamEncoding,
    ) -> Self {
        Self::new_with_details_encoding_and_resources(
            child,
            interactive,
            command,
            resolved_cwd,
            harness_metadata,
            owner_scope,
            None,
            output_encoding,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_details_encoding_and_resources(
        mut child: Child,
        interactive: bool,
        command: String,
        resolved_cwd: String,
        harness_metadata: Option<SessionHarnessMetadata>,
        owner_scope: Option<String>,
        transport_session_id: Option<String>,
        output_encoding: StreamEncoding,
        resource_lease: Option<ExecutionLease>,
    ) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let stdin = child.stdin.take();
        let stdin_open = stdin.is_some();
        let started_at_iso = timestamp();
        let execution_resources = resource_lease.as_ref().map(ExecutionLease::to_value);
        Self {
            session_id,
            child: AsyncMutex::new(Some(child)),
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
            finished_at: Mutex::new(None),
            finished_at_iso: Mutex::new(None),
            last_output_at: Mutex::new(started_at_iso),
            exit_code: Mutex::new(None),
            exited: AtomicBool::new(false),
            last_access: Mutex::new(Instant::now()),
            termination_reason: Mutex::new(None),
            reader_tasks: AsyncMutex::new(Vec::new()),
            harness_metadata,
            owner_scope,
            transport_session_id,
            harness_finalized: AtomicBool::new(false),
            terminal_observed: AtomicBool::new(false),
            externally_retained: AtomicBool::new(false),
            resource_lease: Mutex::new(resource_lease),
            execution_resources,
            expected_exit_codes: vec![0],
            durable_job: None,
        }
    }

    fn from_durable_job(job: DurableJobHandle, state: DurableCommandState) -> Self {
        let exited = !matches!(state.status.as_str(), "starting" | "running");
        Self {
            session_id: job.spec.session_id.clone(),
            child: AsyncMutex::new(None),
            stdin: AsyncMutex::new(None),
            stdin_open: Mutex::new(state.stdin_open),
            interactive: false,
            command: job.spec.command.clone(),
            resolved_cwd: job.spec.cwd.clone(),
            output_encoding: StreamEncoding::Utf8,
            stdout: Mutex::new(StreamBuffer::default()),
            stderr: Mutex::new(StreamBuffer::default()),
            started_at: Instant::now(),
            started_at_iso: state.started_at.clone(),
            finished_at: Mutex::new(exited.then(Instant::now)),
            finished_at_iso: Mutex::new(state.finished_at.clone()),
            last_output_at: Mutex::new(state.last_output_at.clone()),
            exit_code: Mutex::new(state.exit_code),
            exited: AtomicBool::new(exited),
            last_access: Mutex::new(Instant::now()),
            termination_reason: Mutex::new(Some(state.termination_reason.clone())),
            reader_tasks: AsyncMutex::new(Vec::new()),
            harness_metadata: job.spec.harness_metadata.clone(),
            owner_scope: job.spec.owner_scope.clone(),
            transport_session_id: job.spec.transport_session_id.clone(),
            harness_finalized: AtomicBool::new(job.paths.harness_finalized.is_file()),
            terminal_observed: AtomicBool::new(job.paths.observed.is_file()),
            externally_retained: AtomicBool::new(true),
            resource_lease: Mutex::new(None),
            execution_resources: job.spec.execution_resources.clone(),
            expected_exit_codes: job.spec.expected_exit_codes.clone(),
            durable_job: Some(job),
        }
    }

    pub(crate) fn attach_durable_job(mut self, job: DurableJobHandle) -> Self {
        self.session_id = job.spec.session_id.clone();
        self.started_at_iso = job.spec.started_at.clone();
        self.durable_job = Some(job);
        self.externally_retained.store(true, Ordering::Release);
        self
    }

    pub fn is_durable(&self) -> bool {
        self.durable_job.is_some()
    }

    pub fn with_expected_exit_codes(mut self, mut expected_exit_codes: Vec<i32>) -> Self {
        expected_exit_codes.sort_unstable();
        expected_exit_codes.dedup();
        if expected_exit_codes.is_empty() {
            expected_exit_codes.push(0);
        }
        self.expected_exit_codes = expected_exit_codes;
        self
    }

    pub fn harness_metadata(&self) -> Option<SessionHarnessMetadata> {
        self.harness_metadata.clone()
    }

    pub fn owner_scope(&self) -> Option<&str> {
        self.owner_scope.as_deref()
    }

    pub fn mark_externally_retained(&self) {
        self.externally_retained.store(true, Ordering::Release);
    }

    fn externally_retained(&self) -> bool {
        self.externally_retained.load(Ordering::Acquire)
    }

    pub fn terminal_observed(&self) -> bool {
        if let Some(job) = &self.durable_job {
            if job.terminal_observed() {
                self.terminal_observed.store(true, Ordering::Release);
            }
        }
        self.terminal_observed.load(Ordering::Acquire)
    }

    pub fn mark_terminal_observed(&self) {
        if self.has_exited() && !self.terminal_observed.swap(true, Ordering::AcqRel) {
            if let Some(job) = &self.durable_job {
                job.mark_terminal_observed();
            }
            self.touch();
        }
    }

    pub fn mark_harness_finalized(&self) -> bool {
        if let Some(job) = &self.durable_job {
            let created = job.mark_harness_finalized();
            if created {
                self.harness_finalized.store(true, Ordering::Release);
            }
            return created;
        }
        self.harness_finalized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub async fn spawn_readers(self: &Arc<Self>) {
        if self.is_durable() {
            return;
        }
        let stdout = {
            let mut guard = self.child.lock().await;
            guard.as_mut().and_then(|child| child.stdout.take())
        };
        let stderr = {
            let mut guard = self.child.lock().await;
            guard.as_mut().and_then(|child| child.stderr.take())
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
        if let Some(job) = &self.durable_job {
            let _ = job.enqueue_control(&DurableControl {
                action: "signal".into(),
                chars: None,
                signal: Some("KILL".into()),
            });
            for _ in 0..100 {
                self.refresh_status().await;
                if self.has_exited() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            return;
        }
        let status = {
            let mut child = self.child.lock().await;
            let Some(child) = child.as_mut() else {
                return;
            };
            let _ = child.start_kill();
            child.wait().await.ok()
        };
        if let Some(status) = status {
            self.record_exit_status(status);
        }
    }

    pub async fn refresh_status(&self) {
        if let Some(job) = &self.durable_job {
            self.refresh_durable_status(job);
            return;
        }
        let mut child = self.child.lock().await;
        let Some(child) = child.as_mut() else {
            return;
        };
        if let Ok(Some(status)) = child.try_wait() {
            self.record_exit_status(status);
        }
    }

    fn refresh_durable_status(&self, job: &DurableJobHandle) {
        let Ok(mut state) = job.read_state() else {
            return;
        };
        let supervisor_lost = state
            .supervisor_pid
            .is_some_and(|pid| !crate::platform::platform().is_process_alive(pid));
        let launcher_never_published_pid = state.status == "starting"
            && state.supervisor_pid.is_none()
            && chrono::DateTime::parse_from_rfc3339(&state.started_at)
                .ok()
                .and_then(|started| {
                    Utc::now()
                        .signed_duration_since(started.with_timezone(&Utc))
                        .to_std()
                        .ok()
                })
                .is_some_and(|elapsed| elapsed > Duration::from_secs(5));
        if matches!(state.status.as_str(), "starting" | "running")
            && (supervisor_lost || launcher_never_published_pid)
        {
            state.status = "interrupted".into();
            state.termination_reason = if launcher_never_published_pid {
                "launch_interrupted".into()
            } else {
                "supervisor_lost".into()
            };
            state.finished_at = Some(timestamp());
            state.stdin_open = false;
            let _ = write_json_atomic(&job.paths.state, &state);
        }
        let exited = !matches!(state.status.as_str(), "starting" | "running");
        *self.exit_code.lock().expect("exit_code lock") = state.exit_code;
        *self.stdin_open.lock().expect("stdin_open lock") = state.stdin_open;
        *self.last_output_at.lock().expect("last output lock") = state.last_output_at.clone();
        *self.termination_reason.lock().expect("termination lock") =
            Some(state.termination_reason.clone());
        *self.finished_at_iso.lock().expect("finished_at_iso lock") = state.finished_at.clone();
        if exited && !self.exited.swap(true, Ordering::AcqRel) {
            *self.finished_at.lock().expect("finished_at lock") = Some(Instant::now());
            self.resource_lease
                .lock()
                .expect("resource lease lock")
                .take();
            self.touch();
        }
    }

    fn record_exit_status(&self, status: std::process::ExitStatus) {
        if self.has_exited() {
            return;
        }
        *self.exit_code.lock().expect("exit_code lock") = status.code();
        let mut finished_at = self.finished_at.lock().expect("finished_at lock");
        if finished_at.is_none() {
            *finished_at = Some(Instant::now());
            *self.finished_at_iso.lock().expect("finished_at_iso lock") = Some(timestamp());
        }
        self.exited.store(true, Ordering::Release);
        *self.stdin_open.lock().expect("stdin_open lock") = false;
        let mut reason = self.termination_reason.lock().expect("termination lock");
        if reason.is_none() {
            *reason = Some("exited".into());
        }
        self.resource_lease
            .lock()
            .expect("resource lease lock")
            .take();
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
        if let Some(job) = &self.durable_job {
            return job.retained_stream_bytes(stream);
        }
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
        if let Some(job) = &self.durable_job {
            self.refresh_durable_status(job);
        }
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
            "exited" | "late_success" => {
                Some(exit_code.is_some_and(|code| self.expected_exit_codes.contains(&code)))
            }
            "running" => None,
            _ => Some(false),
        };
        let execution_status = match reason {
            "running" => "running",
            "exited" | "late_success" if command_ok == Some(true) => "succeeded",
            "exited" => "failed",
            "cancelled" => "cancelled",
            "timeout" => "timed_out",
            "killed" => "killed",
            "spawn_failed" => "spawn_failed",
            "server_restart" | "supervisor_lost" | "launch_interrupted" => "interrupted",
            _ => "failed",
        };
        let retryable = matches!(
            reason,
            "timeout"
                | "killed"
                | "spawn_failed"
                | "server_restart"
                | "supervisor_lost"
                | "launch_interrupted"
        );
        let session_age_ms = self
            .durable_job
            .as_ref()
            .and_then(|_| chrono::DateTime::parse_from_rfc3339(&self.started_at_iso).ok())
            .map(|started| {
                Utc::now()
                    .signed_duration_since(started.with_timezone(&Utc))
                    .num_milliseconds()
                    .max(0) as u128
            })
            .unwrap_or_else(|| self.started_at.elapsed().as_millis());
        let finished_at = *self.finished_at.lock().expect("finished_at lock");
        let durable_finished = self
            .finished_at_iso
            .lock()
            .expect("finished_at_iso lock")
            .clone()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok());
        let durable_started = chrono::DateTime::parse_from_rfc3339(&self.started_at_iso).ok();
        let execution_duration_ms = match (durable_started, durable_finished) {
            (Some(started), Some(finished)) => finished
                .signed_duration_since(started)
                .num_milliseconds()
                .max(0) as u128,
            _ => finished_at
                .map(|finished_at| finished_at.duration_since(self.started_at).as_millis())
                .unwrap_or(session_age_ms),
        };
        let retained_ms = durable_finished
            .map(|finished| {
                Utc::now()
                    .signed_duration_since(finished.with_timezone(&Utc))
                    .num_milliseconds()
                    .max(0) as u128
            })
            .or_else(|| finished_at.map(|finished_at| finished_at.elapsed().as_millis()))
            .unwrap_or(0);
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
                "supervisor_lost" | "launch_interrupted" => {
                    "durable supervisor did not complete; start a new command rather than replaying this session"
                }
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
            "elapsed_ms": execution_duration_ms,
            "execution_duration_ms": execution_duration_ms,
            "session_age_ms": session_age_ms,
            "retained_ms": retained_ms,
            "started_at": self.started_at_iso,
            "finished_at": self.finished_at_iso.lock().expect("finished_at_iso lock").clone(),
            "last_output_at": self.last_output_at.lock().expect("last output lock").clone(),
            "result_observed": self.terminal_observed(),
            "output_refs": {
                "stdout": format!("session:{}:stdout", self.session_id),
                "stderr": format!("session:{}:stderr", self.session_id)
            },
            "execution_resources": self.execution_resources
            ,"process_bound": !self.is_durable()
            ,"durable": self.is_durable()
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

pub fn read_output(store: &CommandSessionStore, args: &Value) -> Result<Value, WorkspaceError> {
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

pub fn write_stdin(store: &CommandSessionStore, args: &Value) -> Result<Value, WorkspaceError> {
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
        session.mark_terminal_observed();
        return Ok(finalize_execution_result(
            session.snapshot(max_output_bytes),
        ));
    }

    if !chars.is_empty() {
        if let Some(job) = session.durable_job.as_ref() {
            if !session
                .snapshot(0)
                .get("stdin_open")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: "Session stdin is closed.".into(),
                    category: "runtime",
                    retryable: false,
                });
            }
            job.enqueue_control(&DurableControl {
                action: "stdin".into(),
                chars: Some(chars.to_string()),
                signal: None,
            })?;
        } else {
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
    }

    let yield_ms = args
        .get("yield_time_ms")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .min(30_000);
    std::thread::sleep(std::time::Duration::from_millis(yield_ms));
    crate::async_runtime::block_on(session.refresh_status());
    session.mark_terminal_observed();
    Ok(finalize_execution_result(
        session.snapshot(max_output_bytes),
    ))
}

pub fn kill_session(store: &CommandSessionStore, args: &Value) -> Result<Value, WorkspaceError> {
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
        if let Some(job) = session.durable_job.as_ref() {
            let _ = job.enqueue_control(&DurableControl {
                action: "signal".into(),
                chars: None,
                signal: Some(signal.to_string()),
            });
            crate::async_runtime::block_on(async {
                let deadline = Instant::now() + Duration::from_millis(wait_ms);
                while Instant::now() < deadline {
                    session.refresh_status().await;
                    if session.has_exited() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            });
        } else {
            crate::async_runtime::block_on(async {
                let pid = {
                    let child = session.child.lock().await;
                    child.as_ref().and_then(|child| child.id())
                };
                if let Some(pid) = pid {
                    send_session_signal(pid, signal);
                } else {
                    let mut child = session.child.lock().await;
                    if let Some(child) = child.as_mut() {
                        let _ = child.start_kill();
                    }
                }
                let _ = tokio::time::timeout(std::time::Duration::from_millis(wait_ms), async {
                    let mut child = session.child.lock().await;
                    if let Some(child) = child.as_mut() {
                        let _ = child.wait().await;
                    }
                })
                .await;
            });
        }
        crate::async_runtime::block_on(session.refresh_status());
        if crate::async_runtime::block_on(session.is_running()) {
            status = "terminating";
            evicted = false;
        } else {
            killed = true;
            status = "killed";
        }
    }

    if evicted {
        session.mark_terminal_observed();
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
