#![cfg(target_os = "windows")]

use std::collections::{BTreeMap, HashSet};
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::build_identity::BuildIdentity;
use crate::control::{self, DaemonLaunchSpec};
use crate::daemon::ServiceSelection;
use crate::error::{AppError, AppResult};
use crate::gateway_control::{self, GatewayOperation};
use crate::gateway_daemon;
use crate::platform::platform;

const PLAN_SCHEMA_VERSION: u32 = 1;
const SERVICE_NAME_PREFIX: &str = "AnchorControlPlane";
const SERVICE_DISPLAY_NAME_PREFIX: &str = "Anchor Control Plane";
const SERVICE_PLAN_FILE: &str = "windows-service.json";
const SERVICE_PLAN_LOCK_FILE: &str = ".windows-service.lock";
const SERVICE_RUNTIME_FILE: &str = "windows-service-runtime.json";
const SERVICE_RUNTIME_SCHEMA_VERSION: u32 = 1;
const PIPE_USER_ENV: &str = "ANCHOR_PIPE_USER";
const SERVICE_OWNER_SID_ENV: &str = "ANCHOR_WINDOWS_SERVICE_OWNER_SID";
const SERVICE_OWNER_USERNAME_ENV: &str = "ANCHOR_WINDOWS_SERVICE_OWNER_USERNAME";
const SERVICE_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
const SERVICE_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);

const SERVICE_WIN32_OWN_PROCESS: u32 = 0x0000_0010;
const SERVICE_STOPPED: u32 = 0x0000_0001;
const SERVICE_START_PENDING: u32 = 0x0000_0002;
const SERVICE_STOP_PENDING: u32 = 0x0000_0003;
const SERVICE_RUNNING: u32 = 0x0000_0004;
const SERVICE_ACCEPT_STOP: u32 = 0x0000_0001;
const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x0000_0004;
const SERVICE_CONTROL_STOP: u32 = 0x0000_0001;
const SERVICE_CONTROL_SHUTDOWN: u32 = 0x0000_0005;
const NO_ERROR: u32 = 0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsWorkspaceAutostart {
    pub workspace_id: String,
    pub service: ServiceSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_services: Option<ServiceSelection>,
}

fn parse_scm_binary_executable(output: &str) -> Option<String> {
    let regex = regex::Regex::new(r"(?mi)^\s*BINARY_PATH_NAME\s*:\s*(.+?)\s*$")
        .expect("SCM binary path regex");
    let command_line = regex.captures(output)?.get(1)?.as_str().trim();
    if let Some(quoted) = command_line.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some(quoted[..end].to_string());
    }
    command_line
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn validate_service_owner(owner_sid: &str, owner_username: &str) -> AppResult<()> {
    let owner_sid = owner_sid.trim();
    if !valid_sid(owner_sid) {
        return Err(AppError::Message(
            "Windows Service owner SID 无效；请从配置 owner 的 Windows 会话重试".into(),
        ));
    }
    if matches!(owner_sid, "S-1-5-18" | "S-1-5-19" | "S-1-5-20") {
        return Err(AppError::Message(
            "Windows Service owner 不能是 LocalSystem/LocalService/NetworkService；请从实际桌面用户会话安装或更新 Service"
                .into(),
        ));
    }
    let owner_username = owner_username.trim();
    if owner_username.is_empty()
        || owner_username
            .chars()
            .any(|ch| ch == '\0' || ch == '\r' || ch == '\n')
    {
        return Err(AppError::Message(
            "Windows Service owner username 无效；请从配置 owner 的 Windows 会话重试".into(),
        ));
    }
    Ok(())
}

fn trusted_service_owner_sid() -> AppResult<String> {
    let sid = std::env::var(SERVICE_OWNER_SID_ENV).map_err(|_| {
        AppError::Message(
            "Windows Service registration 缺少可信 owner SID；请从配置 owner 会话重新执行 service install/update"
                .into(),
        )
    })?;
    let sid = sid.trim().to_string();
    if !valid_sid(&sid) {
        return Err(AppError::Message(
            "Windows Service registration 中的 owner SID 无效；请重新执行 service install/update"
                .into(),
        ));
    }
    Ok(sid)
}

fn trusted_service_owner_username() -> AppResult<String> {
    let username = std::env::var(SERVICE_OWNER_USERNAME_ENV).map_err(|_| {
        AppError::Message(
            "Windows Service registration 缺少可信 owner username；请从配置 owner 会话重新执行 service install/update"
                .into(),
        )
    })?;
    let username = username.trim().to_string();
    if username.is_empty()
        || username
            .chars()
            .any(|ch| ch == '\0' || ch == '\r' || ch == '\n')
    {
        return Err(AppError::Message(
            "Windows Service registration 中的 owner username 无效；请重新执行 service install/update"
                .into(),
        ));
    }
    Ok(username)
}

