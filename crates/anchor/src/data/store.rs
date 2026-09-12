use std::fs::{File, OpenOptions};
use std::sync::{Mutex, MutexGuard};

use fs2::FileExt;

use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::tunnel::TunnelProfile;
use crate::workspace::{WorkspaceProfile, WorkspaceRuntimeContext};

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
    let mut tunnel_ids = std::collections::HashSet::new();
    let mut targets = std::collections::HashSet::new();
    for tunnel in &data.tunnels {
        if tunnel.id.trim().is_empty() {
            return Err(AppError::Message("tunnel id 不能为空".into()));
        }
        if !tunnel_ids.insert(tunnel.id.as_str()) {
            return Err(AppError::Message(format!(
                "duplicate tunnel id: {}",
                tunnel.id
            )));
        }
        if !data
            .profiles
            .iter()
            .any(|workspace| workspace.id == tunnel.workspace_id)
        {
            return Err(AppError::Message(format!(
                "tunnel {} targets missing workspace: {}",
                tunnel.id, tunnel.workspace_id
            )));
        }
        if !tunnel.service.eq_ignore_ascii_case("mcp") {
            return Err(AppError::Message(format!(
                "unsupported tunnel service `{}`; expected mcp",
                tunnel.service
            )));
        }
        let target = format!(
            "{}:{}",
            tunnel.workspace_id,
            tunnel.service.to_ascii_lowercase()
        );
        if !targets.insert(target) {
            return Err(AppError::Message(format!(
                "workspace {} already has an MCP tunnel resource",
                tunnel.workspace_id
            )));
        }
        if !matches!(
            tunnel.config.tunnel_type.as_str(),
            "none" | "frp" | "cloudflare"
        ) {
            return Err(AppError::Message(format!(
                "unsupported tunnel type `{}`",
                tunnel.config.tunnel_type
            )));
        }
    }
    Ok(())
}

fn populate_workspace_secrets(data: &mut AppData, profile_id: &str) {
    let secrets = data
        .workspace_secrets
        .entry(profile_id.to_string())
        .or_default();
    // oauth_client_secret is optional for MCP OAuth (ChatGPT PKCE); not auto-generated.
    for key in ["oauth_password", "oauth_token_secret", "bearer_token"] {
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

    pub fn list_tunnels(&self) -> &[TunnelProfile] {
        &self.data.tunnels
    }

    pub fn get_tunnel(&self, id: &str) -> Option<&TunnelProfile> {
        self.data.tunnels.iter().find(|tunnel| tunnel.id == id)
    }

    pub fn tunnel_for_workspace(&self, workspace_id: &str) -> Option<&TunnelProfile> {
        self.data.tunnel_for_workspace(workspace_id)
    }

    pub fn runtime_context_for(
        &self,
        workspace: &WorkspaceProfile,
    ) -> AppResult<WorkspaceRuntimeContext> {
        WorkspaceRuntimeContext::new(
            workspace.clone(),
            self.tunnel_for_workspace(&workspace.id).cloned(),
        )
    }

    pub fn register_tunnel(&mut self, tunnel: TunnelProfile) -> AppResult<()> {
        if self.data.tunnels.iter().any(|item| item.id == tunnel.id) {
            return Err(AppError::Message(format!(
                "tunnel already exists: {}",
                tunnel.id
            )));
        }
        self.data.tunnels.push(tunnel);
        validate_data(&self.data)?;
        self.save()
    }

    pub fn update_tunnel(&mut self, tunnel: TunnelProfile) -> AppResult<()> {
        let Some(index) = self
            .data
            .tunnels
            .iter()
            .position(|item| item.id == tunnel.id)
        else {
            return Err(AppError::Message(format!(
                "tunnel not found: {}",
                tunnel.id
            )));
        };
        self.data.tunnels[index] = tunnel;
        validate_data(&self.data)?;
        self.save()
    }

    pub fn remove_tunnel(&mut self, id: &str) -> AppResult<Option<TunnelProfile>> {
        let Some(index) = self.data.tunnels.iter().position(|item| item.id == id) else {
            return Ok(None);
        };
        let removed = self.data.tunnels.remove(index);
        if self.data.mcp_gateway.tunnel_id == id {
            self.data.mcp_gateway.enabled = false;
            self.data.mcp_gateway.tunnel_id.clear();
            self.data.mcp_gateway.clear_observation();
        }
        for scope in ["tunnel_frp_token", "tunnel_cloudflare_token"] {
            if let Some(items) = self.data.app_secrets.get_mut(scope) {
                items.remove(id);
                if items.is_empty() {
                    self.data.app_secrets.remove(scope);
                }
            }
        }
        self.save()?;
        Ok(Some(removed))
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
        let tunnel_ids = self
            .data
            .tunnels
            .iter()
            .filter(|tunnel| tunnel.workspace_id == id)
            .map(|tunnel| tunnel.id.clone())
            .collect::<Vec<_>>();
        self.data.tunnels.retain(|tunnel| tunnel.workspace_id != id);
        for scope in ["tunnel_frp_token", "tunnel_cloudflare_token"] {
            if let Some(items) = self.data.app_secrets.get_mut(scope) {
                for tunnel_id in &tunnel_ids {
                    items.remove(tunnel_id);
                }
                if items.is_empty() {
                    self.data.app_secrets.remove(scope);
                }
            }
        }
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
    let process_guard =
        crate::locking::lock_mutex_app(&DATA_FILE_LOCK, "DATA_STORE_LOCK", "configuration data")?;
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
    crate::locking::lock_file_app(&lock_file, "DATA_STORE_LOCK", "configuration data")?;
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
