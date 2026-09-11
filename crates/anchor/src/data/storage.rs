#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;
use crate::platform::platform;
use crate::tunnel::{TunnelConfig, TunnelProfile};

use super::model::{
    AppData, ProfilesData, SecretsData, PROFILES_SCHEMA_VERSION, SECRETS_SCHEMA_VERSION,
};
use super::secret_protection;

const SECRETS_ENVELOPE_VERSION: u32 = 1;
#[cfg(windows)]
const SERVICE_RUNTIME_APP_SECRET_SCOPES: &[&str] =
    &["oauth_refresh_replay", "federation_request_replay"];

#[derive(Debug, Serialize, Deserialize)]
struct SecretsEnvelope {
    version: u32,
    protection: String,
    payload: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_protection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_payload: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretAccess {
    User,
    #[cfg(windows)]
    Service,
}

pub fn data_file_path() -> AppResult<PathBuf> {
    Ok(platform()
        .app_config_dir()?
        .join("data")
        .join("profiles.json"))
}

fn has_primary_or_backup(path: &Path) -> bool {
    path.exists() || backup_path(path).exists()
}

fn load_secrets(data: &mut AppData, access: SecretAccess) -> AppResult<()> {
    let secrets_path = secrets_file_path()?;
    if has_primary_or_backup(&secrets_path) {
        let loaded = load_secrets_with_backup(&secrets_path, access)?;
        loaded.apply_to(data);
    }
    Ok(())
}

pub fn secrets_file_path() -> AppResult<PathBuf> {
    Ok(platform()
        .app_config_dir()?
        .join("data")
        .join("secrets.json"))
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default, clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn writes_backup_before_replacing_configuration() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        let mut first = AppData::default();
        first.last_workspace_id = "first".into();
        write_data(&path, &first).expect("first write");

        let mut second = AppData::default();
        second.last_workspace_id = "second".into();
        write_data(&path, &second).expect("second write");

        assert_eq!(
            read_data(&path).expect("current").last_workspace_id,
            "second"
        );
        assert_eq!(
            read_data(&backup_path(&path))
                .expect("backup")
                .last_workspace_id,
            "first"
        );
    }

