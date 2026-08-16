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

use super::model::{AppData, ProfilesData, SecretsData};
use super::secret_protection;

const SECRETS_ENVELOPE_VERSION: u32 = 1;
#[cfg(windows)]
const SERVICE_RUNTIME_APP_SECRET_SCOPES: &[&str] = &["oauth_refresh_replay"];

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
        return Ok(load_with_backup::<ProfilesData>(&path)?.into_app_data());
    }
    Ok(AppData::default())
}

fn load_with_secret_access(access: SecretAccess) -> AppResult<AppData> {
    let mut data = load_profiles_only()?;
    load_secrets(&mut data, access)?;
    Ok(data)
}

pub fn save(data: &AppData) -> AppResult<()> {
    let path = data_file_path()?;
    let secrets_path = secrets_file_path()?;
    let secrets = SecretsData::from_app_data(data);
    match current_secret_access() {
        SecretAccess::User => write_secrets_data(&secrets_path, &secrets)?,
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
    match read_secrets_file(path, access) {
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
            let recovered = read_secrets_file(&backup, access).map_err(|backup_error| {
                crate::error::AppError::Message(format!(
                    "凭据文件损坏且备份无法读取：主文件错误：{primary_error}；备份错误：{backup_error}"
                ))
            })?;
            match access {
                SecretAccess::User => write_secrets_data(path, &recovered)?,
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

fn read_secrets_file(path: &Path, access: SecretAccess) -> AppResult<SecretsData> {
    let envelope = read_secrets_envelope(path)?;
    let (protection, payload) = match access {
        SecretAccess::User => (envelope.protection.as_str(), envelope.payload.as_str()),
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
        SecretAccess::Service => secret_protection::unprotect_for_service(protection, &protected),
    }
    .map_err(crate::error::AppError::Message)?;
    serde_json::from_slice::<SecretsData>(&plaintext).map_err(|error| {
        crate::error::AppError::Message(format!("无法解析解密后的凭据文件：{error}"))
    })
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

#[cfg(test)]
fn read_data(path: &Path) -> AppResult<AppData> {
    read_json::<ProfilesData>(path).map(ProfilesData::into_app_data)
}

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

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
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