fn service_build_state(
    installed: bool,
    state: &str,
    runtime: Option<&WindowsServiceRuntimeState>,
    current_build: &BuildIdentity,
) -> &'static str {
    if !installed {
        "not_installed"
    } else if state != "running" {
        "stopped"
    } else if let Some(runtime) = runtime {
        if runtime.build_identity.same_build(current_build) {
            "current"
        } else {
            "different"
        }
    } else {
        "unknown"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsServicePlan {
    pub schema_version: u32,
    #[serde(default)]
    pub owner_sid: String,
    #[serde(default)]
    pub owner_username: String,
    #[serde(default)]
    pub workspaces: Vec<WindowsWorkspaceAutostart>,
    #[serde(default)]
    pub gateway_workspace_ids: Vec<String>,
}

impl Default for WindowsServicePlan {
    fn default() -> Self {
        Self {
            schema_version: PLAN_SCHEMA_VERSION,
            owner_sid: String::new(),
            owner_username: String::new(),
            workspaces: Vec::new(),
            gateway_workspace_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsScmServiceStatus {
    pub supported: bool,
    pub service_name: String,
    pub installed: bool,
    pub state: String,
    pub auto_start: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    pub config_dir: String,
    pub plan_path: String,
    pub plan: WindowsServicePlan,
    pub build_state: String,
    pub current_build: BuildIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<WindowsServiceRuntimeState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsServiceRuntimeState {
    pub schema_version: u32,
    pub pid: u32,
    pub started_at_unix: u64,
    pub executable_path: String,
    pub build_identity: BuildIdentity,
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

#[derive(Debug)]
struct ScOutput {
    success: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[repr(C)]
struct RawServiceStatus {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
    service_specific_exit_code: u32,
    checkpoint: u32,
    wait_hint: u32,
}

type ServiceMain = unsafe extern "system" fn(u32, *mut *mut u16);
type HandlerEx = unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32;
type ServiceStatusHandle = *mut c_void;

#[repr(C)]
struct RawServiceTableEntry {
    service_name: *mut u16,
    service_proc: Option<ServiceMain>,
}

#[link(name = "Advapi32")]
extern "system" {
    fn StartServiceCtrlDispatcherW(service_start_table: *const RawServiceTableEntry) -> i32;
    fn RegisterServiceCtrlHandlerExW(
        service_name: *const u16,
        handler_proc: Option<HandlerEx>,
        context: *mut c_void,
    ) -> ServiceStatusHandle;
    fn SetServiceStatus(
        service_status_handle: ServiceStatusHandle,
        service_status: *const RawServiceStatus,
    ) -> i32;
}

static SERVICE_CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();
static SERVICE_NAME_WIDE: OnceLock<Vec<u16>> = OnceLock::new();
static SERVICE_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static SERVICE_STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);

pub fn service_name() -> AppResult<String> {
    Ok(service_name_for_dir(&platform().app_config_dir()?))
}

pub fn service_name_for_dir(config_dir: &Path) -> String {
    let normalized = config_dir
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    let scope = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{SERVICE_NAME_PREFIX}-{scope}")
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

pub fn load_plan() -> AppResult<WindowsServicePlan> {
    let path = plan_path()?;
    let _guard = acquire_plan_lock()?;
    read_plan_unlocked(&path)
}

fn read_plan_unlocked(path: &Path) -> AppResult<WindowsServicePlan> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            let mut plan: WindowsServicePlan = serde_json::from_str(&raw).map_err(|error| {
                AppError::Message(format!("Windows service plan 损坏：{error}"))
            })?;
            if plan.schema_version != PLAN_SCHEMA_VERSION {
                return Err(AppError::Message(format!(
                    "Windows service plan schema={}，当前仅支持 {}",
                    plan.schema_version, PLAN_SCHEMA_VERSION
                )));
            }
            normalize_plan(&mut plan);
            Ok(plan)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(WindowsServicePlan::default())
        }
        Err(error) => Err(error.into()),
    }
}

pub fn save_plan(plan: &WindowsServicePlan) -> AppResult<()> {
    let path = plan_path()?;
    let _guard = acquire_plan_lock()?;
    let mut normalized = plan.clone();
    normalize_plan(&mut normalized);
    write_plan_unlocked(&path, &normalized)
}

fn mutate_plan(
    update: impl FnOnce(&mut WindowsServicePlan) -> AppResult<()>,
) -> AppResult<WindowsServicePlan> {
    let path = plan_path()?;
    let _guard = acquire_plan_lock()?;
    let mut plan = read_plan_unlocked(&path)?;
    update(&mut plan)?;
    ensure_owner_sid(&mut plan)?;
    normalize_plan(&mut plan);
    write_plan_unlocked(&path, &plan)?;
    Ok(plan)
}

fn write_plan_unlocked(path: &Path, plan: &WindowsServicePlan) -> AppResult<()> {
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
    replace_runtime_file(&temp, path)?;
    Ok(())
}

fn service_runtime_path() -> AppResult<PathBuf> {
    Ok(platform().app_config_dir()?.join(SERVICE_RUNTIME_FILE))
}

fn read_service_runtime_state() -> AppResult<Option<WindowsServiceRuntimeState>> {
    let path = service_runtime_path()?;
    match fs::read_to_string(path) {
        Ok(raw) => {
            let state: WindowsServiceRuntimeState =
                serde_json::from_str(&raw).map_err(|error| {
                    AppError::Message(format!("Windows service runtime state 损坏：{error}"))
                })?;
            if state.schema_version != SERVICE_RUNTIME_SCHEMA_VERSION {
                return Err(AppError::Message(format!(
                    "Windows service runtime schema={}，当前仅支持 {}",
                    state.schema_version, SERVICE_RUNTIME_SCHEMA_VERSION
                )));
            }
            Ok(Some(state))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_service_runtime_state(state: &WindowsServiceRuntimeState) -> AppResult<()> {
    let path = service_runtime_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", state.pid));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)?;
    file.write_all(serde_json::to_string_pretty(state)?.as_bytes())?;
    file.sync_all()?;
    replace_runtime_file(&temp, &path)?;
    Ok(())
}

fn replace_runtime_file(source: &Path, destination: &Path) -> AppResult<()> {
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
                AppError::Message(format!("Windows service state replacement failed: {error}"))
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

fn remove_service_runtime_state_if_owned(pid: u32) {
    let Ok(path) = service_runtime_path() else {
        return;
    };
    let owned = read_service_runtime_state()
        .ok()
        .flatten()
        .is_some_and(|state| state.pid == pid);
    if owned {
        let _ = fs::remove_file(path);
    }
}

fn validate_service_runtime_state(
    state: WindowsServiceRuntimeState,
    scm_pid: Option<u32>,
    registered_executable: Option<&str>,
) -> Result<WindowsServiceRuntimeState, String> {
    let scm_pid = scm_pid.ok_or_else(|| "scm_pid_missing".to_string())?;
    if scm_pid != state.pid {
        return Err(format!("pid_mismatch:scm={scm_pid},runtime={}", state.pid));
    }
    // The caller already obtained `RUNNING` and this PID from SCM queryex.
    // A non-elevated config owner may be denied OpenProcess against the
    // LocalSystem service, so `is_process_alive`/process_image_path are not
    // authoritative here. Prefer the SCM registration (`sc qc`), which is
    // readable to the owner and is the same source SCM uses to launch the
    // process. Fall back to process inspection only when registration data is
    // unexpectedly unavailable.
    let expected = match registered_executable {
        Some(executable) => executable.to_string(),
        None => {
            if !platform().is_process_alive(state.pid) {
                return Err(format!("runtime_pid_not_alive:{}", state.pid));
            }
            platform()
                .process_image_path(state.pid)
                .ok()
                .flatten()
                .ok_or_else(|| "registered_executable_unavailable".to_string())?
        }
    };
    if normalize_windows_path(&expected) != normalize_windows_path(&state.executable_path) {
        return Err(format!(
            "executable_mismatch:registered={expected},runtime={}",
            state.executable_path
        ));
    }
    Ok(state)
}

fn normalize_windows_path(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
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

fn normalize_plan(plan: &mut WindowsServicePlan) {
    plan.schema_version = PLAN_SCHEMA_VERSION;
    plan.owner_sid = plan.owner_sid.trim().to_string();
    plan.owner_username = plan.owner_username.trim().to_string();
    let mut workspaces = BTreeMap::<String, WindowsWorkspaceAutostart>::new();
    for mut entry in plan.workspaces.drain(..) {
        entry.workspace_id = entry.workspace_id.trim().to_string();
        if !entry.workspace_id.is_empty() {
            workspaces.insert(entry.workspace_id.clone(), entry);
        }
    }
    plan.workspaces = workspaces.into_values().collect();
    plan.gateway_workspace_ids = normalized_ids(&plan.gateway_workspace_ids);
}

fn ensure_owner_sid(plan: &mut WindowsServicePlan) -> AppResult<()> {
    if valid_sid(&plan.owner_sid) {
        ensure_owner_username(plan);
        return Ok(());
    }
    plan.owner_sid = current_user_sid()?;
    ensure_owner_username(plan);
    Ok(())
}

fn ensure_owner_username(plan: &mut WindowsServicePlan) {
    if !plan.owner_username.trim().is_empty() {
        return;
    }
    plan.owner_username = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown-user".into());
}

pub fn pipe_identity_user() -> String {
    if let Ok(value) = std::env::var(PIPE_USER_ENV) {
        if !value.trim().is_empty() {
            return value.trim().to_string();
        }
    }
    if !in_service_context() {
        if let Ok(value) = std::env::var("USERNAME").or_else(|_| std::env::var("USER")) {
            if !value.trim().is_empty() {
                return value.trim().to_string();
            }
        }
    }
    if let Ok(plan) = load_plan() {
        if !plan.owner_username.trim().is_empty() {
            return plan.owner_username;
        }
    }
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown-user".into())
}

fn valid_sid(value: &str) -> bool {
    value.starts_with("S-1-")
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn current_user_sid() -> AppResult<String> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(|error| {
            AppError::Message(format!("无法读取当前 Windows 进程令牌：{error}"))
        })?;
    }
    let token = OwnedWindowsHandle(token);
    token_user_sid(token.raw())
}

fn current_user_name() -> AppResult<String> {
    let username = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .map_err(|_| AppError::Message("无法读取当前 Windows 用户名".into()))?;
    let username = username.trim().to_string();
    if username.is_empty()
        || username
            .chars()
            .any(|ch| ch == '\0' || ch == '\r' || ch == '\n')
    {
        return Err(AppError::Message("当前 Windows 用户名无效".into()));
    }
    Ok(username)
}

pub fn control_pipe_security_sddl() -> String {
    let owner_sid = if in_service_context() {
        trusted_service_owner_sid().ok()
    } else {
        current_user_sid().ok().or_else(|| {
            load_plan()
                .ok()
                .map(|plan| plan.owner_sid)
                .filter(|sid| valid_sid(sid))
        })
    };
    match owner_sid {
        Some(sid) => format!("D:P(A;;GA;;;SY)(A;;GA;;;OW)(A;;GA;;;{sid})"),
        None => "D:P(A;;GA;;;SY)(A;;GA;;;OW)".into(),
    }
}

pub fn in_service_context() -> bool {
    std::env::var_os(crate::brand::WINDOWS_SERVICE_CONTEXT_ENV)
        .is_some_and(|value| !value.is_empty() && value != "0")
}

struct OwnedWindowsHandle(windows::Win32::Foundation::HANDLE);

impl OwnedWindowsHandle {
    fn raw(&self) -> windows::Win32::Foundation::HANDLE {
        self.0
    }
}

impl Drop for OwnedWindowsHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

struct OwnedEnvironmentBlock(*mut c_void);

impl Drop for OwnedEnvironmentBlock {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = windows::Win32::System::Environment::DestroyEnvironmentBlock(self.0);
            }
        }
    }
}

fn token_user_sid(token: windows::Win32::Foundation::HANDLE) -> AppResult<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_USER};

    let mut required = 0_u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut required) };
    if required == 0 {
        return Err(AppError::Message(format!(
            "无法读取 Windows 登录令牌 SID 大小：{}",
            std::io::Error::last_os_error()
        )));
    }
    let word = std::mem::size_of::<usize>();
    let words = (required as usize).div_ceil(word);
    let mut buffer = vec![0_usize; words];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            required,
            &mut required,
        )
        .map_err(|error| AppError::Message(format!("无法读取 Windows 登录令牌 SID：{error}")))?;
    }
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = PWSTR::null();
    unsafe {
        ConvertSidToStringSidW(token_user.User.Sid, &mut text).map_err(|error| {
            AppError::Message(format!("无法格式化 Windows 登录令牌 SID：{error}"))
        })?;
    }
    let sid = unsafe {
        let mut len = 0_usize;
        while *text.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text.0, len))
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    }
    Ok(sid)
}

