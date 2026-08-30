mod args;
mod config;
mod frp;
#[cfg(unix)]
mod handoff;
mod ilink;
mod plugin;
mod software;
mod tunnel;
mod upgrade;
mod workspace;

pub(crate) use args::ConfigApplyOptions;
pub(crate) use config::{
    apply_staged_config, preview_profile_config, stage_profile_config, ConfigApplyReport,
    ConfigSetReport,
};

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;

use crate::control::{self, WorkspaceControlStatus};
use crate::daemon;
use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::gateway_control::{self, GatewayOperation};
use crate::gateway_daemon;
use crate::logging::{profile_log_files, ProfileLogService};
use crate::mcp::gateway;
use crate::platform::platform;
use crate::runtime::{
    await_listener_shutdown, loopback_port_bindable, update_public_url, RuntimeSupervisor,
    ServiceKind,
};
use crate::settings::McpGatewayConfig;
use crate::tunnel::{
    ensure_for_runtime, is_quick_tunnel_url_change_error, log_dir_for_profile,
    maybe_start_for_runtime, reconcile_mcp_gateway, stop_for_runtime,
    supervisor as tunnel_supervisor, TunnelServiceKind, TunnelStatus,
};
use crate::workspace::config_apply::plan_workspace_config_apply;
use crate::workspace::{RuntimeStatusDto, WorkspaceProfile};

use args::{
    AdminCommand, CliArgs, Command, EventsOptions, EventsTarget, GatewayCommand,
    GatewayConfigureOptions, GatewayEventsOptions, GatewayLogsOptions, GatewayStartOptions,
    GatewayStopOptions, LogSelection, LogsOptions, ReloadOptions, RunOptions, ServiceCommand,
    ServiceSelection, StatusOptions, StopOptions,
};

#[derive(Debug, Clone, Copy)]
struct CliTunnelRetry {
    attempts: u8,
    next_attempt: tokio::time::Instant,
}

async fn execute_admin(command: AdminCommand, as_json: bool) -> AppResult<i32> {
    match command {
        AdminCommand::Serve { port } => crate::admin::serve(port, as_json).await.map(|_| 0),
        AdminCommand::DaemonRun { port } => {
            crate::admin_daemon::run(port, as_json).await.map(|_| 0)
        }
        AdminCommand::Start { port } => {
            print_admin_service_status(crate::admin_service::start(port).await?, as_json)
        }
        AdminCommand::Stop { force } => {
            print_admin_service_status(crate::admin_service::stop(force).await?, as_json)
        }
        AdminCommand::Restart { port, force } => {
            print_admin_service_status(crate::admin_service::restart(port, force).await?, as_json)
        }
        AdminCommand::Status => {
            print_admin_service_status(crate::admin_service::status().await?, as_json)
        }
        AdminCommand::Install { port } => {
            print_admin_service_status(crate::admin_service::install(port).await?, as_json)
        }
        AdminCommand::Uninstall { force } => {
            print_admin_service_status(crate::admin_service::uninstall(force).await?, as_json)
        }
        AdminCommand::Enable => {
            print_admin_service_status(crate::admin_service::enable().await?, as_json)
        }
        AdminCommand::Disable => {
            print_admin_service_status(crate::admin_service::disable().await?, as_json)
        }
        AdminCommand::Upgrade => {
            print_admin_service_status(crate::admin_service::upgrade().await?, as_json)
        }
    }
}

fn print_admin_service_status(
    status: crate::admin_service::AdminServiceStatus,
    as_json: bool,
) -> AppResult<i32> {
    if as_json {
        print_json(&status)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&status)?);
    }
    Ok(0)
}

fn normalize_gateway_route_ids(workspace_ids: Vec<String>) -> AppResult<Vec<String>> {
    let mut normalized = Vec::with_capacity(workspace_ids.len());
    for workspace_id in workspace_ids {
        let workspace_id = workspace_id.trim();
        if workspace_id.is_empty() {
            return Err(AppError::Message(
                "Gateway route workspace id 不能为空".into(),
            ));
        }
        normalized.push(workspace_id.to_string());
    }
    normalized.sort();
    normalized.dedup();
    if normalized.is_empty() {
        return Err(AppError::Message(
            "Gateway set_routes 至少需要一个 workspace；零 route 请使用 shutdown".into(),
        ));
    }
    Ok(normalized)
}

fn resolve_gateway_route_profiles(
    profiles: &[WorkspaceProfile],
    workspace_ids: &[String],
) -> AppResult<Vec<WorkspaceProfile>> {
    let mut selected = Vec::with_capacity(workspace_ids.len());
    for workspace_id in workspace_ids {
        let profile = profiles
            .iter()
            .find(|profile| profile.id == *workspace_id)
            .cloned()
            .ok_or_else(|| {
                AppError::Message(format!("Gateway route workspace 不存在：{workspace_id}"))
            })?;
        ensure_workspace_directory(&profile)?;
        selected.push(profile);
    }
    Ok(selected)
}

async fn apply_gateway_routes(
    runtime: &mut RuntimeSupervisor,
    started: &mut Vec<WorkspaceProfile>,
    selected: &mut Vec<WorkspaceProfile>,
    config: &mut McpGatewayConfig,
    all_profiles: &mut Vec<WorkspaceProfile>,
    workspace_ids: Vec<String>,
) -> AppResult<()> {
    let desired_ids = normalize_gateway_route_ids(workspace_ids)?;
    let mut previous_ids = selected
        .iter()
        .map(|profile| profile.id.clone())
        .collect::<Vec<_>>();
    previous_ids.sort();
    previous_ids.dedup();
    if desired_ids == previous_ids {
        gateway_daemon::update_state(&previous_ids, config.local_port)?;
        return Ok(());
    }

    let previous = GatewayRuntimeSnapshot {
        config: config.clone(),
        profiles: all_profiles.clone(),
        selected: selected.clone(),
    };
    let store = DataStore::load()?;
    let latest_profiles = store.list().to_vec();
    let mut latest_config = store.settings().mcp_gateway;
    drop(store);
    if !latest_config.enabled {
        return Err(AppError::Message(
            "Gateway 配置已禁用；不能修改运行 route".into(),
        ));
    }
    gateway::validate_config(&latest_config, &latest_profiles)?;
    let latest_selected = resolve_gateway_route_profiles(&latest_profiles, &desired_ids)?;

    shutdown_gateway_services(runtime, started, config, all_profiles).await?;
    started.clear();
    match start_gateway_services(
        runtime,
        &latest_selected,
        &mut latest_config,
        &latest_profiles,
    )
    .await
    {
        Ok(next_started) => {
            *started = next_started;
            *selected = latest_selected;
            *config = latest_config;
            *all_profiles = latest_profiles;
            if let Err(state_error) = gateway_daemon::update_state(&desired_ids, config.local_port)
            {
                let cleanup =
                    shutdown_gateway_services(runtime, started, config, all_profiles).await;
                if let Err(cleanup_error) = cleanup {
                    return Err(AppError::Message(format!(
                        "Gateway route 已切换但 daemon state 更新失败：{state_error}；清理新运行态也失败：{cleanup_error}"
                    )));
                }
                started.clear();
                return match restore_gateway_runtime(
                    runtime,
                    started,
                    selected,
                    config,
                    all_profiles,
                    previous,
                    &previous_ids,
                )
                .await
                {
                    Ok(()) => Err(AppError::Message(format!(
                        "Gateway route state 更新失败，已恢复旧 routes：{state_error}"
                    ))),
                    Err(rollback_error) => Err(AppError::Message(format!(
                        "Gateway route state 更新失败：{state_error}；恢复旧 routes 也失败：{rollback_error}"
                    ))),
                };
            }
            Ok(())
        }
        Err(error) => match restore_gateway_runtime(
            runtime,
            started,
            selected,
            config,
            all_profiles,
            previous,
            &previous_ids,
        )
        .await
        {
            Ok(()) => Err(AppError::Message(format!(
                "Gateway route 切换失败，已恢复旧 routes：{error}"
            ))),
            Err(rollback_error) => Err(AppError::Message(format!(
                "Gateway route 切换失败：{error}；恢复旧 routes 也失败：{rollback_error}"
            ))),
        },
    }
}

struct DaemonOwnership {
    guard: Option<daemon::DaemonGuard>,
    control_server: Option<control::ControlServer>,
}

impl DaemonOwnership {
    fn release(&mut self) {
        self.control_server.take();
        self.guard.take();
    }
}

#[derive(Default)]
struct WorkspaceServeContext {
    ownership: Option<DaemonOwnership>,
    #[cfg(unix)]
    imported_listeners: Option<handoff::ImportedListeners>,
    #[cfg(unix)]
    handoff_id: Option<String>,
}

