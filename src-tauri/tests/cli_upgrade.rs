use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

fn anchor() -> &'static str {
    env!("CARGO_BIN_EXE_anchor")
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

    let (upgrade, report) = run_json(temp.path(), &["upgrade", &workspace_id, "--timeout", "20"]);
    assert_success(&upgrade);
    assert_eq!(report["event"], "runtime_upgrade_complete");
    let result = &report["results"][0];
    assert_eq!(result["status"], "upgraded", "{report}");
    assert_eq!(result["previousPid"], old_pid);
    let new_pid = result["pid"].as_u64().expect("new pid") as u32;
    assert_ne!(new_pid, old_pid);
    assert_eq!(result["rollbackAvailable"], true);
    assert_eq!(
        result["previousBuild"]["gitSha"],
        "simulated-previous-build"
    );
    assert_eq!(result["currentBuild"]["gitSha"], actual_git);
    assert!(result["outageMs"].as_u64().is_some());

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
