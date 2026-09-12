use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::control::{
    self, ControlLogChunk, ControlLogSelection, DaemonLaunchSpec, WorkspaceControlStatus,
};
use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::gateway_control::{self, GatewayControlStatus, GatewayEventBatch, GatewayEventCursor};
use crate::platform::{open_path_in_file_manager, platform};
use crate::settings::{
    AppSettings, DownloadConfig, FrpProfile, FrpProfileInput, McpGatewayConfig, ProxyConfig,
};
use crate::tunnel::{
    drop_workspace as drop_tunnel_workspace, TunnelProfile, TunnelServiceKind, TunnelStatus,
};
use crate::workspace::resources::{
    assign_free_workspace_ports_with_reserved, validate_service_start, WorkspaceService,
};
use crate::workspace::{RuntimeRecoveryDto, RuntimeStatusDto, WorkspaceProfile};

const MANAGEMENT_DAEMON_TIMEOUT: Duration = Duration::from_secs(15);
const MANAGEMENT_TUNNEL_TIMEOUT: Duration = Duration::from_secs(15);
const FRP_PROFILE_TOKEN_SCOPE: &str = "frp_profile_token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayConfigWriteAction {
    PersistLocally,
    ApplyViaDaemon { pid: u32 },
    ShutdownThenPersist { pid: u32 },
}

pub(crate) fn federation_signing_status(
) -> AppResult<crate::federation::FederationLocalSigningStatus> {
    crate::federation::local_signing_status()
}

pub(crate) fn federation_bootstrap_bundle(
) -> AppResult<crate::federation::FederationBootstrapBundle> {
    let profiles = list_workspaces()?;
    let descriptor = crate::federation::local_peer_descriptor(&profiles)?;
    crate::federation::local_bootstrap_bundle(&descriptor)
}

pub(crate) fn rotate_federation_signing_key(
) -> AppResult<crate::federation::FederationLocalSigningStatus> {
    let profiles = list_workspaces()?;
    let descriptor = crate::federation::local_peer_descriptor(&profiles)?;
    crate::federation::rotate_local_signing_identity(&descriptor)
}

pub(crate) fn federation_discovery_document(
) -> AppResult<crate::federation::FederationDiscoveryDocument> {
    let profiles = list_workspaces()?;
    crate::federation::local_discovery_document(&profiles)
}

pub(crate) async fn inspect_federation_peer_discovery(
    node_id: &str,
) -> AppResult<crate::federation::FederationDiscoveryInspection> {
    crate::federation::inspect_registered_peer_discovery(node_id).await
}

pub(crate) fn accept_federation_peer_rebootstrap(
    node_id: &str,
    discovery: &crate::federation::FederationDiscoveryDocument,
) -> AppResult<crate::federation::FederationPeerView> {
    crate::federation::accept_discovered_rebootstrap(node_id, discovery)
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
            #[cfg(target_os = "linux")]
            if let Err(plan_error) =
                crate::linux_service::set_workspace_desired(&profile.id, Some(previous))
            {
                return AppError::Message(format!(
                    "{primary}；已恢复 Workspace daemon，但恢复 Linux Service 计划失败：{plan_error}"
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
        #[cfg(target_os = "linux")]
        crate::linux_service::set_gateway_desired(&desired_routes)?;
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
    #[cfg(target_os = "linux")]
    crate::linux_service::set_gateway_desired(&desired_routes)?;
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

pub(crate) fn list_workspaces() -> AppResult<Vec<WorkspaceProfile>> {
    DataStore::read_file(|data| Ok(data.profiles.clone()))
}

fn workspace_profile(id: &str) -> AppResult<WorkspaceProfile> {
    DataStore::read_file(|data| {
        data.profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })
}

pub(crate) fn frp_profile_has_token(id: &str) -> AppResult<bool> {
    DataStore::read_file(|data| {
        if !data.frp_profiles.iter().any(|profile| profile.id == id) {
            return Err(AppError::Message(format!("FRP profile not found: {id}")));
        }
        Ok(data
            .app_secrets
            .get(FRP_PROFILE_TOKEN_SCOPE)
            .and_then(|tokens| tokens.get(id))
            .is_some_and(|value| !value.trim().is_empty()))
    })
}

pub(crate) fn set_frp_profile_token(id: &str, token: &str) -> AppResult<FrpProfileDto> {
    let token = token.trim();
    if token.is_empty() {
        return Err(AppError::Message("FRP Token 不能为空。".into()));
    }
    let (profile, workspaces, gateway, unchanged) = DataStore::read_file(|data| {
        let profile = data
            .frp_profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("FRP profile not found: {id}")))?;
        let unchanged = data
            .app_secrets
            .get(FRP_PROFILE_TOKEN_SCOPE)
            .and_then(|tokens| tokens.get(id))
            .is_some_and(|current| current == token);
        Ok((
            profile,
            data.profiles.clone(),
            data.mcp_gateway.clone(),
            unchanged,
        ))
    })?;
    if unchanged {
        return DataStore::read_file(|data| Ok(frp_profile_dto(data, &profile)));
    }
    ensure_frp_profile_not_live(&profile, &workspaces, &gateway)?;
    DataStore::update_file(|data| {
        let profile = data
            .frp_profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("FRP profile not found: {id}")))?;
        data.app_secrets
            .entry(FRP_PROFILE_TOKEN_SCOPE.into())
            .or_default()
            .insert(id.to_string(), token.to_string());
        Ok(frp_profile_dto(data, &profile))
    })
}

pub(crate) fn clear_frp_profile_token(id: &str) -> AppResult<FrpProfileDto> {
    let (profile, workspaces, gateway, has_token) = DataStore::read_file(|data| {
        let profile = data
            .frp_profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("FRP profile not found: {id}")))?;
        let has_token = data
            .app_secrets
            .get(FRP_PROFILE_TOKEN_SCOPE)
            .and_then(|tokens| tokens.get(id))
            .is_some();
        Ok((
            profile,
            data.profiles.clone(),
            data.mcp_gateway.clone(),
            has_token,
        ))
    })?;
    if has_token {
        ensure_frp_profile_not_live(&profile, &workspaces, &gateway)?;
    }
    DataStore::update_file(|data| {
        let profile = data
            .frp_profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("FRP profile not found: {id}")))?;
        if let Some(tokens) = data.app_secrets.get_mut(FRP_PROFILE_TOKEN_SCOPE) {
            tokens.remove(id);
            if tokens.is_empty() {
                data.app_secrets.remove(FRP_PROFILE_TOKEN_SCOPE);
            }
        }
        Ok(frp_profile_dto(data, &profile))
    })
}

