use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::secret::SecretStore;
use crate::tunnel::append_profile_log;
use crate::workspace::WorkspaceProfile;

use super::ilink::{self, GetUpdatesOutcome, ILinkAccount, ILinkConfig, PollError};
use super::state;

const WORKER_STATE_VERSION: u32 = 1;
const RETRY_DELAYS: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ILinkWorkerStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub state: String,
    pub logged_in: bool,
    pub bound: bool,
    pub reauthorization_required: bool,
    pub last_error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeState {
    schema_version: u32,
    profile_id: String,
    pid: u32,
    state: String,
    executable_path: String,
    started_at_unix_ms: u64,
    last_error: String,
}

struct WorkerLock(File);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingAction {
    Bind,
    Refresh,
    Ignore,
}

impl Drop for WorkerLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn start(profile: &WorkspaceProfile) -> AppResult<ILinkWorkerStatus> {
    let current = status(profile)?;
    if current.running {
        return Ok(current);
    }
    if SecretStore::get(&profile.id, "ilink_bot_token")?.is_none() {
        return Err(AppError::Message(
            "iLink 尚未登录；请先执行 `anchor workspace ilink login <workspace>`".into(),
        ));
    }
    let executable = std::env::current_exe()?;
    let mut command = Command::new(&executable);
    command
        .arg("workspace")
        .arg("ilink")
        .arg("run")
        .arg(&profile.id)
        .current_dir(&profile.path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
        .map_err(|error| AppError::Message(format!("启动 iLink worker 失败：{error}")))?;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(50));
        let observed = status(profile)?;
        if observed.running || observed.reauthorization_required {
            return Ok(observed);
        }
    }
    Ok(ILinkWorkerStatus {
        running: platform().is_process_alive(child.id()),
        pid: Some(child.id()),
        state: "starting".into(),
        logged_in: true,
        bound: binding_configured(&profile.id)?,
        reauthorization_required: false,
        last_error: String::new(),
    })
}

pub fn stop(profile: &WorkspaceProfile) -> AppResult<ILinkWorkerStatus> {
    let Some(state) = load_runtime_state(&profile.id)? else {
        return status(profile);
    };
    if !platform().is_process_alive(state.pid) {
        let _ = remove_runtime_state(&profile.id);
        return status(profile);
    }
    let actual = platform().process_image_path(state.pid)?.ok_or_else(|| {
        AppError::Message("无法验证 iLink worker 进程镜像，拒绝终止未知进程".into())
    })?;
    if !same_executable(Path::new(&actual), Path::new(&state.executable_path)) {
        return Err(AppError::Message(
            "iLink worker PID 已被其他进程复用，拒绝终止".into(),
        ));
    }
    platform().terminate_process_tree(state.pid)?;
    let _ = remove_runtime_state(&profile.id);
    status(profile)
}

pub fn status(profile: &WorkspaceProfile) -> AppResult<ILinkWorkerStatus> {
    let logged_in = SecretStore::get(&profile.id, "ilink_bot_token")?.is_some();
    let bound = binding_configured(&profile.id)?;
    let Some(state) = load_runtime_state(&profile.id)? else {
        return Ok(ILinkWorkerStatus {
            running: false,
            pid: None,
            state: if logged_in {
                "stopped"
            } else {
                "not_logged_in"
            }
            .into(),
            logged_in,
            bound,
            reauthorization_required: false,
            last_error: String::new(),
        });
    };
    let running = platform().is_process_alive(state.pid)
        && platform()
            .process_image_path(state.pid)
            .ok()
            .flatten()
            .is_some_and(|actual| {
                same_executable(Path::new(&actual), Path::new(&state.executable_path))
            });
    let reauthorization_required = state.state == "reauthorization_required";
    Ok(ILinkWorkerStatus {
        running,
        pid: running.then_some(state.pid),
        state: if running {
            state.state
        } else if reauthorization_required {
            "reauthorization_required".into()
        } else {
            "stopped".into()
        },
        logged_in,
        bound,
        reauthorization_required,
        last_error: bounded(&state.last_error, 400),
    })
}

pub fn clear_runtime_status(profile_id: &str) -> AppResult<()> {
    remove_runtime_state(profile_id)
}

