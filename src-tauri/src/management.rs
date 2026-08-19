use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::control::{
    self, ControlEventBatch, ControlEventCursor, ControlLogChunk, ControlLogSelection,
    DaemonLaunchSpec, WorkspaceControlStatus,
};
use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::gateway_control::{self, GatewayControlStatus, GatewayEventBatch, GatewayEventCursor};
use crate::platform::platform;
use crate::settings::{AppSettings, DownloadConfig, McpGatewayConfig, ProxyConfig};
use crate::tunnel::{TunnelServiceKind, TunnelStatus};
use crate::workspace::resources::{validate_service_start, WorkspaceService};
use crate::workspace::{RuntimeRecoveryDto, RuntimeStatusDto, WorkspaceProfile};

const MANAGEMENT_DAEMON_TIMEOUT: Duration = Duration::from_secs(15);
const MANAGEMENT_TUNNEL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayConfigWriteAction {
    PersistLocally,
    ApplyViaDaemon { pid: u32 },
    ShutdownThenPersist { pid: u32 },
}

fn desired_gateway_routes(current: &[String], workspace_id: &str, enabled: bool) -> Vec<String> {
    let mut routes = current
        .iter()
        .filter(|id| id.as_str() != workspace_id)
        .cloned()
        .collect::<Vec<_>>();
    if enabled {
        routes.push(workspace_id.to_string());
    }
    routes.sort();
    routes.dedup();
    routes
}

async fn restore_direct_workspace_after_gateway_failure(
    profile: &WorkspaceProfile,
    previous: Option<DaemonLaunchSpec>,
    primary: AppError,
) -> AppError {
    let Some(previous) = previous else {
        return primary;
    };
    match control::reconcile_daemon(profile, Some(previous), MANAGEMENT_DAEMON_TIMEOUT, true).await
    {
        Ok(_) => {
            #[cfg(windows)]
            if let Err(plan_error) =
                crate::windows_service::set_workspace_desired(&profile.id, Some(previous))
            {
                return AppError::Message(format!(
                    "{primary}；已恢复 Workspace daemon，但恢复 Windows Service 计划失败：{plan_error}"
                ));
            }
            AppError::Message(format!(
                "{primary}；已恢复该 Workspace 原有 MCP daemon 运行态"
            ))
        }
        Err(rollback_error) => AppError::Message(format!(
            "{primary}；恢复该 Workspace 原有 MCP daemon 运行态也失败：{rollback_error}"
        )),
    }
}

pub(crate) async fn set_gateway_workspace_route(
    workspace_id: &str,
    enabled: bool,
) -> AppResult<GatewayControlStatus> {
    let store = DataStore::load()?;
    let config = store.settings().mcp_gateway;
    let profiles = store.list().to_vec();
    if !config.enabled {
        return Err(AppError::Message("MCP Gateway 尚未启用".into()));
    }
    crate::mcp::gateway::validate_config(&config, &profiles)?;
    let profile = profiles
        .iter()
        .find(|profile| profile.id == workspace_id)
        .cloned()
        .ok_or_else(|| {
            AppError::Message(format!("Gateway route workspace 不存在：{workspace_id}"))
        })?;
    if enabled {
        validate_service_start(&profiles, workspace_id, WorkspaceService::Mcp)?;
        if !std::path::Path::new(&profile.path).is_dir() {
            return Err(AppError::Message(format!(
                "Workspace 目录不存在或不可用：{}",
                profile.path
            )));
        }
    }
    drop(store);

    let inspection = crate::gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.running && !inspection.pid_matches {
        return Err(AppError::Message(
            "Gateway daemon reports running but PID ownership does not match".into(),
        ));
    }
    let current_state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches);
    let current_routes = current_state
        .as_ref()
        .map(|state| state.workspace_ids.clone())
        .unwrap_or_default();
    let desired_routes = desired_gateway_routes(&current_routes, workspace_id, enabled);

    if desired_routes == current_routes {
        if current_state.is_some() {
            gateway_control::ping()
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
        }
        #[cfg(windows)]
        crate::windows_service::set_gateway_desired(&desired_routes)?;
        return gateway_control::status_via_daemon_or_local().await;
    }

    let direct_inspection = crate::daemon::inspect(&profile)?;
    if direct_inspection.ambiguous {
        return Err(AppError::Message(direct_inspection.detail));
    }
    let direct_previous = direct_inspection
        .state
        .as_ref()
        .filter(|_| direct_inspection.running && direct_inspection.pid_matches)
        .filter(|state| state.service.includes_mcp())
        .map(|state| DaemonLaunchSpec {
            service: state.service,
            tunnels: state.managed_tunnels(),
        });
    if enabled && direct_previous.is_some() {
        control::set_daemon_service(
            &profile,
            WorkspaceService::Mcp,
            false,
            false,
            MANAGEMENT_DAEMON_TIMEOUT,
            true,
        )
        .await?;
    }

    let transition = async {
        match current_state {
            Some(current) if desired_routes.is_empty() => {
                let accepted_pid =
                    gateway_control::request_exit(gateway_control::GatewayOperation::Shutdown)
                        .await
                        .map_err(|error| AppError::Message(error.to_string()))?;
                if accepted_pid != current.pid {
                    return Err(AppError::Message(format!(
                        "Gateway route shutdown PID mismatch: state={}, response={accepted_pid}",
                        current.pid
                    )));
                }
                crate::gateway_daemon::wait_for_exit(
                    current.pid,
                    MANAGEMENT_DAEMON_TIMEOUT,
                    false,
                )
                .await?;
            }
            Some(_) => {
                gateway_control::request_set_routes(
                    desired_routes.clone(),
                    MANAGEMENT_DAEMON_TIMEOUT,
                )
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            }
            None if !desired_routes.is_empty() => {
                let pid = crate::gateway_daemon::spawn(&desired_routes)?;
                if let Err(error) =
                    crate::gateway_daemon::wait_ready(pid, MANAGEMENT_DAEMON_TIMEOUT).await
                {
                    let cleanup = crate::gateway_daemon::terminate_spawned(pid).await;
                    return match cleanup {
                        Ok(()) => Err(error),
                        Err(cleanup_error) => Err(AppError::Message(format!(
                            "Gateway route 启动失败：{error}；清理 PID {pid} 也失败：{cleanup_error}"
                        ))),
                    };
                }
            }
            None => {
                crate::gateway_daemon::cleanup()?;
            }
        }
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = transition {
        if enabled && direct_previous.is_some() {
            return Err(restore_direct_workspace_after_gateway_failure(
                &profile,
                direct_previous,
                error,
            )
            .await);
        }
        return Err(error);
    }

    #[cfg(windows)]
    crate::windows_service::set_gateway_desired(&desired_routes)?;
    gateway_control::status_via_daemon_or_local().await
}

