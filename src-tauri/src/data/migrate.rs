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
use crate::settings::AppSettings;

use super::model::{AppData, LegacyProfilesOnlyFile, SecretsData};
use super::secret_protection;

const LEGACY_PROFILES_FILE: &str = "profiles.json";
const LEGACY_SETTINGS_FILE: &str = "app_settings.json";
const SECRETS_ENVELOPE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct SecretsEnvelope {
    version: u32,
    protection: String,
    payload: String,
}
const LEGACY_BRAND_DATA_FILES: &[&str] = &[
    "data/profiles.json",
    "data/profiles.json.bak",
    "data/secrets.json",
    "data/secrets.json.bak",
    "profiles.json",
    "profiles.json.bak",
    "app_settings.json",
    "app_settings.json.bak",
];
const LEGACY_BRAND_DATA_DIRS: &[&str] = &["bin", "frpc"];
const MAX_LEGACY_BRAND_FILES: usize = 4_096;
const MAX_LEGACY_BRAND_BYTES: u64 = 512 * 1024 * 1024;

pub fn data_file_path() -> AppResult<PathBuf> {
    Ok(platform()
        .app_config_dir()?
        .join("data")
        .join("profiles.json"))
}

fn has_primary_or_backup(path: &Path) -> bool {
    path.exists() || backup_path(path).exists()
}

fn migrate_or_load_secrets(profile_path: &Path, data: &mut AppData) -> AppResult<()> {
    let secrets_path = secrets_file_path()?;
    migrate_or_load_secrets_at(profile_path, &secrets_path, data)
}

fn migrate_or_load_secrets_at(
    profile_path: &Path,
    secrets_path: &Path,
    data: &mut AppData,
) -> AppResult<()> {
    let legacy = SecretsData::from_app_data(data);
    let had_inline_secrets = !legacy.is_empty();

    if has_primary_or_backup(secrets_path) {
        let (loaded, protected) = load_secrets_with_backup(secrets_path)?;
        loaded.apply_to(data);
        if !protected {
            write_secrets_data(secrets_path, &SecretsData::from_app_data(data))?;
        }
    } else if had_inline_secrets {
        write_secrets_data(secrets_path, &legacy)?;
    }

    if had_inline_secrets {
        write_data(profile_path, data)?;
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
        atomic_write(
            &backup_path(&path),
            format!("{}\n", serde_json::to_string_pretty(&data).expect("json")).as_bytes(),
        )
        .expect("backup write");
        fs::write(&path, "{not-json").expect("corrupt primary");

        let recovered = load_with_backup::<AppData>(&path).expect("recover");

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
        atomic_write(
            &backup_path(&path),
            format!("{}\n", serde_json::to_string_pretty(&data).expect("json")).as_bytes(),
        )
        .expect("backup write");

        let recovered = load_with_backup::<AppData>(&path).expect("recover");

        assert_eq!(recovered.last_workspace_id, "backup-only");
        assert!(path.exists());
    }

    #[test]
    fn rejects_invalid_primary_without_backup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("profiles.json");
        fs::write(&path, "{not-json").expect("corrupt primary");

        let error = load_with_backup::<AppData>(&path).expect_err("invalid config must fail");

        assert!(error.to_string().contains("无法解析配置文件"));
    }

    #[test]
    fn migrates_inline_secrets_to_separate_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profile_path = temp.path().join("profiles.json");
        let secrets_path = temp.path().join("secrets.json");
        fs::write(
            &profile_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "shared_secrets": {"token": "shared-secret"},
                "workspace_secrets": {"workspace": {"password": "workspace-secret"}},
                "app_secrets": {},
                "profiles": []
            }))
            .expect("legacy json"),
        )
        .expect("legacy write");
        let mut data = read_data(&profile_path).expect("legacy read");

        migrate_or_load_secrets_at(&profile_path, &secrets_path, &mut data).expect("migrate");

        let secrets = read_secrets_data(&secrets_path).expect("secrets read");
        assert_eq!(
            secrets.shared_secrets.get("token").map(String::as_str),
            Some("shared-secret")
        );
        assert_eq!(
            secrets
                .workspace_secrets
                .get("workspace")
                .and_then(|items| items.get("password"))
                .map(String::as_str),
            Some("workspace-secret")
        );
        let protected_file = fs::read_to_string(&secrets_path).expect("protected secrets read");
        let envelope: SecretsEnvelope =
            serde_json::from_str(&protected_file).expect("protected envelope");
        assert_eq!(envelope.version, SECRETS_ENVELOPE_VERSION);
        assert!(!protected_file.contains("shared-secret"));
        assert!(!protected_file.contains("workspace-secret"));
        let sanitized: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&profile_path).expect("sanitized profile read"),
        )
        .expect("sanitized profile json");
        assert!(sanitized.get("shared_secrets").is_none());
        assert!(sanitized.get("workspace_secrets").is_none());
        assert!(sanitized.get("app_secrets").is_none());
    }

    #[test]
    fn imports_profiles_and_secrets_from_the_legacy_brand_directory() {
        let source = tempfile::tempdir().expect("legacy config");
        let target = tempfile::tempdir().expect("anchor config");
        fs::create_dir_all(source.path().join("data")).expect("legacy data dir");
        fs::write(
            source.path().join("data/profiles.json"),
            "{\"profiles\":[]}",
        )
        .expect("legacy profiles");
        fs::write(
            source.path().join("data/secrets.json"),
            "{\"shared_secrets\":{},\"workspace_secrets\":{},\"app_secrets\":{}}",
        )
        .expect("legacy secrets");
        fs::create_dir_all(source.path().join("bin/downloads")).expect("legacy bin dir");
        fs::write(source.path().join("bin/cloudflared"), "binary").expect("legacy binary");
        fs::write(source.path().join("bin/downloads/archive"), "archive").expect("legacy archive");
        fs::create_dir_all(source.path().join("frpc/workspace")).expect("legacy frpc dir");
        fs::write(
            source.path().join("frpc/workspace/frpc.pid"),
            "42\n/legacy/frpc\n",
        )
        .expect("legacy frpc state");

        assert!(
            migrate_legacy_brand_data_at(source.path(), target.path()).expect("brand migration")
        );
        assert_eq!(
            fs::read_to_string(target.path().join("data/profiles.json"))
                .expect("migrated profiles"),
            "{\"profiles\":[]}"
        );
        assert!(target.path().join("data/secrets.json").is_file());
        assert!(target.path().join("bin/cloudflared").is_file());
        assert!(target.path().join("bin/downloads/archive").is_file());
        assert!(target.path().join("frpc/workspace/frpc.pid").is_file());
        assert!(!migrate_legacy_brand_data_at(source.path(), target.path())
            .expect("idempotent migration"));
    }
}

