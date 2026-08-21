use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::admin_daemon::{self, AdminDaemonInspection, AdminDaemonState};
use crate::build_identity::BuildIdentity;
use crate::error::{AppError, AppResult};

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetOEMCP() -> u32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        source: *const u8,
        source_len: i32,
        destination: *mut u16,
        destination_len: i32,
    ) -> i32;
}
use crate::platform::platform;

const SERVICE_CONFIG_SCHEMA_VERSION: u32 = 1;
const START_TIMEOUT: Duration = Duration::from_secs(20);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminServiceConfig {
    pub schema_version: u32,
    pub manager: String,
    pub port: u16,
    pub executable_path: String,
    pub config_dir: String,
    pub installed_build: BuildIdentity,
}

#[cfg(target_os = "linux")]
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
            "无法启用当前用户 linger，不能保证 Admin service 重启后自动启动：{}",
            String::from_utf8_lossy(&enable.stderr).trim()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminServiceStatus {
    pub supported: bool,
    pub manager: String,
    pub installed: bool,
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    pub build_state: String,
    pub current_build: BuildIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon: Option<AdminDaemonState>,
    pub detail: String,
}

pub fn supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "windows"))
}

pub async fn status() -> AppResult<AdminServiceStatus> {
    let current_build = BuildIdentity::current();
    if !supported() {
        return Ok(AdminServiceStatus {
            supported: false,
            manager: "unsupported".into(),
            installed: false,
            enabled: false,
            running: false,
            port: crate::admin::DEFAULT_ADMIN_PORT,
            build_state: "unsupported".into(),
            current_build,
            daemon: None,
            detail: "Persistent Admin service 当前仅支持 Windows 和 Linux".into(),
        });
    }

    let config = read_service_config()?;
    if let Some(config) = config.as_ref() {
        let _ = admin_daemon::recover_registered_listener(
            config.port,
            Path::new(&config.executable_path),
        )?;
    }
    let inspection = admin_daemon::inspect()?;
    let installed = platform_registered(config.as_ref())?;
    let enabled = if installed {
        platform_enabled()?
    } else {
        false
    };
    let port = if installed {
        config
            .as_ref()
            .map(|value| value.port)
            .unwrap_or(crate::admin::DEFAULT_ADMIN_PORT)
    } else {
        inspection
            .state
            .as_ref()
            .map(|value| value.port)
            .or_else(|| config.as_ref().map(|value| value.port))
            .unwrap_or(crate::admin::DEFAULT_ADMIN_PORT)
    };
    let daemon = inspection.state.clone().filter(|_| inspection.running);
    let build_state = if let Some(build) = daemon
        .as_ref()
        .and_then(|state| state.build_identity.as_ref())
    {
        if build.same_build(&current_build) {
            "current"
        } else {
            "different"
        }
    } else if inspection.running {
        "unknown"
    } else if installed {
        if config
            .as_ref()
            .is_some_and(|value| !value.installed_build.same_build(&current_build))
        {
            "different"
        } else {
            "stopped"
        }
    } else {
        "not_installed"
    }
    .to_string();

    Ok(AdminServiceStatus {
        supported: true,
        manager: manager_name().into(),
        installed,
        enabled,
        running: inspection.running,
        port,
        build_state,
        current_build,
        daemon,
        detail: status_detail(&inspection, installed, enabled, config.as_ref()),
    })
}

pub async fn start(port_override: Option<u16>) -> AppResult<AdminServiceStatus> {
    ensure_supported()?;
    let config = read_service_config()?;
    let registered = platform_registered(config.as_ref())?;
    if config.is_some() && !registered {
        return Err(AppError::Message(
            "Admin service config 存在但 OS autostart 注册缺失；请运行 `anchor admin upgrade` 修复，不会静默降级为非托管后台进程"
                .into(),
        ));
    }
    if registered {
        let config = config.ok_or_else(|| {
            AppError::Message(
                "Admin service 已注册但本地 service config 缺失；请运行 admin install 修复".into(),
            )
        })?;
        if let Some(port) = port_override {
            if port != config.port {
                return Err(AppError::Message(format!(
                    "Admin service 已固定端口 {}；如需修改请重新运行 `anchor admin install --port {port}`",
                    config.port
                )));
            }
        }
        cleanup_stale_runtime()?;
        platform_start()?;
        admin_daemon::wait_until_ready(config.port, START_TIMEOUT).await?;
        return status().await;
    }

    let port = port_override.unwrap_or(crate::admin::DEFAULT_ADMIN_PORT);
    cleanup_stale_runtime()?;
    let pid = admin_daemon::spawn(port)?;
    if let Err(error) = admin_daemon::wait_ready(pid, START_TIMEOUT).await {
        let _ = admin_daemon::stop(Duration::from_secs(2), true).await;
        return Err(error);
    }
    status().await
}