fn gateway_config_write_action(
    inspection: &crate::gateway_daemon::GatewayDaemonInspection,
    enabled: bool,
) -> AppResult<GatewayConfigWriteAction> {
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail.clone()));
    }
    if !inspection.running {
        return Ok(GatewayConfigWriteAction::PersistLocally);
    }
    if !inspection.pid_matches {
        return Err(AppError::Message(
            "Gateway daemon reports running but PID ownership does not match".into(),
        ));
    }
    let pid = inspection
        .state
        .as_ref()
        .map(|state| state.pid)
        .ok_or_else(|| {
            AppError::Message("Gateway daemon reports running without state metadata".into())
        })?;
    if enabled {
        Ok(GatewayConfigWriteAction::ApplyViaDaemon { pid })
    } else {
        Ok(GatewayConfigWriteAction::ShutdownThenPersist { pid })
    }
}

#[cfg(feature = "cli")]
pub(crate) fn preview_workspace_config(
    base: &WorkspaceProfile,
    candidate: &WorkspaceProfile,
) -> AppResult<crate::cli::ConfigSetReport> {
    crate::cli::preview_profile_config(base, candidate)
}

#[cfg(feature = "cli")]
pub(crate) fn stage_workspace_config(
    base: &WorkspaceProfile,
    candidate: &WorkspaceProfile,
) -> AppResult<crate::cli::ConfigSetReport> {
    crate::cli::stage_profile_config(base, candidate)
}

