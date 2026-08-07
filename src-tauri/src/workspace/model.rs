use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProfile {
    pub id: String,
    pub name: String,
    pub path: String,
    pub tunnel: TunnelConfig,
    pub auth: AuthConfig,
    pub runtime: RuntimeConfig,
    pub actions: ActionsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// When true, apply global proxy from Settings → General when starting the tunnel.
    pub use_proxy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub oauth_client_id: String,
    /// Exact OAuth callback URLs registered for this MCP client, one per line.
    pub oauth_redirect_uris: String,
    /// Callback host enrollment allowlist, one host or `*.suffix` per line.
    pub oauth_redirect_hosts: String,
    pub use_shared_secrets: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub local_port: u16,
    pub tool_profile: String,
    pub permission_mode: String,
    pub runtime_command: String,
    /// Optional JSON configuration containing stdio MCP servers to merge into this service.
    pub mcp_config: String,
    /// Workspace execution policy shared by MCP clients.
    pub allowed_commands: String,
    pub workspace_local_entries: bool,
    pub workspace_script_extensions: String,
    /// Expose Agent Skills from configured directories through MCP tools/resources.
    pub skill_service_enabled: bool,
    /// Newline-separated Skill roots. Relative paths resolve from the workspace root.
    pub skill_roots: String,
    /// Runtime-enforced read boundary. It is strict by default; only an
    /// operator-enabled dangerous profile may explicitly opt out.
    pub strict_workspace_reads: bool,
    /// Trusted-control-plane approval for commands classified as external_paid.
    pub external_paid_commands_enabled: bool,
    pub external_paid_max_runs_per_day: u64,
    pub external_paid_max_duration_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionsConfig {
    pub public_url: String,
    pub tunnel_type: String,
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
    pub local_port: u16,
    pub permission_mode: String,
    pub runtime_command: String,
    pub auth_type: String,
    pub oauth_client_id: String,
    /// Exact OAuth callback URLs registered for this Actions client, one per line.
    pub oauth_redirect_uris: String,
    /// Callback host enrollment allowlist, one host or `*.suffix` per line.
    pub oauth_redirect_hosts: String,
    pub oauth_scopes: String,
    pub allowed_commands: String,
    pub max_patch_bytes: u32,
    pub use_shared_secrets: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatusDto {
    pub state: String,
    pub pid: Option<u32>,
    pub local_message: String,
    pub public_message: String,
    pub local_endpoint: String,
    pub public_endpoint: String,
    pub recovery: RuntimeRecoveryDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<McpActivityDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpActivityDto {
    pub state: String,
    pub message: String,
    pub in_flight_requests: u64,
    pub oldest_in_flight_ms: Option<u64>,
    pub last_activity_at: Option<String>,
    pub last_activity_age_ms: Option<u64>,
    pub last_completed_at: Option<String>,
    pub current_method: String,
    pub current_tool: String,
    pub completed_requests: u64,
    pub recent_window_ms: u64,
    pub suspected_stall_after_ms: u64,
    #[serde(default)]
    pub last_transport_activity_at: Option<String>,
    #[serde(default)]
    pub last_transport_activity_age_ms: Option<u64>,
    #[serde(default)]
    pub last_transport_method: String,
    #[serde(default)]
    pub transport_requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRecoveryDto {
    pub enabled: bool,
    pub attempt: u8,
    pub max_attempts: u8,
    pub retry_in_ms: Option<u64>,
    pub recovered_count: u32,
    pub last_error: String,
}

fn default_tunnel_type() -> String {
    "cloudflare".to_string()
}

fn default_new_cloudflare_mode() -> String {
    "named".to_string()
}

fn default_use_proxy() -> bool {
    true
}

fn default_auth_type() -> String {
    "oauth".to_string()
}

fn default_frp_server_port() -> u16 {
    7000
}

fn default_frp_proxy_type() -> String {
    "http".to_string()
}

fn default_actions_auth_type() -> String {
    "api_key".to_string()
}

fn default_actions_oauth_client_id() -> String {
    format!(
        "chatgpt-actions-{}",
        &uuid::Uuid::new_v4().to_string()[..12]
    )
}

fn default_oauth_client_id() -> String {
    format!("chatgpt-client-{}", &uuid::Uuid::new_v4().to_string()[..12])
}

fn default_mcp_port() -> u16 {
    28766
}

fn default_actions_port() -> u16 {
    8787
}

fn default_tool_profile() -> String {
    "core".to_string()
}

fn default_permission_mode() -> String {
    "trusted".to_string()
}

fn default_allowed_commands() -> String {
    "pytest,python,python3,npm,npx,node,pnpm,yarn,make,mvn,mvnw,gradle,gradlew,cargo,go,ruff,mypy,eslint,tsc,git,cmd,powershell,pwsh".to_string()
}

fn default_workspace_local_entries() -> bool {
    true
}

fn default_workspace_script_extensions() -> String {
    ".exe,.bat,.cmd,.ps1".to_string()
}

fn default_skill_service_enabled() -> bool {
    true
}

fn default_strict_workspace_reads() -> bool {
    true
}

fn default_external_paid_max_runs_per_day() -> u64 {
    1
}

fn default_external_paid_max_duration_seconds() -> u64 {
    1800
}

fn default_skill_roots() -> String {
    ".agents/skills\n.codex/skills\nskills".to_string()
}

fn default_max_patch_bytes() -> u32 {
    200_000
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            tunnel_type: default_tunnel_type(),
            public_url: String::new(),
            frp_server: String::new(),
            frp_subdomain: String::new(),
            frp_profile_id: String::new(),
            frp_server_port: default_frp_server_port(),
            frp_proxy_type: default_frp_proxy_type(),
            frp_cert_path: String::new(),
            frp_key_path: String::new(),
            cloudflare_mode: default_new_cloudflare_mode(),
            use_proxy: default_use_proxy(),
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            auth_type: default_auth_type(),
            oauth_client_id: default_oauth_client_id(),
            oauth_redirect_uris: String::new(),
            oauth_redirect_hosts: String::new(),
            use_shared_secrets: false,
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            local_port: default_mcp_port(),
            tool_profile: default_tool_profile(),
            permission_mode: default_permission_mode(),
            runtime_command: String::new(),
            mcp_config: String::new(),
            allowed_commands: default_allowed_commands(),
            workspace_local_entries: default_workspace_local_entries(),
            workspace_script_extensions: default_workspace_script_extensions(),
            skill_service_enabled: default_skill_service_enabled(),
            skill_roots: default_skill_roots(),
            strict_workspace_reads: default_strict_workspace_reads(),
            external_paid_commands_enabled: false,
            external_paid_max_runs_per_day: default_external_paid_max_runs_per_day(),
            external_paid_max_duration_seconds: default_external_paid_max_duration_seconds(),
        }
    }
}

impl Default for ActionsConfig {
    fn default() -> Self {
        Self {
            public_url: String::new(),
            tunnel_type: default_tunnel_type(),
            frp_server: String::new(),
            frp_subdomain: String::new(),
            frp_profile_id: String::new(),
            frp_server_port: default_frp_server_port(),
            frp_proxy_type: default_frp_proxy_type(),
            frp_cert_path: String::new(),
            frp_key_path: String::new(),
            cloudflare_mode: default_new_cloudflare_mode(),
            use_proxy: default_use_proxy(),
            local_port: default_actions_port(),
            permission_mode: default_permission_mode(),
            runtime_command: String::new(),
            auth_type: default_actions_auth_type(),
            oauth_client_id: default_actions_oauth_client_id(),
            oauth_redirect_uris: String::new(),
            oauth_redirect_hosts: String::new(),
            oauth_scopes: String::new(),
            allowed_commands: default_allowed_commands(),
            max_patch_bytes: default_max_patch_bytes(),
            use_shared_secrets: false,
        }
    }
}

impl WorkspaceProfile {
    pub fn new(path: String, name: Option<String>) -> Self {
        let cleaned = path.trim_end_matches(['\\', '/']).to_string();
        let label = name.unwrap_or_else(|| {
            cleaned
                .replace('\\', "/")
                .split('/')
                .next_back()
                .unwrap_or("工作区")
                .to_string()
        });
        Self {
            id: uuid::Uuid::new_v4().to_string().replace('-', ""),
            name: label,
            path: cleaned,
            tunnel: TunnelConfig::default(),
            auth: AuthConfig::default(),
            runtime: RuntimeConfig::default(),
            actions: ActionsConfig::default(),
        }
    }

    pub fn local_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.runtime.local_port)
    }

    pub fn effective_public_url(&self) -> crate::error::AppResult<String> {
        Ok(self.effective_public_url_with(&AppSettings::load()?))
    }

    pub fn effective_public_url_with(&self, settings: &AppSettings) -> String {
        computed_public_url(
            &self.tunnel.tunnel_type,
            &self.tunnel.frp_server,
            &self.tunnel.frp_subdomain,
            &self.tunnel.public_url,
            &self.tunnel.frp_profile_id,
            settings,
        )
    }

    /// External base URL used by this logical MCP server. In gateway mode the
    /// public hostname is shared, while the workspace path remains unique.
    pub fn mcp_external_base_url_with(&self, settings: &AppSettings) -> String {
        if settings.mcp_gateway.enabled {
            let base = settings.mcp_gateway.effective_public_url();
            return format!("{base}/w/{}", self.id);
        }
        self.effective_public_url_with(settings)
    }

    pub fn public_endpoint(&self) -> crate::error::AppResult<String> {
        Ok(self.public_endpoint_with(&AppSettings::load()?))
    }

    pub fn public_endpoint_with(&self, settings: &AppSettings) -> String {
        let base = self.mcp_external_base_url_with(settings);
        if base.is_empty() {
            return String::new();
        }
        format!("{}/mcp", base.trim_end_matches('/'))
    }

    pub fn actions_local_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.actions.local_port)
    }

    pub fn actions_effective_public_url(&self) -> crate::error::AppResult<String> {
        Ok(self.actions_effective_public_url_with(&AppSettings::load()?))
    }

    pub fn actions_effective_public_url_with(&self, settings: &AppSettings) -> String {
        computed_public_url(
            &self.actions.tunnel_type,
            &self.actions.frp_server,
            &self.actions.frp_subdomain,
            &self.actions.public_url,
            &self.actions.frp_profile_id,
            settings,
        )
    }

    pub fn actions_openapi_url(&self) -> crate::error::AppResult<String> {
        Ok(self.actions_openapi_url_with(&AppSettings::load()?))
    }

    pub fn actions_openapi_url_with(&self, settings: &AppSettings) -> String {
        let base = self.actions_public_base_url_with(settings);
        if base.is_empty() {
            return String::new();
        }
        format!("{}/openapi.json", base.trim_end_matches('/'))
    }

    /// Public URL for GPT schema import; falls back to localhost when no tunnel is configured.
    pub fn actions_public_base_url(&self) -> crate::error::AppResult<String> {
        Ok(self.actions_public_base_url_with(&AppSettings::load()?))
    }

    pub fn actions_public_base_url_with(&self, settings: &AppSettings) -> String {
        let public = self.actions_effective_public_url_with(settings);
        if public.is_empty() {
            self.actions_local_base_url()
        } else {
            public
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

#[cfg(test)]
mod tests {
    use super::{ActionsConfig, McpActivityDto, RuntimeConfig, TunnelConfig, WorkspaceProfile};
    use crate::settings::AppSettings;

    #[test]
    fn workspace_defaults_to_stable_cloudflare_named_tunnels() {
        let profile = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));

        assert_eq!(profile.tunnel.tunnel_type, "cloudflare");
        assert_eq!(profile.tunnel.cloudflare_mode, "named");
        assert_eq!(profile.actions.tunnel_type, "cloudflare");
        assert_eq!(profile.actions.cloudflare_mode, "named");
    }

    #[test]
    fn persisted_tunnel_configs_reject_missing_fields() {
        assert!(serde_json::from_value::<TunnelConfig>(serde_json::json!({})).is_err());
        assert!(serde_json::from_value::<ActionsConfig>(serde_json::json!({})).is_err());
    }

    #[test]
    fn explicit_frp_tunnel_type_is_preserved() {
        let mut value = serde_json::to_value(TunnelConfig::default()).expect("default tunnel");
        value["type"] = serde_json::json!("frp");
        let tunnel: TunnelConfig =
            serde_json::from_value(value).expect("explicit FRP tunnel config");

        assert_eq!(tunnel.tunnel_type, "frp");
        assert_eq!(tunnel.cloudflare_mode, "named");
    }

    #[test]
    fn legacy_tunnel_configs_default_to_http_without_certificate_paths() {
        let mut tunnel = serde_json::to_value(TunnelConfig::default()).expect("tunnel config");
        let object = tunnel.as_object_mut().expect("tunnel object");
        object.remove("frp_proxy_type");
        object.remove("frp_cert_path");
        object.remove("frp_key_path");

        let tunnel: TunnelConfig = serde_json::from_value(tunnel).expect("legacy tunnel");
        assert_eq!(tunnel.frp_proxy_type, "http");
        assert!(tunnel.frp_cert_path.is_empty());
        assert!(tunnel.frp_key_path.is_empty());

        let mut actions = serde_json::to_value(ActionsConfig::default()).expect("actions config");
        let object = actions.as_object_mut().expect("actions object");
        object.remove("frp_proxy_type");
        object.remove("frp_cert_path");
        object.remove("frp_key_path");

        let actions: ActionsConfig = serde_json::from_value(actions).expect("legacy actions");
        assert_eq!(actions.frp_proxy_type, "http");
        assert!(actions.frp_cert_path.is_empty());
        assert!(actions.frp_key_path.is_empty());
    }

    #[test]
    fn explicit_frp_public_url_is_not_replaced_by_the_control_address() {
        let mut profile = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));
        profile.tunnel.tunnel_type = "frp".into();
        profile.tunnel.frp_server = "43.157.17.95".into();
        profile.tunnel.frp_subdomain = "anchor".into();
        profile.tunnel.public_url = "https://anchor.taoyan.icu/".into();

        assert_eq!(
            profile.effective_public_url_with(&AppSettings::default()),
            "https://anchor.taoyan.icu"
        );
    }

    #[test]
    fn persisted_runtime_config_rejects_missing_fields() {
        assert!(serde_json::from_value::<RuntimeConfig>(serde_json::json!({})).is_err());
    }

    #[test]
    fn legacy_mcp_activity_payload_defaults_new_transport_fields() {
        let activity: McpActivityDto = serde_json::from_value(serde_json::json!({
            "state": "idle",
            "message": "当前没有在途 MCP 调用",
            "inFlightRequests": 0,
            "oldestInFlightMs": null,
            "lastActivityAt": "2026-08-07T00:00:00.000Z",
            "lastActivityAgeMs": 30_000,
            "lastCompletedAt": "2026-08-07T00:00:00.000Z",
            "currentMethod": "",
            "currentTool": "",
            "completedRequests": 3,
            "recentWindowMs": 15_000,
            "suspectedStallAfterMs": 120_000
        }))
        .expect("legacy MCP activity payload");

        assert_eq!(activity.state, "idle");
        assert_eq!(activity.transport_requests, 0);
        assert!(activity.last_transport_activity_at.is_none());
        assert!(activity.last_transport_activity_age_ms.is_none());
        assert!(activity.last_transport_method.is_empty());
    }

    #[test]
    fn persisted_runtime_config_accepts_large_paid_run_limits() {
        let mut value = serde_json::to_value(RuntimeConfig::default()).expect("runtime config");
        value["external_paid_max_runs_per_day"] = serde_json::json!(5_000_000_000_u64);

        let runtime: RuntimeConfig = serde_json::from_value(value).expect("large paid run limit");

        assert_eq!(runtime.external_paid_max_runs_per_day, 5_000_000_000);
    }
}