pub fn load_or_migrate() -> AppResult<AppData> {
    migrate_legacy_brand_data()?;
    let path = data_file_path()?;
    if has_primary_or_backup(&path) {
        let mut data = load_with_backup(&path)?;
        migrate_or_load_secrets(&path, &mut data)?;
        return Ok(data);
    }

    let app_root = platform().app_config_dir()?;
    let mut data = AppData::default();

    let legacy_profiles = app_root.join(LEGACY_PROFILES_FILE);
    if legacy_profiles.exists() {
        let raw = fs::read_to_string(&legacy_profiles)?;
        if let Ok(file) = serde_json::from_str::<LegacyProfilesOnlyFile>(&raw) {
            data.profiles = file.profiles;
        }
    }

    let legacy_settings = app_root.join(LEGACY_SETTINGS_FILE);
    if legacy_settings.exists() {
        let raw = fs::read_to_string(&legacy_settings)?;
        if let Ok(settings) = serde_json::from_str::<AppSettings>(&raw) {
            merge_settings(&mut data, settings);
        }
    }

    Ok(data)
}

fn migrate_legacy_brand_data() -> AppResult<bool> {
    let target_root = platform().app_config_dir()?;
    let target_profiles = target_root.join("data").join("profiles.json");
    let target_secrets = target_root.join("data").join("secrets.json");
    if has_primary_or_backup(&target_profiles) || has_primary_or_backup(&target_secrets) {
        return Ok(false);
    }

    for source_root in platform().legacy_app_config_dirs()? {
        if migrate_legacy_brand_data_at(&source_root, &target_root)? {
            eprintln!(
                "Anchor imported configuration from legacy directory {}",
                source_root.display()
            );
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_legacy_brand_data_at(source_root: &Path, target_root: &Path) -> AppResult<bool> {
    if !source_root.is_dir() || source_root == target_root {
        return Ok(false);
    }
    let mut copied = false;
    for relative in LEGACY_BRAND_DATA_FILES {
        let source = source_root.join(relative);
        let target = target_root.join(relative);
        if !source.is_file() || target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target)?;
        copied = true;
    }

    let mut copied_files = 0_usize;
    let mut copied_bytes = 0_u64;
    for relative in LEGACY_BRAND_DATA_DIRS {
        copied |= copy_legacy_brand_directory(
            &source_root.join(relative),
            &target_root.join(relative),
            &mut copied_files,
            &mut copied_bytes,
        )?;
    }
    Ok(copied)
}

fn copy_legacy_brand_directory(
    source: &Path,
    target: &Path,
    copied_files: &mut usize,
    copied_bytes: &mut u64,
) -> AppResult<bool> {
    if !source.is_dir() {
        return Ok(false);
    }
    let mut copied = false;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if file_type.is_dir() {
            copied |= copy_legacy_brand_directory(
                &source_path,
                &target_path,
                copied_files,
                copied_bytes,
            )?;
            continue;
        }
        if !file_type.is_file() || target_path.exists() {
            continue;
        }
        let bytes = entry.metadata()?.len();
        *copied_files += 1;
        *copied_bytes = copied_bytes.saturating_add(bytes);
        if *copied_files > MAX_LEGACY_BRAND_FILES || *copied_bytes > MAX_LEGACY_BRAND_BYTES {
            return Err(crate::error::AppError::Message(
                "旧版配置迁移超过文件数量或容量上限".into(),
            ));
        }
        fs::create_dir_all(target)?;
        fs::copy(source_path, target_path)?;
        copied = true;
    }
    Ok(copied)
}

pub fn save(data: &AppData) -> AppResult<()> {
    let path = data_file_path()?;
    let secrets_path = secrets_file_path()?;
    write_secrets_data(&secrets_path, &SecretsData::from_app_data(data))?;
    write_data(&path, data)
}

fn write_data(path: &Path, data: &AppData) -> AppResult<()> {
    write_json(path, data)
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

fn load_secrets_with_backup(path: &Path) -> AppResult<(SecretsData, bool)> {
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
            write_secrets_data(path, &recovered.0)?;
            eprintln!(
                "凭据文件 {} 损坏，已从 {} 恢复",
                path.display(),
                backup.display()
            );
            Ok((recovered.0, true))
        }
    }
}