#[cfg(feature = "cli")]
pub(crate) async fn apply_workspace_config(
    workspace_id: String,
    wait_seconds: u64,
) -> AppResult<crate::cli::ConfigApplyReport> {
    crate::cli::apply_staged_config(crate::cli::ConfigApplyOptions {
        workspace: workspace_id,
        wait_seconds,
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrpProfileDto {
    pub id: String,
    pub name: String,
    pub server: String,
    pub server_port: u16,
    pub has_token: bool,
}

pub(crate) fn list_software() -> AppResult<Vec<crate::tunnel::SoftwareStatus>> {
    Ok(crate::tunnel::list_software())
}

pub(crate) fn get_mcp_gateway() -> AppResult<crate::settings::McpGatewayConfig> {
    DataStore::read_file(|data| Ok(data.mcp_gateway.clone()))
}

pub(crate) async fn get_mcp_gateway_status() -> AppResult<GatewayControlStatus> {
    gateway_control::status_via_daemon_or_local().await
}

pub(crate) async fn set_mcp_gateway(
    mut config: McpGatewayConfig,
) -> AppResult<GatewayControlStatus> {
    config.public_url = config.public_url.trim().trim_end_matches('/').to_string();
    let store = DataStore::load()?;
    let profiles = store.list().to_vec();
    let previous = store.settings().mcp_gateway;
    drop(store);

    let legacy_status = crate::mcp::gateway::status(&previous).await;
    if legacy_status.state == "running" && previous != config {
        return Err(AppError::Message(
            "检测到旧版 process-local Gateway 正在运行；Web Admin 不会在该运行态上热改配置，请先退出旧桌面运行态。"
                .into(),
        ));
    }
    if previous.identity_changed(&config) {
        config.clear_observation();
    } else {
        config.observed_public_url = previous.observed_public_url.clone();
        config.observed_owner_workspace_id = previous.observed_owner_workspace_id.clone();
        config.observed_tunnel_signature = previous.observed_tunnel_signature.clone();
    }
    crate::mcp::gateway::validate_config(&config, &profiles)?;
    let enabled = config.enabled;
    let inspection = crate::gateway_daemon::inspect()?;
    match gateway_config_write_action(&inspection, enabled)? {
        GatewayConfigWriteAction::ApplyViaDaemon { pid } => {
            gateway_control::ping().await.map_err(|error| {
                AppError::Message(format!("Gateway daemon IPC 不可用：{error}"))
            })?;
            gateway_control::request_apply_config(config, MANAGEMENT_DAEMON_TIMEOUT)
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            let status = gateway_control::request_status()
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            if status.pid != Some(pid) {
                return Err(AppError::Message(format!(
                    "Gateway apply_config PID changed unexpectedly: expected {pid}, status={:?}",
                    status.pid
                )));
            }
        }
        GatewayConfigWriteAction::ShutdownThenPersist { pid } => {
            gateway_control::ping().await.map_err(|error| {
                AppError::Message(format!("Gateway daemon IPC 不可用：{error}"))
            })?;
            let accepted_pid =
                gateway_control::request_exit(gateway_control::GatewayOperation::Shutdown)
                    .await
                    .map_err(|error| AppError::Message(error.to_string()))?;
            if accepted_pid != pid {
                return Err(AppError::Message(format!(
                    "Gateway disable PID mismatch: state={pid}, response={accepted_pid}"
                )));
            }
            crate::gateway_daemon::wait_for_exit(pid, MANAGEMENT_DAEMON_TIMEOUT, false).await?;
            gateway_control::persist_config(&config)?;
        }
        GatewayConfigWriteAction::PersistLocally => gateway_control::persist_config(&config)?,
    }
    #[cfg(windows)]
    if !enabled {
        crate::windows_service::set_gateway_desired(&[])?;
    }
    gateway_control::status_via_daemon_or_local().await
}

pub(crate) async fn reload_mcp_gateway() -> AppResult<GatewayControlStatus> {
    gateway_control::request_reload(Duration::from_secs(20))
        .await
        .map_err(|error| AppError::Message(error.to_string()))?;
    gateway_control::request_status()
        .await
        .map_err(|error| AppError::Message(error.to_string()))
}

pub(crate) async fn get_gateway_control_events(
    cursor: Option<GatewayEventCursor>,
    wait_ms: u32,
) -> AppResult<Option<GatewayEventBatch>> {
    match gateway_control::request_events(cursor, 32, wait_ms).await {
        Ok(batch) => Ok(Some(batch)),
        Err(error) if error.is_unavailable() => Ok(None),
        Err(error) => Err(AppError::Message(error.to_string())),
    }
}

fn tunnel_configured_for_service(profile: &WorkspaceProfile, service: WorkspaceService) -> bool {
    match service {
        WorkspaceService::Mcp => profile.tunnel.tunnel_type != "none",
        WorkspaceService::Actions => profile.actions.tunnel_type != "none",
    }
}

fn load_workspace_for_control(
    id: &str,
    validate_start: Option<WorkspaceService>,
) -> AppResult<(WorkspaceProfile, AppSettings)> {
    let store = DataStore::load()?;
    if let Some(service) = validate_start {
        validate_service_start(store.list(), id, service)?;
    }
    let profile = store
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))?;
    let settings = store.settings();
    Ok((profile, settings))
}

fn reject_gateway_managed_mcp(settings: &AppSettings, service: WorkspaceService) -> AppResult<()> {
    if service == WorkspaceService::Mcp && settings.mcp_gateway.enabled {
        return Err(AppError::Message(
            "MCP 当前由 Gateway 控制域管理；Web Admin 不会绕过 Gateway 启停独立 Workspace MCP daemon，请使用 Gateway 管理入口。"
                .into(),
        ));
    }
    Ok(())
}

fn ensure_management_port_available(port: u16, label: &str) -> AppResult<()> {
    if let Some(pid) = platform().find_pid_listening_on_port(port)? {
        return Err(AppError::Message(format!(
            "{label} 端口 {port} 已被 PID {pid} 占用；Web Admin 不会接管未知 listener"
        )));
    }
    Ok(())
}

pub(crate) async fn start_workspace_service(
    id: &str,
    service: WorkspaceService,
) -> AppResult<RuntimeStatusDto> {
    let (profile, settings) = load_workspace_for_control(id, Some(service))?;
    reject_gateway_managed_mcp(&settings, service)?;
    let inspection = crate::daemon::inspect(&profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let selected = inspection
        .state
        .as_ref()
        .filter(|_| inspection.running && inspection.pid_matches)
        .is_some_and(|state| control::service_is_selected(state.service, service));
    if !selected {
        match service {
            WorkspaceService::Mcp => {
                ensure_management_port_available(profile.runtime.local_port, "MCP")?
            }
            WorkspaceService::Actions => {
                ensure_management_port_available(profile.actions.local_port, "Actions")?
            }
        }
    }
    control::set_daemon_service(
        &profile,
        service,
        true,
        tunnel_configured_for_service(&profile, service),
        MANAGEMENT_DAEMON_TIMEOUT,
        true,
    )
    .await?;
    runtime_status(id, service).await
}

pub(crate) async fn stop_workspace_service(
    id: &str,
    service: WorkspaceService,
) -> AppResult<RuntimeStatusDto> {
    let (profile, settings) = load_workspace_for_control(id, None)?;
    reject_gateway_managed_mcp(&settings, service)?;
    control::set_daemon_service(
        &profile,
        service,
        false,
        false,
        MANAGEMENT_DAEMON_TIMEOUT,
        true,
    )
    .await?;
    runtime_status(id, service).await
}

pub(crate) async fn restart_workspace_service(
    id: &str,
    service: WorkspaceService,
) -> AppResult<RuntimeStatusDto> {
    let (profile, settings) = load_workspace_for_control(id, Some(service))?;
    reject_gateway_managed_mcp(&settings, service)?;
    let inspection = crate::daemon::inspect(&profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let selected = inspection
        .state
        .as_ref()
        .filter(|_| inspection.running && inspection.pid_matches)
        .is_some_and(|state| control::service_is_selected(state.service, service));
    if !selected {
        match service {
            WorkspaceService::Mcp => {
                ensure_management_port_available(profile.runtime.local_port, "MCP")?
            }
            WorkspaceService::Actions => {
                ensure_management_port_available(profile.actions.local_port, "Actions")?
            }
        }
    }
    control::restart_daemon_service(
        &profile,
        service,
        tunnel_configured_for_service(&profile, service),
        MANAGEMENT_DAEMON_TIMEOUT,
        true,
    )
    .await?;
    runtime_status(id, service).await
}

fn workspace_service_for_tunnel(kind: TunnelServiceKind) -> WorkspaceService {
    match kind {
        TunnelServiceKind::Mcp => WorkspaceService::Mcp,
        TunnelServiceKind::Actions => WorkspaceService::Actions,
    }
}

fn configured_tunnel_status(
    profile: &WorkspaceProfile,
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

fn persist_tunnel_public_url(id: &str, kind: TunnelServiceKind, public_url: &str) -> AppResult<()> {
    if public_url.is_empty() {
        return Ok(());
    }
    DataStore::update_file(|data| {
        let Some(profile) = data.profiles.iter_mut().find(|profile| profile.id == id) else {
            return Ok(());
        };
        match kind {
            TunnelServiceKind::Mcp => profile.tunnel.public_url = public_url.to_string(),
            TunnelServiceKind::Actions => profile.actions.public_url = public_url.to_string(),
        }
        Ok(())
    })?;
    let service = match kind {
        TunnelServiceKind::Mcp => "mcp",
        TunnelServiceKind::Actions => "actions",
    };
    crate::runtime::update_public_url(id, service, public_url);
    Ok(())
}

async fn daemon_tunnel_status(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<TunnelStatus> {
    let inspection = crate::daemon::inspect(profile)?;
    if !inspection.running {
        return configured_tunnel_status(profile, kind);
    }
    let status = control::request_workspace_status(profile)
        .await
        .map_err(|error| AppError::Message(format!("读取 daemon 隧道状态失败：{error}")))?;
    match kind {
        TunnelServiceKind::Mcp => status.mcp_tunnel,
        TunnelServiceKind::Actions => status.actions_tunnel,
    }
    .ok_or_else(|| AppError::Message("daemon control status omitted tunnel state".into()))
}

fn load_tunnel_workspace(
    id: &str,
    kind: TunnelServiceKind,
    validate_start: bool,
) -> AppResult<WorkspaceProfile> {
    let service = workspace_service_for_tunnel(kind);
    let store = DataStore::load()?;
    if validate_start {
        validate_service_start(store.list(), id, service)?;
    }
    let profile = store
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))?;
    reject_gateway_managed_mcp(&store.settings(), service)?;
    Ok(profile)
}

fn tunnel_is_configured(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> bool {
    match kind {
        TunnelServiceKind::Mcp => profile.tunnel.tunnel_type != "none",
        TunnelServiceKind::Actions => profile.actions.tunnel_type != "none",
    }
}

pub(crate) async fn start_workspace_tunnel(id: &str, service: &str) -> AppResult<TunnelStatus> {
    let kind = TunnelServiceKind::parse(service)?;
    let profile = load_tunnel_workspace(id, kind, true)?;
    if !tunnel_is_configured(&profile, kind) {
        return configured_tunnel_status(&profile, kind);
    }
    let status = control::request_tunnel_operation(
        &profile,
        kind,
        control::ControlTunnelAction::Start,
        MANAGEMENT_TUNNEL_TIMEOUT,
    )
    .await
    .map_err(|error| AppError::Message(format!("daemon 隧道启动失败：{error}")))?;
    persist_tunnel_public_url(id, kind, &status.public_url)?;
    Ok(status)
}

pub(crate) async fn restart_workspace_tunnel(id: &str, service: &str) -> AppResult<TunnelStatus> {
    let kind = TunnelServiceKind::parse(service)?;
    let profile = load_tunnel_workspace(id, kind, true)?;
    if !crate::daemon::inspect(&profile)?.running {
        return configured_tunnel_status(&profile, kind);
    }
    if !tunnel_is_configured(&profile, kind) {
        return configured_tunnel_status(&profile, kind);
    }
    let current = daemon_tunnel_status(&profile, kind).await?;
    let action = if current.state == "running" {
        control::ControlTunnelAction::Restart
    } else {
        control::ControlTunnelAction::Start
    };
    let status =
        control::request_tunnel_operation(&profile, kind, action, MANAGEMENT_TUNNEL_TIMEOUT)
            .await
            .map_err(|error| AppError::Message(format!("daemon 隧道重载失败：{error}")))?;
    persist_tunnel_public_url(id, kind, &status.public_url)?;
    Ok(status)
}

pub(crate) async fn stop_workspace_tunnel(id: &str, service: &str) -> AppResult<TunnelStatus> {
    let kind = TunnelServiceKind::parse(service)?;
    let profile = load_tunnel_workspace(id, kind, false)?;
    if !crate::daemon::inspect(&profile)?.running {
        return configured_tunnel_status(&profile, kind);
    }
    control::request_tunnel_operation(
        &profile,
        kind,
        control::ControlTunnelAction::Stop,
        MANAGEMENT_TUNNEL_TIMEOUT,
    )
    .await
    .map_err(|error| AppError::Message(format!("daemon 隧道停止失败：{error}")))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelTestResult {
    pub success: bool,
    pub public_url: String,
    pub kept_running: bool,
    pub message: String,
}

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
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|error| AppError::Message(format!("创建公网探测客户端失败：{error}")))?;
    let mut last_error = String::new();
    for attempt in 0..5 {
        match client.get(&endpoint).send().await {
            Ok(_) => return Ok(()),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))).await;
    }
    Err(AppError::Message(format!(
        "frpc 已建立代理，但公网地址仍不可访问：{last_error}。若使用 FRP HTTPS→HTTP，请确认服务端字段为 vhostHTTPSPort。"
    )))
}

async fn restore_tunnel_test_runtime(
    profile: &WorkspaceProfile,
    previous: Option<DaemonLaunchSpec>,
) -> AppResult<()> {
    control::reconcile_daemon(profile, previous, MANAGEMENT_TUNNEL_TIMEOUT, true)
        .await
        .map(|_| ())
}

pub(crate) async fn test_workspace_tunnel(id: &str, service: &str) -> AppResult<TunnelTestResult> {
    let kind = TunnelServiceKind::parse(service)?;
    let target_service = workspace_service_for_tunnel(kind);
    let profile = load_tunnel_workspace(id, kind, true)?;
    if !tunnel_is_configured(&profile, kind) {
        return Err(AppError::Message("当前服务未配置隧道。".into()));
    }
    let inspection = crate::daemon::inspect(&profile)?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.running && !inspection.pid_matches {
        return Err(AppError::Message(
            "Workspace daemon reports running but PID ownership does not match".into(),
        ));
    }
    let previous = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .map(|state| DaemonLaunchSpec {
            service: state.service,
            tunnels: state.managed_tunnels(),
        });
    let service_was_running =
        previous.is_some_and(|spec| control::service_is_selected(spec.service, target_service));

    if !service_was_running {
        let desired_service = control::desired_service_selection(
            previous.map(|spec| spec.service),
            target_service,
            true,
        )
        .expect("enabling a service always yields a daemon selection");
        control::reconcile_daemon(
            &profile,
            Some(DaemonLaunchSpec {
                service: desired_service,
                tunnels: previous.and_then(|spec| spec.tunnels),
            }),
            MANAGEMENT_TUNNEL_TIMEOUT,
            true,
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
        match control::request_tunnel_operation(&profile, kind, action, MANAGEMENT_TUNNEL_TIMEOUT)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                if !service_was_running {
                    let _ = restore_tunnel_test_runtime(&profile, previous).await;
                }
                return Err(AppError::Message(format!(
                    "daemon 隧道测试启动失败：{error}"
                )));
            }
        };

    let public_url = status.public_url.clone();
    if let Err(error) = probe_public_tunnel(&public_url, kind).await {
        if !service_was_running {
            let _ = control::request_tunnel_operation(
                &profile,
                kind,
                control::ControlTunnelAction::Stop,
                MANAGEMENT_TUNNEL_TIMEOUT,
            )
            .await;
            let _ = restore_tunnel_test_runtime(&profile, previous).await;
        }
        return Err(error);
    }

    if service_was_running {
        persist_tunnel_public_url(id, kind, &public_url)?;
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
        MANAGEMENT_TUNNEL_TIMEOUT,
    )
    .await
    .map_err(|error| AppError::Message(format!("测试后停止 daemon 隧道失败：{error}")))?;
    restore_tunnel_test_runtime(&profile, previous).await?;

    Ok(TunnelTestResult {
        success: !public_url.is_empty(),
        public_url: public_url.clone(),
        kept_running: false,
        message: if public_url.is_empty() {
            "隧道进程已退出，未获取到公网地址。".into()
        } else {
            "隧道配置验证通过。本地服务未运行，测试连接已自动断开。".into()
        },
    })
}

pub(crate) async fn read_gateway_logs(lines: u32) -> AppResult<gateway_control::GatewayLogChunk> {
    gateway_control::logs_via_daemon_or_local(lines.clamp(1, 5_000), None).await
}

pub(crate) fn windows_service_status() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
    }
    #[cfg(not(windows))]
    Ok(serde_json::json!({
        "supported": false,
        "serviceName": "",
        "installed": false,
        "state": "unsupported",
        "autoStart": false,
        "configDir": "",
        "planPath": "",
        "plan": {
            "schemaVersion": 1,
            "ownerSid": "",
            "ownerUsername": "",
            "workspaces": [],
            "gatewayWorkspaceIds": []
        }
    }))
}

