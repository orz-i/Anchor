use serde::{Deserialize, Serialize};
use std::ops::{Deref, DerefMut};

use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::tunnel::TunnelProfile;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProfile {
    pub id: String,
    pub name: String,
    pub path: String,
    pub auth: AuthConfig,
    pub runtime: RuntimeConfig,
}

/// Non-persisted composition used by runtime/control paths that need both the
/// Workspace authority and its optional top-level MCP Tunnel authority.
///
/// The Tunnel remains a first-class `TunnelProfile`; this type never copies
/// Tunnel fields back into `WorkspaceProfile`.
#[derive(Debug, Clone)]
pub struct WorkspaceRuntimeContext {
    pub workspace: WorkspaceProfile,
    pub tunnel: Option<TunnelProfile>,
}

impl WorkspaceRuntimeContext {
    pub fn new(workspace: WorkspaceProfile, tunnel: Option<TunnelProfile>) -> AppResult<Self> {
        if let Some(tunnel) = &tunnel {
            if tunnel.workspace_id != workspace.id || !tunnel.service.eq_ignore_ascii_case("mcp") {
                return Err(AppError::Message(format!(
                    "tunnel {} does not target workspace {} MCP service",
                    tunnel.id, workspace.id
                )));
            }
        }
        Ok(Self { workspace, tunnel })
    }

    pub fn tunnel_profile(&self) -> Option<&TunnelProfile> {
        self.tunnel.as_ref()
    }

    pub fn effective_public_url_with(&self, settings: &AppSettings) -> String {
        self.tunnel
            .as_ref()
            .map(|tunnel| tunnel.effective_public_url(settings))
            .unwrap_or_default()
    }

    pub fn effective_public_url(&self) -> AppResult<String> {
        Ok(self.effective_public_url_with(&AppSettings::load()?))
    }

    /// External base URL used by this logical MCP server. In gateway mode the
    /// public hostname is shared, while the workspace path remains unique.
    pub fn mcp_external_base_url_with(&self, settings: &AppSettings) -> String {
        if settings.mcp_gateway.enabled {
            let base = settings.mcp_gateway.effective_public_url();
            return format!("{base}/w/{}", self.workspace.id);
        }
        self.effective_public_url_with(settings)
    }

    pub fn public_endpoint_with(&self, settings: &AppSettings) -> String {
        let base = self.mcp_external_base_url_with(settings);
        if base.is_empty() {
            return String::new();
        }
        format!("{}/mcp", base.trim_end_matches('/'))
    }
}

impl Deref for WorkspaceRuntimeContext {
    type Target = WorkspaceProfile;

    fn deref(&self) -> &Self::Target {
        &self.workspace
    }
}

impl DerefMut for WorkspaceRuntimeContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.workspace
    }
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
    pub last_transport_activity_at: Option<String>,
    pub last_transport_activity_age_ms: Option<u64>,
    pub last_transport_method: String,
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
            auth: AuthConfig::default(),
            runtime: RuntimeConfig::default(),
        }
    }

    pub fn local_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.runtime.local_port)
    }
}

#[cfg(test)]
mod tests {
    use super::{McpActivityDto, RuntimeConfig, WorkspaceProfile, WorkspaceRuntimeContext};
    use crate::settings::AppSettings;
    use crate::tunnel::{TunnelConfig, TunnelProfile};

    #[test]
    fn workspace_defaults_without_owned_tunnel_configuration() {
        let profile = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));

        let value = serde_json::to_value(&profile).expect("workspace json");
        assert!(value.get("tunnel").is_none());
        assert!(value.get("tunnel_id").is_none());
        assert!(value.get("tunnel_enabled").is_none());
        assert!(value.get("tunnel_revision").is_none());
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
    fn persisted_tunnel_configs_require_current_frp_fields() {
        let mut tunnel = serde_json::to_value(TunnelConfig::default()).expect("tunnel config");
        let object = tunnel.as_object_mut().expect("tunnel object");
        object.remove("frp_proxy_type");
        object.remove("frp_cert_path");
        object.remove("frp_key_path");

        assert!(serde_json::from_value::<TunnelConfig>(tunnel).is_err());
    }

    #[test]
    fn explicit_frp_public_url_is_not_replaced_by_the_control_address() {
        let profile = WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()));
        let mut tunnel = TunnelProfile::new(profile.id.clone(), &profile.name, "mcp");
        tunnel.config.tunnel_type = "frp".into();
        tunnel.config.frp_server = "43.157.17.95".into();
        tunnel.config.frp_subdomain = "anchor".into();
        tunnel.config.public_url = "https://anchor.taoyan.icu/".into();
        let profile = WorkspaceRuntimeContext::new(profile, Some(tunnel)).expect("runtime context");

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
    fn mcp_activity_payload_requires_current_transport_fields() {
        let payload = serde_json::json!({
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
        });

        assert!(serde_json::from_value::<McpActivityDto>(payload).is_err());
    }

    #[test]
    fn persisted_runtime_config_requires_preferred_shell() {
        let mut runtime = serde_json::to_value(RuntimeConfig::default()).expect("runtime config");
        runtime
            .as_object_mut()
            .expect("runtime object")
            .remove("preferred_shell");

        assert!(serde_json::from_value::<RuntimeConfig>(runtime).is_err());
    }

    #[test]
    fn persisted_runtime_config_accepts_large_paid_run_limits() {
        let mut value = serde_json::to_value(RuntimeConfig::default()).expect("runtime config");
        value["external_paid_max_runs_per_day"] = serde_json::json!(5_000_000_000_u64);

        let runtime: RuntimeConfig = serde_json::from_value(value).expect("large paid run limit");

        assert_eq!(runtime.external_paid_max_runs_per_day, 5_000_000_000);
    }
}
