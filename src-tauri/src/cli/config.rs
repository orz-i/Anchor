use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::validate_redirect_policy;
use crate::control::{self, ControlConfigApplyResult};
use crate::daemon;
use crate::data::{validate_workspace_profile, DataStore};
use crate::error::{AppError, AppResult};
use crate::gateway_control;
use crate::gateway_daemon;
use crate::mcp::gateway;
use crate::platform::platform;
use crate::workspace::config_apply::{plan_workspace_config_apply, WorkspaceConfigApplyPlan};
use crate::workspace::resources::validate_workspace_resources_update;
use crate::workspace::WorkspaceProfile;

use super::args::{
    ConfigApplyOptions, ConfigAssignment, ConfigCommand, ConfigGetOptions, ConfigMutationOptions,
};

const PENDING_SCHEMA_VERSION: u32 = 1;
const MAX_PENDING_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingWorkspaceConfig {
    schema_version: u32,
    workspace_id: String,
    base: WorkspaceProfile,
    candidate: WorkspaceProfile,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigChange {
    path: String,
    before: Value,
    after: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigViewReport {
    event: &'static str,
    workspace_id: String,
    source: &'static str,
    key: Option<String>,
    value: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigDiffReport {
    event: &'static str,
    workspace_id: String,
    staged: bool,
    changes: Vec<ConfigChange>,
    apply_plan: WorkspaceConfigApplyPlan,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigSetReport {
    event: &'static str,
    workspace_id: String,
    staged: bool,
    changes: Vec<ConfigChange>,
    apply_plan: WorkspaceConfigApplyPlan,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigApplyReport {
    event: &'static str,
    workspace_id: String,
    applied: bool,
    changes: Vec<ConfigChange>,
    apply_plan: WorkspaceConfigApplyPlan,
    workspace_runtime: Option<ControlConfigApplyResult>,
    gateway_reloaded: bool,
    warnings: Vec<String>,
}

pub async fn execute(command: ConfigCommand, as_json: bool) -> AppResult<i32> {
    match command {
        ConfigCommand::Get(options) => get_config(options, as_json).map(|_| 0),
        ConfigCommand::Diff(options) => diff_config(options, as_json).map(|_| 0),
        ConfigCommand::Set(options) => set_config(options, as_json).map(|_| 0),
        ConfigCommand::Apply(options) => apply_config(options, as_json).await.map(|_| 0),
    }
}

fn get_config(options: ConfigGetOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let active = super::resolve_workspace(store.list(), &options.workspace)?.clone();
    let (source, profile) = if options.pending {
        let pending = load_pending_for_active(&active)?.ok_or_else(|| {
            AppError::Message(format!("workspace {} 当前没有待应用配置", active.name))
        })?;
        ("pending", pending.candidate)
    } else {
        ("active", active.clone())
    };
    let root = serde_json::to_value(&profile)?;
    let value = match options.key.as_deref() {
        Some(path) => value_at_path(&root, path)?.clone(),
        None => root,
    };
    let report = ConfigViewReport {
        event: "config_get",
        workspace_id: active.id,
        source,
        key: options.key,
        value,
    };
    print_report(&report, as_json)
}

fn diff_config(options: ConfigMutationOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let active = super::resolve_workspace(store.list(), &options.workspace)?.clone();
    let staged = load_pending_for_active(&active)?;
    let mut candidate = staged
        .as_ref()
        .map(|pending| pending.candidate.clone())
        .unwrap_or_else(|| active.clone());
    candidate = apply_assignments(&candidate, &options.assignments)?;
    validate_candidate(&store, &active, &candidate)?;
    let report = diff_report("config_diff", &active, &candidate, staged.is_some());
    print_report(&report, as_json)
}

fn set_config(options: ConfigMutationOptions, as_json: bool) -> AppResult<()> {
    let output = stage_config(options)?;
    print_report(&output, as_json)
}

pub(crate) fn stage_config(options: ConfigMutationOptions) -> AppResult<ConfigSetReport> {
    let (active, candidate) = preview_config(&options)?;
    let has_staged_changes = !profiles_equal(&candidate, &active)?;
    let report = diff_report("config_set", &active, &candidate, has_staged_changes);
    if !has_staged_changes {
        remove_pending(&active.id)?;
    } else {
        write_pending(&PendingWorkspaceConfig {
            schema_version: PENDING_SCHEMA_VERSION,
            workspace_id: active.id.clone(),
            base: active.clone(),
            candidate: candidate.clone(),
        })?;
    }
    let output = ConfigSetReport {
        event: "config_set",
        workspace_id: active.id,
        staged: has_staged_changes,
        changes: report.changes,
        apply_plan: report.apply_plan,
    };
    Ok(output)
}

pub(crate) fn preview_config(
    options: &ConfigMutationOptions,
) -> AppResult<(WorkspaceProfile, WorkspaceProfile)> {
    let store = DataStore::load()?;
    let active = super::resolve_workspace(store.list(), &options.workspace)?.clone();
    let staged = load_pending_for_active(&active)?;
    let starting = staged
        .as_ref()
        .map(|pending| pending.candidate.clone())
        .unwrap_or_else(|| active.clone());
    let candidate = apply_assignments(&starting, &options.assignments)?;
    validate_candidate(&store, &active, &candidate)?;
    Ok((active, candidate))
}

pub(crate) fn pending_candidate(active: &WorkspaceProfile) -> AppResult<Option<WorkspaceProfile>> {
    Ok(load_pending_for_active(active)?.map(|pending| pending.candidate))
}

async fn apply_config(options: ConfigApplyOptions, as_json: bool) -> AppResult<()> {
    let output = apply_staged_config(options).await?;
    print_report(&output, as_json)
}

pub(crate) async fn apply_staged_config(
    options: ConfigApplyOptions,
) -> AppResult<ConfigApplyReport> {
    let mut store = DataStore::load()?;
    let active = super::resolve_workspace(store.list(), &options.workspace)?.clone();
    let Some(pending) = load_pending_for_active(&active)? else {
        return Ok(ConfigApplyReport {
            event: "config_apply",
            workspace_id: active.id.clone(),
            applied: false,
            changes: Vec::new(),
            apply_plan: plan_workspace_config_apply(&active, &active),
            workspace_runtime: None,
            gateway_reloaded: false,
            warnings: vec!["没有待应用配置".into()],
        });
    };
    let candidate = pending.candidate;
    validate_candidate(&store, &active, &candidate)?;
    let plan = plan_workspace_config_apply(&active, &candidate);
    let changes = config_changes(&active, &candidate)?;
    let previous_settings = store.settings();
    let daemon_inspection = daemon::inspect(&active)?;
    if daemon_inspection.ambiguous {
        return Err(AppError::Message(daemon_inspection.detail));
    }
    let daemon_running = daemon_inspection.running && daemon_inspection.pid_matches;
    reject_uncoordinated_live_runtime(&active, &plan, &previous_settings, daemon_running)?;
    let gateway_inspection = gateway_daemon::inspect()?;
    if gateway_inspection.ambiguous {
        return Err(AppError::Message(gateway_inspection.detail));
    }
    let gateway_live = gateway_inspection.running
        && gateway_inspection.state.as_ref().is_some_and(|state| {
            state.workspace_ids.contains(&active.id)
                || previous_settings.mcp_gateway.owner_workspace_id == active.id
        });

    let reset_gateway_observed =
        gateway::owner_tunnel_identity_changed(&previous_settings.mcp_gateway, &active, &candidate);
    store.update(candidate.clone())?;
    if reset_gateway_observed {
        let mut settings = store.settings();
        settings.mcp_gateway.clear_observation();
        store.update_settings(settings)?;
    }
    drop(store);

    let timeout = Duration::from_secs(options.wait_seconds);
    let workspace_runtime = if daemon_running {
        match control::request_apply_config_operation(&candidate, timeout).await {
            Ok(result) => Some(result),
            Err(error) => {
                let restore_errors =
                    restore_active_config(&active, &previous_settings, false, false, timeout).await;
                return Err(apply_failure(
                    format!("Workspace daemon 配置应用失败：{error}"),
                    restore_errors,
                ));
            }
        }
    } else {
        None
    };

    let mut gateway_reloaded = false;
    if gateway_live && plan.mcp_tunnel_changed {
        if let Err(error) = gateway_control::request_reload(timeout).await {
            let restore_errors = restore_active_config(
                &active,
                &previous_settings,
                workspace_runtime.is_some(),
                true,
                timeout,
            )
            .await;
            return Err(apply_failure(
                format!("Gateway daemon 配置应用失败：{error}"),
                restore_errors,
            ));
        }
        gateway_reloaded = true;
    }

    let mut warnings = Vec::new();
    if let Err(error) = remove_pending(&active.id) {
        warnings.push(format!("运行态已应用，但清理待应用文件失败：{error}"));
    }
    let output = ConfigApplyReport {
        event: "config_apply",
        workspace_id: active.id,
        applied: true,
        changes,
        apply_plan: plan,
        workspace_runtime,
        gateway_reloaded,
        warnings,
    };
    Ok(output)
}

async fn restore_active_config(
    active: &WorkspaceProfile,
    settings: &crate::settings::AppSettings,
    rollback_workspace_runtime: bool,
    rollback_gateway: bool,
    timeout: Duration,
) -> Vec<String> {
    let mut errors = Vec::new();
    match DataStore::load().and_then(|mut store| {
        store.update(active.clone())?;
        store.update_settings(settings.clone())
    }) {
        Ok(()) => {}
        Err(error) => {
            errors.push(format!("恢复旧磁盘配置失败：{error}"));
            return errors;
        }
    }
    if rollback_workspace_runtime {
        if let Err(error) = control::request_apply_config_operation(active, timeout).await {
            errors.push(format!("恢复 Workspace daemon 运行态失败：{error}"));
        }
    }
    if rollback_gateway {
        if let Err(error) = gateway_control::request_reload(timeout).await {
            errors.push(format!("恢复 Gateway daemon 运行态失败：{error}"));
        }
    }
    errors
}

fn apply_failure(primary: String, restore_errors: Vec<String>) -> AppError {
    if restore_errors.is_empty() {
        AppError::Message(format!("{primary}；已恢复旧配置"))
    } else {
        AppError::Message(format!(
            "{primary}；回滚存在错误：{}",
            restore_errors.join("；")
        ))
    }
}

fn reject_uncoordinated_live_runtime(
    active: &WorkspaceProfile,
    plan: &WorkspaceConfigApplyPlan,
    settings: &crate::settings::AppSettings,
    daemon_running: bool,
) -> AppResult<()> {
    if daemon_running {
        return Ok(());
    }
    let mcp_runtime_change = plan.mcp_listener_reload
        || plan.mcp_callback_policy_hot_update
        || (!settings.mcp_gateway.enabled && plan.mcp_tunnel_changed);
    let actions_runtime_change = plan.actions_listener_reload
        || plan.actions_callback_policy_hot_update
        || plan.actions_tunnel_changed;
    if mcp_runtime_change
        && platform()
            .find_pid_listening_on_port(active.runtime.local_port)?
            .is_some()
    {
        return Err(AppError::Message(
            "当前 Workspace daemon 未运行，但 MCP 端口存在活动 listener；CLI config apply 不会让活动 GUI Server/外部运行态与磁盘配置分叉。请先停止该 listener，或先启动并由 Workspace daemon 接管。"
                .into(),
        ));
    }
    if actions_runtime_change
        && platform()
            .find_pid_listening_on_port(active.actions.local_port)?
            .is_some()
    {
        return Err(AppError::Message(
            "当前 Workspace daemon 未运行，但 Actions 端口存在活动 listener；CLI config apply 不会让活动 GUI Server/外部运行态与磁盘配置分叉。请先停止该 listener，或先启动并由 Workspace daemon 接管。"
                .into(),
        ));
    }
    if settings.mcp_gateway.enabled
        && plan.mcp_tunnel_changed
        && platform()
            .find_pid_listening_on_port(settings.mcp_gateway.local_port)?
            .is_some()
    {
        return Err(AppError::Message(
            "当前平台后台 Gateway daemon 尚未可用，且 Gateway 端口存在活动运行态；CLI config apply 不会接管 GUI Server Gateway。"
                .into(),
        ));
    }
    Ok(())
}

fn validate_candidate(
    store: &DataStore,
    active: &WorkspaceProfile,
    candidate: &WorkspaceProfile,
) -> AppResult<()> {
    if candidate.id != active.id {
        return Err(AppError::Message("config 不允许修改 workspace id".into()));
    }
    validate_workspace_profile(candidate)?;
    validate_workspace_resources_update(store.list(), active, candidate)?;
    gateway::validate_workspace_ports(&store.settings().mcp_gateway, candidate)?;
    let plan = plan_workspace_config_apply(active, candidate);
    if plan.mcp_callback_policy_hot_update {
        validate_redirect_policy(
            &candidate.auth.oauth_redirect_uris,
            &candidate.auth.oauth_redirect_hosts,
        )
        .map_err(AppError::Message)?;
    }
    if plan.actions_callback_policy_hot_update {
        validate_redirect_policy(
            &candidate.actions.oauth_redirect_uris,
            &candidate.actions.oauth_redirect_hosts,
        )
        .map_err(AppError::Message)?;
    }
    Ok(())
}

pub(crate) fn apply_assignments(
    profile: &WorkspaceProfile,
    assignments: &[ConfigAssignment],
) -> AppResult<WorkspaceProfile> {
    let mut value = serde_json::to_value(profile)?;
    for assignment in assignments {
        if assignment.path == "id" || assignment.path.starts_with("id.") {
            return Err(AppError::Message("config 不允许修改 workspace id".into()));
        }
        let target = value_at_path_mut(&mut value, &assignment.path)?;
        *target = parse_typed_value(target, &assignment.value, &assignment.path)?;
    }
    serde_json::from_value(value).map_err(|error| {
        AppError::Message(format!(
            "配置字段更新后无法反序列化 WorkspaceProfile：{error}"
        ))
    })
}

fn parse_typed_value(current: &Value, raw: &str, path: &str) -> AppResult<Value> {
    match current {
        Value::String(_) => Ok(Value::String(raw.to_string())),
        Value::Bool(_) => raw
            .parse::<bool>()
            .map(Value::Bool)
            .map_err(|_| AppError::Message(format!("{path} 必须是 true 或 false"))),
        Value::Number(number) if number.is_u64() => raw
            .parse::<u64>()
            .map(|value| Value::Number(value.into()))
            .map_err(|_| AppError::Message(format!("{path} 必须是无符号整数"))),
        Value::Number(number) if number.is_i64() => raw
            .parse::<i64>()
            .map(|value| Value::Number(value.into()))
            .map_err(|_| AppError::Message(format!("{path} 必须是整数"))),
        Value::Number(_) => serde_json::from_str::<Value>(raw)
            .ok()
            .filter(Value::is_number)
            .ok_or_else(|| AppError::Message(format!("{path} 必须是数字"))),
        _ => Err(AppError::Message(format!(
            "{path} 不是可通过 --set 修改的标量字段"
        ))),
    }
}

fn value_at_path<'a>(root: &'a Value, path: &str) -> AppResult<&'a Value> {
    let mut current = root;
    for segment in path_segments(path)? {
        current = current
            .get(segment)
            .ok_or_else(|| AppError::Message(format!("未知配置字段：{path}")))?;
    }
    Ok(current)
}

fn value_at_path_mut<'a>(root: &'a mut Value, path: &str) -> AppResult<&'a mut Value> {
    let segments = path_segments(path)?;
    descend_mut(root, &segments, path)
}

fn descend_mut<'a>(
    root: &'a mut Value,
    segments: &[&str],
    full_path: &str,
) -> AppResult<&'a mut Value> {
    let Some((first, rest)) = segments.split_first() else {
        return Ok(root);
    };
    let child = root
        .get_mut(*first)
        .ok_or_else(|| AppError::Message(format!("未知配置字段：{full_path}")))?;
    descend_mut(child, rest, full_path)
}

