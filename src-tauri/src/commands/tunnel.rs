use std::time::Duration;

use tauri::State;

use crate::app_state::AppState;
use crate::control::{self, DaemonLaunchSpec, WorkspaceControlStatus};
use crate::daemon;
use crate::error::{AppError, AppResult};
use crate::tunnel::{frp_snippet, TunnelServiceKind, TunnelStatus};
use crate::workspace::resources::{validate_service_start, WorkspaceService};

#[cfg(windows)]
use crate::platform::platform;
#[cfg(windows)]
use crate::tunnel::supervisor;

const DESKTOP_TUNNEL_TIMEOUT: Duration = Duration::from_secs(15);

async fn probe_public_tunnel(public_url: &str, kind: TunnelServiceKind) -> AppResult<()> {
    let base = public_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(AppError::Message("隧道未返回公网 URL。".into()));
    }
    let endpoint = match kind {
        TunnelServiceKind::Mcp => format!("{base}/mcp"),
        TunnelServiceKind::Actions => format!("{base}/openapi.json"),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|error| AppError::Message(format!("创建公网探测客户端失败：{error}")))?;
    let mut last_error = String::new();
    for attempt in 0..5 {
        match client.get(&endpoint).send().await {
            Ok(_) => return Ok(()),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_millis(500 * (attempt + 1))).await;
    }
    Err(AppError::Message(format!(
        "frpc 已建立代理，但公网地址仍不可访问：{last_error}。若使用 FRP HTTPS→HTTP，请确认服务端字段为 vhostHTTPSPort。"
    )))
}

#[cfg(windows)]
fn desktop_server_mode() -> bool {
    !daemon::supported()
}

