#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::build_identity::BuildIdentity;
use crate::control::{self, DaemonLaunchSpec};
use crate::daemon::{self, ServiceSelection};
use crate::error::{AppError, AppResult};
use crate::gateway_control::{self, GatewayOperation};
use crate::gateway_daemon;
use crate::platform::platform;

const PLAN_SCHEMA_VERSION: u32 = 1;
const SERVICE_PLAN_FILE: &str = "linux-service.json";
const SERVICE_PLAN_LOCK_FILE: &str = ".linux-service.lock";
const SERVICE_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
const SERVICE_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxWorkspaceAutostart {
    pub workspace_id: String,
    pub service: ServiceSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_services: Option<ServiceSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxServicePlan {
    pub schema_version: u32,
    #[serde(default)]
    pub executable_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_build: Option<BuildIdentity>,
    #[serde(default)]
    pub workspaces: Vec<LinuxWorkspaceAutostart>,
    #[serde(default)]
    pub gateway_workspace_ids: Vec<String>,
}

impl Default for LinuxServicePlan {
    fn default() -> Self {
        Self {
            schema_version: PLAN_SCHEMA_VERSION,
            executable_path: String::new(),
            installed_build: None,
            workspaces: Vec::new(),
            gateway_workspace_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxServiceStatus {
    pub supported: bool,
    pub manager: String,
    pub unit_name: String,
    pub installed: bool,
    pub enabled: bool,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    pub config_dir: String,
    pub unit_path: String,
    pub plan_path: String,
    pub plan: LinuxServicePlan,
    pub build_state: String,
    pub current_build: BuildIdentity,
}

#[derive(Debug)]
struct PlanGuard {
    file: File,
}

impl Drop for PlanGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn plan_path() -> AppResult<PathBuf> {
    Ok(platform().app_config_dir()?.join(SERVICE_PLAN_FILE))
}

fn plan_lock_path() -> AppResult<PathBuf> {
    Ok(platform().app_config_dir()?.join(SERVICE_PLAN_LOCK_FILE))
}

fn acquire_plan_lock() -> AppResult<PlanGuard> {
    let path = plan_lock_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    FileExt::lock_exclusive(&file)?;
    Ok(PlanGuard { file })
}

pub fn load_plan() -> AppResult<LinuxServicePlan> {
    let path = plan_path()?;
    let _guard = acquire_plan_lock()?;
    read_plan_unlocked(&path)
}

fn read_plan_unlocked(path: &Path) -> AppResult<LinuxServicePlan> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            let mut plan: LinuxServicePlan = serde_json::from_str(&raw)
                .map_err(|error| AppError::Message(format!("Linux service plan 损坏：{error}")))?;
            if plan.schema_version != PLAN_SCHEMA_VERSION {
                return Err(AppError::Message(format!(
                    "Linux service plan schema={}，当前仅支持 {}",
                    plan.schema_version, PLAN_SCHEMA_VERSION
                )));
            }
            normalize_plan(&mut plan);
            Ok(plan)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LinuxServicePlan::default())
        }
        Err(error) => Err(error.into()),
    }
}

fn mutate_plan(
    update: impl FnOnce(&mut LinuxServicePlan) -> AppResult<()>,
) -> AppResult<LinuxServicePlan> {
    let path = plan_path()?;
    let _guard = acquire_plan_lock()?;
    let mut plan = read_plan_unlocked(&path)?;
    update(&mut plan)?;
    normalize_plan(&mut plan);
    write_plan_unlocked(&path, &plan)?;
    Ok(plan)
}

fn write_plan_unlocked(path: &Path, plan: &LinuxServicePlan) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)?;
    file.write_all(serde_json::to_string_pretty(plan)?.as_bytes())?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    Ok(())
}

fn normalize_plan(plan: &mut LinuxServicePlan) {
    plan.schema_version = PLAN_SCHEMA_VERSION;
    let mut workspaces = BTreeMap::<String, LinuxWorkspaceAutostart>::new();
    for mut entry in plan.workspaces.drain(..) {
        entry.workspace_id = entry.workspace_id.trim().to_string();
        if !entry.workspace_id.is_empty() {
            workspaces.insert(entry.workspace_id.clone(), entry);
        }
    }
    plan.workspaces = workspaces.into_values().collect();
    plan.gateway_workspace_ids = normalized_ids(&plan.gateway_workspace_ids);
}

