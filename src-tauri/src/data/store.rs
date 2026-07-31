use std::fs::{File, OpenOptions};
use std::sync::{Mutex, MutexGuard};

use fs2::FileExt;

use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

use super::migrate::{data_file_path, load_or_migrate, maybe_backup_legacy_files, save};
use super::model::AppData;

static DATA_FILE_LOCK: Mutex<()> = Mutex::new(());

struct DataFileGuard {
    _process_guard: MutexGuard<'static, ()>,
    lock_file: File,
}

fn migrate_gateway_url_state(data: &mut AppData) -> bool {
    data.mcp_gateway.migrate_legacy_url_state()
}

fn migrate_tool_profiles(data: &mut AppData) -> bool {
    let mut changed = false;
    for profile in &mut data.profiles {
        let canonical =
            crate::tools::registry::normalize_tool_profile(&profile.runtime.tool_profile);
        if profile.runtime.tool_profile != canonical {
            profile.runtime.tool_profile = canonical.to_string();
            changed = true;
        }
    }
    changed
}

fn normalize_workspace_profile(mut profile: WorkspaceProfile) -> WorkspaceProfile {
    profile.runtime.tool_profile =
        crate::tools::registry::normalize_tool_profile(&profile.runtime.tool_profile).to_string();
    profile
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

const SHARED_KEYS: &[&str] = &[
    "oauth_client_id",
    "bearer_token",
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
    "actions_api_key",
    "actions_oauth_client_secret",
    "actions_oauth_password",
    "actions_oauth_token_secret",
];

#[derive(Debug)]
pub struct DataStore {
    data: AppData,
}

impl DataStore {
    pub fn load() -> AppResult<Self> {
        let _guard = lock_data_file()?;
        let path = data_file_path()?;
        let existed_before = path.exists();
        let mut data = load_or_migrate()?;
        let gateway_migrated = migrate_gateway_url_state(&mut data);
        let tool_profiles_migrated = migrate_tool_profiles(&mut data);
        let store = Self { data };
        if !existed_before || gateway_migrated || tool_profiles_migrated {
            store.persist_unlocked()?;
        }
        if !existed_before {
            maybe_backup_legacy_files(&path)?;
        }
        Ok(store)
    }

    pub fn read_file<R>(f: impl FnOnce(&AppData) -> AppResult<R>) -> AppResult<R> {
        let _guard = lock_data_file()?;
        let mut data = load_or_migrate()?;
        if migrate_gateway_url_state(&mut data) | migrate_tool_profiles(&mut data) {
            save(&data)?;
        }
        f(&data)
    }

    pub fn update_file<R>(f: impl FnOnce(&mut AppData) -> AppResult<R>) -> AppResult<R> {
        let _guard = lock_data_file()?;
        let mut data = load_or_migrate()?;
        migrate_gateway_url_state(&mut data);
        migrate_tool_profiles(&mut data);
        let result = f(&mut data)?;
        save(&data)?;
        Ok(result)
    }

    pub fn data(&self) -> &AppData {
        &self.data
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
        let profile = normalize_workspace_profile(profile);
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
        let profile = normalize_workspace_profile(profile);
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

    pub fn init_shared_secrets(&mut self) -> AppResult<()> {
        let mut changed = false;
        for key in SHARED_KEYS {
            if !self.data.shared_secrets.contains_key(*key) {
                self.data
                    .shared_secrets
                    .insert(key.to_string(), shared_value_for_key(key));
                changed = true;
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
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

    pub fn set_workspace_secret(
        &mut self,
        profile_id: &str,
        key: &str,
        value: &str,
    ) -> AppResult<()> {
        self.data
            .workspace_secrets
            .entry(profile_id.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        self.save()
    }

    pub fn regenerate_workspace_secret(
        &mut self,
        profile_id: &str,
        key: &str,
    ) -> AppResult<String> {
        let value = shared_value_for_key(key);
        self.set_workspace_secret(profile_id, key, &value)?;
        Ok(value)
    }

    pub fn remove_workspace_secrets(&mut self, profile_id: &str) -> AppResult<()> {
        self.data.workspace_secrets.remove(profile_id);
        self.save()
    }

    pub fn get_shared_secret(&self, key: &str) -> Option<String> {
        self.data.shared_secrets.get(key).cloned()
    }

    pub fn set_shared_secret(&mut self, key: &str, value: &str) -> AppResult<()> {
        self.data
            .shared_secrets
            .insert(key.to_string(), value.to_string());
        self.save()
    }

    pub fn regenerate_shared_secret(&mut self, key: &str) -> AppResult<String> {
        let value = random_secret();
        self.set_shared_secret(key, &value)?;
        Ok(value)
    }

    pub fn get_app_secret(&self, scope: &str, item_id: &str) -> Option<String> {
        self.data
            .app_secrets
            .get(scope)
            .and_then(|items| items.get(item_id))
            .filter(|value| !value.is_empty())
            .cloned()
    }

    pub fn set_app_secret(&mut self, scope: &str, item_id: &str, value: &str) -> AppResult<()> {
        self.data
            .app_secrets
            .entry(scope.to_string())
            .or_default()
            .insert(item_id.to_string(), value.to_string());
        self.save()
    }

    pub fn delete_app_secret(&mut self, scope: &str, item_id: &str) -> AppResult<()> {
        if let Some(items) = self.data.app_secrets.get_mut(scope) {
            items.remove(item_id);
            if items.is_empty() {
                self.data.app_secrets.remove(scope);
            }
        }
        self.save()
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

fn shared_value_for_key(key: &str) -> String {
    if key == "oauth_client_id" {
        format!("chatgpt-client-{}", &uuid::Uuid::new_v4().to_string()[..12])
    } else {
        random_secret()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_secret_roundtrip() {
        let id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let mut store = DataStore::load().expect("load");
        store
            .set_workspace_secret(&id, "oauth_client_secret", "roundtrip-secret")
            .expect("set");
        let loaded = store
            .get_workspace_secret(&id, "oauth_client_secret")
            .expect("get");
        assert_eq!(loaded.as_deref(), Some("roundtrip-secret"));
        store.remove_workspace_secrets(&id).expect("remove");
    }

    #[test]
    fn shared_oauth_client_id_uses_client_id_format() {
        let value = shared_value_for_key("oauth_client_id");
        assert!(value.starts_with("chatgpt-client-"));
        assert_eq!(value.len(), "chatgpt-client-".len() + 12);
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
    fn legacy_tool_profiles_are_migrated_to_canonical_values() {
        let mut data = AppData::default();
        data.profiles.push(WorkspaceProfile {
            id: "full-profile".into(),
            name: "full".into(),
            path: ".".into(),
            tunnel: Default::default(),
            auth: Default::default(),
            runtime: crate::workspace::RuntimeConfig {
                tool_profile: "full".into(),
                ..Default::default()
            },
            actions: Default::default(),
        });
        data.profiles.push(WorkspaceProfile {
            id: "compat-profile".into(),
            name: "compat".into(),
            path: ".".into(),
            tunnel: Default::default(),
            auth: Default::default(),
            runtime: crate::workspace::RuntimeConfig {
                tool_profile: "compat-readonly-all".into(),
                ..Default::default()
            },
            actions: Default::default(),
        });

        assert!(migrate_tool_profiles(&mut data));
        assert_eq!(data.profiles[0].runtime.tool_profile, "advanced");
        assert_eq!(data.profiles[1].runtime.tool_profile, "read-only");
        assert!(!migrate_tool_profiles(&mut data));
    }
}
