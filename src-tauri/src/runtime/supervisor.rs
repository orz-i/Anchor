use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use crate::async_runtime::JoinHandle;

use crate::actions;
use crate::error::AppResult;
use crate::mcp;
use crate::platform::platform;
use crate::runtime::port::{
    is_own_process, port_busy_message, try_reclaim_previous_macos_app_port,
    wait_for_port_free_blocking,
};
use crate::secret::SecretStore;
use crate::settings::AppSettings;
use crate::tools::policy::PolicySettings;
use crate::tunnel::append_profile_log;
use crate::workspace::{RuntimeRecoveryDto, RuntimeStatusDto, WorkspaceProfile};

const MAX_RECOVERY_ATTEMPTS: u8 = 5;
const RECOVERY_BASE_DELAY_MS: u64 = 1_000;
const RECOVERY_MAX_DELAY_MS: u64 = 16_000;
const STARTING_STALL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    Mcp,
    Actions,
}

fn starting_stalled(entry: &RuntimeEntry) -> bool {
    entry.phase == RuntimePhase::Starting
        && entry
            .started_at
            .is_some_and(|started| started.elapsed() >= STARTING_STALL_TIMEOUT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePhase {
    Stopped,
    Starting,
    Running,
    Recovering,
    Stopping,
    Error,
}

struct RuntimeEntry {
    phase: RuntimePhase,
    shutdown: Option<mcp::ShutdownSender>,
    handle: Option<JoinHandle<()>>,
    error_message: Option<String>,
    started_at: Option<std::time::Instant>,
    missing_port_checks: u8,
    desired_running: bool,
    recovery_attempt: u8,
    next_retry_at: Option<std::time::Instant>,
    recovered_count: u32,
}

#[derive(Default)]
pub struct RuntimeSupervisor {
    entries: HashMap<(String, ServiceKind), RuntimeEntry>,
}

impl RuntimeSupervisor {
    #[cfg(test)]
    fn mcp_status_with_settings(
        &self,
        profile: &WorkspaceProfile,
        settings: &AppSettings,
    ) -> RuntimeStatusDto {
        let mut status = self.status_with_settings(profile, ServiceKind::Mcp, settings);
        if status.state == "running" {
            status.activity = Some(mcp::activity_snapshot(&profile.id));
        }
        status
    }

    pub fn start_mcp(&mut self, profile: &WorkspaceProfile) -> AppResult<RuntimeStatusDto> {
        self.start(profile, ServiceKind::Mcp)
    }

    pub fn start_actions(&mut self, profile: &WorkspaceProfile) -> AppResult<RuntimeStatusDto> {
        self.start(profile, ServiceKind::Actions)
    }

    pub fn restart_mcp(&mut self, profile: &WorkspaceProfile) -> AppResult<RuntimeStatusDto> {
        self.restart(profile, ServiceKind::Mcp)
    }

    pub fn restart_actions(&mut self, profile: &WorkspaceProfile) -> AppResult<RuntimeStatusDto> {
        self.restart(profile, ServiceKind::Actions)
    }

    /// True when the service for this workspace is currently running.
    pub fn is_running(&self, workspace_id: &str, kind: ServiceKind) -> bool {
        matches!(
            self.entries
                .get(&(workspace_id.to_string(), kind))
                .map(|entry| &entry.phase),
            Some(RuntimePhase::Running)
        )
    }

    pub fn maintain_mcp(&mut self, profile: &WorkspaceProfile) -> AppResult<RuntimeStatusDto> {
        self.maintain(profile, ServiceKind::Mcp)
    }

    pub fn maintain_actions(&mut self, profile: &WorkspaceProfile) -> AppResult<RuntimeStatusDto> {
        self.maintain(profile, ServiceKind::Actions)
    }

    pub fn drop_workspace(&mut self, profile: &WorkspaceProfile) {
        self.sync_stop_and_wait(profile, ServiceKind::Mcp);
        self.sync_stop_and_wait(profile, ServiceKind::Actions);
    }

    pub fn active_mcp_workspace_ids(&self) -> HashSet<String> {
        self.entries
            .iter()
            .filter_map(|((workspace_id, kind), entry)| {
                if *kind == ServiceKind::Mcp
                    && matches!(
                        entry.phase,
                        RuntimePhase::Running | RuntimePhase::Starting | RuntimePhase::Recovering
                    )
                {
                    Some(workspace_id.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn begin_stop(&mut self, workspace_id: &str, kind: ServiceKind) -> Option<JoinHandle<()>> {
        let key = (workspace_id.to_string(), kind);
        let entry = self.entries.get_mut(&key)?;

        entry.phase = RuntimePhase::Stopping;
        entry.desired_running = false;
        entry.next_retry_at = None;
        let shutdown = entry.shutdown.take();
        let handle = entry.handle.take();
        if let Some(shutdown) = shutdown {
            let _ = shutdown.send(());
        }
        handle
    }

    pub fn finish_stop(&mut self, workspace_id: &str, kind: ServiceKind) {
        self.entries.remove(&(workspace_id.to_string(), kind));
    }

    fn status(&self, profile: &WorkspaceProfile, kind: ServiceKind) -> AppResult<RuntimeStatusDto> {
        let settings = AppSettings::load()?;
        Ok(self.status_with_settings(profile, kind, &settings))
    }

    fn status_with_settings(
        &self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
        settings: &AppSettings,
    ) -> RuntimeStatusDto {
        let key = (profile.id.clone(), kind);
        let phase = self
            .entries
            .get(&key)
            .map(|entry| entry.phase)
            .unwrap_or(RuntimePhase::Stopped);

        let (local_endpoint, public_endpoint) = endpoints(profile, kind, settings);
        let port = port_for(profile, kind);
        let service_label = service_label(kind);
        let recovery = self.recovery_status(&key);

        match phase {
            RuntimePhase::Running => RuntimeStatusDto {
                state: "running".into(),
                pid: None,
                local_message: format!("{service_label}正在监听 127.0.0.1:{port}"),
                public_message: public_message_for(profile, kind, settings),
                local_endpoint,
                public_endpoint,
                recovery,
                activity: None,
            },
            RuntimePhase::Starting => RuntimeStatusDto {
                state: "starting".into(),
                pid: None,
                local_message: format!("正在启动{service_label}端口 {port}"),
                public_message: "等待服务就绪".into(),
                local_endpoint,
                public_endpoint,
                recovery,
                activity: None,
            },
            RuntimePhase::Recovering => RuntimeStatusDto {
                state: "recovering".into(),
                pid: None,
                local_message: recovery_message(service_label, &recovery),
                public_message: "连接中断，正在自动恢复".into(),
                local_endpoint,
                public_endpoint,
                recovery,
                activity: None,
            },
            RuntimePhase::Stopping => RuntimeStatusDto {
                state: "stopping".into(),
                pid: None,
                local_message: "正在停止".into(),
                public_message: "正在停止".into(),
                local_endpoint,
                public_endpoint,
                recovery,
                activity: None,
            },
            RuntimePhase::Error => {
                let message = self
                    .entries
                    .get(&key)
                    .and_then(|entry| entry.error_message.clone())
                    .unwrap_or_else(|| "运行失败".into());
                RuntimeStatusDto {
                    state: "error".into(),
                    pid: None,
                    local_message: message.clone(),
                    public_message: message,
                    local_endpoint,
                    public_endpoint,
                    recovery,
                    activity: None,
                }
            }
            RuntimePhase::Stopped => RuntimeStatusDto {
                state: "stopped".into(),
                pid: None,
                local_message: "未启动".into(),
                public_message: "未知".into(),
                local_endpoint,
                public_endpoint,
                recovery,
                activity: None,
            },
        }
    }

    fn recovery_status(&self, key: &(String, ServiceKind)) -> RuntimeRecoveryDto {
        let Some(entry) = self.entries.get(key) else {
            return RuntimeRecoveryDto {
                enabled: false,
                attempt: 0,
                max_attempts: MAX_RECOVERY_ATTEMPTS,
                retry_in_ms: None,
                recovered_count: 0,
                last_error: String::new(),
            };
        };
        RuntimeRecoveryDto {
            enabled: entry.desired_running,
            attempt: entry.recovery_attempt,
            max_attempts: MAX_RECOVERY_ATTEMPTS,
            retry_in_ms: entry.next_retry_at.map(|deadline| {
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_millis()
                    .min(u64::MAX as u128) as u64
            }),
            recovered_count: entry.recovered_count,
            last_error: entry.error_message.clone().unwrap_or_default(),
        }
    }

    fn start(
        &mut self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
    ) -> AppResult<RuntimeStatusDto> {
        self.attempt_start(profile, kind, false)
    }

    fn attempt_start(
        &mut self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
        recovery: bool,
    ) -> AppResult<RuntimeStatusDto> {
        let key = (profile.id.clone(), kind);
        if !recovery
            && matches!(
                self.entries.get(&key).map(|e| &e.phase),
                Some(RuntimePhase::Running) | Some(RuntimePhase::Starting)
            )
        {
            return self.status(profile, kind);
        }
        if !recovery
            && matches!(
                self.entries.get(&key).map(|e| &e.phase),
                Some(RuntimePhase::Stopping)
            )
        {
            return Err(crate::error::AppError::Message(format!(
                "{}正在停止，请稍后再试",
                service_label(kind).trim()
            )));
        }

        let previous = self.entries.remove(&key);
        let previous_recovered_count = previous
            .as_ref()
            .map(|entry| entry.recovered_count)
            .unwrap_or(0);
        let recovery_attempt = if recovery {
            previous
                .as_ref()
                .map(|entry| entry.recovery_attempt.saturating_add(1))
                .unwrap_or(1)
        } else {
            0
        };
        let previous_error = previous.and_then(|entry| entry.error_message);

        self.entries.insert(
            key.clone(),
            RuntimeEntry {
                phase: RuntimePhase::Starting,
                shutdown: None,
                handle: None,
                error_message: previous_error,
                started_at: Some(std::time::Instant::now()),
                missing_port_checks: 0,
                desired_running: true,
                recovery_attempt,
                next_retry_at: None,
                recovered_count: previous_recovered_count,
            },
        );

        let port = port_for(profile, kind);
        if let Some(pid) = platform().find_pid_listening_on_port(port)? {
            if is_own_process(pid) {
                wait_for_port_free_blocking(port, Duration::from_secs(3));
            }
            if try_reclaim_previous_macos_app_port(port) {
                // A previous source-built or installed instance of this macOS
                // app released the port; continue with the current listener.
            }
            if let Some(pid) = platform().find_pid_listening_on_port(port)? {
                let message = port_busy_message(port, service_label(kind).trim(), pid);
                append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    &format!("[start] {message}"),
                );
                if recovery {
                    self.record_recovery_failure(
                        &key,
                        recovery_attempt,
                        previous_recovered_count,
                        message,
                    );
                    return self.status(profile, kind);
                }
                self.entries.remove(&key);
                return Err(crate::error::AppError::Message(message));
            }
        }

        let spawn_result = match kind {
            ServiceKind::Mcp => {
                let settings = AppSettings::load()?;
                let mut runtime_config = profile.runtime.clone();
                runtime_config.strict_workspace_reads = settings.mcp_gateway.enabled;
                let use_shared = profile.auth.use_shared_secrets;
                let mut auth = profile.auth.clone();
                if use_shared {
                    if let Some(client_id) = SecretStore::get_shared("oauth_client_id")? {
                        auth.oauth_client_id = client_id;
                    }
                }
                // ChatGPT connectors use PKCE and do not send client_secret.
                let oauth_client_secret = None;
                let oauth_password = if profile.auth.oauth_enabled() {
                    resolve_secret(&profile.id, "oauth_password", use_shared)?
                } else {
                    None
                };
                let oauth_token_secret = if profile.auth.oauth_enabled() {
                    resolve_secret(&profile.id, "oauth_token_secret", use_shared)?
                } else {
                    None
                };
                mcp::spawn_listener(
                    port,
                    PathBuf::from(&profile.path),
                    profile.id.clone(),
                    profile.name.clone(),
                    auth,
                    profile.mcp_external_base_url_with(&settings),
                    oauth_client_secret,
                    oauth_password,
                    oauth_token_secret,
                    runtime_config,
                )
            }
            ServiceKind::Actions => {
                let auth_type = profile.actions.auth_type.clone();
                let use_shared = profile.actions.use_shared_secrets;
                let api_key = if auth_type == "api_key" {
                    resolve_secret(&profile.id, "actions_api_key", use_shared)?
                } else {
                    None
                };
                let oauth_client_secret = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_client_secret", true)?
                    } else {
                        Some(actions_oauth_secret(
                            &profile.id,
                            "actions_oauth_client_secret",
                        )?)
                    }
                } else {
                    None
                };
                let oauth_password = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_password", true)?
                    } else {
                        Some(actions_oauth_secret(&profile.id, "actions_oauth_password")?)
                    }
                } else {
                    None
                };
                let oauth_token_secret = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_token_secret", true)?
                    } else {
                        Some(actions_oauth_secret(
                            &profile.id,
                            "actions_oauth_token_secret",
                        )?)
                    }
                } else {
                    None
                };
                let public_base_url = profile.actions_public_base_url()?;
                let policy = PolicySettings::from_actions_config(&profile.actions);
                actions::spawn_listener(
                    &profile.id,
                    profile.name.clone(),
                    port,
                    PathBuf::from(&profile.path),
                    public_base_url,
                    auth_type,
                    api_key,
                    profile.actions.oauth_client_id.clone(),
                    profile.actions.oauth_redirect_uris.clone(),
                    profile.actions.oauth_redirect_hosts.clone(),
                    oauth_client_secret,
                    oauth_password,
                    oauth_token_secret,
                    policy,
                )
            }
        };

        match spawn_result {
            Ok((shutdown, handle)) => {
                let started_at = self
                    .entries
                    .get(&key)
                    .and_then(|entry| entry.started_at)
                    .or_else(|| Some(std::time::Instant::now()));
                self.entries.insert(
                    key,
                    RuntimeEntry {
                        phase: RuntimePhase::Running,
                        shutdown: Some(shutdown),
                        handle: Some(handle),
                        error_message: None,
                        started_at,
                        missing_port_checks: 0,
                        desired_running: true,
                        recovery_attempt: 0,
                        next_retry_at: None,
                        recovered_count: previous_recovered_count
                            .saturating_add(u32::from(recovery)),
                    },
                );
                if recovery {
                    append_profile_log(
                        &profile.id,
                        "stdout.log",
                        &format!(
                            "[recovery] {}已自动恢复（第 {recovery_attempt} 次尝试）",
                            service_label(kind).trim()
                        ),
                    );
                }
            }
            Err(err) => {
                // spawn_listener can fail synchronously before the server task is
                // ever created (e.g. missing API key / OAuth secret). In that case
                // serve() never runs, so nothing writes to the stderr log and the
                // failure was previously invisible in the log viewer. Record it here.
                append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    &format!("[start] {}启动失败：{err}", service_label(kind).trim()),
                );
                if recovery {
                    self.record_recovery_failure(
                        &key,
                        recovery_attempt,
                        previous_recovered_count,
                        err,
                    );
                    return self.status(profile, kind);
                }
                self.entries.insert(
                    key,
                    RuntimeEntry {
                        phase: RuntimePhase::Error,
                        shutdown: None,
                        handle: None,
                        error_message: Some(err.to_string()),
                        started_at: None,
                        missing_port_checks: 0,
                        desired_running: false,
                        recovery_attempt: 0,
                        next_retry_at: None,
                        recovered_count: previous_recovered_count,
                    },
                );
            }
        }

        self.status(profile, kind)
    }

    fn record_recovery_failure(
        &mut self,
        key: &(String, ServiceKind),
        attempt: u8,
        recovered_count: u32,
        message: String,
    ) {
        let exhausted = attempt >= MAX_RECOVERY_ATTEMPTS;
        let error_message = if exhausted {
            format!(
                "自动恢复已尝试 {MAX_RECOVERY_ATTEMPTS} 次仍失败：{message}。请检查配置后手动重试。"
            )
        } else {
            message
        };
        self.entries.insert(
            key.clone(),
            RuntimeEntry {
                phase: if exhausted {
                    RuntimePhase::Error
                } else {
                    RuntimePhase::Recovering
                },
                shutdown: None,
                handle: None,
                error_message: Some(error_message),
                started_at: None,
                missing_port_checks: 0,
                desired_running: !exhausted,
                recovery_attempt: attempt,
                next_retry_at: (!exhausted)
                    .then(|| std::time::Instant::now() + recovery_delay(attempt)),
                recovered_count,
            },
        );
    }

    /// Stop the current service (if running), then immediately start a new one.
    /// This is the canonical "restart" — used when the user regenerates a key or
    /// toggles the shared-secret switch, so the listener picks up the new value.
    ///
    /// stop_internal sends the graceful-shutdown signal but the OS port may not
    /// be freed instantly (the old listener's socket is closed on the tokio
    /// event loop). We retry `start` with a short back-off to smooth over this
    /// window.
    fn restart(
        &mut self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
    ) -> AppResult<RuntimeStatusDto> {
        self.sync_stop_and_wait(profile, kind);
        self.start(profile, kind)
    }

    fn sync_stop_and_wait(&mut self, profile: &WorkspaceProfile, kind: ServiceKind) {
        let port = port_for(profile, kind);
        let handle = self.begin_stop(&profile.id, kind);
        if handle.is_some() {
            crate::runtime::port::await_listener_shutdown_blocking(handle, port);
        } else if platform()
            .find_pid_listening_on_port(port)
            .ok()
            .flatten()
            .is_some()
        {
            wait_for_port_free_blocking(port, Duration::from_secs(3));
        }
        self.finish_stop(&profile.id, kind);
    }

    fn maintain(
        &mut self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
    ) -> AppResult<RuntimeStatusDto> {
        let key = (profile.id.clone(), kind);
        let port = port_for(profile, kind);
        let mut should_retry = false;
        if let Some(entry) = self.entries.get_mut(&key) {
            if starting_stalled(entry) {
                entry.phase = RuntimePhase::Recovering;
                entry.error_message = Some(format!(
                    "{}启动流程长时间未完成，已转入自动恢复",
                    service_label(kind).trim()
                ));
                entry.started_at = None;
                entry.desired_running = true;
                entry.recovery_attempt = 0;
                entry.next_retry_at = Some(std::time::Instant::now() + recovery_delay(0));
                append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    "[recovery] 启动流程超时，准备自动恢复",
                );
            } else if entry.phase == RuntimePhase::Running {
                let task_finished = entry.handle.as_ref().is_some_and(JoinHandle::is_finished);
                let listening = match platform().find_pid_listening_on_port(port) {
                    Ok(pid) => pid.is_some(),
                    Err(error) => {
                        append_profile_log(
                            &profile.id,
                            stderr_log_name(kind),
                            &format!("[refresh] 检查端口 {port} 失败，保留当前线路：{error}"),
                        );
                        return self.status(profile, kind);
                    }
                };
                if task_finished || should_mark_runtime_error(entry, listening) {
                    if let Some(handle) = entry.handle.take() {
                        handle.abort();
                        crate::async_runtime::spawn(async move {
                            let _ = handle.await;
                        });
                    }
                    entry.shutdown.take();
                    let occupied_by_self = platform()
                        .find_pid_listening_on_port(port)
                        .ok()
                        .flatten()
                        .map(is_own_process)
                        .unwrap_or(false);
                    let message = if occupied_by_self {
                        format!(
                            "{}端口 {} 未能成功启动，可能仍被本应用上一次服务占用，请先停止后再试",
                            service_label(kind).trim(),
                            port
                        )
                    } else {
                        format!(
                            "{}端口 {} 未能成功启动，可能已被其他程序占用",
                            service_label(kind).trim(),
                            port
                        )
                    };
                    entry.phase = RuntimePhase::Recovering;
                    entry.error_message = Some(message);
                    entry.started_at = None;
                    entry.desired_running = true;
                    entry.recovery_attempt = 0;
                    entry.next_retry_at = Some(std::time::Instant::now() + recovery_delay(0));
                    append_profile_log(
                        &profile.id,
                        stderr_log_name(kind),
                        &format!(
                            "[recovery] 检测到{}断联，准备自动恢复",
                            service_label(kind).trim()
                        ),
                    );
                }
            } else if entry.phase == RuntimePhase::Recovering
                && entry.desired_running
                && entry
                    .next_retry_at
                    .is_some_and(|deadline| deadline <= std::time::Instant::now())
            {
                should_retry = true;
            }
        }
        if should_retry {
            return self.attempt_start(profile, kind, true);
        }
        self.status(profile, kind)
    }
}