fn normalized_ids(ids: &[String]) -> Vec<String> {
    let mut ids = ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

pub fn set_workspace_desired(
    workspace_id: &str,
    desired: Option<DaemonLaunchSpec>,
) -> AppResult<LinuxServicePlan> {
    let workspace_id = workspace_id.trim().to_string();
    if workspace_id.is_empty() {
        return Err(AppError::Message("workspace id 不能为空".into()));
    }
    mutate_plan(|plan| {
        plan.workspaces
            .retain(|entry| entry.workspace_id != workspace_id);
        if let Some(spec) = desired {
            plan.workspaces.push(LinuxWorkspaceAutostart {
                workspace_id: workspace_id.clone(),
                service: spec.service,
                tunnel_services: spec.tunnels,
            });
        }
        Ok(())
    })
}

pub fn forget_workspace(workspace_id: &str) -> AppResult<LinuxServicePlan> {
    let workspace_id = workspace_id.trim().to_string();
    mutate_plan(|plan| {
        plan.workspaces
            .retain(|entry| entry.workspace_id != workspace_id);
        plan.gateway_workspace_ids.retain(|id| id != &workspace_id);
        Ok(())
    })
}

pub fn set_gateway_desired(workspace_ids: &[String]) -> AppResult<LinuxServicePlan> {
    let ids = normalized_ids(workspace_ids);
    mutate_plan(|plan| {
        plan.gateway_workspace_ids = ids;
        Ok(())
    })
}

fn running_plan_snapshot() -> AppResult<(Vec<LinuxWorkspaceAutostart>, Vec<String>)> {
    let store = crate::data::DataStore::load()?;
    let profiles = store.list().to_vec();
    drop(store);
    let mut workspaces = Vec::new();
    for profile in &profiles {
        let inspection = daemon::inspect(profile)?;
        if let Some(state) = inspection
            .state
            .filter(|_| inspection.running && inspection.pid_matches)
        {
            workspaces.push(LinuxWorkspaceAutostart {
                workspace_id: profile.id.clone(),
                service: state.service,
                tunnel_services: state.managed_tunnels(),
            });
        }
    }
    let gateway_inspection = gateway_daemon::inspect()?;
    let gateway_workspace_ids = gateway_inspection
        .state
        .filter(|_| gateway_inspection.running && gateway_inspection.pid_matches)
        .map(|state| state.workspace_ids)
        .unwrap_or_default();
    Ok((workspaces, gateway_workspace_ids))
}

pub fn sync_plan_from_running() -> AppResult<LinuxServicePlan> {
    let (workspaces, gateway_workspace_ids) = running_plan_snapshot()?;
    mutate_plan(|plan| {
        plan.workspaces = workspaces;
        plan.gateway_workspace_ids = gateway_workspace_ids;
        Ok(())
    })
}

fn unit_name() -> AppResult<String> {
    Ok(format!(
        "anchor-control-plane-{}.service",
        gateway_daemon::config_scope()?
    ))
}

fn unit_path() -> AppResult<PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| AppError::Message("无法确定 systemd user 配置目录".into()))?;
    Ok(config.join("systemd").join("user").join(unit_name()?))
}

fn ensure_linux_linger() -> AppResult<()> {
    let uid = unsafe { libc::geteuid() }.to_string();
    let state = Command::new("loginctl")
        .args(["show-user", &uid, "-p", "Linger", "--value"])
        .output()
        .map_err(|error| AppError::Message(format!("无法查询 systemd user linger：{error}")))?;
    if state.status.success() && String::from_utf8_lossy(&state.stdout).trim() == "yes" {
        return Ok(());
    }
    let enable = Command::new("loginctl")
        .args(["enable-linger", &uid])
        .output()
        .map_err(|error| AppError::Message(format!("无法启用 systemd user linger：{error}")))?;
    if !enable.status.success() {
        return Err(AppError::Message(format!(
            "无法启用当前用户 linger，不能保证 Workspace service 在节点重启后自动启动：{}",
            String::from_utf8_lossy(&enable.stderr).trim()
        )));
    }
    Ok(())
}

fn run_systemctl(args: &[&str], allow_nonzero: bool) -> AppResult<std::process::Output> {
    crate::platform::run_user_systemctl(args, allow_nonzero)
}

fn systemd_quote(value: &str) -> AppResult<String> {
    if value.chars().any(|ch| matches!(ch, '\0' | '\n' | '\r')) {
        return Err(AppError::Message(
            "systemd ExecStart 参数包含非法控制字符".into(),
        ));
    }
    Ok(format!(
        "\"{}\"",
        value
            .replace('%', "%%")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    ))
}