fn path_segments(path: &str) -> AppResult<Vec<&str>> {
    let segments = path.split('.').collect::<Vec<_>>();
    if segments.is_empty() || segments.iter().any(|segment| segment.trim().is_empty()) {
        return Err(AppError::Message(format!("无效配置字段路径：{path}")));
    }
    Ok(segments)
}

fn diff_report(
    event: &'static str,
    active: &WorkspaceProfile,
    candidate: &WorkspaceProfile,
    staged: bool,
) -> ConfigDiffReport {
    ConfigDiffReport {
        event,
        workspace_id: active.id.clone(),
        staged,
        changes: config_changes(active, candidate).unwrap_or_default(),
        apply_plan: plan_workspace_config_apply(active, candidate),
    }
}

fn config_changes(
    active: &WorkspaceProfile,
    candidate: &WorkspaceProfile,
) -> AppResult<Vec<ConfigChange>> {
    let before = serde_json::to_value(active)?;
    let after = serde_json::to_value(candidate)?;
    let mut changes = Vec::new();
    collect_changes("", &before, &after, &mut changes);
    Ok(changes)
}

fn collect_changes(path: &str, before: &Value, after: &Value, changes: &mut Vec<ConfigChange>) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(left), Value::Object(right)) => {
            let keys = left
                .keys()
                .chain(right.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                collect_changes(
                    &child_path,
                    left.get(&key).unwrap_or(&Value::Null),
                    right.get(&key).unwrap_or(&Value::Null),
                    changes,
                );
            }
        }
        _ => changes.push(ConfigChange {
            path: path.to_string(),
            before: before.clone(),
            after: after.clone(),
        }),
    }
}