pub async fn run(profile_id: &str) -> AppResult<()> {
    let profile = load_profile(profile_id)?;
    let _lock = acquire_worker_lock(profile_id)?;
    let executable_path = std::env::current_exe()?.display().to_string();
    let mut runtime = RuntimeState {
        schema_version: WORKER_STATE_VERSION,
        profile_id: profile.id.clone(),
        pid: std::process::id(),
        state: "running".into(),
        executable_path,
        started_at_unix_ms: unix_time_ms(),
        last_error: String::new(),
    };
    save_runtime_state(&runtime)?;
    let result = poll_loop(&profile, &mut runtime).await;
    match &result {
        Err(AppError::Message(message)) if runtime.state == "reauthorization_required" => {
            runtime.last_error = bounded(message, 300);
            let _ = save_runtime_state(&runtime);
        }
        Err(error) => {
            runtime.state = "stopped_with_error".into();
            runtime.last_error = bounded(&error.to_string(), 300);
            let _ = save_runtime_state(&runtime);
        }
        Ok(()) => {
            let _ = remove_runtime_state(&profile.id);
        }
    }
    result
}

async fn poll_loop(profile: &WorkspaceProfile, runtime: &mut RuntimeState) -> AppResult<()> {
    let (account, account_key) = load_account(&profile.id)?;
    let mut cursor = state::load_cursor(&profile.id, &account_key).map_err(AppError::Message)?;
    let mut timeout_ms = 35_000_u64;
    let mut failure_count = 0_usize;
    loop {
        match ilink::get_updates(&account, &cursor, timeout_ms).await {
            Ok(GetUpdatesOutcome::Timeout) => {
                failure_count = 0;
                set_runtime_state(runtime, "running", "")?;
            }
            Ok(GetUpdatesOutcome::Batch(batch)) => {
                for message in &batch.messages {
                    handle_inbound(profile, &account, message).await?;
                }
                if batch.cursor != cursor {
                    state::save_cursor(&profile.id, &account_key, &batch.cursor)
                        .map_err(AppError::Message)?;
                    cursor = batch.cursor;
                }
                timeout_ms = batch.next_timeout_ms;
                failure_count = 0;
                set_runtime_state(runtime, "running", "")?;
            }
            Err(PollError::StaleToken) => {
                let _ = state::reset_cursor(&profile.id);
                set_runtime_state(
                    runtime,
                    "reauthorization_required",
                    "iLink bot token is stale; QR login is required",
                )?;
                append_profile_log(
                    &profile.id,
                    "stderr.log",
                    "[ilink] bot token stale; worker stopped and QR login is required",
                );
                return Err(AppError::Message(
                    "iLink bot token 已失效；请重新执行 QR 登录".into(),
                ));
            }
            Err(error) => {
                failure_count = failure_count.saturating_add(1);
                let message = error.safe_message();
                set_runtime_state(runtime, "retrying", &message)?;
                append_profile_log(
                    &profile.id,
                    "stderr.log",
                    &format!(
                        "[ilink] getupdates transient failure: {}",
                        bounded(&message, 300)
                    ),
                );
                let delay = RETRY_DELAYS
                    .get(failure_count.saturating_sub(1))
                    .copied()
                    .unwrap_or_else(|| *RETRY_DELAYS.last().expect("retry delays"));
                tokio::time::sleep(delay).await;
            }
        }
    }
}

async fn handle_inbound(
    profile: &WorkspaceProfile,
    account: &ILinkAccount,
    message: &ilink::InboundTextMessage,
) -> AppResult<()> {
    let scanner = SecretStore::get(&profile.id, "ilink_login_user_id")?;
    let current_target = SecretStore::get(&profile.id, "ilink_target_user_id")?;
    match binding_action(
        scanner.as_deref(),
        current_target.as_deref(),
        &message.from_user_id,
        &message.text,
    ) {
        BindingAction::Bind => {
            SecretStore::set_many(
                &profile.id,
                &[
                    ("ilink_target_user_id", &message.from_user_id),
                    ("ilink_context_token", &message.context_token),
                ],
            )?;
            append_profile_log(
                &profile.id,
                "stdout.log",
                "[ilink] notification target bound from explicit /bind message",
            );
            let ack = ILinkConfig::new(
                account.bot_token.clone(),
                message.from_user_id.clone(),
                message.context_token.clone(),
                Some(account.base_url.clone()),
            )
            .map_err(AppError::Message)?;
            if let Err(error) = ilink::send_text(
                &ack,
                "Anchor 已绑定此微信会话，Harness 任务完成后会发送通知。",
            )
            .await
            {
                append_profile_log(
                    &profile.id,
                    "stderr.log",
                    &format!(
                        "[ilink] bind acknowledgement failed: {}",
                        bounded(&error, 240)
                    ),
                );
            }
        }
        BindingAction::Refresh => {
            SecretStore::set(&profile.id, "ilink_context_token", &message.context_token)?;
        }
        BindingAction::Ignore if message.text.trim() == "/bind" => {
            append_profile_log(
                &profile.id,
                "stderr.log",
                "[ilink] ignored /bind from a user that did not authorize the QR login",
            );
        }
        BindingAction::Ignore => {}
    }
    Ok(())
}