fn process_user_sid(pid: u32) -> AppResult<String> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{
        OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let process = OwnedWindowsHandle(unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(|error| {
            AppError::Message(format!("无法打开 PID {pid} 查询进程 owner：{error}"))
        })?
    });
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(process.raw(), TOKEN_QUERY, &mut token).map_err(|error| {
            AppError::Message(format!("无法读取 PID {pid} 的 Windows 进程令牌：{error}"))
        })?;
    }
    let token = OwnedWindowsHandle(token);
    token_user_sid(token.raw())
}

fn process_is_config_owner(pid: u32) -> AppResult<bool> {
    Ok(process_user_sid(pid)?.eq_ignore_ascii_case(&trusted_service_owner_sid()?))
}

fn managed_frpc_image_candidates() -> AppResult<Vec<PathBuf>> {
    let mut candidates = vec![platform().app_config_dir()?.join("bin").join("frpc.exe")];
    if let Some(parent) = std::env::current_exe()?.parent() {
        candidates.push(parent.join("frpc.exe"));
    }
    // Also include the exact candidates visible to the LocalSystem supervisor.
    // Legacy service-owned daemons resolved frpc in this same account context,
    // so PATH / Program Files candidates are required to reclaim those children.
    candidates.extend(platform().frpc_candidates());
    candidates.sort_by_key(|path| normalize_windows_path(&path.display().to_string()));
    candidates.dedup_by(|left, right| {
        normalize_windows_path(&left.display().to_string())
            == normalize_windows_path(&right.display().to_string())
    });
    Ok(candidates)
}

fn cleanup_wrong_owner_managed_frpc_processes() -> AppResult<usize> {
    let expected_sid = trusted_service_owner_sid()?;
    let mut terminated = 0_usize;
    for image in managed_frpc_image_candidates()? {
        for pid in platform().process_ids_by_image_path(&image)? {
            let actual_sid = process_user_sid(pid)?;
            if actual_sid.eq_ignore_ascii_case(&expected_sid) {
                continue;
            }
            append_service_log(&format!(
                "[service] legacy frpc PID {pid} owner mismatch: expected={expected_sid}, actual={actual_sid}; terminating {}",
                image.display()
            ));
            platform().terminate_process_tree(pid)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while platform().is_process_alive(pid) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if platform().is_process_alive(pid) {
                return Err(AppError::Message(format!(
                    "legacy frpc PID {pid} owner 不匹配且无法终止"
                )));
            }
            terminated = terminated.saturating_add(1);
        }
    }
    Ok(terminated)
}

fn active_owner_token() -> AppResult<OwnedWindowsHandle> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::RemoteDesktop::{
        WTSActive, WTSEnumerateSessionsW, WTSFreeMemory, WTSQueryUserToken,
        WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW,
    };

    let owner_sid = trusted_service_owner_sid()?;
    let owner_username = trusted_service_owner_username()?;

    let mut sessions = std::ptr::null_mut::<WTS_SESSION_INFOW>();
    let mut count = 0_u32;
    unsafe {
        WTSEnumerateSessionsW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            0,
            1,
            &mut sessions,
            &mut count,
        )
        .map_err(|error| AppError::Message(format!("无法枚举 Windows 登录会话：{error}")))?;
    }
    struct SessionBuffer(*mut WTS_SESSION_INFOW);
    impl Drop for SessionBuffer {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { WTSFreeMemory(self.0.cast()) };
            }
        }
    }
    let _sessions_guard = SessionBuffer(sessions);
    let sessions = if sessions.is_null() || count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(sessions, count as usize) }
    };

    for session in sessions.iter().filter(|session| session.State == WTSActive) {
        let mut token = HANDLE::default();
        if unsafe { WTSQueryUserToken(session.SessionId, &mut token) }.is_err() {
            continue;
        }
        let token = OwnedWindowsHandle(token);
        match token_user_sid(token.raw()) {
            Ok(sid) if sid.eq_ignore_ascii_case(&owner_sid) => return Ok(token),
            Ok(_) | Err(_) => {}
        }
    }

    Err(AppError::Message(format!(
        "配置 owner {} ({}) 当前没有可用的 Active Windows 登录会话；Service 将等待用户登录后再启动 Workspace/Gateway daemon",
        owner_username, owner_sid
    )))
}

