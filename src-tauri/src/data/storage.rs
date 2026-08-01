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

#[derive(Debug, Serialize, Deserialize)]
struct SecretsEnvelope {
    version: u32,
    protection: String,
    payload: String,
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

fn load_secrets(data: &mut AppData) -> AppResult<()> {
    let secrets_path = secrets_file_path()?;
    if has_primary_or_backup(&secrets_path) {
        let loaded = load_secrets_with_backup(&secrets_path)?;
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

        let error =
            load_with_backup::<ProfilesData>(&path).expect_err("invalid config must fail");

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

        let error = read_secrets_file(&secrets_path).expect_err("plaintext secrets must fail");

        assert!(error.to_string().contains("受保护的凭据封装"));
    }

}

pub fn load() -> AppResult<AppData> {
    let path = data_file_path()?;
    if has_primary_or_backup(&path) {
        let mut data = load_with_backup::<ProfilesData>(&path)?.into_app_data();
        load_secrets(&mut data)?;
        return Ok(data);
    }
    Ok(AppData::default())
}

pub fn save(data: &AppData) -> AppResult<()> {
    let path = data_file_path()?;
    let secrets_path = secrets_file_path()?;
    write_secrets_data(&secrets_path, &SecretsData::from_app_data(data))?;
    write_data(&path, data)
}

fn write_data(path: &Path, data: &AppData) -> AppResult<()> {
    write_json(path, &ProfilesData::from_app_data(data))
}

fn write_secrets_data(path: &Path, data: &SecretsData) -> AppResult<()> {
    let plaintext = serde_json::to_vec(data)?;
    let (protection, protected) = secret_protection::protect(&plaintext)
        .map_err(crate::error::AppError::Message)?;
    let envelope = SecretsEnvelope {
        version: SECRETS_ENVELOPE_VERSION,
        protection: protection.into(),
        payload: BASE64_STANDARD.encode(protected),
    };
    write_json(path, &envelope)
}

fn load_secrets_with_backup(path: &Path) -> AppResult<SecretsData> {
    match read_secrets_file(path) {
        Ok(data) => Ok(data),
        Err(primary_error) => {
            let backup = backup_path(path);
            if !backup.exists() {
                return Err(primary_error);
            }
            let recovered = read_secrets_file(&backup).map_err(|backup_error| {
                crate::error::AppError::Message(format!(
                    "凭据文件损坏且备份无法读取：主文件错误：{primary_error}；备份错误：{backup_error}"
                ))
            })?;
            write_secrets_data(path, &recovered)?;
            eprintln!(
                "凭据文件 {} 损坏，已从 {} 恢复",
                path.display(),
                backup.display()
            );
            Ok(recovered)
        }
    }
}

fn read_secrets_file(path: &Path) -> AppResult<SecretsData> {
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
    let protected = BASE64_STANDARD.decode(envelope.payload).map_err(|error| {
        crate::error::AppError::Message(format!("凭据载荷 Base64 无效：{error}"))
    })?;
    let plaintext = secret_protection::unprotect(&envelope.protection, &protected)
        .map_err(crate::error::AppError::Message)?;
    serde_json::from_slice::<SecretsData>(&plaintext).map_err(|error| {
        crate::error::AppError::Message(format!("无法解析解密后的凭据文件：{error}"))
    })
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

fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
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