fn execute_service(command: ServiceCommand, as_json: bool) -> AppResult<i32> {
    #[cfg(windows)]
    {
        if matches!(
            &command,
            ServiceCommand::Install | ServiceCommand::Start | ServiceCommand::Restart
        ) {
            // Direct CLI service administration does not pass through the
            // desktop UAC helper. Prepare the LocalMachine service mirror
            // while the CLI still has the config owner's DPAPI identity.
            let _ = DataStore::load()?;
        }
        let status = match command {
            ServiceCommand::Status => crate::windows_service::scm_status()?,
            ServiceCommand::Install => crate::windows_service::install_scm_service()?,
            ServiceCommand::Uninstall => crate::windows_service::uninstall_scm_service()?,
            ServiceCommand::Start => crate::windows_service::start_scm_service()?,
            ServiceCommand::Stop => crate::windows_service::stop_scm_service()?,
            ServiceCommand::Restart => crate::windows_service::restart_scm_service()?,
            ServiceCommand::Sync => {
                let _ = crate::windows_service::sync_plan_from_running()?;
                crate::windows_service::scm_status()?
            }
        };
        if as_json {
            print_json(&status)?;
        } else {
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Ok(0)
    }

    #[cfg(target_os = "linux")]
    {
        let status = match command {
            ServiceCommand::Status => crate::linux_service::service_status()?,
            ServiceCommand::Install => crate::linux_service::install_service()?,
            ServiceCommand::Uninstall => crate::linux_service::uninstall_service()?,
            ServiceCommand::Start => crate::linux_service::start_service()?,
            ServiceCommand::Stop => crate::linux_service::stop_service()?,
            ServiceCommand::Restart => crate::linux_service::restart_service()?,
            ServiceCommand::Sync => {
                let _ = crate::linux_service::sync_plan_from_running()?;
                crate::linux_service::service_status()?
            }
        };
        if as_json {
            print_json(&status)?;
        } else {
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Ok(0)
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (command, as_json);
        Err(AppError::Message(
            "Control Plane Service 当前仅支持 Windows 和 Linux".into(),
        ))
    }
}

async fn show_control_plane_status(options: StatusOptions, as_json: bool) -> AppResult<()> {
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    let mut event_cursor = None;
    loop {
        let store = DataStore::load()?;
        let profiles = store.list().to_vec();
        drop(store);
        let workspace_ids = profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        let status = control::control_plane_status(&profiles).await?;
        if as_json && options.watch {
            print_json_line(&json!({
                "event": "control_plane_status_snapshot",
                "controlPlane": status
            }))?;
        } else if as_json {
            print_json(&status)?;
        } else {
            print_control_plane_status(&status);
        }
        if !options.watch {
            return Ok(());
        }
        let wait_ms = u32::try_from(options.interval_seconds.saturating_mul(1_000))
            .unwrap_or(25_000)
            .min(25_000);
        let wait = control::control_plane_events(&profiles, event_cursor.clone(), 64, wait_ms);
        let batch = tokio::select! {
            result = &mut shutdown_signal => return result,
            result = wait => result?,
        };
        event_cursor = Some(batch.next_cursor);

        if batch.events.is_empty() && batch.reset_sources.is_empty() {
            let latest_store = DataStore::load()?;
            let latest_ids = latest_store
                .list()
                .iter()
                .map(|profile| profile.id.clone())
                .collect::<Vec<_>>();
            if latest_ids == workspace_ids {
                continue;
            }
        }
    }
}

fn print_control_plane_status(status: &control::ControlPlaneStatus) {
    println!(
        "Gateway\t{}\troutes={}{}",
        status.gateway.state,
        status.gateway.route_count,
        status
            .gateway
            .pid
            .map(|pid| format!("\tpid={pid}"))
            .unwrap_or_default()
    );
    for workspace in &status.workspaces {
        println!(
            "{} ({})\tMCP={}\tActions={}",
            workspace.status.name,
            workspace.status.id,
            workspace.mcp_state,
            workspace.actions_state
        );
    }
}

fn publish_gateway_runtime_event(
    event_scope: Option<&str>,
    kind: gateway_control::GatewayEventKind,
    state: impl Into<String>,
    message: impl Into<String>,
) {
    if let Some(scope) = event_scope {
        gateway_control::publish_gateway_event(scope, kind, state, message);
    }
}

#[derive(Debug, Clone)]
struct GatewayRuntimeSnapshot {
    config: McpGatewayConfig,
    profiles: Vec<WorkspaceProfile>,
    selected: Vec<WorkspaceProfile>,
}

async fn apply_gateway_config(
    runtime: &mut RuntimeSupervisor,
    started: &mut Vec<WorkspaceProfile>,
    selected: &mut Vec<WorkspaceProfile>,
    config: &mut McpGatewayConfig,
    all_profiles: &mut Vec<WorkspaceProfile>,
    mut next_config: McpGatewayConfig,
) -> AppResult<()> {
    if !next_config.enabled {
        return Err(AppError::Message(
            "运行中的 Gateway 不能通过 apply_config 禁用；请先执行 shutdown".into(),
        ));
    }
    let previous = GatewayRuntimeSnapshot {
        config: config.clone(),
        profiles: all_profiles.clone(),
        selected: selected.clone(),
    };
    let selected_ids = selected
        .iter()
        .map(|profile| profile.id.clone())
        .collect::<Vec<_>>();

    let latest_store = DataStore::load()?;
    let latest_profiles = latest_store.list().to_vec();
    drop(latest_store);
    gateway::validate_config(&next_config, &latest_profiles)?;
    let mut next_selected = Vec::new();
    for workspace_id in &selected_ids {
        let profile = resolve_workspace(&latest_profiles, workspace_id)?.clone();
        ensure_workspace_directory(&profile)?;
        next_selected.push(profile);
    }

    shutdown_gateway_services(runtime, started, config, all_profiles).await?;
    started.clear();
    match start_gateway_services(runtime, &next_selected, &mut next_config, &latest_profiles).await
    {
        Ok(next_started) => {
            *started = next_started;
            *selected = next_selected;
            *config = next_config;
            *all_profiles = latest_profiles;

            if let Err(state_error) = gateway_daemon::update_state(&selected_ids, config.local_port)
            {
                let cleanup =
                    shutdown_gateway_services(runtime, started, config, all_profiles).await;
                if let Err(cleanup_error) = cleanup {
                    return Err(AppError::Message(format!(
                        "Gateway 新运行态已建立但 daemon state 更新失败：{state_error}；清理新运行态也失败：{cleanup_error}"
                    )));
                }
                started.clear();
                return match restore_gateway_runtime(
                    runtime,
                    started,
                    selected,
                    config,
                    all_profiles,
                    previous.clone(),
                    &selected_ids,
                )
                .await
                {
                    Ok(()) => Err(AppError::Message(format!(
                        "Gateway daemon state 更新失败，已恢复旧运行态：{state_error}"
                    ))),
                    Err(rollback_error) => Err(AppError::Message(format!(
                        "Gateway daemon state 更新失败：{state_error}；恢复旧运行态也失败：{rollback_error}"
                    ))),
                };
            }

            if let Err(persist_error) = gateway_control::persist_config(config) {
                let cleanup =
                    shutdown_gateway_services(runtime, started, config, all_profiles).await;
                if let Err(cleanup_error) = cleanup {
                    return Err(AppError::Message(format!(
                        "Gateway 新运行态已建立但持久化失败：{persist_error}；清理新运行态也失败：{cleanup_error}"
                    )));
                }
                started.clear();
                return match restore_gateway_runtime(
                    runtime,
                    started,
                    selected,
                    config,
                    all_profiles,
                    previous,
                    &selected_ids,
                )
                .await
                {
                    Ok(()) => Err(AppError::Message(format!(
                        "Gateway 配置持久化失败，已恢复旧运行态：{persist_error}"
                    ))),
                    Err(rollback_error) => Err(AppError::Message(format!(
                        "Gateway 配置持久化失败：{persist_error}；恢复旧运行态也失败：{rollback_error}"
                    ))),
                };
            }
            Ok(())
        }
        Err(error) => {
            match restore_gateway_runtime(
                runtime,
                started,
                selected,
                config,
                all_profiles,
                previous,
                &selected_ids,
            )
            .await
            {
                Ok(()) => Err(AppError::Message(format!(
                    "Gateway 配置应用失败，已恢复旧运行态：{error}"
                ))),
                Err(rollback_error) => Err(AppError::Message(format!(
                    "Gateway 配置应用失败：{error}；恢复旧运行态也失败：{rollback_error}"
                ))),
            }
        }
    }
}

async fn restore_gateway_runtime(
    runtime: &mut RuntimeSupervisor,
    started: &mut Vec<WorkspaceProfile>,
    selected: &mut Vec<WorkspaceProfile>,
    config: &mut McpGatewayConfig,
    all_profiles: &mut Vec<WorkspaceProfile>,
    previous: GatewayRuntimeSnapshot,
    selected_ids: &[String],
) -> AppResult<()> {
    let mut rollback_config = previous.config;
    let previous_started = start_gateway_services(
        runtime,
        &previous.selected,
        &mut rollback_config,
        &previous.profiles,
    )
    .await?;
    *started = previous_started;
    *selected = previous.selected;
    *config = rollback_config;
    *all_profiles = previous.profiles;
    gateway_daemon::update_state(selected_ids, config.local_port)
}

async fn start_gateway_services(
    runtime: &mut RuntimeSupervisor,
    selected: &[WorkspaceProfile],
    config: &mut McpGatewayConfig,
    all_profiles: &[WorkspaceProfile],
) -> AppResult<Vec<WorkspaceProfile>> {
    ensure_gateway_ports_available(config, selected)?;
    let mut started = Vec::new();
    let startup = async {
        for profile in selected {
            ensure_running(runtime.start_mcp(profile)?, "MCP")?;
            started.push(profile.clone());
        }
        let active = runtime.active_mcp_workspace_ids();
        gateway::ensure(config, all_profiles, &active).await?;
        if let Some(url) = reconcile_mcp_gateway(config, all_profiles, &active).await? {
            persist_cli_gateway_observation(config, all_profiles, &url)?;
        }
        Ok::<(), AppError>(())
    }
    .await;
    if let Err(error) = startup {
        let cleanup = shutdown_gateway_services(runtime, &started, config, all_profiles).await;
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(AppError::Message(format!(
                "Gateway 启动失败：{error}；清理已启动服务也失败：{cleanup_error}"
            ))),
        };
    }
    Ok(started)
}

async fn apply_gateway_reload(
    runtime: &mut RuntimeSupervisor,
    started: &mut Vec<WorkspaceProfile>,
    selected: &mut Vec<WorkspaceProfile>,
    config: &mut McpGatewayConfig,
    all_profiles: &mut Vec<WorkspaceProfile>,
) -> AppResult<()> {
    let previous = GatewayRuntimeSnapshot {
        config: config.clone(),
        profiles: all_profiles.clone(),
        selected: selected.clone(),
    };
    let selected_ids = selected
        .iter()
        .map(|profile| profile.id.clone())
        .collect::<Vec<_>>();

    let store = DataStore::load()?;
    let latest_profiles = store.list().to_vec();
    let mut latest_config = store.settings().mcp_gateway;
    if !latest_config.enabled {
        return Err(AppError::Message(
            "Gateway 配置已禁用；请使用 gateway stop 关闭当前 daemon".into(),
        ));
    }
    gateway::validate_config(&latest_config, &latest_profiles)?;
    let mut latest_selected = Vec::new();
    for workspace_id in &selected_ids {
        let profile = resolve_workspace(&latest_profiles, workspace_id)?.clone();
        ensure_workspace_directory(&profile)?;
        latest_selected.push(profile);
    }
    drop(store);

    shutdown_gateway_services(runtime, started, config, all_profiles).await?;
    started.clear();
    match start_gateway_services(
        runtime,
        &latest_selected,
        &mut latest_config,
        &latest_profiles,
    )
    .await
    {
        Ok(next_started) => {
            *started = next_started;
            *selected = latest_selected;
            *config = latest_config;
            *all_profiles = latest_profiles;
            if let Err(state_error) = gateway_daemon::update_state(&selected_ids, config.local_port)
            {
                let cleanup =
                    shutdown_gateway_services(runtime, started, config, all_profiles).await;
                if let Err(cleanup_error) = cleanup {
                    return Err(AppError::Message(format!(
                        "Gateway reload 后 daemon state 更新失败：{state_error}；清理新运行态也失败：{cleanup_error}"
                    )));
                }
                started.clear();
                return match restore_gateway_runtime(
                    runtime,
                    started,
                    selected,
                    config,
                    all_profiles,
                    previous,
                    &selected_ids,
                )
                .await
                {
                    Ok(()) => Err(AppError::Message(format!(
                        "Gateway reload 后 daemon state 更新失败，已恢复旧运行态：{state_error}"
                    ))),
                    Err(rollback_error) => Err(AppError::Message(format!(
                        "Gateway reload 后 daemon state 更新失败：{state_error}；恢复旧运行态也失败：{rollback_error}"
                    ))),
                };
            }
            Ok(())
        }
        Err(error) => {
            let rollback = restore_gateway_runtime(
                runtime,
                started,
                selected,
                config,
                all_profiles,
                previous,
                &selected_ids,
            )
            .await;
            match rollback {
                Ok(()) => Err(AppError::Message(format!(
                    "Gateway reload 失败，已恢复旧运行态：{error}"
                ))),
                Err(rollback_error) => Err(AppError::Message(format!(
                    "Gateway reload 失败：{error}；恢复旧运行态也失败：{rollback_error}"
                ))),
            }
        }
    }
}

async fn run_gateway_daemon(config_scope: &str, workspaces: &[String]) -> AppResult<()> {
    #[cfg(windows)]
    crate::logging::redirect_stdio_to_file(&gateway_daemon::daemon_log_path()?)?;
    if config_scope != gateway_daemon::config_scope()? {
        return Err(AppError::Message(
            "Gateway daemon child config scope does not match the active config domain".into(),
        ));
    }
    let workspace_ids = resolve_gateway_workspace_ids(workspaces)?;
    let store = DataStore::load()?;
    let config = store.settings().mcp_gateway;
    drop(store);
    let _guard = gateway_daemon::acquire(&workspace_ids, config.local_port)?;
    gateway_control::reset_gateway_event_stream(config_scope);
    let (sender, receiver) = gateway_control::control_channel();
    let server = gateway_control::GatewayControlServer::start(sender)?;
    gateway_control::publish_gateway_event(
        config_scope,
        gateway_control::GatewayEventKind::GatewayState,
        "starting",
        format!("Gateway daemon PID {} 正在启动", std::process::id()),
    );
    gateway_daemon::append_log(&format!(
        "[daemon] started pid={} scope={} endpoint={:?} routes={}",
        std::process::id(),
        config_scope,
        server.endpoint(),
        workspace_ids.len()
    ));
    let result = serve_gateway(
        &workspace_ids,
        false,
        false,
        Some(receiver),
        Some(config_scope),
    )
    .await;
    if let Err(error) = &result {
        gateway_control::publish_gateway_event(
            config_scope,
            gateway_control::GatewayEventKind::GatewayState,
            "error",
            error.to_string(),
        );
    }
    gateway_daemon::append_log(&format!(
        "[daemon] exiting pid={} result={}",
        std::process::id(),
        match &result {
            Ok(()) => "ok".to_string(),
            Err(error) => error.to_string(),
        }
    ));
    result
}

async fn reload_daemon_config(options: ReloadOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    let inspection = daemon::inspect(&profile)?;
    let state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .ok_or_else(|| {
            AppError::Message("workspace daemon 未运行，reload 只能应用到当前活动服务".into())
        })?;
    let requested = match options.service {
        ServiceSelection::Mcp => vec![control::ControlService::Mcp],
        ServiceSelection::Actions => vec![control::ControlService::Actions],
        ServiceSelection::All => vec![
            control::ControlService::Mcp,
            control::ControlService::Actions,
        ],
    };
    if requested.iter().any(|service| match service {
        control::ControlService::Mcp => !state.service.includes_mcp(),
        control::ControlService::Actions => !state.service.includes_actions(),
    }) {
        return Err(AppError::Message(format!(
            "daemon 当前 service={}，不能 reload 未运行的目标服务；请先启动该服务或只选择活动服务",
            state.service.as_str()
        )));
    }
    let mut reloaded = Vec::new();
    for service in requested {
        control::request_reload_operation(&profile, service, Duration::from_secs(15))
            .await
            .map_err(|error| {
                AppError::Message(format!(
                    "daemon reload {} 失败：{error}",
                    match service {
                        control::ControlService::Mcp => "mcp",
                        control::ControlService::Actions => "actions",
                    }
                ))
            })?;
        reloaded.push(match service {
            control::ControlService::Mcp => "mcp",
            control::ControlService::Actions => "actions",
        });
    }
    if as_json {
        print_json(&json!({
            "event": "reloaded",
            "workspaceId": profile.id,
            "services": reloaded
        }))?;
    } else {
        println!(
            "workspace {} 已通过 daemon reload：{}",
            profile.name,
            reloaded.join(", ")
        );
    }
    Ok(())
}

async fn show_events(options: EventsOptions, as_json: bool) -> AppResult<()> {
    match options.target {
        EventsTarget::Workspace(workspace) => {
            show_workspace_events(&workspace, options.follow, options.wait_seconds, as_json).await
        }
        EventsTarget::ControlPlane => {
            show_control_plane_events(options.follow, options.wait_seconds, as_json).await
        }
    }
}

async fn show_workspace_events(
    workspace: &str,
    follow: bool,
    wait_seconds: u64,
    as_json: bool,
) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), workspace)?.clone();
    drop(store);
    let mut cursor = None;
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        let wait_ms = if follow {
            u32::try_from(wait_seconds.saturating_mul(1_000)).unwrap_or(25_000)
        } else {
            0
        };
        let request = control::request_events(&profile, cursor.clone(), 64, wait_ms);
        let batch = if follow {
            tokio::select! {
                result = &mut shutdown_signal => return result,
                result = request => result.map_err(|error| AppError::Message(error.to_string()))?,
            }
        } else {
            request
                .await
                .map_err(|error| AppError::Message(error.to_string()))?
        };
        cursor = Some(batch.next_cursor.clone());
        if as_json {
            if follow {
                for event in &batch.events {
                    print_json_line(event)?;
                }
                if batch.reset {
                    print_json_line(&json!({
                        "kind": "cursor_reset",
                        "cursor": batch.next_cursor
                    }))?;
                }
            } else {
                print_json(&batch)?;
            }
        } else {
            if batch.reset {
                eprintln!("event cursor 已重置到当前 daemon stream");
            }
            for event in &batch.events {
                println!(
                    "{}\t{:?}\t{}\t{}\t{}",
                    event.sequence,
                    event.kind,
                    event
                        .service
                        .map(|service| format!("{service:?}").to_lowercase())
                        .unwrap_or_else(|| "daemon".into()),
                    event.state,
                    event.message
                );
            }
        }
        if !follow {
            return Ok(());
        }
    }
}

async fn show_control_plane_events(
    follow: bool,
    wait_seconds: u64,
    as_json: bool,
) -> AppResult<()> {
    let mut cursor = None;
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        let store = DataStore::load()?;
        let profiles = store.list().to_vec();
        drop(store);
        let wait_ms = if follow {
            u32::try_from(wait_seconds.saturating_mul(1_000)).unwrap_or(25_000)
        } else {
            0
        };
        let request = control::control_plane_events(&profiles, cursor.clone(), 64, wait_ms);
        let batch = if follow {
            tokio::select! {
                result = &mut shutdown_signal => return result,
                result = request => result?,
            }
        } else {
            request.await?
        };
        cursor = Some(batch.next_cursor.clone());
        if as_json {
            if follow {
                for event in &batch.events {
                    print_json_line(event)?;
                }
                if !batch.reset_sources.is_empty() {
                    print_json_line(&json!({
                        "kind": "cursor_reset",
                        "sources": batch.reset_sources,
                        "cursor": batch.next_cursor
                    }))?;
                }
            } else {
                print_json(&batch)?;
            }
        } else {
            for source in &batch.reset_sources {
                eprintln!("control-plane event cursor reset: {source:?}");
            }
            for event in &batch.events {
                match event {
                    control::ControlPlaneEvent::Gateway { event } => println!(
                        "gateway\t{}\t{:?}\t{}\t{}",
                        event.sequence, event.kind, event.state, event.message
                    ),
                    control::ControlPlaneEvent::Workspace {
                        workspace_id,
                        event,
                    } => println!(
                        "workspace:{workspace_id}\t{}\t{:?}\t{}\t{}",
                        event.sequence, event.kind, event.state, event.message
                    ),
                }
            }
        }
        if !follow {
            return Ok(());
        }
    }
}

async fn next_daemon_control_command(
    receiver: &mut Option<control::DaemonControlReceiver>,
) -> Option<control::DaemonControlCommand> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

fn path_or_parent_writable(path: &Path) -> bool {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if let Ok(metadata) = std::fs::metadata(candidate) {
            return metadata.is_dir() && !metadata.permissions().readonly();
        }
        current = candidate.parent();
    }
    false
}

async fn execute_gateway(command: GatewayCommand, as_json: bool) -> AppResult<i32> {
    match command {
        GatewayCommand::Show => show_gateway(as_json).map(|_| 0),
        GatewayCommand::Status => show_gateway_status(as_json).await.map(|_| 0),
        GatewayCommand::Configure(options) => configure_gateway(options, as_json).await.map(|_| 0),
        GatewayCommand::Serve { workspaces } => {
            serve_gateway(&workspaces, as_json, true, None, None)
                .await
                .map(|_| 0)
        }
        GatewayCommand::Start(options) => start_gateway_daemon(options, as_json).await.map(|_| 0),
        GatewayCommand::Stop(options) => stop_gateway_daemon(options, as_json).await.map(|_| 0),
        GatewayCommand::Restart(options) => {
            restart_gateway_daemon(options, as_json).await.map(|_| 0)
        }
        GatewayCommand::Reload => reload_gateway_daemon(as_json).await.map(|_| 0),
        GatewayCommand::Logs(options) => show_gateway_logs(options, as_json).await.map(|_| 0),
        GatewayCommand::Events(options) => show_gateway_events(options, as_json).await.map(|_| 0),
    }
}

async fn show_gateway_logs(options: GatewayLogsOptions, as_json: bool) -> AppResult<()> {
    let inspection = gateway_daemon::inspect()?;
    if options.follow && !inspection.running {
        return Err(AppError::Message(
            "Gateway daemon 未运行；历史日志可直接读取，但 --follow 只允许跟随活动 daemon".into(),
        ));
    }
    let tail_lines = u32::try_from(options.lines).unwrap_or(u32::MAX);
    let initial = if inspection.running {
        gateway_control::request_logs(tail_lines, None)
            .await
            .map_err(|error| {
                AppError::Message(format!(
                "Gateway daemon 日志控制请求失败：{error}；运行中的 Gateway 不会回退到直接文件读取"
            ))
            })?
    } else {
        gateway_control::logs_via_daemon_or_local(tail_lines, None).await?
    };

    if !options.follow {
        if as_json {
            print_json(&initial)?;
        } else if !initial.exists {
            println!("暂无 Gateway daemon 日志：{}", initial.path);
        } else {
            println!("==> {} <==", initial.path);
            print!("{}", initial.content);
            if !initial.content.ends_with('\n') {
                println!();
            }
        }
        return Ok(());
    }

    emit_gateway_log_chunk(&initial, "log_snapshot", as_json)?;
    let mut cursor = gateway_control::GatewayLogCursor {
        offset: initial.next_offset,
    };
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        tokio::select! {
            result = &mut shutdown_signal => return result,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                let chunk = gateway_control::request_logs(0, Some(cursor.clone()))
                    .await
                    .map_err(|error| AppError::Message(format!(
                        "Gateway daemon 日志跟随中断：{error}"
                    )))?;
                cursor.offset = chunk.next_offset;
                emit_gateway_log_chunk(&chunk, "log_append", as_json)?;
            }
        }
    }
}

fn emit_gateway_log_chunk(
    chunk: &gateway_control::GatewayLogChunk,
    event: &str,
    as_json: bool,
) -> AppResult<()> {
    if !chunk.exists || chunk.content.is_empty() {
        return Ok(());
    }
    if as_json {
        print_json_line(&json!({
            "event": event,
            "name": chunk.name,
            "path": chunk.path,
            "content": chunk.content,
            "nextOffset": chunk.next_offset,
            "truncated": chunk.truncated
        }))?;
    } else {
        if event == "log_snapshot" {
            println!("==> {} <==", chunk.path);
        }
        print!("{}", chunk.content);
        if event == "log_snapshot" && !chunk.content.ends_with('\n') {
            println!();
        }
        std::io::stdout().flush()?;
    }
    Ok(())
}

