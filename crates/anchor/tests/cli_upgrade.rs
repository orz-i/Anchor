#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

fn anchor() -> &'static str {
    env!("CARGO_BIN_EXE_anchor")
}

#[cfg(target_os = "linux")]
fn probe_http(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(250)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
    if stream
        .write_all(
            b"GET /__anchor_upgrade_probe__ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .is_err()
    {
        return false;
    }
    let mut response = [0_u8; 16];
    let Ok(read) = stream.read(&mut response) else {
        return false;
    };
    read >= 5 && response[..read].starts_with(b"HTTP/")
}

#[cfg(target_os = "linux")]
fn mcp_post(port: u16, body: &Value, session_id: Option<&str>) -> (u16, HashMap<String, String>) {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(1)).expect("connect MCP listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set MCP read timeout");
    let body = serde_json::to_vec(body).expect("MCP request JSON");
    let session_header = session_id
        .map(|session_id| format!("Mcp-Session-Id: {session_id}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMcp-Protocol-Version: 2025-11-25\r\n{session_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("write MCP headers");
    stream.write_all(&body).expect("write MCP body");
    stream.flush().expect("flush MCP request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read MCP response");
    let response = String::from_utf8_lossy(&response);
    let headers = response
        .split_once("\r\n\r\n")
        .map(|(headers, _)| headers)
        .unwrap_or(response.as_ref());
    let mut lines = headers.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .expect("MCP HTTP status");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    (status, headers)
}

#[cfg(target_os = "linux")]
fn initialize_mcp_session(port: u16) -> String {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "rolling-upgrade-test", "version": "1" }
        }
    });
    let (status, headers) = mcp_post(port, &initialize, None);
    assert_eq!(status, 200, "initialize must succeed");
    let session_id = headers
        .get("mcp-session-id")
        .cloned()
        .expect("initialize returns MCP session id");
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let (status, _) = mcp_post(port, &initialized, Some(&session_id));
    assert_eq!(status, 202, "initialized notification must be accepted");
    session_id
}

fn run(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(anchor())
        .arg("--config-dir")
        .arg(config_dir)
        .args(args)
        .output()
        .expect("run anchor CLI")
}

fn run_json(config_dir: &Path, args: &[&str]) -> (Output, Value) {
    let output = Command::new(anchor())
        .arg("--config-dir")
        .arg(config_dir)
        .arg("--json")
        .args(args)
        .output()
        .expect("run anchor CLI");
    let value = serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON stdout: {error}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output, value)
}

fn run_json_with_env(
    config_dir: &Path,
    args: &[&str],
    env_key: &str,
    env_value: &str,
) -> (Output, Value) {
    let output = Command::new(anchor())
        .arg("--config-dir")
        .arg(config_dir)
        .arg("--json")
        .args(args)
        .env(env_key, env_value)
        .output()
        .expect("run anchor CLI with env");
    let value = serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON stdout: {error}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output, value)
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn register_workspace(config_dir: &Path, workspace: &Path, name: &str) -> Value {
    let (output, value) = run_json(
        config_dir,
        &[
            "workspace",
            "register",
            workspace.to_str().expect("workspace utf8"),
            "--name",
            name,
        ],
    );
    assert_success(&output);
    assert_eq!(value["event"], "registered");
    value
}

struct DaemonCleanup {
    config_dir: PathBuf,
    workspace_id: String,
}

impl Drop for DaemonCleanup {
    fn drop(&mut self) {
        let _ = run(
            &self.config_dir,
            &["stop", &self.workspace_id, "--timeout", "5", "--force"],
        );
    }
}

#[test]
fn upgrade_requires_an_explicit_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = run(temp.path(), &["upgrade"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("upgrade 缺少目标"));
}

#[test]
fn upgrade_rejects_all_combined_with_explicit_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = run(temp.path(), &["upgrade", "workspace-a", "--all"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("upgrade --all 不能与显式 workspace 同时使用"));
}

#[test]
fn gateway_dry_run_is_safe_when_gateway_is_stopped() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (output, value) = run_json(temp.path(), &["upgrade", "--gateway", "--dry-run"]);

    assert_success(&output);
    assert_eq!(value["event"], "runtime_upgrade_plan");
    assert_eq!(value["dryRun"], true);
    assert_eq!(value["results"].as_array().expect("results").len(), 1);
    assert_eq!(value["results"][0]["targetKind"], "gateway");
    assert_eq!(value["results"][0]["status"], "skipped");
    assert!(value["currentBuild"]["gitSha"].is_string());
}

