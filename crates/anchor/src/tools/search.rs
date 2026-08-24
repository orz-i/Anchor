use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::tools::file;
use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};
use crate::tools::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Text,
    Symbol,
    Callers,
    Callees,
    Impact,
    Explore,
}

enum SemanticSearchAttempt {
    Result(SemanticSearchResult),
    Degraded(String),
}

#[derive(Debug)]
enum CodeGraphRunError {
    Cancelled,
    Failure(String),
}

struct CodeGraphOutput {
    stdout: String,
}

struct CodeGraphCapture {
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Drop for CodeGraphCapture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.stdout);
        let _ = fs::remove_file(&self.stderr);
    }
}

static CODEGRAPH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const CODEGRAPH_POLL_INTERVAL: Duration = Duration::from_millis(15);
const MAX_CODEGRAPH_OUTPUT_BYTES: u64 = 2 * 1024 * 1024;

impl SearchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Symbol => "symbol",
            Self::Callers => "callers",
            Self::Callees => "callees",
            Self::Impact => "impact",
            Self::Explore => "explore",
        }
    }

    fn is_semantic(self) -> bool {
        !matches!(self, Self::Text)
    }
}

struct SemanticSearchResult {
    engine: &'static str,
    data: Value,
    warnings: Vec<String>,
}

/// Unified code-search entry point. Concrete engines remain an implementation
/// detail so the public contract can evolve independently from ripgrep or
/// CodeGraph process semantics.
pub fn search(
    ws: &Workspace,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("query is required"))?;
    let requested_mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
    let mode = resolve_mode(requested_mode, query, args)?;

    if mode.is_semantic() {
        match semantic_search(ws, query, mode, args, cancellation)? {
            SemanticSearchAttempt::Result(result) => {
                if requested_mode != "auto" || !semantic_result_is_empty(&result.data) {
                    return Ok(tool_ok(json!({
                        "query": query,
                        "requested_mode": requested_mode,
                        "mode": mode.as_str(),
                        "engine": result.engine,
                        "degraded": false,
                        "degraded_reason": null,
                        "data": result.data,
                        "warnings": result.warnings
                    })));
                }
                return text_search(
                    ws,
                    query,
                    requested_mode,
                    mode,
                    args,
                    cancellation,
                    Some("semantic search returned no results; auto mode fell back to text search"),
                );
            }
            SemanticSearchAttempt::Degraded(reason) => {
                return text_search(
                    ws,
                    query,
                    requested_mode,
                    mode,
                    args,
                    cancellation,
                    Some(&reason),
                );
            }
        }
    }

    text_search(ws, query, requested_mode, mode, args, cancellation, None)
}

fn resolve_mode(
    requested_mode: &str,
    query: &str,
    args: &Value,
) -> Result<SearchMode, WorkspaceError> {
    let explicit = match requested_mode {
        "auto" => None,
        "text" => Some(SearchMode::Text),
        "symbol" => Some(SearchMode::Symbol),
        "callers" => Some(SearchMode::Callers),
        "callees" => Some(SearchMode::Callees),
        "impact" => Some(SearchMode::Impact),
        "explore" => Some(SearchMode::Explore),
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "mode must be auto, text, symbol, callers, callees, impact, or explore",
            ))
        }
    };
    if let Some(mode) = explicit {
        return Ok(mode);
    }

    if text_specific_arguments_present(args) {
        return Ok(SearchMode::Text);
    }
    if query.chars().any(char::is_whitespace) {
        return Ok(SearchMode::Explore);
    }
    if looks_like_symbol(query) {
        return Ok(SearchMode::Symbol);
    }
    Ok(SearchMode::Text)
}

fn text_specific_arguments_present(args: &Value) -> bool {
    args.get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path != ".")
        || args.get("regex").and_then(Value::as_bool) == Some(true)
        || args
            .get("include_globs")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
        || args
            .get("exclude_globs")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
        || args
            .get("context_lines")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
        || args.get("cursor").and_then(Value::as_u64).unwrap_or(0) > 0
        || args
            .get("output_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode != "matches")
}