async fn show_gateway_events(options: GatewayEventsOptions, as_json: bool) -> AppResult<()> {
    let inspection = gateway_daemon::inspect()?;
    if !inspection.running {
        return Err(AppError::Message(
            "Gateway daemon 未运行；Gateway events 只存在于活动 daemon 的内存 journal 中".into(),
        ));
    }
    let mut cursor = None;
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        let wait_ms = if options.follow {
            u32::try_from(options.wait_seconds.saturating_mul(1_000)).unwrap_or(25_000)
        } else {
            0
        };
        let request = gateway_control::request_events(cursor.clone(), 32, wait_ms);
        let batch = if options.follow {
            tokio::select! {
                result = &mut shutdown_signal => return result,
                result = request => result.map_err(|error| AppError::Message(error.to_string()))?,
            }
        } else {
            request
                .await
                .map_err(|error| AppError::Message(error.to_string()))?
        };
        cursor = Some(batch.next_cursor.clone());
        if as_json {
            if options.follow {
                for event in &batch.events {
                    print_json_line(event)?;
                }
                if batch.reset {
                    print_json_line(&json!({
                        "kind": "cursor_reset",
                        "cursor": batch.next_cursor
                    }))?;
                }
            } else {
                print_json(&batch)?;
            }
        } else {
            if batch.reset {
                eprintln!("Gateway event cursor 已重置到当前 daemon stream");
            }
            for event in &batch.events {
                println!(
                    "{}\t{:?}\t{}\t{}",
                    event.sequence, event.kind, event.state, event.message
                );
            }
        }
        if !options.follow {
            return Ok(());
        }
    }
}

async fn show_gateway_status(as_json: bool) -> AppResult<()> {
    let status = gateway_control::status_via_daemon_or_local().await?;
    if as_json {
        print_json(&status)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&status)?);
    }
    Ok(())
}

fn resolve_gateway_workspace_ids(selectors: &[String]) -> AppResult<Vec<String>> {
    let store = DataStore::load()?;
    let config = store.settings().mcp_gateway;
    if !config.enabled {
        return Err(AppError::Message(
            "MCP Gateway 尚未启用；请先运行 gateway configure --enable --owner WORKSPACE".into(),
        ));
    }
    gateway::validate_config(&config, store.list())?;
    let mut ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for selector in selectors {
        let profile = resolve_workspace(store.list(), selector)?;
        ensure_workspace_directory(profile)?;
        if seen.insert(profile.id.clone()) {
            ids.push(profile.id.clone());
        }
    }
    if ids.is_empty() {
        return Err(AppError::Message("Gateway 没有选中的工作区。".into()));
    }
    ids.sort();
    Ok(ids)
}

async fn start_gateway_daemon(options: GatewayStartOptions, as_json: bool) -> AppResult<()> {
    if !gateway_daemon::supported() {
        return Err(AppError::Message(
            "Gateway daemon 当前仅支持 Windows 和 Linux；请使用 gateway serve 前台模式".into(),
        ));
    }
    let workspace_ids = resolve_gateway_workspace_ids(&options.workspaces)?;
    let inspection = gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.running {
        gateway_control::ping().await.map_err(|error| {
            AppError::Message(format!("Gateway daemon 状态存在但 IPC 不可用：{error}"))
        })?;
        return Err(AppError::Message("Gateway daemon 已经运行".into()));
    }
    let pid = gateway_daemon::spawn(&workspace_ids)?;
    let state =
        match gateway_daemon::wait_ready(pid, Duration::from_secs(options.wait_seconds)).await {
            Ok(state) => state,
            Err(error) => {
                let cleanup = gateway_daemon::terminate_spawned(pid).await;
                return match cleanup {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => Err(AppError::Message(format!(
                        "Gateway daemon 启动失败：{error}；清理 PID {pid} 也失败：{cleanup_error}"
                    ))),
                };
            }
        };
    let status = gateway_control::request_status()
        .await
        .map_err(|error| AppError::Message(error.to_string()))?;
    #[cfg(windows)]
    crate::windows_service::set_gateway_desired(&workspace_ids)?;
    #[cfg(target_os = "linux")]
    crate::linux_service::set_gateway_desired(&workspace_ids)?;
    if as_json {
        print_json(&json!({
            "event": "gateway_started",
            "pid": state.pid,
            "status": status
        }))?;
    } else {
        println!(
            "Gateway daemon 已启动，PID {}，routes={}，端口 {}",
            state.pid,
            state.workspace_ids.len(),
            state.local_port
        );
    }
    Ok(())
}

async fn stop_gateway_daemon(options: GatewayStopOptions, as_json: bool) -> AppResult<()> {
    if !gateway_daemon::supported() {
        return Err(AppError::Message(
            "Gateway daemon 当前仅支持 Windows 和 Linux".into(),
        ));
    }
    let inspection = gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let Some(state) = inspection.state else {
        gateway_daemon::cleanup()?;
        #[cfg(windows)]
        crate::windows_service::set_gateway_desired(&[])?;
        #[cfg(target_os = "linux")]
        crate::linux_service::set_gateway_desired(&[])?;
        if as_json {
            print_json(&json!({ "event": "gateway_stopped", "alreadyStopped": true }))?;
        } else {
            println!("Gateway daemon 未运行。");
        }
        return Ok(());
    };
    if !inspection.running || !inspection.pid_matches {
        gateway_daemon::cleanup()?;
        #[cfg(windows)]
        crate::windows_service::set_gateway_desired(&[])?;
        #[cfg(target_os = "linux")]
        crate::linux_service::set_gateway_desired(&[])?;
        if as_json {
            print_json(&json!({ "event": "gateway_stopped", "alreadyStopped": true }))?;
        } else {
            println!("Gateway daemon 未运行，已清理过期状态。");
        }
        return Ok(());
    }
    let accepted_pid = gateway_control::request_exit(GatewayOperation::Shutdown)
        .await
        .map_err(|error| AppError::Message(error.to_string()))?;
    if accepted_pid != state.pid {
        return Err(AppError::Message(format!(
            "Gateway shutdown PID mismatch: state={}, response={accepted_pid}",
            state.pid
        )));
    }
    gateway_daemon::wait_for_exit(
        state.pid,
        Duration::from_secs(options.timeout_seconds),
        options.force,
    )
    .await?;
    #[cfg(windows)]
    crate::windows_service::set_gateway_desired(&[])?;
    #[cfg(target_os = "linux")]
    crate::linux_service::set_gateway_desired(&[])?;
    if as_json {
        print_json(&json!({ "event": "gateway_stopped", "pid": state.pid }))?;
    } else {
        println!("Gateway daemon PID {} 已停止。", state.pid);
    }
    Ok(())
}

async fn restart_gateway_daemon(options: GatewayStopOptions, as_json: bool) -> AppResult<()> {
    if !gateway_daemon::supported() {
        return Err(AppError::Message(
            "Gateway daemon 当前仅支持 Windows 和 Linux".into(),
        ));
    }
    let inspection = gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    let state = inspection
        .state
        .filter(|_| inspection.running && inspection.pid_matches)
        .ok_or_else(|| {
            AppError::Message("Gateway daemon 未运行；请使用 gateway start 指定 routes".into())
        })?;
    let workspace_ids = state.workspace_ids.clone();
    let accepted_pid = gateway_control::request_exit(GatewayOperation::Restart)
        .await
        .map_err(|error| AppError::Message(error.to_string()))?;
    if accepted_pid != state.pid {
        return Err(AppError::Message(format!(
            "Gateway restart PID mismatch: state={}, response={accepted_pid}",
            state.pid
        )));
    }
    gateway_daemon::wait_for_exit(
        state.pid,
        Duration::from_secs(options.timeout_seconds),
        options.force,
    )
    .await?;
    let pid = gateway_daemon::spawn(&workspace_ids)?;
    let next =
        match gateway_daemon::wait_ready(pid, Duration::from_secs(options.timeout_seconds)).await {
            Ok(state) => state,
            Err(error) => {
                let _ = gateway_daemon::terminate_spawned(pid).await;
                return Err(error);
            }
        };
    if as_json {
        print_json(&json!({
            "event": "gateway_restarted",
            "previousPid": state.pid,
            "pid": next.pid,
            "workspaces": workspace_ids
        }))?;
    } else {
        println!("Gateway daemon 已重启：{} -> {}", state.pid, next.pid);
    }
    Ok(())
}

async fn reload_gateway_daemon(as_json: bool) -> AppResult<()> {
    if !gateway_daemon::supported() {
        return Err(AppError::Message(
            "Gateway daemon 当前仅支持 Windows 和 Linux".into(),
        ));
    }
    gateway_control::request_reload(Duration::from_secs(20))
        .await
        .map_err(|error| AppError::Message(error.to_string()))?;
    let status = gateway_control::request_status()
        .await
        .map_err(|error| AppError::Message(error.to_string()))?;
    if as_json {
        print_json(&json!({ "event": "gateway_reloaded", "status": status }))?;
    } else {
        println!(
            "Gateway daemon 配置已 reload，routes={}。",
            status.route_count
        );
    }
    Ok(())
}

fn show_gateway(as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let config = store.settings().mcp_gateway;
    let routes = store
        .list()
        .iter()
        .map(|profile| {
            let endpoint = gateway::workspace_base_url(&config, &profile.id)
                .map(|base| format!("{base}/mcp"))
                .unwrap_or_default();
            json!({
                "workspaceId": profile.id,
                "workspaceName": profile.name,
                "endpoint": endpoint
            })
        })
        .collect::<Vec<_>>();
    let value = json!({ "config": config, "routes": routes });
    if as_json {
        print_json(&value)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

async fn configure_gateway(options: GatewayConfigureOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let previous = store.settings().mcp_gateway;
    let mut config = previous.clone();
    if let Some(enabled) = options.enabled {
        config.enabled = enabled;
    }
    if let Some(port) = options.local_port {
        config.local_port = port;
    }
    if let Some(selector) = options.owner_workspace {
        config.owner_workspace_id = resolve_workspace(store.list(), &selector)?.id.clone();
    }
    if let Some(public_url) = options.public_url {
        config.public_url = public_url.trim().trim_end_matches('/').to_string();
    }
    if previous.identity_changed(&config) {
        config.clear_observation();
    } else {
        config.observed_public_url = previous.observed_public_url;
        config.observed_owner_workspace_id = previous.observed_owner_workspace_id;
        config.observed_tunnel_signature = previous.observed_tunnel_signature;
    }
    gateway::validate_config(&config, store.list())?;
    drop(store);

    let inspection = gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    if inspection.running {
        let state = inspection.state.ok_or_else(|| {
            AppError::Message("Gateway daemon reports running without state metadata".into())
        })?;
        gateway_control::ping()
            .await
            .map_err(|error| AppError::Message(format!("Gateway daemon IPC 不可用：{error}")))?;
        if config.enabled {
            gateway_control::request_apply_config(config.clone(), Duration::from_secs(20))
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
        } else {
            let accepted_pid = gateway_control::request_exit(GatewayOperation::Shutdown)
                .await
                .map_err(|error| AppError::Message(error.to_string()))?;
            if accepted_pid != state.pid {
                return Err(AppError::Message(format!(
                    "Gateway disable PID mismatch: state={}, response={accepted_pid}",
                    state.pid
                )));
            }
            gateway_daemon::wait_for_exit(state.pid, Duration::from_secs(10), false).await?;
            gateway_control::persist_config(&config)?;
        }
    } else {
        gateway_control::persist_config(&config)?;
    }
    #[cfg(windows)]
    if !config.enabled {
        crate::windows_service::set_gateway_desired(&[])?;
    }
    #[cfg(target_os = "linux")]
    if !config.enabled {
        crate::linux_service::set_gateway_desired(&[])?;
    }
    let applied_config = DataStore::load()?.settings().mcp_gateway;
    if as_json {
        print_json(&json!({ "event": "gateway_configured", "config": applied_config }))?;
    } else {
        println!("MCP Gateway 配置已保存。");
        println!("{}", serde_json::to_string_pretty(&applied_config)?);
    }
    Ok(())
}

async fn next_gateway_control_command(
    receiver: &mut Option<gateway_control::GatewayControlReceiver>,
) -> Option<gateway_control::GatewayControlCommand> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn serve_gateway(
    selectors: &[String],
    as_json: bool,
    foreground: bool,
    mut control_commands: Option<gateway_control::GatewayControlReceiver>,
    event_scope: Option<&str>,
) -> AppResult<()> {
    let store = DataStore::load()?;
    let mut all_profiles = store.list().to_vec();
    let mut config = store.settings().mcp_gateway;
    if !config.enabled {
        return Err(AppError::Message(
            "MCP Gateway 尚未启用；请先运行 gateway configure --enable --owner WORKSPACE".into(),
        ));
    }
    gateway::validate_config(&config, &all_profiles)?;
    let mut selected = Vec::new();
    let mut selected_ids = std::collections::HashSet::new();
    for selector in selectors {
        let profile = resolve_workspace(&all_profiles, selector)?.clone();
        if selected_ids.insert(profile.id.clone()) {
            ensure_workspace_directory(&profile)?;
            selected.push(profile);
        }
    }
    if selected.is_empty() {
        return Err(AppError::Message("Gateway 没有选中的工作区。".into()));
    }
    let mut runtime = RuntimeSupervisor::default();
    let mut started =
        start_gateway_services(&mut runtime, &selected, &mut config, &all_profiles).await?;
    publish_gateway_runtime_event(
        event_scope,
        gateway_control::GatewayEventKind::DaemonReady,
        "running",
        format!(
            "Gateway daemon 已就绪，routes={} localPort={}",
            selected.len(),
            config.local_port
        ),
    );
    publish_gateway_runtime_event(
        event_scope,
        gateway_control::GatewayEventKind::RouteState,
        "active",
        format!("当前注册 {} 条 Gateway route", selected.len()),
    );

    if as_json {
        print_json(&json!({
            "event": "gateway_ready",
            "localEndpoint": format!("http://127.0.0.1:{}", config.local_port),
            "publicBaseUrl": config.effective_public_url(),
            "routes": selected.iter().map(|profile| json!({
                "workspaceId": profile.id,
                "workspaceName": profile.name,
                "endpoint": format!("{}/mcp", gateway::workspace_base_url(&config, &profile.id).unwrap_or_default())
            })).collect::<Vec<_>>()
        }))?;
    } else {
        println!("MCP Gateway 已启动：http://127.0.0.1:{}", config.local_port);
        for profile in &selected {
            println!(
                "{}\t{}/mcp",
                profile.name,
                gateway::workspace_base_url(&config, &profile.id)?
            );
        }
        if foreground {
            println!("前台运行中，按 Ctrl+C 停止。");
        }
    }

    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    let mut maintenance = tokio::time::interval(Duration::from_secs(2));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    maintenance.tick().await;
    let mut terminal_error = None;
    let mut gateway_tunnel_retry: Option<CliTunnelRetry> = None;
    loop {
        tokio::select! {
            signal = &mut shutdown_signal => {
                if let Err(error) = signal {
                    terminal_error = Some(error);
                }
                break;
            }
            command = next_gateway_control_command(&mut control_commands) => {
                match command {
                    Some(gateway_control::GatewayControlCommand::Shutdown { operation }) => {
                        gateway_daemon::append_log(&format!(
                            "[control] accepted {operation:?}; beginning graceful shutdown"
                        ));
                        break;
                    }
                    Some(gateway_control::GatewayControlCommand::Reload { operation_id }) => {
                        gateway_control::mark_operation_running(&operation_id);
                        let result = apply_gateway_reload(
                            &mut runtime,
                            &mut started,
                            &mut selected,
                            &mut config,
                            &mut all_profiles,
                        )
                        .await;
                        match &result {
                            Ok(()) => publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::Reload,
                                "succeeded",
                                format!("Gateway reload 完成，routes={}", selected.len()),
                            ),
                            Err(error) => publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::Reload,
                                "failed",
                                error.to_string(),
                            ),
                        }
                        gateway_control::finish_operation(&operation_id, result);
                    }
                    Some(gateway_control::GatewayControlCommand::SetRoutes {
                        operation_id,
                        workspace_ids,
                    }) => {
                        gateway_control::mark_operation_running(&operation_id);
                        let result = apply_gateway_routes(
                            &mut runtime,
                            &mut started,
                            &mut selected,
                            &mut config,
                            &mut all_profiles,
                            workspace_ids,
                        )
                        .await;
                        match &result {
                            Ok(()) => publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::RouteState,
                                "succeeded",
                                format!("Gateway routes 已切换，routes={}", selected.len()),
                            ),
                            Err(error) => publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::RouteState,
                                "failed",
                                error.to_string(),
                            ),
                        }
                        gateway_control::finish_operation(&operation_id, result);
                    }
                    Some(gateway_control::GatewayControlCommand::ApplyConfig {
                        operation_id,
                        config: next_config,
                    }) => {
                        gateway_control::mark_operation_running(&operation_id);
                        let result = apply_gateway_config(
                            &mut runtime,
                            &mut started,
                            &mut selected,
                            &mut config,
                            &mut all_profiles,
                            *next_config,
                        )
                        .await;
                        match &result {
                            Ok(()) => publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::ConfigApplied,
                                "succeeded",
                                format!(
                                    "Gateway 配置已应用，localPort={} routes={}",
                                    config.local_port,
                                    selected.len()
                                ),
                            ),
                            Err(error) => publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::ConfigApplied,
                                "failed",
                                error.to_string(),
                            ),
                        }
                        gateway_control::finish_operation(&operation_id, result);
                    }
                    None => {
                        terminal_error = Some(AppError::Message(
                            "Gateway control command channel closed unexpectedly".into(),
                        ));
                        publish_gateway_runtime_event(
                            event_scope,
                            gateway_control::GatewayEventKind::GatewayState,
                            "error",
                            "Gateway control command channel closed unexpectedly",
                        );
                        break;
                    }
                }
            }
            _ = maintenance.tick() => {
                for profile in &selected {
                    match runtime.maintain_mcp(profile) {
                        Ok(status) if status.state == "error" && !status.recovery.enabled => {
                            let error = AppError::Message(format!(
                                "工作区 {} MCP 自动恢复耗尽：{}",
                                profile.name, status.local_message
                            ));
                            publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::GatewayState,
                                "error",
                                error.to_string(),
                            );
                            terminal_error = Some(error);
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::GatewayState,
                                "error",
                                error.to_string(),
                            );
                            terminal_error = Some(error);
                            break;
                        }
                    }
                }
                if terminal_error.is_some() {
                    break;
                }
                let active = runtime.active_mcp_workspace_ids();
                if let Err(error) = gateway::ensure(&config, &all_profiles, &active).await {
                    publish_gateway_runtime_event(
                        event_scope,
                        gateway_control::GatewayEventKind::GatewayState,
                        "error",
                        error.to_string(),
                    );
                    terminal_error = Some(error);
                    break;
                }
                if gateway_tunnel_retry
                    .is_some_and(|retry| tokio::time::Instant::now() < retry.next_attempt)
                {
                    continue;
                }
                match reconcile_mcp_gateway(&config, &all_profiles, &active).await {
                    Ok(Some(url)) => {
                        let recovered_attempts = gateway_tunnel_retry.take().map(|retry| retry.attempts);
                        gateway::clear_runtime_error().await;
                        if url != config.observed_public_url {
                            persist_cli_gateway_observation(&mut config, &all_profiles, &url)?;
                        }
                        if let Some(attempts) = recovered_attempts {
                            publish_gateway_runtime_event(
                                event_scope,
                                gateway_control::GatewayEventKind::TunnelState,
                                "running",
                                format!(
                                    "Gateway 隧道已在第 {attempts} 次重试后恢复：{}",
                                    config.effective_public_url()
                                ),
                            );
                            if as_json {
                                print_json_line(&json!({
                                    "event": "gateway_tunnel_reconnected",
                                    "attempts": attempts,
                                    "publicBaseUrl": config.effective_public_url()
                                }))?;
                            } else {
                                println!("Gateway 隧道已在第 {attempts} 次重试后恢复。");
                            }
                        }
                    }
                    Ok(None) => {
                        gateway_tunnel_retry = None;
                        gateway::clear_runtime_error().await;
                    }
                    Err(error) if is_quick_tunnel_url_change_error(&error) => {
                        gateway::record_runtime_error(format!("Gateway 隧道维护失败：{error}")).await;
                        publish_gateway_runtime_event(
                            event_scope,
                            gateway_control::GatewayEventKind::TunnelState,
                            "error",
                            error.to_string(),
                        );
                        terminal_error = Some(error);
                        break;
                    }
                    Err(error) => {
                        gateway::record_runtime_error(format!("Gateway 隧道维护失败：{error}")).await;
                        let attempts = gateway_tunnel_retry
                            .map(|retry| retry.attempts.saturating_add(1))
                            .unwrap_or(1);
                        let delay = cli_tunnel_retry_delay(attempts);
                        gateway_tunnel_retry = Some(CliTunnelRetry {
                            attempts,
                            next_attempt: tokio::time::Instant::now() + delay,
                        });
                        publish_gateway_runtime_event(
                            event_scope,
                            gateway_control::GatewayEventKind::TunnelState,
                            "recovering",
                            format!(
                                "Gateway 隧道维护失败，第 {attempts} 次重试将在 {} 秒后进行：{error}",
                                delay.as_secs()
                            ),
                        );
                        if as_json {
                            print_json_line(&json!({
                                "event": "gateway_tunnel_retry",
                                "detail": error.to_string(),
                                "attempt": attempts,
                                "retryInSeconds": delay.as_secs()
                            }))?;
                        } else {
                            eprintln!(
                                "Gateway 隧道维护失败，将在 {} 秒后进行第 {} 次重试：{error}",
                                delay.as_secs(),
                                attempts
                            );
                        }
                    }
                }
            }
        }
    }

    publish_gateway_runtime_event(
        event_scope,
        gateway_control::GatewayEventKind::DaemonStopping,
        "stopping",
        terminal_error
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "Gateway daemon 正在优雅停止".into()),
    );
    shutdown_gateway_services(&mut runtime, &started, &config, &all_profiles).await?;
    publish_gateway_runtime_event(
        event_scope,
        gateway_control::GatewayEventKind::GatewayState,
        "stopped",
        "Gateway listener、routes 与 tunnel 已停止",
    );
    if as_json {
        print_json(&json!({ "event": "gateway_stopped" }))?;
    }
    match terminal_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn ensure_gateway_ports_available(
    config: &McpGatewayConfig,
    profiles: &[WorkspaceProfile],
) -> AppResult<()> {
    if let Some(pid) = platform().find_pid_listening_on_port(config.local_port)? {
        return Err(AppError::Message(format!(
            "Gateway 端口 {} 已被 PID {pid} 占用；CLI 不会接管 GUI 或其他进程",
            config.local_port
        )));
    }
    for profile in profiles {
        ensure_selected_ports_available(profile, ServiceSelection::Mcp)?;
    }
    Ok(())
}

