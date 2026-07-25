use std::sync::LazyLock;

use std::collections::HashSet;

use tokio::sync::Mutex;

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::runtime::{current_public_url, update_public_url};
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

use super::{TunnelServiceKind, TunnelSupervisor};

static TUNNEL_SUPERVISOR: LazyLock<Mutex<TunnelSupervisor>> =
    LazyLock::new(|| Mutex::new(TunnelSupervisor::new()));

pub fn supervisor() -> &'static Mutex<TunnelSupervisor> {
    &TUNNEL_SUPERVISOR
}

pub async fn ensure_for_runtime(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<Option<String>> {
    let tunnel_type = tunnel_type_for(profile, kind);
    if tunnel_type.is_empty() || tunnel_type == "none" {
        return Ok(None);
    }
    let settings = AppSettings::load_or_default();
    let mut guard = supervisor().lock().await;
    let current = guard.status(profile, kind, &settings);
    if current.state == "running" {
        if let Err(error) = validate_automatic_recovery_url(profile, kind, &current.public_url) {
            let _ = guard.stop(profile, kind, &settings).await;
            return Err(error);
        }
        publish_listener_url(profile, kind, &current.public_url);
        return Ok(Some(current.public_url));
    }
    let status = guard.start(profile, kind, &settings).await?;
    if let Err(error) = validate_automatic_recovery_url(profile, kind, &status.public_url) {
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
    let tunnel_type = tunnel_type_for(profile, kind);
    if tunnel_type.is_empty() || tunnel_type == "none" {
        return Ok(None);
    }
    let settings = AppSettings::load_or_default();
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
) -> AppResult<()> {
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
    let settings = AppSettings::load_or_default();
    let mut guard = supervisor().lock().await;
    guard.stop(profile, kind, &settings).await
}

pub async fn drop_workspace(workspace_id: &str) -> AppResult<()> {
    let mut guard = supervisor().lock().await;
    guard.drop_workspace(workspace_id).await
}

pub async fn sync_managed_runtime_routes(
    active_runtime_keys: HashSet<(String, TunnelServiceKind)>,
) -> AppResult<()> {
    let settings = AppSettings::load_or_default();
    let profiles = DataStore::read_file(|data| Ok(data.profiles.clone()))?;
    let mut guard = supervisor().lock().await;
    guard.restore_active_frp_routes(&profiles, &active_runtime_keys, &settings);
    Ok(())
}

pub async fn cleanup_orphan_for_runtime(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    runtime_listening: bool,
) -> AppResult<()> {
    let port = match kind {
        TunnelServiceKind::Mcp => profile.runtime.local_port,
        TunnelServiceKind::Actions => profile.actions.local_port,
    };
    if runtime_listening || platform().find_pid_listening_on_port(port)?.is_some() {
        return Ok(());
    }
    let mut guard = supervisor().lock().await;
    // 等待 supervisor 锁期间 runtime 可能已经恢复，再确认一次才允许删除 route。
    if platform().find_pid_listening_on_port(port)?.is_some() {
        return Ok(());
    }
    guard.cleanup_orphan(profile, kind, false).await
}

#[cfg(test)]
mod tests {
    use crate::runtime::register_public_url;
    use crate::workspace::WorkspaceProfile;

    use super::{
        is_quick_tunnel_url_change_error, validate_automatic_recovery_url, TunnelServiceKind,
    };

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
            "https://old.trycloudflare.com"
        )
        .is_ok());
        let error = validate_automatic_recovery_url(
            &profile,
            TunnelServiceKind::Mcp,
            "https://new.trycloudflare.com",
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
            "https://new.trycloudflare.com"
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
            "https://fixed.example.com"
        )
        .is_ok());
    }
}
