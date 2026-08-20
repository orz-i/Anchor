use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

use super::model::{AppData, ProfilesData, SecretsData};
use super::storage::{atomic_write, data_file_path, secrets_file_path};
use super::DataStore;

const PORTABLE_CONFIG_KIND: &str = "anchor-portable-config";
const PORTABLE_CONFIG_VERSION: u32 = 1;
const PORTABLE_KDF: &str = "argon2id-v1";
const PORTABLE_CIPHER: &str = "aes-256-gcm-v1";
const PORTABLE_AAD_PREFIX: &str = "anchor-portable-config-v1";
const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;
const KEY_BYTES: usize = 32;
const MIN_PASSPHRASE_BYTES: usize = 12;
const MAX_PORTABLE_CONFIG_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspacePathMapping {
    pub selector: String,
    pub target: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigExportSummary {
    pub output: String,
    pub workspace_count: usize,
    pub source_platform: String,
    pub source_version: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportedWorkspacePath {
    pub workspace_id: String,
    pub name: String,
    pub source_path: String,
    pub target_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigImportSummary {
    pub input: String,
    pub source_platform: String,
    pub source_version: String,
    pub exported_at: String,
    pub dry_run: bool,
    pub replaced_existing_config: bool,
    pub workspaces: Vec<ImportedWorkspacePath>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableConfigEnvelope {
    kind: String,
    version: u32,
    exported_at: String,
    source_platform: String,
    source_version: String,
    encryption: PortableEncryption,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableEncryption {
    kdf: String,
    cipher: String,
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableConfigPayload {
    profiles: ProfilesData,
    secrets: SecretsData,
}

pub(crate) fn export_portable_config(
    output: &Path,
    passphrase: &[u8],
    force: bool,
) -> AppResult<ConfigExportSummary> {
    validate_passphrase(passphrase)?;
    if output.is_dir() {
        return Err(AppError::Message(format!(
            "导出路径必须是文件，不能是目录：{}",
            output.display()
        )));
    }
    if output.exists() && !force {
        return Err(AppError::Message(format!(
            "导出文件已存在：{}；如需覆盖请使用 --force",
            output.display()
        )));
    }
    validate_existing_parent(output, "导出")?;
    validate_export_destination(output)?;

    let data = DataStore::read_file(|data| Ok(data.clone()))?;
    let envelope = encode_bundle(&data, passphrase)?;
    let bytes = serde_json::to_vec_pretty(&envelope)?;
    if bytes.len() as u64 > MAX_PORTABLE_CONFIG_BYTES {
        return Err(AppError::Message(format!(
            "导出配置超过 {} MiB 上限",
            MAX_PORTABLE_CONFIG_BYTES / 1024 / 1024
        )));
    }
    write_private_atomic(output, &bytes, force)?;

    Ok(ConfigExportSummary {
        output: path_string(output),
        workspace_count: data.profiles.len(),
        source_platform: envelope.source_platform,
        source_version: envelope.source_version,
    })
}

pub(crate) fn import_portable_config(
    input: &Path,
    passphrase: &[u8],
    mappings: &[WorkspacePathMapping],
    dry_run: bool,
    force: bool,
) -> AppResult<ConfigImportSummary> {
    validate_passphrase(passphrase)?;
    let metadata = fs::metadata(input).map_err(|error| {
        AppError::Message(format!("无法读取导入文件 {}：{error}", input.display()))
    })?;
    if !metadata.is_file() {
        return Err(AppError::Message(format!(
            "导入路径必须是普通文件：{}",
            input.display()
        )));
    }
    if metadata.len() > MAX_PORTABLE_CONFIG_BYTES {
        return Err(AppError::Message(format!(
            "导入文件超过 {} MiB 上限",
            MAX_PORTABLE_CONFIG_BYTES / 1024 / 1024
        )));
    }

    let raw = fs::read(input)?;
    let envelope: PortableConfigEnvelope = serde_json::from_slice(&raw)
        .map_err(|error| AppError::Message(format!("导入文件不是有效的 Anchor 迁移包：{error}")))?;
    let data = decode_bundle(&envelope, passphrase)?;
    let (data, workspaces, warnings) =
        prepare_import_data(data, mappings, &envelope.source_platform)?;

    let existing = destination_config_exists()?;
    if existing && !dry_run && !force {
        return Err(AppError::Message(
            "目标配置已存在；为避免覆盖现有配置，请先备份并使用 --force，或先使用 --dry-run 检查迁移结果"
                .into(),
        ));
    }
    if !dry_run {
        DataStore::replace_file(data)?;
    }

    Ok(ConfigImportSummary {
        input: path_string(input),
        source_platform: envelope.source_platform,
        source_version: envelope.source_version,
        exported_at: envelope.exported_at,
        dry_run,
        replaced_existing_config: existing && !dry_run,
        workspaces,
        warnings,
    })
}

fn encode_bundle(data: &AppData, passphrase: &[u8]) -> AppResult<PortableConfigEnvelope> {
    validate_passphrase(passphrase)?;
    let exported_at = chrono::Utc::now().to_rfc3339();
    let source_platform = std::env::consts::OS.to_string();
    let source_version = env!("CARGO_PKG_VERSION").to_string();
    let aad = portable_aad(
        PORTABLE_CONFIG_KIND,
        PORTABLE_CONFIG_VERSION,
        &exported_at,
        &source_platform,
        &source_version,
    );
    let payload = PortableConfigPayload {
        profiles: ProfilesData::from_app_data(data),
        secrets: SecretsData::from_app_data(data),
    };
    let plaintext = serde_json::to_vec(&payload)?;

    let mut salt = [0_u8; SALT_BYTES];
    let mut nonce = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut salt)
        .map_err(|error| AppError::Message(format!("生成迁移加密 salt 失败：{error}")))?;
    getrandom::fill(&mut nonce)
        .map_err(|error| AppError::Message(format!("生成迁移加密 nonce 失败：{error}")))?;

    let key = derive_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| AppError::Message("初始化迁移加密器失败".into()))?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| AppError::Message("迁移配置加密失败".into()))?;

    Ok(PortableConfigEnvelope {
        kind: PORTABLE_CONFIG_KIND.into(),
        version: PORTABLE_CONFIG_VERSION,
        exported_at,
        source_platform,
        source_version,
        encryption: PortableEncryption {
            kdf: PORTABLE_KDF.into(),
            cipher: PORTABLE_CIPHER.into(),
            salt: BASE64_STANDARD.encode(salt),
            nonce: BASE64_STANDARD.encode(nonce),
            ciphertext: BASE64_STANDARD.encode(ciphertext),
        },
    })
}

fn decode_bundle(envelope: &PortableConfigEnvelope, passphrase: &[u8]) -> AppResult<AppData> {
    validate_passphrase(passphrase)?;
    if envelope.kind != PORTABLE_CONFIG_KIND || envelope.version != PORTABLE_CONFIG_VERSION {
        return Err(AppError::Message(format!(
            "不支持的 Anchor 迁移包：kind={} version={}",
            envelope.kind, envelope.version
        )));
    }
    if envelope.encryption.kdf != PORTABLE_KDF || envelope.encryption.cipher != PORTABLE_CIPHER {
        return Err(AppError::Message(format!(
            "不支持的迁移包加密方式：kdf={} cipher={}",
            envelope.encryption.kdf, envelope.encryption.cipher
        )));
    }

    let salt = decode_fixed::<SALT_BYTES>(&envelope.encryption.salt, "salt")?;
    let nonce = decode_fixed::<NONCE_BYTES>(&envelope.encryption.nonce, "nonce")?;
    let ciphertext = BASE64_STANDARD
        .decode(&envelope.encryption.ciphertext)
        .map_err(|error| AppError::Message(format!("迁移包 ciphertext Base64 无效：{error}")))?;
    let key = derive_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| AppError::Message("初始化迁移解密器失败".into()))?;
    let aad = portable_aad(
        &envelope.kind,
        envelope.version,
        &envelope.exported_at,
        &envelope.source_platform,
        &envelope.source_version,
    );
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| {
            AppError::Message("迁移包解密失败：passphrase 错误，或文件已损坏/被篡改".into())
        })?;
    let payload: PortableConfigPayload = serde_json::from_slice(&plaintext)
        .map_err(|error| AppError::Message(format!("迁移包解密后的配置无效：{error}")))?;
    let mut data = payload.profiles.into_app_data();
    payload.secrets.apply_to(&mut data);
    Ok(data)
}

fn portable_aad(
    kind: &str,
    version: u32,
    exported_at: &str,
    source_platform: &str,
    source_version: &str,
) -> Vec<u8> {
    format!(
        "{PORTABLE_AAD_PREFIX}\0{kind}\0{version}\0{exported_at}\0{source_platform}\0{source_version}"
    )
    .into_bytes()
}

fn derive_key(passphrase: &[u8], salt: &[u8; SALT_BYTES]) -> AppResult<[u8; KEY_BYTES]> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        Some(KEY_BYTES),
    )
    .map_err(|error| AppError::Message(format!("初始化迁移 KDF 参数失败：{error}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0_u8; KEY_BYTES];
    argon2
        .hash_password_into(passphrase, salt, &mut key)
        .map_err(|error| AppError::Message(format!("迁移 passphrase 派生密钥失败：{error}")))?;
    Ok(key)
}

fn decode_fixed<const N: usize>(value: &str, label: &str) -> AppResult<[u8; N]> {
    let decoded = BASE64_STANDARD
        .decode(value)
        .map_err(|error| AppError::Message(format!("迁移包 {label} Base64 无效：{error}")))?;
    decoded.try_into().map_err(|bytes: Vec<u8>| {
        AppError::Message(format!(
            "迁移包 {label} 长度无效：期望 {N} bytes，实际 {} bytes",
            bytes.len()
        ))
    })
}

fn prepare_import_data(
    mut data: AppData,
    mappings: &[WorkspacePathMapping],
    source_platform: &str,
) -> AppResult<(AppData, Vec<ImportedWorkspacePath>, Vec<String>)> {
    validate_unique_workspace_ids(&data)?;
    let resolved = resolve_workspace_mappings(&data, mappings)?;
    let mut used_targets = HashSet::new();
    let mut workspaces = Vec::with_capacity(data.profiles.len());
    let mut warnings = Vec::new();

    for (index, profile) in data.profiles.iter_mut().enumerate() {
        let source_path = profile.path.clone();
        let requested = resolved
            .get(&index)
            .cloned()
            .unwrap_or_else(|| PathBuf::from(&source_path));
        let canonical = canonical_workspace_directory(&requested, &profile.name)?;
        if !used_targets.insert(canonical.clone()) {
            return Err(AppError::Message(format!(
                "多个 workspace 映射到同一目录：{}",
                canonical.display()
            )));
        }
        let target_path = path_string(&canonical);
        remap_known_profile_paths(
            profile,
            &source_path,
            &canonical,
            source_platform,
            &mut warnings,
        );
        profile.path = target_path.clone();
        super::validate_workspace_profile(profile)?;
        collect_portability_warnings(profile, &source_path, source_platform, &mut warnings);
        workspaces.push(ImportedWorkspacePath {
            workspace_id: profile.id.clone(),
            name: profile.name.clone(),
            source_path,
            target_path,
        });
    }

    Ok((data, workspaces, warnings))
}

fn resolve_workspace_mappings(
    data: &AppData,
    mappings: &[WorkspacePathMapping],
) -> AppResult<HashMap<usize, PathBuf>> {
    let mut resolved = HashMap::new();
    for mapping in mappings {
        let selector = mapping.selector.trim();
        if selector.is_empty() {
            return Err(AppError::Message(
                "--workspace-path 的 selector 不能为空".into(),
            ));
        }
        let index = resolve_workspace_selector(&data.profiles, selector)?;
        if resolved.insert(index, mapping.target.clone()).is_some() {
            return Err(AppError::Message(format!(
                "workspace {} 被重复指定 --workspace-path",
                data.profiles[index].name
            )));
        }
    }
    Ok(resolved)
}

fn resolve_workspace_selector(
    profiles: &[crate::workspace::WorkspaceProfile],
    selector: &str,
) -> AppResult<usize> {
    if let Some(index) = profiles.iter().position(|profile| profile.id == selector) {
        return Ok(index);
    }
    if let Some(index) = profiles.iter().position(|profile| profile.path == selector) {
        return Ok(index);
    }
    let matches = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| profile.name == selector)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(AppError::Message(format!(
            "--workspace-path 找不到 workspace：{selector}"
        ))),
        _ => Err(AppError::Message(format!(
            "--workspace-path 的 workspace 名称不唯一：{selector}；请改用 workspace ID 或原始路径"
        ))),
    }
}

