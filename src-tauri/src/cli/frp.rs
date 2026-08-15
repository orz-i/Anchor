use std::io::{self, Read};

use serde::Serialize;

use crate::data::{AppData, DataStore};
use crate::error::{AppError, AppResult};
use crate::settings::{FrpProfile, McpGatewayConfig};
use crate::workspace::WorkspaceProfile;

use super::args::{FrpAddOptions, FrpCommand, FrpDeleteOptions, FrpTokenInput, FrpUpdateOptions};

const FRP_PROFILE_TOKEN_SCOPE: &str = "frp_profile_token";
const MAX_FRP_TOKEN_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrpProfileView {
    pub id: String,
    pub name: String,
    pub server: String,
    pub server_port: u16,
    pub has_token: bool,
    pub references: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrpMutationReport {
    event: &'static str,
    profile: FrpProfileView,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrpDeleteReport {
    event: &'static str,
    id: String,
    name: String,
    deleted: bool,
}

pub async fn execute(command: FrpCommand, _as_json: bool) -> AppResult<i32> {
    match command {
        FrpCommand::List => {
            let profiles = DataStore::read_file(|data| Ok(profile_views(data)))?;
            super::print_json(&profiles)?;
        }
        FrpCommand::Show { profile } => {
            let view = DataStore::read_file(|data| {
                let index = resolve_profile_index(&data.frp_profiles, &profile)?;
                Ok(profile_view(data, &data.frp_profiles[index]))
            })?;
            super::print_json(&view)?;
        }
        FrpCommand::Add(options) => {
            let view = add_profile(options)?;
            super::print_json(&FrpMutationReport {
                event: "frp_profile_added",
                profile: view,
            })?;
        }
        FrpCommand::Update(options) => {
            let view = update_profile(options)?;
            super::print_json(&FrpMutationReport {
                event: "frp_profile_updated",
                profile: view,
            })?;
        }
        FrpCommand::Delete(options) => {
            let (id, name) = delete_profile(options)?;
            super::print_json(&FrpDeleteReport {
                event: "frp_profile_deleted",
                id,
                name,
                deleted: true,
            })?;
        }
    }
    Ok(0)
}

fn add_profile(options: FrpAddOptions) -> AppResult<FrpProfileView> {
    let name = normalize_required("FRP profile name", &options.name)?;
    let server = normalize_server(&options.server)?;
    let token = read_token_input(options.token)?;
    DataStore::update_file(|data| {
        ensure_unique_name(&data.frp_profiles, &name, None)?;
        let profile = FrpProfile {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name,
            server,
            server_port: options.server_port,
        };
        if let Some(token) = token.as_ref() {
            data.app_secrets
                .entry(FRP_PROFILE_TOKEN_SCOPE.into())
                .or_default()
                .insert(profile.id.clone(), token.clone());
        }
        data.frp_profiles.push(profile.clone());
        Ok(profile_view(data, &profile))
    })
}

fn update_profile(options: FrpUpdateOptions) -> AppResult<FrpProfileView> {
    let token = read_token_input(options.token)?;
    let connection_changes = options.server.is_some()
        || options.server_port.is_some()
        || token.is_some()
        || options.clear_token;
    if connection_changes {
        let (profile, workspaces, gateway) = DataStore::read_file(|data| {
            let index = resolve_profile_index(&data.frp_profiles, &options.profile)?;
            Ok((
                data.frp_profiles[index].clone(),
                data.profiles.clone(),
                data.mcp_gateway.clone(),
            ))
        })?;
        ensure_profile_not_live(&profile, &workspaces, &gateway)?;
    }
    DataStore::update_file(|data| {
        let index = resolve_profile_index(&data.frp_profiles, &options.profile)?;
        let id = data.frp_profiles[index].id.clone();

        if let Some(name) = options.name.as_deref() {
            let name = normalize_required("FRP profile name", name)?;
            ensure_unique_name(&data.frp_profiles, &name, Some(&id))?;
            data.frp_profiles[index].name = name;
        }
        if let Some(server) = options.server.as_deref() {
            data.frp_profiles[index].server = normalize_server(server)?;
        }
        if let Some(port) = options.server_port {
            if port == 0 {
                return Err(AppError::Message("FRP 服务器端口必须大于 0".into()));
            }
            data.frp_profiles[index].server_port = port;
        }
        if let Some(token) = token.as_ref() {
            data.app_secrets
                .entry(FRP_PROFILE_TOKEN_SCOPE.into())
                .or_default()
                .insert(id.clone(), token.clone());
        } else if options.clear_token {
            delete_token(data, &id);
        }

        let profile = data.frp_profiles[index].clone();
        Ok(profile_view(data, &profile))
    })
}

fn delete_profile(options: FrpDeleteOptions) -> AppResult<(String, String)> {
    if !options.force {
        return Err(AppError::Message(
            "删除 FRP profile 需要显式添加 --force；不会删除任何 workspace。".into(),
        ));
    }
    DataStore::update_file(|data| {
        let index = resolve_profile_index(&data.frp_profiles, &options.profile)?;
        let profile = data.frp_profiles[index].clone();
        let references = all_profile_references(data, &profile.id)?;
        if !references.is_empty() {
            return Err(AppError::Message(format!(
                "FRP profile {} 仍被以下 tunnel 使用：{}。请先通过 `anchor tunnel configure ... --clear-frp-profile` 解除引用。",
                profile.name,
                references.join(", ")
            )));
        }
        data.frp_profiles.remove(index);
        delete_token(data, &profile.id);
        Ok((profile.id, profile.name))
    })
}

fn delete_token(data: &mut AppData, id: &str) {
    if let Some(tokens) = data.app_secrets.get_mut(FRP_PROFILE_TOKEN_SCOPE) {
        tokens.remove(id);
        if tokens.is_empty() {
            data.app_secrets.remove(FRP_PROFILE_TOKEN_SCOPE);
        }
    }
}

fn normalize_required(label: &str, value: &str) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::Message(format!("{label} 不能为空")));
    }
    Ok(value.to_string())
}