    #[test]
    fn restores_invalid_primary_file_from_backup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        let mut data = AppData::default();
        data.last_workspace_id = "recover-me".into();
        let persisted = ProfilesData::from_app_data(&data);
        atomic_write(
            &backup_path(&path),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&persisted).expect("json")
            )
            .as_bytes(),
        )
        .expect("backup write");
        fs::write(&path, "{not-json").expect("corrupt primary");

        let recovered = load_with_backup::<ProfilesData>(&path)
            .expect("recover")
            .into_app_data();

        assert_eq!(recovered.last_workspace_id, "recover-me");
        assert_eq!(
            read_data(&path).expect("restored").last_workspace_id,
            "recover-me"
        );
    }

    #[test]
    fn restores_missing_primary_file_from_backup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        let mut data = AppData::default();
        data.last_workspace_id = "backup-only".into();
        let persisted = ProfilesData::from_app_data(&data);
        atomic_write(
            &backup_path(&path),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&persisted).expect("json")
            )
            .as_bytes(),
        )
        .expect("backup write");

        let recovered = load_with_backup::<ProfilesData>(&path)
            .expect("recover")
            .into_app_data();

        assert_eq!(recovered.last_workspace_id, "backup-only");
        assert!(path.exists());
    }

    #[test]
    fn rejects_invalid_primary_without_backup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        fs::write(&path, "{not-json").expect("corrupt primary");

        let error = load_with_backup::<ProfilesData>(&path).expect_err("invalid config must fail");

        assert!(error.to_string().contains("无法解析配置文件"));
    }

    #[test]
    fn unversioned_profiles_are_hard_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        let mut data = AppData::default();
        data.profiles.push(crate::workspace::WorkspaceProfile::new(
            ".".into(),
            Some("unversioned".into()),
        ));
        let mut value =
            serde_json::to_value(ProfilesData::from_app_data(&data)).expect("profiles json");
        value
            .as_object_mut()
            .expect("profiles object")
            .remove("schema_version");
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&value).expect("json")),
        )
        .expect("unversioned profiles");

        let error =
            load_profiles_with_backup(&path).expect_err("unversioned profiles must be rejected");

        assert!(error.to_string().contains("缺少 schema_version"));
    }

    #[test]
    fn profiles_reject_future_schema_versions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        let data = AppData::default();
        let mut value =
            serde_json::to_value(ProfilesData::from_app_data(&data)).expect("profiles json");
        value["schema_version"] = serde_json::Value::from(PROFILES_SCHEMA_VERSION + 1);
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&value).expect("json")),
        )
        .expect("future profiles");

        let error = load_profiles_with_backup(&path).expect_err("future schema must fail");
        assert!(error.to_string().contains("不支持的配置 schema_version"));
    }

    #[test]
    fn profiles_v1_migrate_workspace_tunnel_and_gateway_owner_to_top_level_tunnel() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        let mut data = AppData::default();
        let mut workspace = crate::workspace::WorkspaceProfile::new(
            "C:/workspace/demo".into(),
            Some("demo".into()),
        );
        workspace.id = "workspace-demo".into();
        data.profiles.push(workspace);
        let mut value =
            serde_json::to_value(ProfilesData::from_app_data(&data)).expect("profiles json");
        value["schema_version"] = serde_json::Value::from(1);
        value.as_object_mut().expect("root").remove("tunnels");
        value["profiles"][0]["tunnel"] =
            serde_json::to_value(TunnelConfig::default()).expect("legacy tunnel");
        let gateway = value["mcp_gateway"].as_object_mut().expect("gateway");
        gateway.remove("tunnelId");
        gateway.remove("observedTunnelId");
        gateway.insert(
            "ownerWorkspaceId".into(),
            serde_json::Value::String("workspace-demo".into()),
        );
        gateway.insert(
            "observedOwnerWorkspaceId".into(),
            serde_json::Value::String("workspace-demo".into()),
        );
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&value).expect("json")),
        )
        .expect("legacy profiles");

        let migrated = load_profiles_with_backup(&path).expect("migrate v1");
        assert_eq!(migrated.schema_version, PROFILES_SCHEMA_VERSION);
        assert_eq!(migrated.tunnels.len(), 1);
        assert_eq!(migrated.tunnels[0].id, "workspace-demo-mcp");
        assert_eq!(migrated.tunnels[0].workspace_id, "workspace-demo");
        assert!(migrated.tunnels[0].enabled);
        assert_eq!(migrated.mcp_gateway.tunnel_id, "workspace-demo-mcp");
        assert_eq!(
            migrated.mcp_gateway.observed_tunnel_id,
            "workspace-demo-mcp"
        );

        let rewritten: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("rewritten profiles"))
                .expect("rewritten json");
        assert_eq!(rewritten["schema_version"], PROFILES_SCHEMA_VERSION);
        assert!(rewritten["profiles"][0].get("tunnel").is_none());
        assert_eq!(rewritten["tunnels"][0]["workspace_id"], "workspace-demo");
        assert_eq!(rewritten["mcp_gateway"]["tunnelId"], "workspace-demo-mcp");
        assert!(rewritten["mcp_gateway"].get("ownerWorkspaceId").is_none());
    }

    #[test]
    fn legacy_workspace_tunnel_tokens_move_to_tunnel_scopes() {
        let mut data = AppData::default();
        data.tunnels.push(TunnelProfile::migrated(
            "workspace-demo".into(),
            "demo",
            TunnelConfig::default(),
        ));
        data.workspace_secrets
            .entry("workspace-demo".into())
            .or_default()
            .insert("frp_token".into(), "frp-secret".into());
        data.workspace_secrets
            .entry("workspace-demo".into())
            .or_default()
            .insert("cloudflare_token".into(), "cf-secret".into());

        assert!(migrate_legacy_tunnel_secrets(&mut data));
        assert!(data.workspace_secrets.get("workspace-demo").is_none());
        assert_eq!(
            data.app_secrets["tunnel_frp_token"]["workspace-demo-mcp"],
            "frp-secret"
        );
        assert_eq!(
            data.app_secrets["tunnel_cloudflare_token"]["workspace-demo-mcp"],
            "cf-secret"
        );
    }

    #[test]
    fn rejects_unprotected_plaintext_secrets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let secrets_path = temp.path().join("secrets.json");
        fs::write(
            &secrets_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "shared_secrets": {"token": "plaintext"},
                "workspace_secrets": {},
                "app_secrets": {}
            }))
            .expect("plaintext json"),
        )
        .expect("plaintext write");

        let error = read_secrets_file(&secrets_path, SecretAccess::User)
            .expect_err("plaintext secrets must fail");

        assert!(error.to_string().contains("受保护的凭据封装"));
    }

    #[test]
    fn unversioned_secrets_are_hard_rejected() {
        let mut unversioned = serde_json::to_value(SecretsData::default()).expect("secrets json");
        unversioned
            .as_object_mut()
            .expect("secrets object")
            .remove("schema_version");
        let plaintext = serde_json::to_vec(&unversioned).expect("unversioned secrets");
        let (protection, protected) =
            secret_protection::protect(&plaintext).expect("protect unversioned secrets");
        let envelope = SecretsEnvelope {
            version: SECRETS_ENVELOPE_VERSION,
            protection: protection.into(),
            payload: BASE64_STANDARD.encode(protected),
            service_protection: None,
            service_payload: None,
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("secrets.json");
        atomic_write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&envelope).expect("envelope")
            )
            .as_bytes(),
        )
        .expect("unversioned envelope");

        let error = load_secrets_with_backup(&path, SecretAccess::User)
            .expect_err("unversioned secrets must be rejected");
        assert!(error.to_string().contains("缺少 schema_version"));
    }

    #[test]
    fn secrets_reject_future_content_schema_versions() {
        let mut value = serde_json::to_value(SecretsData::default()).expect("secrets json");
        value["schema_version"] = serde_json::Value::from(SECRETS_SCHEMA_VERSION + 1);
        let plaintext = serde_json::to_vec(&value).expect("future secrets");

        let error = parse_secrets_payload(&plaintext).expect_err("future schema must fail");
        assert!(error
            .to_string()
            .contains("不支持的凭据内容 schema_version"));
    }

    #[test]
    fn secret_envelope_round_trips_user_and_service_payloads() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("secrets.json");
        let mut data = SecretsData::default();
        data.workspace_secrets
            .entry("workspace".into())
            .or_default()
            .insert("bearer_token".into(), "secret-value".into());

        write_secrets_data(&path, &data).expect("write dual envelope");

        let user = read_secrets_file(&path, SecretAccess::User).expect("read user payload");
        assert_eq!(user.workspace_secrets, data.workspace_secrets);

        #[cfg(windows)]
        {
            let service =
                read_secrets_file(&path, SecretAccess::Service).expect("read service payload");
            assert_eq!(service.workspace_secrets, data.workspace_secrets);
            let envelope = read_secrets_envelope(&path).expect("read envelope");
            assert_eq!(envelope.protection, "windows-dpapi-current-user-v1");
            assert_eq!(
                envelope.service_protection.as_deref(),
                Some("windows-dpapi-local-machine-v1")
            );
            assert!(envelope.service_payload.is_some());
        }
    }

    #[cfg(windows)]
    #[test]
    fn user_load_upgrades_legacy_envelope_with_service_mirror() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("secrets.json");
        let mut data = SecretsData::default();
        data.shared_secrets
            .insert("bearer_token".into(), "legacy-secret".into());
        let plaintext = serde_json::to_vec(&data).expect("serialize secrets");
        let (protection, protected) = secret_protection::protect(&plaintext).expect("protect user");
        let legacy = SecretsEnvelope {
            version: SECRETS_ENVELOPE_VERSION,
            protection: protection.into(),
            payload: BASE64_STANDARD.encode(protected),
            service_protection: None,
            service_payload: None,
        };
        write_json(&path, &legacy).expect("write legacy envelope");
        let original = read_secrets_envelope(&path).expect("legacy envelope");

        let loaded =
            load_secrets_with_backup(&path, SecretAccess::User).expect("upgrade user envelope");
        assert_eq!(loaded.shared_secrets, data.shared_secrets);

        let upgraded = read_secrets_envelope(&path).expect("upgraded envelope");
        assert_eq!(upgraded.protection, original.protection);
        assert_eq!(upgraded.payload, original.payload);
        assert_eq!(
            upgraded.service_protection.as_deref(),
            Some("windows-dpapi-local-machine-v1")
        );
        let service =
            read_secrets_file(&path, SecretAccess::Service).expect("service mirror readable");
        assert_eq!(service.shared_secrets, data.shared_secrets);
    }

    #[cfg(windows)]
    #[test]
    fn service_secret_update_preserves_user_ciphertext() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("secrets.json");
        let mut user_data = SecretsData::default();
        user_data
            .shared_secrets
            .insert("key".into(), "user-value".into());
        write_secrets_data(&path, &user_data).expect("write initial secrets");
        let before = read_secrets_envelope(&path).expect("before envelope");

        let mut service_data = user_data.clone();
        service_data
            .app_secrets
            .entry("oauth_refresh_replay".into())
            .or_default()
            .insert("workspace".into(), "runtime-state".into());
        write_service_secrets_data(&path, &service_data).expect("update service mirror");

        let after = read_secrets_envelope(&path).expect("after envelope");
        assert_eq!(after.protection, before.protection);
        assert_eq!(after.payload, before.payload);
        let user = read_secrets_file(&path, SecretAccess::User).expect("user payload");
        let service = read_secrets_file(&path, SecretAccess::Service).expect("service payload");
        assert_eq!(user.app_secrets, user_data.app_secrets);
        assert_eq!(service.app_secrets, service_data.app_secrets);

        let mut later_user_data = user_data.clone();
        later_user_data
            .shared_secrets
            .insert("key".into(), "updated-user-value".into());
        write_secrets_data(&path, &later_user_data).expect("refresh user and service payloads");

        let later_user = read_secrets_file(&path, SecretAccess::User).expect("later user payload");
        let later_service =
            read_secrets_file(&path, SecretAccess::Service).expect("later service payload");
        assert_eq!(
            later_user.shared_secrets.get("key").map(String::as_str),
            Some("updated-user-value")
        );
        assert_eq!(
            later_service.shared_secrets.get("key").map(String::as_str),
            Some("updated-user-value")
        );
        assert_eq!(
            later_service
                .app_secrets
                .get("oauth_refresh_replay")
                .and_then(|items| items.get("workspace"))
                .map(String::as_str),
            Some("runtime-state")
        );
    }
}