fn render_systemd_unit(executable: &Path, config_dir: &Path) -> AppResult<String> {
    Ok(format!(
        "[Unit]\nDescription=Anchor Workspace Control Plane ({scope})\nAfter=network-online.target\nWants=network-online.target\nStartLimitIntervalSec=60\nStartLimitBurst=5\n\n[Service]\nType=simple\nExecStart={exe} --config-dir {config} service-run {config}\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec=30\nNoNewPrivileges=true\n\n[Install]\nWantedBy=default.target\n",
        scope = gateway_daemon::config_scope()?,
        exe = systemd_quote(&executable.display().to_string())?,
        config = systemd_quote(&config_dir.display().to_string())?,
    ))
}

pub fn service_status() -> AppResult<LinuxServiceStatus> {
    let config_dir = platform().app_config_dir()?;
    let unit = unit_name()?;
    let path = unit_path()?;
    let installed = path.is_file();
    let enabled = installed
        && run_systemctl(&["is-enabled", &unit], true)?
            .status
            .success();
    let running = installed && run_systemctl(&["is-active", &unit], true)?.status.success();
    let process_id = if running {
        let output = run_systemctl(&["show", &unit, "-p", "MainPID", "--value"], true)?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid != 0)
    } else {
        None
    };
    let plan = load_plan()?;
    let current_build = BuildIdentity::current();
    let build_state = if !installed {
        "not_installed"
    } else if plan
        .installed_build
        .as_ref()
        .is_some_and(|build| build.same_build(&current_build))
    {
        "current"
    } else if plan.installed_build.is_some() {
        "different"
    } else {
        "unknown"
    };
    Ok(LinuxServiceStatus {
        supported: true,
        manager: "systemd-user".into(),
        unit_name: unit,
        installed,
        enabled,
        running,
        process_id,
        config_dir: config_dir.display().to_string(),
        unit_path: path.display().to_string(),
        plan_path: plan_path()?.display().to_string(),
        plan,
        build_state: build_state.into(),
        current_build,
    })
}

pub fn install_service() -> AppResult<LinuxServiceStatus> {
    let existing = load_plan()?;
    if existing.workspaces.is_empty() && existing.gateway_workspace_ids.is_empty() {
        let _ = sync_plan_from_running()?;
    }
    let executable = std::env::current_exe()?;
    let config_dir = platform().app_config_dir()?;
    let current_build = BuildIdentity::current();
    mutate_plan(|plan| {
        plan.executable_path = executable.display().to_string();
        plan.installed_build = Some(current_build.clone());
        Ok(())
    })?;
    ensure_linux_linger()?;
    // Validate the user-manager transport before writing/replacing the unit so
    // a missing SSH session environment cannot leave a misleading partial
    // installation behind. run_systemctl supplies the canonical runtime dir.
    run_systemctl(&["show-environment"], false)?;
    let path = unit_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, render_systemd_unit(&executable, &config_dir)?)?;
    let unit = unit_name()?;
    run_systemctl(&["daemon-reload"], false)?;
    run_systemctl(&["enable", &unit], false)?;
    run_systemctl(&["restart", &unit], false)?;
    service_status()
}

pub fn uninstall_service() -> AppResult<LinuxServiceStatus> {
    let unit = unit_name()?;
    let _ = run_systemctl(&["stop", &unit], true);
    let _ = run_systemctl(&["disable", &unit], true);
    match fs::remove_file(unit_path()?) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    run_systemctl(&["daemon-reload"], false)?;
    let _ = run_systemctl(&["reset-failed", &unit], true);
    service_status()
}

pub fn start_service() -> AppResult<LinuxServiceStatus> {
    run_systemctl(&["start", &unit_name()?], false)?;
    service_status()
}

pub fn stop_service() -> AppResult<LinuxServiceStatus> {
    run_systemctl(&["stop", &unit_name()?], false)?;
    service_status()
}

pub fn restart_service() -> AppResult<LinuxServiceStatus> {
    run_systemctl(&["restart", &unit_name()?], false)?;
    service_status()
}

