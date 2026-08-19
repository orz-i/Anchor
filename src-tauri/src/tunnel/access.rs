use std::sync::LazyLock;

use std::collections::HashSet;

use tokio::sync::Mutex;

use crate::error::{AppError, AppResult};
use crate::runtime::{current_public_url, update_public_url};
use crate::settings::{AppSettings, McpGatewayConfig};
use crate::workspace::WorkspaceProfile;

use super::{TunnelServiceKind, TunnelSupervisor};

static TUNNEL_SUPERVISOR: LazyLock<Mutex<TunnelSupervisor>> =
    LazyLock::new(|| Mutex::new(TunnelSupervisor::new()));

#[derive(Clone)]
struct GatewayTunnelBinding {
    profile: WorkspaceProfile,
    signature: String,
}

static GATEWAY_TUNNEL_BINDING: LazyLock<Mutex<Option<GatewayTunnelBinding>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn supervisor() -> &'static Mutex<TunnelSupervisor> {
    &TUNNEL_SUPERVISOR
}

pub async fn ensure_for_runtime(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<Option<String>> {
    let settings = AppSettings::load()?;
    if kind == TunnelServiceKind::Mcp && settings.mcp_gateway.enabled {
        return Ok(None);
    }
    let tunnel_type = tunnel_type_for(profile, kind);
    if tunnel_type.is_empty() || tunnel_type == "none" {
        return Ok(None);
    }
    let mut guard = supervisor().lock().await;
    let current = guard.status(profile, kind, &settings);
    if current.state == "running" {
        if let Err(error) =
            validate_automatic_recovery_url(profile, kind, &current.public_url, &settings)
        {
            let _ = guard.stop(profile, kind, &settings).await;
            return Err(error);
        }
        publish_listener_url(profile, kind, &current.public_url);
        return Ok(Some(current.public_url));
    }
    let status = guard.start(profile, kind, &settings).await?;
    if let Err(error) =
        validate_automatic_recovery_url(profile, kind, &status.public_url, &settings)
    {
        let _ = guard.stop(profile, kind, &settings).await;
        return Err(error);
    }
    publish_listener_url(profile, kind, &status.public_url);
    Ok(Some(status.public_url))
}

fn tunnel_type_for(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> &str {
    match kind {
        TunnelServiceKind::Mcp => profile.tunnel.tunnel_type.as_str(),
        TunnelServiceKind::Actions => profile.actions.tunnel_type.as_str(),
    }
}

pub async fn maybe_start_for_runtime(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<Option<String>> {
    let settings = AppSettings::load()?;
    if kind == TunnelServiceKind::Mcp && settings.mcp_gateway.enabled {
        return Ok(None);
    }
    let tunnel_type = tunnel_type_for(profile, kind);
    if tunnel_type.is_empty() || tunnel_type == "none" {
        return Ok(None);
    }
    let mut guard = supervisor().lock().await;
    let status = guard.start(profile, kind, &settings).await?;
    publish_listener_url(profile, kind, &status.public_url);
    Ok(Some(status.public_url))
}

const QUICK_TUNNEL_URL_CHANGED: &str = "QUICK_TUNNEL_URL_CHANGED";

pub fn is_quick_tunnel_url_change_error(error: &AppError) -> bool {
    error.to_string().starts_with(QUICK_TUNNEL_URL_CHANGED)
}

fn validate_automatic_recovery_url(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    recovered_url: &str,
    settings: &AppSettings,
) -> AppResult<()> {
    if kind == TunnelServiceKind::Mcp && settings.mcp_gateway.enabled {
        return Ok(());
    }
    if !is_quick_cloudflare(profile, kind) {
        return Ok(());
    }
    let accepted = current_public_url(&profile.id, service_key(kind));
    let configured = accepted
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| configured_public_url(profile, kind))
        .trim()
        .trim_end_matches('/');
    let recovered = recovered_url.trim().trim_end_matches('/');
    if configured.is_empty() || configured == recovered {
        return Ok(());
    }
    Err(AppError::Message(format!(
        "{QUICK_TUNNEL_URL_CHANGED}: Quick Tunnel 临时地址已从 {configured} 变为 {recovered}；旧 ChatGPT 连接无法自动迁移。已停止新隧道，请手动重新启动隧道并更新 ChatGPT MCP 地址，或改用固定 FRP/Named Tunnel 地址。"
    )))
}

fn publish_listener_url(profile: &WorkspaceProfile, kind: TunnelServiceKind, url: &str) {
    update_public_url(&profile.id, service_key(kind), url);
}

fn service_key(kind: TunnelServiceKind) -> &'static str {
    match kind {
        TunnelServiceKind::Mcp => "mcp",
        TunnelServiceKind::Actions => "actions",
    }
}

fn configured_public_url(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> &str {
    match kind {
        TunnelServiceKind::Mcp => &profile.tunnel.public_url,
        TunnelServiceKind::Actions => &profile.actions.public_url,
    }
}

fn is_quick_cloudflare(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> bool {
    match kind {
        TunnelServiceKind::Mcp => {
            profile.tunnel.tunnel_type == "cloudflare" && profile.tunnel.cloudflare_mode == "quick"
        }
        TunnelServiceKind::Actions => {
            profile.actions.tunnel_type == "cloudflare"
                && profile.actions.cloudflare_mode == "quick"
        }
    }
}

pub async fn stop_for_runtime(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<()> {
    let settings = AppSettings::load()?;
    if kind == TunnelServiceKind::Mcp && settings.mcp_gateway.enabled {
        return Ok(());
    }
    let mut guard = supervisor().lock().await;
    guard.stop(profile, kind, &settings).await
}

pub async fn drop_workspace(workspace_id: &str) -> AppResult<()> {
    let mut guard = supervisor().lock().await;
    guard.drop_workspace(workspace_id).await
}

/// Reconcile the one public MCP tunnel used by gateway mode. Existing direct
/// workspace MCP tunnels are stopped before the owner tunnel is pointed at the
/// gateway port. Actions tunnels remain independent.
pub async fn reconcile_mcp_gateway(
    config: &McpGatewayConfig,
    profiles: &[WorkspaceProfile],
    active_workspace_ids: &HashSet<String>,
) -> AppResult<Option<String>> {
    let settings = AppSettings::load()?;
    let mut binding = GATEWAY_TUNNEL_BINDING.lock().await;
    let mut guard = supervisor().lock().await;

    if !config.enabled {
        if let Some(previous) = binding.take() {
            guard
                .stop(&previous.profile, TunnelServiceKind::Mcp, &settings)
                .await?;
        }
        return Ok(None);
    }

    if active_workspace_ids.is_empty() {
        if let Some(previous) = binding.take() {
            guard
                .stop(&previous.profile, TunnelServiceKind::Mcp, &settings)
                .await?;
        }
        for profile in profiles {
            guard
                .stop(profile, TunnelServiceKind::Mcp, &settings)
                .await?;
        }
        return Ok(None);
    }

    crate::mcp::gateway::validate_config(config, profiles)?;
    let owner = profiles
        .iter()
        .find(|profile| profile.id == config.owner_workspace_id)
        .ok_or_else(|| AppError::Message("MCP Gateway 隧道所有者工作区不存在。".into()))?;
    let mut gateway_profile = owner.clone();
    gateway_profile.runtime.local_port = config.local_port;
    if !config.public_url.trim().is_empty() {
        gateway_profile.tunnel.public_url =
            config.public_url.trim().trim_end_matches('/').to_string();
    }
    let signature = crate::mcp::gateway::tunnel_identity_signature(config, owner)?;

    let binding_changed = binding
        .as_ref()
        .is_some_and(|previous| previous.signature != signature);
    if binding_changed {
        if let Some(previous) = binding.take() {
            guard
                .stop(&previous.profile, TunnelServiceKind::Mcp, &settings)
                .await?;
        }
    }

    // Remove direct workspace MCP tunnels. The owner key is also stopped on
    // the first gateway reconciliation so a Cloudflare session cannot retain
    // the old workspace listener port.
    if binding.is_none() {
        for profile in profiles {
            guard
                .stop(profile, TunnelServiceKind::Mcp, &settings)
                .await?;
        }
    }

    let tunnel_type = gateway_profile.tunnel.tunnel_type.as_str();
    if tunnel_type.is_empty() || tunnel_type == "none" {
        *binding = Some(GatewayTunnelBinding {
            profile: gateway_profile,
            signature,
        });
        let base = if config.public_url.trim().is_empty() {
            config.effective_public_url()
        } else {
            config.public_url.trim().trim_end_matches('/').to_string()
        };
        publish_gateway_workspace_urls(&base, active_workspace_ids);
        return Ok(Some(base));
    }

    let status = guard
        .start(&gateway_profile, TunnelServiceKind::Mcp, &settings)
        .await?;
    if let Err(error) =
        validate_gateway_recovery_url(config, &gateway_profile, &signature, &status.public_url)
    {
        let _ = guard
            .stop(&gateway_profile, TunnelServiceKind::Mcp, &settings)
            .await;
        return Err(error);
    }
    let public_url = status.public_url.trim().trim_end_matches('/').to_string();
    publish_gateway_workspace_urls(&public_url, active_workspace_ids);
    *binding = Some(GatewayTunnelBinding {
        profile: gateway_profile,
        signature,
    });
    Ok(Some(public_url))
}

fn validate_gateway_recovery_url(
    config: &McpGatewayConfig,
    profile: &WorkspaceProfile,
    signature: &str,
    recovered_url: &str,
) -> AppResult<()> {
    if profile.tunnel.tunnel_type != "cloudflare" || profile.tunnel.cloudflare_mode != "quick" {
        return Ok(());
    }
    let observed = config.observed_public_url.trim().trim_end_matches('/');
    let configured = config.public_url.trim().trim_end_matches('/');
    let observed_matches = crate::mcp::gateway::observation_matches_tunnel(config, signature);
    let accepted = if observed.is_empty() || !observed_matches {
        configured
    } else {
        observed
    };
    let recovered = recovered_url.trim().trim_end_matches('/');
    if accepted.is_empty() || accepted == recovered {
        return Ok(());
    }
    Err(AppError::Message(format!(
        "{QUICK_TUNNEL_URL_CHANGED}: MCP Gateway Quick Tunnel 临时地址已从 {accepted} 变为 {recovered}；已拒绝静默迁移，请更新 ChatGPT 中所有 Gateway 工作区地址。"
    )))
}

fn publish_gateway_workspace_urls(base_url: &str, active_workspace_ids: &HashSet<String>) {
    let base = base_url.trim().trim_end_matches('/');
    for workspace_id in active_workspace_ids {
        let url = format!("{base}/w/{workspace_id}");
        update_public_url(workspace_id, "mcp", &url);
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::register_public_url;
    use crate::workspace::WorkspaceProfile;

    use super::{
        is_quick_tunnel_url_change_error, validate_automatic_recovery_url,
        validate_gateway_recovery_url, TunnelServiceKind,
    };
    use crate::settings::{AppSettings, McpGatewayConfig};

    fn quick_profile() -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new("C:/workspace/quick".into(), Some("quick".into()));
        profile.tunnel.tunnel_type = "cloudflare".into();
        profile.tunnel.cloudflare_mode = "quick".into();
        profile.tunnel.public_url = "https://old.trycloudflare.com".into();
        profile
    }

    #[test]
    fn automatic_quick_tunnel_recovery_rejects_a_changed_url() {
        let profile = quick_profile();
        let _listener = register_public_url(&profile.id, "mcp", profile.tunnel.public_url.clone());
        assert!(validate_automatic_recovery_url(
            &profile,
            TunnelServiceKind::Mcp,
            "https://old.trycloudflare.com",
            &AppSettings::default()
        )
        .is_ok());
        let error = validate_automatic_recovery_url(
            &profile,
            TunnelServiceKind::Mcp,
            "https://new.trycloudflare.com",
            &AppSettings::default(),
        )
        .expect_err("URL drift");
        assert!(is_quick_tunnel_url_change_error(&error));
    }

    #[test]
    fn explicit_hot_publish_prevents_false_drift_before_profile_persistence() {
        let profile = quick_profile();
        let _listener =
            register_public_url(&profile.id, "mcp", "https://new.trycloudflare.com".into());
        assert!(validate_automatic_recovery_url(
            &profile,
            TunnelServiceKind::Mcp,
            "https://new.trycloudflare.com",
            &AppSettings::default()
        )
        .is_ok());
    }

    #[test]
    fn fixed_tunnels_allow_their_configured_url_to_refresh() {
        let mut profile = quick_profile();
        profile.tunnel.tunnel_type = "frp".into();
        assert!(validate_automatic_recovery_url(
            &profile,
            TunnelServiceKind::Mcp,
            "https://fixed.example.com",
            &AppSettings::default()
        )
        .is_ok());
    }

    #[test]
    fn gateway_quick_tunnel_refuses_silent_public_url_drift() {
        let profile = quick_profile();
        let config = McpGatewayConfig {
            enabled: true,
            local_port: 28765,
            owner_workspace_id: profile.id.clone(),
            public_url: "https://old.trycloudflare.com".into(),
            ..McpGatewayConfig::default()
        };
        let signature = crate::mcp::gateway::tunnel_identity_signature(&config, &profile).unwrap();
        assert!(validate_gateway_recovery_url(
            &config,
            &profile,
            &signature,
            "https://new.trycloudflare.com"
        )
        .is_err());
    }

    #[test]
    fn quick_tunnel_runtime_url_does_not_change_gateway_signature() {
        let mut first = quick_profile();
        first.tunnel.public_url = "https://first.trycloudflare.com".into();
        let mut second = first.clone();
        second.tunnel.public_url = "https://second.trycloudflare.com".into();
        assert_eq!(
            crate::mcp::gateway::tunnel_identity_signature(
                &McpGatewayConfig {
                    owner_workspace_id: first.id.clone(),
                    ..McpGatewayConfig::default()
                },
                &first
            )
            .unwrap(),
            crate::mcp::gateway::tunnel_identity_signature(
                &McpGatewayConfig {
                    owner_workspace_id: second.id.clone(),
                    ..McpGatewayConfig::default()
                },
                &second
            )
            .unwrap()
        );
        second.tunnel.cloudflare_mode = "named".into();
        assert_ne!(
            crate::mcp::gateway::tunnel_identity_signature(
                &McpGatewayConfig {
                    owner_workspace_id: first.id.clone(),
                    ..McpGatewayConfig::default()
                },
                &first
            )
            .unwrap(),
            crate::mcp::gateway::tunnel_identity_signature(
                &McpGatewayConfig {
                    owner_workspace_id: second.id.clone(),
                    ..McpGatewayConfig::default()
                },
                &second
            )
            .unwrap()
        );
    }
}
