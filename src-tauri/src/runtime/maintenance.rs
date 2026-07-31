#[cfg(feature = "desktop")]
use std::collections::HashMap;
#[cfg(feature = "desktop")]
use std::time::Duration;

#[cfg(feature = "desktop")]
use tauri::Manager;

#[cfg(feature = "desktop")]
use crate::app_state::AppState;
#[cfg(feature = "desktop")]
use crate::error::AppResult;
#[cfg(feature = "desktop")]
use crate::mcp::gateway;
#[cfg(feature = "desktop")]
use crate::tunnel::{
    append_profile_log, cleanup_orphan_for_runtime, ensure_for_runtime,
    is_quick_tunnel_url_change_error, reconcile_mcp_gateway, TunnelServiceKind,
};
#[cfg(feature = "desktop")]
use crate::workspace::{RuntimeStatusDto, WorkspaceProfile};

#[cfg(feature = "desktop")]
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(feature = "desktop")]
const TUNNEL_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);

#[cfg(feature = "desktop")]
#[derive(Debug, Clone)]
struct TunnelRetryState {
    attempts: u8,
    next_attempt: tokio::time::Instant,
}

#[cfg(feature = "desktop")]
#[derive(Debug, Clone)]
enum GatewayRetryKind {
    Listener,
    Tunnel,
}

#[cfg(feature = "desktop")]
#[derive(Debug, Clone)]
struct GatewayRetryState {
    signature: String,
    kind: GatewayRetryKind,
    attempts: u8,
    next_attempt: tokio::time::Instant,
    blocked: bool,
}

#[cfg(feature = "desktop")]
pub fn spawn_desktop_maintenance(app: tauri::AppHandle) {
    crate::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(MAINTENANCE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tunnel_retries = HashMap::new();
        let mut gateway_retry = None;
        loop {
            ticker.tick().await;
            if let Err(error) = maintain_all(&app, &mut tunnel_retries, &mut gateway_retry).await {
                eprintln!("runtime maintenance failed: {error}");
            }
        }
    });
}

#[cfg(feature = "desktop")]
async fn maintain_all(
    app: &tauri::AppHandle,
    tunnel_retries: &mut HashMap<(String, TunnelServiceKind), TunnelRetryState>,
    gateway_retry: &mut Option<GatewayRetryState>,
) -> AppResult<()> {
    let state = app.state::<AppState>();
    let (profiles, settings) =
        state.with_workspaces(|store| Ok((store.list().to_vec(), store.settings())))?;

    for profile in &profiles {
        for kind in [TunnelServiceKind::Mcp, TunnelServiceKind::Actions] {
            let result = state.with_runtime(|runtime| match kind {
                TunnelServiceKind::Mcp => runtime.maintain_mcp(profile),
                TunnelServiceKind::Actions => runtime.maintain_actions(profile),
            });
            match result {
                Ok(runtime) => {
                    if kind != TunnelServiceKind::Mcp || !settings.mcp_gateway.enabled {
                        maintain_tunnel(&state, profile, kind, &runtime, tunnel_retries).await;
                    }
                }
                Err(error) => append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    &format!("[recovery] 后台维护失败，本轮跳过：{error}"),
                ),
            }
        }
    }
    maintain_mcp_gateway(&state, &profiles, &settings.mcp_gateway, gateway_retry).await;
    Ok(())
}

#[cfg(feature = "desktop")]
async fn maintain_mcp_gateway(
    state: &AppState,
    profiles: &[WorkspaceProfile],
    config: &crate::settings::McpGatewayConfig,
    retry: &mut Option<GatewayRetryState>,
) {
    let active = match state.with_runtime(|runtime| Ok(runtime.active_mcp_workspace_ids())) {
        Ok(active) => active,
        Err(error) => {
            log_gateway_error(config, &format!("读取活动工作区失败：{error}"));
            return;
        }
    };
    let signature = gateway_retry_signature(config, profiles, &active);
    if retry
        .as_ref()
        .is_some_and(|current| current.signature != signature)
    {
        *retry = None;
    }
    if !config.enabled || active.is_empty() {
        *retry = None;
    } else if retry.as_ref().is_some_and(|current| {
        matches!(current.kind, GatewayRetryKind::Listener)
            && (current.blocked || current.next_attempt > tokio::time::Instant::now())
    }) {
        return;
    }
    if let Err(error) = gateway::ensure(config, profiles, &active).await {
        gateway::record_runtime_error(format!("Gateway listener 维护失败：{error}")).await;
        schedule_gateway_retry(
            config,
            retry,
            signature,
            GatewayRetryKind::Listener,
            false,
            &format!("Gateway listener 维护失败：{error}"),
        );
        return;
    }
    if retry
        .as_ref()
        .is_some_and(|current| matches!(current.kind, GatewayRetryKind::Listener))
    {
        *retry = None;
    }
    if retry.as_ref().is_some_and(|current| {
        matches!(current.kind, GatewayRetryKind::Tunnel)
            && (current.blocked || current.next_attempt > tokio::time::Instant::now())
    }) {
        return;
    }
    match reconcile_mcp_gateway(config, profiles, &active).await {
        Ok(Some(url)) => {
            *retry = None;
            gateway::clear_runtime_error().await;
            if let Err(error) = persist_gateway_observation(state, config, profiles, &url) {
                log_gateway_error(config, &format!("保存 Gateway 公网地址失败：{error}"));
            }
        }
        Ok(None) => {
            *retry = None;
            gateway::clear_runtime_error().await;
        }
        Err(error) => {
            gateway::record_runtime_error(format!("Gateway 隧道维护失败：{error}")).await;
            let blocked = is_quick_tunnel_url_change_error(&error);
            schedule_gateway_retry(
                config,
                retry,
                signature,
                GatewayRetryKind::Tunnel,
                blocked,
                &format!("Gateway 隧道维护失败：{error}"),
            );
        }
    }
}