fn recovery_delay(completed_attempts: u8) -> Duration {
    let multiplier = 1u64 << completed_attempts.min(4);
    Duration::from_millis(
        RECOVERY_BASE_DELAY_MS
            .saturating_mul(multiplier)
            .min(RECOVERY_MAX_DELAY_MS),
    )
}

fn recovery_message(service_label: &str, recovery: &RuntimeRecoveryDto) -> String {
    let next_attempt = recovery
        .attempt
        .saturating_add(1)
        .min(recovery.max_attempts);
    match recovery.retry_in_ms {
        Some(ms) => format!(
            "{}连接中断，{} 秒后进行第 {next_attempt}/{} 次自动恢复",
            service_label.trim(),
            ms.div_ceil(1_000).max(1),
            recovery.max_attempts
        ),
        None => format!(
            "{}连接中断，正在进行第 {next_attempt}/{} 次自动恢复",
            service_label.trim(),
            recovery.max_attempts
        ),
    }
}

fn should_mark_runtime_error(entry: &mut RuntimeEntry, listening: bool) -> bool {
    if entry.phase != RuntimePhase::Running {
        return false;
    }
    if listening {
        entry.missing_port_checks = 0;
        return false;
    }

    entry.missing_port_checks = entry.missing_port_checks.saturating_add(1);
    entry.missing_port_checks >= 3
        && entry
            .started_at
            .map(|started| started.elapsed() > Duration::from_millis(200))
            .unwrap_or(true)
}