fn workspace_path(id: &str) -> AppResult<PathBuf> {
    workspace_profile(id).map(|profile| PathBuf::from(profile.path))
}

fn workspace_skill_catalog(id: &str, enabled: bool) -> AppResult<crate::skills::SkillCatalog> {
    let profile = workspace_profile(id)?;
    let catalog = crate::skills::SkillCatalog::new(PathBuf::from(profile.path));
    catalog.configure(crate::skills::SkillSettings::new(enabled));
    Ok(catalog)
}

pub(crate) fn inspect_workspace_skills(id: &str, enabled: bool) -> AppResult<serde_json::Value> {
    let catalog = workspace_skill_catalog(id, enabled)?;
    Ok(serde_json::json!({
        "catalog": catalog.list(None, 200),
        "packages": catalog.packages()
    }))
}

pub(crate) fn install_workspace_skill_package(
    id: &str,
    path: &str,
    channel: &str,
    activate: bool,
) -> AppResult<serde_json::Value> {
    let profile = workspace_profile(id)?;
    let catalog = workspace_skill_catalog(id, profile.runtime.skill_service_enabled)?;
    let channel = crate::skills::SkillChannel::parse(channel).map_err(AppError::Message)?;
    let package = catalog
        .install_package(path, channel, activate)
        .map_err(AppError::Message)?;
    Ok(serde_json::to_value(package)?)
}

pub(crate) fn set_workspace_skill_channel(
    id: &str,
    name: &str,
    channel: &str,
    version: &str,
) -> AppResult<serde_json::Value> {
    let profile = workspace_profile(id)?;
    let catalog = workspace_skill_catalog(id, profile.runtime.skill_service_enabled)?;
    let channel = crate::skills::SkillChannel::parse(channel).map_err(AppError::Message)?;
    Ok(serde_json::to_value(
        catalog
            .set_channel(name, channel, version)
            .map_err(AppError::Message)?,
    )?)
}

pub(crate) fn activate_workspace_skill_package(
    id: &str,
    name: &str,
    channel: &str,
) -> AppResult<serde_json::Value> {
    let profile = workspace_profile(id)?;
    let catalog = workspace_skill_catalog(id, profile.runtime.skill_service_enabled)?;
    let channel = crate::skills::SkillChannel::parse(channel).map_err(AppError::Message)?;
    Ok(serde_json::to_value(
        catalog
            .activate_package(name, channel)
            .map_err(AppError::Message)?,
    )?)
}

pub(crate) fn rollback_workspace_skill_package(
    id: &str,
    name: &str,
) -> AppResult<serde_json::Value> {
    let profile = workspace_profile(id)?;
    let catalog = workspace_skill_catalog(id, profile.runtime.skill_service_enabled)?;
    Ok(serde_json::to_value(
        catalog.rollback_package(name).map_err(AppError::Message)?,
    )?)
}

pub(crate) fn remove_workspace_skill_package(
    id: &str,
    name: &str,
    version: &str,
) -> AppResult<serde_json::Value> {
    let profile = workspace_profile(id)?;
    let catalog = workspace_skill_catalog(id, profile.runtime.skill_service_enabled)?;
    Ok(serde_json::to_value(
        catalog
            .remove_package(name, version)
            .map_err(AppError::Message)?,
    )?)
}

fn workspace_path_string(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    value.into_owned()
}