pub fn load() -> AppResult<AppData> {
    load_with_secret_access(current_secret_access())
}

pub(crate) fn load_profiles_only() -> AppResult<AppData> {
    let path = data_file_path()?;
    if has_primary_or_backup(&path) {
        return Ok(load_profiles_with_backup(&path)?.into_app_data());
    }
    Ok(AppData::default())
}

fn load_with_secret_access(access: SecretAccess) -> AppResult<AppData> {
    let mut data = load_profiles_only()?;
    load_secrets(&mut data, access)?;
    if migrate_legacy_tunnel_secrets(&mut data) {
        let secrets_path = secrets_file_path()?;
        let secrets = SecretsData::from_app_data(&data);
        match access {
            SecretAccess::User => write_secrets_data(&secrets_path, &secrets)?,
            #[cfg(windows)]
            SecretAccess::Service => write_service_secrets_data(&secrets_path, &secrets)?,
        }
    }
    Ok(data)
}

fn migrate_legacy_tunnel_secrets(data: &mut AppData) -> bool {
    let mut changed = false;
    for tunnel in &data.tunnels {
        let Some(workspace_secrets) = data.workspace_secrets.get_mut(&tunnel.workspace_id) else {
            continue;
        };
        for (legacy_key, scope) in [
            ("frp_token", "tunnel_frp_token"),
            ("cloudflare_token", "tunnel_cloudflare_token"),
        ] {
            let Some(value) = workspace_secrets.remove(legacy_key) else {
                continue;
            };
            data.app_secrets
                .entry(scope.to_string())
                .or_default()
                .entry(tunnel.id.clone())
                .or_insert(value);
            changed = true;
        }
    }
    data.workspace_secrets
        .retain(|_, secrets| !secrets.is_empty());
    changed
}