fn port_for(profile: &WorkspaceProfile, kind: ServiceKind) -> u16 {
    match kind {
        ServiceKind::Mcp => profile.runtime.local_port,
        ServiceKind::Actions => profile.actions.local_port,
    }
}

fn endpoints(
    profile: &WorkspaceProfile,
    kind: ServiceKind,
    settings: &AppSettings,
) -> (String, String) {
    match kind {
        ServiceKind::Mcp => (
            profile.local_endpoint(),
            profile.public_endpoint_with(settings),
        ),
        ServiceKind::Actions => (
            profile.actions_local_base_url(),
            profile.actions_openapi_url_with(settings),
        ),
    }
}

fn public_message_for(
    profile: &WorkspaceProfile,
    kind: ServiceKind,
    settings: &AppSettings,
) -> String {
    match kind {
        ServiceKind::Mcp => profile.mcp_external_base_url_with(settings),
        ServiceKind::Actions => profile.actions_effective_public_url_with(settings),
    }
}

fn service_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "本地 MCP ",
        ServiceKind::Actions => "本地 Actions ",
    }
}

fn stderr_log_name(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "stderr.log",
        ServiceKind::Actions => "actions-stderr.log",
    }
}

/// Resolve a secret from the shared pool or per-workspace keyring.
fn resolve_secret(profile_id: &str, key: &str, use_shared: bool) -> AppResult<Option<String>> {
    if use_shared {
        SecretStore::get_shared(key)
    } else {
        SecretStore::get(profile_id, key)
    }
}

