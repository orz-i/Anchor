use std::fs::{File, OpenOptions};
use std::sync::{Mutex, MutexGuard};

use fs2::FileExt;

use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

use super::model::AppData;
#[cfg(windows)]
use super::storage::load_profiles_only as load_profiles_data_only;
use super::storage::{data_file_path, load, save};

static DATA_FILE_LOCK: Mutex<()> = Mutex::new(());

struct DataFileGuard {
    _process_guard: MutexGuard<'static, ()>,
    lock_file: File,
}

pub(crate) fn validate_workspace_profile(profile: &WorkspaceProfile) -> AppResult<()> {
    crate::tools::registry::require_tool_profile(&profile.runtime.tool_profile)
        .map(|_| ())
        .map_err(AppError::Message)?;
    if !matches!(
        profile.runtime.preferred_shell.as_str(),
        "auto" | "pwsh" | "powershell" | "cmd"
    ) {
        return Err(AppError::Message(format!(
            "unsupported preferred shell `{}`; expected auto, pwsh, powershell, or cmd",
            profile.runtime.preferred_shell
        )));
    }
    Ok(())
}

fn validate_data(data: &AppData) -> AppResult<()> {
    for profile in &data.profiles {
        validate_workspace_profile(profile)?;
    }
    Ok(())
}

fn populate_workspace_secrets(data: &mut AppData, profile_id: &str) {
    let secrets = data
        .workspace_secrets
        .entry(profile_id.to_string())
        .or_default();
    // oauth_client_secret is optional for MCP OAuth (ChatGPT PKCE); not auto-generated.
    for key in [
        "oauth_password",
        "oauth_token_secret",
        "bearer_token",
        "actions_api_key",
        "actions_oauth_client_secret",
        "actions_oauth_password",
        "actions_oauth_token_secret",
    ] {
        secrets.entry(key.to_string()).or_insert_with(random_secret);
    }
}

impl Drop for DataFileGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock_file);
    }
}

#[derive(Debug)]
pub struct DataStore {
    data: AppData,
}

impl DataStore {
    pub fn load() -> AppResult<Self> {
        let _guard = lock_data_file()?;
        let path = data_file_path()?;
        let existed_before = path.exists();
        let data = load()?;
        validate_data(&data)?;
        let store = Self { data };
        if !existed_before {
            store.persist_unlocked()?;
        }
        Ok(store)
    }

    /// Load only non-secret configuration. This is intentionally used by the
    /// Windows SCM supervisor when it only needs the desired workspace plan;
    /// LocalSystem must not need to decrypt user-scoped DPAPI secrets just to
    /// decide which daemon processes should exist.
    #[cfg(windows)]
    pub(crate) fn load_profiles_only() -> AppResult<Self> {
        let _guard = lock_data_file()?;
        let data = load_profiles_data_only()?;
        validate_data(&data)?;
        Ok(Self { data })
    }

    pub fn read_file<R>(f: impl FnOnce(&AppData) -> AppResult<R>) -> AppResult<R> {
        let _guard = lock_data_file()?;
        let data = load()?;
        validate_data(&data)?;
        f(&data)
    }

    pub fn update_file<R>(f: impl FnOnce(&mut AppData) -> AppResult<R>) -> AppResult<R> {
        let _guard = lock_data_file()?;
        let mut data = load()?;
        validate_data(&data)?;
        let result = f(&mut data)?;
        validate_data(&data)?;
        save(&data)?;
        Ok(result)
    }

    /// Atomically replace the complete persisted configuration without first
    /// decrypting the destination secrets file. Portable config import needs
    /// this path because a copied Windows DPAPI envelope is intentionally not
    /// decryptable on Linux/macOS (and vice versa).
    pub(crate) fn replace_file(data: AppData) -> AppResult<()> {
        let _guard = lock_data_file()?;
        validate_data(&data)?;
        save(&data)
    }

    pub fn save(&self) -> AppResult<()> {
        let _guard = lock_data_file()?;
        self.persist_unlocked()
    }

    fn persist_unlocked(&self) -> AppResult<()> {
        save(&self.data)
    }

    pub fn settings(&self) -> AppSettings {
        AppSettings::from_data(&self.data)
    }