fn looks_like_symbol(query: &str) -> bool {
    !query.is_empty()
        && query.len() <= 256
        && query
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | ':' | '.' | '#' | '$'))
}

fn text_search(
    ws: &Workspace,
    query: &str,
    requested_mode: &str,
    resolved_mode: SearchMode,
    args: &Value,
    cancellation: &CancellationToken,
    degraded_reason: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let mut data = file::grep(ws, args, cancellation)?;
    if let Some(object) = data.as_object_mut() {
        object.remove("ok");
    }
    let engine = data
        .pointer("/scan/engine")
        .and_then(Value::as_str)
        .unwrap_or("anchor");
    let mut warnings = Vec::new();
    if let Some(reason) = degraded_reason {
        warnings.push(reason.to_string());
    }
    Ok(tool_ok(json!({
        "query": query,
        "requested_mode": requested_mode,
        "mode": if degraded_reason.is_some() { "text" } else { resolved_mode.as_str() },
        "engine": engine,
        "degraded": degraded_reason.is_some(),
        "degraded_reason": degraded_reason,
        "data": data,
        "warnings": warnings
    })))
}

fn semantic_search(
    ws: &Workspace,
    query: &str,
    mode: SearchMode,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<SemanticSearchAttempt, WorkspaceError> {
    let Some(program) = crate::tunnel::resolve_codegraph() else {
        return Ok(SemanticSearchAttempt::Degraded(
            "semantic backend unavailable; fell back to text search".into(),
        ));
    };
    semantic_search_with_program(ws, &program, query, mode, args, cancellation)
}

fn semantic_search_with_program(
    ws: &Workspace,
    program: &Path,
    query: &str,
    mode: SearchMode,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<SemanticSearchAttempt, WorkspaceError> {
    if let Err(error) = ws.ensure_child_process_boundary() {
        return Ok(SemanticSearchAttempt::Degraded(format!(
            "semantic backend workspace boundary unavailable: {}; fell back to text search",
            error.message()
        )));
    }
    let timeout = graph_timeout(args);
    let _guard = match acquire_codegraph_lock(cancellation, timeout) {
        Ok(guard) => guard,
        Err(CodeGraphRunError::Cancelled) => return Err(cancelled_error()),
        Err(CodeGraphRunError::Failure(reason)) => {
            return Ok(SemanticSearchAttempt::Degraded(reason))
        }
    };

    if let Err(error) = prepare_codegraph_index(program, ws.root(), timeout, cancellation) {
        return match error {
            CodeGraphRunError::Cancelled => Err(cancelled_error()),
            CodeGraphRunError::Failure(reason) => Ok(SemanticSearchAttempt::Degraded(format!(
                "CodeGraph index unavailable: {reason}; fell back to text search"
            ))),
        };
    }

    let command_args = semantic_command_args(mode, query, args);
    let output = match run_codegraph(program, ws.root(), &command_args, timeout, cancellation) {
        Ok(output) => output,
        Err(CodeGraphRunError::Cancelled) => return Err(cancelled_error()),
        Err(CodeGraphRunError::Failure(reason)) => {
            return Ok(SemanticSearchAttempt::Degraded(format!(
                "CodeGraph query failed: {reason}; fell back to text search"
            )))
        }
    };
    let data = if matches!(mode, SearchMode::Explore) {
        Value::String(output.stdout.trim().to_string())
    } else {
        match serde_json::from_str::<Value>(&output.stdout) {
            Ok(value) => value,
            Err(error) => {
                return Ok(SemanticSearchAttempt::Degraded(format!(
                    "CodeGraph returned invalid JSON: {error}; fell back to text search"
                )))
            }
        }
    };
    Ok(SemanticSearchAttempt::Result(SemanticSearchResult {
        engine: "codegraph",
        data,
        warnings: Vec::new(),
    }))
}

fn graph_timeout(args: &Value) -> Duration {
    Duration::from_millis(
        args.get("graph_timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(60_000)
            .clamp(1_000, 120_000),
    )
}

fn graph_limit(args: &Value) -> usize {
    args.get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .clamp(1, 10_000) as usize
}

fn graph_depth(args: &Value) -> usize {
    args.get("graph_depth")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(1, 10) as usize
}

fn semantic_command_args(mode: SearchMode, query: &str, args: &Value) -> Vec<String> {
    let limit = graph_limit(args).to_string();
    match mode {
        SearchMode::Symbol => vec![
            "query".into(),
            query.into(),
            "--limit".into(),
            limit,
            "--json".into(),
        ],
        SearchMode::Callers => vec![
            "callers".into(),
            query.into(),
            "--limit".into(),
            limit,
            "--json".into(),
        ],
        SearchMode::Callees => vec![
            "callees".into(),
            query.into(),
            "--limit".into(),
            limit,
            "--json".into(),
        ],
        SearchMode::Impact => vec![
            "impact".into(),
            query.into(),
            "--depth".into(),
            graph_depth(args).to_string(),
            "--json".into(),
        ],
        SearchMode::Explore => vec!["explore".into(), query.into()],
        SearchMode::Text => Vec::new(),
    }
}

fn prepare_codegraph_index(
    program: &Path,
    root: &Path,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<(), CodeGraphRunError> {
    let database = root.join(".codegraph").join("codegraph.db");
    let root_arg = root.to_string_lossy().to_string();
    if !database.is_file() {
        let init = run_codegraph(
            program,
            root,
            &["init".into(), root_arg.clone()],
            timeout,
            cancellation,
        );
        let runtime_ignore = ensure_codegraph_runtime_ignore(root);
        init?;
        runtime_ignore?;
        return Ok(());
    }

    let status = run_codegraph(
        program,
        root,
        &["status".into(), root_arg.clone(), "--json".into()],
        timeout,
        cancellation,
    )?;
    let status: Value = serde_json::from_str(&status.stdout).map_err(|error| {
        CodeGraphRunError::Failure(format!("status returned invalid JSON: {error}"))
    })?;
    let refresh = if status_requires_reindex(&status) {
        run_codegraph(
            program,
            root,
            &["index".into(), root_arg, "--quiet".into()],
            timeout,
            cancellation,
        )
    } else {
        run_codegraph(
            program,
            root,
            &["sync".into(), root_arg, "--quiet".into()],
            timeout,
            cancellation,
        )
    };
    let runtime_ignore = ensure_codegraph_runtime_ignore(root);
    refresh?;
    runtime_ignore?;
    Ok(())
}

fn status_requires_reindex(status: &Value) -> bool {
    status.get("initialized").and_then(Value::as_bool) == Some(false)
        || status
            .get("worktreeMismatch")
            .is_some_and(|mismatch| !mismatch.is_null())
        || status
            .pointer("/index/reindexRecommended")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || status
            .pointer("/index/state")
            .and_then(Value::as_str)
            .map(|state| !matches!(state, "complete" | "ready"))
            .unwrap_or(true)
        || status
            .pointer("/index/pendingRefs")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
}

fn ensure_codegraph_runtime_ignore(root: &Path) -> Result<(), CodeGraphRunError> {
    let directory = root.join(".codegraph");
    if !directory.is_dir() {
        return Ok(());
    }
    fs::write(directory.join(".gitignore"), "*\n").map_err(|error| {
        CodeGraphRunError::Failure(format!(
            "unable to isolate CodeGraph runtime artifacts: {error}"
        ))
    })
}

fn acquire_codegraph_lock(
    cancellation: &CancellationToken,
    timeout: Duration,
) -> Result<MutexGuard<'static, ()>, CodeGraphRunError> {
    let lock = CODEGRAPH_LOCK.get_or_init(|| Mutex::new(()));
    let started = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            return Err(CodeGraphRunError::Cancelled);
        }
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(CodeGraphRunError::Failure(
                    "semantic backend admission lock is unavailable".into(),
                ))
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                if started.elapsed() >= timeout {
                    return Err(CodeGraphRunError::Failure(
                        "semantic backend admission timed out".into(),
                    ));
                }
                std::thread::sleep(CODEGRAPH_POLL_INTERVAL);
            }
        }
    }
}