fn actions_oauth_secret(profile_id: &str, key: &str) -> AppResult<String> {
    match SecretStore::get(profile_id, key)? {
        Some(value) if !value.is_empty() => Ok(value),
        _ => SecretStore::regenerate(profile_id, key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(phase: RuntimePhase, started_at: Option<std::time::Instant>) -> RuntimeEntry {
        RuntimeEntry {
            phase,
            shutdown: None,
            handle: None,
            error_message: None,
            started_at,
            missing_port_checks: 0,
            desired_running: phase != RuntimePhase::Stopped,
            recovery_attempt: 0,
            next_retry_at: None,
            recovered_count: 0,
        }
    }

    #[test]
    fn refresh_does_not_cleanup_a_running_runtime_that_is_listening() {
        let mut runtime = entry(RuntimePhase::Running, Some(std::time::Instant::now()));
        assert!(!should_mark_runtime_error(&mut runtime, true));
    }

    #[test]
    fn refresh_does_not_cleanup_a_starting_runtime() {
        let mut runtime = entry(RuntimePhase::Starting, None);
        assert!(!should_mark_runtime_error(&mut runtime, false));
    }

    #[test]
    fn refresh_cleans_up_only_after_running_runtime_is_confirmed_missing() {
        let mut runtime = entry(
            RuntimePhase::Running,
            Some(std::time::Instant::now() - Duration::from_secs(1)),
        );
        assert!(!should_mark_runtime_error(&mut runtime, false));
        assert!(!should_mark_runtime_error(&mut runtime, false));
        assert!(should_mark_runtime_error(&mut runtime, false));
    }

    #[test]
    fn a_recovered_port_clears_missing_port_checks() {
        let mut runtime = entry(
            RuntimePhase::Running,
            Some(std::time::Instant::now() - Duration::from_secs(1)),
        );
        assert!(!should_mark_runtime_error(&mut runtime, false));
        assert!(!should_mark_runtime_error(&mut runtime, true));
        assert!(!should_mark_runtime_error(&mut runtime, false));
    }

    #[test]
    fn recovery_delay_uses_bounded_exponential_backoff() {
        assert_eq!(recovery_delay(0), Duration::from_secs(1));
        assert_eq!(recovery_delay(1), Duration::from_secs(2));
        assert_eq!(recovery_delay(2), Duration::from_secs(4));
        assert_eq!(recovery_delay(4), Duration::from_secs(16));
        assert_eq!(recovery_delay(8), Duration::from_secs(16));
    }

    #[test]
    fn exhausted_recovery_stops_automatic_retries() {
        let mut supervisor = RuntimeSupervisor::default();
        let key = ("workspace".to_string(), ServiceKind::Mcp);

        supervisor.record_recovery_failure(
            &key,
            MAX_RECOVERY_ATTEMPTS,
            2,
            "still unavailable".into(),
        );

        let entry = supervisor.entries.get(&key).expect("entry");
        assert_eq!(entry.phase, RuntimePhase::Error);
        assert!(!entry.desired_running);
        assert!(entry.next_retry_at.is_none());
        assert_eq!(entry.recovered_count, 2);
        assert!(entry
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("5 次"));
    }

    #[test]
    fn stalled_starting_entry_is_eligible_for_recovery() {
        let runtime = entry(
            RuntimePhase::Starting,
            Some(std::time::Instant::now() - STARTING_STALL_TIMEOUT - Duration::from_secs(1)),
        );
        assert!(starting_stalled(&runtime));
        assert!(!starting_stalled(&entry(
            RuntimePhase::Starting,
            Some(std::time::Instant::now()),
        )));
    }

    #[test]
    fn running_mcp_status_exposes_registered_activity() {
        let profile = WorkspaceProfile::new(".".into(), Some("activity".into()));
        let tracker = mcp::register_activity(&profile.id);
        let mut supervisor = RuntimeSupervisor::default();
        supervisor.entries.insert(
            (profile.id.clone(), ServiceKind::Mcp),
            entry(RuntimePhase::Running, Some(std::time::Instant::now())),
        );

        let settings = AppSettings::default();
        let idle = supervisor
            .mcp_status_with_settings(&profile, &settings)
            .activity
            .expect("activity");
        assert_eq!(idle.state, "idle");

        let request = tracker
            .request_started("session", &json!(1), "tools/call", "read_file")
            .expect("tracked request");
        let active = supervisor
            .mcp_status_with_settings(&profile, &settings)
            .activity
            .expect("activity");
        assert_eq!(active.state, "active");
        assert_eq!(active.current_tool, "read_file");

        drop(request);
        let recent = supervisor
            .mcp_status_with_settings(&profile, &settings)
            .activity
            .expect("activity");
        assert_eq!(recent.state, "recent");
    }
}