pub fn save(data: &AppData) -> AppResult<()> {
    let path = data_file_path()?;
    let secrets_path = secrets_file_path()?;
    let secrets = SecretsData::from_app_data(data);
    match current_secret_access() {
        SecretAccess::User => write_secrets_data(&secrets_path, &secrets)?,
        #[cfg(windows)]
        SecretAccess::Service => write_service_secrets_data(&secrets_path, &secrets)?,
    }
    write_data(&path, data)
}

fn write_data(path: &Path, data: &AppData) -> AppResult<()> {
    write_json(path, &ProfilesData::from_app_data(data))
}

fn write_secrets_data(path: &Path, data: &SecretsData) -> AppResult<()> {
    let plaintext = serde_json::to_vec(data)?;
    let (protection, protected) =
        secret_protection::protect(&plaintext).map_err(crate::error::AppError::Message)?;
    #[cfg(windows)]
    let (service_protection, service_payload) = {
        let service_data = service_secrets_for_user_write(path, data);
        let service_plaintext = serde_json::to_vec(&service_data)?;
        let (protection, protected) = secret_protection::protect_for_service(&service_plaintext)
            .map_err(crate::error::AppError::Message)?;
        (
            Some(protection.into()),
            Some(BASE64_STANDARD.encode(protected)),
        )
    };
    #[cfg(not(windows))]
    let (service_protection, service_payload) = (None, None);
    let envelope = SecretsEnvelope {
        version: SECRETS_ENVELOPE_VERSION,
        protection: protection.into(),
        payload: BASE64_STANDARD.encode(protected),
        service_protection,
        service_payload,
    };
    write_json(path, &envelope)
}

