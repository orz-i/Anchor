use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::tunnel::log_dir_for_profile;
use crate::workspace::WorkspaceProfile;

const STATE_SCHEMA_VERSION: u32 = 2;
const DAEMON_LOG_FILE: &str = "daemon.log";
#[cfg(unix)]
const SIGTERM_VALUE: i32 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceSelection {
    Mcp,
    Actions,
    All,
}

fn process_matches_spawned_daemon(pid: u32, workspace_id: &str) -> bool {
    if process_matches_daemon(pid, workspace_id) {
        return true;
    }
    #[cfg(windows)]
    {
        // Startup cleanup is the only path allowed to use the parent image as
        // a fallback: spawn_with_tunnels always launches current_exe, and the
        // child may fail before it has persisted DaemonState.
        let Ok(current) = std::env::current_exe() else {
            return false;
        };
        let Ok(Some(actual)) = platform().process_image_path(pid) else {
            return false;
        };
        normalize_windows_image_path(&current) == normalize_windows_image_path(Path::new(&actual))
    }
    #[cfg(not(windows))]
    false
}

impl ServiceSelection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Actions => "actions",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "mcp" => Ok(Self::Mcp),
            "actions" => Ok(Self::Actions),
            "all" => Ok(Self::All),
            _ => Err(format!("无效服务类型：{value}；可选值为 mcp、actions、all")),
        }
    }

    pub fn includes_mcp(self) -> bool {
        matches!(self, Self::Mcp | Self::All)
    }

    pub fn includes_actions(self) -> bool {
        matches!(self, Self::Actions | Self::All)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonState {
    pub schema_version: u32,
    pub workspace_id: String,
    pub workspace_name: String,
    pub workspace_path: String,
    pub pid: u32,
    pub started_at_unix: u64,
    pub service: ServiceSelection,
    pub tunnel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_services: Option<ServiceSelection>,
    pub log_path: String,
    pub version: String,
    pub executable_path: String,
}

impl DaemonState {
    pub fn managed_tunnels(&self) -> Option<ServiceSelection> {
        self.tunnel_services
            .or_else(|| self.tunnel.then_some(self.service))
    }
}

fn discover_daemon_states(profile: &WorkspaceProfile) -> AppResult<Vec<DaemonState>> {
    #[cfg(target_os = "linux")]
    {
        let mut states = Vec::new();
        for entry in fs::read_dir("/proc")?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(raw) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let args = raw
                .split(|byte| *byte == 0)
                .filter_map(|part| std::str::from_utf8(part).ok())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            let Some((service, tunnel_services)) = parse_daemon_args(&args, &profile.id) else {
                continue;
            };
            states.push(DaemonState {
                schema_version: STATE_SCHEMA_VERSION,
                workspace_id: profile.id.clone(),
                workspace_name: profile.name.clone(),
                workspace_path: profile.path.clone(),
                pid,
                started_at_unix: 0,
                service,
                tunnel: tunnel_services.is_some(),
                tunnel_services,
                log_path: daemon_log_path(&profile.id).display().to_string(),
                version: "unknown".into(),
                executable_path: args.first().copied().unwrap_or_default().to_string(),
            });
        }
        states.sort_by_key(|state| state.pid);
        Ok(states)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = profile;
        Ok(Vec::new())
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_daemon_args(
    args: &[&str],
    workspace_id: &str,
) -> Option<(ServiceSelection, Option<ServiceSelection>)> {
    let daemon_index = args.iter().position(|arg| *arg == "daemon-run")?;
    if args.get(daemon_index + 1).copied()? != workspace_id {
        return None;
    }
    let mut service = ServiceSelection::Mcp;
    let mut tunnel_services = None;
    let mut index = daemon_index + 2;
    while index < args.len() {
        match args[index] {
            "--service" => {
                service = ServiceSelection::parse(args.get(index + 1).copied()?).ok()?;
                index += 2;
            }
            "--tunnel" => {
                tunnel_services = Some(service);
                index += 1;
            }
            "--no-tunnel" => {
                tunnel_services = None;
                index += 1;
            }
            "--tunnel-service" => {
                tunnel_services =
                    Some(ServiceSelection::parse(args.get(index + 1).copied()?).ok()?);
                index += 2;
            }
            _ => index += 1,
        }
    }
    Some((service, tunnel_services))
}

pub async fn terminate_spawned(profile: &WorkspaceProfile, pid: u32) -> AppResult<()> {
    ensure_daemon_supported()?;
    if !platform().is_process_alive(pid) {
        cleanup(profile)?;
        return Ok(());
    }
    if !process_matches_spawned_daemon(pid, &profile.id) {
        return Err(AppError::Message(format!(
            "启动失败后的 PID {pid} 不再匹配当前 workspace daemon，拒绝终止"
        )));
    }
    #[cfg(unix)]
    signal(pid, SIGTERM_VALUE)?;
    #[cfg(windows)]
    platform().terminate_process_tree(pid)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while platform().is_process_alive(pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if platform().is_process_alive(pid) {
        platform().terminate_process_tree(pid)?;
    }
    cleanup(profile)
}

fn set_private_dir_permissions(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInspection {
    pub supported: bool,
    pub running: bool,
    pub stale: bool,
    pub ambiguous: bool,
    pub pid_matches: bool,
    pub state: Option<DaemonState>,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct DaemonPaths {
    dir: PathBuf,
    lock: PathBuf,
    pid: PathBuf,
    state: PathBuf,
    control: PathBuf,
}

pub struct DaemonGuard {
    lock_file: File,
    paths: DaemonPaths,
    pid: u32,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let remove_pid = fs::read_to_string(&self.paths.pid)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            == Some(self.pid);
        if remove_pid {
            let _ = fs::remove_file(&self.paths.pid);
            let _ = fs::remove_file(&self.paths.state);
            let _ = fs::remove_file(&self.paths.control);
        }
        let _ = FileExt::unlock(&self.lock_file);
    }
}

pub fn supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "windows"))
}

pub fn daemon_log_path(profile_id: &str) -> PathBuf {
    log_dir_for_profile(profile_id).join(DAEMON_LOG_FILE)
}

#[cfg(unix)]
pub(crate) fn control_socket_path(profile_id: &str) -> AppResult<PathBuf> {
    Ok(daemon_paths(profile_id)?.control)
}

#[cfg(windows)]
pub(crate) fn control_pipe_name(profile_id: &str) -> AppResult<String> {
    let config_dir = crate::platform::app_config_dir_override()
        .map(Ok)
        .unwrap_or_else(|| platform().app_config_dir())?;
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown-user".into());
    let digest = Sha256::digest(format!("{user}\0{}", config_dir.display()).as_bytes());
    let scope = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        r"\\.\pipe\{}-{scope}-{}",
        crate::brand::SERVER_NAME,
        sanitize_id(profile_id)
    ))
}