fn read_secrets_file(path: &Path) -> AppResult<(SecretsData, bool)> {
    let raw = fs::read_to_string(path)?;
    if let Ok(envelope) = serde_json::from_str::<SecretsEnvelope>(&raw) {
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
        let data = serde_json::from_slice::<SecretsData>(&plaintext).map_err(|error| {
            crate::error::AppError::Message(format!("无法解析解密后的凭据文件：{error}"))
        })?;
        return Ok((data, true));
    }

    let legacy = serde_json::from_str::<SecretsData>(&raw).map_err(|error| {
        crate::error::AppError::Message(format!(
            "无法解析凭据文件 {}：{error}",
            path.display()
        ))
    })?;
    Ok((legacy, false))
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
    read_json(path)
}

#[cfg(test)]
fn read_secrets_data(path: &Path) -> AppResult<SecretsData> {
    read_secrets_file(path).map(|value| value.0)
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

pub fn maybe_backup_legacy_files(path: &Path) -> AppResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let app_root = platform().app_config_dir()?;
    for name in [LEGACY_PROFILES_FILE, LEGACY_SETTINGS_FILE] {
        let legacy = app_root.join(name);
        if legacy.exists() {
            let backup = app_root.join(format!("{name}.bak"));
            if !backup.exists() {
                let _ = fs::rename(&legacy, &backup);
            }
        }
    }
    Ok(())
}

fn merge_settings(data: &mut AppData, settings: AppSettings) {
    data.frp_profiles = settings.frp_profiles;
    data.last_workspace_id = settings.last_workspace_id;
    data.download = settings.download;
    data.proxy = settings.proxy;
    data.shared_secrets = settings.shared_secrets;
    data.workspace_secrets = settings.workspace_secrets;
    data.app_secrets = settings.app_secrets;
}