fn normalize_server(value: &str) -> AppResult<String> {
    let value = normalize_required("FRP server", value)?;
    if value.contains("//") || value.contains('/') || value.contains(char::is_whitespace) {
        return Err(AppError::Message(
            "FRP server 只接受主机名或 IP，不要包含协议、路径或空白字符".into(),
        ));
    }
    Ok(value.trim_end_matches('.').to_string())
}

fn read_token_input(input: Option<FrpTokenInput>) -> AppResult<Option<String>> {
    let Some(input) = input else {
        return Ok(None);
    };
    let raw = match input {
        FrpTokenInput::Inline(value) => value,
        FrpTokenInput::File(path) => {
            let metadata = std::fs::metadata(&path).map_err(|error| {
                AppError::Message(format!(
                    "无法读取 FRP token 文件 {}：{error}",
                    path.display()
                ))
            })?;
            if metadata.len() > MAX_FRP_TOKEN_BYTES {
                return Err(AppError::Message(format!(
                    "FRP token 文件过大：{} bytes；最大允许 {} bytes",
                    metadata.len(),
                    MAX_FRP_TOKEN_BYTES
                )));
            }
            std::fs::read_to_string(&path).map_err(|error| {
                AppError::Message(format!(
                    "无法读取 FRP token 文件 {}：{error}",
                    path.display()
                ))
            })?
        }
        FrpTokenInput::Stdin => {
            let mut raw = String::new();
            io::stdin()
                .take(MAX_FRP_TOKEN_BYTES + 1)
                .read_to_string(&mut raw)
                .map_err(|error| {
                    AppError::Message(format!("读取 FRP token stdin 失败：{error}"))
                })?;
            if raw.len() as u64 > MAX_FRP_TOKEN_BYTES {
                return Err(AppError::Message(format!(
                    "FRP token stdin 过大；最大允许 {} bytes",
                    MAX_FRP_TOKEN_BYTES
                )));
            }
            raw
        }
    };
    let token = raw.trim().to_string();
    if token.is_empty() {
        return Err(AppError::Message("FRP token 不能为空".into()));
    }
    Ok(Some(token))
}

fn ensure_unique_name(
    profiles: &[FrpProfile],
    name: &str,
    except_id: Option<&str>,
) -> AppResult<()> {
    if profiles.iter().any(|profile| {
        Some(profile.id.as_str()) != except_id && profile.name.trim().eq_ignore_ascii_case(name)
    }) {
        return Err(AppError::Message(format!("FRP profile 名称已存在：{name}")));
    }
    Ok(())
}

pub(crate) fn resolve_profile_id(profiles: &[FrpProfile], selector: &str) -> AppResult<String> {
    let index = resolve_profile_index(profiles, selector)?;
    Ok(profiles[index].id.clone())
}

fn resolve_profile_index(profiles: &[FrpProfile], selector: &str) -> AppResult<usize> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(AppError::Message("FRP profile 不能为空".into()));
    }
    if let Some(index) = profiles.iter().position(|profile| profile.id == selector) {
        return Ok(index);
    }
    let matches = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| profile.name.trim().eq_ignore_ascii_case(selector))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(AppError::Message(format!("未找到 FRP profile：{selector}"))),
        _ => Err(AppError::Message(format!(
            "FRP profile 名称不唯一：{selector}；请改用 profile ID"
        ))),
    }
}

