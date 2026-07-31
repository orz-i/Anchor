use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::settings::{DownloadConfig, FrpProfile, McpGatewayConfig, ProxyConfig};
use crate::workspace::WorkspaceProfile;

/// Unified on-disk payload stored in `data/profiles.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppData {
    #[serde(default)]
    pub frp_profiles: Vec<FrpProfile>,
    #[serde(default)]
    pub last_workspace_id: String,
    #[serde(default)]
    pub download: DownloadConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub mcp_gateway: McpGatewayConfig,
    #[serde(skip)]
    pub shared_secrets: HashMap<String, String>,
    #[serde(skip)]
    pub workspace_secrets: HashMap<String, HashMap<String, String>>,
    #[serde(skip)]
    pub app_secrets: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub profiles: Vec<WorkspaceProfile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretsData {
    #[serde(default)]
    pub shared_secrets: HashMap<String, String>,
    #[serde(default)]
    pub workspace_secrets: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
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
    fn app_data_serialization_excludes_inline_secrets() {
        let mut data = AppData::default();
        data.shared_secrets.insert("token".into(), "secret".into());
        data.workspace_secrets
            .entry("workspace".into())
            .or_default()
            .insert("password".into(), "secret".into());

        let value = serde_json::to_value(data).expect("serialize app data");

        assert!(value.get("shared_secrets").is_none());
        assert!(value.get("workspace_secrets").is_none());
        assert!(value.get("app_secrets").is_none());
    }

    #[test]
    fn app_data_ignores_inline_secret_fields() {
        let data: AppData = serde_json::from_value(serde_json::json!({
            "shared_secrets": {"token": "ignored"},
            "workspace_secrets": {"workspace": {"password": "ignored"}},
            "app_secrets": {},
            "profiles": []
        }))
        .expect("read current data");

        assert!(data.shared_secrets.is_empty());
        assert!(data.workspace_secrets.is_empty());
        assert!(data.app_secrets.is_empty());
    }
}