fn canonical_workspace_path(raw: &str) -> AppResult<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::Message("workspace path 不能为空".into()));
    }
    let path = PathBuf::from(trimmed);
    let canonical = path.canonicalize().map_err(|error| {
        AppError::Message(format!(
            "workspace 目录不存在或无法访问：{trimmed}（{error}）"
        ))
    })?;
    if !canonical.is_dir() {
        return Err(AppError::Message(format!(
            "workspace path 不是目录：{}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn normalize_workspace_path(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized.to_string()
    }
}

fn same_workspace_path(existing: &str, canonical: &Path) -> bool {
    Path::new(existing)
        .canonicalize()
        .map(|path| path == canonical)
        .unwrap_or_else(|_| {
            normalize_workspace_path(existing)
                == normalize_workspace_path(&canonical.to_string_lossy())
        })
}

fn next_available_workspace_port(
    preferred: u16,
    reserved: &std::collections::HashSet<u16>,
) -> AppResult<u16> {
    for port in preferred.max(1)..=u16::MAX {
        if reserved.contains(&port) {
            continue;
        }
        if platform().find_pid_listening_on_port(port)?.is_none() {
            return Ok(port);
        }
    }
    Err(AppError::Message(format!(
        "无法从端口 {preferred} 起找到可用端口"
    )))
}

fn assign_os_available_workspace_ports(
    profiles: &[WorkspaceProfile],
    profile: &mut WorkspaceProfile,
) -> AppResult<()> {
    let reserved = profiles
        .iter()
        .map(|item| item.runtime.local_port)
        .collect::<std::collections::HashSet<_>>();
    profile.runtime.local_port =
        next_available_workspace_port(profile.runtime.local_port, &reserved)?;
    Ok(())
}

pub(crate) fn register_workspace(
    path: &str,
    name: Option<String>,
) -> AppResult<(WorkspaceProfile, bool)> {
    let canonical = canonical_workspace_path(path)?;
    let canonical_text = workspace_path_string(&canonical);
    let mut store = DataStore::load()?;

    if let Some(existing) = store
        .list()
        .iter()
        .find(|profile| same_workspace_path(&profile.path, &canonical))
        .cloned()
    {
        return Ok((existing, false));
    }

    let requested_name = name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut profile = WorkspaceProfile::new(canonical_text, requested_name);
    if store
        .list()
        .iter()
        .any(|existing| existing.name.eq_ignore_ascii_case(&profile.name))
    {
        return Err(AppError::Message(format!(
            "workspace 名称已存在：{}；请指定唯一名称",
            profile.name
        )));
    }
    let gateway = store.settings().mcp_gateway;
    let reserved = if gateway.enabled {
        std::collections::HashSet::from([gateway.local_port])
    } else {
        std::collections::HashSet::new()
    };
    assign_free_workspace_ports_with_reserved(store.list(), &mut profile, &reserved)?;
    assign_os_available_workspace_ports(store.list(), &mut profile)?;
    store.register_workspace(profile.clone())?;
    Ok((profile, true))
}

pub(crate) fn create_workspace(path: String, name: Option<String>) -> AppResult<WorkspaceProfile> {
    register_workspace(&path, name).map(|(profile, _)| profile)
}

pub(crate) struct WorkspaceDeletion {
    pub profile: WorkspaceProfile,
    pub warnings: Vec<String>,
}

pub(crate) fn open_workspace_directory(path: &str) -> AppResult<()> {
    open_path_in_file_manager(&PathBuf::from(path.trim()))
}

pub(crate) async fn delete_workspace_with_timeout(
    id: &str,
    timeout: Duration,
) -> AppResult<WorkspaceDeletion> {
    let store = DataStore::load()?;
    crate::mcp::gateway::ensure_workspace_is_not_owner(
        &store.settings().mcp_gateway,
        store.list(),
        id,
    )?;
    let profile = store
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))?;
    drop(store);

    let gateway_inspection = crate::gateway_daemon::inspect()?;
    if gateway_inspection.ambiguous {
        return Err(AppError::Message(gateway_inspection.detail));
    }
    if gateway_inspection.running
        && gateway_inspection
            .state
            .as_ref()
            .is_some_and(|gateway_state| gateway_state.workspace_ids.iter().any(|item| item == id))
    {
        return Err(AppError::Message(
            "该 Workspace 正由 Gateway daemon 提供路由。请先关闭对应 Gateway route，再删除 Workspace。"
                .into(),
        ));
    }

    let inspection = crate::daemon::inspect(&profile)?;
    if inspection.running {
        control::request_daemon_exit_and_wait(
            &profile,
            control::ControlOperation::Shutdown,
            timeout,
            true,
        )
        .await?;
    } else if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }

    let mut warnings = Vec::new();
    if let Some(pid) = platform().find_pid_listening_on_port(profile.runtime.local_port)? {
        warnings.push(format!(
            "MCP 端口 {} 当前由外部 PID {pid} 监听；删除 Workspace 不会停止该进程",
            profile.runtime.local_port
        ));
    }

    drop_tunnel_workspace(&profile.id).await?;
    crate::daemon::cleanup(&profile)?;

    let mut store = DataStore::load()?;
    crate::mcp::gateway::ensure_workspace_is_not_owner(
        &store.settings().mcp_gateway,
        store.list(),
        id,
    )?;
    let removed = store
        .remove(id)?
        .ok_or_else(|| AppError::Message(format!("workspace 已不存在：{}", profile.id)))?;
    if removed.id == profile.id {
        crate::secret::SecretStore::clear_refresh_replay_state(id)?;
    }
    #[cfg(windows)]
    crate::windows_service::forget_workspace(id)?;
    #[cfg(target_os = "linux")]
    crate::linux_service::forget_workspace(id)?;
    Ok(WorkspaceDeletion {
        profile: removed,
        warnings,
    })
}

pub(crate) async fn delete_workspace(id: &str) -> AppResult<()> {
    delete_workspace_with_timeout(id, MANAGEMENT_DAEMON_TIMEOUT)
        .await
        .map(|_| ())
}