#[test]
fn explicit_workspace_aliases_are_deduplicated_before_preflight() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let registered = register_workspace(temp.path(), &workspace, "upgrade-dedupe");
    let workspace_id = registered["workspace"]["id"]
        .as_str()
        .expect("workspace id");

    let (output, value) = run_json(
        temp.path(),
        &["upgrade", workspace_id, "upgrade-dedupe", "--dry-run"],
    );

    assert_success(&output);
    let results = value["results"].as_array().expect("results");
    assert_eq!(results.len(), 1, "{value}");
    assert_eq!(results[0]["workspaceId"], workspace_id);
    assert_eq!(results[0]["status"], "skipped");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_upgrade_replaces_a_running_generation_and_verifies_current_build() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let registered = register_workspace(temp.path(), &workspace, "upgrade-live");
    let workspace_id = registered["workspace"]["id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let mcp_port = registered["mcpPort"].as_u64().expect("MCP port") as u16;

    let (start, started) = run_json(
        temp.path(),
        &["start", &workspace_id, "--service", "mcp", "--wait", "15"],
    );
    assert_success(&start);
    let old_pid = started["pid"].as_u64().expect("old pid") as u32;
    let _cleanup = DaemonCleanup {
        config_dir: temp.path().to_path_buf(),
        workspace_id: workspace_id.clone(),
    };

    let state_path = temp.path().join("run").join(format!("{workspace_id}.json"));
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).expect("daemon state"))
        .expect("daemon state json");
    assert_eq!(state["pid"], old_pid);
    let actual_git = state["buildIdentity"]["gitSha"]
        .as_str()
        .expect("daemon build git")
        .to_string();
    state["buildIdentity"]["gitSha"] = Value::String("simulated-previous-build".into());
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("state json"),
    )
    .expect("tamper build identity for upgrade simulation");

    assert!(probe_http(mcp_port), "pre-upgrade MCP probe must succeed");
    let stop_probe = Arc::new(AtomicBool::new(false));
    let probe_attempts = Arc::new(AtomicUsize::new(0));
    let probe_failures = Arc::new(AtomicUsize::new(0));
    let probe_thread = {
        let stop_probe = Arc::clone(&stop_probe);
        let probe_attempts = Arc::clone(&probe_attempts);
        let probe_failures = Arc::clone(&probe_failures);
        std::thread::spawn(move || {
            while !stop_probe.load(Ordering::Relaxed) {
                probe_attempts.fetch_add(1, Ordering::Relaxed);
                if !probe_http(mcp_port) {
                    probe_failures.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let (upgrade, report) = run_json(temp.path(), &["upgrade", &workspace_id, "--timeout", "20"]);
    stop_probe.store(true, Ordering::Relaxed);
    probe_thread.join().expect("upgrade probe thread");
    assert_success(&upgrade);
    assert_eq!(report["event"], "runtime_upgrade_complete");
    let result = &report["results"][0];
    assert_eq!(result["status"], "upgraded", "{report}");
    assert_eq!(result["previousPid"], old_pid);
    let new_pid = result["pid"].as_u64().expect("new pid") as u32;
    assert_ne!(new_pid, old_pid);
    assert_eq!(result["mode"], "zero_downtime_handoff", "{report}");
    assert_eq!(result["handoffSupported"], true, "{report}");
    assert!(result["handoffId"].as_str().is_some(), "{report}");
    assert_eq!(result["outageMs"], 0, "{report}");
    assert!(result["listenerReadyMs"].as_u64().is_some(), "{report}");
    assert!(
        result.get("drainMs").is_none() || result["drainMs"].is_null(),
        "self-upgrade success returns before predecessor drain completes: {report}"
    );
    assert_eq!(result["rollbackAvailable"], true);
    assert_eq!(
        result["previousBuild"]["gitSha"],
        "simulated-previous-build"
    );
    assert_eq!(result["currentBuild"]["gitSha"], actual_git);
    assert!(
        probe_attempts.load(Ordering::Relaxed) >= 2,
        "upgrade should overlap multiple live probes"
    );
    assert_eq!(
        probe_failures.load(Ordering::Relaxed),
        0,
        "no MCP HTTP probe may fail during generation handoff"
    );
    assert!(probe_http(mcp_port), "post-upgrade MCP probe must succeed");

    let state: Value = serde_json::from_slice(&fs::read(&state_path).expect("new daemon state"))
        .expect("new daemon state json");
    assert_eq!(state["pid"], new_pid);
    assert_eq!(state["buildIdentity"]["gitSha"], actual_git);

    let rollback_dir = temp.path().join("run/upgrade-rollback");
    let retained = fs::read_dir(&rollback_dir)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(
        retained, 0,
        "successful upgrade must discard temporary rollback image"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_handoff_failure_before_cutover_keeps_predecessor_serving() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let registered = register_workspace(temp.path(), &workspace, "handoff-pre-cutover-failure");
    let workspace_id = registered["workspace"]["id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let mcp_port = registered["mcpPort"].as_u64().expect("MCP port") as u16;
    let (start, started) = run_json(
        temp.path(),
        &["start", &workspace_id, "--service", "mcp", "--wait", "15"],
    );
    assert_success(&start);
    let old_pid = started["pid"].as_u64().expect("old pid") as u32;
    let _cleanup = DaemonCleanup {
        config_dir: temp.path().to_path_buf(),
        workspace_id: workspace_id.clone(),
    };
    assert!(probe_http(mcp_port), "pre-handoff MCP probe must succeed");

    let state_path = temp.path().join("run").join(format!("{workspace_id}.json"));
    let state: Value = serde_json::from_slice(&fs::read(&state_path).expect("daemon state"))
        .expect("daemon state json");
    let socket_path = temp.path().join("run").join(format!("{workspace_id}.sock"));
    let mut stream = UnixStream::connect(&socket_path).expect("connect daemon control socket");
    let request = json!({
        "protocolVersion": 7,
        "requestId": "pre-cutover-failure",
        "method": "prepare_handoff",
        "workspaceId": workspace_id,
        "initiatorPid": std::process::id(),
        "executablePath": temp.path().join("missing-anchor").to_string_lossy(),
        "expectedBuild": state["buildIdentity"].clone(),
    });
    stream
        .write_all(format!("{request}\n").as_bytes())
        .expect("write handoff request");
    stream.flush().expect("flush handoff request");
    let mut response_line = String::new();
    BufReader::new(stream)
        .read_line(&mut response_line)
        .expect("read handoff response");
    let response: Value = serde_json::from_str(&response_line).expect("handoff response json");
    assert_eq!(response["ok"], true, "{response}");
    let result = &response["result"];
    assert_eq!(result["type"], "operation_accepted", "{response}");
    let handoff_id = result["operation_id"]
        .as_str()
        .or_else(|| result["operationId"].as_str())
        .expect("handoff operation id")
        .to_string();
    let handoff_path = temp
        .path()
        .join("run")
        .join(format!("handoff-{handoff_id}.json"));
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let failed_state = loop {
        if let Ok(raw) = fs::read(&handoff_path) {
            let value: Value = serde_json::from_slice(&raw).expect("handoff state json");
            if value["stage"] == "failed" {
                break value;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "handoff did not fail before timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(failed_state["ownershipReleased"], false, "{failed_state}");
    assert!(
        failed_state["failure"]
            .as_str()
            .is_some_and(|message| message.contains("cannot resolve handoff executable")),
        "{failed_state}"
    );

    let current_state: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("current daemon state"))
            .expect("current daemon state json");
    assert_eq!(
        current_state["pid"], old_pid,
        "predecessor must remain canonical"
    );
    for _ in 0..10 {
        assert!(
            probe_http(mcp_port),
            "pre-cutover handoff failure must not interrupt MCP serving"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_handoff_refuses_cutover_while_mcp_transport_session_is_active() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let registered = register_workspace(temp.path(), &workspace, "handoff-active-mcp-session");
    let workspace_id = registered["workspace"]["id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let mcp_port = registered["mcpPort"].as_u64().expect("MCP port") as u16;
    let (set_auth, _) = run_json(
        temp.path(),
        &["config", "set", &workspace_id, "--set", "auth.type=noauth"],
    );
    assert_success(&set_auth);
    let (apply_auth, _) = run_json(temp.path(), &["config", "apply", &workspace_id]);
    assert_success(&apply_auth);
    let (start, started) = run_json(
        temp.path(),
        &["start", &workspace_id, "--service", "mcp", "--wait", "15"],
    );
    assert_success(&start);
    let old_pid = started["pid"].as_u64().expect("old pid") as u32;
    let _cleanup = DaemonCleanup {
        config_dir: temp.path().to_path_buf(),
        workspace_id: workspace_id.clone(),
    };
    let session_id = initialize_mcp_session(mcp_port);

    let state_path = temp.path().join("run").join(format!("{workspace_id}.json"));
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).expect("daemon state"))
        .expect("daemon state json");
    state["buildIdentity"]["gitSha"] = Value::String("active-session-old-build".into());
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("state json"),
    )
    .expect("tamper build identity for handoff preflight");

    let (upgrade, report) = run_json(temp.path(), &["upgrade", &workspace_id, "--timeout", "10"]);
    assert!(
        !upgrade.status.success(),
        "quiescence block must fail the rollout"
    );
    let result = &report["results"][0];
    assert_eq!(result["status"], "failed", "{report}");
    assert_eq!(result["mode"], "zero_downtime_handoff", "{report}");
    assert_eq!(result["outageMs"], 0, "{report}");
    assert!(
        result["failure"]
            .as_str()
            .is_some_and(|message| message.contains("active_transport_sessions=1")),
        "{report}"
    );
    let current_state: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("current daemon state"))
            .expect("current daemon state json");
    assert_eq!(
        current_state["pid"], old_pid,
        "predecessor remains canonical"
    );

    let tools_list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}});
    let (status, _) = mcp_post(mcp_port, &tools_list, Some(&session_id));
    assert_eq!(
        status, 200,
        "the existing MCP transport session must remain usable after blocked handoff"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_handoff_failure_after_cutover_rolls_back_previous_generation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let registered = register_workspace(temp.path(), &workspace, "handoff-post-cutover-rollback");
    let workspace_id = registered["workspace"]["id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let mcp_port = registered["mcpPort"].as_u64().expect("MCP port") as u16;
    let (start, started) = run_json_with_env(
        temp.path(),
        &["start", &workspace_id, "--service", "mcp", "--wait", "15"],
        "ANCHOR_TEST_HANDOFF_FAIL_AFTER_CUTOVER",
        "1",
    );
    assert_success(&start);
    let old_pid = started["pid"].as_u64().expect("old pid") as u32;
    let _cleanup = DaemonCleanup {
        config_dir: temp.path().to_path_buf(),
        workspace_id: workspace_id.clone(),
    };
    assert!(probe_http(mcp_port), "pre-upgrade MCP probe must succeed");

    let state_path = temp.path().join("run").join(format!("{workspace_id}.json"));
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).expect("daemon state"))
        .expect("daemon state json");
    // Unknown prior build identity still requires a generation replacement, but
    // accepts the trusted executable snapshot as a valid rollback generation.
    state["buildIdentity"] = Value::Null;
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("state json"),
    )
    .expect("remove previous build identity for rollback simulation");

    let (upgrade, report) = run_json(temp.path(), &["upgrade", &workspace_id, "--timeout", "20"]);
    assert!(
        !upgrade.status.success(),
        "rolled-back upgrade remains a failed rollout"
    );
    assert_eq!(report["event"], "runtime_upgrade_complete");
    let result = &report["results"][0];
    assert_eq!(result["status"], "rolled_back", "{report}");
    assert_eq!(result["mode"], "zero_downtime_handoff", "{report}");
    assert_eq!(result["handoffSupported"], true, "{report}");
    assert_eq!(result["rollbackAttempted"], true, "{report}");
    assert_eq!(result["rollbackSucceeded"], true, "{report}");
    assert!(
        result["failure"]
            .as_str()
            .is_some_and(|message| message.contains("debug handoff failpoint")),
        "{report}"
    );
    let rollback_pid = result["pid"].as_u64().expect("rollback pid") as u32;
    assert_ne!(
        rollback_pid, old_pid,
        "rollback must be a new daemon process"
    );
    let current_state: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("rollback daemon state"))
            .expect("rollback daemon state json");
    assert_eq!(current_state["pid"], rollback_pid);
    assert!(
        probe_http(mcp_port),
        "rollback generation must restore MCP serving"
    );
}

#[test]
fn upgrade_json_error_contract_remains_a_single_object() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (output, value) = run_json(temp.path(), &["upgrade", "missing-workspace", "--dry-run"]);

    assert!(!output.status.success());
    assert_eq!(value["ok"], false);
    assert!(value["error"].as_str().is_some());
    assert_eq!(value.as_object().expect("object").len(), 2, "{value}");
    assert_ne!(value, json!(null));
}
