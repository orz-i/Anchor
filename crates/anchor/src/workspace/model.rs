use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;
use crate::tunnel::TunnelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProfile {
    pub id: String,
    pub name: String,
    pub path: String,
    /// Derived runtime projection of the top-level Tunnel resource targeting
    /// this workspace MCP service. It is intentionally excluded from the
    /// workspace persistence/API model; Tunnel is the sole configuration owner.
    #[serde(skip, default = "TunnelConfig::disabled")]
    pub(crate) tunnel: TunnelConfig,
    #[serde(skip, default)]
    pub(crate) tunnel_id: String,
    #[serde(skip, default)]
    pub(crate) tunnel_enabled: bool,
    #[serde(skip, default)]
    pub(crate) tunnel_revision: u64,
    pub auth: AuthConfig,
    pub runtime: RuntimeConfig,
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
    #[serde(default = "default_preferred_shell")]
    pub preferred_shell: String,
    pub runtime_command: String,
    /// Optional JSON configuration containing stdio MCP servers to merge into this service.
    pub mcp_config: String,
    /// Workspace execution policy shared by MCP clients.
    pub allowed_commands: String,
    pub workspace_local_entries: bool,
    pub workspace_script_extensions: String,
    /// Expose installed and active Agent Skill packages through MCP tools/resources.
    pub skill_service_enabled: bool,
    /// Runtime-enforced read boundary. It is strict by default; only an
    /// operator-enabled dangerous profile may explicitly opt out.
    pub strict_workspace_reads: bool,
    /// Trusted-control-plane approval for commands classified as external_paid.
    pub external_paid_commands_enabled: bool,
    pub external_paid_max_runs_per_day: u64,
    pub external_paid_max_duration_seconds: u64,
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

fn default_auth_type() -> String {
    "oauth".to_string()
}

fn default_oauth_client_id() -> String {
    format!("chatgpt-client-{}", &uuid::Uuid::new_v4().to_string()[..12])
}

fn default_mcp_port() -> u16 {
    28766
}

fn default_tool_profile() -> String {
    "core".to_string()
}

fn default_permission_mode() -> String {
    "trusted".to_string()
}

fn default_preferred_shell() -> String {
    "auto".to_string()
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
            preferred_shell: default_preferred_shell(),
            runtime_command: String::new(),
            mcp_config: String::new(),
            allowed_commands: default_allowed_commands(),
            workspace_local_entries: default_workspace_local_entries(),
            workspace_script_extensions: default_workspace_script_extensions(),
            skill_service_enabled: default_skill_service_enabled(),
            strict_workspace_reads: default_strict_workspace_reads(),
            external_paid_commands_enabled: false,
            external_paid_max_runs_per_day: default_external_paid_max_runs_per_day(),
            external_paid_max_duration_seconds: default_external_paid_max_duration_seconds(),
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
            tunnel: TunnelConfig::disabled(),
            tunnel_id: String::new(),
            tunnel_enabled: false,
            tunnel_revision: 0,
            auth: AuthConfig::default(),
            runtime: RuntimeConfig::default(),
        }
    }

    pub fn local_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.runtime.local_port)
    }

    pub fn effective_public_url(&self) -> crate::error::AppResult<String> {
        Ok(self.effective_public_url_with(&AppSettings::load()?))
    }

    pub fn effective_public_url_with(&self, settings: &AppSettings) -> String {
        self.tunnel.effective_public_url(settings)
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
}

#[cfg(test)]
mod tests {
    use super::{McpActivityDto, RuntimeConfig, WorkspaceProfile};
    use crate::settings::AppSettings;
    use crate::tunnel::TunnelConfig;

    #[test]
    fn workspace_defaults_without_owned_tunnel_configuration() {
        let profile = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));

        assert_eq!(profile.tunnel.tunnel_type, "none");
        assert_eq!(profile.tunnel.cloudflare_mode, "named");
    }

    #[test]
    fn persisted_tunnel_configs_reject_missing_fields() {
        assert!(serde_json::from_value::<TunnelConfig>(serde_json::json!({})).is_err());
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