pub async fn stop(force: bool) -> AppResult<AdminServiceStatus> {
    ensure_supported()?;
    let config = read_service_config()?;
    if platform_registered(config.as_ref())? {
        platform_stop()?;
        wait_until_stopped(force).await?;
    } else {
        admin_daemon::stop(STOP_TIMEOUT, force).await?;
    }
    status().await
}

pub async fn restart(port_override: Option<u16>, force: bool) -> AppResult<AdminServiceStatus> {
    ensure_supported()?;
    let config = read_service_config()?;
    if platform_registered(config.as_ref())? {
        let config = config.ok_or_else(|| AppError::Message("Admin service config 缺失".into()))?;
        if let Some(port) = port_override {
            if port != config.port {
                return Err(AppError::Message(format!(
                    "Admin service 已固定端口 {}；如需修改请重新运行 `anchor admin install --port {port}`",
                    config.port
                )));
            }
        }
        platform_restart()?;
        admin_daemon::wait_until_ready(config.port, START_TIMEOUT).await?;
        return status().await;
    }
    let port = port_override.unwrap_or_else(|| {
        admin_daemon::inspect()
            .ok()
            .and_then(|inspection| inspection.state.map(|state| state.port))
            .unwrap_or(crate::admin::DEFAULT_ADMIN_PORT)
    });
    admin_daemon::stop(STOP_TIMEOUT, force).await?;
    start(Some(port)).await
}

pub async fn install(port: u16) -> AppResult<AdminServiceStatus> {
    ensure_supported()?;
    let executable = fs::canonicalize(std::env::current_exe()?).map_err(|error| {
        AppError::Message(format!("无法解析当前 Anchor CLI 可执行文件：{error}"))
    })?;
    let config_dir = platform().app_config_dir()?;
    fs::create_dir_all(&config_dir)?;
    let config = AdminServiceConfig {
        schema_version: SERVICE_CONFIG_SCHEMA_VERSION,
        manager: manager_name().into(),
        port,
        executable_path: executable.display().to_string(),
        config_dir: config_dir.display().to_string(),
        installed_build: BuildIdentity::current(),
    };
    validate_service_config(&config)?;
    write_service_config(&config)?;
    if let Err(error) = platform_install(&config) {
        let _ = platform_uninstall();
        let _ = remove_service_config();
        return Err(error);
    }
    admin_daemon::wait_until_ready(port, START_TIMEOUT).await?;
    status().await
}

pub async fn uninstall(force: bool) -> AppResult<AdminServiceStatus> {
    ensure_supported()?;
    let config = read_service_config()?;
    if platform_registered(config.as_ref())? {
        platform_uninstall()?;
    }
    if let Err(error) = admin_daemon::stop(STOP_TIMEOUT, force).await {
        if !force {
            return Err(error);
        }
    }
    remove_service_config()?;
    admin_daemon::cleanup()?;
    status().await
}

pub async fn enable() -> AppResult<AdminServiceStatus> {
    ensure_installed()?;
    platform_enable()?;
    status().await
}

pub async fn disable() -> AppResult<AdminServiceStatus> {
    ensure_installed()?;
    platform_disable()?;
    status().await
}

pub async fn upgrade() -> AppResult<AdminServiceStatus> {
    ensure_supported()?;
    let config = read_service_config()?.ok_or_else(|| {
        AppError::Message("Admin service 尚未安装；请先运行 `anchor admin install`".into())
    })?;
    install(config.port).await
}