pub(crate) async fn run_health_checks(id: &str) -> AppResult<Vec<crate::health::HealthItem>> {
    crate::health::run_health_checks(&workspace_profile(id)?).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminTaskEntry {
    pub workspace_id: String,
    pub workspace_name: String,
    pub task: crate::tasks::TaskView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminTaskWorkspaceError {
    pub workspace_id: String,
    pub workspace_name: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminTaskList {
    pub tasks: Vec<AdminTaskEntry>,
    pub workspace_errors: Vec<AdminTaskWorkspaceError>,
    pub refreshed_at: String,
}

pub(crate) fn list_tasks() -> AppResult<AdminTaskList> {
    let profiles = list_workspaces()?;
    let mut tasks = Vec::new();
    let mut workspace_errors = Vec::new();
    for profile in profiles {
        match crate::tasks::list_workspace_tasks(Path::new(&profile.path)) {
            Ok(list) => tasks.extend(list.tasks.into_iter().map(|task| AdminTaskEntry {
                workspace_id: profile.id.clone(),
                workspace_name: profile.name.clone(),
                task,
            })),
            Err(error) => workspace_errors.push(AdminTaskWorkspaceError {
                workspace_id: profile.id,
                workspace_name: profile.name,
                message: crate::tasks::harness_error_message(error),
            }),
        }
    }
    Ok(AdminTaskList {
        tasks,
        workspace_errors,
        refreshed_at: chrono::Utc::now().to_rfc3339(),
    })
}

pub(crate) fn get_task_snapshot(id: &str, task_id: &str) -> AppResult<crate::tasks::TaskSnapshot> {
    crate::tasks::workspace_task_snapshot(&workspace_path(id)?, task_id)
        .map_err(|error| AppError::Message(crate::tasks::harness_error_message(error)))
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

pub(crate) fn software_target_version(kind: &str) -> AppResult<&'static str> {
    crate::tunnel::software_target_version(kind)
}

pub(crate) async fn install_software(kind: &str) -> AppResult<crate::tunnel::SoftwareStatus> {
    crate::tunnel::install_software(kind).await
}

pub(crate) fn uninstall_software(kind: &str) -> AppResult<crate::tunnel::SoftwareStatus> {
    crate::tunnel::uninstall_software(kind)
}

pub(crate) fn get_mcp_gateway() -> AppResult<crate::settings::McpGatewayConfig> {
    DataStore::read_file(|data| Ok(data.mcp_gateway.clone()))
}

pub(crate) fn list_tunnels() -> AppResult<Vec<TunnelProfile>> {
    Ok(DataStore::load()?.list_tunnels().to_vec())
}

pub(crate) fn create_tunnel(workspace_id: &str, name: Option<&str>) -> AppResult<TunnelProfile> {
    let mut store = DataStore::load()?;
    let workspace = store
        .get(workspace_id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("workspace not found: {workspace_id}")))?;
    if store.tunnel_for_workspace(workspace_id).is_some() {
        return Err(AppError::Message(format!(
            "workspace {} 已存在 MCP Tunnel；请编辑现有 Tunnel",
            workspace.name
        )));
    }
    let mut tunnel = TunnelProfile::new(workspace.id, &workspace.name, "mcp");
    if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
        tunnel.name = name.to_string();
    }
    store.register_tunnel(tunnel.clone())?;
    Ok(tunnel)
}

pub(crate) async fn update_tunnel(mut tunnel: TunnelProfile) -> AppResult<TunnelProfile> {
    tunnel.name = tunnel.name.trim().to_string();
    tunnel.workspace_id = tunnel.workspace_id.trim().to_string();
    tunnel.service = tunnel.service.trim().to_ascii_lowercase();
    tunnel.config.public_url = tunnel
        .config
        .public_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    if tunnel.name.is_empty() {
        return Err(AppError::Message("Tunnel 名称不能为空".into()));
    }
    let store = DataStore::load()?;
    let current = store
        .get_tunnel(&tunnel.id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("tunnel not found: {}", tunnel.id)))?;
    if tunnel.workspace_id != current.workspace_id || tunnel.service != current.service {
        return Err(AppError::Message(
            "Tunnel target is immutable; delete and recreate the Tunnel to retarget it".into(),
        ));
    }
    let settings = store.settings();
    let mut runtime_profile = store.get(&current.workspace_id).cloned().ok_or_else(|| {
        AppError::Message(format!("workspace not found: {}", current.workspace_id))
    })?;
    runtime_profile.tunnel = current.config.clone();
    runtime_profile.tunnel_id = current.id.clone();
    runtime_profile.tunnel_enabled = current.enabled;
    runtime_profile.tunnel_revision = current.revision;
    drop(store);

    let runtime_changed = current.config != tunnel.config;
    tunnel.revision = if runtime_changed {
        current.revision.saturating_add(1)
    } else {
        current.revision
    };
    let gateway_selected =
        settings.mcp_gateway.enabled && settings.mcp_gateway.tunnel_id == tunnel.id;
    let direct_daemon_running =
        !settings.mcp_gateway.enabled && crate::daemon::inspect(&runtime_profile)?.running;
    let direct_running = if direct_daemon_running {
        daemon_tunnel_status(&runtime_profile, TunnelServiceKind::Mcp)
            .await
            .is_ok_and(|status| status.state == "running")
    } else {
        false
    };

    let mut store = DataStore::load()?;
    store.update_tunnel(tunnel.clone())?;
    drop(store);

    let runtime_result = if gateway_selected && runtime_changed {
        set_mcp_gateway(settings.mcp_gateway.clone())
            .await
            .map(|_| ())
    } else if direct_running && (!tunnel.enabled || tunnel.config.tunnel_type == "none") {
        stop_tunnel(&tunnel.id).await.map(|_| ())
    } else if direct_daemon_running
        && tunnel.enabled
        && tunnel.config.tunnel_type != "none"
        && (runtime_changed || (!current.enabled && tunnel.enabled))
    {
        async {
            if direct_running {
                stop_tunnel(&tunnel.id).await?;
            }
            start_tunnel(&tunnel.id).await?;
            Ok(())
        }
        .await
    } else {
        Ok(())
    };
    if let Err(error) = runtime_result {
        let mut store = DataStore::load()?;
        store.update_tunnel(current.clone())?;
        drop(store);
        if gateway_selected {
            let _ = set_mcp_gateway(settings.mcp_gateway).await;
        } else if direct_running {
            let _ = start_tunnel(&current.id).await;
        }
        return Err(error);
    }
    Ok(tunnel)
}

pub(crate) async fn delete_tunnel(id: &str) -> AppResult<()> {
    let (tunnel, profile, settings) = load_tunnel_target_unchecked(id)?;
    if settings.mcp_gateway.enabled && settings.mcp_gateway.tunnel_id == id {
        return Err(AppError::Message(
            "Tunnel is used by the enabled MCP Gateway; select another Tunnel or disable Gateway first"
                .into(),
        ));
    }
    if !settings.mcp_gateway.enabled && crate::daemon::inspect(&profile)?.running {
        let _ = control::request_tunnel_operation(
            &profile,
            TunnelServiceKind::parse(&tunnel.service)?,
            control::ControlTunnelAction::Stop,
            MANAGEMENT_TUNNEL_TIMEOUT,
        )
        .await;
    }
    let mut store = DataStore::load()?;
    if store.remove_tunnel(id)?.is_none() {
        return Err(AppError::Message(format!("tunnel not found: {id}")));
    }
    Ok(())
}

fn tunnel_secret_scope(key: &str) -> AppResult<&'static str> {
    match key {
        "frp_token" => Ok("tunnel_frp_token"),
        "cloudflare_token" => Ok("tunnel_cloudflare_token"),
        _ => Err(AppError::Message(format!(
            "invalid tunnel secret key: {key}"
        ))),
    }
}

pub(crate) fn get_tunnel_secret(id: &str, key: &str) -> AppResult<Option<String>> {
    let scope = tunnel_secret_scope(key)?;
    DataStore::read_file(|data| {
        if !data.tunnels.iter().any(|tunnel| tunnel.id == id) {
            return Err(AppError::Message(format!("tunnel not found: {id}")));
        }
        Ok(data
            .app_secrets
            .get(scope)
            .and_then(|items| items.get(id))
            .filter(|value| !value.is_empty())
            .cloned())
    })
}