fn profiles_equal(left: &WorkspaceProfile, right: &WorkspaceProfile) -> AppResult<bool> {
    Ok(serde_json::to_value(left)? == serde_json::to_value(right)?)
}

fn load_pending_for_active(active: &WorkspaceProfile) -> AppResult<Option<PendingWorkspaceConfig>> {
    let Some(pending) = read_pending(&active.id)? else {
        return Ok(None);
    };
    if !profiles_equal(&pending.base, active)? {
        return Err(AppError::Message(format!(
            "workspace {} 的活动配置已在 staging 之后变化；拒绝覆盖。请重新执行 config set 生成新的待应用配置。",
            active.name
        )));
    }
    Ok(Some(pending))
}

fn read_pending(workspace_id: &str) -> AppResult<Option<PendingWorkspaceConfig>> {
    let path = pending_path(workspace_id)?;
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_PENDING_CONFIG_BYTES {
        return Err(AppError::Message(format!(
            "待应用配置文件过大：{} bytes",
            metadata.len()
        )));
    }
    let raw = fs::read(&path)?;
    let pending: PendingWorkspaceConfig = serde_json::from_slice(&raw)
        .map_err(|error| AppError::Message(format!("待应用配置损坏：{error}")))?;
    if pending.schema_version != PENDING_SCHEMA_VERSION || pending.workspace_id != workspace_id {
        return Err(AppError::Message(
            "待应用配置与当前 schema/workspace 不匹配".into(),
        ));
    }
    Ok(Some(pending))
}

