use std::path::Path;
use std::time::Duration;

use crate::build_identity::BuildIdentity;
use crate::daemon::{self, DaemonHandoffStage, ServiceSelection};
use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::runtime::{HandoffListener, RuntimeSupervisor, ServiceKind};
use crate::workspace::WorkspaceProfile;

use super::args::DaemonHandoffOptions;

const HANDOFF_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CHILD_ACTIVATION_GRACE: Duration = Duration::from_millis(25);
const SUCCESSOR_PREPARE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct PreparedSuccessor {
    pub(crate) handoff_id: String,
    pub(crate) successor_pid: u32,
}

pub(crate) async fn acquire_successor_ownership(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    timeout: Duration,
) -> AppResult<daemon::DaemonGuard> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match daemon::acquire_with_tunnels(profile, service, None) {
            Ok(guard) => return Ok(guard),
            Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
            Err(_) => {}
        }
        tokio::time::sleep(HANDOFF_POLL_INTERVAL).await;
    }
}

pub(crate) struct ImportedListeners {
    pub(crate) mcp: Option<HandoffListener>,
    pub(crate) mcp_snapshot: Option<crate::mcp::McpHandoffSnapshot>,
    pub(crate) actions: Option<HandoffListener>,
}

pub(crate) async fn prepare_successor(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    handoff_id: &str,
    executable: &Path,
    expected_build: BuildIdentity,
    initiator_pid: u32,
    runtime: &RuntimeSupervisor,
) -> AppResult<PreparedSuccessor> {
    let timeout = SUCCESSOR_PREPARE_TIMEOUT;
    daemon::create_handoff_state(
        profile,
        handoff_id,
        service,
        initiator_pid,
        expected_build,
        None,
        executable,
    )?;
    let (mcp_listener, mcp_snapshot) = if service.includes_mcp() {
        match runtime.duplicate_listener_for_handoff(&profile.id, ServiceKind::Mcp, initiator_pid) {
            Ok((listener, snapshot)) => (Some(listener), snapshot),
            Err(error) => {
                mark_failed(profile, handoff_id, &error.to_string());
                return Err(error);
            }
        }
    } else {
        (None, None)
    };
    let actions_listener = if service.includes_actions() {
        match runtime.duplicate_listener_for_handoff(
            &profile.id,
            ServiceKind::Actions,
            initiator_pid,
        ) {
            Ok((listener, _)) => Some(listener),
            Err(error) => {
                mark_failed(profile, handoff_id, &error.to_string());
                return Err(error);
            }
        }
    } else {
        None
    };
    let mut state = daemon::read_handoff_state(&profile.id, handoff_id)?
        .ok_or_else(|| AppError::Message("daemon handoff state disappeared before spawn".into()))?;
    state.mcp_snapshot = mcp_snapshot.clone();
    daemon::write_handoff_state(&state)?;
    let successor_pid = match daemon::spawn_handoff_successor(
        profile,
        service,
        executable,
        handoff_id,
        std::process::id(),
        mcp_listener.as_ref(),
        actions_listener.as_ref(),
    ) {
        Ok(pid) => pid,
        Err(error) => {
            mark_failed(profile, handoff_id, &error.to_string());
            return Err(error);
        }
    };
    // Parent copies can close immediately after spawn; the successor inherited
    // its own descriptors and the predecessor still owns its retained copies.
    drop(mcp_listener);
    drop(actions_listener);

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let state = daemon::read_handoff_state(&profile.id, handoff_id)?.ok_or_else(|| {
            AppError::Message(format!("daemon handoff state disappeared: {handoff_id}"))
        })?;
        match state.stage {
            DaemonHandoffStage::SuccessorPrepared if state.successor_pid == Some(successor_pid) => {
                if let Some(expected) = mcp_snapshot.as_ref() {
                    if let Err(error) =
                        runtime.validate_mcp_handoff_snapshot(&profile.id, initiator_pid, expected)
                    {
                        mark_failed(profile, handoff_id, &error.to_string());
                        let _ = platform().terminate_process_tree(successor_pid);
                        return Err(error);
                    }
                }
                return Ok(PreparedSuccessor {
                    handoff_id: handoff_id.to_string(),
                    successor_pid,
                });
            }
            DaemonHandoffStage::Failed => {
                return Err(AppError::Message(
                    state
                        .failure
                        .unwrap_or_else(|| "handoff successor preparation failed".into()),
                ));
            }
            DaemonHandoffStage::Requested | DaemonHandoffStage::SuccessorPrepared => {}
            other => {
                return Err(AppError::Message(format!(
                    "unexpected handoff stage before cutover: {other:?}"
                )))
            }
        }
        if !platform().is_process_alive(successor_pid) {
            let message =
                format!("handoff successor PID {successor_pid} exited before listener preparation");
            mark_failed(profile, handoff_id, &message);
            return Err(AppError::Message(message));
        }
        if tokio::time::Instant::now() >= deadline {
            let message =
                format!("handoff successor PID {successor_pid} did not prepare before timeout");
            let _ = platform().terminate_process_tree(successor_pid);
            mark_failed(profile, handoff_id, &message);
            return Err(AppError::Message(message));
        }
        tokio::time::sleep(HANDOFF_POLL_INTERVAL).await;
    }
}