#[cfg(feature = "desktop")]
fn gateway_retry_signature(
    config: &crate::settings::McpGatewayConfig,
    profiles: &[WorkspaceProfile],
    active: &std::collections::HashSet<String>,
) -> String {
    let owner = profiles
        .iter()
        .find(|profile| profile.id == config.owner_workspace_id);
    let mut active = active.iter().cloned().collect::<Vec<_>>();
    active.sort();
    serde_json::to_string(&serde_json::json!({
        "enabled": config.enabled,
        "localPort": config.local_port,
        "owner": config.owner_workspace_id,
        "configuredUrl": config.public_url,
        "observedUrl": config.observed_public_url,
        "observedOwner": config.observed_owner_workspace_id,
        "observedTunnelSignature": config.observed_tunnel_signature,
        "active": active,
        "tunnel": owner.map(|profile| serde_json::json!({
            "type": profile.tunnel.tunnel_type,
            "publicUrl": profile.tunnel.public_url,
            "frpServer": profile.tunnel.frp_server,
            "frpSubdomain": profile.tunnel.frp_subdomain,
            "frpProfileId": profile.tunnel.frp_profile_id,
            "frpServerPort": profile.tunnel.frp_server_port,
            "cloudflareMode": profile.tunnel.cloudflare_mode,
            "useProxy": profile.tunnel.use_proxy,
        })),
    }))
    .unwrap_or_default()
}

#[cfg(feature = "desktop")]
fn schedule_gateway_retry(
    config: &crate::settings::McpGatewayConfig,
    retry: &mut Option<GatewayRetryState>,
    signature: String,
    kind: GatewayRetryKind,
    blocked: bool,
    message: &str,
) {
    let attempts = retry
        .as_ref()
        .map(|current| current.attempts.saturating_add(1))
        .unwrap_or(1);
    let delay = tunnel_retry_delay(attempts);
    *retry = Some(GatewayRetryState {
        signature,
        kind,
        attempts,
        next_attempt: tokio::time::Instant::now() + delay,
        blocked,
    });
    if blocked {
        log_gateway_error(
            config,
            &format!("{message}；已阻断自动重试，修改 Gateway/owner 隧道配置后才会恢复"),
        );
    } else {
        log_gateway_error(
            config,
            &format!(
                "{message}；第 {attempts} 次失败，{} 秒后重试",
                delay.as_secs()
            ),
        );
    }
}

#[cfg(feature = "desktop")]
fn persist_gateway_observation(
    state: &AppState,
    config: &crate::settings::McpGatewayConfig,
    profiles: &[WorkspaceProfile],
    url: &str,
) -> AppResult<()> {
    let normalized = url.trim().trim_end_matches('/');
    if normalized.is_empty() || normalized.starts_with("http://127.0.0.1:") {
        return Ok(());
    }
    let owner = profiles
        .iter()
        .find(|profile| profile.id == config.owner_workspace_id)
        .ok_or_else(|| {
            crate::error::AppError::Message("MCP Gateway 隧道所有者工作区不存在。".into())
        })?;
    let signature = gateway::tunnel_identity_signature(config, owner)?;
    let mut candidate = config.clone();
    candidate.observed_public_url = normalized.to_string();
    candidate.observed_owner_workspace_id = config.owner_workspace_id.clone();
    candidate.observed_tunnel_signature = signature.clone();
    gateway::validate_config(&candidate, profiles)?;
    state.with_settings(|store| {
        let mut settings = store.settings();
        if settings.mcp_gateway.identity_changed(config) {
            return Ok(());
        }
        if settings.mcp_gateway.observed_public_url == normalized
            && settings.mcp_gateway.observed_owner_workspace_id == config.owner_workspace_id
            && settings.mcp_gateway.observed_tunnel_signature == signature
        {
            return Ok(());
        }
        settings.mcp_gateway.observed_public_url = normalized.to_string();
        settings.mcp_gateway.observed_owner_workspace_id = config.owner_workspace_id.clone();
        settings.mcp_gateway.observed_tunnel_signature = signature;
        store.update_settings(settings)
    })
}

