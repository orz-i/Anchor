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
use crate::tunnel::{
    append_profile_log, cleanup_orphan_for_runtime, ensure_for_runtime,
    is_quick_tunnel_url_change_error, TunnelServiceKind,
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
pub fn spawn_desktop_maintenance(app: tauri::AppHandle) {
    crate::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(MAINTENANCE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tunnel_retries = HashMap::new();
        loop {
            ticker.tick().await;
            if let Err(error) = maintain_all(&app, &mut tunnel_retries).await {
                eprintln!("runtime maintenance failed: {error}");
            }
        }
    });
}

#[cfg(feature = "desktop")]
async fn maintain_all(
    app: &tauri::AppHandle,
    tunnel_retries: &mut HashMap<(String, TunnelServiceKind), TunnelRetryState>,
) -> AppResult<()> {
    let state = app.state::<AppState>();
    let profiles = state.with_workspaces(|store| Ok(store.list().to_vec()))?;

    for profile in profiles {
        for kind in [TunnelServiceKind::Mcp, TunnelServiceKind::Actions] {
            let result = state.with_runtime(|runtime| match kind {
                TunnelServiceKind::Mcp => runtime.maintain_mcp(&profile),
                TunnelServiceKind::Actions => runtime.maintain_actions(&profile),
            });
            match result {
                Ok(runtime) => {
                    maintain_tunnel(&state, &profile, kind, &runtime, tunnel_retries).await;
                }
                Err(error) => append_profile_log(
                    &profile.id,
                    stderr_log_name(kind),
                    &format!("[recovery] 后台维护失败，本轮跳过：{error}"),
                ),
            }
        }
    }
    Ok(())
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
