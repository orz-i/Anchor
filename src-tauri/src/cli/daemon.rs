use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::tunnel::log_dir_for_profile;
use crate::workspace::WorkspaceProfile;

use super::args::ServiceSelection;

const STATE_SCHEMA_VERSION: u32 = 1;
const DAEMON_LOG_FILE: &str = "daemon.log";
const SIGTERM_VALUE: i32 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub log_path: String,
    pub version: String,
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
            let Some((service, tunnel)) = parse_daemon_args(&args, &profile.id) else {
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
                tunnel,
                log_path: daemon_log_path(&profile.id).display().to_string(),
                version: "unknown".into(),
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

fn parse_daemon_args(args: &[&str], workspace_id: &str) -> Option<(ServiceSelection, bool)> {
    let daemon_index = args.iter().position(|arg| *arg == "daemon-run")?;
    if args.get(daemon_index + 1).copied()? != workspace_id {
        return None;
    }
    let mut service = ServiceSelection::Mcp;
    let mut tunnel = false;
    let mut index = daemon_index + 2;
    while index < args.len() {
        match args[index] {
            "--service" => {
                service = ServiceSelection::parse(args.get(index + 1).copied()?).ok()?;
                index += 2;
            }
            "--tunnel" => {
                tunnel = true;
                index += 1;
            }
            "--no-tunnel" => {
                tunnel = false;
                index += 1;
            }
            _ => index += 1,
        }
    }
    Some((service, tunnel))
}

pub async fn terminate_spawned(
    profile: &WorkspaceProfile,
    pid: u32,
) -> AppResult<()> {
    ensure_linux()?;
    if !platform().is_process_alive(pid) {
        cleanup(profile)?;
        return Ok(());
    }
    if !process_matches_daemon(pid, &profile.id) {
        return Err(AppError::Message(format!(
            "启动失败后的 PID {pid} 不再匹配当前 workspace daemon，拒绝终止"
        )));
    }
    signal(pid, SIGTERM_VALUE)?;
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

#[derive(Debug, Clone, Serialize)]
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
        }
        let _ = FileExt::unlock(&self.lock_file);
    }
}

pub fn supported() -> bool {
    cfg!(target_os = "linux")
}

pub fn daemon_log_path(profile_id: &str) -> PathBuf {
    log_dir_for_profile(profile_id).join(DAEMON_LOG_FILE)
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
            detail: "daemon 目前仅支持 Linux".into(),
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
            if state.schema_version != STATE_SCHEMA_VERSION
                || state.workspace_id != profile.id =>
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
            format!(
                "状态文件中的 PID {} 属于其他进程，拒绝接管",
                state.pid
            )
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
            "daemon 正在运行，PID {}，service={}，tunnel={}{}",
            state.pid,
            state.service.as_str(),
            state.tunnel,
            if recovered { "（从 /proc 恢复）" } else { "" }
        ),
        state: Some(state),
    }
}

