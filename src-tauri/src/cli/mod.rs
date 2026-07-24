mod args;

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde_json::json;

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::runtime::{await_listener_shutdown, RuntimeSupervisor, ServiceKind};
use crate::tunnel::{
    ensure_for_runtime, maybe_start_for_runtime, stop_for_runtime, TunnelServiceKind,
};
use crate::workspace::{RuntimeStatusDto, WorkspaceProfile};

use args::{CliArgs, Command, ServiceSelection};

#[derive(Debug, Clone, Copy)]
struct CliTunnelRetry {
    attempts: u8,
    next_attempt: tokio::time::Instant,
}

fn cli_tunnel_retry_delay(attempts: u8) -> Duration {
    Duration::from_secs((1u64 << attempts.saturating_sub(1).min(6)).min(60))
}

pub fn run() -> i32 {
    let parsed = match args::parse(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    if let Some(path) = &parsed.config_dir {
        std::env::set_var("CODING_TOOLS_MCP_CONFIG_DIR", path);
    }

    match crate::async_runtime::block_on(execute(parsed)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("错误：{error}");
            1
        }
    }
}

fn redacted_profile_value(profile: &WorkspaceProfile) -> AppResult<serde_json::Value> {
    let mut value = serde_json::to_value(profile)?;
    if let Some(actions) = value
        .get_mut("actions")
        .and_then(serde_json::Value::as_object_mut)
    {
        actions.remove("cloudflare_token");
    }
    Ok(value)
}