fn control_log_service(service: &str) -> AppResult<ControlLogSelection> {
    Ok(match service {
        "mcp" => ControlLogSelection::Mcp,
        "actions" => ControlLogSelection::Actions,
        other => return Err(AppError::Message(format!("unknown log service: {other}"))),
    })
}

pub(crate) fn gui_log_chunks(chunks: Vec<ControlLogChunk>) -> Vec<LogChunk> {
    chunks
        .into_iter()
        .filter(|chunk| chunk.exists)
        .map(|chunk| LogChunk {
            name: Path::new(&chunk.path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&chunk.name)
                .to_string(),
            content: chunk.content,
        })
        .collect()
}

pub(crate) async fn read_workspace_logs(id: &str, service: &str) -> AppResult<Vec<LogChunk>> {
    let store = DataStore::load()?;
    let profile = store
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))?;
    drop(store);
    let selection = control_log_service(service)?;
    let chunks = if crate::daemon::inspect(&profile)?.running {
        control::request_logs(&profile, selection, 5_000, Vec::new())
            .await
            .map_err(|error| {
                AppError::Message(format!(
                    "daemon 日志请求失败：{error}；运行中的 daemon 不会回退到管理端直接文件读取"
                ))
            })?
    } else {
        control::read_log_batch(&profile, selection, 5_000, &[])?
    };
    Ok(gui_log_chunks(chunks))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogChunk {
    pub name: String,
    pub content: String,
}