async fn wait_until_stopped(force: bool) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    loop {
        let inspection = admin_daemon::inspect()?;
        if !inspection.running {
            admin_daemon::cleanup()?;
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            if force {
                return admin_daemon::stop(Duration::from_secs(2), true).await;
            }
            return Err(AppError::Message(
                "等待 Admin service 停止超时；可显式使用 --force".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn cleanup_stale_runtime() -> AppResult<()> {
    let inspection = admin_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.stale && !inspection.running {
        admin_daemon::cleanup()?;
    }
    Ok(())
}

fn ensure_installed() -> AppResult<AdminServiceConfig> {
    let config =
        read_service_config()?.ok_or_else(|| AppError::Message("Admin service 尚未安装".into()))?;
    if !platform_registered(Some(&config))? {
        return Err(AppError::Message(
            "Admin service 注册缺失；请重新运行 `anchor admin install`".into(),
        ));
    }
    Ok(config)
}

fn status_detail(
    inspection: &AdminDaemonInspection,
    installed: bool,
    enabled: bool,
    config: Option<&AdminServiceConfig>,
) -> String {
    if inspection.running {
        return inspection.detail.clone();
    }
    if installed {
        return format!(
            "Admin service 已安装（autostart={}，port={}），当前未运行；{}",
            if enabled { "enabled" } else { "disabled" },
            config
                .map(|value| value.port)
                .unwrap_or(crate::admin::DEFAULT_ADMIN_PORT),
            inspection.detail
        );
    }
    if config.is_some() {
        return format!(
            "Admin service config 存在但 {} 注册缺失；运行 `anchor admin upgrade` 可重建注册；{}",
            manager_name(),
            inspection.detail
        );
    }
    inspection.detail.clone()
}

fn service_config_path() -> AppResult<PathBuf> {
    Ok(platform()
        .app_config_dir()?
        .join("admin")
        .join("service.json"))
}

fn read_service_config() -> AppResult<Option<AdminServiceConfig>> {
    let path = service_config_path()?;
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let config: AdminServiceConfig = serde_json::from_str(&raw).map_err(|error| {
                AppError::Message(format!("Admin service config 损坏：{error}"))
            })?;
            validate_service_config(&config)?;
            Ok(Some(config))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn validate_service_config(config: &AdminServiceConfig) -> AppResult<()> {
    if config.schema_version != SERVICE_CONFIG_SCHEMA_VERSION
        || config.manager != manager_name()
        || config.port == 0
        || !Path::new(&config.executable_path).is_absolute()
        || !Path::new(&config.config_dir).is_absolute()
    {
        return Err(AppError::Message(
            "Admin service config 与当前平台/schema 不匹配".into(),
        ));
    }
    Ok(())
}

fn write_service_config(config: &AdminServiceConfig) -> AppResult<()> {
    let path = service_config_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Message("Admin service config 路径无父目录".into()))?;
    fs::create_dir_all(parent)?;
    set_private_dir_permissions(parent)?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)?;
        set_private_file_permissions(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(config)?)?;
        file.sync_all()?;
    }
    fs::rename(&temp, &path)?;
    set_private_file_permissions(&path)
}

fn remove_service_config() -> AppResult<()> {
    match fs::remove_file(service_config_path()?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn ensure_supported() -> AppResult<()> {
    if supported() {
        Ok(())
    } else {
        Err(AppError::Message(
            "Persistent Admin service 当前仅支持 Windows 和 Linux".into(),
        ))
    }
}

fn manager_name() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "systemd-user"
    }
    #[cfg(windows)]
    {
        "task-scheduler"
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        "unsupported"
    }
}

#[cfg(target_os = "linux")]
fn unit_name() -> AppResult<String> {
    Ok(format!(
        "anchor-admin-{}.service",
        admin_daemon::config_scope()?
    ))
}

#[cfg(any(windows, test))]
fn encode_windows_task_xml(xml: &str) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(2 + xml.len() * 2);
    // schtasks /Create /XML is not reliably compatible with UTF-8 task files
    // on all supported Windows installations. Task Scheduler's native export
    // format is UTF-16 LE with a BOM, so emit that exact representation.
    encoded.extend_from_slice(&[0xFF, 0xFE]);
    for unit in xml.encode_utf16() {
        encoded.extend_from_slice(&unit.to_le_bytes());
    }
    encoded
}

#[cfg(windows)]
fn decode_windows_command_output(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Some(payload) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        if payload.len().is_multiple_of(2) {
            let units = payload
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
            if let Ok(text) = String::from_utf16(&units.collect::<Vec<_>>()) {
                return text;
            }
        }
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        if !text.contains('\0') {
            return text.to_string();
        }
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
    wide.truncate(written as usize);
    String::from_utf16_lossy(&wide)
}

#[cfg(target_os = "linux")]
fn unit_path() -> AppResult<PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| AppError::Message("无法确定 systemd user 配置目录".into()))?;
    Ok(config.join("systemd").join("user").join(unit_name()?))
}