fn binding_action(
    scanner: Option<&str>,
    target: Option<&str>,
    sender: &str,
    text: &str,
) -> BindingAction {
    if text.trim() == "/bind" {
        return if scanner == Some(sender) {
            BindingAction::Bind
        } else {
            BindingAction::Ignore
        };
    }
    if target == Some(sender) {
        BindingAction::Refresh
    } else {
        BindingAction::Ignore
    }
}

fn load_account(profile_id: &str) -> AppResult<(ILinkAccount, String)> {
    let bot_token = SecretStore::get(profile_id, "ilink_bot_token")?
        .ok_or_else(|| AppError::Message("iLink bot token 未配置".into()))?;
    let base_url = SecretStore::get(profile_id, "ilink_base_url")?;
    let account = ILinkAccount::new(bot_token.clone(), base_url).map_err(AppError::Message)?;
    let account_key = SecretStore::get(profile_id, "ilink_bot_id")?
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("token:{:x}", Sha256::digest(bot_token.as_bytes())));
    Ok((account, account_key))
}

fn binding_configured(profile_id: &str) -> AppResult<bool> {
    Ok(
        SecretStore::get(profile_id, "ilink_target_user_id")?.is_some()
            && SecretStore::get(profile_id, "ilink_context_token")?.is_some(),
    )
}

fn load_profile(profile_id: &str) -> AppResult<WorkspaceProfile> {
    let store = DataStore::load()?;
    store
        .list()
        .iter()
        .find(|profile| profile.id == profile_id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {profile_id}")))
}

fn runtime_dir() -> AppResult<PathBuf> {
    let path = platform().app_config_dir()?.join("runtime").join("ilink");
    fs::create_dir_all(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

fn runtime_state_path(profile_id: &str) -> AppResult<PathBuf> {
    validate_profile_id(profile_id)?;
    Ok(runtime_dir()?.join(format!("{profile_id}.json")))
}

fn runtime_lock_path(profile_id: &str) -> AppResult<PathBuf> {
    validate_profile_id(profile_id)?;
    Ok(runtime_dir()?.join(format!("{profile_id}.lock")))
}

fn acquire_worker_lock(profile_id: &str) -> AppResult<WorkerLock> {
    let path = runtime_lock_path(profile_id)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.try_lock_exclusive().map_err(|_| {
        AppError::Message("iLink worker 已在运行，拒绝启动第二个 getupdates consumer".into())
    })?;
    Ok(WorkerLock(file))
}

fn load_runtime_state(profile_id: &str) -> AppResult<Option<RuntimeState>> {
    let path = runtime_state_path(profile_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    let state: RuntimeState = serde_json::from_slice(&bytes)?;
    if state.schema_version != WORKER_STATE_VERSION || state.profile_id != profile_id {
        return Err(AppError::Message(
            "iLink worker state 不兼容或已损坏".into(),
        ));
    }
    Ok(Some(state))
}

fn save_runtime_state(state: &RuntimeState) -> AppResult<()> {
    let path = runtime_state_path(&state.profile_id)?;
    let mut bytes = serde_json::to_vec_pretty(state)?;
    bytes.push(b'\n');
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

fn remove_runtime_state(profile_id: &str) -> AppResult<()> {
    let path = runtime_state_path(profile_id)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn set_runtime_state(state: &mut RuntimeState, name: &str, error: &str) -> AppResult<()> {
    state.state = name.to_string();
    state.last_error = bounded(error, 300);
    save_runtime_state(state)
}

fn same_executable(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn validate_profile_id(value: &str) -> AppResult<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(AppError::Message("invalid workspace profile id".into()));
    }
    Ok(())
}

fn bounded(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let mut result = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_ids_are_path_safe() {
        assert!(validate_profile_id("abc_123-def").is_ok());
        assert!(validate_profile_id("../escape").is_err());
    }

    #[test]
    fn binding_requires_scanner_and_refreshes_only_bound_user() {
        assert_eq!(
            binding_action(Some("scanner"), None, "scanner", "/bind"),
            BindingAction::Bind
        );
        assert_eq!(
            binding_action(Some("scanner"), None, "attacker", "/bind"),
            BindingAction::Ignore
        );
        assert_eq!(
            binding_action(Some("scanner"), Some("scanner"), "scanner", "hello"),
            BindingAction::Refresh
        );
        assert_eq!(
            binding_action(Some("scanner"), Some("scanner"), "other", "hello"),
            BindingAction::Ignore
        );
    }
}