fn persist_cli_gateway_observation(
    config: &mut McpGatewayConfig,
    profiles: &[WorkspaceProfile],
    url: &str,
) -> AppResult<()> {
    let normalized = url.trim().trim_end_matches('/');
    if normalized.is_empty() || normalized.starts_with("http://127.0.0.1:") {
        return Ok(());
    }
    let owner = profiles
        .iter()
        .find(|profile| profile.id == config.owner_workspace_id)
        .ok_or_else(|| AppError::Message("MCP Gateway 隧道所有者工作区不存在。".into()))?;
    let signature = gateway::tunnel_identity_signature(config, owner)?;
    config.observed_public_url = normalized.to_string();
    config.observed_owner_workspace_id = config.owner_workspace_id.clone();
    config.observed_tunnel_signature = signature.clone();
    gateway::validate_config(config, profiles)?;
    DataStore::update_file(|data| {
        if data.mcp_gateway.identity_changed(config) {
            return Ok(());
        }
        if data.mcp_gateway.observed_public_url == normalized
            && data.mcp_gateway.observed_owner_workspace_id == config.owner_workspace_id
            && data.mcp_gateway.observed_tunnel_signature == signature
        {
            return Ok(());
        }
        data.mcp_gateway.observed_public_url = normalized.to_string();
        data.mcp_gateway.observed_owner_workspace_id = config.owner_workspace_id.clone();
        data.mcp_gateway.observed_tunnel_signature = signature.clone();
        Ok(())
    })
}

async fn shutdown_gateway_services(
    runtime: &mut RuntimeSupervisor,
    profiles: &[WorkspaceProfile],
    config: &McpGatewayConfig,
    all_profiles: &[WorkspaceProfile],
) -> AppResult<()> {
    for profile in profiles.iter().rev() {
        let handle = runtime.begin_stop(&profile.id, ServiceKind::Mcp);
        await_listener_shutdown(handle, profile.runtime.local_port).await;
        runtime.finish_stop(&profile.id, ServiceKind::Mcp);
    }
    let active = runtime.active_mcp_workspace_ids();
    let tunnel_result = reconcile_mcp_gateway(config, all_profiles, &active).await;
    let gateway_result = gateway::stop().await;
    for profile in profiles {
        update_public_url(&profile.id, "mcp", "");
    }
    tunnel_result?;
    gateway_result
}

async fn start_daemon(options: RunOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    ensure_workspace_directory(&profile)?;
    let service = options.service.unwrap_or(ServiceSelection::Mcp);
    let tunnel = options.tunnel.unwrap_or(false);
    if store.settings().mcp_gateway.enabled && service.includes_mcp() {
        return Err(AppError::Message(
            "MCP Gateway 模式不支持每工作区独立 MCP daemon；请使用 `anchor gateway start <workspace ...>` 管理 Gateway route。"
                .into(),
        ));
    }

    let already_running = daemon::inspect(&profile)?.running;
    if !already_running {
        ensure_selected_ports_available(&profile, service)?;
    }
    let state = control::ensure_daemon_running(
        &profile,
        control::DaemonLaunchSpec {
            service,
            tunnels: tunnel.then_some(service),
        },
        Duration::from_secs(options.wait_seconds),
    )
    .await?;
    #[cfg(windows)]
    crate::windows_service::set_workspace_desired(
        &profile.id,
        Some(control::DaemonLaunchSpec {
            service,
            tunnels: tunnel.then_some(service),
        }),
    )?;
    #[cfg(target_os = "linux")]
    crate::linux_service::set_workspace_desired(
        &profile.id,
        Some(control::DaemonLaunchSpec {
            service,
            tunnels: tunnel.then_some(service),
        }),
    )?;
    print_daemon_result(
        if already_running {
            "already_running"
        } else {
            "started"
        },
        &profile,
        Some(state.pid),
        service,
        tunnel,
        as_json,
    )
}

async fn stop_daemon(options: StopOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    let stopped = control::request_daemon_exit_and_wait(
        &profile,
        control::ControlOperation::Shutdown,
        Duration::from_secs(options.timeout_seconds),
        options.force,
    )
    .await?;
    #[cfg(windows)]
    crate::windows_service::set_workspace_desired(&profile.id, None)?;
    #[cfg(target_os = "linux")]
    crate::linux_service::set_workspace_desired(&profile.id, None)?;
    if as_json {
        print_json(&json!({
            "event": if stopped.is_some() { "stopped" } else { "already_stopped" },
            "workspace_id": profile.id,
            "pid": stopped
        }))?;
    } else if let Some(pid) = stopped {
        println!("workspace {} 的 daemon 已停止（PID {pid}）", profile.name);
    } else {
        println!("workspace {} 的 daemon 未运行", profile.name);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliLogChunk {
    name: String,
    path: String,
    content: String,
}

async fn show_logs(options: LogsOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    if daemon::inspect(&profile)?.running {
        return show_logs_via_daemon(&profile, options, as_json).await;
    }
    let files = selected_log_files(&profile, options.service);
    if !options.follow {
        let mut chunks = Vec::new();
        for (name, path) in files {
            if !path.exists() {
                continue;
            }
            chunks.push(CliLogChunk {
                name,
                path: path.display().to_string(),
                content: read_tail_lines(&path, options.lines)?,
            });
        }
        if as_json {
            print_json(&chunks)?;
        } else if chunks.is_empty() {
            println!("暂无日志：{}", log_dir_for_profile(&profile.id).display());
        } else {
            for chunk in chunks {
                println!("==> {} <==", chunk.path);
                print!("{}", chunk.content);
                if !chunk.content.ends_with('\n') {
                    println!();
                }
            }
        }
        return Ok(());
    }

    follow_logs(files, options.lines, as_json).await
}

async fn show_logs_via_daemon(
    profile: &WorkspaceProfile,
    options: LogsOptions,
    as_json: bool,
) -> AppResult<()> {
    let selection = match options.service {
        LogSelection::Daemon => control::ControlLogSelection::Daemon,
        LogSelection::Mcp => control::ControlLogSelection::Mcp,
        LogSelection::Actions => control::ControlLogSelection::Actions,
        LogSelection::All => control::ControlLogSelection::All,
    };
    let tail_lines = u32::try_from(options.lines).unwrap_or(u32::MAX);
    let initial = control::request_logs(profile, selection, tail_lines, Vec::new())
        .await
        .map_err(|error| {
            AppError::Message(format!(
                "daemon 日志控制请求失败：{error}；运行中的 daemon 不会回退到直接文件轮询"
            ))
        })?;

    if !options.follow {
        let chunks = initial
            .into_iter()
            .filter(|chunk| chunk.exists)
            .map(|chunk| CliLogChunk {
                name: chunk.name,
                path: chunk.path,
                content: chunk.content,
            })
            .collect::<Vec<_>>();
        if as_json {
            print_json(&chunks)?;
        } else if chunks.is_empty() {
            println!("暂无日志：{}", log_dir_for_profile(&profile.id).display());
        } else {
            for chunk in chunks {
                println!("==> {} <==", chunk.path);
                print!("{}", chunk.content);
                if !chunk.content.ends_with('\n') {
                    println!();
                }
            }
        }
        return Ok(());
    }

    let mut cursors = Vec::new();
    emit_control_log_chunks(&initial, "log_snapshot", as_json)?;
    replace_log_cursors(&mut cursors, &initial);

    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        tokio::select! {
            result = &mut shutdown_signal => return result,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                let chunks = control::request_logs(profile, selection, 0, cursors.clone())
                    .await
                    .map_err(|error| AppError::Message(format!(
                        "daemon 日志跟随中断：{error}"
                    )))?;
                emit_control_log_chunks(&chunks, "log_append", as_json)?;
                replace_log_cursors(&mut cursors, &chunks);
            }
        }
    }
}

fn emit_control_log_chunks(
    chunks: &[control::ControlLogChunk],
    event: &str,
    as_json: bool,
) -> AppResult<()> {
    for chunk in chunks
        .iter()
        .filter(|chunk| chunk.exists && !chunk.content.is_empty())
    {
        if as_json {
            print_json_line(&json!({
                "event": event,
                "name": chunk.name,
                "path": chunk.path,
                "content": chunk.content,
                "nextOffset": chunk.next_offset,
                "truncated": chunk.truncated
            }))?;
        } else {
            if event == "log_snapshot" {
                println!("==> {} <==", chunk.path);
            }
            print!("{}", chunk.content);
            if event == "log_snapshot" && !chunk.content.ends_with('\n') {
                println!();
            }
            std::io::stdout().flush()?;
        }
    }
    Ok(())
}

fn replace_log_cursors(
    cursors: &mut Vec<control::ControlLogCursor>,
    chunks: &[control::ControlLogChunk],
) {
    *cursors = chunks
        .iter()
        .map(|chunk| control::ControlLogCursor {
            name: chunk.name.clone(),
            offset: chunk.next_offset,
        })
        .collect();
}

async fn follow_logs(files: Vec<(String, PathBuf)>, lines: usize, as_json: bool) -> AppResult<()> {
    let mut offsets = std::collections::HashMap::new();
    for (name, path) in &files {
        if path.exists() {
            let content = read_tail_lines(path, lines)?;
            if as_json {
                print_json_line(&json!({
                    "event": "log_snapshot",
                    "name": name,
                    "path": path,
                    "content": content
                }))?;
            } else {
                println!("==> {} <==", path.display());
                print!("{content}");
                if !content.ends_with('\n') {
                    println!();
                }
                std::io::stdout().flush()?;
            }
            offsets.insert(path.clone(), fs_len(path));
        } else {
            offsets.insert(path.clone(), 0);
        }
    }

    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        tokio::select! {
            result = &mut shutdown_signal => return result,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                for (name, path) in &files {
                    let offset = offsets.get(path).copied().unwrap_or(0);
                    let (next, content) = read_log_since(path, offset)?;
                    offsets.insert(path.clone(), next);
                    if content.is_empty() {
                        continue;
                    }
                    if as_json {
                        print_json_line(&json!({
                            "event": "log_append",
                            "name": name,
                            "path": path,
                            "content": content
                        }))?;
                    } else {
                        print!("{content}");
                        std::io::stdout().flush()?;
                    }
                }
            }
        }
    }
}

fn selected_log_files(
    profile: &WorkspaceProfile,
    selection: LogSelection,
) -> Vec<(String, PathBuf)> {
    let log_dir = log_dir_for_profile(&profile.id);
    let mut files = Vec::new();
    if matches!(selection, LogSelection::Daemon | LogSelection::All) {
        files.push(("daemon".into(), daemon::daemon_log_path(&profile.id)));
    }
    if matches!(selection, LogSelection::Mcp | LogSelection::All) {
        files.extend(
            profile_log_files(profile, ProfileLogService::Mcp)
                .into_iter()
                .map(|(label, file_name)| (label.into(), log_dir.join(file_name))),
        );
    }
    if matches!(selection, LogSelection::Actions | LogSelection::All) {
        files.extend(
            profile_log_files(profile, ProfileLogService::Actions)
                .into_iter()
                .map(|(label, file_name)| (label.into(), log_dir.join(file_name))),
        );
    }
    files
}

fn read_tail_lines(path: &Path, lines: usize) -> AppResult<String> {
    const MAX_BYTES: u64 = 1_048_576;
    let mut file = File::open(path)?;
    let size = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(size.saturating_sub(MAX_BYTES)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let selected = text.lines().rev().take(lines).collect::<Vec<_>>();
    let mut output = selected.into_iter().rev().collect::<Vec<_>>().join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    Ok(output)
}

fn read_log_since(path: &Path, offset: u64) -> AppResult<(u64, String)> {
    let Ok(mut file) = File::open(path) else {
        return Ok((0, String::new()));
    };
    let size = file.seek(SeekFrom::End(0))?;
    let start = if size < offset { 0 } else { offset };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((size, String::from_utf8_lossy(&bytes).into_owned()))
}

fn fs_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|value| value.len())
        .unwrap_or(0)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheck {
    name: String,
    ok: bool,
    detail: String,
    hint: String,
}