#[cfg(target_os = "linux")]
fn platform_registered(_config: Option<&AdminServiceConfig>) -> AppResult<bool> {
    Ok(unit_path()?.is_file())
}

#[cfg(target_os = "linux")]
fn platform_enabled() -> AppResult<bool> {
    Ok(run_systemctl(&["is-enabled", &unit_name()?], true)?
        .status
        .success())
}

#[cfg(target_os = "linux")]
fn platform_install(config: &AdminServiceConfig) -> AppResult<()> {
    ensure_linux_linger()?;
    let path = unit_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Message("systemd user unit 路径无父目录".into()))?;
    fs::create_dir_all(parent)?;
    let unit = render_systemd_unit(config)?;
    fs::write(&path, unit)?;
    run_systemctl(&["daemon-reload"], false)?;
    run_systemctl(&["enable", &unit_name()?], false)?;
    run_systemctl(&["restart", &unit_name()?], false)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_uninstall() -> AppResult<()> {
    let name = unit_name()?;
    let _ = run_systemctl(&["stop", &name], true);
    let _ = run_systemctl(&["disable", &name], true);
    match fs::remove_file(unit_path()?) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    run_systemctl(&["daemon-reload"], false)?;
    let _ = run_systemctl(&["reset-failed", &name], true);
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_start() -> AppResult<()> {
    run_systemctl(&["start", &unit_name()?], false).map(|_| ())
}

#[cfg(target_os = "linux")]
fn platform_stop() -> AppResult<()> {
    run_systemctl(&["stop", &unit_name()?], false).map(|_| ())
}

#[cfg(target_os = "linux")]
fn platform_restart() -> AppResult<()> {
    run_systemctl(&["restart", &unit_name()?], false).map(|_| ())
}

#[cfg(target_os = "linux")]
fn platform_enable() -> AppResult<()> {
    run_systemctl(&["enable", &unit_name()?], false).map(|_| ())
}

#[cfg(target_os = "linux")]
fn platform_disable() -> AppResult<()> {
    run_systemctl(&["disable", &unit_name()?], false).map(|_| ())
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str], allow_nonzero: bool) -> AppResult<Output> {
    crate::platform::run_user_systemctl(args, allow_nonzero)
}

#[cfg(target_os = "linux")]
fn render_systemd_unit(config: &AdminServiceConfig) -> AppResult<String> {
    validate_service_config(config)?;
    Ok(format!(
        "[Unit]\nDescription=Anchor Web Admin ({scope})\nAfter=network.target\nStartLimitIntervalSec=60\nStartLimitBurst=3\n\n[Service]\nType=simple\nExecStart={exe} --config-dir {config_dir} admin daemon-run --port {port}\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec=15\nNoNewPrivileges=true\n\n[Install]\nWantedBy=default.target\n",
        scope = admin_daemon::config_scope()?,
        exe = systemd_quote(&config.executable_path)?,
        config_dir = systemd_quote(&config.config_dir)?,
        port = config.port,
    ))
}

#[cfg(target_os = "linux")]
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

#[cfg(windows)]
fn task_name() -> AppResult<String> {
    Ok(format!("Anchor Admin {}", admin_daemon::config_scope()?))
}

#[cfg(windows)]
fn platform_registered(_config: Option<&AdminServiceConfig>) -> AppResult<bool> {
    Ok(
        run_schtasks(&["/Query", "/TN", &task_name()?, "/XML"], true)?
            .status
            .success(),
    )
}