fn run_codegraph(
    program: &Path,
    root: &Path,
    args: &[String],
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<CodeGraphOutput, CodeGraphRunError> {
    let capture = codegraph_capture()?;
    let stdout = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&capture.stdout)
        .map_err(|error| CodeGraphRunError::Failure(format!("stdout capture failed: {error}")))?;
    let stderr = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&capture.stderr)
        .map_err(|error| CodeGraphRunError::Failure(format!("stderr capture failed: {error}")))?;
    let mut command = command_for_codegraph(program, args);
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .env("CODEGRAPH_TELEMETRY", "0")
        .env("CODEGRAPH_NO_UPDATE_CHECK", "1")
        .env("DO_NOT_TRACK", "1")
        .env("CI", "1")
        .env("NO_COLOR", "1");
    let mut child = command.spawn().map_err(|error| {
        CodeGraphRunError::Failure(format!("unable to start CodeGraph: {error}"))
    })?;
    let pid = child.id();
    let started = Instant::now();
    let status = loop {
        if cancellation.is_cancelled() {
            terminate_codegraph_process_tree(&mut child, pid);
            return Err(CodeGraphRunError::Cancelled);
        }
        if started.elapsed() >= timeout {
            terminate_codegraph_process_tree(&mut child, pid);
            return Err(CodeGraphRunError::Failure(format!(
                "timed out after {} ms",
                timeout.as_millis()
            )));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(CODEGRAPH_POLL_INTERVAL),
            Err(error) => {
                terminate_codegraph_process_tree(&mut child, pid);
                return Err(CodeGraphRunError::Failure(format!(
                    "unable to observe CodeGraph process: {error}"
                )));
            }
        }
    };
    let stdout = read_codegraph_capture(&capture.stdout, "stdout")?;
    let stderr = read_codegraph_capture(&capture.stderr, "stderr")?;
    if !status.success() {
        let summary = stderr.trim();
        return Err(CodeGraphRunError::Failure(if summary.is_empty() {
            format!("exited with status {status}")
        } else {
            format!("exited with status {status}: {summary}")
        }));
    }
    Ok(CodeGraphOutput { stdout })
}