async fn doctor_workspace(selector: &str, as_json: bool) -> AppResult<bool> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?.clone();
    let inspection = daemon::inspect(&profile)?;
    let mut checks = Vec::new();
    checks.push(doctor_check(
        "后台 daemon 支持",
        daemon::supported(),
        if daemon::supported() {
            "可使用 start/stop/restart".into()
        } else {
            "当前平台只支持 serve 前台模式".into()
        },
        "在 Windows/Linux 主机运行 daemon 运维命令",
    ));
    checks.push(doctor_check(
        "Workspace 目录",
        Path::new(&profile.path).is_dir(),
        profile.path.clone(),
        "修正 WorkspaceProfile.path",
    ));
    checks.push(doctor_check(
        "Daemon 状态",
        !inspection.stale,
        inspection.detail.clone(),
        "停止错误进程或清理过期状态后重新 start",
    ));
    if inspection.running && inspection.pid_matches {
        match control::request_version(&profile.id).await {
            Ok(version) => {
                let current = crate::build_identity::BuildIdentity::current();
                let detail = match version.build_identity.as_ref() {
                    Some(identity) => format!(
                        "protocol={} package={} git={}{}",
                        version.protocol_version,
                        identity.package_version,
                        identity.short_git_sha(),
                        if identity.git_dirty { " dirty" } else { "" }
                    ),
                    None => format!(
                        "protocol={} package={} build identity=unavailable",
                        version.protocol_version, version.daemon_version
                    ),
                };
                checks.push(doctor_check(
                    "Daemon 构建",
                    version
                        .build_identity
                        .as_ref()
                        .is_some_and(|identity| identity.same_build(&current)),
                    detail,
                    "使用当前 Anchor 构建协调 restart；若由 Windows SCM 监督，请先更新 SCM Service，再确认 daemon 已由新构建恢复",
                ));
            }
            Err(error) => checks.push(doctor_check(
                "Daemon 构建",
                false,
                error.to_string(),
                "检查 daemon 控制协议；升级时仅 lifecycle drain 允许兼容旧协议，普通写操作仍会 fail-closed",
            )),
        }
    }

    let owner_pid = inspection.state.as_ref().map(|state| state.pid);
    for (label, port) in [
        ("MCP 端口", profile.runtime.local_port),
        ("Actions 端口", profile.actions.local_port),
    ] {
        let pid = platform().find_pid_listening_on_port(port)?;
        let owned = pid.is_none() || (inspection.running && pid == owner_pid);
        checks.push(doctor_check(
            label,
            owned,
            match pid {
                Some(pid) if Some(pid) == owner_pid => {
                    format!("{port} 由 daemon PID {pid} 监听")
                }
                Some(pid) => format!("{port} 被外部 PID {pid} 占用"),
                None => format!("{port} 可用"),
            },
            "停止 GUI/其他进程或修改 profile 端口",
        ));
    }

    let log_dir = log_dir_for_profile(&profile.id);
    let log_ok = path_or_parent_writable(&log_dir);
    checks.push(doctor_check(
        "日志目录",
        log_ok,
        log_dir.display().to_string(),
        "检查当前用户对配置目录的写权限",
    ));
    append_tunnel_doctor_checks(&profile, &mut checks);

    if as_json {
        print_json(&json!({
            "workspace": {"id": profile.id, "name": profile.name, "path": profile.path},
            "ok": checks.iter().all(|check| check.ok),
            "checks": checks
        }))?;
    } else {
        println!("{} ({})", profile.name, profile.id);
        for check in &checks {
            println!(
                "{}\t{}\t{}",
                if check.ok { "PASS" } else { "FAIL" },
                check.name,
                check.detail
            );
            if !check.ok {
                println!("  建议：{}", check.hint);
            }
        }
    }
    Ok(checks.iter().all(|check| check.ok))
}

fn append_tunnel_doctor_checks(profile: &WorkspaceProfile, checks: &mut Vec<DoctorCheck>) {
    for (label, tunnel_type) in [
        ("MCP 隧道依赖", profile.tunnel.tunnel_type.as_str()),
        ("Actions 隧道依赖", profile.actions.tunnel_type.as_str()),
    ] {
        let result = match tunnel_type {
            "frp" => crate::tunnel::resolve_frpc().map(|path| path.display().to_string()),
            "cloudflare" => {
                crate::tunnel::resolve_cloudflared().map(|path| path.display().to_string())
            }
            _ => {
                checks.push(doctor_check(label, true, "未配置隧道".into(), ""));
                continue;
            }
        };
        match result {
            Ok(path) => checks.push(doctor_check(label, true, path, "")),
            Err(error) => checks.push(doctor_check(
                label,
                false,
                error.to_string(),
                "安装对应隧道二进制或关闭 --tunnel",
            )),
        }
    }
}

fn doctor_check(name: &str, ok: bool, detail: String, hint: &str) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        ok,
        detail,
        hint: if ok { String::new() } else { hint.into() },
    }
}

async fn restart_daemon(options: RunOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    let inspection = daemon::inspect(&profile)?;
    let current = inspection.state.as_ref().filter(|_| inspection.running);
    let service = options
        .service
        .or_else(|| current.map(|state| state.service))
        .unwrap_or(ServiceSelection::Mcp);
    let tunnels = match options.tunnel {
        Some(true) => Some(service),
        Some(false) => None,
        None => current.and_then(|state| state.managed_tunnels()),
    };
    if store.settings().mcp_gateway.enabled && service.includes_mcp() {
        return Err(AppError::Message(
            "MCP Gateway 模式不支持每工作区独立 MCP daemon；请使用 `anchor gateway start <workspace ...>` 管理 Gateway route。"
                .into(),
        ));
    }
    if current.is_none() {
        ensure_selected_ports_available(&profile, service)?;
    } else {
        control::request_daemon_exit_and_wait(
            &profile,
            control::ControlOperation::Restart,
            Duration::from_secs(10),
            true,
        )
        .await?;
        wait_for_selected_ports_available(
            &profile,
            service,
            Duration::from_secs(options.wait_seconds),
        )
        .await?;
    }
    let state = control::ensure_daemon_running(
        &profile,
        control::DaemonLaunchSpec { service, tunnels },
        Duration::from_secs(options.wait_seconds),
    )
    .await?;
    #[cfg(windows)]
    crate::windows_service::set_workspace_desired(
        &profile.id,
        Some(control::DaemonLaunchSpec { service, tunnels }),
    )?;
    #[cfg(target_os = "linux")]
    crate::linux_service::set_workspace_desired(
        &profile.id,
        Some(control::DaemonLaunchSpec { service, tunnels }),
    )?;
    print_daemon_result(
        "started",
        &profile,
        Some(state.pid),
        service,
        tunnels.is_some(),
        as_json,
    )
}

async fn run_daemon(
    selector: &str,
    service: ServiceSelection,
    tunnel_services: Option<ServiceSelection>,
    handoff_options: Option<args::DaemonHandoffOptions>,
) -> AppResult<()> {
    // daemon-run is an internal child entrypoint; all managed spawn paths pass
    // the canonical workspace id as selector. Redirect before DataStore load so
    // owner-token startup/config failures remain observable in daemon.log.
    #[cfg(windows)]
    crate::logging::redirect_stdio_to_file(&daemon::daemon_log_path(selector))?;
    #[cfg(windows)]
    if let Err(error) = crate::platform::install_windows_kill_on_close_job() {
        crate::tunnel::append_profile_log(
            selector,
            "daemon.log",
            &format!(
                "[daemon] warning: failed to install Windows kill-on-close process job: {error}"
            ),
        );
    }
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?.clone();

    #[cfg(not(unix))]
    if handoff_options.is_some() {
        return Err(AppError::Message(
            "daemon handoff child mode is unsupported on this platform".into(),
        ));
    }

    #[cfg(unix)]
    let (imported_listeners, handoff_id) = if let Some(options) = handoff_options.as_ref() {
        if tunnel_services.is_some() {
            return Err(AppError::Message(
                "zero-downtime handoff does not yet support managed tunnels".into(),
            ));
        }
        let imported =
            handoff::prepare_child(&profile, service, options, Duration::from_secs(15)).await?;
        (Some(imported), Some(options.handoff_id.clone()))
    } else {
        (None, None)
    };

    #[cfg(unix)]
    let guard = if handoff_options.is_some() {
        handoff::acquire_successor_ownership(&profile, service, Duration::from_secs(3)).await?
    } else {
        daemon::acquire_with_tunnels(&profile, service, tunnel_services)?
    };
    #[cfg(not(unix))]
    let guard = daemon::acquire_with_tunnels(&profile, service, tunnel_services)?;
    let (control_sender, control_receiver) = control::control_channel();
    let control_server = control::ControlServer::start(profile.clone(), control_sender)?;
    crate::tunnel::append_profile_log(
        &profile.id,
        "daemon.log",
        &format!(
            "[daemon] started pid={} service={} tunnels={} control={:?}",
            std::process::id(),
            service.as_str(),
            tunnel_services
                .map(ServiceSelection::as_str)
                .unwrap_or("none"),
            control_server.endpoint()
        ),
    );
    let result = serve_workspace(
        &profile.id,
        service,
        tunnel_services,
        false,
        false,
        Some(control_receiver),
        WorkspaceServeContext {
            ownership: Some(DaemonOwnership {
                guard: Some(guard),
                control_server: Some(control_server),
            }),
            #[cfg(unix)]
            imported_listeners,
            #[cfg(unix)]
            handoff_id,
        },
    )
    .await;
    if let Err(error) = &result {
        crate::tunnel::append_profile_log(
            &profile.id,
            "daemon.log",
            &format!("[daemon] stopped with error: {error}"),
        );
    } else {
        crate::tunnel::append_profile_log(&profile.id, "daemon.log", "[daemon] stopped");
    }
    result
}

fn print_daemon_result(
    event: &str,
    profile: &WorkspaceProfile,
    pid: Option<u32>,
    service: ServiceSelection,
    tunnel: bool,
    as_json: bool,
) -> AppResult<()> {
    if as_json {
        print_json(&json!({
            "event": event,
            "workspace": {"id": profile.id, "name": profile.name, "path": profile.path},
            "pid": pid,
            "service": service,
            "tunnel": tunnel,
            "log_path": daemon::daemon_log_path(&profile.id)
        }))?;
    } else {
        let pid = pid
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "workspace {} daemon {}：PID {pid}，service={}，tunnel={tunnel}",
            profile.name,
            if event == "started" {
                "已启动"
            } else {
                "已在运行"
            },
            service.as_str()
        );
        println!("日志：{}", daemon::daemon_log_path(&profile.id).display());
        #[cfg(target_os = "linux")]
        if let Ok(service_status) = crate::linux_service::service_status() {
            if !service_status.installed || !service_status.enabled {
                println!(
                    "提示：节点重启自动恢复尚未启用；运行 `anchor service install` 注册并启用当前配置域的 systemd user service。"
                );
            } else if service_status.build_state == "different" {
                println!(
                    "警告：systemd service 记录的 Anchor build 与当前 CLI 不一致；完成二进制更新后再次运行 `anchor service install` 刷新 service 注册。"
                );
            }
        }
    }
    Ok(())
}

fn ensure_selected_ports_available(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
) -> AppResult<()> {
    let mut selected = Vec::new();
    if service.includes_mcp() {
        selected.push(("MCP", profile.runtime.local_port));
    }
    if service.includes_actions() {
        selected.push(("Actions", profile.actions.local_port));
    }
    for (label, port) in selected {
        if let Some(pid) = platform().find_pid_listening_on_port(port)? {
            return Err(AppError::Message(format!(
                "{label} 端口 {port} 已被 PID {pid} 占用；CLI 不会接管 GUI 或其他进程"
            )));
        }
        if !loopback_port_bindable(port) {
            return Err(AppError::Message(format!(
                "{label} 端口 {port} 当前没有活动 LISTEN owner，但仍无法重新绑定；Windows 上这通常表示上一次连接尚未完成 TCP 释放"
            )));
        }
    }
    Ok(())
}

async fn wait_for_selected_ports_available(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
    timeout: Duration,
) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let error = match ensure_selected_ports_available(profile, service) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(error);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn cli_tunnel_retry_delay(attempts: u8) -> Duration {
    Duration::from_secs((1u64 << attempts.saturating_sub(1).min(6)).min(60))
}

pub fn run() -> i32 {
    let parsed = match args::parse(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    if let Some(path) = &parsed.config_dir {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.clone())
        };
        std::env::set_var(crate::brand::CONFIG_DIR_ENV, absolute);
    }

    if let Command::ServiceRun {
        config_dir,
        owner_sid,
        owner_username,
    } = &parsed.command
    {
        #[cfg(windows)]
        {
            return match crate::windows_service::run_service_dispatcher(
                config_dir.clone(),
                owner_sid.clone(),
                owner_username.clone(),
            ) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!(
                        "{}",
                        crate::logging::timestamped_line(&format!("错误：{error}"))
                    );
                    1
                }
            };
        }
        #[cfg(target_os = "linux")]
        {
            let _ = (owner_sid, owner_username);
            return match crate::async_runtime::block_on(crate::linux_service::run_service(
                config_dir.clone(),
            )) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!(
                        "{}",
                        crate::logging::timestamped_line(&format!("错误：{error}"))
                    );
                    1
                }
            };
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (config_dir, owner_sid, owner_username);
            eprintln!("错误：service-run 当前仅支持 Windows 和 Linux");
            return 1;
        }
    }

    if let Command::ServiceAdminRun {
        action,
        config_dir,
        owner_sid,
        owner_username,
    } = &parsed.command
    {
        #[cfg(windows)]
        {
            return match crate::windows_service::run_admin_action(
                action,
                config_dir.clone(),
                owner_sid,
                owner_username,
            ) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!(
                        "{}",
                        crate::logging::timestamped_line(&format!("错误：{error}"))
                    );
                    1
                }
            };
        }
        #[cfg(not(windows))]
        {
            let _ = (action, config_dir, owner_sid, owner_username);
            eprintln!("错误：service-admin-run 仅支持 Windows");
            return 1;
        }
    }

    let as_json = parsed.json;
    let daemon_mode = matches!(
        &parsed.command,
        Command::DaemonRun { .. }
            | Command::GatewayDaemonRun { .. }
            | Command::ExecSupervisorRun { .. }
            | Command::Admin(AdminCommand::DaemonRun { .. })
            | Command::ServiceRun { .. }
            | Command::ServiceAdminRun { .. }
    );
    match crate::async_runtime::block_on(execute(parsed)) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "ok": false,
                        "error": error.to_string()
                    }))
                    .unwrap_or_else(|_| "{\"ok\":false}".into())
                );
            } else {
                let message = format!("错误：{error}");
                if daemon_mode {
                    eprintln!("{}", crate::logging::timestamped_line(&message));
                } else {
                    eprintln!("{message}");
                }
            }
            1
        }
    }
}

