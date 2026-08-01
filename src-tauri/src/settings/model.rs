use serde::{Deserialize, Serialize};

use crate::data::AppData;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrpProfile {
    pub id: String,
    pub name: String,
    pub server: String,
    pub server_port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrpProfileInput {
    pub id: String,
    pub name: String,
    pub server: String,
    pub server_port: u16,
}

impl From<FrpProfileInput> for FrpProfile {
    fn from(value: FrpProfileInput) -> Self {
        Self {
            id: value.id,
            name: value.name,
            server: value.server,
            server_port: value.server_port,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGatewayConfig {
    pub enabled: bool,
    pub local_port: u16,
    /// Workspace whose existing MCP tunnel configuration owns the one public
    /// gateway tunnel. This does not grant access to other workspaces.
    pub owner_workspace_id: String,
    /// Optional operator-configured public gateway base URL without a workspace
    /// path. Runtime-discovered tunnel URLs are stored separately.
    pub public_url: String,
    /// Last successfully observed public tunnel URL. This is maintained by the
    /// runtime and must be cleared when the gateway identity changes.
    pub observed_public_url: String,
    pub observed_owner_workspace_id: String,
    pub observed_tunnel_signature: String,
}

impl Default for McpGatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            local_port: default_mcp_gateway_port(),
            owner_workspace_id: String::new(),
            public_url: String::new(),
            observed_public_url: String::new(),
            observed_owner_workspace_id: String::new(),
            observed_tunnel_signature: String::new(),
        }
    }
}

impl McpGatewayConfig {
    pub fn clear_observation(&mut self) {
        self.observed_public_url.clear();
        self.observed_owner_workspace_id.clear();
        self.observed_tunnel_signature.clear();
    }

    pub fn effective_public_url(&self) -> String {
        let observed = self.observed_public_url.trim().trim_end_matches('/');
        let observed_owner_matches = self.observed_owner_workspace_id == self.owner_workspace_id;
        if !observed.is_empty() && observed_owner_matches {
            return observed.to_string();
        }
        let configured = self.public_url.trim().trim_end_matches('/');
        if !configured.is_empty() {
            return configured.to_string();
        }
        format!("http://127.0.0.1:{}", self.local_port)
    }

    pub fn identity_changed(&self, next: &Self) -> bool {
        self.enabled != next.enabled
            || self.local_port != next.local_port
            || self.owner_workspace_id != next.owner_workspace_id
            || self.public_url.trim().trim_end_matches('/')
                != next.public_url.trim().trim_end_matches('/')
    }
}

/// Download settings for fetching frpc / cloudflared binaries.
///
/// GitHub is slow/unreliable from some networks, so downloads try a mirror
/// prefix first (ghproxy-style: `{mirror}/{full_github_url}`) and fall back to
/// the direct GitHub URL. An optional proxy can be layered on top.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadConfig {
    /// Mirror prefix applied before the full GitHub URL. Empty = direct.
    pub github_mirror: String,
    /// "none" (no proxy) | "system" (env HTTP(S)_PROXY) | "manual".
    pub proxy_mode: String,
    /// Proxy URL used when `proxy_mode == "manual"` (e.g. http://127.0.0.1:7890).
    pub proxy_url: String,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            github_mirror: default_github_mirror(),
            proxy_mode: default_proxy_mode(),
            proxy_url: String::new(),
        }
    }
}

/// Global outbound proxy used by network-facing operations such as the
/// Cloudflare quick tunnel. Binary downloads use `download.proxy` separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyConfig {
    /// "none" (no proxy) | "system" (env HTTP(S)_PROXY) | "manual".
    pub mode: String,
    /// Proxy URL used when `mode == "manual"` (e.g. http://127.0.0.1:7890).
    pub url: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            mode: default_proxy_mode(),
            url: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AppSettings {
    pub frp_profiles: Vec<FrpProfile>,
    pub last_workspace_id: String,
    pub download: DownloadConfig,
    /// Global outbound proxy (Cloudflare tunnel, etc.).
    pub proxy: ProxyConfig,
    /// One local MCP gateway can expose multiple workspace listeners through
    /// `/w/<workspace_id>/...` paths and a single public tunnel.
    pub mcp_gateway: McpGatewayConfig,
}

fn default_github_mirror() -> String {
    "https://gh-proxy.com".to_string()
}

fn default_proxy_mode() -> String {
    "system".to_string()
}

fn default_mcp_gateway_port() -> u16 {
    28765
}

impl AppSettings {
    pub fn from_data(data: &AppData) -> Self {
        Self {
            frp_profiles: data.frp_profiles.clone(),
            last_workspace_id: data.last_workspace_id.clone(),
            download: data.download.clone(),
            proxy: data.proxy.clone(),
            mcp_gateway: data.mcp_gateway.clone(),
        }
    }

    pub fn apply_to(&self, data: &mut AppData) {
        data.frp_profiles = self.frp_profiles.clone();
        data.last_workspace_id = self.last_workspace_id.clone();
        data.download = self.download.clone();
        data.proxy = self.proxy.clone();
        data.mcp_gateway = self.mcp_gateway.clone();
    }

    pub fn load_or_default() -> Self {
        crate::data::DataStore::read_file(|data| Ok(Self::from_data(data))).unwrap_or_default()
    }

    pub fn find_frp_profile(&self, id: &str) -> Option<&FrpProfile> {
        if id.trim().is_empty() {
            return None;
        }
        self.frp_profiles.iter().find(|profile| profile.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrpProfile, FrpProfileInput, McpGatewayConfig};

    #[test]
    fn frontend_input_accepts_camel_case_server_port() {
        let profile: FrpProfileInput = serde_json::from_value(serde_json::json!({
            "id": "p1",
            "name": "公司 FRP",
            "server": "frp.example.com",
            "serverPort": 7004
        }))
        .expect("FRP profile should deserialize");

        assert_eq!(profile.server_port, 7004);
    }

    #[test]
    fn persisted_profile_only_accepts_snake_case_server_port() {
        let profile: FrpProfile = serde_json::from_value(serde_json::json!({
            "id": "p1",
            "name": "公司 FRP",
            "server": "frp.example.com",
            "server_port": 7005
        }))
        .expect("persisted FRP profile should deserialize");

        assert_eq!(profile.server_port, 7005);
        assert!(serde_json::from_value::<FrpProfile>(serde_json::json!({
            "id": "p1",
            "name": "公司 FRP",
            "server": "frp.example.com",
            "serverPort": 7004
        }))
        .is_err());
    }

    #[test]
    fn gateway_defaults_to_disabled_reserved_port() {
        let gateway = McpGatewayConfig::default();
        assert!(!gateway.enabled);
        assert_eq!(gateway.local_port, 28765);
        assert!(gateway.owner_workspace_id.is_empty());
        assert!(gateway.observed_public_url.is_empty());
        assert!(gateway.observed_owner_workspace_id.is_empty());
        assert!(gateway.observed_tunnel_signature.is_empty());
    }

}