const WORKSPACE_SECRET_KEYS: &[&str] = &[
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
    "bearer_token",
    "cloudflare_token",
    "actions_cloudflare_token",
    "actions_api_key",
    "actions_oauth_client_secret",
    "actions_oauth_password",
    "actions_oauth_token_secret",
    "frp_token",
    "actions_frp_token",
];

const SHARED_SECRET_KEYS: &[&str] = &[
    "oauth_client_id",
    "bearer_token",
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
    "actions_api_key",
    "actions_oauth_client_secret",
    "actions_oauth_password",
    "actions_oauth_token_secret",
];

pub(crate) fn validate_workspace_secret_key(key: &str) -> AppResult<()> {
    if WORKSPACE_SECRET_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(AppError::Message(format!("invalid secret key: {key}")))
    }
}

pub(crate) fn validate_shared_secret_key(key: &str) -> AppResult<()> {
    if SHARED_SECRET_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(AppError::Message(format!("invalid shared key: {key}")))
    }
}

pub(crate) fn get_workspace_secret(id: &str, key: &str) -> AppResult<Option<String>> {
    validate_workspace_secret_key(key)?;
    DataStore::read_file(|data| {
        if !data.profiles.iter().any(|profile| profile.id == id) {
            return Err(AppError::Message(format!("workspace not found: {id}")));
        }
        Ok(data
            .workspace_secrets
            .get(id)
            .and_then(|secrets| secrets.get(key))
            .filter(|value| !value.is_empty())
            .cloned())
    })
}