fn canonical_workspace_directory(path: &Path, name: &str) -> AppResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(AppError::Message(format!(
            "workspace {name} 的目标目录不能为空"
        )));
    }
    if !path.is_absolute() {
        return Err(AppError::Message(format!(
            "workspace {name} 的目标目录必须是绝对路径：{}",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(|error| {
        AppError::Message(format!(
            "workspace {name} 的目标目录不存在或无法访问：{}（{error}）",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(AppError::Message(format!(
            "workspace {name} 的目标路径不是目录：{}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn validate_unique_workspace_ids(data: &AppData) -> AppResult<()> {
    let mut ids = HashSet::new();
    for profile in &data.profiles {
        if profile.id.trim().is_empty() {
            return Err(AppError::Message(format!(
                "迁移包中的 workspace {} 缺少 ID",
                profile.name
            )));
        }
        if !ids.insert(profile.id.as_str()) {
            return Err(AppError::Message(format!(
                "迁移包包含重复 workspace ID：{}",
                profile.id
            )));
        }
    }
    Ok(())
}

fn remap_known_profile_paths(
    profile: &mut crate::workspace::WorkspaceProfile,
    source_root: &str,
    target_root: &Path,
    source_platform: &str,
    warnings: &mut Vec<String>,
) {
    for (label, value) in [
        ("MCP FRP 证书", &mut profile.tunnel.frp_cert_path),
        ("MCP FRP 私钥", &mut profile.tunnel.frp_key_path),
        ("Actions FRP 证书", &mut profile.actions.frp_cert_path),
        ("Actions FRP 私钥", &mut profile.actions.frp_key_path),
    ] {
        if let Some(remapped) =
            remap_path_under_workspace(value, source_root, target_root, source_platform)
        {
            *value = path_string(&remapped);
        }
        if !value.trim().is_empty() {
            let candidate = PathBuf::from(value.trim());
            let resolved = if candidate.is_absolute() {
                candidate
            } else {
                target_root.join(candidate)
            };
            if !resolved.is_file() {
                warnings.push(format!(
                    "workspace {} 的 {label} 在目标平台不存在：{}",
                    profile.name,
                    resolved.display()
                ));
            }
        }
    }

    let remapped_roots = profile
        .runtime
        .skill_roots
        .lines()
        .map(|line| {
            remap_path_under_workspace(line, source_root, target_root, source_platform)
                .map(|path| path_string(&path))
                .unwrap_or_else(|| line.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");
    profile.runtime.skill_roots = remapped_roots;
}

fn remap_path_under_workspace(
    value: &str,
    source_root: &str,
    target_root: &Path,
    source_platform: &str,
) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() || source_root.trim().is_empty() {
        return None;
    }
    let source = normalize_portable_path(source_root);
    let candidate = normalize_portable_path(value);
    let case_insensitive = source_platform.eq_ignore_ascii_case("windows");
    let (source_cmp, candidate_cmp) = if case_insensitive {
        (source.to_ascii_lowercase(), candidate.to_ascii_lowercase())
    } else {
        (source.clone(), candidate.clone())
    };
    let suffix = if candidate_cmp == source_cmp {
        ""
    } else {
        let prefix = format!("{}/", source_cmp.trim_end_matches('/'));
        candidate_cmp.strip_prefix(&prefix)?;
        &candidate[source.len().saturating_add(1)..]
    };
    let mut target = target_root.to_path_buf();
    for segment in suffix.split('/').filter(|segment| !segment.is_empty()) {
        target.push(segment);
    }
    Some(target)
}

fn normalize_portable_path(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

fn collect_portability_warnings(
    profile: &crate::workspace::WorkspaceProfile,
    source_path: &str,
    source_platform: &str,
    warnings: &mut Vec<String>,
) {
    if profile.tunnel.tunnel_type == "cloudflare" && profile.tunnel.cloudflare_mode == "quick" {
        warnings.push(format!(
            "workspace {} 的 MCP 使用 Cloudflare quick tunnel；URL 可能变化，无法保证复用 ChatGPT 中原有注册入口",
            profile.name
        ));
    }
    if profile.actions.tunnel_type == "cloudflare" && profile.actions.cloudflare_mode == "quick" {
        warnings.push(format!(
            "workspace {} 的 Actions 使用 Cloudflare quick tunnel；URL 可能变化，无法保证复用原有注册入口",
            profile.name
        ));
    }
    if source_platform != std::env::consts::OS {
        let normalized_source = normalize_portable_path(source_path);
        if !profile.runtime.runtime_command.trim().is_empty()
            && normalize_portable_path(&profile.runtime.runtime_command)
                .contains(&normalized_source)
        {
            warnings.push(format!(
                "workspace {} 的 runtime_command 可能包含源平台绝对路径，请在导入后复核",
                profile.name
            ));
        }
        if !profile.runtime.mcp_config.trim().is_empty()
            && normalize_portable_path(&profile.runtime.mcp_config).contains(&normalized_source)
        {
            warnings.push(format!(
                "workspace {} 的 mcp_config 可能包含源平台绝对路径，请在导入后复核",
                profile.name
            ));
        }
    }
}

fn validate_passphrase(passphrase: &[u8]) -> AppResult<()> {
    if passphrase.len() < MIN_PASSPHRASE_BYTES {
        return Err(AppError::Message(format!(
            "迁移 passphrase 至少需要 {MIN_PASSPHRASE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_existing_parent(path: &Path, label: &str) -> AppResult<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(AppError::Message(format!(
            "{label}路径的父目录不存在或不是目录：{}",
            parent.display()
        )));
    }
    Ok(())
}

fn validate_export_destination(path: &Path) -> AppResult<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|error| {
            AppError::Message(format!(
                "无法解析导出路径的父目录 {}：{error}",
                path.display()
            ))
        })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| AppError::Message(format!("导出路径缺少文件名：{}", path.display())))?;
    let candidate = parent.join(file_name);

    let profiles = data_file_path()?;
    let secrets = secrets_file_path()?;
    let mut reserved = vec![profiles.clone(), secrets.clone()];
    for config_path in [&profiles, &secrets] {
        if let Some(name) = config_path.file_name().and_then(|value| value.to_str()) {
            reserved.push(config_path.with_file_name(format!("{name}.bak")));
        }
    }
    if let Some(data_dir) = profiles.parent() {
        reserved.push(data_dir.join(".profiles.lock"));
    }

    for reserved_path in reserved {
        let Some(reserved_parent) = reserved_path.parent() else {
            continue;
        };
        let Ok(reserved_parent) = reserved_parent.canonicalize() else {
            continue;
        };
        let Some(reserved_name) = reserved_path.file_name() else {
            continue;
        };
        if candidate == reserved_parent.join(reserved_name) {
            return Err(AppError::Message(format!(
                "导出路径不能覆盖 Anchor 当前配置、备份或锁文件：{}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn destination_config_exists() -> AppResult<bool> {
    Ok(data_file_path()?.exists() || secrets_file_path()?.exists())
}

fn write_private_atomic(path: &Path, bytes: &[u8], force: bool) -> AppResult<()> {
    validate_existing_parent(path, "导出")?;
    if path.exists() && !force {
        return Err(AppError::Message(format!(
            "导出文件已存在：{}；如需覆盖请使用 --force",
            path.display()
        )));
    }
    let mut content = Vec::with_capacity(bytes.len() + 1);
    content.extend_from_slice(bytes);
    content.push(b'\n');
    atomic_write(path, &content)
}

fn path_string(path: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceProfile;

    fn sample_data(path: String) -> AppData {
        let mut profile = WorkspaceProfile::new(path, Some("demo".into()));
        profile.id = "stable-workspace-id".into();
        profile.auth.oauth_client_id = "chatgpt-client-stable".into();
        profile.actions.oauth_client_id = "chatgpt-actions-stable".into();
        let mut data = AppData {
            profiles: vec![profile],
            last_workspace_id: "stable-workspace-id".into(),
            ..AppData::default()
        };
        data.workspace_secrets
            .entry("stable-workspace-id".into())
            .or_default()
            .insert("bearer_token".into(), "portable-secret".into());
        data
    }

    #[test]
    fn encrypted_bundle_round_trip_preserves_registration_identity_and_secrets() {
        let data = sample_data("C:/work/demo".into());
        let envelope = encode_bundle(&data, b"migration-passphrase").expect("encode");
        let decoded = decode_bundle(&envelope, b"migration-passphrase").expect("decode");

        assert_eq!(decoded.profiles[0].id, "stable-workspace-id");
        assert_eq!(
            decoded.profiles[0].auth.oauth_client_id,
            "chatgpt-client-stable"
        );
        assert_eq!(
            decoded.profiles[0].actions.oauth_client_id,
            "chatgpt-actions-stable"
        );
        assert_eq!(
            decoded.workspace_secrets["stable-workspace-id"]["bearer_token"],
            "portable-secret"
        );
        let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
        assert!(!serialized.contains("portable-secret"));
        assert!(!serialized.contains("chatgpt-client-stable"));
    }

    #[test]
    fn encrypted_bundle_rejects_wrong_passphrase() {
        let data = sample_data("C:/work/demo".into());
        let envelope = encode_bundle(&data, b"migration-passphrase").expect("encode");
        let error = decode_bundle(&envelope, b"different-passphrase")
            .expect_err("wrong passphrase must fail");
        assert!(error.to_string().contains("解密失败"));
    }

    #[test]
    fn encrypted_bundle_authenticates_source_metadata() {
        let data = sample_data("C:/work/demo".into());
        let mut envelope = encode_bundle(&data, b"migration-passphrase").expect("encode");
        envelope.source_platform = "windows".into();
        let error = decode_bundle(&envelope, b"migration-passphrase")
            .expect_err("tampered source metadata must fail authentication");
        assert!(error.to_string().contains("解密失败"));
    }

    #[test]
    fn windows_workspace_mapping_rewrites_known_paths_and_preserves_ids() {
        let target = tempfile::tempdir().expect("target workspace");
        let cert_dir = target.path().join(".anchor").join("cert");
        fs::create_dir_all(&cert_dir).expect("cert dir");
        fs::write(cert_dir.join("server.pem"), "cert").expect("cert");
        fs::write(cert_dir.join("server.key"), "key").expect("key");

        let mut data = sample_data(r"D:\projects\demo".into());
        data.profiles[0].tunnel.frp_cert_path = r"D:\projects\demo\.anchor\cert\server.pem".into();
        data.profiles[0].tunnel.frp_key_path = r"D:\projects\demo\.anchor\cert\server.key".into();
        data.profiles[0].runtime.skill_roots = r"D:\projects\demo\.agents\skills
.codex/skills"
            .into();
        let mapping = WorkspacePathMapping {
            selector: "stable-workspace-id".into(),
            target: target.path().to_path_buf(),
        };

        let (mapped, workspaces, warnings) =
            prepare_import_data(data, &[mapping], "windows").expect("prepare import");
        let profile = &mapped.profiles[0];
        assert_eq!(profile.id, "stable-workspace-id");
        assert_eq!(
            profile.path,
            path_string(&target.path().canonicalize().unwrap())
        );
        assert_eq!(
            PathBuf::from(&profile.tunnel.frp_cert_path),
            target
                .path()
                .join(".anchor")
                .join("cert")
                .join("server.pem")
        );
        assert_eq!(
            PathBuf::from(&profile.tunnel.frp_key_path),
            target
                .path()
                .join(".anchor")
                .join("cert")
                .join("server.key")
        );
        assert!(profile
            .runtime
            .skill_roots
            .lines()
            .next()
            .unwrap()
            .contains(".agents"));
        assert_eq!(workspaces[0].source_path, r"D:\projects\demo");
        assert!(warnings.is_empty());
    }

    #[test]
    fn import_requires_existing_absolute_workspace_directory() {
        let data = sample_data(r"D:\projects\demo".into());
        let mapping = WorkspacePathMapping {
            selector: "stable-workspace-id".into(),
            target: PathBuf::from("relative/path"),
        };
        let error = prepare_import_data(data, &[mapping], "windows")
            .expect_err("relative target must fail");
        assert!(error.to_string().contains("必须是绝对路径"));
    }
}