pub async fn run_service(config_dir: PathBuf) -> AppResult<()> {
    let expected_config_dir = platform().app_config_dir()?;
    let expected = expected_config_dir
        .canonicalize()
        .unwrap_or(expected_config_dir.clone());
    let requested = config_dir.canonicalize().unwrap_or(config_dir.clone());
    if expected != requested {
        return Err(AppError::Message(format!(
            "service-run config dir mismatch: expected={}, actual={}",
            expected.display(),
            requested.display()
        )));
    }
    append_service_log(&format!(
        "[service] started pid={} build={}",
        std::process::id(),
        BuildIdentity::current().git_sha
    ));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| AppError::Message(format!("无法监听 SIGTERM：{error}")))?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|error| AppError::Message(format!("无法监听 SIGINT：{error}")))?;
    let mut managed_workspaces = HashSet::<String>::new();
    let mut gateway_managed = false;
    loop {
        match load_plan() {
            Ok(plan) => {
                if let Err(error) =
                    reconcile_service_plan(&plan, &mut managed_workspaces, &mut gateway_managed)
                        .await
                {
                    append_service_log(&format!("[service] reconcile failed: {error}"));
                }
            }
            Err(error) => append_service_log(&format!("[service] plan read failed: {error}")),
        }
        tokio::select! {
            _ = tokio::time::sleep(SERVICE_RECONCILE_INTERVAL) => {},
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
    }
    shutdown_managed_control_plane(&mut managed_workspaces, &mut gateway_managed).await;
    append_service_log("[service] stopped");
    Ok(())
}