#[cfg(windows)]
fn platform_enabled() -> AppResult<bool> {
    let output = run_schtasks(&["/Query", "/TN", &task_name()?, "/XML"], false)?;
    Ok(decode_windows_command_output(&output.stdout).contains("<Enabled>true</Enabled>"))
}

#[cfg(windows)]
fn platform_install(config: &AdminServiceConfig) -> AppResult<()> {
    let user = current_windows_task_user()?;
    let xml = render_windows_task_xml(config, &user)?;
    let xml_path = PathBuf::from(&config.config_dir)
        .join("admin")
        .join(format!("task-{}.xml", std::process::id()));
    fs::write(&xml_path, encode_windows_task_xml(&xml))?;
    let xml_arg = xml_path.display().to_string();
    let create = run_schtasks(
        &["/Create", "/TN", &task_name()?, "/XML", &xml_arg, "/F"],
        false,
    );
    let _ = fs::remove_file(&xml_path);
    create?;
    run_schtasks(&["/Run", "/TN", &task_name()?], false)?;
    Ok(())
}

#[cfg(windows)]
fn platform_uninstall() -> AppResult<()> {
    let name = task_name()?;
    let _ = run_schtasks(&["/End", "/TN", &name], true);
    run_schtasks(&["/Delete", "/TN", &name, "/F"], true)?;
    Ok(())
}

#[cfg(windows)]
fn platform_start() -> AppResult<()> {
    run_schtasks(&["/Run", "/TN", &task_name()?], false).map(|_| ())
}

#[cfg(windows)]
fn platform_stop() -> AppResult<()> {
    run_schtasks(&["/End", "/TN", &task_name()?], false).map(|_| ())
}

#[cfg(windows)]
fn platform_restart() -> AppResult<()> {
    let name = task_name()?;
    let _ = run_schtasks(&["/End", "/TN", &name], true);
    run_schtasks(&["/Run", "/TN", &name], false).map(|_| ())
}

#[cfg(windows)]
fn platform_enable() -> AppResult<()> {
    run_schtasks(&["/Change", "/TN", &task_name()?, "/ENABLE"], false).map(|_| ())
}

#[cfg(windows)]
fn platform_disable() -> AppResult<()> {
    run_schtasks(&["/Change", "/TN", &task_name()?, "/DISABLE"], false).map(|_| ())
}

#[cfg(windows)]
fn run_schtasks(args: &[&str], allow_nonzero: bool) -> AppResult<Output> {
    let mut command = Command::new("schtasks.exe");
    command.args(args);
    crate::platform::hide_std_console(&mut command);
    let output = command
        .output()
        .map_err(|error| AppError::Message(format!("无法执行 schtasks.exe：{error}")))?;
    if !allow_nonzero && !output.status.success() {
        return Err(AppError::Message(format!(
            "schtasks {} 失败：{}",
            args.join(" "),
            decode_windows_command_output(&output.stderr).trim()
        )));
    }
    Ok(output)
}

#[cfg(windows)]
fn current_windows_task_user() -> AppResult<String> {
    let mut command = Command::new("whoami.exe");
    crate::platform::hide_std_console(&mut command);
    let output = command
        .output()
        .map_err(|error| AppError::Message(format!("无法获取当前 Windows 用户：{error}")))?;
    if !output.status.success() {
        return Err(AppError::Message(format!(
            "whoami.exe 执行失败：{}",
            decode_windows_command_output(&output.stderr).trim()
        )));
    }
    let user = decode_windows_command_output(&output.stdout)
        .trim()
        .to_string();
    if user.is_empty() || user.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n')) {
        return Err(AppError::Message("当前 Windows 用户标识无效".into()));
    }
    Ok(user)
}

#[cfg(any(windows, test))]
fn windows_quote(value: &str) -> AppResult<String> {
    if value.chars().any(|ch| matches!(ch, '\0' | '\n' | '\r')) {
        return Err(AppError::Message(
            "Windows Task 参数包含非法控制字符".into(),
        ));
    }
    let mut quoted = String::from("\"");
    let mut slashes = 0usize;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
        } else if ch == '"' {
            quoted.push_str(&"\\".repeat(slashes * 2 + 1));
            quoted.push('"');
            slashes = 0;
        } else {
            quoted.push_str(&"\\".repeat(slashes));
            slashes = 0;
            quoted.push(ch);
        }
    }
    quoted.push_str(&"\\".repeat(slashes * 2));
    quoted.push('"');
    Ok(quoted)
}