fn quote_windows_arg(value: &str) -> String {
    if !value.is_empty() && !value.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        return value.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0_usize;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            quoted.push(ch);
        }
        backslashes = 0;
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

fn owner_child_command_line(executable: &Path, args: &[String]) -> String {
    let mut command_line = quote_windows_arg(&executable.display().to_string());
    for arg in args {
        command_line.push(' ');
        command_line.push_str(&quote_windows_arg(arg));
    }
    command_line
}

fn spawn_as_config_owner(
    executable: &Path,
    args: Vec<String>,
    current_dir: &Path,
    label: &str,
) -> AppResult<u32> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Environment::CreateEnvironmentBlock;
    use windows::Win32::System::Threading::{
        CreateProcessAsUserW, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
        STARTUPINFOW,
    };

    let owner_username = trusted_service_owner_username()?;
    let token = active_owner_token()?;
    let mut environment = std::ptr::null_mut::<c_void>();
    unsafe {
        CreateEnvironmentBlock(&mut environment, Some(token.raw()), false).map_err(|error| {
            AppError::Message(format!(
                "无法为配置 owner {} 创建 Windows 用户环境：{error}",
                owner_username
            ))
        })?;
    }
    let _environment_guard = OwnedEnvironmentBlock(environment);

    let application = wide_null(&executable.display().to_string());
    let mut command_line = wide_null(&owner_child_command_line(&executable, &args));
    let current_dir = wide_null(&current_dir.display().to_string());
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessAsUserW(
            Some(token.raw()),
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            Some(environment as *const c_void),
            PCWSTR(current_dir.as_ptr()),
            &startup,
            &mut process,
        )
        .map_err(|error| {
            AppError::Message(format!(
                "以配置 owner {} 启动 {label} 失败：{error}",
                owner_username
            ))
        })?;
        let _ = windows::Win32::Foundation::CloseHandle(process.hThread);
        let _ = windows::Win32::Foundation::CloseHandle(process.hProcess);
    }
    Ok(process.dwProcessId)
}

pub(crate) fn spawn_workspace_daemon_as_owner_from_executable(
    executable: &Path,
    profile: &crate::workspace::WorkspaceProfile,
    service: ServiceSelection,
    tunnel_services: Option<ServiceSelection>,
) -> AppResult<u32> {
    let config_dir = platform().app_config_dir()?;
    let mut args = vec![
        "--config-dir".to_string(),
        config_dir.display().to_string(),
        "daemon-run".to_string(),
        profile.id.clone(),
        "--service".to_string(),
        service.as_str().to_string(),
    ];
    if let Some(tunnels) = tunnel_services {
        args.push("--tunnel-service".to_string());
        args.push(tunnels.as_str().to_string());
    } else {
        args.push("--no-tunnel".to_string());
    }
    spawn_as_config_owner(
        executable,
        args,
        Path::new(&profile.path),
        "Workspace daemon",
    )
}

pub(crate) fn spawn_gateway_daemon_as_owner_from_executable(
    executable: &Path,
    workspace_ids: &[String],
) -> AppResult<u32> {
    let config_dir = platform().app_config_dir()?;
    let mut args = vec![
        "--config-dir".to_string(),
        config_dir.display().to_string(),
        "gateway-daemon-run".to_string(),
        gateway_daemon::config_scope()?,
    ];
    args.extend(normalized_ids(workspace_ids));
    spawn_as_config_owner(executable, args, &config_dir, "Gateway daemon")
}

pub fn set_workspace_desired(
    workspace_id: &str,
    desired: Option<DaemonLaunchSpec>,
) -> AppResult<WindowsServicePlan> {
    let workspace_id = workspace_id.trim().to_string();
    if workspace_id.is_empty() {
        return Err(AppError::Message("workspace id 不能为空".into()));
    }
    mutate_plan(|plan| {
        plan.workspaces
            .retain(|entry| entry.workspace_id != workspace_id);
        if let Some(spec) = desired {
            plan.workspaces.push(WindowsWorkspaceAutostart {
                workspace_id: workspace_id.clone(),
                service: spec.service,
                tunnel_services: spec.tunnels,
            });
        }
        Ok(())
    })
}

pub fn forget_workspace(workspace_id: &str) -> AppResult<WindowsServicePlan> {
    let workspace_id = workspace_id.trim().to_string();
    if workspace_id.is_empty() {
        return Err(AppError::Message("workspace id 不能为空".into()));
    }
    mutate_plan(|plan| {
        plan.workspaces
            .retain(|entry| entry.workspace_id != workspace_id);
        plan.gateway_workspace_ids.retain(|id| id != &workspace_id);
        Ok(())
    })
}

pub fn set_gateway_desired(workspace_ids: &[String]) -> AppResult<WindowsServicePlan> {
    let ids = normalized_ids(workspace_ids);
    mutate_plan(|plan| {
        plan.gateway_workspace_ids = ids;
        Ok(())
    })
}