fn codegraph_capture() -> Result<CodeGraphCapture, CodeGraphRunError> {
    let id = Uuid::new_v4().simple();
    let root = std::env::temp_dir();
    let stdout = root.join(format!("anchor-codegraph-{id}.stdout"));
    let stderr = root.join(format!("anchor-codegraph-{id}.stderr"));
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&stdout)
        .and_then(|_| {
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&stderr)
        })
        .map_err(|error| {
            let _ = fs::remove_file(&stdout);
            let _ = fs::remove_file(&stderr);
            CodeGraphRunError::Failure(format!("unable to create CodeGraph capture: {error}"))
        })?;
    Ok(CodeGraphCapture { stdout, stderr })
}

fn read_codegraph_capture(path: &Path, stream: &str) -> Result<String, CodeGraphRunError> {
    let size = fs::metadata(path)
        .map_err(|error| {
            CodeGraphRunError::Failure(format!("unable to inspect {stream}: {error}"))
        })?
        .len();
    if size > MAX_CODEGRAPH_OUTPUT_BYTES {
        return Err(CodeGraphRunError::Failure(format!(
            "{stream} exceeded {} bytes",
            MAX_CODEGRAPH_OUTPUT_BYTES
        )));
    }
    fs::read_to_string(path)
        .map_err(|error| CodeGraphRunError::Failure(format!("unable to read {stream}: {error}")))
}

fn terminate_codegraph_process_tree(child: &mut std::process::Child, pid: u32) {
    let _ = crate::platform::platform().terminate_process_tree(pid);
    let _ = child.kill();
    let _ = child.wait();
}