#[cfg(windows)]
fn service_secrets_for_user_write(path: &Path, data: &SecretsData) -> SecretsData {
    let mut service_data = data.clone();
    let Ok(existing_service_data) = read_secrets_file(path, SecretAccess::Service) else {
        return service_data;
    };
    for scope in SERVICE_RUNTIME_APP_SECRET_SCOPES {
        if let Some(items) = existing_service_data.app_secrets.get(*scope) {
            service_data
                .app_secrets
                .insert((*scope).to_string(), items.clone());
        }
    }
    service_data
}

#[cfg(windows)]
fn write_service_secrets_data(path: &Path, data: &SecretsData) -> AppResult<()> {
    let mut envelope = read_secrets_envelope(path).map_err(|error| {
        crate::error::AppError::Message(format!(
            "Windows Service 无法更新凭据镜像，因为用户凭据封装不可用：{error}"
        ))
    })?;
    let plaintext = serde_json::to_vec(data)?;
    let (protection, protected) = secret_protection::protect_for_service(&plaintext)
        .map_err(crate::error::AppError::Message)?;
    envelope.service_protection = Some(protection.into());
    envelope.service_payload = Some(BASE64_STANDARD.encode(protected));
    write_json(path, &envelope)
}

fn load_secrets_with_backup(path: &Path, access: SecretAccess) -> AppResult<SecretsData> {
    match read_secrets_file_versioned(path, access) {
        Ok(data) => {
            if access == SecretAccess::User {
                ensure_service_secret_mirror(path, &data)?;
            }
            Ok(data)
        }
        Err(primary_error) => {
            let backup = backup_path(path);
            if !backup.exists() {
                return Err(primary_error);
            }
            let recovered = read_secrets_file_versioned(&backup, access).map_err(|backup_error| {
                crate::error::AppError::Message(format!(
                    "凭据文件损坏且备份无法读取：主文件错误：{primary_error}；备份错误：{backup_error}"
                ))
            })?;
            match access {
                SecretAccess::User => write_secrets_data(path, &recovered)?,
                #[cfg(windows)]
                SecretAccess::Service => {
                    let raw = fs::read(&backup)?;
                    atomic_write(path, &raw)?;
                }
            }
            eprintln!(
                "凭据文件 {} 损坏，已从 {} 恢复",
                path.display(),
                backup.display()
            );
            Ok(recovered)
        }
    }
}