pub(crate) async fn set_tunnel_secret(id: &str, key: &str, value: &str) -> AppResult<()> {
    let scope = tunnel_secret_scope(key)?;
    let (tunnel, profile, settings) = load_tunnel_target_unchecked(id)?;
    let previous = get_tunnel_secret(id, key)?.unwrap_or_default();
    let next = value.trim().to_string();
    if previous == next {
        return Ok(());
    }
    let direct_daemon_running =
        !settings.mcp_gateway.enabled && crate::daemon::inspect(&profile)?.running;
    let direct_running = if direct_daemon_running {
        daemon_tunnel_status(&profile, TunnelServiceKind::parse(&tunnel.service)?)
            .await
            .is_ok_and(|status| status.state == "running")
    } else {
        false
    };
    DataStore::update_file(|data| {
        let tunnel = data
            .tunnels
            .iter_mut()
            .find(|tunnel| tunnel.id == id)
            .ok_or_else(|| AppError::Message(format!("tunnel not found: {id}")))?;
        tunnel.revision = tunnel.revision.saturating_add(1);
        let items = data.app_secrets.entry(scope.to_string()).or_default();
        if next.is_empty() {
            items.remove(id);
        } else {
            items.insert(id.to_string(), next.clone());
        }
        Ok(())
    })?;

    let gateway_selected = settings.mcp_gateway.enabled && settings.mcp_gateway.tunnel_id == id;
    let runtime_result = if gateway_selected {
        set_mcp_gateway(settings.mcp_gateway.clone())
            .await
            .map(|_| ())
    } else if direct_running
        || (direct_daemon_running && tunnel.enabled && tunnel.config.tunnel_type != "none")
    {
        async {
            if direct_running {
                stop_tunnel(id).await?;
            }
            start_tunnel(id).await?;
            Ok(())
        }
        .await
    } else {
        Ok(())
    };
    if let Err(error) = runtime_result {
        DataStore::update_file(|data| {
            let current = data
                .tunnels
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or_else(|| AppError::Message(format!("tunnel not found: {id}")))?;
            current.revision = tunnel.revision;
            let items = data.app_secrets.entry(scope.to_string()).or_default();
            if previous.is_empty() {
                items.remove(id);
            } else {
                items.insert(id.to_string(), previous.clone());
            }
            Ok(())
        })?;
        if gateway_selected {
            let _ = set_mcp_gateway(settings.mcp_gateway).await;
        } else if direct_running {
            let _ = start_tunnel(id).await;
        }
        return Err(error);
    }
    Ok(())
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

    if previous.identity_changed(&config) {
        config.clear_observation();
    } else {
        config.observed_public_url = previous.observed_public_url.clone();
        config.observed_tunnel_id = previous.observed_tunnel_id.clone();
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
    #[cfg(target_os = "linux")]
    if !enabled {
        crate::linux_service::set_gateway_desired(&[])?;
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

fn workspace_service_for_tunnel(kind: TunnelServiceKind) -> WorkspaceService {
    match kind {
        TunnelServiceKind::Mcp => WorkspaceService::Mcp,
    }
}

fn configured_tunnel_status(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<TunnelStatus> {
    let public_url = match kind {
        TunnelServiceKind::Mcp => profile.effective_public_url()?,
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
        let Some(tunnel) = data.tunnels.iter_mut().find(|tunnel| tunnel.id == id) else {
            return Ok(());
        };
        match kind {
            TunnelServiceKind::Mcp => tunnel.config.public_url = public_url.to_string(),
        }
        Ok(())
    })?;
    let (workspace_id, service) = DataStore::read_file(|data| {
        let tunnel = data
            .tunnels
            .iter()
            .find(|tunnel| tunnel.id == id)
            .ok_or_else(|| AppError::Message(format!("tunnel not found: {id}")))?;
        Ok((tunnel.workspace_id.clone(), tunnel.service.clone()))
    })?;
    crate::runtime::update_public_url(&workspace_id, &service, public_url);
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
    }
    .ok_or_else(|| AppError::Message("daemon control status omitted tunnel state".into()))
}

fn load_tunnel_target_unchecked(
    id: &str,
) -> AppResult<(TunnelProfile, WorkspaceProfile, AppSettings)> {
    let store = DataStore::load()?;
    let tunnel = store
        .get_tunnel(id)
        .cloned()
        .ok_or_else(|| AppError::Message(format!("tunnel not found: {id}")))?;
    let kind = TunnelServiceKind::parse(&tunnel.service)?;
    let service = workspace_service_for_tunnel(kind);
    validate_service_start(store.list(), &tunnel.workspace_id, service)?;
    let mut profile = store.get(&tunnel.workspace_id).cloned().ok_or_else(|| {
        AppError::Message(format!("workspace not found: {}", tunnel.workspace_id))
    })?;
    let settings = store.settings();
    profile.tunnel = tunnel.config.clone();
    profile.tunnel_id = tunnel.id.clone();
    profile.tunnel_enabled = tunnel.enabled;
    profile.tunnel_revision = tunnel.revision;
    Ok((tunnel, profile, settings))
}

fn load_tunnel_target(id: &str) -> AppResult<(TunnelProfile, WorkspaceProfile, AppSettings)> {
    let (tunnel, profile, settings) = load_tunnel_target_unchecked(id)?;
    reject_gateway_managed_mcp(
        &settings,
        workspace_service_for_tunnel(TunnelServiceKind::parse(&tunnel.service)?),
    )?;
    Ok((tunnel, profile, settings))
}

fn tunnel_is_configured(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> bool {
    match kind {
        TunnelServiceKind::Mcp => profile.tunnel.tunnel_type != "none",
    }
}

pub(crate) async fn start_tunnel(id: &str) -> AppResult<TunnelStatus> {
    let (tunnel, profile, _) = load_tunnel_target(id)?;
    let kind = TunnelServiceKind::parse(&tunnel.service)?;
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

pub(crate) async fn stop_tunnel(id: &str) -> AppResult<TunnelStatus> {
    let (tunnel, profile, _) = load_tunnel_target(id)?;
    let kind = TunnelServiceKind::parse(&tunnel.service)?;
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
    .map_err(|error| AppError::Message(format!("daemon 隧道停止失败：{error}")))?;
    configured_tunnel_status(&profile, kind)
}

pub(crate) async fn get_tunnel_status(id: &str) -> AppResult<TunnelStatus> {
    let (tunnel, profile, settings) = load_tunnel_target_unchecked(id)?;
    let kind = TunnelServiceKind::parse(&tunnel.service)?;
    if settings.mcp_gateway.enabled {
        if settings.mcp_gateway.tunnel_id == id {
            let status = get_mcp_gateway_status().await?;
            return Ok(TunnelStatus {
                state: if status.running {
                    "running".into()
                } else {
                    status.state
                },
                public_url: status.public_base_url,
                tunnel_pid: None,
            });
        }
        return configured_tunnel_status(&profile, kind);
    }
    daemon_tunnel_status(&profile, kind).await
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

pub(crate) async fn test_tunnel(id: &str) -> AppResult<TunnelTestResult> {
    let (tunnel, profile, _) = load_tunnel_target(id)?;
    let kind = TunnelServiceKind::parse(&tunnel.service)?;
    let target_service = workspace_service_for_tunnel(kind);
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowsServicePrivilegedTarget {
    pub service_name: String,
    pub revision: String,
}

pub(crate) fn windows_service_privileged_target(
    action: &str,
) -> AppResult<WindowsServicePrivilegedTarget> {
    #[cfg(windows)]
    {
        let target = crate::windows_service::privileged_action_target(action)?;
        Ok(WindowsServicePrivilegedTarget {
            service_name: target.service_name,
            revision: target.revision,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = action;
        Err(AppError::Message(
            "Windows SCM Service 仅支持 Windows".into(),
        ))
    }
}

#[cfg(windows)]
async fn run_windows_service_elevated(action: &'static str) -> AppResult<serde_json::Value> {
    tokio::task::spawn_blocking(move || crate::windows_service::run_elevated_admin_action(action))
        .await
        .map_err(|error| AppError::Message(format!("Windows UAC helper task failed: {error}")))??;
    Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
}

pub(crate) async fn install_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_windows_service_elevated("install").await
    }
    #[cfg(not(windows))]
    Err(AppError::Message(
        "Windows SCM Service 仅支持 Windows".into(),
    ))
}

pub(crate) async fn uninstall_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_windows_service_elevated("uninstall").await
    }
    #[cfg(not(windows))]
    Err(AppError::Message(
        "Windows SCM Service 仅支持 Windows".into(),
    ))
}

pub(crate) async fn start_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_windows_service_elevated("start").await
    }
    #[cfg(not(windows))]
    Err(AppError::Message(
        "Windows SCM Service 仅支持 Windows".into(),
    ))
}

pub(crate) async fn stop_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_windows_service_elevated("stop").await
    }
    #[cfg(not(windows))]
    Err(AppError::Message(
        "Windows SCM Service 仅支持 Windows".into(),
    ))
}