pub(crate) async fn prepare_child(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    options: &DaemonHandoffOptions,
    timeout: Duration,
) -> AppResult<ImportedListeners> {
    let result = prepare_child_inner(profile, service, options, timeout).await;
    if let Err(error) = &result {
        mark_failed(profile, &options.handoff_id, &error.to_string());
    }
    result
}

async fn prepare_child_inner(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    options: &DaemonHandoffOptions,
    timeout: Duration,
) -> AppResult<ImportedListeners> {
    let mut state = daemon::read_handoff_state(&profile.id, &options.handoff_id)?
        .ok_or_else(|| AppError::Message("daemon handoff state is missing".into()))?;
    if state.stage != DaemonHandoffStage::Requested {
        return Err(AppError::Message(format!(
            "daemon handoff child expected requested stage, got {:?}",
            state.stage
        )));
    }
    if state.predecessor_pid != options.predecessor_pid {
        return Err(AppError::Message(format!(
            "daemon handoff predecessor PID mismatch: state={} args={}",
            state.predecessor_pid, options.predecessor_pid
        )));
    }
    if state.service != service {
        return Err(AppError::Message(format!(
            "daemon handoff service mismatch: state={} args={}",
            state.service.as_str(),
            service.as_str()
        )));
    }
    let current_build = BuildIdentity::current();
    if !state.expected_build.same_build(&current_build) {
        return Err(AppError::Message(format!(
            "handoff successor build mismatch: expected {} current {}",
            state.expected_build.short_git_sha(),
            current_build.short_git_sha()
        )));
    }
    let current_executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let expected_executable = std::fs::canonicalize(&state.target_executable)?;
    if current_executable != expected_executable {
        return Err(AppError::Message(format!(
            "handoff successor executable mismatch: expected {} current {}",
            expected_executable.display(),
            current_executable.display()
        )));
    }

    let mcp = match options.mcp_fd {
        Some(fd) => Some(unsafe { HandoffListener::from_inherited_fd(fd) }?),
        None => None,
    };
    let mcp_snapshot = state.mcp_snapshot.clone();
    if service.includes_mcp() && mcp_snapshot.is_none() {
        return Err(AppError::Message(
            "daemon handoff is missing the MCP state snapshot".into(),
        ));
    }
    let actions = match options.actions_fd {
        Some(fd) => Some(unsafe { HandoffListener::from_inherited_fd(fd) }?),
        None => None,
    };
    state.successor_pid = Some(std::process::id());
    state.stage = DaemonHandoffStage::SuccessorPrepared;
    state.failure = None;
    daemon::write_handoff_state(&state)?;

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let state =
            daemon::read_handoff_state(&profile.id, &options.handoff_id)?.ok_or_else(|| {
                AppError::Message(
                    "daemon handoff state disappeared while waiting for cutover".into(),
                )
            })?;
        match state.stage {
            DaemonHandoffStage::OwnershipReleased => break,
            DaemonHandoffStage::Failed => {
                return Err(AppError::Message(
                    state
                        .failure
                        .unwrap_or_else(|| "daemon handoff failed".into()),
                ));
            }
            DaemonHandoffStage::SuccessorPrepared => {}
            other => {
                return Err(AppError::Message(format!(
                    "unexpected handoff stage while waiting for cutover: {other:?}"
                )))
            }
        }
        if !platform().is_process_alive(options.predecessor_pid) {
            return Err(AppError::Message(format!(
                "handoff predecessor PID {} exited before releasing ownership",
                options.predecessor_pid
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Message(
                "timed out waiting for handoff predecessor to release ownership".into(),
            ));
        }
        tokio::time::sleep(HANDOFF_POLL_INTERVAL).await;
    }

    // Give the predecessor's graceful-shutdown signal one scheduler turn to
    // stop its accept loop before this generation activates the inherited fd.
    tokio::time::sleep(CHILD_ACTIVATION_GRACE).await;
    Ok(ImportedListeners {
        mcp,
        mcp_snapshot,
        actions,
    })
}

pub(crate) fn mark_ownership_released(
    profile: &WorkspaceProfile,
    prepared: &PreparedSuccessor,
) -> AppResult<()> {
    let mut state = daemon::read_handoff_state(&profile.id, &prepared.handoff_id)?
        .ok_or_else(|| AppError::Message("daemon handoff state disappeared at cutover".into()))?;
    if state.stage != DaemonHandoffStage::SuccessorPrepared
        || state.successor_pid != Some(prepared.successor_pid)
    {
        return Err(AppError::Message(format!(
            "cannot release daemon ownership from handoff stage {:?} / successor {:?}",
            state.stage, state.successor_pid
        )));
    }
    state.stage = DaemonHandoffStage::OwnershipReleased;
    state.ownership_released = true;
    state.failure = None;
    daemon::write_handoff_state(&state)
}

pub(crate) fn mark_canonical_ready(profile_id: &str, handoff_id: &str) -> AppResult<()> {
    let mut state = daemon::read_handoff_state(profile_id, handoff_id)?
        .ok_or_else(|| AppError::Message("daemon handoff state disappeared before ready".into()))?;
    if state.stage != DaemonHandoffStage::OwnershipReleased
        || state.successor_pid != Some(std::process::id())
    {
        return Err(AppError::Message(format!(
            "cannot mark daemon handoff ready from stage {:?} / successor {:?}",
            state.stage, state.successor_pid
        )));
    }
    state.stage = DaemonHandoffStage::CanonicalReady;
    state.failure = None;
    daemon::write_handoff_state(&state)
}

pub(crate) async fn wait_canonical_ready(
    profile: &WorkspaceProfile,
    prepared: &PreparedSuccessor,
    timeout: Duration,
) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let state =
            daemon::read_handoff_state(&profile.id, &prepared.handoff_id)?.ok_or_else(|| {
                AppError::Message("daemon handoff state disappeared after cutover".into())
            })?;
        match state.stage {
            DaemonHandoffStage::CanonicalReady
                if state.successor_pid == Some(prepared.successor_pid) =>
            {
                return Ok(())
            }
            DaemonHandoffStage::Failed => {
                return Err(AppError::Message(
                    state
                        .failure
                        .unwrap_or_else(|| "daemon handoff failed".into()),
                ))
            }
            DaemonHandoffStage::OwnershipReleased => {}
            other => {
                return Err(AppError::Message(format!(
                    "unexpected daemon handoff stage after cutover: {other:?}"
                )))
            }
        }
        if !platform().is_process_alive(prepared.successor_pid) {
            return Err(AppError::Message(format!(
                "handoff successor PID {} exited before canonical readiness",
                prepared.successor_pid
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Message(format!(
                "handoff successor PID {} did not become canonical before timeout",
                prepared.successor_pid
            )));
        }
        tokio::time::sleep(HANDOFF_POLL_INTERVAL).await;
    }
}

pub(crate) fn mark_failed(profile: &WorkspaceProfile, handoff_id: &str, failure: &str) {
    let Ok(Some(mut state)) = daemon::read_handoff_state(&profile.id, handoff_id) else {
        return;
    };
    state.stage = DaemonHandoffStage::Failed;
    state.failure = Some(failure.to_string());
    let _ = daemon::write_handoff_state(&state);
}