fn read_secrets_envelope(path: &Path) -> AppResult<SecretsEnvelope> {
    let raw = fs::read_to_string(path)?;
    let envelope = serde_json::from_str::<SecretsEnvelope>(&raw).map_err(|error| {
        crate::error::AppError::Message(format!(
            "凭据文件 {} 不是受保护的凭据封装：{error}",
            path.display()
        ))
    })?;
    if envelope.version != SECRETS_ENVELOPE_VERSION {
        return Err(crate::error::AppError::Message(format!(
            "不支持的凭据文件版本：{}",
            envelope.version
        )));
    }
    Ok(envelope)
}

#[cfg(any(test, windows))]
fn read_secrets_file(path: &Path, access: SecretAccess) -> AppResult<SecretsData> {
    read_secrets_file_versioned(path, access)
}

fn read_secrets_file_versioned(path: &Path, access: SecretAccess) -> AppResult<SecretsData> {
    let envelope = read_secrets_envelope(path)?;
    let (protection, payload) = match access {
        SecretAccess::User => (envelope.protection.as_str(), envelope.payload.as_str()),
        #[cfg(windows)]
        SecretAccess::Service => (
            envelope.service_protection.as_deref().ok_or_else(|| {
                crate::error::AppError::Message(
                    "Windows Service 凭据镜像尚未准备；请先用当前用户启动新版 Anchor 或重新安装 Windows Service"
                        .into(),
                )
            })?,
            envelope.service_payload.as_deref().ok_or_else(|| {
                crate::error::AppError::Message(
                    "Windows Service 凭据镜像缺少 payload；请先用当前用户启动新版 Anchor 或重新安装 Windows Service"
                        .into(),
                )
            })?,
        ),
    };
    let protected = BASE64_STANDARD.decode(payload).map_err(|error| {
        crate::error::AppError::Message(format!("凭据载荷 Base64 无效：{error}"))
    })?;
    let plaintext = match access {
        SecretAccess::User => secret_protection::unprotect(protection, &protected),
        #[cfg(windows)]
        SecretAccess::Service => secret_protection::unprotect_for_service(protection, &protected),
    }
    .map_err(crate::error::AppError::Message)?;
    parse_secrets_payload(&plaintext)
}

fn parse_secrets_payload(plaintext: &[u8]) -> AppResult<SecretsData> {
    let value: serde_json::Value = serde_json::from_slice(plaintext).map_err(|error| {
        crate::error::AppError::Message(format!("无法解析解密后的凭据文件：{error}"))
    })?;
    let version = value
        .get("schema_version")
        .ok_or_else(|| {
            crate::error::AppError::Message(
                "凭据内容缺少 schema_version；无版本凭据已停止支持，请先使用支持旧格式的 Anchor 完成迁移"
                    .into(),
            )
        })?
        .as_u64()
        .ok_or_else(|| {
            crate::error::AppError::Message("凭据内容 schema_version 必须是非负整数".into())
        })?;
    if version != u64::from(SECRETS_SCHEMA_VERSION) {
        return Err(crate::error::AppError::Message(format!(
            "不支持的凭据内容 schema_version：{version}；当前仅支持 {SECRETS_SCHEMA_VERSION}"
        )));
    }
    let data = serde_json::from_value::<SecretsData>(value).map_err(|error| {
        crate::error::AppError::Message(format!("无法解析解密后的凭据文件：{error}"))
    })?;
    Ok(data)
}