#[cfg(any(windows, test))]
fn render_windows_task_xml(config: &AdminServiceConfig, user: &str) -> AppResult<String> {
    if !Path::new(&config.executable_path).is_absolute()
        || !Path::new(&config.config_dir).is_absolute()
        || config.port == 0
        || user.is_empty()
        || user.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
    {
        return Err(AppError::Message("Windows Admin Task 注册参数无效".into()));
    }
    let arguments = [
        "--config-dir".to_string(),
        windows_quote(&config.config_dir)?,
        "admin".into(),
        "daemon-run".into(),
        "--port".into(),
        config.port.to_string(),
    ]
    .join(" ");
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
  <RegistrationInfo><Description>Anchor Web Admin ({scope})</Description></RegistrationInfo>\n\
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>\n\
  <Principals><Principal id=\"Author\"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n\
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><StartWhenAvailable>true</StartWhenAvailable><AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings>\n\
  <Actions Context=\"Author\"><Exec><Command>{exe}</Command><Arguments>{args}</Arguments></Exec></Actions>\n\
</Task>\n",
        scope = xml_escape(&admin_daemon::config_scope()?),
        user = xml_escape(user),
        exe = xml_escape(&config.executable_path),
        args = xml_escape(&arguments),
    ))
}

#[cfg(any(windows, test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_registered(_config: Option<&AdminServiceConfig>) -> AppResult<bool> {
    Ok(false)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_enabled() -> AppResult<bool> {
    Ok(false)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_install(_config: &AdminServiceConfig) -> AppResult<()> {
    ensure_supported()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_uninstall() -> AppResult<()> {
    ensure_supported()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_start() -> AppResult<()> {
    ensure_supported()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_stop() -> AppResult<()> {
    ensure_supported()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_restart() -> AppResult<()> {
    ensure_supported()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_enable() -> AppResult<()> {
    ensure_supported()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_disable() -> AppResult<()> {
    ensure_supported()
}

fn set_private_dir_permissions(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_unit_has_bounded_restart_and_exact_config_scope() {
        let config = AdminServiceConfig {
            schema_version: SERVICE_CONFIG_SCHEMA_VERSION,
            manager: manager_name().into(),
            port: 28_769,
            executable_path: "/opt/Anchor App/anchor".into(),
            config_dir: "/home/demo/.config/anchor".into(),
            installed_build: BuildIdentity::current(),
        };
        let unit = render_systemd_unit(&config).expect("unit");
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=5"));
        assert!(unit.contains("StartLimitBurst=3"));
        assert!(unit.contains("admin daemon-run --port 28769"));
        assert!(unit.contains("\"/opt/Anchor App/anchor\""));
    }

    #[test]
    fn windows_task_xml_has_bounded_restart_and_current_user_scope() {
        let config = AdminServiceConfig {
            schema_version: SERVICE_CONFIG_SCHEMA_VERSION,
            manager: "task-scheduler".into(),
            port: 28_769,
            executable_path: if cfg!(windows) {
                r"C:\Program Files\Anchor\anchor.exe".into()
            } else {
                "/opt/anchor.exe".into()
            },
            config_dir: if cfg!(windows) {
                r"C:\Users\demo\AppData\Roaming\Anchor".into()
            } else {
                "/tmp/anchor-config".into()
            },
            installed_build: BuildIdentity::current(),
        };
        let xml = render_windows_task_xml(&config, r"DESKTOP\demo").expect("task xml");
        assert!(xml.contains("<RestartOnFailure><Interval>PT1M</Interval><Count>3</Count>"));
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("admin daemon-run --port 28769"));
        assert!(xml.contains("DESKTOP\\demo"));
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>"));

        let encoded = encode_windows_task_xml(&xml);
        assert!(encoded.starts_with(&[0xFF, 0xFE]));
        let decoded = String::from_utf16(
            &encoded[2..]
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>(),
        )
        .expect("utf-16 task xml");
        assert_eq!(decoded, xml);
    }
}
