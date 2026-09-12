use std::io::{self, Read};

use serde::Serialize;

use crate::data::{AppData, DataStore};
use crate::error::{AppError, AppResult};
use crate::settings::{FrpProfile, FrpProfileInput};

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

pub async fn execute(command: FrpCommand) -> AppResult<i32> {
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
    let token = read_token_input(options.token)?;
    let saved = crate::management::save_frp_profile_metadata(FrpProfileInput {
        id: String::new(),
        name: options.name,
        server: options.server,
        server_port: options.server_port,
    })?;
    if let Some(token) = token {
        crate::management::set_frp_profile_token(&saved.id, &token)?;
    }
    profile_view_by_id(&saved.id)
}

fn update_profile(options: FrpUpdateOptions) -> AppResult<FrpProfileView> {
    let token = read_token_input(options.token)?;
    let current = DataStore::read_file(|data| {
        let index = resolve_profile_index(&data.frp_profiles, &options.profile)?;
        Ok(data.frp_profiles[index].clone())
    })?;
    let saved = crate::management::save_frp_profile_metadata(FrpProfileInput {
        id: current.id.clone(),
        name: options.name.unwrap_or(current.name),
        server: options.server.unwrap_or(current.server),
        server_port: options.server_port.unwrap_or(current.server_port),
    })?;
    if let Some(token) = token {
        crate::management::set_frp_profile_token(&saved.id, &token)?;
    } else if options.clear_token {
        crate::management::clear_frp_profile_token(&saved.id)?;
    }
    profile_view_by_id(&saved.id)
}

fn delete_profile(options: FrpDeleteOptions) -> AppResult<(String, String)> {
    if !options.force {
        return Err(AppError::Message(
            "删除 FRP profile 需要显式添加 --force；不会删除任何 workspace。".into(),
        ));
    }
    let profile = DataStore::read_file(|data| {
        let index = resolve_profile_index(&data.frp_profiles, &options.profile)?;
        Ok(data.frp_profiles[index].clone())
    })?;
    crate::management::delete_frp_profile(&profile.id)?;
    Ok((profile.id, profile.name))
}

pub(super) fn read_token_input(input: Option<FrpTokenInput>) -> AppResult<Option<String>> {
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

fn profile_view_by_id(id: &str) -> AppResult<FrpProfileView> {
    DataStore::read_file(|data| {
        let profile = data
            .frp_profiles
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| AppError::Message(format!("FRP profile not found: {id}")))?;
        Ok(profile_view(data, profile))
    })
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
    for tunnel in &data.tunnels {
        if tunnel.service.eq_ignore_ascii_case("mcp") && tunnel.config.frp_profile_id == id {
            let workspace_name = data
                .profiles
                .iter()
                .find(|workspace| workspace.id == tunnel.workspace_id)
                .map(|workspace| workspace.name.as_str())
                .unwrap_or(tunnel.workspace_id.as_str());
            references.push(format!("{workspace_name}:mcp"));
        }
    }
    references
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnel::TunnelProfile;
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
        let workspace = WorkspaceProfile::new("/tmp/demo".into(), Some("demo".into()));
        let mut tunnel = TunnelProfile::new(workspace.id.clone(), &workspace.name, "mcp");
        tunnel.config.frp_profile_id = "p1".into();
        let mut data = AppData {
            frp_profiles: vec![profile.clone()],
            profiles: vec![workspace],
            tunnels: vec![tunnel],
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