pub fn inspect(profile: &WorkspaceProfile) -> AppResult<DaemonInspection> {
    if !supported() {
        return Ok(DaemonInspection {
            supported: false,
            running: false,
            stale: false,
            ambiguous: false,
            pid_matches: false,
            state: None,
            detail: "Workspace daemon 当前仅支持 Windows 和 Linux".into(),
        });
    }
    let paths = daemon_paths(&profile.id)?;
    let mut state_error;
    let state = match read_state(&paths.state) {
        Ok(state) => {
            state_error = None;
            state
        }
        Err(error) => {
            state_error = Some(error.to_string());
            None
        }
    };
    let state = match state {
        Some(state)
            if state.schema_version != STATE_SCHEMA_VERSION || state.workspace_id != profile.id =>
        {
            state_error = Some(format!(
                "状态文件与当前 workspace/schema 不匹配：workspace={} schema={}",
                state.workspace_id, state.schema_version
            ));
            None
        }
        other => other,
    };
    if let Some(state) = state.as_ref() {
        let alive = platform().is_process_alive(state.pid);
        let pid_matches = alive && process_matches_daemon(state.pid, &profile.id);
        if alive && pid_matches {
            return Ok(running_inspection(state.clone(), false));
        }
    }

    let discovered = discover_daemon_states(profile)?;
    if discovered.len() == 1 {
        return Ok(running_inspection(discovered[0].clone(), true));
    }
    if discovered.len() > 1 {
        return Ok(DaemonInspection {
            supported: true,
            running: false,
            stale: true,
            ambiguous: true,
            pid_matches: false,
            state: None,
            detail: format!(
                "发现 {} 个匹配当前 workspace 的 daemon 进程，拒绝自动选择",
                discovered.len()
            ),
        });
    }

    let Some(state) = state else {
        return Ok(DaemonInspection {
            supported: true,
            running: false,
            stale: state_error.is_some(),
            ambiguous: false,
            pid_matches: false,
            state: None,
            detail: state_error
                .map(|error| format!("daemon 状态文件无效：{error}"))
                .unwrap_or_else(|| "daemon 未运行".into()),
        });
    };
    let alive = platform().is_process_alive(state.pid);
    let pid_matches = alive && process_matches_daemon(state.pid, &profile.id);
    Ok(DaemonInspection {
        supported: true,
        running: false,
        stale: true,
        ambiguous: false,
        pid_matches,
        detail: if alive {
            format!("状态文件中的 PID {} 属于其他进程，拒绝接管", state.pid)
        } else {
            format!("daemon 状态已过期，PID {} 不存在", state.pid)
        },
        state: Some(state),
    })
}

