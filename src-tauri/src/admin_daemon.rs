use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use sha2::Digest;

use crate::build_identity::BuildIdentity;
use crate::error::{AppError, AppResult};
use crate::platform::platform;

const STATE_SCHEMA_VERSION: u32 = 1;
#[cfg(unix)]
const SIGTERM_VALUE: i32 = 15;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDaemonState {
    pub schema_version: u32,
    pub config_scope: String,
    pub pid: u32,
    pub started_at_unix: u64,
    pub port: u16,
    pub log_path: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_identity: Option<BuildIdentity>,
    #[serde(default)]
    pub executable_path: String,
}

fn normalize_process_path(path: &Path) -> String {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        return resolved
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
    }
    #[cfg(not(windows))]
    {
        resolved.to_string_lossy().to_string()
    }
}

pub fn recover_registered_listener(port: u16, executable: &Path) -> AppResult<bool> {
    ensure_supported()?;
    if read_state(&admin_paths()?.state)?.is_some() {
        return Ok(false);
    }
    let Some(pid) = platform().find_pid_listening_on_port(port)? else {
        return Ok(false);
    };
    let Some(actual) = platform().process_image_path(pid)? else {
        return Ok(false);
    };
    if normalize_process_path(executable) != normalize_process_path(Path::new(&actual)) {
        return Ok(false);
    }
    let paths = admin_paths()?;
    ensure_private_dir(&paths.dir)?;
    let state = AdminDaemonState {
        schema_version: STATE_SCHEMA_VERSION,
        config_scope: config_scope()?,
        pid,
        started_at_unix: 0,
        port,
        log_path: daemon_log_path()?.display().to_string(),
        version: "unknown".into(),
        build_identity: None,
        executable_path: actual,
    };
    atomic_write_json(&paths.state, &state)?;
    write_private_text(&paths.pid, &format!("{pid}\n"))?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDaemonInspection {
    pub supported: bool,
    pub running: bool,
    pub stale: bool,
    pub ambiguous: bool,
    pub pid_matches: bool,
    pub state: Option<AdminDaemonState>,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct AdminDaemonPaths {
    dir: PathBuf,
    lock: PathBuf,
    pid: PathBuf,
    state: PathBuf,
}

pub struct AdminDaemonGuard {
    lock_file: File,
    paths: AdminDaemonPaths,
    pid: u32,
}

impl Drop for AdminDaemonGuard {
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
    cfg!(any(target_os = "linux", target_os = "windows"))
}

pub fn config_scope() -> AppResult<String> {
    crate::gateway_daemon::config_scope()
}

pub fn daemon_log_path() -> AppResult<PathBuf> {
    Ok(platform()
        .app_config_dir()?
        .join("logs")
        .join("admin")
        .join("daemon.log"))
}

pub fn inspect() -> AppResult<AdminDaemonInspection> {
    if !supported() {
        return Ok(AdminDaemonInspection {
            supported: false,
            running: false,
            stale: false,
            ambiguous: false,
            pid_matches: false,
            state: None,
            detail: "Admin daemon 当前仅支持 Windows 和 Linux".into(),
        });
    }
    let scope = config_scope()?;
    let paths = admin_paths()?;
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
                "Admin daemon 状态文件与当前配置域/schema 不匹配：scope={} schema={}",
                state.config_scope, state.schema_version
            ));
            None
        }
        other => other,
    };
    if let Some(state) = state.as_ref() {
        let alive = platform().is_process_alive(state.pid);
        let pid_matches = alive && process_matches_admin_daemon(state.pid, &scope);
        if alive && pid_matches {
            return Ok(running_inspection(state.clone(), false));
        }
    }

    let discovered = discover_admin_daemons(&scope)?;
    if discovered.len() == 1 {
        return Ok(running_inspection(discovered[0].clone(), true));
    }
    if discovered.len() > 1 {
        return Ok(AdminDaemonInspection {
            supported: true,
            running: false,
            stale: true,
            ambiguous: true,
            pid_matches: false,
            state: None,
            detail: format!(
                "发现 {} 个匹配当前配置域的 Admin daemon，拒绝自动选择",
                discovered.len()
            ),
        });
    }

    let Some(state) = state else {
        return Ok(AdminDaemonInspection {
            supported: true,
            running: false,
            stale: state_error.is_some(),
            ambiguous: false,
            pid_matches: false,
            state: None,
            detail: state_error
                .map(|error| format!("Admin daemon 状态文件无效：{error}"))
                .unwrap_or_else(|| "Admin daemon 未运行".into()),
        });
    };
    let alive = platform().is_process_alive(state.pid);
    let pid_matches = alive && process_matches_admin_daemon(state.pid, &scope);
    Ok(AdminDaemonInspection {
        supported: true,
        running: false,
        stale: true,
        ambiguous: false,
        pid_matches,
        detail: if alive {
            format!(
                "Admin 状态文件中的 PID {} 属于其他进程，拒绝接管",
                state.pid
            )
        } else {
            format!("Admin daemon 状态已过期，PID {} 不存在", state.pid)
        },
        state: Some(state),
    })
}

