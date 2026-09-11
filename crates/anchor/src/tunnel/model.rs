use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TunnelConfig {
    #[serde(rename = "type")]
    pub tunnel_type: String,
    pub public_url: String,
    pub frp_server: String,
    pub frp_subdomain: String,
    pub frp_profile_id: String,
    pub frp_server_port: u16,
    #[serde(default = "default_frp_proxy_type")]
    pub frp_proxy_type: String,
    #[serde(default)]
    pub frp_cert_path: String,
    #[serde(default)]
    pub frp_key_path: String,
    pub cloudflare_mode: String,
    pub use_proxy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TunnelProfile {
    pub id: String,
    pub name: String,
    pub workspace_id: String,
    pub service: String,
    pub enabled: bool,
    #[serde(default)]
    pub revision: u64,
    pub config: TunnelConfig,
}

impl TunnelProfile {
    pub fn new(workspace_id: String, workspace_name: &str, service: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: format!("{workspace_name} {service}"),
            workspace_id,
            service: service.to_string(),
            enabled: false,
            revision: 1,
            config: TunnelConfig::default(),
        }
    }

    pub fn migrated(workspace_id: String, workspace_name: &str, config: TunnelConfig) -> Self {
        Self {
            id: format!("{workspace_id}-mcp"),
            name: format!("{workspace_name} MCP"),
            workspace_id,
            service: "mcp".into(),
            enabled: config.tunnel_type != "none",
            revision: 1,
            config,
        }
    }

    pub fn effective_public_url(&self, settings: &AppSettings) -> String {
        self.config.effective_public_url(settings)
    }
}

impl TunnelConfig {
    pub fn disabled() -> Self {
        Self {
            tunnel_type: "none".into(),
            ..Self::default()
        }
    }

    pub fn effective_public_url(&self, settings: &AppSettings) -> String {
        computed_public_url(
            &self.tunnel_type,
            &self.frp_server,
            &self.frp_subdomain,
            &self.public_url,
            &self.frp_profile_id,
            settings,
        )
    }
}

fn default_frp_server_port() -> u16 {
    7000
}

fn default_frp_proxy_type() -> String {
    "http".to_string()
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            tunnel_type: "cloudflare".into(),
            public_url: String::new(),
            frp_server: String::new(),
            frp_subdomain: String::new(),
            frp_profile_id: String::new(),
            frp_server_port: default_frp_server_port(),
            frp_proxy_type: default_frp_proxy_type(),
            frp_cert_path: String::new(),
            frp_key_path: String::new(),
            cloudflare_mode: "named".into(),
            use_proxy: true,
        }
    }
}

fn computed_public_url(
    tunnel_type: &str,
    frp_server: &str,
    frp_subdomain: &str,
    public_url: &str,
    frp_profile_id: &str,
    settings: &AppSettings,
) -> String {
    if tunnel_type == "frp" {
        let explicit = public_url.trim().trim_end_matches('/');
        if !explicit.is_empty() {
            return explicit.to_string();
        }
        let server = settings
            .find_frp_profile(frp_profile_id)
            .map(|profile| profile.server.as_str())
            .unwrap_or(frp_server);
        if !server.is_empty() && !frp_subdomain.is_empty() {
            return format!("https://{frp_subdomain}.{server}");
        }
    }
    public_url.trim_end_matches('/').to_string()
}