pub fn sync_plan_from_running() -> AppResult<WindowsServicePlan> {
    let store = crate::data::DataStore::load_profiles_only()?;
    let profiles = store.list().to_vec();
    drop(store);
    let mut workspaces = Vec::new();
    for profile in &profiles {
        let inspection = crate::daemon::inspect(profile)?;
        if let Some(state) = inspection
            .state
            .filter(|_| inspection.running && inspection.pid_matches)
        {
            workspaces.push(WindowsWorkspaceAutostart {
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
    mutate_plan(|plan| {
        plan.workspaces = workspaces;
        plan.gateway_workspace_ids = gateway_workspace_ids;
        Ok(())
    })
}

pub fn scm_status() -> AppResult<WindowsScmServiceStatus> {
    let config_dir = platform().app_config_dir()?;
    let service_name = service_name_for_dir(&config_dir);
    let query = run_sc(&["queryex", &service_name])?;
    let (installed, state) = if query.success {
        (true, parse_scm_state(&query.stdout).to_string())
    } else if contains_sc_error(&query, 1060) {
        (false, "not_installed".to_string())
    } else {
        return Err(sc_error("查询 Windows SCM service", &query));
    };
    let service_config = if installed {
        let qc = run_sc(&["qc", &service_name])?;
        if qc.success {
            Some(qc.stdout)
        } else {
            None
        }
    } else {
        None
    };
    let auto_start = service_config.as_deref().is_some_and(parse_scm_auto_start);
    let registered_executable = service_config
        .as_deref()
        .and_then(parse_scm_binary_executable);
    let process_id = installed.then(|| parse_scm_pid(&query.stdout)).flatten();
    let (runtime, runtime_issue) = if state == "running" {
        match read_service_runtime_state() {
            Ok(Some(runtime)) => match validate_service_runtime_state(
                runtime,
                process_id,
                registered_executable.as_deref(),
            ) {
                Ok(runtime) => (Some(runtime), None),
                Err(issue) => (None, Some(issue)),
            },
            Ok(None) => (None, Some("runtime_state_missing".into())),
            Err(error) => (None, Some(format!("runtime_state_read_failed:{error}"))),
        }
    } else {
        (None, None)
    };
    let current_build = BuildIdentity::current();
    let build_state = service_build_state(installed, &state, runtime.as_ref(), &current_build);
    Ok(WindowsScmServiceStatus {
        supported: true,
        service_name,
        installed,
        state,
        auto_start,
        process_id,
        config_dir: config_dir.display().to_string(),
        plan_path: plan_path()?.display().to_string(),
        plan: load_plan()?,
        build_state: build_state.into(),
        current_build,
        runtime,
        runtime_issue,
    })
}

pub fn install_scm_service() -> AppResult<WindowsScmServiceStatus> {
    let owner_sid = current_user_sid()?;
    let owner_username = current_user_name()?;
    install_scm_service_for_owner(&owner_sid, &owner_username)
}

fn install_scm_service_for_owner(
    owner_sid: &str,
    owner_username: &str,
) -> AppResult<WindowsScmServiceStatus> {
    validate_service_owner(owner_sid, owner_username)?;
    let existing = load_plan()?;
    if existing.workspaces.is_empty() && existing.gateway_workspace_ids.is_empty() {
        let _ = sync_plan_from_running()?;
    }
    let owner_sid = owner_sid.trim().to_string();
    let owner_username = owner_username.trim().to_string();
    let _ = mutate_plan(|plan| {
        plan.owner_sid = owner_sid.clone();
        plan.owner_username = owner_username.clone();
        Ok(())
    })?;
    let config_dir = platform().app_config_dir()?;
    let service_name = service_name_for_dir(&config_dir);
    let service_scope = service_name
        .strip_prefix(&format!("{SERVICE_NAME_PREFIX}-"))
        .unwrap_or(service_name.as_str());
    let display_name = format!("{SERVICE_DISPLAY_NAME_PREFIX} ({service_scope})");
    let executable = std::env::current_exe()?;
    let binary_path = service_binary_path(&executable, &config_dir, &owner_sid, &owner_username);
    let status = scm_status()?;
    let operation = if status.installed { "config" } else { "create" };
    let mut args = vec![
        operation,
        service_name.as_str(),
        "binPath=",
        binary_path.as_str(),
    ];
    if !status.installed {
        args.extend(["start=", "auto", "DisplayName=", display_name.as_str()]);
    } else {
        args.extend(["start=", "auto"]);
    }
    let changed = run_sc(&args)?;
    if !changed.success {
        return Err(sc_error("安装/更新 Windows SCM service", &changed));
    }
    let description = format!(
        "Anchor per-user control plane supervisor for {}",
        config_dir.display()
    );
    let _ = run_sc(&["description", &service_name, &description]);
    let _ = run_sc(&[
        "failure",
        &service_name,
        "reset=",
        "86400",
        "actions=",
        "restart/5000/restart/15000/restart/60000",
    ]);
    if status.installed && status.state != "stopped" {
        stop_scm_service()?;
    }
    start_scm_service()
}

pub fn uninstall_scm_service() -> AppResult<WindowsScmServiceStatus> {
    let service_name = service_name()?;
    let status = scm_status()?;
    if !status.installed {
        return Ok(status);
    }
    if status.state != "stopped" {
        stop_scm_service()?;
    }
    let deleted = run_sc(&["delete", &service_name])?;
    if !deleted.success && !contains_sc_error(&deleted, 1060) {
        return Err(sc_error("卸载 Windows SCM service", &deleted));
    }
    let mut result = scm_status()?;
    result.installed = false;
    result.state = "not_installed".into();
    result.auto_start = false;
    Ok(result)
}

pub fn start_scm_service() -> AppResult<WindowsScmServiceStatus> {
    let service_name = service_name()?;
    let started = run_sc(&["start", &service_name])?;
    if !started.success && !contains_sc_error(&started, 1056) {
        return Err(sc_error("启动 Windows SCM service", &started));
    }
    wait_for_scm_state(&service_name, "running", SERVICE_OPERATION_TIMEOUT)?;
    scm_status()
}

pub fn stop_scm_service() -> AppResult<WindowsScmServiceStatus> {
    let service_name = service_name()?;
    let stopped = run_sc(&["stop", &service_name])?;
    if !stopped.success && !contains_sc_error(&stopped, 1062) {
        return Err(sc_error("停止 Windows SCM service", &stopped));
    }
    wait_for_scm_state(&service_name, "stopped", SERVICE_OPERATION_TIMEOUT)?;
    scm_status()
}

pub fn restart_scm_service() -> AppResult<WindowsScmServiceStatus> {
    stop_scm_service()?;
    start_scm_service()
}

pub fn run_admin_action(
    action: &str,
    config_dir: PathBuf,
    owner_sid: &str,
    owner_username: &str,
) -> AppResult<()> {
    if !config_dir.is_absolute() {
        return Err(AppError::Message(
            "service-admin-run config dir 必须是绝对路径".into(),
        ));
    }
    std::env::set_var(crate::brand::CONFIG_DIR_ENV, &config_dir);
    let result = match action {
        "install" => install_scm_service_for_owner(owner_sid, owner_username).map(|_| ()),
        "uninstall" => uninstall_scm_service().map(|_| ()),
        "start" => start_scm_service().map(|_| ()),
        "stop" => stop_scm_service().map(|_| ()),
        "restart" => restart_scm_service().map(|_| ()),
        other => Err(AppError::Message(format!(
            "未知 Windows Service 管理操作：{other}"
        ))),
    };
    match &result {
        Ok(()) => append_service_log(&format!("[service-admin] action={action} succeeded")),
        Err(error) => {
            append_service_log(&format!("[service-admin] action={action} failed: {error}"))
        }
    }
    result
}

pub fn run_elevated_admin_action(action: &str) -> AppResult<()> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

    if !matches!(
        action,
        "install" | "uninstall" | "start" | "stop" | "restart"
    ) {
        return Err(AppError::Message(format!(
            "未知 Windows Service 管理操作：{action}"
        )));
    }
    if matches!(action, "install" | "start" | "restart") {
        // Provision the LocalMachine DPAPI mirror before UAC. The user may
        // choose a different administrator account at the prompt, but this
        // process still owns the CurrentUser DPAPI key needed to decrypt the
        // canonical user payload.
        let _ = crate::data::DataStore::load()?;
    }
    // Persist the unelevated caller identity before showing UAC. Windows may
    // allow the user to enter another administrator account; the service must
    // still grant its control pipes to the owner of this config domain rather
    // than to the temporary elevation account.
    let owner_sid = current_user_sid()?;
    let owner_username = current_user_name()?;
    mutate_plan(|plan| {
        plan.owner_sid = owner_sid.clone();
        plan.owner_username = owner_username.clone();
        Ok(())
    })?;
    let executable = std::env::current_exe()?;
    let config_dir = platform().app_config_dir()?;
    let verb = wide_null("runas");
    let file = wide_null(&executable.display().to_string());
    let parameters = wide_null(&format!(
        "service-admin-run {} {} {} {}",
        quote_windows_arg(action),
        quote_windows_arg(&config_dir.display().to_string()),
        quote_windows_arg(&owner_sid),
        quote_windows_arg(&owner_username)
    ));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|error| {
            AppError::Message(format!(
                "Windows UAC 提权启动失败：{error}；如果取消了 UAC 提示，Service 配置不会改变"
            ))
        })?;
    }
    if info.hProcess.is_invalid() {
        return Err(AppError::Message(
            "Windows UAC helper 未返回可等待的进程句柄".into(),
        ));
    }
    unsafe {
        let _ = WaitForSingleObject(info.hProcess, INFINITE);
    }
    let mut exit_code = u32::MAX;
    let read_exit = unsafe { GetExitCodeProcess(info.hProcess, &mut exit_code) };
    unsafe {
        let _ = CloseHandle(info.hProcess);
    }
    read_exit.map_err(|error| {
        AppError::Message(format!("无法读取 Windows UAC helper 退出码：{error}"))
    })?;
    if exit_code != 0 {
        return Err(AppError::Message(format!(
            "Windows Service 管理 helper 执行失败（exit={exit_code}）；请查看 Anchor/Windows Service 日志"
        )));
    }
    Ok(())
}

fn wide_null(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn service_binary_path(
    executable: &Path,
    config_dir: &Path,
    owner_sid: &str,
    owner_username: &str,
) -> String {
    [
        quote_windows_arg(&executable.display().to_string()),
        "service-run".to_string(),
        quote_windows_arg(&config_dir.display().to_string()),
        quote_windows_arg(owner_sid),
        quote_windows_arg(owner_username),
    ]
    .join(" ")
}

fn run_sc(args: &[&str]) -> AppResult<ScOutput> {
    let mut command = Command::new("sc.exe");
    command.args(args);
    crate::platform::hide_std_console(&mut command);
    let output = command
        .output()
        .map_err(|error| AppError::Message(format!("无法执行 sc.exe：{error}")))?;
    Ok(ScOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn contains_sc_error(output: &ScOutput, code: u32) -> bool {
    let needle = code.to_string();
    output.code == i32::try_from(code).ok()
        || output.stdout.contains(&needle)
        || output.stderr.contains(&needle)
}

fn sc_error(operation: &str, output: &ScOutput) -> AppError {
    if contains_sc_error(output, 5) {
        return AppError::Message(format!(
            "{operation}失败：Windows SCM 拒绝访问（错误 5）；该操作需要管理员权限，请从管理员终端执行或以管理员身份启动 Anchor"
        ));
    }
    let detail = [output.stdout.trim(), output.stderr.trim()]
        .into_iter()
        .filter(|value| !value.is_empty() && !value.contains('\u{FFFD}'))
        .collect::<Vec<_>>()
        .join("；");
    let detail = if detail.is_empty() {
        format!("Windows SCM exit={:?}", output.code)
    } else {
        detail
    };
    AppError::Message(format!("{operation}失败：{detail}"))
}

fn parse_scm_state(output: &str) -> &'static str {
    let regex = regex::Regex::new(r":\s+([1-7])\s+").expect("SCM state regex");
    let code = output.lines().find_map(|line| {
        regex
            .captures(line)
            .and_then(|captures| captures.get(1))
            .and_then(|value| value.as_str().parse::<u32>().ok())
    });
    match code {
        Some(1) => "stopped",
        Some(2) => "start_pending",
        Some(3) => "stop_pending",
        Some(4) => "running",
        Some(5) => "continue_pending",
        Some(6) => "pause_pending",
        Some(7) => "paused",
        _ => "unknown",
    }
}

fn parse_scm_pid(output: &str) -> Option<u32> {
    let regex = regex::Regex::new(r"(?mi)^\s*PID\s*:\s*(\d+)\s*$").expect("SCM PID regex");
    regex
        .captures(output)
        .and_then(|captures| captures.get(1))
        .and_then(|value| value.as_str().parse::<u32>().ok())
        .filter(|pid| *pid != 0)
}

fn wait_for_scm_state(service_name: &str, expected: &str, timeout: Duration) -> AppResult<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let query = run_sc(&["query", service_name])?;
        if query.success && parse_scm_state(&query.stdout) == expected {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let observed = if query.success {
                parse_scm_state(&query.stdout).to_string()
            } else {
                combined_sc_output(&query)
            };
            return Err(AppError::Message(format!(
                "等待 Windows SCM service {service_name} 进入 {expected} 超时；当前状态：{observed}"
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn combined_sc_output(output: &ScOutput) -> String {
    [output.stdout.trim(), output.stderr.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("；")
}

fn parse_scm_auto_start(output: &str) -> bool {
    let regex = regex::Regex::new(r":\s+2\s+").expect("SCM start type regex");
    output.lines().any(|line| regex.is_match(line))
}

fn normalized_ids(ids: &[String]) -> Vec<String> {
    let mut ids = ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

pub fn run_service_dispatcher(
    config_dir: PathBuf,
    owner_sid: Option<String>,
    owner_username: Option<String>,
) -> AppResult<()> {
    if !config_dir.is_absolute() {
        return Err(AppError::Message(
            "Windows service-run config dir 必须是绝对路径".into(),
        ));
    }
    std::env::set_var(crate::brand::CONFIG_DIR_ENV, &config_dir);
    let (owner_sid, owner_username) = match (owner_sid, owner_username) {
        (Some(owner_sid), Some(owner_username)) => (owner_sid, owner_username),
        _ => {
            let error = AppError::Message(
                "Windows Service registration 尚未固定配置 owner 身份；请从配置 owner 会话重新执行 service install/update 后再启动"
                    .into(),
            );
            append_service_log(&format!("[service] registration rejected: {error}"));
            return Err(error);
        }
    };
    validate_service_owner(&owner_sid, &owner_username)?;
    std::env::set_var(crate::brand::WINDOWS_SERVICE_CONTEXT_ENV, "1");
    std::env::set_var(SERVICE_OWNER_SID_ENV, owner_sid.trim());
    std::env::set_var(SERVICE_OWNER_USERNAME_ENV, owner_username.trim());
    std::env::set_var(PIPE_USER_ENV, owner_username.trim());
    let service_name = service_name_for_dir(&config_dir);
    SERVICE_CONFIG_DIR
        .set(config_dir)
        .map_err(|_| AppError::Message("Windows service config dir 已初始化".into()))?;
    let wide = service_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    SERVICE_NAME_WIDE
        .set(wide)
        .map_err(|_| AppError::Message("Windows service name 已初始化".into()))?;
    SERVICE_STOP_REQUESTED.store(false, Ordering::SeqCst);
    let name_ptr = SERVICE_NAME_WIDE
        .get()
        .expect("service name")
        .as_ptr()
        .cast_mut();
    let table = [
        RawServiceTableEntry {
            service_name: name_ptr,
            service_proc: Some(service_main),
        },
        RawServiceTableEntry {
            service_name: null_mut(),
            service_proc: None,
        },
    ];
    let result = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if result == 0 {
        return Err(AppError::Message(format!(
            "连接 Windows Service Control Manager 失败：{}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let _ = std::panic::catch_unwind(service_main_inner);
}

fn service_main_inner() {
    let Some(name) = SERVICE_NAME_WIDE.get() else {
        return;
    };
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(service_control_handler), null_mut())
    };
    if handle.is_null() {
        return;
    }
    SERVICE_STATUS_HANDLE.store(handle as usize, Ordering::SeqCst);
    report_service_status(SERVICE_START_PENDING, 0, NO_ERROR, 5_000);
    publish_current_service_runtime_state();
    report_service_status(
        SERVICE_RUNNING,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
        NO_ERROR,
        0,
    );
    let result = crate::async_runtime::block_on(run_service_supervisor());
    let exit_code = if result.is_ok() { NO_ERROR } else { 1 };
    if let Err(error) = result {
        append_service_log(&format!("[service] supervisor exited with error: {error}"));
    }
    report_service_status(SERVICE_STOPPED, 0, exit_code, 0);
}

unsafe extern "system" fn service_control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    if matches!(control, SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN) {
        SERVICE_STOP_REQUESTED.store(true, Ordering::SeqCst);
        report_service_status(SERVICE_STOP_PENDING, 0, NO_ERROR, 20_000);
    }
    NO_ERROR
}

fn report_service_status(state: u32, controls: u32, exit_code: u32, wait_hint: u32) {
    let handle = SERVICE_STATUS_HANDLE.load(Ordering::SeqCst) as ServiceStatusHandle;
    if handle.is_null() {
        return;
    }
    let status = RawServiceStatus {
        service_type: SERVICE_WIN32_OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        win32_exit_code: exit_code,
        service_specific_exit_code: 0,
        checkpoint: 0,
        wait_hint,
    };
    unsafe {
        let _ = SetServiceStatus(handle, &status);
    }
}

fn publish_current_service_runtime_state() {
    let service_pid = std::process::id();
    match std::env::current_exe() {
        Ok(executable) => {
            let runtime = WindowsServiceRuntimeState {
                schema_version: SERVICE_RUNTIME_SCHEMA_VERSION,
                pid: service_pid,
                started_at_unix: unix_now(),
                executable_path: executable.display().to_string(),
                build_identity: BuildIdentity::current(),
            };
            if let Err(error) = write_service_runtime_state(&runtime) {
                append_service_log(&format!("[service] runtime state write failed: {error}"));
            }
        }
        Err(error) => append_service_log(&format!(
            "[service] current executable lookup failed: {error}"
        )),
    }
}

async fn run_service_supervisor() -> AppResult<()> {
    let service_pid = std::process::id();
    append_service_log(&format!(
        "[service] started pid={} configDir={}",
        service_pid,
        SERVICE_CONFIG_DIR
            .get()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
    ));
    match cleanup_wrong_owner_managed_frpc_processes() {
        Ok(count) if count > 0 => append_service_log(&format!(
            "[service] removed {count} legacy managed frpc process(es) owned by a non-config account"
        )),
        Ok(_) => {}
        Err(error) => append_service_log(&format!(
            "[service] legacy frpc owner cleanup failed: {error}"
        )),
    }
    let mut managed_workspaces = HashSet::<String>::new();
    let mut gateway_managed = false;
    loop {
        if SERVICE_STOP_REQUESTED.load(Ordering::SeqCst) {
            break;
        }
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
        tokio::time::sleep(SERVICE_RECONCILE_INTERVAL).await;
    }
    shutdown_managed_control_plane(&mut managed_workspaces, &mut gateway_managed).await;
    remove_service_runtime_state_if_owned(service_pid);
    append_service_log("[service] stopped");
    Ok(())
}

async fn reconcile_service_plan(
    plan: &WindowsServicePlan,
    managed_workspaces: &mut HashSet<String>,
    gateway_managed: &mut bool,
) -> AppResult<()> {
    let store = crate::data::DataStore::load_profiles_only()?;
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
            if let Err(error) = ensure_workspace_process_owner(profile).await {
                errors.push(format!("Workspace {} owner: {error}", profile.name));
                continue;
            }
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
            match crate::daemon::inspect(profile) {
                Ok(inspection) if inspection.running => {
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
                        Err(error) => {
                            errors.push(format!("Workspace {} stop: {error}", profile.name))
                        }
                    }
                }
                Ok(_) => {
                    managed_workspaces.remove(&profile.id);
                }
                Err(error) => errors.push(format!("Workspace {} inspect: {error}", profile.name)),
            }
        }
    }

    let desired_gateway = normalized_ids(&plan.gateway_workspace_ids);
    if desired_gateway.is_empty() {
        if *gateway_managed {
            match stop_gateway_if_running().await {
                Ok(()) => *gateway_managed = false,
                Err(error) => errors.push(format!("Gateway stop: {error}")),
            }
        }
    } else {
        if !gateway_config.enabled {
            errors.push("Windows service plan 请求 Gateway，但全局 MCP Gateway 配置未启用".into());
        } else {
            let missing = desired_gateway
                .iter()
                .filter(|workspace_id| !profile_ids.contains(workspace_id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                errors.push(format!(
                    "Windows service plan 引用了不存在的 Gateway workspace：{}",
                    missing.join(", ")
                ));
            } else {
                match ensure_gateway_running(&desired_gateway).await {
                    Ok(()) => *gateway_managed = true,
                    Err(error) => errors.push(format!("Gateway: {error}")),
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::Message(errors.join("；")))
    }
}

async fn ensure_workspace_process_owner(
    profile: &crate::workspace::WorkspaceProfile,
) -> AppResult<()> {
    let inspection = crate::daemon::inspect(profile)?;
    let Some(state) = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
    else {
        return Ok(());
    };
    if process_is_config_owner(state.pid)? {
        return Ok(());
    }

    let expected_sid = trusted_service_owner_sid()?;
    let actual_sid = process_user_sid(state.pid).unwrap_or_else(|_| "unknown".into());
    append_service_log(&format!(
        "[service] Workspace {} PID {} owner mismatch: expected={}, actual={}; restarting under config owner",
        profile.name, state.pid, expected_sid, actual_sid
    ));
    control::request_daemon_exit_and_wait(
        profile,
        control::ControlOperation::Restart,
        SERVICE_OPERATION_TIMEOUT,
        true,
    )
    .await?;
    Ok(())
}

async fn ensure_gateway_running(workspace_ids: &[String]) -> AppResult<()> {
    let inspection = gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if let Some(state) = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
    {
        let owner_matches = process_is_config_owner(state.pid)?;
        if owner_matches && state.workspace_ids == normalized_ids(workspace_ids) {
            gateway_control::ping()
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            return Ok(());
        }
        if !owner_matches {
            let expected_sid = trusted_service_owner_sid()?;
            let actual_sid = process_user_sid(state.pid).unwrap_or_else(|_| "unknown".into());
            append_service_log(&format!(
                "[service] Gateway PID {} owner mismatch: expected={}, actual={}; restarting under config owner",
                state.pid, expected_sid, actual_sid
            ));
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
    let pid = gateway_daemon::spawn(workspace_ids)?;
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
    let accepted = gateway_control::request_exit(GatewayOperation::Shutdown)
        .await
        .map_err(|error| AppError::Message(error.to_string()))?;
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
        let _ = stop_gateway_if_running().await;
        *gateway_managed = false;
    }
    let Ok(store) = crate::data::DataStore::load_profiles_only() else {
        return;
    };
    let profiles = store.list().to_vec();
    drop(store);
    for profile in profiles.iter().rev() {
        if managed_workspaces.contains(&profile.id) {
            let _ = control::request_daemon_exit_and_wait(
                profile,
                control::ControlOperation::Shutdown,
                SERVICE_OPERATION_TIMEOUT,
                true,
            )
            .await;
        }
    }
    managed_workspaces.clear();
}

fn append_service_log(line: &str) {
    let Ok(config_dir) = platform().app_config_dir() else {
        return;
    };
    let path = config_dir.join("logs").join("windows-service.log");
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", crate::logging::timestamped_line(line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_is_scoped_by_config_directory() {
        let first = service_name_for_dir(Path::new(r"C:\Users\alice\AppData\Roaming\anchor"));
        let same = service_name_for_dir(Path::new(r"c:\users\alice\appdata\roaming\anchor"));
        let slash = service_name_for_dir(Path::new(r"C:/Users/alice/AppData/Roaming/anchor/"));
        let other = service_name_for_dir(Path::new(r"C:\Users\bob\AppData\Roaming\anchor"));
        assert_eq!(first, same);
        assert_eq!(first, slash);
        assert_ne!(first, other);
        assert!(first.starts_with("AnchorControlPlane-"));
    }

    #[test]
    fn scm_state_parser_uses_numeric_state_and_ignores_type() {
        let output = "SERVICE_NAME: Anchor\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        STATE              : 4  RUNNING\r\n";
        assert_eq!(parse_scm_state(output), "running");
    }

    #[test]
    fn scm_queryex_parser_reads_nonzero_pid() {
        let output = "SERVICE_NAME: Anchor\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        STATE              : 4  RUNNING\r\n        PID                : 12345\r\n";
        assert_eq!(parse_scm_pid(output), Some(12_345));
        assert_eq!(parse_scm_pid("PID : 0\r\n"), None);
    }

    #[test]
    fn scm_qc_parser_reads_registered_executable_without_process_access() {
        let quoted = concat!(
            "SERVICE_NAME: Anchor\r\n",
            "        START_TYPE         : 2   AUTO_START\r\n",
            "        BINARY_PATH_NAME   : \"D:\\Program Files\\Anchor\\anchor-desktop.exe\" service-run C:\\config SID user\r\n",
        );
        assert_eq!(
            parse_scm_binary_executable(quoted).as_deref(),
            Some(r"D:\Program Files\Anchor\anchor-desktop.exe")
        );

        let unquoted =
            "BINARY_PATH_NAME : D:\\anchor\\anchor-desktop.exe service-run C:\\config\r\n";
        assert_eq!(
            parse_scm_binary_executable(unquoted).as_deref(),
            Some(r"D:\anchor\anchor-desktop.exe")
        );
    }

    #[test]
    fn runtime_validation_uses_scm_registration_without_opening_system_process() {
        let runtime = WindowsServiceRuntimeState {
            schema_version: SERVICE_RUNTIME_SCHEMA_VERSION,
            pid: u32::MAX - 7,
            started_at_unix: 1,
            executable_path: r"D:\Program Files\Anchor\anchor-desktop.exe".into(),
            build_identity: BuildIdentity::current(),
        };
        assert!(validate_service_runtime_state(
            runtime.clone(),
            Some(runtime.pid),
            Some(r"D:\Program Files\Anchor\anchor-desktop.exe")
        )
        .is_ok());
        let issue = validate_service_runtime_state(
            runtime.clone(),
            Some(runtime.pid),
            Some(r"D:\Other\anchor-desktop.exe"),
        )
        .expect_err("registered executable mismatch must fail");
        assert!(issue.starts_with("executable_mismatch:"));
    }

    #[test]
    fn service_build_state_detects_same_version_different_git_sha() {
        let current = BuildIdentity {
            package_version: "0.1.23".into(),
            git_sha: "aaaaaaaa".into(),
            git_dirty: false,
            build_workspace: "D:/anchor".into(),
        };
        let runtime = WindowsServiceRuntimeState {
            schema_version: SERVICE_RUNTIME_SCHEMA_VERSION,
            pid: 42,
            started_at_unix: 1,
            executable_path: r"D:\Program Files\Anchor\anchor-desktop.exe".into(),
            build_identity: BuildIdentity {
                package_version: "0.1.23".into(),
                git_sha: "bbbbbbbb".into(),
                git_dirty: false,
                build_workspace: "C:/build".into(),
            },
        };

        assert_eq!(
            service_build_state(true, "running", Some(&runtime), &current),
            "different"
        );
        assert_eq!(
            service_build_state(true, "running", None, &current),
            "unknown"
        );

        let mut same = runtime;
        same.build_identity.git_sha = current.git_sha.clone();
        assert_eq!(
            service_build_state(true, "running", Some(&same), &current),
            "current"
        );
    }

    #[test]
    fn service_binary_path_persists_config_owner_identity() {
        let binary = service_binary_path(
            Path::new(r"C:\Program Files\Anchor\anchor-desktop.exe"),
            Path::new(r"C:\Users\Demo User\AppData\Roaming\anchor"),
            "S-1-5-21-100-200-300-1001",
            "Demo User",
        );
        assert_eq!(
            binary,
            r#""C:\Program Files\Anchor\anchor-desktop.exe" service-run "C:\Users\Demo User\AppData\Roaming\anchor" S-1-5-21-100-200-300-1001 "Demo User""#
        );
    }

    #[test]
    fn owner_child_command_line_uses_windows_argument_quoting() {
        let command_line = owner_child_command_line(
            Path::new(r"C:\Program Files\Anchor\anchor-desktop.exe"),
            &[
                "--config-dir".into(),
                r"C:\Users\Demo User\AppData\Roaming\anchor".into(),
                "daemon-run".into(),
                "workspace-a".into(),
            ],
        );
        assert_eq!(
            command_line,
            r#""C:\Program Files\Anchor\anchor-desktop.exe" --config-dir "C:\Users\Demo User\AppData\Roaming\anchor" daemon-run workspace-a"#
        );
        assert_eq!(quote_windows_arg(""), "\"\"");
        assert_eq!(quote_windows_arg("plain"), "plain");
        assert_eq!(quote_windows_arg("a b"), r#""a b""#);
        assert_eq!(quote_windows_arg(r#"a"b"#), r#""a\"b""#);
    }

    #[test]
    fn service_pipe_acl_includes_valid_owner_sid_only() {
        assert!(valid_sid("S-1-5-21-100-200-300-1001"));
        assert!(!valid_sid("S-1-5-21-100;(A;;GA;;;WD)"));
    }

    #[test]
    fn service_owner_identity_validation_fails_closed() {
        assert!(validate_service_owner("S-1-5-21-100-200-300-1001", "Demo User").is_ok());
        assert!(validate_service_owner("not-a-sid", "Demo User").is_err());
        assert!(validate_service_owner("S-1-5-18", "SYSTEM").is_err());
        assert!(validate_service_owner("S-1-5-21-100-200-300-1001", "").is_err());
        assert!(validate_service_owner("S-1-5-21-100-200-300-1001", "Demo\nUser").is_err());
    }

    #[test]
    fn scm_access_denied_uses_stable_localized_message_without_mojibake() {
        let output = ScOutput {
            success: false,
            code: Some(5),
            stdout: "[SC] OpenSCManager � 5".into(),
            stderr: String::new(),
        };
        let message = sc_error("安装 Windows SCM service", &output).to_string();
        assert!(message.contains("错误 5"));
        assert!(message.contains("管理员权限"));
        assert!(!message.contains('�'));
    }

    #[test]
    fn runtime_state_replacement_overwrites_stale_windows_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("runtime.tmp");
        let destination = temp.path().join("runtime.json");
        fs::write(&source, b"new-runtime").expect("write source");
        fs::write(&destination, b"stale-runtime").expect("write destination");

        replace_runtime_file(&source, &destination).expect("replace runtime state");

        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"new-runtime"
        );
        assert!(!source.exists());
    }
}