    pub fn update_settings(&mut self, settings: AppSettings) -> AppResult<()> {
        settings.apply_to(&mut self.data);
        self.save()
    }

    pub fn list(&self) -> &[WorkspaceProfile] {
        &self.data.profiles
    }

    pub fn get(&self, id: &str) -> Option<&WorkspaceProfile> {
        self.data.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn register_workspace(&mut self, profile: WorkspaceProfile) -> AppResult<()> {
        validate_workspace_profile(&profile)?;
        if self.data.profiles.iter().any(|item| item.id == profile.id) {
            return Err(AppError::Message(format!(
                "workspace already exists: {}",
                profile.id
            )));
        }
        populate_workspace_secrets(&mut self.data, &profile.id);
        self.data.profiles.push(profile);
        self.save()
    }

    pub fn update(&mut self, profile: WorkspaceProfile) -> AppResult<()> {
        validate_workspace_profile(&profile)?;
        let Some(index) = self
            .data
            .profiles
            .iter()
            .position(|item| item.id == profile.id)
        else {
            return Err(AppError::Message(format!(
                "workspace not found: {}",
                profile.id
            )));
        };
        self.data.profiles[index] = profile;
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> AppResult<Option<WorkspaceProfile>> {
        let Some(index) = self.data.profiles.iter().position(|item| item.id == id) else {
            return Ok(None);
        };
        let removed = self.data.profiles.remove(index);
        self.data.workspace_secrets.remove(id);
        self.save()?;
        Ok(Some(removed))
    }

    pub fn get_workspace_secret(&self, profile_id: &str, key: &str) -> AppResult<Option<String>> {
        Ok(self
            .data
            .workspace_secrets
            .get(profile_id)
            .and_then(|secrets| secrets.get(key))
            .filter(|value| !value.is_empty())
            .cloned())
    }

    pub fn get_shared_secret(&self, key: &str) -> Option<String> {
        self.data.shared_secrets.get(key).cloned()
    }
}

fn lock_data_file() -> AppResult<DataFileGuard> {
    let process_guard = DATA_FILE_LOCK
        .lock()
        .map_err(|_| AppError::Message("data file lock poisoned".into()))?;
    let data_path = data_file_path()?;
    let parent = data_path
        .parent()
        .ok_or_else(|| AppError::Message(format!("配置路径缺少父目录：{}", data_path.display())))?;
    std::fs::create_dir_all(parent)?;
    let lock_path = parent.join(".profiles.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock_file = options.open(lock_path)?;
    FileExt::lock_exclusive(&lock_file)?;
    Ok(DataFileGuard {
        _process_guard: process_guard,
        lock_file,
    })
}

fn random_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).replace('-', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_secret_lookup_reads_in_memory_state() {
        let id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let mut store = DataStore {
            data: AppData::default(),
        };
        store
            .data
            .workspace_secrets
            .entry(id.clone())
            .or_default()
            .insert("oauth_client_secret".into(), "roundtrip-secret".into());
        let loaded = store
            .get_workspace_secret(&id, "oauth_client_secret")
            .expect("get");
        assert_eq!(loaded.as_deref(), Some("roundtrip-secret"));
    }

    #[test]
    fn workspace_registration_populates_secrets_without_overwriting_existing_values() {
        let mut data = AppData::default();
        data.workspace_secrets
            .entry("workspace".into())
            .or_default()
            .insert("bearer_token".into(), "keep-me".into());

        populate_workspace_secrets(&mut data, "workspace");

        let secrets = &data.workspace_secrets["workspace"];
        assert_eq!(secrets["bearer_token"], "keep-me");
        assert!(secrets.contains_key("oauth_password"));
        assert!(secrets.contains_key("actions_api_key"));
        assert!(!secrets.contains_key("oauth_client_secret"));
    }

    #[test]
    fn invalid_tool_profile_is_rejected_instead_of_normalized() {
        let mut profile = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));
        profile.runtime.tool_profile = "full".into();
        let data = AppData {
            profiles: vec![profile],
            ..AppData::default()
        };

        let error = validate_data(&data).expect_err("invalid profile must fail");
        assert!(error
            .to_string()
            .contains("unsupported tool profile `full`"));
    }
}