#[cfg(windows)]
async fn server_start_tunnel(
    state: &AppState,
    profile: &crate::workspace::WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<TunnelStatus> {
    let settings = state.with_settings(|store| Ok(store.settings()))?;
    let status = supervisor()
        .lock()
        .await
        .start(profile, kind, &settings)
        .await?;
    persist_public_url(state, &profile.id, kind, &status.public_url)?;
    Ok(status)
}

#[cfg(windows)]
async fn server_stop_tunnel(
    state: &AppState,
    profile: &crate::workspace::WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<TunnelStatus> {
    let settings = state.with_settings(|store| Ok(store.settings()))?;
    let mut guard = supervisor().lock().await;
    guard.stop(profile, kind, &settings).await?;
    Ok(guard.status(profile, kind, &settings))
}

#[cfg(windows)]
async fn server_restart_tunnel(
    state: &AppState,
    profile: &crate::workspace::WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<TunnelStatus> {
    let settings = state.with_settings(|store| Ok(store.settings()))?;
    let mut guard = supervisor().lock().await;
    let current = guard.status(profile, kind, &settings);
    if current.state != "running" {
        return Ok(current);
    }
    let tunnel_type = match kind {
        TunnelServiceKind::Mcp => profile.tunnel.tunnel_type.as_str(),
        TunnelServiceKind::Actions => profile.actions.tunnel_type.as_str(),
    };
    let status = if tunnel_type == "frp" {
        // TunnelSupervisor::start performs FRP route replacement atomically and
        // restores the old route if the new configuration fails.
        guard.start(profile, kind, &settings).await?
    } else {
        guard.stop(profile, kind, &settings).await?;
        guard.start(profile, kind, &settings).await?
    };
    drop(guard);
    persist_public_url(state, &profile.id, kind, &status.public_url)?;
    Ok(status)
}

#[cfg(windows)]
fn local_service_listening(
    profile: &crate::workspace::WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<bool> {
    let port = match kind {
        TunnelServiceKind::Mcp => profile.runtime.local_port,
        TunnelServiceKind::Actions => profile.actions.local_port,
    };
    Ok(platform().find_pid_listening_on_port(port)?.is_some())
}

fn tunnel_configured(
    profile: &crate::workspace::WorkspaceProfile,
    kind: TunnelServiceKind,
) -> bool {
    match kind {
        TunnelServiceKind::Mcp => profile.tunnel.tunnel_type != "none",
        TunnelServiceKind::Actions => profile.actions.tunnel_type != "none",
    }
}

fn profile_by_id(state: &AppState, id: &str) -> AppResult<crate::workspace::WorkspaceProfile> {
    state.with_workspaces(|store| {
        store
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })
}

fn ensure_not_gateway_managed(state: &AppState, kind: TunnelServiceKind) -> AppResult<()> {
    if kind == TunnelServiceKind::Mcp
        && state.with_settings(|store| Ok(store.settings().mcp_gateway.enabled))?
    {
        return Err(AppError::Message(
            "MCP 隧道当前由单一 Gateway 管理；请在“设置 → 通用 → 单一 MCP Gateway”中操作。".into(),
        ));
    }
    Ok(())
}

fn validate_tunnel_start_resources(
    state: &AppState,
    id: &str,
    kind: TunnelServiceKind,
) -> AppResult<()> {
    let service = match kind {
        TunnelServiceKind::Mcp => WorkspaceService::Mcp,
        TunnelServiceKind::Actions => WorkspaceService::Actions,
    };
    state.with_workspaces(|store| validate_service_start(store.list(), id, service))
}

fn persist_public_url(
    state: &AppState,
    id: &str,
    kind: TunnelServiceKind,
    public_url: &str,
) -> AppResult<()> {
    if public_url.is_empty() {
        return Ok(());
    }
    state.with_workspaces(|store| {
        let Some(mut profile) = store.get(id).cloned() else {
            return Ok(());
        };
        match kind {
            TunnelServiceKind::Mcp => profile.tunnel.public_url = public_url.to_string(),
            TunnelServiceKind::Actions => profile.actions.public_url = public_url.to_string(),
        }
        store.update(profile)?;
        Ok(())
    })?;
    let service = match kind {
        TunnelServiceKind::Mcp => "mcp",
        TunnelServiceKind::Actions => "actions",
    };
    crate::runtime::update_public_url(id, service, public_url);
    Ok(())
}

fn workspace_service(kind: TunnelServiceKind) -> WorkspaceService {
    match kind {
        TunnelServiceKind::Mcp => WorkspaceService::Mcp,
        TunnelServiceKind::Actions => WorkspaceService::Actions,
    }
}

fn configured_tunnel_status(
    profile: &crate::workspace::WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<TunnelStatus> {
    let public_url = match kind {
        TunnelServiceKind::Mcp => profile.effective_public_url()?,
        TunnelServiceKind::Actions => profile.actions_effective_public_url()?,
    };
    Ok(TunnelStatus {
        state: "stopped".into(),
        public_url,
        tunnel_pid: None,
    })
}

fn tunnel_status_from_control(
    status: WorkspaceControlStatus,
    kind: TunnelServiceKind,
) -> Option<TunnelStatus> {
    match kind {
        TunnelServiceKind::Mcp => status.mcp_tunnel,
        TunnelServiceKind::Actions => status.actions_tunnel,
    }
}

async fn daemon_tunnel_status(
    profile: &crate::workspace::WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<TunnelStatus> {
    let inspection = daemon::inspect(profile)?;
    if !inspection.running {
        return configured_tunnel_status(profile, kind);
    }
    let status = control::request_workspace_status(profile)
        .await
        .map_err(|error| AppError::Message(format!("读取 daemon 隧道状态失败：{error}")))?;
    tunnel_status_from_control(status, kind).ok_or_else(|| {
        AppError::Message("daemon control status omitted tunnel state for protocol v3".into())
    })
}

fn daemon_spec(state: &daemon::DaemonState) -> DaemonLaunchSpec {
    DaemonLaunchSpec {
        service: state.service,
        tunnels: state.managed_tunnels(),
    }
}

#[tauri::command]
pub fn get_frp_snippet(
    state: State<'_, AppState>,
    id: String,
    service: String,
) -> AppResult<String> {
    let mut profile = profile_by_id(&state, &id)?;
    let kind = TunnelServiceKind::parse(&service)?;
    if kind == TunnelServiceKind::Mcp {
        let gateway = state.with_settings(|store| Ok(store.settings().mcp_gateway))?;
        if gateway.enabled {
            if gateway.owner_workspace_id != id {
                return Err(AppError::Message(
                    "只有 MCP Gateway 隧道所有者工作区可以生成共享 FRP 片段。".into(),
                ));
            }
            profile.runtime.local_port = gateway.local_port;
        }
    }
    frp_snippet(&profile, kind)
}

#[tauri::command]
pub async fn restart_tunnel(
    state: State<'_, AppState>,
    id: String,
    service: String,
) -> AppResult<TunnelStatus> {
    let profile = profile_by_id(&state, &id)?;
    let kind = TunnelServiceKind::parse(&service)?;
    ensure_not_gateway_managed(&state, kind)?;
    validate_tunnel_start_resources(&state, &id, kind)?;
    #[cfg(windows)]
    if desktop_server_mode() {
        return server_restart_tunnel(&state, &profile, kind).await;
    }
    if !daemon::inspect(&profile)?.running {
        return configured_tunnel_status(&profile, kind);
    }
    if !tunnel_configured(&profile, kind) {
        return configured_tunnel_status(&profile, kind);
    }
    let current = daemon_tunnel_status(&profile, kind).await?;
    let action = if current.state == "running" {
        control::ControlTunnelAction::Restart
    } else {
        control::ControlTunnelAction::Start
    };
    let status = control::request_tunnel_operation(&profile, kind, action, DESKTOP_TUNNEL_TIMEOUT)
        .await
        .map_err(|error| AppError::Message(format!("daemon 隧道重载失败：{error}")))?;

    persist_public_url(&state, &id, kind, &status.public_url)?;
    Ok(status)
}

#[tauri::command]
pub async fn start_tunnel(
    state: State<'_, AppState>,
    id: String,
    service: String,
) -> AppResult<TunnelStatus> {
    let profile = profile_by_id(&state, &id)?;
    let kind = TunnelServiceKind::parse(&service)?;
    ensure_not_gateway_managed(&state, kind)?;
    validate_tunnel_start_resources(&state, &id, kind)?;
    #[cfg(windows)]
    if desktop_server_mode() {
        return server_start_tunnel(&state, &profile, kind).await;
    }
    let status = control::request_tunnel_operation(
        &profile,
        kind,
        control::ControlTunnelAction::Start,
        DESKTOP_TUNNEL_TIMEOUT,
    )
    .await
    .map_err(|error| AppError::Message(format!("daemon 隧道启动失败：{error}")))?;

    persist_public_url(&state, &id, kind, &status.public_url)?;
    Ok(status)
}

#[tauri::command]
pub async fn stop_tunnel(
    state: State<'_, AppState>,
    id: String,
    service: String,
) -> AppResult<TunnelStatus> {
    let profile = profile_by_id(&state, &id)?;
    let kind = TunnelServiceKind::parse(&service)?;
    ensure_not_gateway_managed(&state, kind)?;
    #[cfg(windows)]
    if desktop_server_mode() {
        return server_stop_tunnel(&state, &profile, kind).await;
    }
    if !daemon::inspect(&profile)?.running {
        return configured_tunnel_status(&profile, kind);
    }
    control::request_tunnel_operation(
        &profile,
        kind,
        control::ControlTunnelAction::Stop,
        DESKTOP_TUNNEL_TIMEOUT,
    )
    .await
    .map_err(|error| AppError::Message(format!("daemon 隧道停止失败：{error}")))
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelTestResult {
    pub success: bool,
    pub public_url: String,
    pub kept_running: bool,
    pub message: String,
}

/// Probe tunnel connectivity without leaving it running unless the local service is already up.
#[tauri::command]
pub async fn test_tunnel(
    state: State<'_, AppState>,
    id: String,
    service: String,
) -> AppResult<TunnelTestResult> {
    let profile = profile_by_id(&state, &id)?;
    let kind = TunnelServiceKind::parse(&service)?;
    ensure_not_gateway_managed(&state, kind)?;
    validate_tunnel_start_resources(&state, &id, kind)?;
    #[cfg(windows)]
    if desktop_server_mode() {
        let runtime_running = local_service_listening(&profile, kind)?;
        let status = server_start_tunnel(&state, &profile, kind).await?;
        let public_url = status.public_url.clone();
        if let Err(error) = probe_public_tunnel(&public_url, kind).await {
            if !runtime_running {
                let _ = server_stop_tunnel(&state, &profile, kind).await;
            }
            return Err(error);
        }
        if runtime_running {
            return Ok(TunnelTestResult {
                success: !public_url.is_empty() || status.state == "running",
                public_url,
                kept_running: true,
                message: "隧道测试成功，已保持连接（Windows GUI Server 服务运行中）。".into(),
            });
        }
        server_stop_tunnel(&state, &profile, kind).await?;
        return Ok(TunnelTestResult {
            success: !public_url.is_empty(),
            public_url,
            kept_running: false,
            message: "隧道配置验证通过。本地服务未运行，测试连接已自动断开。".into(),
        });
    }
    let inspection = daemon::inspect(&profile)?;
    let previous_state = inspection
        .state
        .as_ref()
        .filter(|_| inspection.running)
        .cloned();
    let previous_spec = previous_state.as_ref().map(daemon_spec);
    let runtime_running = previous_state.as_ref().is_some_and(|daemon_state| {
        control::service_is_selected(daemon_state.service, workspace_service(kind))
    });

    if previous_state.is_none() {
        let temporary_service = match kind {
            TunnelServiceKind::Mcp => daemon::ServiceSelection::Mcp,
            TunnelServiceKind::Actions => daemon::ServiceSelection::Actions,
        };
        control::ensure_daemon_running(
            &profile,
            DaemonLaunchSpec {
                service: temporary_service,
                tunnels: None,
            },
            DESKTOP_TUNNEL_TIMEOUT,
        )
        .await?;
    }

    let before = daemon_tunnel_status(&profile, kind).await?;
    let action = if before.state == "running" {
        control::ControlTunnelAction::Restart
    } else {
        control::ControlTunnelAction::Start
    };
    let status =
        match control::request_tunnel_operation(&profile, kind, action, DESKTOP_TUNNEL_TIMEOUT)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                if previous_state.is_none() {
                    let _ = control::reconcile_daemon(
                        &profile,
                        previous_spec,
                        DESKTOP_TUNNEL_TIMEOUT,
                        true,
                    )
                    .await;
                }
                return Err(AppError::Message(format!(
                    "daemon 隧道测试启动失败：{error}"
                )));
            }
        };

    let public_url = status.public_url.clone();
    let keep_tunnel = runtime_running;

    if let Err(error) = probe_public_tunnel(&public_url, kind).await {
        if !keep_tunnel {
            let _ = control::request_tunnel_operation(
                &profile,
                kind,
                control::ControlTunnelAction::Stop,
                DESKTOP_TUNNEL_TIMEOUT,
            )
            .await;
        }
        if previous_state.is_none() {
            let _ =
                control::reconcile_daemon(&profile, previous_spec, DESKTOP_TUNNEL_TIMEOUT, true)
                    .await;
        }
        return Err(error);
    }

    if keep_tunnel {
        persist_public_url(&state, &id, kind, &public_url)?;
        return Ok(TunnelTestResult {
            success: !public_url.is_empty() || status.state == "running",
            public_url,
            kept_running: true,
            message: "隧道测试成功，已保持连接（服务运行中）。".into(),
        });
    }

    control::request_tunnel_operation(
        &profile,
        kind,
        control::ControlTunnelAction::Stop,
        DESKTOP_TUNNEL_TIMEOUT,
    )
    .await
    .map_err(|error| AppError::Message(format!("测试后停止 daemon 隧道失败：{error}")))?;
    if previous_state.is_none() {
        control::reconcile_daemon(&profile, previous_spec, DESKTOP_TUNNEL_TIMEOUT, true).await?;
    }

    let success = !public_url.is_empty();
    let message = if public_url.is_empty() {
        "隧道进程已退出，未获取到公网地址。".into()
    } else {
        "隧道配置验证通过。本地服务未运行，测试连接已自动断开。".into()
    };

    Ok(TunnelTestResult {
        success,
        public_url,
        kept_running: false,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::PortStatus;
    use crate::daemon::{DaemonInspection, DaemonState, ServiceSelection};

    fn control_status(profile: &crate::workspace::WorkspaceProfile) -> WorkspaceControlStatus {
        WorkspaceControlStatus {
            id: profile.id.clone(),
            name: profile.name.clone(),
            path: profile.path.clone(),
            daemon: DaemonInspection {
                supported: true,
                running: true,
                stale: false,
                ambiguous: false,
                pid_matches: true,
                state: Some(DaemonState {
                    schema_version: 2,
                    workspace_id: profile.id.clone(),
                    workspace_name: profile.name.clone(),
                    workspace_path: profile.path.clone(),
                    pid: 42,
                    started_at_unix: 1,
                    service: ServiceSelection::All,
                    tunnel: true,
                    tunnel_services: Some(ServiceSelection::Mcp),
                    log_path: "daemon.log".into(),
                    version: "test".into(),
                    executable_path: "anchor.exe".into(),
                }),
                detail: "running".into(),
            },
            mcp: PortStatus {
                service: "mcp".into(),
                port: profile.runtime.local_port,
                listening: true,
                pid: Some(42),
                owner: "daemon".into(),
                endpoint: profile.local_endpoint(),
            },
            actions: PortStatus {
                service: "actions".into(),
                port: profile.actions.local_port,
                listening: true,
                pid: Some(42),
                owner: "daemon".into(),
                endpoint: profile.actions_local_base_url(),
            },
            mcp_activity: None,
            mcp_tunnel: Some(TunnelStatus {
                state: "running".into(),
                public_url: "https://mcp.example.com".into(),
                tunnel_pid: Some(7),
            }),
            actions_tunnel: Some(TunnelStatus {
                state: "stopped".into(),
                public_url: "https://actions.example.com".into(),
                tunnel_pid: None,
            }),
        }
    }

    #[test]
    fn tunnel_status_is_selected_from_the_daemon_control_snapshot() {
        let profile =
            crate::workspace::WorkspaceProfile::new(".".into(), Some("tunnel-status".into()));
        let status = control_status(&profile);

        assert_eq!(
            tunnel_status_from_control(status.clone(), TunnelServiceKind::Mcp)
                .expect("mcp tunnel")
                .public_url,
            "https://mcp.example.com"
        );
        assert_eq!(
            tunnel_status_from_control(status, TunnelServiceKind::Actions)
                .expect("actions tunnel")
                .state,
            "stopped"
        );
    }

    #[test]
    fn daemon_spec_preserves_partial_tunnel_ownership() {
        let profile =
            crate::workspace::WorkspaceProfile::new(".".into(), Some("tunnel-spec".into()));
        let state = control_status(&profile).daemon.state.expect("daemon state");

        assert_eq!(
            daemon_spec(&state),
            DaemonLaunchSpec {
                service: ServiceSelection::All,
                tunnels: Some(ServiceSelection::Mcp),
            }
        );
    }
}