pub(crate) fn get_shared_secret(key: &str) -> AppResult<Option<String>> {
    validate_shared_secret_key(key)?;
    DataStore::read_file(|data| Ok(data.shared_secrets.get(key).cloned()))
}

pub(crate) fn list_frp_profiles() -> AppResult<Vec<FrpProfileDto>> {
    DataStore::read_file(|data| {
        Ok(data
            .frp_profiles
            .iter()
            .map(|profile| {
                let has_token = data
                    .app_secrets
                    .get("frp_profile_token")
                    .and_then(|tokens| tokens.get(&profile.id))
                    .is_some_and(|value| !value.trim().is_empty());
                FrpProfileDto {
                    id: profile.id.clone(),
                    name: profile.name.clone(),
                    server: profile.server.clone(),
                    server_port: profile.server_port,
                    has_token,
                }
            })
            .collect())
    })
}

pub(crate) fn get_last_workspace_id() -> AppResult<String> {
    DataStore::read_file(|data| Ok(data.last_workspace_id.clone()))
}

pub(crate) fn set_last_workspace(id: String) -> AppResult<()> {
    DataStore::update_file(|data| {
        data.last_workspace_id = id;
        Ok(())
    })
}

pub(crate) fn get_proxy() -> AppResult<ProxyConfig> {
    DataStore::read_file(|data| Ok(data.proxy.clone()))
}

pub(crate) fn set_proxy(proxy: ProxyConfig) -> AppResult<()> {
    DataStore::update_file(|data| {
        data.proxy = proxy;
        Ok(())
    })
}

pub(crate) fn get_download_config() -> AppResult<DownloadConfig> {
    DataStore::read_file(|data| Ok(data.download.clone()))
}

pub(crate) fn set_download_config(config: DownloadConfig) -> AppResult<()> {
    DataStore::update_file(|data| {
        data.download = config;
        Ok(())
    })
}

fn selection_includes(
    selection: crate::daemon::ServiceSelection,
    service: WorkspaceService,
) -> bool {
    match service {
        WorkspaceService::Mcp => selection.includes_mcp(),
        WorkspaceService::Actions => selection.includes_actions(),
    }
}

fn empty_recovery(enabled: bool, last_error: String) -> RuntimeRecoveryDto {
    RuntimeRecoveryDto {
        enabled,
        attempt: 0,
        max_attempts: 0,
        retry_in_ms: None,
        recovered_count: 0,
        last_error,
    }
}