fn command_for_codegraph(program: &Path, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let extension = program
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(extension.as_deref(), Some("bat") | Some("cmd")) {
            let mut command = Command::new("cmd.exe");
            command.args(["/d", "/s", "/c"]);
            command.raw_arg(windows_codegraph_command_line(program, args));
            return command;
        }
    }
    let mut command = Command::new(program);
    command.args(args);
    command
}

#[cfg(windows)]
fn windows_codegraph_command_line(program: &Path, args: &[String]) -> String {
    let mut command = format!(
        "call {}",
        windows_codegraph_token(&program.to_string_lossy())
    );
    for arg in args {
        command.push(' ');
        command.push_str(&windows_codegraph_token(arg));
    }
    command
}

#[cfg(windows)]
fn windows_codegraph_token(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn cancelled_error() -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "REQUEST_CANCELLED",
        message: "Tool request was cancelled".into(),
        category: "runtime",
        retryable: true,
        details: json!({"reason": "client_cancelled", "retryable": true}),
    }
}

fn semantic_result_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn auto_mode_prefers_text_for_explicit_text_controls() {
        let args = json!({"query": "Handler", "regex": true});
        assert_eq!(
            resolve_mode("auto", "Handler", &args).expect("mode"),
            SearchMode::Text
        );
    }

    #[test]
    fn auto_mode_preserves_scoped_path_as_text_search() {
        let args = json!({"query": "Handler", "path": "src/tools"});
        assert_eq!(
            resolve_mode("auto", "Handler", &args).expect("mode"),
            SearchMode::Text
        );
    }

    #[test]
    fn auto_mode_routes_identifier_to_symbol() {
        let args = json!({"query": "dispatch::call_tool"});
        assert_eq!(
            resolve_mode("auto", "dispatch::call_tool", &args).expect("mode"),
            SearchMode::Symbol
        );
    }

    #[test]
    fn auto_mode_routes_natural_language_to_explore() {
        let args = json!({"query": "how does tool dispatch work"});
        assert_eq!(
            resolve_mode("auto", "how does tool dispatch work", &args).expect("mode"),
            SearchMode::Explore
        );
    }

    #[test]
    fn semantic_mode_degrades_to_structured_text_search_when_backend_is_unavailable() {
        let root = tempdir().expect("workspace");
        std::fs::write(
            root.path().join("lib.rs"),
            "fn dispatch() { println!(\"dispatch\"); }\n",
        )
        .expect("source");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");

        let output = search(
            &workspace,
            &json!({"query": "dispatch", "mode": "callers"}),
            &CancellationToken::default(),
        )
        .expect("search");

        assert_eq!(output["ok"], true);
        assert_eq!(output["requested_mode"], "callers");
        assert_eq!(output["mode"], "text");
        assert_eq!(output["degraded"], true);
        assert_eq!(output["data"]["total_matches"], 1);
        assert!(output["degraded_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("semantic backend unavailable")));
    }

    #[test]
    fn semantic_command_arguments_are_stable_and_bounded() {
        let args = json!({"max_results": 12, "graph_depth": 4});
        assert_eq!(
            semantic_command_args(SearchMode::Symbol, "dispatch", &args),
            vec!["query", "dispatch", "--limit", "12", "--json"]
        );
        assert_eq!(
            semantic_command_args(SearchMode::Callers, "dispatch", &args),
            vec!["callers", "dispatch", "--limit", "12", "--json"]
        );
        assert_eq!(
            semantic_command_args(SearchMode::Callees, "dispatch", &args),
            vec!["callees", "dispatch", "--limit", "12", "--json"]
        );
        assert_eq!(
            semantic_command_args(SearchMode::Impact, "dispatch", &args),
            vec!["impact", "dispatch", "--depth", "4", "--json"]
        );
        assert_eq!(
            semantic_command_args(SearchMode::Explore, "request flow", &args),
            vec!["explore", "request flow"]
        );
    }

    #[test]
    fn codegraph_status_reindexes_only_when_index_health_requires_it() {
        let healthy = json!({
            "initialized": true,
            "worktreeMismatch": null,
            "index": {"state": "complete", "reindexRecommended": false, "pendingRefs": 0}
        });
        assert!(!status_requires_reindex(&healthy));
        assert!(status_requires_reindex(&json!({
            "initialized": true,
            "worktreeMismatch": {"worktreeRoot": "/tmp/wt", "indexRoot": "/tmp/main"},
            "index": {"state": "complete", "reindexRecommended": false, "pendingRefs": 0}
        })));
        assert!(status_requires_reindex(&json!({
            "initialized": true,
            "worktreeMismatch": null,
            "index": {"state": "partial", "reindexRecommended": false, "pendingRefs": 0}
        })));
        assert!(status_requires_reindex(&json!({
            "initialized": true,
            "worktreeMismatch": null,
            "index": {"state": "complete", "reindexRecommended": false, "pendingRefs": 2}
        })));
    }

    #[test]
    fn codegraph_runtime_ignore_keeps_index_local_only() {
        let root = tempdir().expect("workspace");
        std::fs::create_dir(root.path().join(".codegraph")).expect("codegraph dir");
        std::fs::write(
            root.path().join(".codegraph/.gitignore"),
            "*\n!.gitignore\n",
        )
        .expect("upstream ignore");

        ensure_codegraph_runtime_ignore(root.path()).expect("runtime ignore");

        assert_eq!(
            std::fs::read_to_string(root.path().join(".codegraph/.gitignore"))
                .expect("runtime ignore"),
            "*\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fake_codegraph_runtime_initializes_then_syncs_the_worktree_index() {
        let root = tempdir().expect("workspace");
        let program = root.path().join("fake-codegraph.sh");
        std::fs::write(
            &program,
            r#"#!/bin/sh
set -eu
cmd="$1"
shift
case "$cmd" in
  init)
    project="$1"
    mkdir -p "$project/.codegraph"
    : > "$project/.codegraph/codegraph.db"
    printf 'init\n' >> "$project/codegraph.log"
    ;;
  status)
    project="$1"
    printf 'status\n' >> "$project/codegraph.log"
    printf '%s\n' '{"initialized":true,"worktreeMismatch":null,"index":{"state":"complete","reindexRecommended":false,"pendingRefs":0}}'
    ;;
  sync)
    project="$1"
    printf 'sync\n' >> "$project/codegraph.log"
    ;;
  index)
    project="$1"
    printf 'index\n' >> "$project/codegraph.log"
    ;;
  query)
    printf 'query\n' >> "$PWD/codegraph.log"
    printf '[{"name":"%s"}]\n' "$1"
    ;;
  *)
    printf 'unexpected command: %s\n' "$cmd" >&2
    exit 2
    ;;