pub(crate) async fn restart_windows_service() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        run_windows_service_elevated("restart").await
    }
    #[cfg(not(windows))]
    Err(AppError::Message(
        "Windows SCM Service 仅支持 Windows".into(),
    ))
}

pub(crate) fn sync_windows_service_plan() -> AppResult<serde_json::Value> {
    #[cfg(windows)]
    {
        let _ = crate::windows_service::sync_plan_from_running()?;
        Ok(serde_json::to_value(crate::windows_service::scm_status()?)?)
    }
    #[cfg(not(windows))]
    Err(AppError::Message(
        "Windows SCM Service 仅支持 Windows".into(),
    ))
}

fn control_log_service(service: &str) -> AppResult<ControlLogSelection> {
    Ok(match service {
        "mcp" => ControlLogSelection::Mcp,
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
];

const SHARED_SECRET_KEYS: &[&str] = &[
    "oauth_client_id",
    "bearer_token",
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
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

pub(crate) fn federation_peer_credential_status(
    target: &crate::federation::FederationRemoteTarget,
) -> AppResult<crate::federation::FederationPeerCredentialStatus> {
    crate::federation::require_registered_target(target)?;
    crate::federation::peer_credential_status(target)
}

pub(crate) fn set_federation_peer_credential(
    target: &crate::federation::FederationRemoteTarget,
    token: &str,
) -> AppResult<crate::federation::FederationPeerCredentialStatus> {
    let record = crate::federation::require_registered_target(target)?;
    Ok(crate::federation::rotate_peer_credential(&record.node_id, token)?.credential)
}

pub(crate) fn clear_federation_peer_credential(
    target: &crate::federation::FederationRemoteTarget,
) -> AppResult<crate::federation::FederationPeerCredentialStatus> {
    let record = crate::federation::require_registered_target(target)?;
    Ok(crate::federation::revoke_peer(&record.node_id)?.credential)
}

pub(crate) async fn probe_federation_peer(
    target: &crate::federation::FederationRemoteTarget,
) -> AppResult<crate::federation::FederationPeerView> {
    let record = crate::federation::require_registered_target(target)?;
    crate::federation::probe_registered_peer(&record.node_id).await
}

pub(crate) async fn read_federation_remote(
    target: &crate::federation::FederationRemoteTarget,
    request: &crate::federation::FederationReadRequest,
) -> AppResult<crate::federation::FederationReadResult> {
    crate::federation::read_trusted_peer(target, request).await
}

pub(crate) fn plan_orchestration_workflow(
    workflow: &crate::orchestration::OrchestrationWorkflowSpec,
) -> AppResult<crate::orchestration::OrchestrationPlan> {
    crate::orchestration::plan_workflow(workflow)
}

pub(crate) async fn inspect_orchestration_workflow(
    workflow: &crate::orchestration::OrchestrationWorkflowSpec,
) -> AppResult<crate::orchestration::OrchestrationInspection> {
    crate::orchestration::inspect_workflow(workflow).await
}

pub(crate) fn list_federation_peers() -> AppResult<Vec<crate::federation::FederationPeerView>> {
    crate::federation::list_peers()
}

pub(crate) fn get_federation_peer(
    node_id: &str,
) -> AppResult<crate::federation::FederationPeerView> {
    crate::federation::get_peer(node_id)
}

pub(crate) fn register_federation_peer(
    input: &crate::federation::FederationPeerRegistration,
) -> AppResult<crate::federation::FederationPeerView> {
    crate::federation::register_peer(input)
}

pub(crate) fn update_federation_peer(
    input: &crate::federation::FederationPeerUpdate,
) -> AppResult<crate::federation::FederationPeerView> {
    crate::federation::update_peer(input)
}

pub(crate) fn trust_federation_peer(
    node_id: &str,
) -> AppResult<crate::federation::FederationPeerView> {
    crate::federation::trust_peer(node_id)
}

pub(crate) fn remove_federation_peer(node_id: &str) -> AppResult<()> {
    crate::federation::remove_peer(node_id)
}

pub(crate) fn revoke_federation_peer(
    node_id: &str,
) -> AppResult<crate::federation::FederationPeerView> {
    crate::federation::revoke_peer(node_id)
}

pub(crate) fn inspect_federation_candidate(
    endpoint: &str,
    display_name: &str,
    bundle: &crate::federation::FederationBootstrapBundle,
) -> AppResult<crate::federation::FederationPeerCandidate> {
    crate::federation::inspect_candidate(endpoint, display_name, bundle)
}

fn generated_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).replace('-', "")
}

const MCP_SHARED_SECRET_KEYS: &[&str] = &[
    "oauth_client_id",
    "bearer_token",
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
];

fn schedule_secret_restart(profiles: Vec<WorkspaceProfile>, key: String, shared: bool) {
    crate::async_runtime::spawn(async move {
        for profile in &profiles {
            restart_running_service_after_secret_change(profile, &key, shared).await;
        }
    });
}

async fn restart_running_service_after_secret_change(
    profile: &WorkspaceProfile,
    key: &str,
    shared: bool,
) {
    let mcp_relevant =
        MCP_SHARED_SECRET_KEYS.contains(&key) && profile.auth.use_shared_secrets == shared;
    match crate::daemon::inspect(profile) {
        Ok(inspection) if inspection.running => {
            let Some(daemon_state) = inspection.state else {
                return;
            };
            let service = if mcp_relevant && daemon_state.service.includes_mcp() {
                Some(WorkspaceService::Mcp)
            } else {
                None
            };
            if let Some(service) = service {
                if let Err(error) = crate::control::restart_daemon_service(
                    profile,
                    service,
                    daemon_state.managed_tunnels(),
                    MANAGEMENT_DAEMON_TIMEOUT,
                    true,
                )
                .await
                {
                    eprintln!(
                        "daemon restart after secret mutation failed for {}: {error}",
                        profile.id
                    );
                }
            }
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!(
                "daemon inspection after secret mutation failed for {}: {error}",
                profile.id
            );
        }
    }
}

pub(crate) fn set_workspace_secret(id: &str, key: &str, value: &str) -> AppResult<()> {
    validate_workspace_secret_key(key)?;
    DataStore::update_file(|data| {
        if !data.profiles.iter().any(|profile| profile.id == id) {
            return Err(AppError::Message(format!("workspace not found: {id}")));
        }
        data.workspace_secrets
            .entry(id.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        Ok(())
    })
}

pub(crate) fn regenerate_workspace_secret(id: &str, key: &str) -> AppResult<String> {
    validate_workspace_secret_key(key)?;
    let value = generated_secret();
    let profile = DataStore::update_file(|data| {
        let profile = data
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))?;
        data.workspace_secrets
            .entry(id.to_string())
            .or_default()
            .insert(key.to_string(), value.clone());
        Ok(profile)
    })?;
    schedule_secret_restart(vec![profile], key.to_string(), false);
    Ok(value)
}

