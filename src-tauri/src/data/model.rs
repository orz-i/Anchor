use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::settings::{DownloadConfig, FrpProfile, McpGatewayConfig, ProxyConfig};
use crate::workspace::WorkspaceProfile;

/// In-memory application state. Disk serialization is intentionally handled by
/// `ProfilesData` and `SecretsData` so configuration and secrets cannot be
/// silently mixed again.
#[derive(Debug, Clone, Default)]
pub struct AppData {
    pub frp_profiles: Vec<FrpProfile>,
    pub last_workspace_id: String,
    pub download: DownloadConfig,
    pub proxy: ProxyConfig,
    pub mcp_gateway: McpGatewayConfig,
    pub shared_secrets: HashMap<String, String>,
    pub workspace_secrets: HashMap<String, HashMap<String, String>>,
    pub app_secrets: HashMap<String, HashMap<String, String>>,
    pub profiles: Vec<WorkspaceProfile>,
}

/// Canonical on-disk payload stored in `data/profiles.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfilesData {
    pub frp_profiles: Vec<FrpProfile>,
    pub last_workspace_id: String,
    pub download: DownloadConfig,
    pub proxy: ProxyConfig,
    pub mcp_gateway: McpGatewayConfig,
    pub profiles: Vec<WorkspaceProfile>,
}

impl ProfilesData {
    pub fn from_app_data(data: &AppData) -> Self {
        Self {
            frp_profiles: data.frp_profiles.clone(),
            last_workspace_id: data.last_workspace_id.clone(),
            download: data.download.clone(),
            proxy: data.proxy.clone(),
            mcp_gateway: data.mcp_gateway.clone(),
            profiles: data.profiles.clone(),
        }
    }

    pub fn into_app_data(self) -> AppData {
        AppData {
            frp_profiles: self.frp_profiles,
            last_workspace_id: self.last_workspace_id,
            download: self.download,
            proxy: self.proxy,
            mcp_gateway: self.mcp_gateway,
            profiles: self.profiles,
            ..AppData::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsData {
    pub shared_secrets: HashMap<String, String>,
    pub workspace_secrets: HashMap<String, HashMap<String, String>>,
    pub app_secrets: HashMap<String, HashMap<String, String>>,
}

impl SecretsData {
    pub fn from_app_data(data: &AppData) -> Self {
        Self {
            shared_secrets: data.shared_secrets.clone(),
            workspace_secrets: data.workspace_secrets.clone(),
            app_secrets: data.app_secrets.clone(),
        }
    }

    pub fn apply_to(self, data: &mut AppData) {
        data.shared_secrets = self.shared_secrets;
        data.workspace_secrets = self.workspace_secrets;
        data.app_secrets = self.app_secrets;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_serialization_excludes_runtime_secrets() {
        let mut data = AppData::default();
        data.shared_secrets.insert("token".into(), "secret".into());
        data.workspace_secrets
            .entry("workspace".into())
            .or_default()
            .insert("password".into(), "secret".into());

        let value = serde_json::to_value(ProfilesData::from_app_data(&data))
            .expect("serialize profiles data");

        assert!(value.get("shared_secrets").is_none());
        assert!(value.get("workspace_secrets").is_none());
        assert!(value.get("app_secrets").is_none());
    }

    #[test]
    fn profiles_reject_inline_secret_fields() {
        let error = serde_json::from_value::<ProfilesData>(serde_json::json!({
            "frp_profiles": [],
            "last_workspace_id": "",
            "download": {
                "githubMirror": "https://gh-proxy.com",
                "proxyMode": "system",
                "proxyUrl": ""
            },
            "proxy": {"mode": "system", "url": ""},
            "mcp_gateway": {
                "enabled": false,
                "localPort": 28765,
                "ownerWorkspaceId": "",
                "publicUrl": "",
                "observedPublicUrl": "",
                "observedOwnerWorkspaceId": "",
                "observedTunnelSignature": ""
            },
            "shared_secrets": {"token": "ignored"},
            "profiles": []
        }))
        .expect_err("inline secrets must be rejected");

        assert!(error.to_string().contains("unknown field `shared_secrets`"));
    }
}
