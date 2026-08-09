use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::platform::platform;

const STATE_SCHEMA_VERSION: u32 = 2;
#[cfg(unix)]
const SIGTERM_VALUE: i32 = 15;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayDaemonState {
    pub schema_version: u32,
    pub config_scope: String,
    pub pid: u32,
    pub started_at_unix: u64,
    pub workspace_ids: Vec<String>,
    pub local_port: u16,
    pub log_path: String,
    pub version: String,
    #[serde(default)]
    pub executable_path: String,
}

#[cfg(windows)]
fn sanitize_pipe_component(value: &str) -> String {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayDaemonInspection {
    pub supported: bool,
    pub running: bool,
    pub stale: bool,
    pub ambiguous: bool,
    pub pid_matches: bool,
    pub state: Option<GatewayDaemonState>,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct GatewayDaemonPaths {
    dir: PathBuf,
    lock: PathBuf,
    pid: PathBuf,
    state: PathBuf,
    control: PathBuf,
}

pub struct GatewayDaemonGuard {
    lock_file: File,
    paths: GatewayDaemonPaths,
    pid: u32,
}

impl Drop for GatewayDaemonGuard {
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

pub fn config_scope() -> AppResult<String> {
    let config_dir = platform().app_config_dir()?;
    let digest = Sha256::digest(config_dir.to_string_lossy().as_bytes());
    Ok(digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn daemon_log_path() -> AppResult<PathBuf> {
    Ok(platform()
        .app_config_dir()?
        .join("logs")
        .join("gateway")
        .join("daemon.log"))
}

pub(crate) fn append_log(line: &str) {
    let Ok(path) = daemon_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if ensure_private_dir(parent).is_err() {
            return;
        }
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", crate::logging::timestamped_line(line));
    }
}

#[cfg(unix)]
pub(crate) fn control_socket_path() -> AppResult<PathBuf> {
    Ok(gateway_paths()?.control)
}

#[cfg(windows)]
pub(crate) fn control_pipe_name() -> AppResult<String> {
    let user = crate::windows_service::pipe_identity_user();
    Ok(format!(
        r"\\.\pipe\{}-{}-gateway-{}",
        crate::brand::SERVER_NAME,
        config_scope()?,
        sanitize_pipe_component(&user)
    ))
}

pub fn inspect() -> AppResult<GatewayDaemonInspection> {
    if !supported() {
        return Ok(GatewayDaemonInspection {
            supported: false,
            running: false,
            stale: false,
            ambiguous: false,
            pid_matches: false,
            state: None,
            detail: "Gateway daemon 当前仅支持 Windows 和 Linux".into(),
        });
    }
    let scope = config_scope()?;
    let paths = gateway_paths()?;
    let mut state_error = None;
    let state = match read_state(&paths.state) {
        Ok(state) => state,
        Err(error) => {
            state_error = Some(error.to_string());
            None
        }
    };
    let state = match state {
        Some(state)
            if state.schema_version != STATE_SCHEMA_VERSION || state.config_scope != scope =>
        {
            state_error = Some(format!(
                "Gateway daemon 状态文件与当前配置域/schema 不匹配：scope={} schema={}",
                state.config_scope, state.schema_version
            ));
            None
        }
        other => other,
    };
    if let Some(state) = state.as_ref() {
        let alive = platform().is_process_alive(state.pid);
        let pid_matches = alive && process_matches_gateway_daemon(state.pid, &scope);
        if alive && pid_matches {
            return Ok(running_inspection(state.clone(), false));
        }
    }

    let discovered = discover_gateway_daemons(&scope)?;
    if discovered.len() == 1 {
        return Ok(running_inspection(discovered[0].clone(), true));
    }
    if discovered.len() > 1 {
        return Ok(GatewayDaemonInspection {
            supported: true,
            running: false,
            stale: true,
            ambiguous: true,
            pid_matches: false,
            state: None,
            detail: format!(
                "发现 {} 个匹配当前配置域的 Gateway daemon，拒绝自动选择",
                discovered.len()
            ),
        });
    }

    let Some(state) = state else {
        return Ok(GatewayDaemonInspection {
            supported: true,
            running: false,
            stale: state_error.is_some(),
            ambiguous: false,
            pid_matches: false,
            state: None,
            detail: state_error
                .map(|error| format!("Gateway daemon 状态文件无效：{error}"))
                .unwrap_or_else(|| "Gateway daemon 未运行".into()),
        });
    };
    let alive = platform().is_process_alive(state.pid);
    let pid_matches = alive && process_matches_gateway_daemon(state.pid, &scope);
    Ok(GatewayDaemonInspection {
        supported: true,
        running: false,
        stale: true,
        ambiguous: false,
        pid_matches,
        detail: if alive {
            format!(
                "Gateway 状态文件中的 PID {} 属于其他进程，拒绝接管",
                state.pid
            )
        } else {
            format!("Gateway daemon 状态已过期，PID {} 不存在", state.pid)
        },
        state: Some(state),
    })
}

fn running_inspection(state: GatewayDaemonState, recovered: bool) -> GatewayDaemonInspection {
    GatewayDaemonInspection {
        supported: true,
        running: true,
        stale: false,
        ambiguous: false,
        pid_matches: true,
        detail: format!(
            "Gateway daemon 正在运行，PID {}，routes={}{}",
            state.pid,
            state.workspace_ids.len(),
            if recovered {
                "（从 /proc 恢复）"
            } else {
                ""
            }
        ),
        state: Some(state),
    }
}

pub fn acquire(workspace_ids: &[String], local_port: u16) -> AppResult<GatewayDaemonGuard> {
    ensure_supported()?;
    let paths = gateway_paths()?;
    ensure_private_dir(&paths.dir)?;
    let lock_file = open_private_file(&paths.lock, false)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| AppError::Message("当前配置域已有 Gateway daemon 正在启动或运行".into()))?;
    cleanup_stale_files(&paths)?;
    let pid = std::process::id();
    let state = GatewayDaemonState {
        schema_version: STATE_SCHEMA_VERSION,
        config_scope: config_scope()?,
        pid,
        started_at_unix: unix_now(),
        workspace_ids: normalized_workspace_ids(workspace_ids),
        local_port,
        log_path: daemon_log_path()?.display().to_string(),
        version: env!("CARGO_PKG_VERSION").into(),
        executable_path: std::env::current_exe()?.display().to_string(),
    };
    atomic_write_json(&paths.state, &state)?;
    write_private_text(&paths.pid, &format!("{pid}\n"))?;
    Ok(GatewayDaemonGuard {
        lock_file,
        paths,
        pid,
    })
}

pub fn update_state(workspace_ids: &[String], local_port: u16) -> AppResult<()> {
    ensure_supported()?;
    let paths = gateway_paths()?;
    let mut state = read_state(&paths.state)?.ok_or_else(|| {
        AppError::Message("Gateway daemon state disappeared while updating runtime metadata".into())
    })?;
    let current_pid = std::process::id();
    let scope = config_scope()?;
    if state.pid != current_pid || state.config_scope != scope {
        return Err(AppError::Message(format!(
            "refusing to update Gateway daemon state owned by PID {} / scope {}",
            state.pid, state.config_scope
        )));
    }
    state.workspace_ids = normalized_workspace_ids(workspace_ids);
    state.local_port = local_port;
    atomic_write_json(&paths.state, &state)
}

pub fn spawn(workspace_ids: &[String]) -> AppResult<u32> {
    ensure_supported()?;
    let inspection = inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.running {
        return Err(AppError::Message(
            "当前配置域的 Gateway daemon 已运行".into(),
        ));
    }
    if inspection.state.is_some() && !inspection.pid_matches {
        cleanup()?;
    }

    let workspace_ids = normalized_workspace_ids(workspace_ids);
    if workspace_ids.is_empty() {
        return Err(AppError::Message(
            "Gateway daemon 至少需要一个 workspace route".into(),
        ));
    }
    let log_path = daemon_log_path()?;
    if let Some(parent) = log_path.parent() {
        ensure_private_dir(parent)?;
    }
    let stdout = open_private_file(&log_path, true)?;
    let stderr = stdout.try_clone()?;
    let executable = std::env::current_exe()?;
    let config_dir = platform().app_config_dir()?;
    fs::create_dir_all(&config_dir)?;
    let mut command = Command::new(executable);
    command.arg("gateway-daemon-run").arg(config_scope()?);
    for workspace_id in &workspace_ids {
        command.arg(workspace_id);
    }
    command
        .current_dir(config_dir)
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
        .map_err(|error| AppError::Message(format!("启动 Gateway daemon 子进程失败：{error}")))?;
    Ok(child.id())
}

pub async fn wait_ready(expected_pid: u32, timeout: Duration) -> AppResult<GatewayDaemonState> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let inspection = inspect()?;
        if let Some(state) = inspection.state.clone() {
            if state.pid != expected_pid {
                return Err(AppError::Message(format!(
                    "Gateway daemon 启动竞争：预期 PID {expected_pid}，状态文件属于 PID {}",
                    state.pid
                )));
            }
            if inspection.running
                && platform().find_pid_listening_on_port(state.local_port)? == Some(state.pid)
            {
                match crate::gateway_control::ping().await {
                    Ok(()) => return Ok(state),
                    Err(error) if error.is_unavailable() => {}
                    Err(error) => {
                        return Err(AppError::Message(format!(
                            "Gateway daemon control endpoint failed readiness check: {error}"
                        )))
                    }
                }
            }
            if !platform().is_process_alive(state.pid) {
                return Err(AppError::Message(format!(
                    "Gateway daemon 启动后立即退出，请查看 {}",
                    daemon_log_path()?.display()
                )));
            }
        } else if !platform().is_process_alive(expected_pid) {
            return Err(AppError::Message(format!(
                "Gateway daemon 子进程 PID {expected_pid} 在写入状态前退出，请查看 {}",
                daemon_log_path()?.display()
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Message(format!(
                "等待 Gateway daemon 就绪超时，请查看 {}",
                daemon_log_path()?.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn terminate_spawned(pid: u32) -> AppResult<()> {
    ensure_supported()?;
    let scope = config_scope()?;
    if !platform().is_process_alive(pid) {
        cleanup()?;
        return Ok(());
    }
    if !process_matches_gateway_daemon(pid, &scope) {
        return Err(AppError::Message(format!(
            "启动失败后的 PID {pid} 不再匹配当前 Gateway daemon，拒绝终止"
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
    cleanup()
}

pub async fn wait_for_exit(pid: u32, timeout: Duration, force: bool) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    while platform().is_process_alive(pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if platform().is_process_alive(pid) {
        if !force {
            return Err(AppError::Message(format!(
                "等待 Gateway daemon PID {pid} 退出超时；可显式使用 --force"
            )));
        }
        let scope = config_scope()?;
        if !process_matches_gateway_daemon(pid, &scope) {
            return Err(AppError::Message(format!(
                "PID {pid} 已不属于当前配置域 Gateway daemon，拒绝强制终止"
            )));
        }
        platform().terminate_process_tree(pid)?;
    }
    cleanup()
}

pub fn cleanup() -> AppResult<()> {
    if !supported() {
        return Ok(());
    }
    cleanup_stale_files(&gateway_paths()?)
}

fn gateway_paths() -> AppResult<GatewayDaemonPaths> {
    Ok(gateway_paths_in(crate::daemon::runtime_dir()?))
}

fn gateway_paths_in(dir: PathBuf) -> GatewayDaemonPaths {
    GatewayDaemonPaths {
        lock: dir.join("gateway.lock"),
        pid: dir.join("gateway.pid"),
        state: dir.join("gateway.json"),
        control: dir.join("gateway.sock"),
        dir,
    }
}

fn read_state(path: &Path) -> AppResult<Option<GatewayDaemonState>> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| AppError::Message(format!("Gateway daemon 状态文件损坏：{error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn cleanup_stale_files(paths: &GatewayDaemonPaths) -> AppResult<()> {
    for path in [&paths.pid, &paths.state, &paths.control] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn discover_gateway_daemons(scope: &str) -> AppResult<Vec<GatewayDaemonState>> {
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
            let args = cmdline_parts(&raw);
            let Some(workspace_ids) = parse_gateway_daemon_args(&args, scope) else {
                continue;
            };
            states.push(GatewayDaemonState {
                schema_version: STATE_SCHEMA_VERSION,
                config_scope: scope.to_string(),
                pid,
                started_at_unix: 0,
                workspace_ids,
                local_port: 0,
                log_path: daemon_log_path()?.display().to_string(),
                version: "unknown".into(),
                executable_path: args.first().copied().unwrap_or_default().to_string(),
            });
        }
        states.sort_by_key(|state| state.pid);
        Ok(states)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = scope;
        Ok(Vec::new())
    }
}

fn process_matches_gateway_daemon(pid: u32, scope: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else {
            return false;
        };
        let args = cmdline_parts(&raw);
        parse_gateway_daemon_args(&args, scope).is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(windows)]
        {
            let Ok(paths) = gateway_paths() else {
                return false;
            };
            let Ok(Some(state)) = read_state(&paths.state) else {
                return false;
            };
            if state.pid != pid
                || state.config_scope != scope
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
        #[cfg(not(windows))]
        {
            let _ = (pid, scope);
            false
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_gateway_daemon_args(args: &[&str], expected_scope: &str) -> Option<Vec<String>> {
    let index = args.iter().position(|arg| *arg == "gateway-daemon-run")?;
    if args.get(index + 1).copied()? != expected_scope {
        return None;
    }
    let workspace_ids = args
        .iter()
        .skip(index + 2)
        .take_while(|arg| !arg.starts_with('-'))
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    (!workspace_ids.is_empty()).then_some(normalized_workspace_ids(&workspace_ids))
}

#[cfg(target_os = "linux")]
fn cmdline_parts(raw: &[u8]) -> Vec<&str> {
    raw.split(|byte| *byte == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .filter(|part| !part.is_empty())
        .collect()
}

fn normalized_workspace_ids(workspace_ids: &[String]) -> Vec<String> {
    let mut ids = workspace_ids
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn ensure_private_dir(path: &Path) -> AppResult<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_private_file(path: &Path, append: bool) -> AppResult<File> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if append {
        options.append(true);
    } else {
        options.truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn write_private_text(path: &Path, content: &str) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Message("Gateway daemon state path has no parent".into()))?;
    ensure_private_dir(parent)?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    write_private_text(&temp, &serde_json::to_string_pretty(value)?)?;
    fs::rename(temp, path)?;
    Ok(())
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
        "发送信号 {value} 到 Gateway daemon PID {pid} 失败：{error}"
    )))
}

fn ensure_supported() -> AppResult<()> {
    if supported() {
        Ok(())
    } else {
        Err(AppError::Message(
            "Gateway daemon 当前仅支持 Windows 和 Linux；可继续使用 gateway serve 前台模式".into(),
        ))
    }
}

#[cfg(windows)]
fn normalize_windows_image_path(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_paths_use_a_global_namespace() {
        let paths = gateway_paths_in(PathBuf::from("/tmp/anchor"));
        assert_eq!(paths.control, PathBuf::from("/tmp/anchor/gateway.sock"));
        assert_eq!(paths.state, PathBuf::from("/tmp/anchor/gateway.json"));
        assert_eq!(paths.pid, PathBuf::from("/tmp/anchor/gateway.pid"));
    }

    #[test]
    fn gateway_daemon_args_require_matching_config_scope() {
        let args = [
            "anchor",
            "gateway-daemon-run",
            "scope-a",
            "workspace-b",
            "workspace-a",
            "workspace-a",
        ];
        assert_eq!(
            parse_gateway_daemon_args(&args, "scope-a"),
            Some(vec!["workspace-a".into(), "workspace-b".into()])
        );
        assert_eq!(parse_gateway_daemon_args(&args, "scope-b"), None);
    }
}