pub(crate) fn set_shared_secret(key: &str, value: &str) -> AppResult<()> {
    validate_shared_secret_key(key)?;
    if value.is_empty() {
        return Err(AppError::Message("密钥不能为空。".into()));
    }
    let profiles = DataStore::update_file(|data| {
        if data
            .shared_secrets
            .get(key)
            .is_some_and(|current| current == value)
        {
            return Ok(None);
        }
        data.shared_secrets
            .insert(key.to_string(), value.to_string());
        Ok(Some(data.profiles.clone()))
    })?;
    if let Some(profiles) = profiles {
        schedule_secret_restart(profiles, key.to_string(), true);
    }
    Ok(())
}

pub(crate) fn regenerate_shared_secret(key: &str) -> AppResult<String> {
    validate_shared_secret_key(key)?;
    let value = generated_secret();
    set_shared_secret(key, &value)?;
    Ok(value)
}

pub(crate) fn list_frp_profiles() -> AppResult<Vec<FrpProfileDto>> {
    DataStore::read_file(|data| {
        Ok(data
            .frp_profiles
            .iter()
            .map(|profile| frp_profile_dto(data, profile))
            .collect())
    })
}

fn frp_profile_dto(data: &crate::data::AppData, profile: &FrpProfile) -> FrpProfileDto {
    let has_token = data
        .app_secrets
        .get(FRP_PROFILE_TOKEN_SCOPE)
        .and_then(|tokens| tokens.get(&profile.id))
        .is_some_and(|value| !value.trim().is_empty());
    FrpProfileDto {
        id: profile.id.clone(),
        name: profile.name.clone(),
        server: profile.server.clone(),
        server_port: profile.server_port,
        has_token,
    }
}

fn normalize_frp_required(label: &str, value: &str) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::Message(format!("{label} 不能为空")));
    }
    Ok(value.to_string())
}

fn normalize_frp_server(value: &str) -> AppResult<String> {
    let value = normalize_frp_required("FRP server", value)?;
    if value.contains("//") || value.contains('/') || value.contains(char::is_whitespace) {
        return Err(AppError::Message(
            "FRP server 只接受主机名或 IP，不要包含协议、路径或空白字符".into(),
        ));
    }
    Ok(value.trim_end_matches('.').to_string())
}