fn running_inspection(state: DaemonState, recovered: bool) -> DaemonInspection {
    DaemonInspection {
        supported: true,
        running: true,
        stale: false,
        ambiguous: false,
        pid_matches: true,
        detail: format!(
            "daemon 正在运行，PID {}，service={}，tunnels={}{}",
            state.pid,
            state.service.as_str(),
            state
                .managed_tunnels()
                .map(ServiceSelection::as_str)
                .unwrap_or("none"),
            if recovered {
                "（从 /proc 恢复）"
            } else {
                ""
            }
        ),
        state: Some(state),
    }
}

pub fn acquire(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    tunnel: bool,
) -> AppResult<DaemonGuard> {
    acquire_with_tunnels(profile, service, tunnel.then_some(service))
}

pub fn acquire_with_tunnels(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    tunnel_services: Option<ServiceSelection>,
) -> AppResult<DaemonGuard> {
    ensure_daemon_supported()?;
    let paths = daemon_paths(&profile.id)?;
    ensure_private_dir(&paths.dir)?;
    let lock_file = open_private_file(&paths.lock, false)?;
    lock_file.try_lock_exclusive().map_err(|_| {
        AppError::Message(format!(
            "workspace {} 已有 daemon 正在启动或运行",
            profile.name
        ))
    })?;

    cleanup_stale_files(&paths)?;
    let pid = std::process::id();
    let state = DaemonState {
        schema_version: STATE_SCHEMA_VERSION,
        workspace_id: profile.id.clone(),
        workspace_name: profile.name.clone(),
        workspace_path: profile.path.clone(),
        pid,
        started_at_unix: unix_now(),
        service,
        tunnel: tunnel_services.is_some(),
        tunnel_services,
        log_path: daemon_log_path(&profile.id).display().to_string(),
        version: env!("CARGO_PKG_VERSION").into(),
        executable_path: std::env::current_exe()?.display().to_string(),
    };
    atomic_write_json(&paths.state, &state)?;
    write_private_text(&paths.pid, &format!("{pid}\n"))?;
    Ok(DaemonGuard {
        lock_file,
        paths,
        pid,
    })
}

pub fn update_tunnel_services(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    tunnel_services: Option<ServiceSelection>,
) -> AppResult<()> {
    ensure_daemon_supported()?;
    let paths = daemon_paths(&profile.id)?;
    let mut state = read_state(&paths.state)?.ok_or_else(|| {
        AppError::Message("daemon state disappeared while updating tunnel ownership".into())
    })?;
    let current_pid = std::process::id();
    if state.pid != current_pid || state.workspace_id != profile.id {
        return Err(AppError::Message(format!(
            "refusing to update daemon state owned by PID {} / workspace {}",
            state.pid, state.workspace_id
        )));
    }
    state.workspace_name = profile.name.clone();
    state.workspace_path = profile.path.clone();
    state.service = service;
    state.tunnel = tunnel_services.is_some();
    state.tunnel_services = tunnel_services;
    atomic_write_json(&paths.state, &state)
}

pub fn spawn(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    tunnel: bool,
) -> AppResult<u32> {
    spawn_with_tunnels(profile, service, tunnel.then_some(service))
}

pub fn spawn_with_tunnels(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    tunnel_services: Option<ServiceSelection>,
) -> AppResult<u32> {
    ensure_daemon_supported()?;
    let inspection = inspect(profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.running {
        return Err(AppError::Message(format!(
            "workspace {} 的 daemon 已运行",
            profile.name
        )));
    }
    if inspection.state.is_some() && !inspection.pid_matches {
        cleanup(profile)?;
    }

    let log_path = daemon_log_path(&profile.id);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
        set_private_dir_permissions(parent)?;
    }
    let stdout = open_private_file(&log_path, true)?;
    let stderr = stdout.try_clone()?;
    let executable = std::env::current_exe()?;
    let mut command = Command::new(executable);
    command
        .arg("daemon-run")
        .arg(&profile.id)
        .arg("--service")
        .arg(service.as_str());
    if let Some(tunnels) = tunnel_services {
        command.arg("--tunnel-service").arg(tunnels.as_str());
    } else {
        command.arg("--no-tunnel");
    }
    command
        .current_dir(&profile.path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    #[cfg(windows)]
    crate::platform::hide_std_console(&mut command);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::umask(0o077);
                Ok(())
            });
        }
    }

    let child = command
        .spawn()
        .map_err(|error| AppError::Message(format!("启动 daemon 子进程失败：{error}")))?;
    Ok(child.id())
}