async fn reconcile_service_plan(
    plan: &LinuxServicePlan,
    managed_workspaces: &mut HashSet<String>,
    gateway_managed: &mut bool,
) -> AppResult<()> {
    let store = crate::data::DataStore::load()?;
    let profiles = store.list().to_vec();
    let gateway_config = store.settings().mcp_gateway;
    drop(store);
    let desired = plan
        .workspaces
        .iter()
        .map(|entry| (entry.workspace_id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let profile_ids = profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<HashSet<_>>();
    managed_workspaces.retain(|workspace_id| profile_ids.contains(workspace_id.as_str()));
    let mut errors = Vec::new();

    for profile in &profiles {
        if let Some(entry) = desired.get(profile.id.as_str()) {
            match control::reconcile_daemon(
                profile,
                Some(DaemonLaunchSpec {
                    service: entry.service,
                    tunnels: entry.tunnel_services,
                }),
                SERVICE_OPERATION_TIMEOUT,
                true,
            )
            .await
            {
                Ok(Some(_)) => {
                    managed_workspaces.insert(profile.id.clone());
                }
                Ok(None) => {
                    managed_workspaces.remove(&profile.id);
                }
                Err(error) => errors.push(format!("Workspace {}: {error}", profile.name)),
            }
        } else if managed_workspaces.contains(&profile.id) {
            match control::request_daemon_exit_and_wait(
                profile,
                control::ControlOperation::Shutdown,
                SERVICE_OPERATION_TIMEOUT,
                true,
            )
            .await
            {
                Ok(_) => {
                    managed_workspaces.remove(&profile.id);
                }
                Err(error) => errors.push(format!("Workspace {} stop: {error}", profile.name)),
            }
        }
    }

    for missing in desired.keys().filter(|id| !profile_ids.contains(**id)) {
        errors.push(format!(
            "Linux service plan 引用了不存在的 Workspace：{missing}"
        ));
    }

    let desired_gateway = normalized_ids(&plan.gateway_workspace_ids);
    if desired_gateway.is_empty() {
        if *gateway_managed {
            match stop_gateway_if_running().await {
                Ok(()) => *gateway_managed = false,
                Err(error) => errors.push(format!("Gateway stop: {error}")),
            }
        }
    } else if !gateway_config.enabled {
        errors.push("Linux service plan 请求 Gateway，但全局 MCP Gateway 配置未启用".into());
    } else {
        let missing = desired_gateway
            .iter()
            .filter(|workspace_id| !profile_ids.contains(workspace_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            errors.push(format!(
                "Linux service plan 引用了不存在的 Gateway workspace：{}",
                missing.join(", ")
            ));
        } else {
            match ensure_gateway_running(&desired_gateway).await {
                Ok(()) => *gateway_managed = true,
                Err(error) => errors.push(format!("Gateway: {error}")),
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::Message(errors.join("；")))
    }
}

async fn ensure_gateway_running(workspace_ids: &[String]) -> AppResult<()> {
    let desired = normalized_ids(workspace_ids);
    let inspection = gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if let Some(state) = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
    {
        if state.workspace_ids == desired {
            gateway_control::ping()
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            return Ok(());
        }
        let accepted = gateway_control::request_exit(GatewayOperation::Restart)
            .await
            .map_err(|error| AppError::Message(error.to_string()))?;
        if accepted != state.pid {
            return Err(AppError::Message(format!(
                "Gateway restart PID mismatch: state={}, response={accepted}",
                state.pid
            )));
        }
        gateway_daemon::wait_for_exit(state.pid, SERVICE_OPERATION_TIMEOUT, true).await?;
    }
    let pid = gateway_daemon::spawn(&desired)?;
    gateway_daemon::wait_ready(pid, SERVICE_OPERATION_TIMEOUT).await?;
    Ok(())
}

async fn stop_gateway_if_running() -> AppResult<()> {
    let inspection = gateway_daemon::inspect()?;
    let Some(state) = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
    else {
        return Ok(());
    };
    let accepted = match gateway_control::request_exit(GatewayOperation::Shutdown).await {
        Ok(pid) => pid,
        Err(control_error) => {
            append_service_log(&format!(
                "[service] Gateway PID {} control shutdown failed: {}; falling back to verified termination",
                state.pid, control_error
            ));
            return gateway_daemon::wait_for_exit(state.pid, Duration::ZERO, true).await;
        }
    };
    if accepted != state.pid {
        return Err(AppError::Message(format!(
            "Gateway shutdown PID mismatch: state={}, response={accepted}",
            state.pid
        )));
    }
    gateway_daemon::wait_for_exit(state.pid, SERVICE_OPERATION_TIMEOUT, true).await
}

async fn shutdown_managed_control_plane(
    managed_workspaces: &mut HashSet<String>,
    gateway_managed: &mut bool,
) {
    if *gateway_managed {
        if let Err(error) = stop_gateway_if_running().await {
            append_service_log(&format!("[service] Gateway shutdown failed: {error}"));
        }
        *gateway_managed = false;
    }
    let Ok(store) = crate::data::DataStore::load() else {
        return;
    };
    let profiles = store.list().to_vec();
    drop(store);
    for profile in profiles.iter().rev() {
        if managed_workspaces.contains(&profile.id) {
            if let Err(error) = control::request_daemon_exit_and_wait(
                profile,
                control::ControlOperation::Shutdown,
                SERVICE_OPERATION_TIMEOUT,
                true,
            )
            .await
            {
                append_service_log(&format!(
                    "[service] Workspace {} shutdown failed: {error}",
                    profile.name
                ));
            }
        }
    }
    managed_workspaces.clear();
}

fn append_service_log(line: &str) {
    let Ok(config_dir) = platform().app_config_dir() else {
        return;
    };
    let path = config_dir.join("logs").join("linux-service.log");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", crate::logging::timestamped_line(line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_enables_reboot_start_and_exact_service_run_scope() {
        let unit = render_systemd_unit(
            Path::new("/usr/local/bin/anchor"),
            Path::new("/home/demo/.config/anchor"),
        )
        .expect("unit");
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("After=network-online.target"));
        assert!(unit.contains(
            "ExecStart=\"/usr/local/bin/anchor\" --config-dir \"/home/demo/.config/anchor\" service-run \"/home/demo/.config/anchor\""
        ));
    }

    #[test]
    fn plan_normalization_deduplicates_workspace_and_gateway_routes() {
        let mut plan = LinuxServicePlan {
            schema_version: 99,
            executable_path: String::new(),
            installed_build: None,
            workspaces: vec![
                LinuxWorkspaceAutostart {
                    workspace_id: " b ".into(),
                    service: ServiceSelection::Mcp,
                    tunnel_services: None,
                },
                LinuxWorkspaceAutostart {
                    workspace_id: "b".into(),
                    service: ServiceSelection::Actions,
                    tunnel_services: None,
                },
            ],
            gateway_workspace_ids: vec!["z".into(), " z ".into(), "a".into()],
        };
        normalize_plan(&mut plan);
        assert_eq!(plan.schema_version, PLAN_SCHEMA_VERSION);
        assert_eq!(plan.workspaces.len(), 1);
        assert_eq!(plan.workspaces[0].workspace_id, "b");
        assert_eq!(plan.workspaces[0].service, ServiceSelection::Actions);
        assert_eq!(plan.gateway_workspace_ids, vec!["a", "z"]);
    }
}