pub(crate) fn runtime_status_from_control(
    profile: &WorkspaceProfile,
    settings: &AppSettings,
    status: &WorkspaceControlStatus,
    service: WorkspaceService,
) -> RuntimeStatusDto {
    let active_tunnel_url = match service {
        WorkspaceService::Mcp => status.mcp_tunnel.as_ref(),
        WorkspaceService::Actions => status.actions_tunnel.as_ref(),
    }
    .filter(|tunnel| tunnel.state == "running")
    .map(|tunnel| tunnel.public_url.trim().trim_end_matches('/'))
    .filter(|url| !url.is_empty());
    let (port, local_endpoint, public_endpoint, public_message, label) = match service {
        WorkspaceService::Mcp => {
            let fallback_base = profile.mcp_external_base_url_with(settings);
            let public_base = active_tunnel_url.unwrap_or(fallback_base.as_str());
            (
                &status.mcp,
                profile.local_endpoint(),
                if public_base.is_empty() {
                    String::new()
                } else {
                    format!("{public_base}/mcp")
                },
                public_base.to_string(),
                "MCP",
            )
        }
        WorkspaceService::Actions => {
            let fallback_base = profile.actions_effective_public_url_with(settings);
            let public_base = active_tunnel_url.unwrap_or(fallback_base.as_str());
            (
                &status.actions,
                profile.actions_local_base_url(),
                if public_base.is_empty() {
                    String::new()
                } else {
                    format!("{public_base}/openapi.json")
                },
                public_base.to_string(),
                "Actions",
            )
        }
    };
    let daemon_pid = status
        .daemon
        .state
        .as_ref()
        .filter(|_| status.daemon.running)
        .map(|state| state.pid);
    let selected = status
        .daemon
        .state
        .as_ref()
        .filter(|_| status.daemon.running)
        .is_some_and(|state| selection_includes(state.service, service));

    let (state, pid, local_message, recovery) = if status.daemon.ambiguous
        || (status.daemon.stale && status.daemon.state.is_some())
    {
        (
            "error",
            daemon_pid,
            status.daemon.detail.clone(),
            empty_recovery(false, status.daemon.detail.clone()),
        )
    } else if selected && port.owner == "daemon" {
        (
            "running",
            daemon_pid,
            format!("{label} 由 daemon 监听 127.0.0.1:{}", port.port),
            empty_recovery(false, String::new()),
        )
    } else if port.owner == "server" {
        #[cfg(windows)]
        {
            let message = format!(
                "检测到旧版 Windows GUI process-local {label} listener 占用端口 {}；当前版本不会接管该运行态，请先退出旧桌面进程",
                port.port
            );
            (
                "error",
                port.pid,
                message.clone(),
                empty_recovery(false, message),
            )
        }
        #[cfg(not(windows))]
        {
            (
                "running",
                port.pid,
                format!("{label} 由桌面进程监听 127.0.0.1:{}", port.port),
                empty_recovery(false, String::new()),
            )
        }
    } else if port.owner == "external" {
        let message = format!(
            "{label} 端口 {} 由外部 PID {} 占用，GUI 不会接管该进程",
            port.port,
            port.pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
        (
            "error",
            port.pid,
            message.clone(),
            empty_recovery(false, message),
        )
    } else if selected && status.daemon.running {
        let message = format!(
            "{label} daemon 正在运行，但端口 {} 暂未监听；等待 daemon 自动恢复",
            port.port
        );
        (
            "recovering",
            daemon_pid,
            message.clone(),
            empty_recovery(true, message),
        )
    } else if !status.daemon.supported {
        (
            "stopped",
            None,
            format!("未启动；{}", status.daemon.detail),
            empty_recovery(false, String::new()),
        )
    } else {
        (
            "stopped",
            None,
            "未启动".into(),
            empty_recovery(false, String::new()),
        )
    };

    RuntimeStatusDto {
        state: state.into(),
        pid,
        local_message,
        public_message: if public_message.is_empty() {
            "未配置公网访问".into()
        } else {
            public_message
        },
        local_endpoint,
        public_endpoint,
        recovery,
        activity: match service {
            WorkspaceService::Mcp => status.mcp_activity.clone(),
            WorkspaceService::Actions => None,
        },
    }
}

pub(crate) async fn runtime_status(
    id: &str,
    service: WorkspaceService,
) -> AppResult<RuntimeStatusDto> {
    let store = DataStore::load()?;
    let profile = store
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))?;
    let settings = store.settings();
    let profiles = store.list().to_vec();
    drop(store);

    #[cfg(windows)]
    if service == WorkspaceService::Mcp
        && settings.mcp_gateway.enabled
        && crate::gateway_daemon::supported()
    {
        return gateway_route_runtime_status(&profile, &settings, &profiles).await;
    }
    #[cfg(not(windows))]
    let _ = &profiles;

    let status = control::workspace_status_via_daemon_or_local(&profile).await?;
    Ok(runtime_status_from_control(
        &profile, &settings, &status, service,
    ))
}