pub async fn wait_ready(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    expected_pid: u32,
    timeout: Duration,
) -> AppResult<DaemonState> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let inspection = inspect(profile)?;
        if let Some(state) = inspection.state.clone() {
            if state.pid != expected_pid {
                return Err(AppError::Message(format!(
                    "daemon 启动竞争：预期 PID {expected_pid}，状态文件属于 PID {}",
                    state.pid
                )));
            }
            if inspection.running && selected_ports_owned_by(profile, service, state.pid)? {
                match crate::control::ipc_ping(&profile.id).await {
                    Ok(()) => return Ok(state),
                    Err(error) if error.is_unavailable() => {}
                    Err(error) => {
                        return Err(AppError::Message(format!(
                            "daemon control endpoint failed readiness check: {error}"
                        )))
                    }
                }
            }
            if !platform().is_process_alive(state.pid) {
                return Err(AppError::Message(format!(
                    "daemon 启动后立即退出，请查看 {}",
                    daemon_log_path(&profile.id).display()
                )));
            }
        } else if !platform().is_process_alive(expected_pid) {
            return Err(AppError::Message(format!(
                "daemon 子进程 PID {expected_pid} 在写入状态前退出，请查看 {}",
                daemon_log_path(&profile.id).display()
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Message(format!(
                "等待 daemon 就绪超时，请查看 {}",
                daemon_log_path(&profile.id).display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn stop(
    profile: &WorkspaceProfile,
    timeout: Duration,
    force: bool,
) -> AppResult<Option<u32>> {
    ensure_daemon_supported()?;
    #[cfg(windows)]
    {
        let _ = (profile, timeout, force);
        Err(AppError::Message(
            "Windows daemon 不允许绕过 Named Pipe 控制面直接停止；请使用 control::request_daemon_exit_and_wait"
                .into(),
        ))
    }
    #[cfg(unix)]
    {
        let inspection = inspect(profile)?;
        if inspection.ambiguous {
            return Err(AppError::Message(inspection.detail));
        }
        let Some(state) = inspection.state else {
            if inspection.stale {
                cleanup(profile)?;
            }
            return Ok(None);
        };
        if !inspection.running {
            cleanup(profile)?;
            return Ok(None);
        }
        if !inspection.pid_matches {
            return Err(AppError::Message(format!(
                "PID {} 不属于当前 workspace daemon，拒绝停止",
                state.pid
            )));
        }
        signal(state.pid, SIGTERM_VALUE)?;
        wait_for_controlled_exit(profile, state.pid, timeout, force).await?;
        Ok(Some(state.pid))
    }
}

pub async fn wait_for_controlled_exit(
    profile: &WorkspaceProfile,
    pid: u32,
    timeout: Duration,
    force: bool,
) -> AppResult<()> {
    ensure_daemon_supported()?;
    let deadline = tokio::time::Instant::now() + timeout;
    while platform().is_process_alive(pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if platform().is_process_alive(pid) {
        if !force {
            return Err(AppError::Message(format!(
                "daemon 在 {} 秒内未停止；可使用 --force",
                timeout.as_secs()
            )));
        }
        if !process_matches_daemon(pid, &profile.id) {
            return Err(AppError::Message(format!(
                "PID {pid} 不再属于当前 workspace daemon，拒绝强制终止"
            )));
        }
        platform().terminate_process_tree(pid)?;
        let force_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while platform().is_process_alive(pid) && tokio::time::Instant::now() < force_deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if platform().is_process_alive(pid) {
            return Err(AppError::Message(format!(
                "强制停止 daemon 失败，PID {pid} 仍然存活"
            )));
        }
    }
    cleanup_after_pid_exit(profile, pid)
}

fn cleanup_after_pid_exit(profile: &WorkspaceProfile, exited_pid: u32) -> AppResult<()> {
    let paths = daemon_paths(&profile.id)?;
    if read_state(&paths.state)?
        .as_ref()
        .is_some_and(|state| state.pid != exited_pid)
    {
        return Ok(());
    }
    cleanup_stale_files(&paths)
}

pub fn cleanup(profile: &WorkspaceProfile) -> AppResult<()> {
    if !supported() {
        return Ok(());
    }
    cleanup_stale_files(&daemon_paths(&profile.id)?)
}

fn selected_ports_owned_by(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    pid: u32,
) -> AppResult<bool> {
    let mut ports = Vec::new();
    if service.includes_mcp() {
        ports.push(profile.runtime.local_port);
    }
    if service.includes_actions() {
        ports.push(profile.actions.local_port);
    }
    for port in ports {
        if platform().find_pid_listening_on_port(port)? != Some(pid) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn daemon_paths(profile_id: &str) -> AppResult<DaemonPaths> {
    Ok(daemon_paths_in(runtime_dir()?, profile_id))
}

fn daemon_paths_in(dir: PathBuf, profile_id: &str) -> DaemonPaths {
    let safe = sanitize_id(profile_id);
    DaemonPaths {
        lock: dir.join(format!("{safe}.lock")),
        pid: dir.join(format!("{safe}.pid")),
        state: dir.join(format!("{safe}.json")),
        control: dir.join(format!("{safe}.sock")),
        dir,
    }
}

pub(crate) fn runtime_dir() -> AppResult<PathBuf> {
    let config_dir = crate::platform::app_config_dir_override();
    #[cfg(unix)]
    {
        let xdg_runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        return Ok(select_runtime_dir(config_dir, xdg_runtime, unsafe {
            libc::geteuid()
        }));
    }
    #[cfg(windows)]
    {
        if let Some(path) = config_dir {
            return Ok(path.join("run"));
        }
        return Ok(platform().app_config_dir()?.join("run"));
    }
    #[allow(unreachable_code)]
    Err(AppError::Message("无法确定 daemon 运行目录".into()))
}

#[cfg(any(unix, test))]
fn select_runtime_dir(
    config_dir: Option<PathBuf>,
    xdg_runtime: Option<PathBuf>,
    uid: u32,
) -> PathBuf {
    if let Some(config_dir) = config_dir {
        return config_dir.join("run");
    }
    if let Some(runtime) = xdg_runtime {
        return runtime.join(crate::brand::SERVER_NAME);
    }
    PathBuf::from(format!("/tmp/{}-{uid}", crate::brand::SERVER_NAME))
}

fn read_state(path: &Path) -> AppResult<Option<DaemonState>> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| AppError::Message(format!("daemon 状态文件损坏：{error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn cleanup_stale_files(paths: &DaemonPaths) -> AppResult<()> {
    for path in [&paths.pid, &paths.state, &paths.control] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn process_matches_daemon(pid: u32, workspace_id: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else {
            return false;
        };
        let args = raw
            .split(|byte| *byte == 0)
            .filter_map(|part| std::str::from_utf8(part).ok())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        parse_daemon_args(&args, workspace_id).is_some()
    }
    #[cfg(target_os = "windows")]
    {
        let Ok(paths) = daemon_paths(workspace_id) else {
            return false;
        };
        let Ok(Some(state)) = read_state(&paths.state) else {
            return false;
        };
        if state.pid != pid
            || state.workspace_id != workspace_id
            || state.schema_version != STATE_SCHEMA_VERSION
            || state.executable_path.trim().is_empty()
        {
            return false;
        }
        let Ok(Some(actual)) = platform().process_image_path(pid) else {
            return false;
        };
        normalize_windows_image_path(Path::new(&state.executable_path))
            == normalize_windows_image_path(Path::new(&actual))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (pid, workspace_id);
        false
    }
}

#[cfg(unix)]
fn signal(pid: u32, value: i32) -> AppResult<()> {
    let result = unsafe { libc::kill(pid as i32, value) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(AppError::Message(format!(
        "发送信号 {value} 到 PID {pid} 失败：{error}"
    )))
}

fn ensure_daemon_supported() -> AppResult<()> {
    if supported() {
        Ok(())
    } else {
        Err(AppError::Message(
            "daemon 当前平台尚未支持；请使用 serve 前台模式".into(),
        ))
    }
}

#[cfg(windows)]
fn normalize_windows_image_path(path: &Path) -> String {
    let normalized = fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('/', "\\")
        .trim_matches('"')
        .to_ascii_lowercase();
    normalized
        .strip_prefix("\\\\?\\")
        .unwrap_or(&normalized)
        .to_string()
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn ensure_private_dir(path: &Path) -> AppResult<()> {
    fs::create_dir_all(path)?;
    set_private_dir_permissions(path)?;
    Ok(())
}

fn open_private_file(path: &Path, append: bool) -> AppResult<File> {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

fn set_private_file_permissions(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn write_private_text(path: &Path, value: &str) -> AppResult<()> {
    let mut file = open_private_file(path, false)?;
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Message("daemon 状态路径缺少父目录".into()))?;
    ensure_private_dir(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("daemon"),
        std::process::id()
    ));
    let payload = format!("{}\n", serde_json::to_string_pretty(value)?);
    write_private_text(&temp, &payload)?;
    replace_file(&temp, path)?;
    Ok(())
}

fn replace_file(source: &Path, destination: &Path) -> AppResult<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let source = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let destination = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
            .map_err(|error| {
                AppError::Message(format!("Windows daemon state replacement failed: {error}"))
            })?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_runtime_file_names() {
        assert_eq!(sanitize_id("../unsafe workspace"), "___unsafe_workspace");
    }

    #[test]
    fn config_override_isolates_runtime_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = select_runtime_dir(Some(temp.path().to_path_buf()), None, 1000);

        assert_eq!(runtime, temp.path().join("run"));
    }

    #[test]
    fn daemon_paths_include_a_sanitized_control_socket() {
        let paths = daemon_paths_in(PathBuf::from("/tmp/anchor"), "../unsafe workspace");

        assert_eq!(
            paths.control,
            PathBuf::from("/tmp/anchor/___unsafe_workspace.sock")
        );
        assert_eq!(
            paths.pid,
            PathBuf::from("/tmp/anchor/___unsafe_workspace.pid")
        );
    }

    #[test]
    fn xdg_runtime_is_preferred_over_tmp_fallback() {
        assert_eq!(
            select_runtime_dir(None, Some(PathBuf::from("/run/user/1000")), 1000),
            PathBuf::from("/run/user/1000/anchor")
        );
        assert_eq!(
            select_runtime_dir(None, None, 1000),
            PathBuf::from("/tmp/anchor-1000")
        );
    }

    #[test]
    fn daemon_state_keeps_operational_parameters() {
        let state = DaemonState {
            schema_version: STATE_SCHEMA_VERSION,
            workspace_id: "workspace".into(),
            workspace_name: "Workspace".into(),
            workspace_path: "/srv/workspace".into(),
            pid: 42,
            started_at_unix: 100,
            service: ServiceSelection::All,
            tunnel: true,
            tunnel_services: Some(ServiceSelection::Mcp),
            log_path: "/tmp/daemon.log".into(),
            version: "1".into(),
            executable_path: "/usr/local/bin/anchor".into(),
        };

        let value = serde_json::to_value(state).expect("serialize state");

        assert_eq!(value["service"], "all");
        assert_eq!(value["tunnel"], true);
        assert_eq!(value["tunnelServices"], "mcp");
        assert_eq!(value["pid"], 42);
        assert_eq!(value["executablePath"], "/usr/local/bin/anchor");
    }

    #[cfg(windows)]
    #[test]
    fn windows_atomic_state_replace_replaces_existing_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("state.tmp");
        let destination = temp.path().join("state.json");
        fs::write(&source, "new").expect("source");
        fs::write(&destination, "old").expect("destination");

        replace_file(&source, &destination).expect("replace");

        assert_eq!(
            fs::read_to_string(&destination).expect("destination"),
            "new"
        );
        assert!(!source.exists());
    }

    #[test]
    fn parses_internal_daemon_command_line() {
        assert_eq!(
            parse_daemon_args(
                &[
                    "/usr/local/bin/anchor",
                    "daemon-run",
                    "workspace",
                    "--service",
                    "all",
                    "--tunnel",
                ],
                "workspace",
            ),
            Some((ServiceSelection::All, Some(ServiceSelection::All)))
        );
        assert_eq!(
            parse_daemon_args(
                &[
                    "anchor",
                    "daemon-run",
                    "workspace",
                    "--service",
                    "all",
                    "--tunnel-service",
                    "mcp",
                ],
                "workspace",
            ),
            Some((ServiceSelection::All, Some(ServiceSelection::Mcp)))
        );
        assert_eq!(
            parse_daemon_args(&["anchor", "daemon-run", "other"], "workspace"),
            None
        );
    }
}