fn write_pending(pending: &PendingWorkspaceConfig) -> AppResult<()> {
    let path = pending_path(&pending.workspace_id)?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Message("待应用配置路径缺少父目录".into()))?;
    fs::create_dir_all(parent)?;
    set_private_dir(parent)?;
    let bytes = serde_json::to_vec_pretty(pending)?;
    if bytes.len() as u64 > MAX_PENDING_CONFIG_BYTES {
        return Err(AppError::Message("待应用配置超过 2 MiB 上限".into()));
    }
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::write(&temp, bytes)?;
    set_private_file(&temp)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&temp, &path)?;
    set_private_file(&path)
}

fn remove_pending(workspace_id: &str) -> AppResult<()> {
    let path = pending_path(workspace_id)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn pending_path(workspace_id: &str) -> AppResult<PathBuf> {
    let safe = workspace_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok(platform()
        .app_config_dir()?
        .join("pending-config")
        .join(format!("{safe}.json")))
}

fn set_private_dir(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn set_private_file(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn print_report(report: &impl Serialize, _as_json: bool) -> AppResult<()> {
    super::print_json(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> WorkspaceProfile {
        WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()))
    }

    #[test]
    fn typed_assignment_updates_strings_booleans_and_numbers() {
        let current = profile();
        let updated = apply_assignments(
            &current,
            &[
                ConfigAssignment {
                    path: "name".into(),
                    value: "renamed".into(),
                },
                ConfigAssignment {
                    path: "runtime.workspace_local_entries".into(),
                    value: "false".into(),
                },
                ConfigAssignment {
                    path: "runtime.local_port".into(),
                    value: "29123".into(),
                },
            ],
        )
        .expect("assignments");

        assert_eq!(updated.name, "renamed");
        assert!(!updated.runtime.workspace_local_entries);
        assert_eq!(updated.runtime.local_port, 29_123);
    }

    #[test]
    fn assignment_uses_serialized_tunnel_type_path_and_rejects_workspace_id() {
        let current = profile();
        let updated = apply_assignments(
            &current,
            &[ConfigAssignment {
                path: "tunnel.type".into(),
                value: "frp".into(),
            }],
        )
        .expect("tunnel type");
        assert_eq!(updated.tunnel.tunnel_type, "frp");

        let error = apply_assignments(
            &current,
            &[ConfigAssignment {
                path: "id".into(),
                value: "other".into(),
            }],
        )
        .expect_err("id must stay immutable");
        assert!(error.to_string().contains("workspace id"));
    }

    #[test]
    fn diff_is_field_level_and_apply_plan_is_shared() {
        let current = profile();
        let mut next = current.clone();
        next.runtime.permission_mode = "read_only".into();
        next.actions.max_patch_bytes += 10;

        let report = diff_report("config_diff", &current, &next, true);
        assert_eq!(report.changes.len(), 2);
        assert!(report
            .changes
            .iter()
            .any(|change| change.path == "runtime.permission_mode"));
        assert!(report
            .changes
            .iter()
            .any(|change| change.path == "actions.max_patch_bytes"));
        assert!(report.apply_plan.mcp_listener_reload);
        assert!(report.apply_plan.actions_listener_reload);
    }
}