fn running_inspection(state: AdminDaemonState, recovered: bool) -> AdminDaemonInspection {
    AdminDaemonInspection {
        supported: true,
        running: true,
        stale: false,
        ambiguous: false,
        pid_matches: true,
        detail: format!(
            "Admin daemon 正在运行，PID {}，port={}{}",
            state.pid,
            state.port,
            if recovered {
                "（从 /proc 恢复）"
            } else {
                ""
            }
        ),
        state: Some(state),
    }
}

pub fn acquire(port: u16) -> AppResult<AdminDaemonGuard> {
    ensure_supported()?;
    let paths = admin_paths()?;
    ensure_private_dir(&paths.dir)?;
    let lock_file = open_private_file(&paths.lock, false)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| AppError::Message("当前配置域已有 Admin daemon 正在启动或运行".into()))?;
    cleanup_stale_files(&paths)?;
    let pid = std::process::id();
    let state = AdminDaemonState {
        schema_version: STATE_SCHEMA_VERSION,
        config_scope: config_scope()?,
        pid,
        started_at_unix: unix_now(),
        port,
        log_path: daemon_log_path()?.display().to_string(),
        version: env!("CARGO_PKG_VERSION").into(),
        build_identity: Some(BuildIdentity::current()),
        executable_path: std::env::current_exe()?.display().to_string(),
    };
    atomic_write_json(&paths.state, &state)?;
    write_private_text(&paths.pid, &format!("{pid}\n"))?;
    Ok(AdminDaemonGuard {
        lock_file,
        paths,
        pid,
    })
}

pub async fn run(port: u16, as_json: bool) -> AppResult<()> {
    let _guard = acquire(port)?;
    crate::admin::serve(port, as_json).await
}

pub fn spawn(port: u16) -> AppResult<u32> {
    let executable = std::env::current_exe()?;
    spawn_from_executable(port, &executable)
}

pub(crate) fn spawn_from_executable(port: u16, executable: &Path) -> AppResult<u32> {
    ensure_supported()?;
    let inspection = inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.running {
        return Err(AppError::Message("当前配置域的 Admin daemon 已运行".into()));
    }
    if inspection.state.is_some() && !inspection.pid_matches {
        cleanup()?;
    }
    if let Some(pid) = platform().find_pid_listening_on_port(port)? {
        return Err(AppError::Message(format!(
            "Admin daemon 无法启动：127.0.0.1:{port} 已由 PID {pid} 监听；不会接管其他进程端口"
        )));
    }

    let log_path = daemon_log_path()?;
    if let Some(parent) = log_path.parent() {
        ensure_private_dir(parent)?;
    }
    let stdout = open_private_file(&log_path, true)?;
    let stderr = stdout.try_clone()?;
    let config_dir = platform().app_config_dir()?;
    fs::create_dir_all(&config_dir)?;
    let mut command = Command::new(executable);
    command
        .arg("--config-dir")
        .arg(&config_dir)
        .arg("admin")
        .arg("daemon-run")
        .arg("--port")
        .arg(port.to_string())
        .current_dir(&config_dir)
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
        .map_err(|error| AppError::Message(format!("启动 Admin daemon 子进程失败：{error}")))?;
    Ok(child.id())
}