async fn execute(cli: CliArgs) -> AppResult<i32> {
    match cli.command {
        Command::Help => {
            println!("{}", args::usage());
            Ok(0)
        }
        Command::Version => {
            println!("{} {}", crate::brand::CLI_NAME, env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Command::List => list_workspaces(cli.json).map(|_| 0),
        Command::Show { workspace } => show_workspace(&workspace, cli.json).map(|_| 0),
        Command::Status(options) => show_status(options, cli.json).await.map(|_| 0),
        Command::Serve {
            workspace,
            service,
            tunnel,
        } => serve_workspace(
            &workspace,
            service,
            tunnel.then_some(service),
            cli.json,
            true,
            None,
            WorkspaceServeContext::default(),
        )
        .await
        .map(|_| 0),
        Command::Start(options) => start_daemon(options, cli.json).await.map(|_| 0),
        Command::Stop(options) => stop_daemon(options, cli.json).await.map(|_| 0),
        Command::Restart(options) => restart_daemon(options, cli.json).await.map(|_| 0),
        Command::Upgrade(options) => upgrade::execute(options, cli.json).await,
        Command::Logs(options) => show_logs(options, cli.json).await.map(|_| 0),
        Command::Events(options) => show_events(options, cli.json).await.map(|_| 0),
        Command::Reload(options) => reload_daemon_config(options, cli.json).await.map(|_| 0),
        Command::Doctor { workspace } => doctor_workspace(&workspace, cli.json)
            .await
            .map(|healthy| if healthy { 0 } else { 1 }),
        Command::Config(command) => config::execute(command, cli.json).await,
        Command::Frp(command) => frp::execute(command, cli.json).await,
        Command::Tunnel(command) => tunnel::execute(command, cli.json).await,
        Command::Software(command) => software::execute(command, cli.json).await,
        Command::Workspace(command) => workspace::execute(command, cli.json).await,
        Command::Plugin(command) => plugin::execute(command, cli.json).await,
        Command::Gateway(command) => execute_gateway(command, cli.json).await,
        Command::Service(command) => execute_service(command, cli.json),
        Command::Admin(command) => execute_admin(command, cli.json).await,
        Command::ServiceRun { config_dir, .. } => {
            let _ = config_dir;
            Err(AppError::Message(
                "service-run 必须由 OS service manager 入口直接分派".into(),
            ))
        }
        Command::ServiceAdminRun {
            action, config_dir, ..
        } => {
            let _ = (action, config_dir);
            Err(AppError::Message(
                "service-admin-run 必须由 Windows UAC helper 入口直接分派".into(),
            ))
        }
        Command::GatewayDaemonRun {
            config_scope,
            workspaces,
        } => run_gateway_daemon(&config_scope, &workspaces)
            .await
            .map(|_| 0),
        Command::ExecSupervisorRun { spec } => {
            crate::tools::command_session::run_durable_command_supervisor(spec)
                .await
                .map_err(AppError::Message)
        }
        Command::DaemonRun {
            workspace,
            service,
            tunnel_services,
            handoff,
        } => run_daemon(&workspace, service, tunnel_services, handoff)
            .await
            .map(|_| 0),
    }
}

#[derive(Serialize)]
struct WorkspaceSummary<'a> {
    id: &'a str,
    name: &'a str,
    path: &'a str,
    mcp_port: u16,
    actions_port: u16,
}

fn list_workspaces(as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let summaries: Vec<_> = store
        .list()
        .iter()
        .map(|profile| WorkspaceSummary {
            id: &profile.id,
            name: &profile.name,
            path: &profile.path,
            mcp_port: profile.runtime.local_port,
            actions_port: profile.actions.local_port,
        })
        .collect();

    if as_json {
        print_json(&summaries)?;
    } else if summaries.is_empty() {
        println!(
            "没有已配置的 workspace。请使用 `anchor workspace register PATH` 或 GUI 创建 profile。"
        );
    } else {
        for item in summaries {
            println!(
                "{}\t{}\t{}\tMCP:{}\tActions:{}",
                item.id, item.name, item.path, item.mcp_port, item.actions_port
            );
        }
    }
    Ok(())
}

fn show_workspace(selector: &str, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?;
    let value = serde_json::to_value(profile)?;

    if as_json {
        print_json(&value)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

async fn show_status(options: StatusOptions, as_json: bool) -> AppResult<()> {
    if options.control_plane {
        return show_control_plane_status(options, as_json).await;
    }
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        let statuses = workspace_statuses(options.workspace.as_deref()).await?;
        let single_workspace = options.workspace.is_some();
        if as_json && options.watch && single_workspace {
            print_json_line(&statuses[0])?;
        } else if as_json && options.watch {
            print_json_line(&json!({
                "event": "status_snapshot",
                "workspaces": statuses
            }))?;
        } else if as_json && single_workspace {
            print_json(&statuses[0])?;
        } else if as_json {
            print_json(&statuses)?;
        } else {
            for (index, status) in statuses.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                print_workspace_status(status);
            }
        }
        if !options.watch {
            return Ok(());
        }
        tokio::select! {
            result = &mut shutdown_signal => return result,
            _ = tokio::time::sleep(Duration::from_secs(options.interval_seconds)) => {}
        }
    }
}

async fn workspace_statuses(selector: Option<&str>) -> AppResult<Vec<WorkspaceControlStatus>> {
    let store = DataStore::load()?;
    match selector {
        Some(selector) => {
            let profile = resolve_workspace(store.list(), selector)?;
            Ok(vec![
                control::workspace_status_via_daemon_or_local(profile).await?,
            ])
        }
        None => {
            let mut statuses = Vec::with_capacity(store.list().len());
            for profile in store.list() {
                statuses.push(control::workspace_status_via_daemon_or_local(profile).await?);
            }
            Ok(statuses)
        }
    }
}

fn print_workspace_status(status: &WorkspaceControlStatus) {
    println!("{} ({})", status.name, status.id);
    println!("daemon\t{}", status.daemon.detail);
    print_port_status(&status.mcp);
    print_port_status(&status.actions);
}

fn print_port_status(status: &control::PortStatus) {
    if let Some(pid) = status.pid {
        println!(
            "{}\tlistening\t{}\tpid={}",
            status.service, status.endpoint, pid
        );
        println!("  owner={}", status.owner);
    } else {
        println!("{}\tstopped\t{}", status.service, status.endpoint);
    }
}

async fn serve_workspace(
    selector: &str,
    service: ServiceSelection,
    tunnel_services: Option<ServiceSelection>,
    as_json: bool,
    foreground: bool,
    mut control_commands: Option<control::DaemonControlReceiver>,
    mut serve_context: WorkspaceServeContext,
) -> AppResult<()> {
    let store = DataStore::load()?;
    let mut profile = resolve_workspace(store.list(), selector)?.clone();
    control::reset_workspace_event_stream(&profile.id);
    ensure_workspace_directory(&profile)?;
    if store.settings().mcp_gateway.enabled && service.includes_mcp() {
        return Err(AppError::Message(
            "MCP Gateway 模式请使用 `anchor gateway serve <workspace ...>`；单工作区 serve 不会创建共享路由。"
                .into(),
        ));
    }

    let mut runtime = RuntimeSupervisor::default();
    let mut started_services = Vec::new();
    let mut managed_tunnels = Vec::new();
    let mut tunnel_retries = std::collections::HashMap::new();

    #[cfg(all(unix, debug_assertions))]
    if let Some(handoff_id) = serve_context.handoff_id.as_deref() {
        if std::env::var_os("ANCHOR_TEST_HANDOFF_FAIL_AFTER_CUTOVER").is_some() {
            let error = AppError::Message(
                "debug handoff failpoint requested after canonical ownership acquisition".into(),
            );
            handoff::mark_failed(&profile, handoff_id, &error.to_string());
            return Err(error);
        }
    }

    let start_result = async {
        if service.includes_mcp() {
            #[cfg(unix)]
            let status = match serve_context.imported_listeners.as_mut() {
                Some(listeners) => match listeners.mcp.take() {
                    Some(listener) => runtime.start_from_handoff(
                        &profile,
                        ServiceKind::Mcp,
                        listener,
                        listeners.mcp_snapshot.take(),
                    )?,
                    None => runtime.start_mcp(&profile)?,
                },
                None => runtime.start_mcp(&profile)?,
            };
            #[cfg(not(unix))]
            let status = runtime.start_mcp(&profile)?;
            ensure_running(status, "MCP")?;
            started_services.push(ServiceKind::Mcp);
        }
        if service.includes_actions() {
            #[cfg(unix)]
            let status = match serve_context
                .imported_listeners
                .as_mut()
                .and_then(|listeners| listeners.actions.take())
            {
                Some(listener) => {
                    runtime.start_from_handoff(&profile, ServiceKind::Actions, listener, None)?
                }
                None => runtime.start_actions(&profile)?,
            };
            #[cfg(not(unix))]
            let status = runtime.start_actions(&profile)?;
            ensure_running(status, "Actions")?;
            started_services.push(ServiceKind::Actions);
        }

        if let Some(tunnel_services) = tunnel_services {
            for kind in selected_tunnels(tunnel_services) {
                managed_tunnels.push(kind);
                match maybe_start_for_runtime(&profile, kind).await {
                    Ok(Some(url)) => {
                        persist_daemon_tunnel_url(&mut profile, kind, &url)?;
                        if !as_json {
                            print_runtime_line(
                                &format!("{} tunnel\t{url}", tunnel_label(kind)),
                                foreground,
                                false,
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let attempts = 1;
                        let delay = cli_tunnel_retry_delay(attempts);
                        tunnel_retries.insert(
                            kind,
                            CliTunnelRetry {
                                attempts,
                                next_attempt: tokio::time::Instant::now() + delay,
                            },
                        );
                        if as_json {
                            print_json(&json!({
                                "event": "tunnel_retry_scheduled",
                                "service": tunnel_label(kind),
                                "attempt": attempts,
                                "retry_in_ms": delay.as_millis(),
                                "detail": error.to_string()
                            }))?;
                        } else {
                            print_runtime_line(
                                &format!(
                                    "{} tunnel 暂未连接，{} 秒后自动重试：{error}",
                                    tunnel_label(kind),
                                    delay.as_secs()
                                ),
                                foreground,
                                true,
                            );
                        }
                    }
                }
            }
        }
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = start_result {
        #[cfg(unix)]
        if let Some(handoff_id) = serve_context.handoff_id.as_deref() {
            handoff::mark_failed(&profile, handoff_id, &error.to_string());
        }
        let _ = shutdown(&mut runtime, &profile, &started_services, &managed_tunnels).await;
        return Err(error);
    }

    #[cfg(unix)]
    if let Some(handoff_id) = serve_context.handoff_id.as_deref() {
        if let Err(error) = handoff::mark_canonical_ready(&profile.id, handoff_id) {
            handoff::mark_failed(&profile, handoff_id, &error.to_string());
            let _ = shutdown(&mut runtime, &profile, &started_services, &managed_tunnels).await;
            return Err(error);
        }
    }

    if as_json {
        print_json(&json!({
            "event": "ready",
            "workspace": {"id": profile.id, "name": profile.name, "path": profile.path},
            "services": started_services.iter().map(|kind| service_label(*kind)).collect::<Vec<_>>(),
            "tunnel": tunnel_services.is_some(),
            "tunnelServices": tunnel_services.map(ServiceSelection::as_str)
        }))?;
    } else {
        print_runtime_line(
            &format!("workspace {} 已启动：", profile.name),
            foreground,
            false,
        );
        for kind in &started_services {
            print_runtime_line(
                &format!(
                    "{}\t{}",
                    service_label(*kind),
                    endpoint_for(&profile, *kind)
                ),
                foreground,
                false,
            );
        }
        if foreground {
            println!("前台运行中，按 Ctrl+C 停止。");
        } else {
            print_runtime_line("daemon 运行中，等待 SIGTERM/SIGINT 停止。", false, false);
        }
    }
    control::publish_workspace_event(
        &profile.id,
        control::ControlEventKind::DaemonReady,
        None,
        "running",
        format!(
            "daemon ready with service={} tunnels={}",
            service.as_str(),
            tunnel_services
                .map(ServiceSelection::as_str)
                .unwrap_or("none")
        ),
    );

    let mut maintenance = tokio::time::interval(std::time::Duration::from_secs(2));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    maintenance.tick().await;
    let mut last_states = std::collections::HashMap::new();
    let mut terminal_error = None;
    let mut handoff_completed = false;
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        tokio::select! {
            signal = &mut shutdown_signal => {
                if let Err(error) = signal {
                    terminal_error = Some(error);
                }
                break;
            }
            command = next_daemon_control_command(&mut control_commands) => {
                match command {
                    Some(control::DaemonControlCommand::Shutdown { operation }) => {
                        control::publish_workspace_event(
                            &profile.id,
                            control::ControlEventKind::DaemonStopping,
                            None,
                            "stopping",
                            format!("accepted {operation:?}"),
                        );
                        crate::tunnel::append_profile_log(
                            &profile.id,
                            "daemon.log",
                            &format!("[control] accepted {operation:?}; beginning graceful shutdown"),
                        );
                        break;
                    }
                    Some(control::DaemonControlCommand::Handoff {
                        handoff_id,
                        initiator_pid,
                        executable_path,
                        expected_build,
                    }) => {
                        #[cfg(unix)]
                        {
                            if tunnel_services.is_some() {
                                let error = AppError::Message(
                                    "zero-downtime handoff does not yet support managed tunnels"
                                        .into(),
                                );
                                let _ = daemon::create_handoff_state(
                                    &profile,
                                    &handoff_id,
                                    service,
                                    initiator_pid,
                                    expected_build,
                                    None,
                                    Path::new(&executable_path),
                                );
                                handoff::mark_failed(&profile, &handoff_id, &error.to_string());
                                crate::tunnel::append_profile_log(
                                    &profile.id,
                                    "daemon.log",
                                    &format!("[handoff] rejected before cutover: {error}"),
                                );
                                continue;
                            }
                            let Some(ownership) = serve_context.ownership.as_mut() else {
                                crate::tunnel::append_profile_log(
                                    &profile.id,
                                    "daemon.log",
                                    "[handoff] rejected: canonical ownership unavailable",
                                );
                                continue;
                            };
                            let prepared = match handoff::prepare_successor(
                                &profile,
                                service,
                                &handoff_id,
                                Path::new(&executable_path),
                                expected_build,
                                initiator_pid,
                                &runtime,
                            )
                            .await
                            {
                                Ok(prepared) => prepared,
                                Err(error) => {
                                    crate::tunnel::append_profile_log(
                                        &profile.id,
                                        "daemon.log",
                                        &format!("[handoff] successor preparation failed: {error}"),
                                    );
                                    continue;
                                }
                            };
                            if let Err(error) = handoff::mark_ownership_released(&profile, &prepared) {
                                handoff::mark_failed(&profile, &handoff_id, &error.to_string());
                                let _ = platform().terminate_process_tree(prepared.successor_pid);
                                crate::tunnel::append_profile_log(
                                    &profile.id,
                                    "daemon.log",
                                    &format!("[handoff] activation failed before cutover: {error}"),
                                );
                                continue;
                            }

                            let mut drains = Vec::new();
                            if service.includes_mcp() {
                                drains.push((
                                    ServiceKind::Mcp,
                                    runtime.begin_stop(&profile.id, ServiceKind::Mcp),
                                ));
                            }
                            if service.includes_actions() {
                                drains.push((
                                    ServiceKind::Actions,
                                    runtime.begin_stop(&profile.id, ServiceKind::Actions),
                                ));
                            }
                            ownership.release();

                            match handoff::wait_canonical_ready(
                                &profile,
                                &prepared,
                                Duration::from_secs(10),
                            )
                            .await
                            {
                                Ok(()) => {
                                    for (kind, handle) in drains {
                                        if let Some(mut handle) = handle {
                                            tokio::select! {
                                                _ = &mut handle => {}
                                                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                                                    handle.abort();
                                                    let _ = handle.await;
                                                }
                                            }
                                        }
                                        runtime.finish_stop(&profile.id, kind);
                                    }
                                    crate::tunnel::append_profile_log(
                                        &profile.id,
                                        "daemon.log",
                                        &format!(
                                            "[handoff] successor PID {} is canonical; predecessor drained",
                                            prepared.successor_pid
                                        ),
                                    );
                                    handoff_completed = true;
                                    break;
                                }
                                Err(error) => {
                                    handoff::mark_failed(&profile, &handoff_id, &error.to_string());
                                    terminal_error = Some(error);
                                    break;
                                }
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = (handoff_id, initiator_pid, executable_path, expected_build);
                            terminal_error = Some(AppError::Message(
                                "zero-downtime daemon handoff is unsupported on this platform"
                                    .into(),
                            ));
                            break;
                        }
                    }
                    Some(control::DaemonControlCommand::Tunnel {
                        operation_id,
                        service: tunnel_service,
                        action,
                    }) => {
                        control::mark_control_operation_running(&operation_id);
                        let result = apply_daemon_tunnel_command(
                            &mut profile,
                            service,
                            &mut managed_tunnels,
                            tunnel_service,
                            action,
                        )
                        .await;
                        if result.is_ok() {
                            tunnel_retries.remove(&tunnel_service);
                        }
                        match &result {
                            Ok(status) => control::publish_workspace_event(
                                &profile.id,
                                control::ControlEventKind::TunnelState,
                                Some(control_service_for_tunnel(tunnel_service)),
                                status.state.clone(),
                                status.public_url.clone(),
                            ),
                            Err(error) => control::publish_workspace_event(
                                &profile.id,
                                control::ControlEventKind::TunnelState,
                                Some(control_service_for_tunnel(tunnel_service)),
                                "error",
                                error.to_string(),
                            ),
                        }
                        control::finish_tunnel_operation(&operation_id, result);
                    }
                    Some(control::DaemonControlCommand::Reload {
                        operation_id,
                        service: reload_service,
                    }) => {
                        control::mark_control_operation_running(&operation_id);
                        let result = apply_daemon_reload_command(
                            &mut profile,
                            service,
                            &managed_tunnels,
                            &mut runtime,
                            reload_service,
                        )
                        .await;
                        match &result {
                            Ok(()) => control::publish_workspace_event(
                                &profile.id,
                                control::ControlEventKind::Reload,
                                Some(reload_service),
                                "succeeded",
                                "service configuration reloaded",
                            ),
                            Err(error) => control::publish_workspace_event(
                                &profile.id,
                                control::ControlEventKind::Reload,
                                Some(reload_service),
                                "failed",
                                error.to_string(),
                            ),
                        }
                        control::finish_reload_operation(&operation_id, result);
                    }
                    Some(control::DaemonControlCommand::ApplyConfig { operation_id }) => {
                        control::mark_control_operation_running(&operation_id);
                        let result = apply_daemon_config_command(
                            &mut profile,
                            service,
                            &mut managed_tunnels,
                            &mut runtime,
                        )
                        .await;
                        match &result {
                            Ok(applied) => control::publish_workspace_event(
                                &profile.id,
                                control::ControlEventKind::ConfigApply,
                                None,
                                "succeeded",
                                format!("workspace config applied changed={}", applied.changed),
                            ),
                            Err(error) => control::publish_workspace_event(
                                &profile.id,
                                control::ControlEventKind::ConfigApply,
                                None,
                                "failed",
                                error.to_string(),
                            ),
                        }
                        control::finish_config_apply_operation(&operation_id, result);
                    }
                    None => {
                        terminal_error = Some(AppError::Message(
                            "daemon control command channel closed unexpectedly".into(),
                        ));
                        break;
                    }
                }
            }
            _ = maintenance.tick() => {
                for kind in started_services.iter().copied() {
                    let status_result = match kind {
                        ServiceKind::Mcp => runtime.maintain_mcp(&profile),
                        ServiceKind::Actions => runtime.maintain_actions(&profile),
                    };
                    let status = match status_result {
                        Ok(status) => status,
                        Err(error) => {
                            terminal_error = Some(error);
                            break;
                        }
                    };
                    let previous = last_states.insert(kind, status.state.clone());
                    if previous.as_deref() != Some(status.state.as_str()) {
                        control::publish_workspace_event(
                            &profile.id,
                            control::ControlEventKind::ServiceState,
                            Some(control_service_for_runtime(kind)),
                            status.state.clone(),
                            status.local_message.clone(),
                        );
                        if as_json {
                            print_json(&json!({
                                "event": "service_state",
                                "service": service_label(kind),
                                "state": status.state,
                                "message": status.local_message,
                                "recovery": status.recovery
                            }))?;
                        } else if status.state == "recovering" {
                            print_runtime_line(&status.local_message, foreground, true);
                        } else if status.state == "running" && previous.as_deref() == Some("recovering") {
                            print_runtime_line(
                                &format!("{} 已自动恢复", service_label(kind)),
                                foreground,
                                false,
                            );
                        }
                    }
                    if status.state == "error" && !status.recovery.enabled {
                        terminal_error = Some(AppError::Message(format!(
                            "{}自动恢复失败：{}",
                            service_label(kind),
                            status.local_message
                        )));
                        break;
                    }
                }
                if terminal_error.is_some() {
                    break;
                }
                for kind in managed_tunnels.iter().copied() {
                    if tunnel_retries
                        .get(&kind)
                        .is_some_and(|retry| retry.next_attempt > tokio::time::Instant::now())
                    {
                        continue;
                    }
                    match ensure_for_runtime(&profile, kind).await {
                        Ok(_) => {
                            if let Some(previous) = tunnel_retries.remove(&kind) {
                                control::publish_workspace_event(
                                    &profile.id,
                                    control::ControlEventKind::TunnelState,
                                    Some(control_service_for_tunnel(kind)),
                                    "running",
                                    format!("reconnected after {} attempts", previous.attempts),
                                );
                                if as_json {
                                    print_json(&json!({
                                        "event": "tunnel_reconnected",
                                        "service": tunnel_label(kind),
                                        "attempts": previous.attempts
                                    }))?;
                                } else {
                                    print_runtime_line(
                                        &format!("{} tunnel 已自动恢复", tunnel_label(kind)),
                                        foreground,
                                        false,
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            if is_quick_tunnel_url_change_error(&error) {
                                tunnel_retries.remove(&kind);
                                terminal_error = Some(error);
                                break;
                            }
                            let attempts = tunnel_retries
                                .get(&kind)
                                .map(|retry| retry.attempts.saturating_add(1))
                                .unwrap_or(1);
                            let delay = cli_tunnel_retry_delay(attempts);
                            tunnel_retries.insert(
                                kind,
                                CliTunnelRetry {
                                    attempts,
                                    next_attempt: tokio::time::Instant::now() + delay,
                                },
                            );
                            control::publish_workspace_event(
                                &profile.id,
                                control::ControlEventKind::TunnelState,
                                Some(control_service_for_tunnel(kind)),
                                "recovering",
                                error.to_string(),
                            );
                            if as_json {
                                print_json(&json!({
                                    "event": "tunnel_retry_scheduled",
                                    "service": tunnel_label(kind),
                                    "attempt": attempts,
                                    "retry_in_ms": delay.as_millis(),
                                    "detail": error.to_string()
                                }))?;
                            } else {
                                print_runtime_line(
                                    &format!(
                                        "{} tunnel 自动重连失败，{} 秒后重试：{error}",
                                        tunnel_label(kind),
                                        delay.as_secs()
                                    ),
                                    foreground,
                                    true,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    if !handoff_completed {
        if !as_json && foreground {
            println!("正在停止……");
        }
        shutdown(&mut runtime, &profile, &started_services, &managed_tunnels).await?;
        if as_json {
            print_json(&json!({"event": "stopped", "workspace_id": profile.id}))?;
        }
    }
    match terminal_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn print_runtime_line(message: &str, foreground: bool, error: bool) {
    let message = if foreground {
        message.to_string()
    } else {
        crate::logging::timestamped_line(message)
    };
    if error {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

fn ensure_running(status: RuntimeStatusDto, label: &str) -> AppResult<()> {
    if status.state == "running" {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "{label} 启动失败：{}",
            status.local_message
        )))
    }
}

async fn shutdown(
    runtime: &mut RuntimeSupervisor,
    profile: &WorkspaceProfile,
    services: &[ServiceKind],
    tunnels: &[TunnelServiceKind],
) -> AppResult<()> {
    let mut first_error = None;
    // Stop ingress first. Keeping FRP/Cloudflare alive while the local server is
    // draining can preserve or recreate a TCP connection to the business port,
    // which is especially harmful on Windows where a recently closed endpoint
    // can make the successor bind fail with WSAEADDRINUSE (10048).
    for kind in tunnels.iter().rev().copied() {
        if let Err(error) = stop_for_runtime(profile, kind).await {
            first_error.get_or_insert(error);
        }
    }

    for kind in services.iter().rev().copied() {
        let handle = runtime.begin_stop(&profile.id, kind);
        await_listener_shutdown(handle, port_for(profile, kind)).await;
        runtime.finish_stop(&profile.id, kind);
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn selected_tunnels(service: ServiceSelection) -> Vec<TunnelServiceKind> {
    let mut values = Vec::new();
    if service.includes_mcp() {
        values.push(TunnelServiceKind::Mcp);
    }
    if service.includes_actions() {
        values.push(TunnelServiceKind::Actions);
    }
    values
}

fn managed_tunnel_selection(tunnels: &[TunnelServiceKind]) -> Option<ServiceSelection> {
    match (
        tunnels.contains(&TunnelServiceKind::Mcp),
        tunnels.contains(&TunnelServiceKind::Actions),
    ) {
        (true, true) => Some(ServiceSelection::All),
        (true, false) => Some(ServiceSelection::Mcp),
        (false, true) => Some(ServiceSelection::Actions),
        (false, false) => None,
    }
}

fn tunnel_type_for_profile(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> &str {
    match kind {
        TunnelServiceKind::Mcp => profile.tunnel.tunnel_type.as_str(),
        TunnelServiceKind::Actions => profile.actions.tunnel_type.as_str(),
    }
}

fn tunnel_config_matches(
    left: &WorkspaceProfile,
    right: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> bool {
    match kind {
        TunnelServiceKind::Mcp => {
            left.tunnel.tunnel_type == right.tunnel.tunnel_type
                && left.tunnel.public_url == right.tunnel.public_url
                && left.tunnel.frp_server == right.tunnel.frp_server
                && left.tunnel.frp_subdomain == right.tunnel.frp_subdomain
                && left.tunnel.frp_profile_id == right.tunnel.frp_profile_id
                && left.tunnel.frp_server_port == right.tunnel.frp_server_port
                && left.tunnel.frp_proxy_type == right.tunnel.frp_proxy_type
                && left.tunnel.frp_cert_path == right.tunnel.frp_cert_path
                && left.tunnel.frp_key_path == right.tunnel.frp_key_path
                && left.tunnel.cloudflare_mode == right.tunnel.cloudflare_mode
                && left.tunnel.use_proxy == right.tunnel.use_proxy
        }
        TunnelServiceKind::Actions => {
            left.actions.public_url == right.actions.public_url
                && left.actions.tunnel_type == right.actions.tunnel_type
                && left.actions.frp_server == right.actions.frp_server
                && left.actions.frp_subdomain == right.actions.frp_subdomain
                && left.actions.frp_profile_id == right.actions.frp_profile_id
                && left.actions.frp_server_port == right.actions.frp_server_port
                && left.actions.frp_proxy_type == right.actions.frp_proxy_type
                && left.actions.frp_cert_path == right.actions.frp_cert_path
                && left.actions.frp_key_path == right.actions.frp_key_path
                && left.actions.cloudflare_mode == right.actions.cloudflare_mode
                && left.actions.use_proxy == right.actions.use_proxy
        }
    }
}

fn restore_daemon_tunnel_config(
    failed: &WorkspaceProfile,
    restored: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<()> {
    let mut store = DataStore::load()?;
    let Some(mut current) = store.get(&failed.id).cloned() else {
        return Ok(());
    };
    if !tunnel_config_matches(&current, failed, kind) {
        return Err(AppError::Message(
            "检测到更新的隧道配置，已拒绝用旧 daemon 配置覆盖。".into(),
        ));
    }
    match kind {
        TunnelServiceKind::Mcp => current.tunnel = restored.tunnel.clone(),
        TunnelServiceKind::Actions => {
            current.actions.public_url = restored.actions.public_url.clone();
            current.actions.tunnel_type = restored.actions.tunnel_type.clone();
            current.actions.frp_server = restored.actions.frp_server.clone();
            current.actions.frp_subdomain = restored.actions.frp_subdomain.clone();
            current.actions.frp_profile_id = restored.actions.frp_profile_id.clone();
            current.actions.frp_server_port = restored.actions.frp_server_port;
            current.actions.frp_proxy_type = restored.actions.frp_proxy_type.clone();
            current.actions.frp_cert_path = restored.actions.frp_cert_path.clone();
            current.actions.frp_key_path = restored.actions.frp_key_path.clone();
            current.actions.cloudflare_mode = restored.actions.cloudflare_mode.clone();
            current.actions.use_proxy = restored.actions.use_proxy;
        }
    }
    store.update(current)
}

fn persist_daemon_tunnel_url(
    profile: &mut WorkspaceProfile,
    kind: TunnelServiceKind,
    public_url: &str,
) -> AppResult<()> {
    if public_url.is_empty() {
        return Ok(());
    }
    let mut store = DataStore::load()?;
    let Some(mut current) = store.get(&profile.id).cloned() else {
        return Ok(());
    };
    if !tunnel_config_matches(&current, profile, kind) {
        return Ok(());
    }
    match kind {
        TunnelServiceKind::Mcp => {
            current.tunnel.public_url = public_url.to_string();
            profile.tunnel.public_url = public_url.to_string();
            update_public_url(&profile.id, "mcp", public_url);
        }
        TunnelServiceKind::Actions => {
            current.actions.public_url = public_url.to_string();
            profile.actions.public_url = public_url.to_string();
            update_public_url(&profile.id, "actions", public_url);
        }
    }
    store.update(current)
}

async fn apply_daemon_tunnel_command(
    profile: &mut WorkspaceProfile,
    service_selection: ServiceSelection,
    managed_tunnels: &mut Vec<TunnelServiceKind>,
    kind: TunnelServiceKind,
    action: control::ControlTunnelAction,
) -> AppResult<TunnelStatus> {
    let store = DataStore::load()?;
    let latest = resolve_workspace(store.list(), &profile.id)?.clone();
    let settings = store.settings();
    drop(store);
    if kind == TunnelServiceKind::Mcp && settings.mcp_gateway.enabled {
        return Err(AppError::Message(
            "MCP 隧道已切换到 Gateway 控制域，Workspace daemon 拒绝直接写入".into(),
        ));
    }

    let was_managed = managed_tunnels.contains(&kind);
    let previous = profile.clone();
    let operation = async {
        let mut tunnels = tunnel_supervisor().lock().await;
        match action {
            control::ControlTunnelAction::Start => {
                if was_managed {
                    Ok(tunnels.status(profile, kind, &settings))
                } else {
                    tunnels.start(&latest, kind, &settings).await
                }
            }
            control::ControlTunnelAction::Stop => {
                if was_managed {
                    tunnels.stop(profile, kind, &settings).await?;
                }
                Ok(tunnels.status(&latest, kind, &settings))
            }
            control::ControlTunnelAction::Restart => {
                if !was_managed {
                    return Ok(tunnels.status(&latest, kind, &settings));
                }
                if tunnel_type_for_profile(&latest, kind) == "frp" {
                    tunnels.start(&latest, kind, &settings).await
                } else {
                    tunnels.stop(profile, kind, &settings).await?;
                    match tunnels.start(&latest, kind, &settings).await {
                        Ok(status) => Ok(status),
                        Err(error) => match tunnels.start(&previous, kind, &settings).await {
                            Ok(_) => Err(error),
                            Err(rollback_error) => Err(AppError::Message(format!(
                                "隧道重载失败：{error}；恢复原线路也失败：{rollback_error}"
                            ))),
                        },
                    }
                }
            }
        }
    }
    .await;

    let status = match operation {
        Ok(status) => status,
        Err(error) => {
            if action == control::ControlTunnelAction::Restart {
                if let Err(rollback_error) = restore_daemon_tunnel_config(&latest, &previous, kind)
                {
                    return Err(AppError::Message(format!(
                        "隧道重载失败：{error}；配置回滚也失败：{rollback_error}"
                    )));
                }
            }
            return Err(error);
        }
    };

    match action {
        control::ControlTunnelAction::Start if status.state == "running" => {
            if !managed_tunnels.contains(&kind) {
                managed_tunnels.push(kind);
            }
            *profile = latest;
        }
        control::ControlTunnelAction::Stop => {
            managed_tunnels.retain(|candidate| *candidate != kind);
            *profile = latest;
        }
        control::ControlTunnelAction::Restart if was_managed => {
            *profile = latest;
        }
        _ => {}
    }
    persist_daemon_tunnel_url(profile, kind, &status.public_url)?;
    daemon::update_tunnel_services(
        profile,
        service_selection,
        managed_tunnel_selection(managed_tunnels),
    )?;
    Ok(status)
}

async fn apply_daemon_reload_command(
    profile: &mut WorkspaceProfile,
    service_selection: ServiceSelection,
    managed_tunnels: &[TunnelServiceKind],
    runtime: &mut RuntimeSupervisor,
    service: control::ControlService,
) -> AppResult<()> {
    let store = DataStore::load()?;
    let latest = resolve_workspace(store.list(), &profile.id)?.clone();
    drop(store);
    ensure_workspace_directory(&latest)?;

    let kind = match service {
        control::ControlService::Mcp => ServiceKind::Mcp,
        control::ControlService::Actions => ServiceKind::Actions,
    };
    let selected = match service {
        control::ControlService::Mcp => service_selection.includes_mcp(),
        control::ControlService::Actions => service_selection.includes_actions(),
    };
    if !selected {
        return Err(AppError::Message(format!(
            "daemon 当前未运行 {}，不能对未活动服务执行 reload",
            service_label(kind)
        )));
    }
    let previous = profile.clone();

    let handle = runtime.begin_stop(&profile.id, kind);
    await_listener_shutdown(handle, port_for(&previous, kind)).await;
    runtime.finish_stop(&profile.id, kind);

    let reload_result = match kind {
        ServiceKind::Mcp => runtime
            .start_mcp(&latest)
            .and_then(|status| ensure_running(status, "MCP")),
        ServiceKind::Actions => runtime
            .start_actions(&latest)
            .and_then(|status| ensure_running(status, "Actions")),
    };
    if let Err(error) = reload_result {
        let handle = runtime.begin_stop(&profile.id, kind);
        await_listener_shutdown(handle, port_for(&latest, kind)).await;
        runtime.finish_stop(&profile.id, kind);
        let rollback = match kind {
            ServiceKind::Mcp => runtime
                .start_mcp(&previous)
                .and_then(|status| ensure_running(status, "MCP")),
            ServiceKind::Actions => runtime
                .start_actions(&previous)
                .and_then(|status| ensure_running(status, "Actions")),
        };
        return match rollback {
            Ok(()) => Err(AppError::Message(format!(
                "{} 配置 reload 失败，已恢复旧 listener：{error}",
                service_label(kind)
            ))),
            Err(rollback_error) => Err(AppError::Message(format!(
                "{} 配置 reload 失败：{error}；恢复旧 listener 也失败：{rollback_error}",
                service_label(kind)
            ))),
        };
    }

    *profile = latest;
    daemon::update_tunnel_services(
        profile,
        service_selection,
        managed_tunnel_selection(managed_tunnels),
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppliedRuntimeConfigChange {
    None,
    ListenerReload,
    CallbackHotUpdate,
}

async fn reload_runtime_service_to_profile(
    runtime: &mut RuntimeSupervisor,
    current: &WorkspaceProfile,
    target: &WorkspaceProfile,
    kind: ServiceKind,
) -> AppResult<()> {
    let handle = runtime.begin_stop(&current.id, kind);
    await_listener_shutdown(handle, port_for(current, kind)).await;
    runtime.finish_stop(&current.id, kind);

    let start_target = match kind {
        ServiceKind::Mcp => runtime
            .start_mcp(target)
            .and_then(|status| ensure_running(status, "MCP")),
        ServiceKind::Actions => runtime
            .start_actions(target)
            .and_then(|status| ensure_running(status, "Actions")),
    };
    if let Err(error) = start_target {
        let handle = runtime.begin_stop(&target.id, kind);
        await_listener_shutdown(handle, port_for(target, kind)).await;
        runtime.finish_stop(&target.id, kind);
        let rollback = match kind {
            ServiceKind::Mcp => runtime
                .start_mcp(current)
                .and_then(|status| ensure_running(status, "MCP")),
            ServiceKind::Actions => runtime
                .start_actions(current)
                .and_then(|status| ensure_running(status, "Actions")),
        };
        return match rollback {
            Ok(()) => Err(AppError::Message(format!(
                "{} 配置应用失败，已恢复旧 listener：{error}",
                service_label(kind)
            ))),
            Err(rollback_error) => Err(AppError::Message(format!(
                "{} 配置应用失败：{error}；恢复旧 listener 也失败：{rollback_error}",
                service_label(kind)
            ))),
        };
    }
    Ok(())
}

async fn apply_runtime_config_change(
    runtime: &mut RuntimeSupervisor,
    current: &WorkspaceProfile,
    target: &WorkspaceProfile,
    service_selection: ServiceSelection,
    service: control::ControlService,
    listener_reload: bool,
    callback_hot_update: bool,
) -> AppResult<AppliedRuntimeConfigChange> {
    let (selected, kind, service_name, oauth_enabled, redirect_uris, redirect_hosts) = match service
    {
        control::ControlService::Mcp => (
            service_selection.includes_mcp(),
            ServiceKind::Mcp,
            "mcp",
            target.auth.auth_type == "oauth",
            target.auth.oauth_redirect_uris.as_str(),
            target.auth.oauth_redirect_hosts.as_str(),
        ),
        control::ControlService::Actions => (
            service_selection.includes_actions(),
            ServiceKind::Actions,
            "actions",
            target.actions.auth_type == "oauth",
            target.actions.oauth_redirect_uris.as_str(),
            target.actions.oauth_redirect_hosts.as_str(),
        ),
    };
    if !selected {
        return Ok(AppliedRuntimeConfigChange::None);
    }
    if listener_reload {
        reload_runtime_service_to_profile(runtime, current, target, kind).await?;
        return Ok(AppliedRuntimeConfigChange::ListenerReload);
    }
    if callback_hot_update && oauth_enabled {
        let updated = crate::auth::update_oauth_redirect_policy(
            &target.id,
            service_name,
            redirect_uris,
            redirect_hosts,
        )
        .map_err(AppError::Message)?;
        if updated {
            return Ok(AppliedRuntimeConfigChange::CallbackHotUpdate);
        }
        reload_runtime_service_to_profile(runtime, current, target, kind).await?;
        return Ok(AppliedRuntimeConfigChange::ListenerReload);
    }
    Ok(AppliedRuntimeConfigChange::None)
}

async fn rollback_runtime_config_change(
    runtime: &mut RuntimeSupervisor,
    current: &WorkspaceProfile,
    target: &WorkspaceProfile,
    service: control::ControlService,
    applied: AppliedRuntimeConfigChange,
) -> AppResult<()> {
    match applied {
        AppliedRuntimeConfigChange::None => Ok(()),
        AppliedRuntimeConfigChange::ListenerReload => {
            let kind = match service {
                control::ControlService::Mcp => ServiceKind::Mcp,
                control::ControlService::Actions => ServiceKind::Actions,
            };
            reload_runtime_service_to_profile(runtime, target, current, kind).await
        }
        AppliedRuntimeConfigChange::CallbackHotUpdate => {
            let (service_name, redirect_uris, redirect_hosts) = match service {
                control::ControlService::Mcp => (
                    "mcp",
                    current.auth.oauth_redirect_uris.as_str(),
                    current.auth.oauth_redirect_hosts.as_str(),
                ),
                control::ControlService::Actions => (
                    "actions",
                    current.actions.oauth_redirect_uris.as_str(),
                    current.actions.oauth_redirect_hosts.as_str(),
                ),
            };
            let updated = crate::auth::update_oauth_redirect_policy(
                &current.id,
                service_name,
                redirect_uris,
                redirect_hosts,
            )
            .map_err(AppError::Message)?;
            if updated {
                Ok(())
            } else {
                Err(AppError::Message(format!(
                    "恢复 {service_name} OAuth Callback 策略时活动 runtime 已不存在"
                )))
            }
        }
    }
}

async fn reconcile_managed_tunnel_config(
    current: &WorkspaceProfile,
    target: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &crate::settings::AppSettings,
) -> AppResult<TunnelStatus> {
    let current_type = tunnel_type_for_profile(current, kind);
    let target_type = tunnel_type_for_profile(target, kind);
    let mut tunnels = tunnel_supervisor().lock().await;
    if target_type == "none" {
        tunnels.stop(current, kind, settings).await?;
        return Ok(tunnels.status(target, kind, settings));
    }
    if current_type == "frp" && target_type == "frp" {
        return tunnels.start(target, kind, settings).await;
    }

    tunnels.stop(current, kind, settings).await?;
    match tunnels.start(target, kind, settings).await {
        Ok(status) => Ok(status),
        Err(error) => {
            let rollback = if current_type == "none" {
                Ok(())
            } else {
                tunnels.start(current, kind, settings).await.map(|_| ())
            };
            match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(AppError::Message(format!(
                    "{} 隧道配置应用失败：{error}；恢复旧线路也失败：{rollback_error}",
                    tunnel_label(kind)
                ))),
            }
        }
    }
}

async fn rollback_managed_tunnel_config(
    current: &WorkspaceProfile,
    target: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &crate::settings::AppSettings,
) -> AppResult<()> {
    reconcile_managed_tunnel_config(target, current, kind, settings)
        .await
        .map(|_| ())
}

async fn apply_daemon_config_command(
    profile: &mut WorkspaceProfile,
    service_selection: ServiceSelection,
    managed_tunnels: &mut Vec<TunnelServiceKind>,
    runtime: &mut RuntimeSupervisor,
) -> AppResult<control::ControlConfigApplyResult> {
    let store = DataStore::load()?;
    let latest = resolve_workspace(store.list(), &profile.id)?.clone();
    let settings = store.settings();
    drop(store);
    ensure_workspace_directory(&latest)?;
    let previous = profile.clone();
    let plan = plan_workspace_config_apply(&previous, &latest);
    let changed = plan.has_changes();
    if !changed {
        *profile = latest;
        daemon::update_tunnel_services(
            profile,
            service_selection,
            managed_tunnel_selection(managed_tunnels),
        )?;
        return Ok(control::ControlConfigApplyResult {
            changed: false,
            mcp_listener_reloaded: false,
            actions_listener_reloaded: false,
            mcp_callback_hot_updated: false,
            actions_callback_hot_updated: false,
            mcp_tunnel_reloaded: false,
            actions_tunnel_reloaded: false,
        });
    }

    let mcp_change = apply_runtime_config_change(
        runtime,
        &previous,
        &latest,
        service_selection,
        control::ControlService::Mcp,
        plan.mcp_listener_reload,
        plan.mcp_callback_policy_hot_update,
    )
    .await?;
    let actions_change = match apply_runtime_config_change(
        runtime,
        &previous,
        &latest,
        service_selection,
        control::ControlService::Actions,
        plan.actions_listener_reload,
        plan.actions_callback_policy_hot_update,
    )
    .await
    {
        Ok(change) => change,
        Err(error) => {
            let rollback = rollback_runtime_config_change(
                runtime,
                &previous,
                &latest,
                control::ControlService::Mcp,
                mcp_change,
            )
            .await;
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(AppError::Message(format!(
                    "Actions 配置应用失败：{error}；恢复 MCP 运行态也失败：{rollback_error}"
                ))),
            };
        }
    };

    let mcp_tunnel_managed =
        managed_tunnels.contains(&TunnelServiceKind::Mcp) && !settings.mcp_gateway.enabled;
    let actions_tunnel_managed = managed_tunnels.contains(&TunnelServiceKind::Actions);
    let mut applied_tunnels = Vec::new();
    for (kind, should_apply) in [
        (
            TunnelServiceKind::Mcp,
            mcp_tunnel_managed && plan.mcp_tunnel_changed,
        ),
        (
            TunnelServiceKind::Actions,
            actions_tunnel_managed && plan.actions_tunnel_changed,
        ),
    ] {
        if !should_apply {
            continue;
        }
        match reconcile_managed_tunnel_config(&previous, &latest, kind, &settings).await {
            Ok(status) => applied_tunnels.push((kind, status)),
            Err(error) => {
                let mut rollback_errors = Vec::new();
                for (applied_kind, _) in applied_tunnels.iter().rev() {
                    if let Err(rollback_error) =
                        rollback_managed_tunnel_config(&previous, &latest, *applied_kind, &settings)
                            .await
                    {
                        rollback_errors.push(rollback_error.to_string());
                    }
                }
                if let Err(rollback_error) = rollback_runtime_config_change(
                    runtime,
                    &previous,
                    &latest,
                    control::ControlService::Actions,
                    actions_change,
                )
                .await
                {
                    rollback_errors.push(rollback_error.to_string());
                }
                if let Err(rollback_error) = rollback_runtime_config_change(
                    runtime,
                    &previous,
                    &latest,
                    control::ControlService::Mcp,
                    mcp_change,
                )
                .await
                {
                    rollback_errors.push(rollback_error.to_string());
                }
                return if rollback_errors.is_empty() {
                    Err(error)
                } else {
                    Err(AppError::Message(format!(
                        "隧道配置应用失败：{error}；运行态回滚存在错误：{}",
                        rollback_errors.join("；")
                    )))
                };
            }
        }
    }

    *profile = latest;
    for (kind, status) in &applied_tunnels {
        if tunnel_type_for_profile(profile, *kind) == "none" {
            managed_tunnels.retain(|candidate| *candidate != *kind);
        }
        persist_daemon_tunnel_url(profile, *kind, &status.public_url)?;
    }
    daemon::update_tunnel_services(
        profile,
        service_selection,
        managed_tunnel_selection(managed_tunnels),
    )?;

    Ok(control::ControlConfigApplyResult {
        changed: true,
        mcp_listener_reloaded: mcp_change == AppliedRuntimeConfigChange::ListenerReload,
        actions_listener_reloaded: actions_change == AppliedRuntimeConfigChange::ListenerReload,
        mcp_callback_hot_updated: mcp_change == AppliedRuntimeConfigChange::CallbackHotUpdate,
        actions_callback_hot_updated: actions_change
            == AppliedRuntimeConfigChange::CallbackHotUpdate,
        mcp_tunnel_reloaded: applied_tunnels
            .iter()
            .any(|(kind, _)| *kind == TunnelServiceKind::Mcp),
        actions_tunnel_reloaded: applied_tunnels
            .iter()
            .any(|(kind, _)| *kind == TunnelServiceKind::Actions),
    })
}

fn control_service_for_runtime(kind: ServiceKind) -> control::ControlService {
    match kind {
        ServiceKind::Mcp => control::ControlService::Mcp,
        ServiceKind::Actions => control::ControlService::Actions,
    }
}

fn control_service_for_tunnel(kind: TunnelServiceKind) -> control::ControlService {
    match kind {
        TunnelServiceKind::Mcp => control::ControlService::Mcp,
        TunnelServiceKind::Actions => control::ControlService::Actions,
    }
}

fn ensure_workspace_directory(profile: &WorkspaceProfile) -> AppResult<()> {
    let path = Path::new(&profile.path);
    if !path.is_dir() {
        return Err(AppError::Message(format!(
            "workspace 目录不存在或不是目录：{}",
            path.display()
        )));
    }
    Ok(())
}

fn resolve_workspace<'a>(
    profiles: &'a [WorkspaceProfile],
    selector: &str,
) -> AppResult<&'a WorkspaceProfile> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(AppError::Message("workspace 不能为空".into()));
    }
    if let Some(profile) = profiles.iter().find(|profile| profile.id == selector) {
        return Ok(profile);
    }

    let normalized = normalize_path(selector);
    if let Some(profile) = profiles
        .iter()
        .find(|profile| normalize_path(&profile.path) == normalized)
    {
        return Ok(profile);
    }

    let named: Vec<_> = profiles
        .iter()
        .filter(|profile| profile.name.eq_ignore_ascii_case(selector))
        .collect();
    match named.as_slice() {
        [profile] => Ok(*profile),
        [] => Err(AppError::Message(format!(
            "找不到 workspace/profile：{selector}"
        ))),
        _ => Err(AppError::Message(format!(
            "workspace 名称不唯一，请改用 profile ID 或项目路径：{selector}"
        ))),
    }
}

fn normalize_path(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized.to_string()
    }
}

fn port_for(profile: &WorkspaceProfile, kind: ServiceKind) -> u16 {
    match kind {
        ServiceKind::Mcp => profile.runtime.local_port,
        ServiceKind::Actions => profile.actions.local_port,
    }
}

fn endpoint_for(profile: &WorkspaceProfile, kind: ServiceKind) -> String {
    match kind {
        ServiceKind::Mcp => profile.local_endpoint(),
        ServiceKind::Actions => profile.actions_local_base_url(),
    }
}

fn service_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "mcp",
        ServiceKind::Actions => "actions",
    }
}

fn tunnel_label(kind: TunnelServiceKind) -> &'static str {
    match kind {
        TunnelServiceKind::Mcp => "mcp",
        TunnelServiceKind::Actions => "actions",
    }
}

fn print_json(value: &impl Serialize) -> AppResult<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_json_line(value: &impl Serialize) -> AppResult<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

async fn wait_for_shutdown_signal() -> AppResult<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|error| AppError::Message(format!("无法监听 SIGTERM：{error}")))?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|error| AppError::Message(format!("无法监听 SIGINT：{error}")))?;
        tokio::select! {
            _ = terminate.recv() => Ok(()),
            _ = interrupt.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|error| AppError::Message(format!("无法监听 Ctrl+C：{error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_retry_delay_uses_bounded_exponential_backoff() {
        assert_eq!(cli_tunnel_retry_delay(1), Duration::from_secs(1));
        assert_eq!(cli_tunnel_retry_delay(2), Duration::from_secs(2));
        assert_eq!(cli_tunnel_retry_delay(3), Duration::from_secs(4));
        assert_eq!(cli_tunnel_retry_delay(6), Duration::from_secs(32));
        assert_eq!(cli_tunnel_retry_delay(7), Duration::from_secs(60));
        assert_eq!(cli_tunnel_retry_delay(20), Duration::from_secs(60));
    }

    #[test]
    fn log_tail_returns_only_requested_lines() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("daemon.log");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").expect("log fixture");

        let tail = read_tail_lines(&path, 2).expect("tail");

        assert_eq!(tail, "three\nfour\n");
    }

    #[test]
    fn cli_log_selection_includes_diagnostic_logs() {
        let mut profile = WorkspaceProfile::new(".".into(), Some("logs".into()));
        profile.tunnel.tunnel_type = "none".into();
        profile.actions.tunnel_type = "none".into();

        let mcp = selected_log_files(&profile, LogSelection::Mcp);
        let actions = selected_log_files(&profile, LogSelection::Actions);

        assert!(mcp.iter().any(|(name, _)| name == "mcp-oauth"));
        assert!(mcp.iter().any(|(name, _)| name == "mcp-requests"));
        assert!(actions.iter().any(|(name, _)| name == "actions-oauth"));
    }

    #[test]
    fn resolves_one_workspace_to_one_profile_by_id_name_or_path() {
        let first = WorkspaceProfile::new("/srv/projects/alpha".into(), Some("alpha".into()));
        let second = WorkspaceProfile::new("/srv/projects/beta".into(), Some("beta".into()));
        let profiles = vec![first.clone(), second];

        assert_eq!(
            resolve_workspace(&profiles, &first.id).expect("id").id,
            first.id
        );
        assert_eq!(
            resolve_workspace(&profiles, "ALPHA").expect("name").id,
            first.id
        );
        assert_eq!(
            resolve_workspace(&profiles, "/srv/projects/alpha/")
                .expect("path")
                .id,
            first.id
        );
    }

    #[test]
    fn rejects_ambiguous_workspace_names() {
        let profiles = vec![
            WorkspaceProfile::new("/srv/a".into(), Some("same".into())),
            WorkspaceProfile::new("/srv/b".into(), Some("same".into())),
        ];

        let error = resolve_workspace(&profiles, "same").expect_err("ambiguous");

        assert!(error.to_string().contains("名称不唯一"));
    }

    #[cfg(windows)]
    #[test]
    fn gateway_daemon_is_supported_on_windows() {
        assert!(gateway_daemon::supported());
    }
}