fn profile_views(data: &AppData) -> Vec<FrpProfileView> {
    data.frp_profiles
        .iter()
        .map(|profile| profile_view(data, profile))
        .collect()
}

fn profile_view(data: &AppData, profile: &FrpProfile) -> FrpProfileView {
    FrpProfileView {
        id: profile.id.clone(),
        name: profile.name.clone(),
        server: profile.server.clone(),
        server_port: profile.server_port,
        has_token: data
            .app_secrets
            .get(FRP_PROFILE_TOKEN_SCOPE)
            .and_then(|tokens| tokens.get(&profile.id))
            .is_some_and(|token| !token.trim().is_empty()),
        references: profile_references(data, &profile.id),
    }
}

fn profile_references(data: &AppData, id: &str) -> Vec<String> {
    let mut references = Vec::new();
    for workspace in &data.profiles {
        if workspace.tunnel.frp_profile_id == id {
            references.push(format!("{}:mcp", workspace.name));
        }
        if workspace.actions.frp_profile_id == id {
            references.push(format!("{}:actions", workspace.name));
        }
    }
    references
}

fn all_profile_references(data: &AppData, id: &str) -> AppResult<Vec<String>> {
    let mut references = profile_references(data, id);
    for workspace in &data.profiles {
        let Some(pending) = super::config::pending_candidate(workspace)? else {
            continue;
        };
        if pending.tunnel.frp_profile_id == id && workspace.tunnel.frp_profile_id != id {
            references.push(format!("{}:mcp(pending)", workspace.name));
        }
        if pending.actions.frp_profile_id == id && workspace.actions.frp_profile_id != id {
            references.push(format!("{}:actions(pending)", workspace.name));
        }
    }
    references.sort();
    references.dedup();
    Ok(references)
}

fn ensure_profile_not_live(
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
            if workspace.actions.frp_profile_id == profile.id
                && managed.is_some_and(|selection| selection.includes_actions())
            {
                live.push(format!("{}:actions", workspace.name));
            }
        }
    }

    if gateway.enabled && !gateway.owner_workspace_id.trim().is_empty() {
        if let Some(owner) = workspaces.iter().find(|workspace| {
            workspace.id == gateway.owner_workspace_id
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceProfile;

    #[test]
    fn profile_selector_prefers_id_and_accepts_unique_name() {
        let profiles = vec![
            FrpProfile {
                id: "p1".into(),
                name: "Production".into(),
                server: "frp.example.com".into(),
                server_port: 7000,
            },
            FrpProfile {
                id: "p2".into(),
                name: "Backup".into(),
                server: "backup.example.com".into(),
                server_port: 7001,
            },
        ];
        assert_eq!(resolve_profile_id(&profiles, "p1").unwrap(), "p1");
        assert_eq!(resolve_profile_id(&profiles, "backup").unwrap(), "p2");
    }

    #[test]
    fn profile_view_never_exposes_token_and_reports_references() {
        let profile = FrpProfile {
            id: "p1".into(),
            name: "Production".into(),
            server: "frp.example.com".into(),
            server_port: 7000,
        };
        let mut workspace = WorkspaceProfile::new("/tmp/demo".into(), Some("demo".into()));
        workspace.tunnel.frp_profile_id = "p1".into();
        let mut data = AppData {
            frp_profiles: vec![profile.clone()],
            profiles: vec![workspace],
            ..AppData::default()
        };
        data.app_secrets
            .entry(FRP_PROFILE_TOKEN_SCOPE.into())
            .or_default()
            .insert("p1".into(), "super-secret".into());

        let view = profile_view(&data, &profile);
        assert!(view.has_token);
        assert_eq!(view.references, vec!["demo:mcp"]);
        let serialized = serde_json::to_string(&view).unwrap();
        assert!(!serialized.contains("super-secret"));
    }

    #[test]
    fn server_rejects_urls_and_paths() {
        assert_eq!(
            normalize_server("frp.example.com.").unwrap(),
            "frp.example.com"
        );
        assert!(normalize_server("https://frp.example.com").is_err());
        assert!(normalize_server("frp.example.com/path").is_err());
    }

    #[test]
    fn token_file_is_trimmed_and_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("token.txt");
        std::fs::write(&path, " secret-from-file\n").unwrap();
        assert_eq!(
            read_token_input(Some(FrpTokenInput::File(path))).unwrap(),
            Some("secret-from-file".into())
        );
    }
}