fn ensure_service_secret_mirror(path: &Path, data: &SecretsData) -> AppResult<()> {
    #[cfg(windows)]
    {
        let envelope = read_secrets_envelope(path)?;
        if envelope.service_protection.is_some() && envelope.service_payload.is_some() {
            return Ok(());
        }
        write_service_secrets_data(path, data)?;
    }
    #[cfg(not(windows))]
    let _ = (path, data);
    Ok(())
}

fn current_secret_access() -> SecretAccess {
    #[cfg(windows)]
    {
        if std::env::var_os(crate::brand::WINDOWS_SERVICE_CONTEXT_ENV)
            .is_some_and(|value| !value.is_empty() && value != "0")
        {
            return SecretAccess::Service;
        }
    }
    SecretAccess::User
}

fn write_json<T>(path: &Path, data: &T) -> AppResult<()>
where
    T: Serialize + DeserializeOwned,
{
    let text = serde_json::to_string_pretty(data)?;
    if path.exists() {
        let current = fs::read(path)?;
        if serde_json::from_slice::<T>(&current).is_ok() {
            atomic_write(&backup_path(path), &current)?;
        }
    }
    atomic_write(path, format!("{text}\n").as_bytes())?;
    Ok(())
}

#[cfg(test)]
fn load_with_backup<T>(path: &Path) -> AppResult<T>
where
    T: Serialize + DeserializeOwned,
{
    match read_json(path) {
        Ok(data) => Ok(data),
        Err(primary_error) => {
            let backup = backup_path(path);
            if !backup.exists() {
                return Err(primary_error);
            }
            let recovered = read_json(&backup).map_err(|backup_error| {
                crate::error::AppError::Message(format!(
                    "配置文件损坏且备份无法读取：主文件错误：{primary_error}；备份错误：{backup_error}"
                ))
            })?;
            let text = serde_json::to_string_pretty(&recovered)?;
            atomic_write(path, format!("{text}\n").as_bytes())?;
            eprintln!(
                "配置文件 {} 损坏，已从 {} 恢复",
                path.display(),
                backup.display()
            );
            Ok(recovered)
        }
    }
}

fn load_profiles_with_backup(path: &Path) -> AppResult<ProfilesData> {
    match read_profiles_json_versioned(path) {
        Ok((data, migrated)) => {
            if migrated {
                write_json(path, &data)?;
            }
            Ok(data)
        }
        Err(primary_error) => {
            let backup = backup_path(path);
            if !backup.exists() {
                return Err(primary_error);
            }
            let (recovered, _) = read_profiles_json_versioned(&backup).map_err(|backup_error| {
                crate::error::AppError::Message(format!(
                    "配置文件损坏且备份无法读取：主文件错误：{primary_error}；备份错误：{backup_error}"
                ))
            })?;
            let text = serde_json::to_string_pretty(&recovered)?;
            atomic_write(path, format!("{text}\n").as_bytes())?;
            eprintln!(
                "配置文件 {} 损坏，已从 {} 恢复",
                path.display(),
                backup.display()
            );
            Ok(recovered)
        }
    }
}

#[cfg(test)]
fn read_profiles_json(path: &Path) -> AppResult<ProfilesData> {
    read_profiles_json_versioned(path).map(|(data, _)| data)
}

fn read_profiles_json_versioned(path: &Path) -> AppResult<(ProfilesData, bool)> {
    let raw = fs::read_to_string(path)?;
    let mut value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        crate::error::AppError::Message(format!("无法解析配置文件 {}：{error}", path.display()))
    })?;
    let version = value
        .get("schema_version")
        .ok_or_else(|| {
            crate::error::AppError::Message(format!(
                "配置文件 {} 缺少 schema_version；无版本配置已停止支持，请先使用支持旧格式的 Anchor 完成迁移",
                path.display()
            ))
        })?
        .as_u64()
        .ok_or_else(|| {
            crate::error::AppError::Message(format!(
                "配置文件 {} 的 schema_version 必须是非负整数",
                path.display()
            ))
        })?;
    let migrated = match version {
        current if current == u64::from(PROFILES_SCHEMA_VERSION) => false,
        1 if PROFILES_SCHEMA_VERSION == 2 => {
            migrate_profiles_v1_to_v2(&mut value)?;
            true
        }
        _ => {
            return Err(crate::error::AppError::Message(format!(
                "不支持的配置 schema_version：{version}；当前仅支持 {PROFILES_SCHEMA_VERSION}"
            )));
        }
    };
    let data = serde_json::from_value::<ProfilesData>(value).map_err(|error| {
        crate::error::AppError::Message(format!("无法解析配置文件 {}：{error}", path.display(),))
    })?;
    Ok((data, migrated))
}