fn ensure_unique_frp_name(profiles: &[FrpProfile], name: &str, except_id: &str) -> AppResult<()> {
    if profiles
        .iter()
        .any(|profile| profile.id != except_id && profile.name.trim().eq_ignore_ascii_case(name))
    {
        return Err(AppError::Message(format!("FRP profile 名称已存在：{name}")));
    }
    Ok(())
}

fn frp_profile_references(data: &crate::data::AppData, id: &str) -> Vec<String> {
    let mut references = data
        .profiles
        .iter()
        .filter(|workspace| workspace.tunnel.frp_profile_id == id)
        .map(|workspace| format!("{}:mcp", workspace.name))
        .collect::<Vec<_>>();
    references.sort();
    references.dedup();
    references
}

fn all_frp_profile_references(data: &crate::data::AppData, id: &str) -> AppResult<Vec<String>> {
    let mut references = frp_profile_references(data, id);
    #[cfg(feature = "cli")]
    for workspace in &data.profiles {
        let Some(pending) = crate::cli::pending_profile_candidate(workspace)? else {
            continue;
        };
        if pending.tunnel.frp_profile_id == id && workspace.tunnel.frp_profile_id != id {
            references.push(format!("{}:mcp(pending)", workspace.name));
        }
    }
    references.sort();
    references.dedup();
    Ok(references)
}

fn ensure_frp_profile_not_live(
    profile: &FrpProfile,
    workspaces: &[WorkspaceProfile],
    gateway: &McpGatewayConfig,
) -> AppResult<()> {
    let mut live = Vec::new();
    for workspace in workspaces {
        let inspection = crate::daemon::inspect(workspace)?;
        if inspection.ambiguous {
            return Err(AppError::Message(format!(
                "无法安全更新 FRP profile：workspace {} daemon 状态不明确：{}",
                workspace.name, inspection.detail
            )));
        }
        if inspection.running && inspection.pid_matches {
            let managed = inspection
                .state
                .as_ref()
                .and_then(|state| state.managed_tunnels());
            if workspace.tunnel.frp_profile_id == profile.id
                && managed.is_some_and(|selection| selection.includes_mcp())
            {
                live.push(format!("{}:mcp", workspace.name));
            }
        }
    }

    if gateway.enabled && !gateway.tunnel_id.trim().is_empty() {
        if let Some(owner) = workspaces.iter().find(|workspace| {
            workspace.tunnel_id == gateway.tunnel_id
                && workspace.tunnel.frp_profile_id == profile.id
        }) {
            let inspection = crate::gateway_daemon::inspect()?;
            if inspection.ambiguous {
                return Err(AppError::Message(format!(
                    "无法安全更新 FRP profile：Gateway daemon 状态不明确：{}",
                    inspection.detail
                )));
            }
            if inspection.running && inspection.pid_matches {
                live.push(format!("{}:gateway-mcp", owner.name));
            }
        }
    }

    if live.is_empty() {
        return Ok(());
    }
    live.sort();
    live.dedup();
    Err(AppError::Message(format!(
        "FRP profile {} 正被运行中的受管 tunnel 使用：{}。为避免磁盘配置与活动 frpc 分叉，请先停止对应 tunnel/daemon，修改 profile 后再启动。仅修改 profile 名称不受此限制。",
        profile.name,
        live.join(", ")
    )))
}

pub(crate) fn save_frp_profile_metadata(profile: FrpProfileInput) -> AppResult<FrpProfileDto> {
    let mut saved = FrpProfile::from(profile);
    saved.name = normalize_frp_required("FRP profile name", &saved.name)?;
    saved.server = normalize_frp_server(&saved.server)?;
    if saved.server_port == 0 {
        return Err(AppError::Message("FRP 服务器端口必须大于 0".into()));
    }
    if saved.id.trim().is_empty() {
        saved.id = uuid::Uuid::new_v4().to_string().replace('-', "");
    }

    let (existing, workspaces, gateway) = DataStore::read_file(|data| {
        Ok((
            data.frp_profiles
                .iter()
                .find(|profile| profile.id == saved.id)
                .cloned(),
            data.profiles.clone(),
            data.mcp_gateway.clone(),
        ))
    })?;
    if let Some(existing) = existing.as_ref() {
        if existing.server != saved.server || existing.server_port != saved.server_port {
            ensure_frp_profile_not_live(existing, &workspaces, &gateway)?;
        }
    }

    DataStore::update_file(|data| {
        ensure_unique_frp_name(&data.frp_profiles, &saved.name, &saved.id)?;
        if let Some(index) = data
            .frp_profiles
            .iter()
            .position(|item| item.id == saved.id)
        {
            data.frp_profiles[index] = saved.clone();
        } else {
            data.frp_profiles.push(saved.clone());
        }
        Ok(frp_profile_dto(data, &saved))
    })
}

pub(crate) fn delete_frp_profile(id: &str) -> AppResult<()> {
    DataStore::update_file(|data| {
        let index = data
            .frp_profiles
            .iter()
            .position(|profile| profile.id == id)
            .ok_or_else(|| AppError::Message(format!("FRP profile not found: {id}")))?;
        let references = all_frp_profile_references(data, id)?;
        if !references.is_empty() {
            return Err(AppError::Message(format!(
                "FRP profile {} 仍被以下 tunnel 使用：{}。请先解除引用后再删除。",
                data.frp_profiles[index].name,
                references.join(", ")
            )));
        }
        data.frp_profiles.remove(index);
        if let Some(tokens) = data.app_secrets.get_mut(FRP_PROFILE_TOKEN_SCOPE) {
            tokens.remove(id);
            if tokens.is_empty() {
                data.app_secrets.remove(FRP_PROFILE_TOKEN_SCOPE);
            }
        }
        Ok(())
    })
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

    #[test]
    fn frp_server_normalization_rejects_urls_paths_and_whitespace() {
        assert_eq!(
            normalize_frp_server("frp.example.com.").expect("host"),
            "frp.example.com"
        );
        assert!(normalize_frp_server("https://frp.example.com").is_err());
        assert!(normalize_frp_server("frp.example.com/path").is_err());
        assert!(normalize_frp_server("frp example.com").is_err());
    }
}