async fn execute(cli: CliArgs) -> AppResult<()> {
    match cli.command {
        Command::Help => {
            println!("{}", args::usage());
            Ok(())
        }
        Command::Version => {
            println!("coding-tools-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::List => list_workspaces(cli.json),
        Command::Show { workspace } => show_workspace(&workspace, cli.json),
        Command::Status { workspace } => show_status(&workspace, cli.json),
        Command::Serve {
            workspace,
            service,
            tunnel,
        } => serve_workspace(&workspace, service, tunnel, cli.json).await,
    }
}

#[derive(Serialize)]
struct WorkspaceSummary<'a> {
    id: &'a str,
    name: &'a str,
    path: &'a str,
    mcp_port: u16,
    actions_port: u16,
}

fn list_workspaces(as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let summaries: Vec<_> = store
        .list()
        .iter()
        .map(|profile| WorkspaceSummary {
            id: &profile.id,
            name: &profile.name,
            path: &profile.path,
            mcp_port: profile.runtime.local_port,
            actions_port: profile.actions.local_port,
        })
        .collect();

    if as_json {
        print_json(&summaries)?;
    } else if summaries.is_empty() {
        println!("没有已配置的 workspace。请先通过 GUI 创建 workspace/profile。");
    } else {
        for item in summaries {
            println!(
                "{}\t{}\t{}\tMCP:{}\tActions:{}",
                item.id, item.name, item.path, item.mcp_port, item.actions_port
            );
        }
    }
    Ok(())
}

fn show_workspace(selector: &str, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?;
    let value = redacted_profile_value(profile)?;

    if as_json {
        print_json(&value)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

#[derive(Serialize)]
struct PortStatus {
    service: &'static str,
    port: u16,
    listening: bool,
    pid: Option<u32>,
    endpoint: String,
}

#[derive(Serialize)]
struct WorkspaceStatus<'a> {
    id: &'a str,
    name: &'a str,
    path: &'a str,
    mcp: PortStatus,
    actions: PortStatus,
}

fn show_status(selector: &str, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?;
    let status = WorkspaceStatus {
        id: &profile.id,
        name: &profile.name,
        path: &profile.path,
        mcp: port_status("mcp", profile.runtime.local_port, profile.local_endpoint())?,
        actions: port_status(
            "actions",
            profile.actions.local_port,
            profile.actions_local_base_url(),
        )?,
    };

    if as_json {
        print_json(&status)?;
    } else {
        println!("{} ({})", status.name, status.id);
        print_port_status(&status.mcp);
        print_port_status(&status.actions);
    }
    Ok(())
}

fn port_status(service: &'static str, port: u16, endpoint: String) -> AppResult<PortStatus> {
    let pid = platform().find_pid_listening_on_port(port)?;
    Ok(PortStatus {
        service,
        port,
        listening: pid.is_some(),
        pid,
        endpoint,
    })
}

fn print_port_status(status: &PortStatus) {
    if let Some(pid) = status.pid {
        println!(
            "{}\tlistening\t{}\tpid={}",
            status.service, status.endpoint, pid
        );
    } else {
        println!("{}\tstopped\t{}", status.service, status.endpoint);
    }
}

async fn serve_workspace(
    selector: &str,
    service: ServiceSelection,
    with_tunnel: bool,
    as_json: bool,
) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?.clone();
    ensure_workspace_directory(&profile)?;

    let mut runtime = RuntimeSupervisor::default();
    let mut started_services = Vec::new();
    let mut managed_tunnels = Vec::new();
    let mut tunnel_retries = std::collections::HashMap::new();

    let start_result = async {
        if service.includes_mcp() {
            ensure_running(runtime.start_mcp(&profile)?, "MCP")?;
            started_services.push(ServiceKind::Mcp);
        }
        if service.includes_actions() {
            ensure_running(runtime.start_actions(&profile)?, "Actions")?;
            started_services.push(ServiceKind::Actions);
        }

        if with_tunnel {
            for kind in selected_tunnels(service) {
                managed_tunnels.push(kind);
                match maybe_start_for_runtime(&profile, kind).await {
                    Ok(Some(url)) if !as_json => {
                        println!("{} tunnel\t{url}", tunnel_label(kind));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let attempts = 1;
                        let delay = cli_tunnel_retry_delay(attempts);
                        tunnel_retries.insert(
                            kind,
                            CliTunnelRetry {
                                attempts,
                                next_attempt: tokio::time::Instant::now() + delay,
                            },
                        );
                        if as_json {
                            print_json(&json!({
                                "event": "tunnel_retry_scheduled",
                                "service": tunnel_label(kind),
                                "attempt": attempts,
                                "retry_in_ms": delay.as_millis(),
                                "detail": error.to_string()
                            }))?;
                        } else {
                            eprintln!(
                                "{} tunnel 暂未连接，{} 秒后自动重试：{error}",
                                tunnel_label(kind),
                                delay.as_secs()
                            );
                        }
                    }
                }
            }
        }
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = start_result {
        let _ = shutdown(&mut runtime, &profile, &started_services, &managed_tunnels).await;
        return Err(error);
    }

    if as_json {
        print_json(&json!({
            "event": "ready",
            "workspace": {"id": profile.id, "name": profile.name, "path": profile.path},
            "services": started_services.iter().map(|kind| service_label(*kind)).collect::<Vec<_>>(),
            "tunnel": with_tunnel
        }))?;
    } else {
        println!("workspace {} 已启动：", profile.name);
        for kind in &started_services {
            println!(
                "{}\t{}",
                service_label(*kind),
                endpoint_for(&profile, *kind)
            );
        }
        println!("前台运行中，按 Ctrl+C 停止。");
    }

    let mut maintenance = tokio::time::interval(std::time::Duration::from_secs(2));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    maintenance.tick().await;
    let mut last_states = std::collections::HashMap::new();
    let mut terminal_error = None;
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                if let Err(error) = signal {
                    terminal_error = Some(AppError::Message(format!(
                        "无法监听 Ctrl+C：{error}"
                    )));
                }
                break;
            }
            _ = maintenance.tick() => {
                for kind in started_services.iter().copied() {
                    let status_result = match kind {
                        ServiceKind::Mcp => runtime.maintain_mcp(&profile),
                        ServiceKind::Actions => runtime.maintain_actions(&profile),
                    };
                    let status = match status_result {
                        Ok(status) => status,
                        Err(error) => {
                            terminal_error = Some(error);
                            break;
                        }
                    };
                    let previous = last_states.insert(kind, status.state.clone());
                    if previous.as_deref() != Some(status.state.as_str()) {
                        if as_json {
                            print_json(&json!({
                                "event": "service_state",
                                "service": service_label(kind),
                                "state": status.state,
                                "message": status.local_message,
                                "recovery": status.recovery
                            }))?;
                        } else if status.state == "recovering" {
                            eprintln!("{}", status.local_message);
                        } else if status.state == "running" && previous.as_deref() == Some("recovering") {
                            println!("{} 已自动恢复", service_label(kind));
                        }
                    }
                    if status.state == "error" && !status.recovery.enabled {
                        terminal_error = Some(AppError::Message(format!(
                            "{}自动恢复失败：{}",
                            service_label(kind),
                            status.local_message
                        )));
                        break;
                    }
                }
                if terminal_error.is_some() {
                    break;
                }
                for kind in managed_tunnels.iter().copied() {
                    if tunnel_retries
                        .get(&kind)
                        .is_some_and(|retry| retry.next_attempt > tokio::time::Instant::now())
                    {
                        continue;
                    }
                    match ensure_for_runtime(&profile, kind).await {
                        Ok(_) => {
                            if let Some(previous) = tunnel_retries.remove(&kind) {
                                if as_json {
                                    print_json(&json!({
                                        "event": "tunnel_reconnected",
                                        "service": tunnel_label(kind),
                                        "attempts": previous.attempts
                                    }))?;
                                } else {
                                    println!("{} tunnel 已自动恢复", tunnel_label(kind));
                                }
                            }
                        }
                        Err(error) => {
                            let attempts = tunnel_retries
                                .get(&kind)
                                .map(|retry| retry.attempts.saturating_add(1))
                                .unwrap_or(1);
                            let delay = cli_tunnel_retry_delay(attempts);
                            tunnel_retries.insert(
                                kind,
                                CliTunnelRetry {
                                    attempts,
                                    next_attempt: tokio::time::Instant::now() + delay,
                                },
                            );
                            if as_json {
                                print_json(&json!({
                                    "event": "tunnel_retry_scheduled",
                                    "service": tunnel_label(kind),
                                    "attempt": attempts,
                                    "retry_in_ms": delay.as_millis(),
                                    "detail": error.to_string()
                                }))?;
                            } else {
                                eprintln!(
                                    "{} tunnel 自动重连失败，{} 秒后重试：{error}",
                                    tunnel_label(kind),
                                    delay.as_secs()
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    if !as_json {
        println!("正在停止……");
    }
    shutdown(&mut runtime, &profile, &started_services, &managed_tunnels).await?;
    if as_json {
        print_json(&json!({"event": "stopped", "workspace_id": profile.id}))?;
    }
    match terminal_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn ensure_running(status: RuntimeStatusDto, label: &str) -> AppResult<()> {
    if status.state == "running" {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "{label} 启动失败：{}",
            status.local_message
        )))
    }
}

async fn shutdown(
    runtime: &mut RuntimeSupervisor,
    profile: &WorkspaceProfile,
    services: &[ServiceKind],
    tunnels: &[TunnelServiceKind],
) -> AppResult<()> {
    for kind in services.iter().rev().copied() {
        let handle = runtime.begin_stop(&profile.id, kind);
        await_listener_shutdown(handle, port_for(profile, kind)).await;
        runtime.finish_stop(&profile.id, kind);
    }

    let mut first_error = None;
    for kind in tunnels.iter().rev().copied() {
        if let Err(error) = stop_for_runtime(profile, kind).await {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn selected_tunnels(service: ServiceSelection) -> Vec<TunnelServiceKind> {
    let mut values = Vec::new();
    if service.includes_mcp() {
        values.push(TunnelServiceKind::Mcp);
    }
    if service.includes_actions() {
        values.push(TunnelServiceKind::Actions);
    }
    values
}

fn ensure_workspace_directory(profile: &WorkspaceProfile) -> AppResult<()> {
    let path = Path::new(&profile.path);
    if !path.is_dir() {
        return Err(AppError::Message(format!(
            "workspace 目录不存在或不是目录：{}",
            path.display()
        )));
    }
    Ok(())
}

fn resolve_workspace<'a>(
    profiles: &'a [WorkspaceProfile],
    selector: &str,
) -> AppResult<&'a WorkspaceProfile> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(AppError::Message("workspace 不能为空".into()));
    }
    if let Some(profile) = profiles.iter().find(|profile| profile.id == selector) {
        return Ok(profile);
    }

    let normalized = normalize_path(selector);
    if let Some(profile) = profiles
        .iter()
        .find(|profile| normalize_path(&profile.path) == normalized)
    {
        return Ok(profile);
    }

    let named: Vec<_> = profiles
        .iter()
        .filter(|profile| profile.name.eq_ignore_ascii_case(selector))
        .collect();
    match named.as_slice() {
        [profile] => Ok(*profile),
        [] => Err(AppError::Message(format!(
            "找不到 workspace/profile：{selector}"
        ))),
        _ => Err(AppError::Message(format!(
            "workspace 名称不唯一，请改用 profile ID 或项目路径：{selector}"
        ))),
    }
}

fn normalize_path(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized.to_string()
    }
}

fn port_for(profile: &WorkspaceProfile, kind: ServiceKind) -> u16 {
    match kind {
        ServiceKind::Mcp => profile.runtime.local_port,
        ServiceKind::Actions => profile.actions.local_port,
    }
}

fn endpoint_for(profile: &WorkspaceProfile, kind: ServiceKind) -> String {
    match kind {
        ServiceKind::Mcp => profile.local_endpoint(),
        ServiceKind::Actions => profile.actions_local_base_url(),
    }
}

fn service_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "mcp",
        ServiceKind::Actions => "actions",
    }
}

fn tunnel_label(kind: TunnelServiceKind) -> &'static str {
    match kind {
        TunnelServiceKind::Mcp => "mcp",
        TunnelServiceKind::Actions => "actions",
    }
}

fn print_json(value: &impl Serialize) -> AppResult<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_retry_delay_uses_bounded_exponential_backoff() {
        assert_eq!(cli_tunnel_retry_delay(1), Duration::from_secs(1));
        assert_eq!(cli_tunnel_retry_delay(2), Duration::from_secs(2));
        assert_eq!(cli_tunnel_retry_delay(3), Duration::from_secs(4));
        assert_eq!(cli_tunnel_retry_delay(6), Duration::from_secs(32));
        assert_eq!(cli_tunnel_retry_delay(7), Duration::from_secs(60));
        assert_eq!(cli_tunnel_retry_delay(20), Duration::from_secs(60));
    }

    #[test]
    fn resolves_one_workspace_to_one_profile_by_id_name_or_path() {
        let first = WorkspaceProfile::new("/srv/projects/alpha".into(), Some("alpha".into()));
        let second = WorkspaceProfile::new("/srv/projects/beta".into(), Some("beta".into()));
        let profiles = vec![first.clone(), second];

        assert_eq!(
            resolve_workspace(&profiles, &first.id).expect("id").id,
            first.id
        );
        assert_eq!(
            resolve_workspace(&profiles, "ALPHA").expect("name").id,
            first.id
        );
        assert_eq!(
            resolve_workspace(&profiles, "/srv/projects/alpha/")
                .expect("path")
                .id,
            first.id
        );
    }

    #[test]
    fn rejects_ambiguous_workspace_names() {
        let profiles = vec![
            WorkspaceProfile::new("/srv/a".into(), Some("same".into())),
            WorkspaceProfile::new("/srv/b".into(), Some("same".into())),
        ];

        let error = resolve_workspace(&profiles, "same").expect_err("ambiguous");

        assert!(error.to_string().contains("名称不唯一"));
    }

    #[test]
    fn show_output_removes_legacy_inline_tokens() {
        let mut profile = WorkspaceProfile::new("/srv/a".into(), Some("a".into()));
        profile.actions.cloudflare_token = "must-not-leak".into();

        let value = redacted_profile_value(&profile).expect("redact");

        assert!(value
            .get("actions")
            .and_then(|actions| actions.get("cloudflare_token"))
            .is_none());
        assert!(!value.to_string().contains("must-not-leak"));
    }
}