pub async fn wait_ready(expected_pid: u32, timeout: Duration) -> AppResult<AdminDaemonState> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let inspection = inspect()?;
        if let Some(state) = inspection.state.clone() {
            if state.pid != expected_pid {
                return Err(AppError::Message(format!(
                    "Admin daemon 启动竞争：预期 PID {expected_pid}，状态文件属于 PID {}",
                    state.pid
                )));
            }
            if inspection.running && health_ready(state.port).await {
                return Ok(state);
            }
            if !platform().is_process_alive(state.pid) {
                return Err(AppError::Message(format!(
                    "Admin daemon 启动后立即退出，请查看 {}",
                    daemon_log_path()?.display()
                )));
            }
        } else if !platform().is_process_alive(expected_pid) {
            return Err(AppError::Message(format!(
                "Admin daemon 子进程 PID {expected_pid} 在写入状态前退出，请查看 {}",
                daemon_log_path()?.display()
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Message(format!(
                "等待 Admin daemon 就绪超时，请查看 {}",
                daemon_log_path()?.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn health_ready(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/api/v1/health");
    let Ok(response) = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_millis(500))
        .send()
        .await
    else {
        return false;
    };
    response.status().is_success()
}

pub async fn wait_until_ready(port: u16, timeout: Duration) -> AppResult<AdminDaemonState> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let inspection = inspect()?;
        if inspection.ambiguous {
            return Err(AppError::Message(inspection.detail));
        }
        if inspection.running {
            if let Some(state) = inspection.state {
                if state.port != port {
                    return Err(AppError::Message(format!(
                        "Admin daemon 已在端口 {} 运行，预期端口 {port}",
                        state.port
                    )));
                }
                if health_ready(port).await {
                    return Ok(state);
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Message(format!(
                "等待 Admin daemon 端口 {port} 就绪超时，请查看 {}",
                daemon_log_path()?.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn stop(timeout: Duration, force: bool) -> AppResult<()> {
    ensure_supported()?;
    let inspection = inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let Some(state) = inspection.state else {
        cleanup()?;
        return Ok(());
    };
    if !inspection.running || !inspection.pid_matches {
        cleanup()?;
        return Ok(());
    }
    terminate_matching(state.pid)?;
    wait_for_exit(state.pid, timeout, force).await
}

pub async fn wait_for_exit(pid: u32, timeout: Duration, force: bool) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    while platform().is_process_alive(pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if platform().is_process_alive(pid) {
        if !force {
            return Err(AppError::Message(format!(
                "等待 Admin daemon PID {pid} 退出超时；可显式使用 --force"
            )));
        }
        if !process_matches_admin_daemon(pid, &config_scope()?) {
            return Err(AppError::Message(format!(
                "PID {pid} 已不属于当前配置域 Admin daemon，拒绝强制终止"
            )));
        }
        platform().terminate_process_tree(pid)?;
    }
    cleanup()
}

fn terminate_matching(pid: u32) -> AppResult<()> {
    if !process_matches_admin_daemon(pid, &config_scope()?) {
        return Err(AppError::Message(format!(
            "PID {pid} 不匹配当前配置域 Admin daemon，拒绝终止"
        )));
    }
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as i32, SIGTERM_VALUE) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(windows)]
    platform().terminate_process_tree(pid)?;
    Ok(())
}

pub fn cleanup() -> AppResult<()> {
    if !supported() {
        return Ok(());
    }
    cleanup_stale_files(&admin_paths()?)
}

fn admin_paths() -> AppResult<AdminDaemonPaths> {
    Ok(admin_paths_in(crate::daemon::runtime_dir()?))
}

fn admin_paths_in(dir: PathBuf) -> AdminDaemonPaths {
    AdminDaemonPaths {
        lock: dir.join("admin.lock"),
        pid: dir.join("admin.pid"),
        state: dir.join("admin.json"),
        dir,
    }
}

fn read_state(path: &Path) -> AppResult<Option<AdminDaemonState>> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| AppError::Message(format!("Admin daemon 状态文件损坏：{error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn cleanup_stale_files(paths: &AdminDaemonPaths) -> AppResult<()> {
    for path in [&paths.pid, &paths.state] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn discover_admin_daemons(scope: &str) -> AppResult<Vec<AdminDaemonState>> {
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
            let Some(port) = parse_admin_daemon_args(&args) else {
                continue;
            };
            if process_scope_from_args(&args).as_deref() != Some(scope) {
                continue;
            }
            states.push(AdminDaemonState {
                schema_version: STATE_SCHEMA_VERSION,
                config_scope: scope.to_string(),
                pid,
                started_at_unix: 0,
                port,
                log_path: daemon_log_path()?.display().to_string(),
                version: "unknown".into(),
                build_identity: None,
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

fn process_matches_admin_daemon(pid: u32, scope: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else {
            return false;
        };
        let args = cmdline_parts(&raw);
        parse_admin_daemon_args(&args).is_some()
            && process_scope_from_args(&args).as_deref() == Some(scope)
    }
    #[cfg(windows)]
    {
        let Ok(paths) = admin_paths() else {
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
        normalize_process_path(Path::new(&state.executable_path))
            == normalize_process_path(Path::new(&actual))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (pid, scope);
        false
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_admin_daemon_args(args: &[&str]) -> Option<u16> {
    let index = args.iter().position(|arg| *arg == "daemon-run")?;
    if args.get(index.wrapping_sub(1)).copied()? != "admin" {
        return None;
    }
    let port_index = args
        .iter()
        .skip(index + 1)
        .position(|arg| *arg == "--port")?
        + index
        + 1;
    args.get(port_index + 1)?.parse::<u16>().ok()
}

#[cfg(target_os = "linux")]
fn process_scope_from_args(args: &[&str]) -> Option<String> {
    let config_index = args.iter().position(|arg| *arg == "--config-dir")?;
    let config_dir = PathBuf::from(args.get(config_index + 1)?);
    let digest = sha2::Sha256::digest(config_dir.to_string_lossy().as_bytes());
    Some(
        digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

#[cfg(target_os = "linux")]
fn cmdline_parts(raw: &[u8]) -> Vec<&str> {
    raw.split(|byte| *byte == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .filter(|part| !part.is_empty())
        .collect()
}

fn ensure_supported() -> AppResult<()> {
    if supported() {
        Ok(())
    } else {
        Err(AppError::Message(
            "Admin daemon 当前仅支持 Windows 和 Linux".into(),
        ))
    }
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
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
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

fn set_private_file_permissions(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let _ = path;
    Ok(())
}

fn write_private_text(path: &Path, value: &str) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    fs::write(path, value)?;
    set_private_file_permissions(path)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let payload = serde_json::to_vec_pretty(value)?;
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)?;
        set_private_file_permissions(&temp)?;
        file.write_all(&payload)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    set_private_file_permissions(path)
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
    fn admin_paths_are_isolated_from_gateway_and_workspace_state() {
        let paths = admin_paths_in(PathBuf::from("/tmp/anchor"));
        assert_eq!(paths.lock, PathBuf::from("/tmp/anchor/admin.lock"));
        assert_eq!(paths.pid, PathBuf::from("/tmp/anchor/admin.pid"));
        assert_eq!(paths.state, PathBuf::from("/tmp/anchor/admin.json"));
    }

    #[test]
    fn parses_internal_admin_daemon_command() {
        let args = [
            "/usr/local/bin/anchor",
            "--config-dir",
            "/home/demo/.config/anchor",
            "admin",
            "daemon-run",
            "--port",
            "28769",
        ];
        assert_eq!(parse_admin_daemon_args(&args), Some(28_769));
        assert_eq!(parse_admin_daemon_args(&["anchor", "admin", "serve"]), None);
    }
}