#[cfg(feature = "desktop")]
fn log_gateway_error(config: &crate::settings::McpGatewayConfig, message: &str) {
    let profile_id = if config.owner_workspace_id.trim().is_empty() {
        "mcp-gateway"
    } else {
        config.owner_workspace_id.as_str()
    };
    append_profile_log(profile_id, "stderr.log", &format!("[gateway] {message}"));
}

#[cfg(feature = "desktop")]
async fn maintain_tunnel(
    state: &AppState,
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    runtime: &RuntimeStatusDto,
    retries: &mut HashMap<(String, TunnelServiceKind), TunnelRetryState>,
) {
    let key = (profile.id.clone(), kind);
    if runtime.state == "running" {
        if retries
            .get(&key)
            .is_some_and(|retry| retry.next_attempt > tokio::time::Instant::now())
        {
            return;
        }
        match ensure_for_runtime(profile, kind).await {
            Ok(Some(url)) => {
                retries.remove(&key);
                if let Err(error) = persist_public_url(state, profile, kind, &url) {
                    append_profile_log(
                        &profile.id,
                        stderr_log_name(kind),
                        &format!("[recovery] 保存自动恢复后的隧道地址失败：{error}"),
                    );
                }
            }
            Ok(None) => {
                retries.remove(&key);
            }
            Err(error) => {
                if is_quick_tunnel_url_change_error(&error) {
                    retries.remove(&key);
                    append_profile_log(
                        &profile.id,
                        stderr_log_name(kind),
                        &format!(
                            "[recovery] Quick Tunnel 地址发生变化，已停止自动重试，需要人工更新 ChatGPT 地址：{error}"
                        ),
                    );
                    return;
                }
                let attempts = retries
                    .get(&key)
                    .map(|retry| retry.attempts.saturating_add(1))
                    .unwrap_or(1);
                let delay = tunnel_retry_delay(attempts);
                retries.insert(
                    key,
                    TunnelRetryState {
                        attempts,
                        next_attempt: tokio::time::Instant::now() + delay,
                    },
                );
                append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    &format!(
                        "[recovery] 隧道自动重连失败（第 {attempts} 次），{} 秒后重试：{error}",
                        delay.as_secs()
                    ),
                );
            }
        }
        return;
    }

    if runtime.state == "error" && !runtime.recovery.enabled {
        retries.remove(&key);
        if let Err(error) = cleanup_orphan_for_runtime(profile, kind, false).await {
            append_profile_log(
                &profile.id,
                stderr_log_name(kind),
                &format!("[recovery] 自动恢复耗尽后清理隧道失败：{error}"),
            );
        }
    } else if runtime.state == "stopped" || runtime.state == "stopping" {
        retries.remove(&key);
    }
}

#[cfg(feature = "desktop")]
fn persist_public_url(
    state: &AppState,
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    url: &str,
) -> AppResult<()> {
    if url.trim().is_empty() {
        return Ok(());
    }
    state.with_workspaces(|store| {
        let Some(mut current) = store.get(&profile.id).cloned() else {
            return Ok(());
        };
        let target = match kind {
            TunnelServiceKind::Mcp => &mut current.tunnel.public_url,
            TunnelServiceKind::Actions => &mut current.actions.public_url,
        };
        if target == url {
            return Ok(());
        }
        *target = url.to_string();
        store.update(current)
    })
}

#[cfg(feature = "desktop")]
fn tunnel_retry_delay(attempts: u8) -> Duration {
    let seconds = 2u64.saturating_pow(attempts.saturating_sub(1).min(5) as u32);
    Duration::from_secs(seconds).min(TUNNEL_RETRY_MAX_DELAY)
}

#[cfg(feature = "desktop")]
fn stderr_log_name(kind: TunnelServiceKind) -> &'static str {
    match kind {
        TunnelServiceKind::Mcp => "stderr.log",
        TunnelServiceKind::Actions => "actions-stderr.log",
    }
}