pub fn acquire(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    tunnel: bool,
) -> AppResult<DaemonGuard> {
    ensure_linux()?;
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
        tunnel,
        log_path: daemon_log_path(&profile.id).display().to_string(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    atomic_write_json(&paths.state, &state)?;
    write_private_text(&paths.pid, &format!("{pid}\n"))?;
    Ok(DaemonGuard {
        lock_file,
        paths,
        pid,
    })
}

pub fn spawn(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    tunnel: bool,
) -> AppResult<u32> {
    ensure_linux()?;
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
        .arg(service.as_str())
        .arg(if tunnel { "--tunnel" } else { "--no-tunnel" })
        .current_dir(&profile.path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

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

    let child = command.spawn().map_err(|error| {
        AppError::Message(format!("启动 daemon 子进程失败：{error}"))
    })?;
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
                return Ok(state);
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
    ensure_linux()?;
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
    let deadline = tokio::time::Instant::now() + timeout;
    while platform().is_process_alive(state.pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if platform().is_process_alive(state.pid) {
        if !force {
            return Err(AppError::Message(format!(
                "daemon 在 {} 秒内未停止；可使用 --force",
                timeout.as_secs()
            )));
        }
        platform().terminate_process_tree(state.pid)?;
        let force_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while platform().is_process_alive(state.pid)
            && tokio::time::Instant::now() < force_deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if platform().is_process_alive(state.pid) {
            return Err(AppError::Message(format!(
                "强制停止 daemon 失败，PID {} 仍然存活",
                state.pid
            )));
        }
    }
    cleanup(profile)?;
    Ok(Some(state.pid))
}

pub fn cleanup(profile: &WorkspaceProfile) -> AppResult<()> {
    if !supported() {
        return Ok(());
    }
    let paths = daemon_paths(&profile.id)?;
    cleanup_stale_files(&paths)
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
    let dir = runtime_dir()?;
    let safe = sanitize_id(profile_id);
    Ok(DaemonPaths {
        lock: dir.join(format!("{safe}.lock")),
        pid: dir.join(format!("{safe}.pid")),
        state: dir.join(format!("{safe}.json")),
        dir,
    })
}

fn runtime_dir() -> AppResult<PathBuf> {
    let config_dir = std::env::var_os("CODING_TOOLS_MCP_CONFIG_DIR").map(PathBuf::from);
    #[cfg(unix)]
    {
        let xdg_runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        return Ok(select_runtime_dir(
            config_dir,
            xdg_runtime,
            unsafe { libc::geteuid() },
        ));
    }
    #[cfg(not(unix))]
    if let Some(path) = config_dir {
        return Ok(path.join("run"));
    }
    #[allow(unreachable_code)]
    Err(AppError::Message(
        "无法确定 daemon 运行目录".into(),
    ))
}

fn select_runtime_dir(
    config_dir: Option<PathBuf>,
    xdg_runtime: Option<PathBuf>,
    uid: u32,
) -> PathBuf {
    if let Some(config_dir) = config_dir {
        return config_dir.join("run");
    }
    if let Some(runtime) = xdg_runtime {
        return runtime.join("coding-tools-mcp");
    }
    PathBuf::from(format!("/tmp/coding-tools-mcp-{uid}"))
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
    for path in [&paths.pid, &paths.state] {
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
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, workspace_id);
        false
    }
}

fn signal(pid: u32, value: i32) -> AppResult<()> {
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as i32, value) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(AppError::Message(format!(
            "发送信号 {value} 到 PID {pid} 失败：{error}"
        )));
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, value);
        Err(AppError::Message("daemon 目前仅支持 Linux".into()))
    }
}

fn ensure_linux() -> AppResult<()> {
    if supported() {
        Ok(())
    } else {
        Err(AppError::Message(
            "daemon 目前仅支持 Linux；请使用 serve 前台模式".into(),
        ))
    }
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
    options.create(true).write(true).append(append).truncate(!append);
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
    fs::rename(&temp, path)?;
    Ok(())
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
    fn xdg_runtime_is_preferred_over_tmp_fallback() {
        assert_eq!(
            select_runtime_dir(None, Some(PathBuf::from("/run/user/1000")), 1000),
            PathBuf::from("/run/user/1000/coding-tools-mcp")
        );
        assert_eq!(
            select_runtime_dir(None, None, 1000),
            PathBuf::from("/tmp/coding-tools-mcp-1000")
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
            log_path: "/tmp/daemon.log".into(),
            version: "1".into(),
        };

        let value = serde_json::to_value(state).expect("serialize state");

        assert_eq!(value["service"], "all");
        assert_eq!(value["tunnel"], true);
        assert_eq!(value["pid"], 42);
    }

    #[test]
    fn parses_internal_daemon_command_line() {
        assert_eq!(
            parse_daemon_args(
                &[
                    "/usr/local/bin/coding-tools-mcp",
                    "daemon-run",
                    "workspace",
                    "--service",
                    "all",
                    "--tunnel",
                ],
                "workspace",
            ),
            Some((ServiceSelection::All, true))
        );
        assert_eq!(
            parse_daemon_args(&["coding-tools-mcp", "daemon-run", "other"], "workspace"),
            None
        );
    }
}