esac
"#,
        )
        .expect("fake runtime");
        let mut permissions = std::fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).expect("executable");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let args = json!({
            "query": "dispatch",
            "mode": "symbol",
            "max_results": 20,
            "graph_timeout_ms": 5_000
        });

        let first = semantic_search_with_program(
            &workspace,
            &program,
            "dispatch",
            SearchMode::Symbol,
            &args,
            &CancellationToken::default(),
        )
        .expect("first search");
        let SemanticSearchAttempt::Result(first) = first else {
            panic!("first semantic search degraded")
        };
        assert_eq!(first.engine, "codegraph");
        assert_eq!(first.data[0]["name"], "dispatch");
        assert_eq!(
            std::fs::read_to_string(root.path().join("codegraph.log")).expect("first log"),
            "init\nquery\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join(".codegraph/.gitignore"))
                .expect("runtime ignore"),
            "*\n"
        );

        let second = semantic_search_with_program(
            &workspace,
            &program,
            "dispatch",
            SearchMode::Symbol,
            &args,
            &CancellationToken::default(),
        )
        .expect("second search");
        assert!(matches!(second, SemanticSearchAttempt::Result(_)));
        assert_eq!(
            std::fs::read_to_string(root.path().join("codegraph.log")).expect("second log"),
            "init\nquery\nstatus\nsync\nquery\n"
        );
    }
}
