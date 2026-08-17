use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(any(not(target_os = "linux"), test))]
use std::io::Read;

use serde::Serialize;
#[cfg(any(not(target_os = "linux"), test))]
use sha2::{Digest, Sha256};

use crate::build_identity::BuildIdentity;
use crate::control::{self, DaemonLaunchSpec};
use crate::daemon::{self, DaemonState};
use crate::error::{AppError, AppResult};
use crate::gateway_control::{self, GatewayOperation};
use crate::gateway_daemon::{self, GatewayDaemonState};
#[cfg(target_os = "linux")]
use crate::platform::platform;
use crate::workspace::WorkspaceProfile;

#[derive(Debug, Clone, Copy)]
pub struct RolloutOptions {
    pub timeout: Duration,
    pub force: bool,
    pub dry_run: bool,
    pub allow_no_rollback: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutStatus {
    Planned,
    Skipped,
    AlreadyCurrent,
    Upgraded,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutTargetKind {
    Workspace,
    Gateway,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutMode {
    ZeroDowntimeHandoff,
    BoundedOutage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRolloutResult {
    pub target_kind: RolloutTargetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    pub status: RolloutStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_build: Option<BuildIdentity>,
    pub current_build: BuildIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<RolloutMode>,
    pub handoff_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listener_ready_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drain_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_executable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_executable: Option<String>,
    pub rollback_available: bool,
    pub rollback_attempted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback_succeeded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outage_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback_failure: Option<String>,
}

impl RuntimeRolloutResult {
    pub fn is_success(&self) -> bool {
        matches!(
            self.status,
            RolloutStatus::Planned
                | RolloutStatus::Skipped
                | RolloutStatus::AlreadyCurrent
                | RolloutStatus::Upgraded
        )
    }
}

#[derive(Debug, Clone)]
struct RollbackExecutable {
    path: PathBuf,
    temporary: bool,
}

struct RolloutFailureContext {
    current_build: BuildIdentity,
    outage_started: Instant,
    failure: String,
    timeout: Duration,
}

impl RollbackExecutable {
    fn discard(&self) {
        if self.temporary {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub async fn rollout_workspace(
    profile: &WorkspaceProfile,
    options: RolloutOptions,
) -> AppResult<RuntimeRolloutResult> {
    let current_build = BuildIdentity::current();
    let inspection = daemon::inspect(profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let Some(previous) = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
    else {
        return Ok(workspace_result(
            profile,
            &current_build,
            RolloutStatus::Skipped,
            None,
        ));
    };
    if build_is_current(previous.build_identity.as_ref(), &current_build) {
        let mut result = workspace_result(
            profile,
            &current_build,
            RolloutStatus::AlreadyCurrent,
            Some(&previous),
        );
        result.pid = Some(previous.pid);
        result.active_executable = Some(previous.executable_path.clone());
        result.message = Some("Workspace daemon 已运行当前构建".into());
        return Ok(result);
    }

    let handoff_supported = control::request_version(&profile.id)
        .await
        .map(|version| version.supports_zero_downtime_handoff())
        .unwrap_or(false);
    let use_zero_downtime_handoff =
        cfg!(unix) && handoff_supported && previous.managed_tunnels().is_none();

    let rollback_available = rollback_source_available(previous.pid, &previous.executable_path)?;
    if !rollback_available && !options.allow_no_rollback {
        return Err(AppError::Message(format!(
            "Workspace {} 无法准备可信 rollback executable；拒绝停止旧 daemon。可显式使用 --allow-no-rollback 接受无自动回滚升级",
            profile.name
        )));
    }
    if options.dry_run {
        let mut result = workspace_result(
            profile,
            &current_build,
            RolloutStatus::Planned,
            Some(&previous),
        );
        result.pid = Some(previous.pid);
        result.active_executable = Some(previous.executable_path.clone());
        result.rollback_available = rollback_available;
        result.handoff_supported = handoff_supported;
        result.mode = Some(if use_zero_downtime_handoff {
            RolloutMode::ZeroDowntimeHandoff
        } else {
            RolloutMode::BoundedOutage
        });
        result.message = Some(if use_zero_downtime_handoff {
            "将通过继承 listener 的 generation handoff 切换当前构建；成功路径不解绑业务端口".into()
        } else if handoff_supported && previous.managed_tunnels().is_some() {
            "当前 daemon 支持 handoff，但受管 tunnel 尚未支持跨代所有权转移；将使用 bounded-outage rollback-safe 升级"
                .into()
        } else {
            "将排空旧 daemon，启动当前构建并验证 readiness/build identity".into()
        });
        return Ok(result);
    }

    let rollback = if rollback_available {
        Some(prepare_rollback_executable(
            previous.pid,
            &previous.executable_path,
            &format!("workspace-{}", profile.id),
        )?)
    } else {
        None
    };
    let spec = DaemonLaunchSpec {
        service: previous.service,
        tunnels: previous.managed_tunnels(),
    };

    #[cfg(unix)]
    if use_zero_downtime_handoff {
        return rollout_workspace_handoff(
            profile,
            &previous,
            spec,
            rollback,
            current_build,
            options,
        )
        .await;
    }

    if let Err(error) = control::request_daemon_exit_and_wait(
        profile,
        control::ControlOperation::Restart,
        options.timeout,
        options.force,
    )
    .await
    {
        if let Some(rollback) = rollback.as_ref() {
            rollback.discard();
        }
        return Err(error);
    }

    let outage_started = Instant::now();
    let current_executable = std::env::current_exe()?;
    match start_workspace_generation(profile, spec, &current_executable, options.timeout).await {
        Ok(next) if build_is_current(next.build_identity.as_ref(), &current_build) => {
            if let Some(rollback) = rollback.as_ref() {
                rollback.discard();
            }
            let mut result = workspace_result(
                profile,
                &current_build,
                RolloutStatus::Upgraded,
                Some(&previous),
            );
            result.pid = Some(next.pid);
            result.active_executable = Some(next.executable_path);
            result.rollback_available = rollback.is_some();
            result.mode = Some(RolloutMode::BoundedOutage);
            result.handoff_supported = handoff_supported;
            result.outage_ms = Some(duration_ms(outage_started.elapsed()));
            result.message = Some("Workspace daemon 已切换到当前构建".into());
            Ok(result)
        }
        Ok(next) => {
            let failure = format!(
                "新 Workspace daemon PID {} readiness 通过，但 build identity 不是当前 CLI 构建",
                next.pid
            );
            let _ = daemon::terminate_spawned(profile, next.pid).await;
            let mut result = rollback_workspace(
                profile,
                &previous,
                spec,
                rollback,
                RolloutFailureContext {
                    current_build,
                    outage_started,
                    failure,
                    timeout: options.timeout,
                },
            )
            .await?;
            result.mode = Some(RolloutMode::BoundedOutage);
            result.handoff_supported = handoff_supported;
            Ok(result)
        }
        Err(error) => {
            let mut result = rollback_workspace(
                profile,
                &previous,
                spec,
                rollback,
                RolloutFailureContext {
                    current_build,
                    outage_started,
                    failure: error.to_string(),
                    timeout: options.timeout,
                },
            )
            .await?;
            result.mode = Some(RolloutMode::BoundedOutage);
            result.handoff_supported = handoff_supported;
            Ok(result)
        }
    }
}

#[cfg(unix)]
async fn rollout_workspace_handoff(
    profile: &WorkspaceProfile,
    previous: &DaemonState,
    spec: DaemonLaunchSpec,
    rollback: Option<RollbackExecutable>,
    current_build: BuildIdentity,
    options: RolloutOptions,
) -> AppResult<RuntimeRolloutResult> {
    let current_executable = std::env::current_exe()?;
    let handoff_started = Instant::now();
    let (handoff_id, accepted_pid) = match control::request_daemon_handoff(
        &profile.id,
        current_executable.display().to_string(),
        current_build.clone(),
    )
    .await
    {
        Ok(accepted) => accepted,
        Err(error) => {
            if let Some(rollback) = rollback.as_ref() {
                rollback.discard();
            }
            return Err(AppError::Message(format!(
                "Workspace handoff request failed before cutover: {error}"
            )));
        }
    };
    if accepted_pid != previous.pid {
        if let Some(rollback) = rollback.as_ref() {
            rollback.discard();
        }
        return Err(AppError::Message(format!(
            "Workspace handoff PID mismatch: state={} response={accepted_pid}",
            previous.pid
        )));
    }

    let deadline = tokio::time::Instant::now() + options.timeout;
    loop {
        let state = daemon::read_handoff_state(&profile.id, &handoff_id)?;
        if let Some(state) = state.as_ref() {
            if state.predecessor_pid != previous.pid {
                if let Some(rollback) = rollback.as_ref() {
                    rollback.discard();
                }
                daemon::remove_handoff_state(&profile.id, &handoff_id);
                return Err(AppError::Message(format!(
                    "Workspace handoff predecessor mismatch: expected {} state {}",
                    previous.pid, state.predecessor_pid
                )));
            }
            match state.stage {
                daemon::DaemonHandoffStage::CanonicalReady => {
                    let successor_pid = state.successor_pid.ok_or_else(|| {
                        AppError::Message("canonical handoff state is missing successor PID".into())
                    })?;
                    let listener_ready_ms = duration_ms(handoff_started.elapsed());
                    let remaining = deadline
                        .checked_duration_since(tokio::time::Instant::now())
                        .unwrap_or(Duration::from_millis(1));
                    match verify_canonical_handoff_successor(
                        profile,
                        successor_pid,
                        &current_build,
                        remaining,
                    )
                    .await
                    {
                        Ok(next)
                            if build_is_current(next.build_identity.as_ref(), &current_build) =>
                        {
                            if let Some(rollback) = rollback.as_ref() {
                                rollback.discard();
                            }
                            let mut result = workspace_result(
                                profile,
                                &current_build,
                                RolloutStatus::Upgraded,
                                Some(previous),
                            );
                            result.pid = Some(next.pid);
                            result.active_executable = Some(next.executable_path);
                            result.rollback_available = rollback.is_some();
                            result.mode = Some(RolloutMode::ZeroDowntimeHandoff);
                            result.handoff_supported = true;
                            result.handoff_id = Some(handoff_id.clone());
                            result.listener_ready_ms = Some(listener_ready_ms);
                            // The predecessor may still be finishing the request that invoked
                            // `anchor upgrade`. Waiting for its PID here would deadlock that
                            // self-upgrade request. Canonical successor readiness is sufficient
                            // for the CLI to return; the predecessor drains independently.
                            result.drain_ms = None;
                            result.outage_ms = Some(0);
                            result.message = Some(
                                "Workspace daemon 已通过继承 listener 的 generation handoff 切换到当前构建"
                                    .into(),
                            );
                            daemon::remove_handoff_state(&profile.id, &handoff_id);
                            return Ok(result);
                        }
                        Ok(next) => {
                            let failure = format!(
                                "handoff successor PID {} readiness 通过，但 build identity 不是当前 CLI 构建",
                                next.pid
                            );
                            let _ = daemon::terminate_spawned(profile, next.pid).await;
                            return rollback_workspace_after_handoff(
                                profile,
                                previous,
                                spec,
                                rollback,
                                current_build,
                                handoff_started,
                                options.timeout,
                                handoff_id,
                                failure,
                            )
                            .await;
                        }
                        Err(error) => {
                            let _ = daemon::terminate_spawned(profile, successor_pid).await;
                            return rollback_workspace_after_handoff(
                                profile,
                                previous,
                                spec,
                                rollback,
                                current_build,
                                handoff_started,
                                options.timeout,
                                handoff_id,
                                error.to_string(),
                            )
                            .await;
                        }
                    }
                }
                daemon::DaemonHandoffStage::Failed => {
                    let failure = state
                        .failure
                        .clone()
                        .unwrap_or_else(|| "Workspace handoff failed".into());
                    if !state.cutover_started()
                        && predecessor_is_still_canonical(profile, previous)?
                    {
                        if let Some(successor_pid) = state.successor_pid {
                            let _ = daemon::terminate_spawned(profile, successor_pid).await;
                        }
                        if let Some(rollback) = rollback.as_ref() {
                            rollback.discard();
                        }
                        let mut result = workspace_result(
                            profile,
                            &current_build,
                            RolloutStatus::Failed,
                            Some(previous),
                        );
                        result.pid = Some(previous.pid);
                        result.active_executable = Some(previous.executable_path.clone());
                        result.rollback_available = rollback.is_some();
                        result.mode = Some(RolloutMode::ZeroDowntimeHandoff);
                        result.handoff_supported = true;
                        result.handoff_id = Some(handoff_id.clone());
                        result.outage_ms = Some(0);
                        result.failure = Some(failure);
                        result.message = Some(
                            "handoff 在 cutover 前失败；旧 Workspace daemon 保持运行，未发生 listener outage"
                                .into(),
                        );
                        daemon::remove_handoff_state(&profile.id, &handoff_id);
                        return Ok(result);
                    }
                    return rollback_workspace_after_handoff(
                        profile,
                        previous,
                        spec,
                        rollback,
                        current_build,
                        handoff_started,
                        options.timeout,
                        handoff_id,
                        failure,
                    )
                    .await;
                }
                daemon::DaemonHandoffStage::Requested
                | daemon::DaemonHandoffStage::SuccessorPrepared
                | daemon::DaemonHandoffStage::OwnershipReleased => {}
            }
        }

        if tokio::time::Instant::now() >= deadline {
            let cutover_started = state.as_ref().is_some_and(|state| state.cutover_started());
            if !cutover_started && predecessor_is_still_canonical(profile, previous)? {
                if let Some(successor_pid) = state.as_ref().and_then(|state| state.successor_pid) {
                    let _ = daemon::terminate_spawned(profile, successor_pid).await;
                }
                if let Some(rollback) = rollback.as_ref() {
                    rollback.discard();
                }
                let mut result = workspace_result(
                    profile,
                    &current_build,
                    RolloutStatus::Failed,
                    Some(previous),
                );
                result.pid = Some(previous.pid);
                result.active_executable = Some(previous.executable_path.clone());
                result.rollback_available = rollback.is_some();
                result.mode = Some(RolloutMode::ZeroDowntimeHandoff);
                result.handoff_supported = true;
                result.handoff_id = Some(handoff_id.clone());
                result.outage_ms = Some(0);
                result.failure = Some("Workspace handoff timed out before cutover".into());
                result.message = Some(
                    "handoff 超时但旧 Workspace daemon 仍保持 canonical；未发生 listener outage"
                        .into(),
                );
                daemon::remove_handoff_state(&profile.id, &handoff_id);
                return Ok(result);
            }
            return rollback_workspace_after_handoff(
                profile,
                previous,
                spec,
                rollback,
                current_build,
                handoff_started,
                options.timeout,
                handoff_id,
                "Workspace handoff timed out after cutover started".into(),
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
async fn verify_canonical_handoff_successor(
    profile: &WorkspaceProfile,
    successor_pid: u32,
    current_build: &BuildIdentity,
    timeout: Duration,
) -> AppResult<DaemonState> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_detail = "successor state/control not ready".to_string();
    loop {
        let inspection = daemon::inspect(profile)?;
        if inspection.running && inspection.pid_matches {
            if let Some(state) = inspection.state {
                if state.pid == successor_pid {
                    if !build_is_current(state.build_identity.as_ref(), current_build) {
                        return Err(AppError::Message(format!(
                            "handoff successor PID {successor_pid} published a non-current build identity"
                        )));
                    }
                    match control::request_version(&profile.id).await {
                        Ok(version) => {
                            if build_is_current(version.build_identity.as_ref(), current_build) {
                                return Ok(state);
                            }
                            return Err(AppError::Message(format!(
                                "handoff successor PID {successor_pid} control endpoint reported a non-current build identity"
                            )));
                        }
                        Err(error) => {
                            last_detail =
                                format!("successor control endpoint is not ready: {error}");
                        }
                    }
                } else {
                    last_detail = format!(
                        "canonical daemon state still reports PID {} instead of successor {successor_pid}",
                        state.pid
                    );
                }
            }
        } else {
            last_detail = inspection.detail;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Message(format!(
                "handoff successor PID {successor_pid} did not become verifiably canonical before timeout: {last_detail}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
fn predecessor_is_still_canonical(
    profile: &WorkspaceProfile,
    previous: &DaemonState,
) -> AppResult<bool> {
    let inspection = daemon::inspect(profile)?;
    Ok(inspection.running
        && inspection.pid_matches
        && inspection
            .state
            .as_ref()
            .is_some_and(|state| state.pid == previous.pid))
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn rollback_workspace_after_handoff(
    profile: &WorkspaceProfile,
    previous: &DaemonState,
    spec: DaemonLaunchSpec,
    rollback: Option<RollbackExecutable>,
    current_build: BuildIdentity,
    outage_started: Instant,
    timeout: Duration,
    handoff_id: String,
    failure: String,
) -> AppResult<RuntimeRolloutResult> {
    if let Some(state) = daemon::read_handoff_state(&profile.id, &handoff_id)? {
        if let Some(successor_pid) = state.successor_pid {
            if platform().is_process_alive(successor_pid) {
                let _ = daemon::terminate_spawned(profile, successor_pid).await;
            }
        }
    }
    if platform().is_process_alive(previous.pid) {
        let _ = daemon::terminate_spawned(profile, previous.pid).await;
    }
    let mut result = rollback_workspace(
        profile,
        previous,
        spec,
        rollback,
        RolloutFailureContext {
            current_build,
            outage_started,
            failure,
            timeout,
        },
    )
    .await?;
    result.mode = Some(RolloutMode::ZeroDowntimeHandoff);
    result.handoff_supported = true;
    result.handoff_id = Some(handoff_id.clone());
    daemon::remove_handoff_state(&profile.id, &handoff_id);
    Ok(result)
}

pub async fn rollout_gateway(options: RolloutOptions) -> AppResult<RuntimeRolloutResult> {
    let current_build = BuildIdentity::current();
    let inspection = gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let Some(previous) = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
    else {
        return Ok(gateway_result(&current_build, RolloutStatus::Skipped, None));
    };
    if build_is_current(previous.build_identity.as_ref(), &current_build) {
        let mut result = gateway_result(
            &current_build,
            RolloutStatus::AlreadyCurrent,
            Some(&previous),
        );
        result.pid = Some(previous.pid);
        result.active_executable = Some(previous.executable_path.clone());
        result.message = Some("Gateway daemon 已运行当前构建".into());
        return Ok(result);
    }

    let rollback_available = rollback_source_available(previous.pid, &previous.executable_path)?;
    if !rollback_available && !options.allow_no_rollback {
        return Err(AppError::Message(
            "Gateway daemon 无法准备可信 rollback executable；拒绝停止旧 daemon。可显式使用 --allow-no-rollback 接受无自动回滚升级"
                .into(),
        ));
    }
    if options.dry_run {
        let mut result = gateway_result(&current_build, RolloutStatus::Planned, Some(&previous));
        result.pid = Some(previous.pid);
        result.active_executable = Some(previous.executable_path.clone());
        result.rollback_available = rollback_available;
        result.message =
            Some("将排空旧 Gateway，启动当前构建并验证 readiness/build identity".into());
        return Ok(result);
    }

    let rollback = if rollback_available {
        Some(prepare_rollback_executable(
            previous.pid,
            &previous.executable_path,
            "gateway",
        )?)
    } else {
        None
    };
    let accepted_pid = match gateway_control::request_exit(GatewayOperation::Restart).await {
        Ok(pid) => pid,
        Err(error) => {
            if let Some(rollback) = rollback.as_ref() {
                rollback.discard();
            }
            return Err(AppError::Message(error.to_string()));
        }
    };
    if accepted_pid != previous.pid {
        if let Some(rollback) = rollback.as_ref() {
            rollback.discard();
        }
        return Err(AppError::Message(format!(
            "Gateway restart PID mismatch: state={}, response={accepted_pid}",
            previous.pid
        )));
    }
    if let Err(error) =
        gateway_daemon::wait_for_exit(previous.pid, options.timeout, options.force).await
    {
        if let Some(rollback) = rollback.as_ref() {
            rollback.discard();
        }
        return Err(error);
    }

    let outage_started = Instant::now();
    let current_executable = std::env::current_exe()?;
    match start_gateway_generation(
        &previous.workspace_ids,
        &current_executable,
        options.timeout,
    )
    .await
    {
        Ok(next) if build_is_current(next.build_identity.as_ref(), &current_build) => {
            if let Some(rollback) = rollback.as_ref() {
                rollback.discard();
            }
            let mut result =
                gateway_result(&current_build, RolloutStatus::Upgraded, Some(&previous));
            result.pid = Some(next.pid);
            result.active_executable = Some(next.executable_path);
            result.rollback_available = rollback.is_some();
            result.outage_ms = Some(duration_ms(outage_started.elapsed()));
            result.message = Some("Gateway daemon 已切换到当前构建".into());
            Ok(result)
        }
        Ok(next) => {
            let failure = format!(
                "新 Gateway daemon PID {} readiness 通过，但 build identity 不是当前 CLI 构建",
                next.pid
            );
            let _ = gateway_daemon::terminate_spawned(next.pid).await;
            rollback_gateway(
                &previous,
                rollback,
                current_build,
                outage_started,
                failure,
                options.timeout,
            )
            .await
        }
        Err(error) => {
            rollback_gateway(
                &previous,
                rollback,
                current_build,
                outage_started,
                error.to_string(),
                options.timeout,
            )
            .await
        }
    }
}

async fn start_workspace_generation(
    profile: &WorkspaceProfile,
    spec: DaemonLaunchSpec,
    executable: &Path,
    timeout: Duration,
) -> AppResult<DaemonState> {
    let pid = daemon::spawn_with_tunnels_from_executable(
        profile,
        spec.service,
        spec.tunnels,
        executable,
    )?;
    match daemon::wait_ready(profile, spec.service, pid, timeout).await {
        Ok(state) => Ok(state),
        Err(error) => {
            let cleanup_error = daemon::terminate_spawned(profile, pid).await.err();
            Err(AppError::Message(format!(
                "{error}{}",
                cleanup_error
                    .map(|cleanup| format!("；清理失败：{cleanup}"))
                    .unwrap_or_default()
            )))
        }
    }
}

async fn start_gateway_generation(
    workspace_ids: &[String],
    executable: &Path,
    timeout: Duration,
) -> AppResult<GatewayDaemonState> {
    let pid = gateway_daemon::spawn_from_executable(workspace_ids, executable)?;
    match gateway_daemon::wait_ready(pid, timeout).await {
        Ok(state) => Ok(state),
        Err(error) => {
            let cleanup_error = gateway_daemon::terminate_spawned(pid).await.err();
            Err(AppError::Message(format!(
                "{error}{}",
                cleanup_error
                    .map(|cleanup| format!("；清理失败：{cleanup}"))
                    .unwrap_or_default()
            )))
        }
    }
}

async fn rollback_workspace(
    profile: &WorkspaceProfile,
    previous: &DaemonState,
    spec: DaemonLaunchSpec,
    rollback: Option<RollbackExecutable>,
    context: RolloutFailureContext,
) -> AppResult<RuntimeRolloutResult> {
    let Some(rollback) = rollback else {
        let mut result = workspace_result(
            profile,
            &context.current_build,
            RolloutStatus::Failed,
            Some(previous),
        );
        result.failure = Some(context.failure);
        result.outage_ms = Some(duration_ms(context.outage_started.elapsed()));
        return Ok(result);
    };
    match start_workspace_generation(profile, spec, &rollback.path, context.timeout).await {
        Ok(state) if rollback_matches_previous(state.build_identity.as_ref(), previous) => {
            let mut result = workspace_result(
                profile,
                &context.current_build,
                RolloutStatus::RolledBack,
                Some(previous),
            );
            result.pid = Some(state.pid);
            result.active_executable = Some(state.executable_path);
            result.rollback_available = true;
            result.rollback_attempted = true;
            result.rollback_succeeded = Some(true);
            result.outage_ms = Some(duration_ms(context.outage_started.elapsed()));
            result.failure = Some(context.failure);
            result.message = Some("当前构建启动失败，已恢复旧 Workspace daemon".into());
            Ok(result)
        }
        Ok(state) => {
            let rollback_failure = format!(
                "rollback Workspace daemon PID {} readiness 通过，但 build identity 与升级前不一致",
                state.pid
            );
            let _ = daemon::terminate_spawned(profile, state.pid).await;
            Ok(failed_workspace_result(
                profile,
                previous,
                context.current_build,
                context.outage_started,
                context.failure,
                rollback_failure,
            ))
        }
        Err(error) => Ok(failed_workspace_result(
            profile,
            previous,
            context.current_build,
            context.outage_started,
            context.failure,
            error.to_string(),
        )),
    }
}

async fn rollback_gateway(
    previous: &GatewayDaemonState,
    rollback: Option<RollbackExecutable>,
    current_build: BuildIdentity,
    outage_started: Instant,
    failure: String,
    timeout: Duration,
) -> AppResult<RuntimeRolloutResult> {
    let Some(rollback) = rollback else {
        let mut result = gateway_result(&current_build, RolloutStatus::Failed, Some(previous));
        result.failure = Some(failure);
        result.outage_ms = Some(duration_ms(outage_started.elapsed()));
        return Ok(result);
    };
    match start_gateway_generation(&previous.workspace_ids, &rollback.path, timeout).await {
        Ok(state) if gateway_rollback_matches_previous(state.build_identity.as_ref(), previous) => {
            let mut result =
                gateway_result(&current_build, RolloutStatus::RolledBack, Some(previous));
            result.pid = Some(state.pid);
            result.active_executable = Some(state.executable_path);
            result.rollback_available = true;
            result.rollback_attempted = true;
            result.rollback_succeeded = Some(true);
            result.outage_ms = Some(duration_ms(outage_started.elapsed()));
            result.failure = Some(failure);
            result.message = Some("当前构建启动失败，已恢复旧 Gateway daemon".into());
            Ok(result)
        }
        Ok(state) => {
            let rollback_failure = format!(
                "rollback Gateway daemon PID {} readiness 通过，但 build identity 与升级前不一致",
                state.pid
            );
            let _ = gateway_daemon::terminate_spawned(state.pid).await;
            Ok(failed_gateway_result(
                previous,
                current_build,
                outage_started,
                failure,
                rollback_failure,
            ))
        }
        Err(error) => Ok(failed_gateway_result(
            previous,
            current_build,
            outage_started,
            failure,
            error.to_string(),
        )),
    }
}

fn failed_workspace_result(
    profile: &WorkspaceProfile,
    previous: &DaemonState,
    current_build: BuildIdentity,
    outage_started: Instant,
    failure: String,
    rollback_failure: String,
) -> RuntimeRolloutResult {
    let mut result = workspace_result(
        profile,
        &current_build,
        RolloutStatus::Failed,
        Some(previous),
    );
    result.rollback_available = true;
    result.rollback_attempted = true;
    result.rollback_succeeded = Some(false);
    result.outage_ms = Some(duration_ms(outage_started.elapsed()));
    result.failure = Some(failure);
    result.rollback_failure = Some(rollback_failure);
    result
}

fn failed_gateway_result(
    previous: &GatewayDaemonState,
    current_build: BuildIdentity,
    outage_started: Instant,
    failure: String,
    rollback_failure: String,
) -> RuntimeRolloutResult {
    let mut result = gateway_result(&current_build, RolloutStatus::Failed, Some(previous));
    result.rollback_available = true;
    result.rollback_attempted = true;
    result.rollback_succeeded = Some(false);
    result.outage_ms = Some(duration_ms(outage_started.elapsed()));
    result.failure = Some(failure);
    result.rollback_failure = Some(rollback_failure);
    result
}

fn workspace_result(
    profile: &WorkspaceProfile,
    current_build: &BuildIdentity,
    status: RolloutStatus,
    previous: Option<&DaemonState>,
) -> RuntimeRolloutResult {
    RuntimeRolloutResult {
        target_kind: RolloutTargetKind::Workspace,
        workspace_id: Some(profile.id.clone()),
        workspace_name: Some(profile.name.clone()),
        status,
        previous_pid: previous.map(|state| state.pid),
        pid: None,
        previous_build: previous.and_then(|state| state.build_identity.clone()),
        current_build: current_build.clone(),
        mode: None,
        handoff_supported: false,
        handoff_id: None,
        listener_ready_ms: None,
        drain_ms: None,
        previous_executable: previous.map(|state| state.executable_path.clone()),
        active_executable: None,
        rollback_available: false,
        rollback_attempted: false,
        rollback_succeeded: None,
        outage_ms: None,
        message: None,
        failure: None,
        rollback_failure: None,
    }
}

fn gateway_result(
    current_build: &BuildIdentity,
    status: RolloutStatus,
    previous: Option<&GatewayDaemonState>,
) -> RuntimeRolloutResult {
    RuntimeRolloutResult {
        target_kind: RolloutTargetKind::Gateway,
        workspace_id: None,
        workspace_name: None,
        status,
        previous_pid: previous.map(|state| state.pid),
        pid: None,
        previous_build: previous.and_then(|state| state.build_identity.clone()),
        current_build: current_build.clone(),
        mode: None,
        handoff_supported: false,
        handoff_id: None,
        listener_ready_ms: None,
        drain_ms: None,
        previous_executable: previous.map(|state| state.executable_path.clone()),
        active_executable: None,
        rollback_available: false,
        rollback_attempted: false,
        rollback_succeeded: None,
        outage_ms: None,
        message: None,
        failure: None,
        rollback_failure: None,
    }
}

fn build_is_current(identity: Option<&BuildIdentity>, current: &BuildIdentity) -> bool {
    identity.is_some_and(|identity| identity.same_build(current))
}

fn rollback_matches_previous(identity: Option<&BuildIdentity>, previous: &DaemonState) -> bool {
    previous
        .build_identity
        .as_ref()
        .is_none_or(|expected| identity.is_some_and(|actual| actual.same_build(expected)))
}

fn gateway_rollback_matches_previous(
    identity: Option<&BuildIdentity>,
    previous: &GatewayDaemonState,
) -> bool {
    previous
        .build_identity
        .as_ref()
        .is_none_or(|expected| identity.is_some_and(|actual| actual.same_build(expected)))
}

fn rollback_source_available(pid: u32, recorded_path: &str) -> AppResult<bool> {
    #[cfg(target_os = "linux")]
    {
        let _ = recorded_path;
        Ok(platform().is_process_alive(pid) && fs::File::open(format!("/proc/{pid}/exe")).is_ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        let recorded = PathBuf::from(recorded_path);
        if !recorded.is_file() {
            return Ok(false);
        }
        let current = std::env::current_exe()?;
        Ok(!same_executable_bytes(&recorded, &current)?)
    }
}

fn prepare_rollback_executable(
    pid: u32,
    recorded_path: &str,
    scope: &str,
) -> AppResult<RollbackExecutable> {
    #[cfg(target_os = "linux")]
    {
        let _ = recorded_path;
        let directory = daemon::runtime_dir()?.join("upgrade-rollback");
        ensure_private_directory(&directory)?;
        let path = directory.join(format!("{}-{pid}", sanitize_component(scope)));
        let source = PathBuf::from(format!("/proc/{pid}/exe"));
        fs::copy(&source, &path).map_err(|error| {
            AppError::Message(format!(
                "无法从 {} 保存旧 daemon rollback 映像到 {}：{error}",
                source.display(),
                path.display()
            ))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(RollbackExecutable {
            path,
            temporary: true,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, scope);
        let path = PathBuf::from(recorded_path);
        if !path.is_file() {
            return Err(AppError::Message(format!(
                "rollback executable 不存在：{}",
                path.display()
            )));
        }
        let current = std::env::current_exe()?;
        if same_executable_bytes(&path, &current)? {
            return Err(AppError::Message(
                "旧 daemon executable 与当前 CLI 指向相同二进制，无法证明可回滚到旧构建".into(),
            ));
        }
        Ok(RollbackExecutable {
            path,
            temporary: false,
        })
    }
}

#[cfg(any(not(target_os = "linux"), test))]
fn same_executable_bytes(left: &Path, right: &Path) -> AppResult<bool> {
    if left.canonicalize().ok() == right.canonicalize().ok() {
        return Ok(true);
    }
    Ok(file_sha256(left)? == file_sha256(right)?)
}

#[cfg(any(not(target_os = "linux"), test))]
fn file_sha256(path: &Path) -> AppResult<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}

#[cfg(target_os = "linux")]
fn ensure_private_directory(path: &Path) -> AppResult<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn sanitize_component(value: &str) -> String {
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

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_build_detection_uses_full_embedded_identity() {
        let current = BuildIdentity::current();
        assert!(build_is_current(Some(&current), &current));

        let mut previous = current.clone();
        previous.git_sha = "different".into();
        assert!(!build_is_current(Some(&previous), &current));
        assert!(!build_is_current(None, &current));
    }

    #[test]
    fn rollback_scope_is_safe_for_runtime_file_names() {
        assert_eq!(sanitize_component("workspace/a:b"), "workspace_a_b");
        assert_eq!(sanitize_component("gateway-1"), "gateway-1");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_can_snapshot_the_actual_running_executable() {
        let scope = format!("rollout-test-{}", uuid::Uuid::new_v4());
        let rollback = prepare_rollback_executable(std::process::id(), "", &scope)
            .expect("snapshot running executable");
        assert!(rollback.path.is_file());
        assert!(same_executable_bytes(&rollback.path, &std::env::current_exe().unwrap()).unwrap());
        rollback.discard();
        assert!(!rollback.path.exists());
    }
}