fn migrate_profiles_v1_to_v2(value: &mut serde_json::Value) -> AppResult<()> {
    let profiles = value
        .get_mut("profiles")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| {
            crate::error::AppError::Message("v1 配置缺少 profiles 数组，无法迁移".into())
        })?;
    let mut tunnels = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let object = profile.as_object_mut().ok_or_else(|| {
            crate::error::AppError::Message("v1 workspace profile 不是对象，无法迁移".into())
        })?;
        let workspace_id = object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                crate::error::AppError::Message("v1 workspace 缺少 id，无法迁移 tunnel".into())
            })?
            .to_string();
        let workspace_name = object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Workspace")
            .to_string();
        let tunnel = object.remove("tunnel").ok_or_else(|| {
            crate::error::AppError::Message(format!(
                "v1 workspace {workspace_id} 缺少 tunnel 配置，无法迁移"
            ))
        })?;
        let config = serde_json::from_value::<TunnelConfig>(tunnel).map_err(|error| {
            crate::error::AppError::Message(format!(
                "v1 workspace {workspace_id} tunnel 配置无效：{error}"
            ))
        })?;
        tunnels.push(serde_json::to_value(TunnelProfile::migrated(
            workspace_id,
            &workspace_name,
            config,
        ))?);
    }
    if let Some(gateway) = value
        .get_mut("mcp_gateway")
        .and_then(serde_json::Value::as_object_mut)
    {
        let owner = gateway
            .remove("ownerWorkspaceId")
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default();
        let observed_owner = gateway
            .remove("observedOwnerWorkspaceId")
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default();
        gateway.insert(
            "tunnelId".into(),
            serde_json::Value::String(if owner.is_empty() {
                String::new()
            } else {
                format!("{owner}-mcp")
            }),
        );
        gateway.insert(
            "observedTunnelId".into(),
            serde_json::Value::String(if observed_owner.is_empty() {
                String::new()
            } else {
                format!("{observed_owner}-mcp")
            }),
        );
    }
    let root = value
        .as_object_mut()
        .ok_or_else(|| crate::error::AppError::Message("v1 配置根节点不是对象，无法迁移".into()))?;
    root.insert("tunnels".into(), serde_json::Value::Array(tunnels));
    root.insert(
        "schema_version".into(),
        serde_json::Value::from(PROFILES_SCHEMA_VERSION),
    );
    Ok(())
}

#[cfg(test)]
fn read_data(path: &Path) -> AppResult<AppData> {
    read_profiles_json(path).map(ProfilesData::into_app_data)
}

#[cfg(test)]
fn read_json<T>(path: &Path) -> AppResult<T>
where
    T: DeserializeOwned,
{
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw).map_err(|error| {
        crate::error::AppError::Message(format!("无法解析配置文件 {}：{error}", path.display()))
    })
}

fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("profiles.json");
    path.with_file_name(format!("{name}.bak"))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let parent = path.parent().ok_or_else(|| {
        crate::error::AppError::Message(format!("配置路径缺少父目录：{}", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("data");
    let temp_path = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut temp = options.open(&temp_path)?;
    let result = (|| -> AppResult<()> {
        temp.write_all(bytes)?;
        temp.sync_all()?;
        drop(temp);
        replace_file(&temp_path, path)?;
        set_private_permissions(path)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
