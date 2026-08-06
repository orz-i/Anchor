use std::time::Duration;

use crate::daemon::{self, DaemonInspection, DaemonState, ServiceSelection};
use crate::error::{AppError, AppResult};
use crate::workspace::resources::WorkspaceService;
use crate::workspace::WorkspaceProfile;

use super::{ipc_ping, request_daemon_exit, ControlOperation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonLaunchSpec {
    pub service: ServiceSelection,
    pub tunnel: bool,
}

pub fn desired_service_selection(
    current: Option<ServiceSelection>,
    service: WorkspaceService,
    enabled: bool,
) -> Option<ServiceSelection> {
    match (current, service, enabled) {
        (None, WorkspaceService::Mcp, true) => Some(ServiceSelection::Mcp),
        (None, WorkspaceService::Actions, true) => Some(ServiceSelection::Actions),
        (None, _, false) => None,
        (Some(ServiceSelection::Mcp), WorkspaceService::Mcp, true)
        | (Some(ServiceSelection::Mcp), WorkspaceService::Actions, false) => {
            Some(ServiceSelection::Mcp)
        }
        (Some(ServiceSelection::Actions), WorkspaceService::Actions, true)
        | (Some(ServiceSelection::Actions), WorkspaceService::Mcp, false) => {
            Some(ServiceSelection::Actions)
        }
        (Some(ServiceSelection::Mcp), WorkspaceService::Actions, true)
        | (Some(ServiceSelection::Actions), WorkspaceService::Mcp, true) => {
            Some(ServiceSelection::All)
        }
        (Some(ServiceSelection::Mcp), WorkspaceService::Mcp, false)
        | (Some(ServiceSelection::Actions), WorkspaceService::Actions, false) => None,
        (Some(ServiceSelection::All), WorkspaceService::Mcp, true)
        | (Some(ServiceSelection::All), WorkspaceService::Actions, true) => {
            Some(ServiceSelection::All)
        }
        (Some(ServiceSelection::All), WorkspaceService::Mcp, false) => {
            Some(ServiceSelection::Actions)
        }
        (Some(ServiceSelection::All), WorkspaceService::Actions, false) => {
            Some(ServiceSelection::Mcp)
        }
    }
}

pub async fn ensure_daemon_running(
    profile: &WorkspaceProfile,
    spec: DaemonLaunchSpec,
    timeout: Duration,
) -> AppResult<DaemonState> {
    let inspection = daemon::inspect(profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.running {
        let state = inspection.state.expect("running daemon state");
        ipc_ping(&profile.id).await.map_err(|error| {
            AppError::Message(format!(
                "daemon 状态显示正在运行，但控制端点不可用：{error}；拒绝使用本地写回退"
            ))
        })?;
        if state.service == spec.service && state.tunnel == spec.tunnel {
            return Ok(state);
        }
        return Err(AppError::Message(format!(
            "daemon 已运行（service={}, tunnel={}），目标配置为 service={}, tunnel={}；请先通过控制面协调重启",
            state.service.as_str(),
            state.tunnel,
            spec.service.as_str(),
            spec.tunnel
        )));
    }

    let child_pid = daemon::spawn(profile, spec.service, spec.tunnel)?;
    match daemon::wait_ready(profile, spec.service, child_pid, timeout).await {
        Ok(state) => Ok(state),
        Err(error) => {
            let cleanup_error = daemon::terminate_spawned(profile, child_pid).await.err();
            Err(AppError::Message(format!(
                "daemon 子进程 PID {child_pid} 未就绪：{error}{}",
                cleanup_error
                    .map(|cleanup| format!("；清理失败：{cleanup}"))
                    .unwrap_or_default()
            )))
        }
    }
}

pub async fn request_daemon_exit_and_wait(
    profile: &WorkspaceProfile,
    operation: ControlOperation,
    timeout: Duration,
    force: bool,
) -> AppResult<Option<u32>> {
    let inspection = daemon::inspect(profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let Some(state) = inspection.state else {
        if inspection.stale {
            daemon::cleanup(profile)?;
        }
        return Ok(None);
    };
    if !inspection.running {
        daemon::cleanup(profile)?;
        return Ok(None);
    }
    if !inspection.pid_matches {
        return Err(AppError::Message(format!(
            "PID {} 不属于当前 workspace daemon，拒绝发送控制请求",
            state.pid
        )));
    }
    let accepted_pid = request_daemon_exit(&profile.id, operation)
        .await
        .map_err(|error| {
            AppError::Message(format!(
                "daemon 未接受 {} 请求：{error}；写操作不会回退到本地进程控制",
                operation_label(operation)
            ))
        })?;
    if accepted_pid != state.pid {
        return Err(AppError::Message(format!(
            "daemon 控制响应 PID 不匹配：状态文件为 {}，响应为 {accepted_pid}",
            state.pid
        )));
    }
    daemon::wait_for_controlled_exit(profile, state.pid, timeout, force).await?;
    Ok(Some(state.pid))
}

pub async fn reconcile_daemon(
    profile: &WorkspaceProfile,
    desired: Option<DaemonLaunchSpec>,
    timeout: Duration,
    force: bool,
) -> AppResult<Option<DaemonState>> {
    let inspection = daemon::inspect(profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let current = running_state(&inspection);

    match (current, desired) {
        (None, None) => {
            if inspection.stale {
                daemon::cleanup(profile)?;
            }
            Ok(None)
        }
        (None, Some(spec)) => ensure_daemon_running(profile, spec, timeout)
            .await
            .map(Some),
        (Some(state), Some(spec))
            if state.service == spec.service && state.tunnel == spec.tunnel =>
        {
            ipc_ping(&profile.id).await.map_err(|error| {
                AppError::Message(format!(
                    "daemon 状态显示正在运行，但控制端点不可用：{error}；拒绝使用本地写回退"
                ))
            })?;
            Ok(Some(state))
        }
        (Some(_), None) => {
            request_daemon_exit_and_wait(profile, ControlOperation::Shutdown, timeout, force)
                .await?;
            Ok(None)
        }
        (Some(_), Some(spec)) => {
            request_daemon_exit_and_wait(profile, ControlOperation::Restart, timeout, force)
                .await?;
            ensure_daemon_running(profile, spec, timeout)
                .await
                .map(Some)
        }
    }
}

pub async fn set_daemon_service(
    profile: &WorkspaceProfile,
    service: WorkspaceService,
    enabled: bool,
    tunnel_on_start: bool,
    timeout: Duration,
    force: bool,
) -> AppResult<Option<DaemonState>> {
    let inspection = daemon::inspect(profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let current = running_state(&inspection);
    let desired_service = desired_service_selection(
        current.as_ref().map(|state| state.service),
        service,
        enabled,
    );
    let desired = desired_service.map(|service| DaemonLaunchSpec {
        service,
        tunnel: current
            .as_ref()
            .map(|state| {
                if enabled {
                    state.tunnel || tunnel_on_start
                } else {
                    state.tunnel
                }
            })
            .unwrap_or(tunnel_on_start),
    });
    reconcile_daemon(profile, desired, timeout, force).await
}

pub async fn restart_daemon_service(
    profile: &WorkspaceProfile,
    service: WorkspaceService,
    tunnel_on_start: bool,
    timeout: Duration,
    force: bool,
) -> AppResult<DaemonState> {
    let inspection = daemon::inspect(profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let current = running_state(&inspection);
    let desired_service =
        desired_service_selection(current.as_ref().map(|state| state.service), service, true)
            .expect("enabling a service always yields a daemon selection");
    let spec = DaemonLaunchSpec {
        service: desired_service,
        tunnel: current
            .as_ref()
            .map(|state| state.tunnel || tunnel_on_start)
            .unwrap_or(tunnel_on_start),
    };

    if current.is_some() {
        request_daemon_exit_and_wait(profile, ControlOperation::Restart, timeout, force).await?;
    }
    ensure_daemon_running(profile, spec, timeout).await
}

fn running_state(inspection: &DaemonInspection) -> Option<DaemonState> {
    inspection
        .running
        .then(|| inspection.state.clone())
        .flatten()
}

fn operation_label(operation: ControlOperation) -> &'static str {
    match operation {
        ControlOperation::Shutdown => "shutdown",
        ControlOperation::Restart => "restart",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceProfile;

    #[test]
    fn service_transition_matrix_preserves_independent_toggles() {
        assert_eq!(
            desired_service_selection(None, WorkspaceService::Mcp, true),
            Some(ServiceSelection::Mcp)
        );
        assert_eq!(
            desired_service_selection(Some(ServiceSelection::Actions), WorkspaceService::Mcp, true,),
            Some(ServiceSelection::All)
        );
        assert_eq!(
            desired_service_selection(Some(ServiceSelection::All), WorkspaceService::Mcp, false,),
            Some(ServiceSelection::Actions)
        );
        assert_eq!(
            desired_service_selection(Some(ServiceSelection::Mcp), WorkspaceService::Mcp, false,),
            None
        );
        assert_eq!(
            desired_service_selection(
                Some(ServiceSelection::Mcp),
                WorkspaceService::Actions,
                false,
            ),
            Some(ServiceSelection::Mcp)
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn unsupported_platform_start_fails_closed_without_local_runtime_fallback() {
        let profile = WorkspaceProfile::new(".".into(), Some("unsupported-daemon".into()));

        let error = ensure_daemon_running(
            &profile,
            DaemonLaunchSpec {
                service: ServiceSelection::Mcp,
                tunnel: false,
            },
            Duration::from_millis(50),
        )
        .await
        .expect_err("unsupported daemon start must fail");

        assert!(error.to_string().contains("daemon 目前仅支持 Linux"));
        assert!(!daemon::inspect(&profile).expect("inspection").running);
    }
}