#[cfg(windows)]
async fn gateway_route_runtime_status(
    profile: &WorkspaceProfile,
    settings: &AppSettings,
    profiles: &[WorkspaceProfile],
) -> AppResult<RuntimeStatusDto> {
    let plane = control::control_plane_status(profiles).await?;
    let workspace = plane
        .workspaces
        .iter()
        .find(|workspace| workspace.status.id == profile.id)
        .ok_or_else(|| AppError::Message(format!("workspace not found: {}", profile.id)))?;
    let mut status =
        runtime_status_from_control(profile, settings, &workspace.status, WorkspaceService::Mcp);
    status.state = workspace.mcp_state.clone();
    let routed = plane
        .gateway
        .route_workspace_ids
        .iter()
        .any(|workspace_id| workspace_id == &profile.id);
    if routed {
        status.pid = plane.gateway.pid;
        status.local_message = match workspace.mcp_state.as_str() {
            "running" => format!(
                "MCP 由 Gateway daemon 路由并监听 127.0.0.1:{}",
                profile.runtime.local_port
            ),
            "recovering" => format!(
                "Gateway daemon 已选择该工作区，但 MCP 端口 {} 尚未就绪",
                profile.runtime.local_port
            ),
            "error" => format!(
                "Gateway daemon 路由异常：{}",
                if plane.gateway.error.is_empty() {
                    plane.gateway.detail.as_str()
                } else {
                    plane.gateway.error.as_str()
                }
            ),
            _ => "Gateway daemon 未启动该工作区 MCP route".into(),
        };
        let public_base = plane.gateway.public_base_url.trim().trim_end_matches('/');
        if !public_base.is_empty() {
            status.public_message = public_base.to_string();
            status.public_endpoint = format!("{public_base}/w/{}/mcp", profile.id);
        }
    } else {
        status.pid = None;
        status.state = "stopped".into();
        status.local_message = "未启动".into();
        status.recovery.enabled = false;
    }
    Ok(status)
}

pub(crate) async fn workspace_control_status(id: &str) -> AppResult<WorkspaceControlStatus> {
    let store = DataStore::load()?;
    let profile = store
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))?;
    drop(store);
    control::workspace_status_via_daemon_or_local(&profile).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway_inspection(
        running: bool,
        ambiguous: bool,
        pid_matches: bool,
    ) -> crate::gateway_daemon::GatewayDaemonInspection {
        crate::gateway_daemon::GatewayDaemonInspection {
            supported: true,
            running,
            stale: false,
            ambiguous,
            pid_matches,
            state: running.then(|| crate::gateway_daemon::GatewayDaemonState {
                schema_version: 2,
                config_scope: "scope".into(),
                pid: 42,
                started_at_unix: 1,
                workspace_ids: vec!["workspace".into()],
                local_port: 28_765,
                log_path: "gateway.log".into(),
                version: "test".into(),
                build_identity: None,
                executable_path: "anchor".into(),
            }),
            detail: if ambiguous {
                "ambiguous".into()
            } else {
                "ok".into()
            },
        }
    }

    #[test]
    fn gateway_config_write_policy_never_falls_back_while_daemon_is_running() {
        let running = gateway_inspection(true, false, true);
        assert_eq!(
            gateway_config_write_action(&running, true).expect("enabled action"),
            GatewayConfigWriteAction::ApplyViaDaemon { pid: 42 }
        );
        assert_eq!(
            gateway_config_write_action(&running, false).expect("disabled action"),
            GatewayConfigWriteAction::ShutdownThenPersist { pid: 42 }
        );

        let stopped = gateway_inspection(false, false, false);
        assert_eq!(
            gateway_config_write_action(&stopped, true).expect("stopped action"),
            GatewayConfigWriteAction::PersistLocally
        );

        assert!(
            gateway_config_write_action(&gateway_inspection(false, true, false), true).is_err()
        );
        assert!(
            gateway_config_write_action(&gateway_inspection(true, false, false), true).is_err()
        );
    }

    #[test]
    fn gateway_enabled_mcp_service_control_is_fail_closed() {
        let mut settings = AppSettings::default();
        settings.mcp_gateway.enabled = true;
        assert!(reject_gateway_managed_mcp(&settings, WorkspaceService::Mcp).is_err());
        assert!(reject_gateway_managed_mcp(&settings, WorkspaceService::Actions).is_ok());
    }

    #[test]
    fn gateway_route_selection_is_sorted_deduplicated_and_idempotent() {
        let current = vec!["workspace-b".into(), "workspace-a".into()];
        assert_eq!(
            desired_gateway_routes(&current, "workspace-a", true),
            vec!["workspace-a", "workspace-b"]
        );
        assert_eq!(
            desired_gateway_routes(&current, "workspace-c", true),
            vec!["workspace-a", "workspace-b", "workspace-c"]
        );
        assert_eq!(
            desired_gateway_routes(&current, "workspace-b", false),
            vec!["workspace-a"]
        );
    }
}

pub(crate) async fn workspace_control_events(
    id: &str,
    cursor: Option<ControlEventCursor>,
    wait_ms: u32,
) -> AppResult<Option<ControlEventBatch>> {
    let store = DataStore::load()?;
    let profile = store
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))?;
    drop(store);
    match control::request_events(&profile, cursor, 64, wait_ms).await {
        Ok(batch) => Ok(Some(batch)),
        Err(error) if error.is_unavailable() => Ok(None),
        Err(error) => Err(AppError::Message(error.to_string())),
    }
}
