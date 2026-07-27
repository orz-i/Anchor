mod args;
mod daemon;
mod workspace;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::mcp::gateway;
use crate::platform::platform;
use crate::runtime::{await_listener_shutdown, update_public_url, RuntimeSupervisor, ServiceKind};
use crate::settings::McpGatewayConfig;
use crate::tunnel::{
    ensure_for_runtime, is_quick_tunnel_url_change_error, log_dir_for_profile,
    maybe_start_for_runtime, reconcile_mcp_gateway, stop_for_runtime, TunnelServiceKind,
};
use crate::workspace::{RuntimeStatusDto, WorkspaceProfile};

use args::{
    CliArgs, Command, GatewayCommand, GatewayConfigureOptions, LogSelection, LogsOptions,
    RunOptions, ServiceSelection, StatusOptions, StopOptions,
};

#[derive(Debug, Clone, Copy)]
struct CliTunnelRetry {
    attempts: u8,
    next_attempt: tokio::time::Instant,
}

fn path_or_parent_writable(path: &Path) -> bool {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if let Ok(metadata) = std::fs::metadata(candidate) {
            return metadata.is_dir() && !metadata.permissions().readonly();
        }
        current = candidate.parent();
    }
    false
}

fn port_owner(pid: Option<u32>, daemon_pid: Option<u32>) -> &'static str {
    match pid {
        Some(pid) if Some(pid) == daemon_pid => "daemon",
        Some(_) => "external",
        None => "none",
    }
}

async fn execute_gateway(command: GatewayCommand, as_json: bool) -> AppResult<i32> {
    match command {
        GatewayCommand::Show => show_gateway(as_json).map(|_| 0),
        GatewayCommand::Configure(options) => configure_gateway(options, as_json).map(|_| 0),
        GatewayCommand::Serve { workspaces } => {
            serve_gateway(&workspaces, as_json).await.map(|_| 0)
        }
    }
}

fn show_gateway(as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let config = store.settings().mcp_gateway;
    let routes = store
        .list()
        .iter()
        .map(|profile| {
            let endpoint = gateway::workspace_base_url(&config, &profile.id)
                .map(|base| format!("{base}/mcp"))
                .unwrap_or_default();
            json!({
                "workspaceId": profile.id,
                "workspaceName": profile.name,
                "endpoint": endpoint
            })
        })
        .collect::<Vec<_>>();
    let value = json!({ "config": config, "routes": routes });
    if as_json {
        print_json(&value)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

fn configure_gateway(options: GatewayConfigureOptions, as_json: bool) -> AppResult<()> {
    let mut store = DataStore::load()?;
    let mut settings = store.settings();
    let previous = settings.mcp_gateway.clone();
    let mut config = previous.clone();
    if let Some(enabled) = options.enabled {
        config.enabled = enabled;
    }
    if let Some(port) = options.local_port {
        config.local_port = port;
    }
    if let Some(selector) = options.owner_workspace {
        config.owner_workspace_id = resolve_workspace(store.list(), &selector)?.id.clone();
    }
    if let Some(public_url) = options.public_url {
        config.public_url = public_url.trim().trim_end_matches('/').to_string();
    }
    config.url_model_version = 2;
    if previous.identity_changed(&config) {
        config.clear_observation();
    } else {
        config.observed_public_url = previous.observed_public_url;
        config.observed_owner_workspace_id = previous.observed_owner_workspace_id;
        config.observed_tunnel_signature = previous.observed_tunnel_signature;
    }
    gateway::validate_config(&config, store.list())?;
    settings.mcp_gateway = config.clone();
    store.update_settings(settings)?;
    if as_json {
        print_json(&json!({ "event": "gateway_configured", "config": config }))?;
    } else {
        println!("MCP Gateway 配置已保存。");
        println!("{}", serde_json::to_string_pretty(&config)?);
    }
    Ok(())
}

async fn serve_gateway(selectors: &[String], as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let all_profiles = store.list().to_vec();
    let mut config = store.settings().mcp_gateway;
    if !config.enabled {
        return Err(AppError::Message(
            "MCP Gateway 尚未启用；请先运行 gateway configure --enable --owner WORKSPACE".into(),
        ));
    }
    gateway::validate_config(&config, &all_profiles)?;
    let mut selected = Vec::new();
    let mut selected_ids = std::collections::HashSet::new();
    for selector in selectors {
        let profile = resolve_workspace(&all_profiles, selector)?.clone();
        if selected_ids.insert(profile.id.clone()) {
            ensure_workspace_directory(&profile)?;
            selected.push(profile);
        }
    }
    if selected.is_empty() {
        return Err(AppError::Message("Gateway 没有选中的工作区。".into()));
    }
    ensure_gateway_ports_available(&config, &selected)?;

    let mut runtime = RuntimeSupervisor::default();
    let mut started = Vec::new();
    let startup = async {
        for profile in &selected {
            ensure_running(runtime.start_mcp(profile)?, "MCP")?;
            started.push(profile.clone());
        }
        let active = runtime.active_mcp_workspace_ids();
        gateway::ensure(&config, &all_profiles, &active).await?;
        if let Some(url) = reconcile_mcp_gateway(&config, &all_profiles, &active).await? {
            persist_cli_gateway_observation(&mut config, &all_profiles, &url)?;
        }
        Ok::<(), AppError>(())
    }
    .await;
    if let Err(error) = startup {
        let cleanup =
            shutdown_gateway_services(&mut runtime, &started, &config, &all_profiles).await;
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(AppError::Message(format!(
                "Gateway 启动失败：{error}；清理已启动服务也失败：{cleanup_error}"
            ))),
        };
    }

    if as_json {
        print_json(&json!({
            "event": "gateway_ready",
            "localEndpoint": format!("http://127.0.0.1:{}", config.local_port),
            "publicBaseUrl": config.effective_public_url(),
            "routes": selected.iter().map(|profile| json!({
                "workspaceId": profile.id,
                "workspaceName": profile.name,
                "endpoint": format!("{}/mcp", gateway::workspace_base_url(&config, &profile.id).unwrap_or_default())
            })).collect::<Vec<_>>()
        }))?;
    } else {
        println!("MCP Gateway 已启动：http://127.0.0.1:{}", config.local_port);
        for profile in &selected {
            println!(
                "{}\t{}/mcp",
                profile.name,
                gateway::workspace_base_url(&config, &profile.id)?
            );
        }
        println!("前台运行中，按 Ctrl+C 停止。");
    }

    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    let mut maintenance = tokio::time::interval(Duration::from_secs(2));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    maintenance.tick().await;
    let mut terminal_error = None;
    let mut gateway_tunnel_retry: Option<CliTunnelRetry> = None;
    loop {
        tokio::select! {
            signal = &mut shutdown_signal => {
                if let Err(error) = signal {
                    terminal_error = Some(error);
                }
                break;
            }
            _ = maintenance.tick() => {
                for profile in &selected {
                    match runtime.maintain_mcp(profile) {
                        Ok(status) if status.state == "error" && !status.recovery.enabled => {
                            terminal_error = Some(AppError::Message(format!(
                                "工作区 {} MCP 自动恢复耗尽：{}",
                                profile.name, status.local_message
                            )));
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            terminal_error = Some(error);
                            break;
                        }
                    }
                }
                if terminal_error.is_some() {
                    break;
                }
                let active = runtime.active_mcp_workspace_ids();
                if let Err(error) = gateway::ensure(&config, &all_profiles, &active).await {
                    terminal_error = Some(error);
                    break;
                }
                if gateway_tunnel_retry
                    .is_some_and(|retry| tokio::time::Instant::now() < retry.next_attempt)
                {
                    continue;
                }
                match reconcile_mcp_gateway(&config, &all_profiles, &active).await {
                    Ok(Some(url)) => {
                        let recovered_attempts = gateway_tunnel_retry.take().map(|retry| retry.attempts);
                        gateway::clear_runtime_error().await;
                        if url != config.observed_public_url {
                            persist_cli_gateway_observation(&mut config, &all_profiles, &url)?;
                        }
                        if let Some(attempts) = recovered_attempts {
                            if as_json {
                                print_json_line(&json!({
                                    "event": "gateway_tunnel_reconnected",
                                    "attempts": attempts,
                                    "publicBaseUrl": config.effective_public_url()
                                }))?;
                            } else {
                                println!("Gateway 隧道已在第 {attempts} 次重试后恢复。");
                            }
                        }
                    }
                    Ok(None) => {
                        gateway_tunnel_retry = None;
                        gateway::clear_runtime_error().await;
                    }
                    Err(error) if is_quick_tunnel_url_change_error(&error) => {
                        gateway::record_runtime_error(format!("Gateway 隧道维护失败：{error}")).await;
                        terminal_error = Some(error);
                        break;
                    }
                    Err(error) => {
                        gateway::record_runtime_error(format!("Gateway 隧道维护失败：{error}")).await;
                        let attempts = gateway_tunnel_retry
                            .map(|retry| retry.attempts.saturating_add(1))
                            .unwrap_or(1);
                        let delay = cli_tunnel_retry_delay(attempts);
                        gateway_tunnel_retry = Some(CliTunnelRetry {
                            attempts,
                            next_attempt: tokio::time::Instant::now() + delay,
                        });
                        if as_json {
                            print_json_line(&json!({
                                "event": "gateway_tunnel_retry",
                                "detail": error.to_string(),
                                "attempt": attempts,
                                "retryInSeconds": delay.as_secs()
                            }))?;
                        } else {
                            eprintln!(
                                "Gateway 隧道维护失败，将在 {} 秒后进行第 {} 次重试：{error}",
                                delay.as_secs(),
                                attempts
                            );
                        }
                    }
                }
            }
        }
    }

    shutdown_gateway_services(&mut runtime, &started, &config, &all_profiles).await?;
    if as_json {
        print_json(&json!({ "event": "gateway_stopped" }))?;
    }
    match terminal_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn ensure_gateway_ports_available(
    config: &McpGatewayConfig,
    profiles: &[WorkspaceProfile],
) -> AppResult<()> {
    if let Some(pid) = platform().find_pid_listening_on_port(config.local_port)? {
        return Err(AppError::Message(format!(
            "Gateway 端口 {} 已被 PID {pid} 占用；CLI 不会接管 GUI 或其他进程",
            config.local_port
        )));
    }
    for profile in profiles {
        ensure_selected_ports_available(profile, ServiceSelection::Mcp)?;
    }
    Ok(())
}

fn persist_cli_gateway_observation(
    config: &mut McpGatewayConfig,
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
        .ok_or_else(|| AppError::Message("MCP Gateway 隧道所有者工作区不存在。".into()))?;
    let signature = gateway::tunnel_identity_signature(config, owner)?;
    config.url_model_version = 2;
    config.observed_public_url = normalized.to_string();
    config.observed_owner_workspace_id = config.owner_workspace_id.clone();
    config.observed_tunnel_signature = signature.clone();
    gateway::validate_config(config, profiles)?;
    let mut store = DataStore::load()?;
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
    settings.mcp_gateway.url_model_version = 2;
    settings.mcp_gateway.observed_public_url = normalized.to_string();
    settings.mcp_gateway.observed_owner_workspace_id = config.owner_workspace_id.clone();
    settings.mcp_gateway.observed_tunnel_signature = signature;
    store.update_settings(settings)
}

async fn shutdown_gateway_services(
    runtime: &mut RuntimeSupervisor,
    profiles: &[WorkspaceProfile],
    config: &McpGatewayConfig,
    all_profiles: &[WorkspaceProfile],
) -> AppResult<()> {
    for profile in profiles.iter().rev() {
        let handle = runtime.begin_stop(&profile.id, ServiceKind::Mcp);
        await_listener_shutdown(handle, profile.runtime.local_port).await;
        runtime.finish_stop(&profile.id, ServiceKind::Mcp);
    }
    let active = runtime.active_mcp_workspace_ids();
    let tunnel_result = reconcile_mcp_gateway(config, all_profiles, &active).await;
    let gateway_result = gateway::stop().await;
    for profile in profiles {
        update_public_url(&profile.id, "mcp", "");
    }
    tunnel_result?;
    gateway_result
}

async fn start_daemon(options: RunOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    ensure_workspace_directory(&profile)?;
    let service = options.service.unwrap_or(ServiceSelection::Mcp);
    let tunnel = options.tunnel.unwrap_or(false);
    if store.settings().mcp_gateway.enabled && service.includes_mcp() {
        return Err(AppError::Message(
            "MCP Gateway 模式不支持每工作区独立 daemon；请使用 `anchor gateway serve <workspace ...>` 并交由 systemd 监督。"
                .into(),
        ));
    }

    let inspection = daemon::inspect(&profile)?;
    if inspection.running {
        let state = inspection.state.expect("running daemon state");
        if state.service == service && state.tunnel == tunnel {
            return print_daemon_result(
                "already_running",
                &profile,
                Some(state.pid),
                service,
                tunnel,
                as_json,
            );
        }
        return Err(AppError::Message(format!(
            "daemon 已运行（service={}, tunnel={}）；请使用 restart 修改运行参数",
            state.service.as_str(),
            state.tunnel
        )));
    }
    ensure_selected_ports_available(&profile, service)?;
    let child_pid = daemon::spawn(&profile, service, tunnel)?;
    match daemon::wait_ready(
        &profile,
        service,
        child_pid,
        Duration::from_secs(options.wait_seconds),
    )
    .await
    {
        Ok(state) => print_daemon_result(
            "started",
            &profile,
            Some(state.pid),
            service,
            tunnel,
            as_json,
        ),
        Err(error) => {
            let cleanup_error = daemon::terminate_spawned(&profile, child_pid).await.err();
            Err(AppError::Message(format!(
                "daemon 子进程 PID {child_pid} 未就绪：{error}{}",
                cleanup_error
                    .map(|cleanup| format!("；清理失败：{cleanup}"))
                    .unwrap_or_default()
            )))
        }
    }
}

async fn stop_daemon(options: StopOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    let stopped = daemon::stop(
        &profile,
        Duration::from_secs(options.timeout_seconds),
        options.force,
    )
    .await?;
    if as_json {
        print_json(&json!({
            "event": if stopped.is_some() { "stopped" } else { "already_stopped" },
            "workspace_id": profile.id,
            "pid": stopped
        }))?;
    } else if let Some(pid) = stopped {
        println!("workspace {} 的 daemon 已停止（PID {pid}）", profile.name);
    } else {
        println!("workspace {} 的 daemon 未运行", profile.name);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliLogChunk {
    name: String,
    path: String,
    content: String,
}

async fn show_logs(options: LogsOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    let files = selected_log_files(&profile, options.service);
    if !options.follow {
        let mut chunks = Vec::new();
        for (name, path) in files {
            if !path.exists() {
                continue;
            }
            chunks.push(CliLogChunk {
                name,
                path: path.display().to_string(),
                content: read_tail_lines(&path, options.lines)?,
            });
        }
        if as_json {
            print_json(&chunks)?;
        } else if chunks.is_empty() {
            println!("暂无日志：{}", log_dir_for_profile(&profile.id).display());
        } else {
            for chunk in chunks {
                println!("==> {} <==", chunk.path);
                print!("{}", chunk.content);
                if !chunk.content.ends_with('\n') {
                    println!();
                }
            }
        }
        return Ok(());
    }

    follow_logs(files, options.lines, as_json).await
}

async fn follow_logs(files: Vec<(String, PathBuf)>, lines: usize, as_json: bool) -> AppResult<()> {
    let mut offsets = std::collections::HashMap::new();
    for (name, path) in &files {
        if path.exists() {
            let content = read_tail_lines(path, lines)?;
            if as_json {
                print_json_line(&json!({
                    "event": "log_snapshot",
                    "name": name,
                    "path": path,
                    "content": content
                }))?;
            } else {
                println!("==> {} <==", path.display());
                print!("{content}");
                if !content.ends_with('\n') {
                    println!();
                }
                std::io::stdout().flush()?;
            }
            offsets.insert(path.clone(), fs_len(path));
        } else {
            offsets.insert(path.clone(), 0);
        }
    }

    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        tokio::select! {
            result = &mut shutdown_signal => return result,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                for (name, path) in &files {
                    let offset = offsets.get(path).copied().unwrap_or(0);
                    let (next, content) = read_log_since(path, offset)?;
                    offsets.insert(path.clone(), next);
                    if content.is_empty() {
                        continue;
                    }
                    if as_json {
                        print_json_line(&json!({
                            "event": "log_append",
                            "name": name,
                            "path": path,
                            "content": content
                        }))?;
                    } else {
                        print!("{content}");
                        std::io::stdout().flush()?;
                    }
                }
            }
        }
    }
}

fn selected_log_files(
    profile: &WorkspaceProfile,
    selection: LogSelection,
) -> Vec<(String, PathBuf)> {
    let log_dir = log_dir_for_profile(&profile.id);
    let mut files = Vec::new();
    if matches!(selection, LogSelection::Daemon | LogSelection::All) {
        files.push(("daemon".into(), daemon::daemon_log_path(&profile.id)));
    }
    if matches!(selection, LogSelection::Mcp | LogSelection::All) {
        if profile.tunnel.tunnel_type == "cloudflare" {
            files.push(("mcp-cloudflare".into(), log_dir.join("cloudflared.log")));
        }
        if profile.tunnel.tunnel_type == "frp" {
            files.push(("mcp-frp".into(), log_dir.join("frpc-mcp.log")));
        }
        files.push(("mcp-stdout".into(), log_dir.join("stdout.log")));
        files.push(("mcp-stderr".into(), log_dir.join("stderr.log")));
    }
    if matches!(selection, LogSelection::Actions | LogSelection::All) {
        if profile.actions.tunnel_type == "cloudflare" {
            files.push((
                "actions-cloudflare".into(),
                log_dir.join("actions-cloudflared.log"),
            ));
        }
        if profile.actions.tunnel_type == "frp" {
            files.push(("actions-frp".into(), log_dir.join("frpc-actions.log")));
        }
        files.push(("actions-stdout".into(), log_dir.join("actions-stdout.log")));
        files.push(("actions-stderr".into(), log_dir.join("actions-stderr.log")));
    }
    files
}

fn read_tail_lines(path: &Path, lines: usize) -> AppResult<String> {
    const MAX_BYTES: u64 = 1_048_576;
    let mut file = File::open(path)?;
    let size = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(size.saturating_sub(MAX_BYTES)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let selected = text.lines().rev().take(lines).collect::<Vec<_>>();
    let mut output = selected.into_iter().rev().collect::<Vec<_>>().join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    Ok(output)
}

fn read_log_since(path: &Path, offset: u64) -> AppResult<(u64, String)> {
    let Ok(mut file) = File::open(path) else {
        return Ok((0, String::new()));
    };
    let size = file.seek(SeekFrom::End(0))?;
    let start = if size < offset { 0 } else { offset };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((size, String::from_utf8_lossy(&bytes).into_owned()))
}

fn fs_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|value| value.len())
        .unwrap_or(0)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheck {
    name: String,
    ok: bool,
    detail: String,
    hint: String,
}

fn doctor_workspace(selector: &str, as_json: bool) -> AppResult<bool> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?.clone();
    let inspection = daemon::inspect(&profile)?;
    let mut checks = Vec::new();
    checks.push(doctor_check(
        "Linux daemon 支持",
        daemon::supported(),
        if daemon::supported() {
            "可使用 start/stop/restart".into()
        } else {
            "当前平台只支持 serve 前台模式".into()
        },
        "在 Linux 主机运行 daemon 运维命令",
    ));
    checks.push(doctor_check(
        "Workspace 目录",
        Path::new(&profile.path).is_dir(),
        profile.path.clone(),
        "修正 WorkspaceProfile.path",
    ));
    checks.push(doctor_check(
        "Daemon 状态",
        !inspection.stale,
        inspection.detail.clone(),
        "停止错误进程或清理过期状态后重新 start",
    ));

    let owner_pid = inspection.state.as_ref().map(|state| state.pid);
    for (label, port) in [
        ("MCP 端口", profile.runtime.local_port),
        ("Actions 端口", profile.actions.local_port),
    ] {
        let pid = platform().find_pid_listening_on_port(port)?;
        let owned = pid.is_none() || (inspection.running && pid == owner_pid);
        checks.push(doctor_check(
            label,
            owned,
            match pid {
                Some(pid) if Some(pid) == owner_pid => {
                    format!("{port} 由 daemon PID {pid} 监听")
                }
                Some(pid) => format!("{port} 被外部 PID {pid} 占用"),
                None => format!("{port} 可用"),
            },
            "停止 GUI/其他进程或修改 profile 端口",
        ));
    }

    let log_dir = log_dir_for_profile(&profile.id);
    let log_ok = path_or_parent_writable(&log_dir);
    checks.push(doctor_check(
        "日志目录",
        log_ok,
        log_dir.display().to_string(),
        "检查当前用户对配置目录的写权限",
    ));
    append_tunnel_doctor_checks(&profile, &mut checks);

    if as_json {
        print_json(&json!({
            "workspace": {"id": profile.id, "name": profile.name, "path": profile.path},
            "ok": checks.iter().all(|check| check.ok),
            "checks": checks
        }))?;
    } else {
        println!("{} ({})", profile.name, profile.id);
        for check in &checks {
            println!(
                "{}\t{}\t{}",
                if check.ok { "PASS" } else { "FAIL" },
                check.name,
                check.detail
            );
            if !check.ok {
                println!("  建议：{}", check.hint);
            }
        }
    }
    Ok(checks.iter().all(|check| check.ok))
}

fn append_tunnel_doctor_checks(profile: &WorkspaceProfile, checks: &mut Vec<DoctorCheck>) {
    for (label, tunnel_type) in [
        ("MCP 隧道依赖", profile.tunnel.tunnel_type.as_str()),
        ("Actions 隧道依赖", profile.actions.tunnel_type.as_str()),
    ] {
        let result = match tunnel_type {
            "frp" => crate::tunnel::resolve_frpc().map(|path| path.display().to_string()),
            "cloudflare" => {
                crate::tunnel::resolve_cloudflared().map(|path| path.display().to_string())
            }
            _ => {
                checks.push(doctor_check(label, true, "未配置隧道".into(), ""));
                continue;
            }
        };
        match result {
            Ok(path) => checks.push(doctor_check(label, true, path, "")),
            Err(error) => checks.push(doctor_check(
                label,
                false,
                error.to_string(),
                "安装对应隧道二进制或关闭 --tunnel",
            )),
        }
    }
}

fn doctor_check(name: &str, ok: bool, detail: String, hint: &str) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        ok,
        detail,
        hint: if ok { String::new() } else { hint.into() },
    }
}

async fn restart_daemon(mut options: RunOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), &options.workspace)?.clone();
    let current = daemon::inspect(&profile)?.state;
    if options.service.is_none() {
        options.service = current.as_ref().map(|state| state.service);
    }
    if options.tunnel.is_none() {
        options.tunnel = current.as_ref().map(|state| state.tunnel);
    }
    let _ = daemon::stop(&profile, Duration::from_secs(10), true).await?;
    start_daemon(options, as_json).await
}

async fn run_daemon(selector: &str, service: ServiceSelection, tunnel: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?.clone();
    let _guard = daemon::acquire(&profile, service, tunnel)?;
    crate::tunnel::append_profile_log(
        &profile.id,
        "daemon.log",
        &format!(
            "[daemon] started pid={} service={} tunnel={tunnel}",
            std::process::id(),
            service.as_str()
        ),
    );
    let result = serve_workspace(&profile.id, service, tunnel, false, false).await;
    if let Err(error) = &result {
        crate::tunnel::append_profile_log(
            &profile.id,
            "daemon.log",
            &format!("[daemon] stopped with error: {error}"),
        );
    } else {
        crate::tunnel::append_profile_log(&profile.id, "daemon.log", "[daemon] stopped");
    }
    result
}

fn print_daemon_result(
    event: &str,
    profile: &WorkspaceProfile,
    pid: Option<u32>,
    service: ServiceSelection,
    tunnel: bool,
    as_json: bool,
) -> AppResult<()> {
    if as_json {
        print_json(&json!({
            "event": event,
            "workspace": {"id": profile.id, "name": profile.name, "path": profile.path},
            "pid": pid,
            "service": service,
            "tunnel": tunnel,
            "log_path": daemon::daemon_log_path(&profile.id)
        }))?;
    } else {
        let pid = pid
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "workspace {} daemon {}：PID {pid}，service={}，tunnel={tunnel}",
            profile.name,
            if event == "started" {
                "已启动"
            } else {
                "已在运行"
            },
            service.as_str()
        );
        println!("日志：{}", daemon::daemon_log_path(&profile.id).display());
    }
    Ok(())
}

fn ensure_selected_ports_available(
    profile: &WorkspaceProfile,
    service: ServiceSelection,
) -> AppResult<()> {
    let mut selected = Vec::new();
    if service.includes_mcp() {
        selected.push(("MCP", profile.runtime.local_port));
    }
    if service.includes_actions() {
        selected.push(("Actions", profile.actions.local_port));
    }
    for (label, port) in selected {
        if let Some(pid) = platform().find_pid_listening_on_port(port)? {
            return Err(AppError::Message(format!(
                "{label} 端口 {port} 已被 PID {pid} 占用；CLI 不会接管 GUI 或其他进程"
            )));
        }
    }
    Ok(())
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
        std::env::set_var(crate::brand::CONFIG_DIR_ENV, path);
    }

    let as_json = parsed.json;
    let daemon_mode = matches!(&parsed.command, Command::DaemonRun { .. });
    match crate::async_runtime::block_on(execute(parsed)) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "ok": false,
                        "error": error.to_string()
                    }))
                    .unwrap_or_else(|_| "{\"ok\":false}".into())
                );
            } else {
                let message = format!("错误：{error}");
                if daemon_mode {
                    eprintln!("{}", crate::logging::timestamped_line(&message));
                } else {
                    eprintln!("{message}");
                }
            }
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

async fn execute(cli: CliArgs) -> AppResult<i32> {
    match cli.command {
        Command::Help => {
            println!("{}", args::usage());
            Ok(0)
        }
        Command::Version => {
            println!("{} {}", crate::brand::CLI_NAME, env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Command::List => list_workspaces(cli.json).map(|_| 0),
        Command::Show { workspace } => show_workspace(&workspace, cli.json).map(|_| 0),
        Command::Status(options) => show_status(options, cli.json).await.map(|_| 0),
        Command::Serve {
            workspace,
            service,
            tunnel,
        } => serve_workspace(&workspace, service, tunnel, cli.json, true)
            .await
            .map(|_| 0),
        Command::Start(options) => start_daemon(options, cli.json).await.map(|_| 0),
        Command::Stop(options) => stop_daemon(options, cli.json).await.map(|_| 0),
        Command::Restart(options) => restart_daemon(options, cli.json).await.map(|_| 0),
        Command::Logs(options) => show_logs(options, cli.json).await.map(|_| 0),
        Command::Doctor { workspace } => {
            doctor_workspace(&workspace, cli.json).map(|healthy| if healthy { 0 } else { 1 })
        }
        Command::Workspace(command) => workspace::execute(command, cli.json).await,
        Command::Gateway(command) => execute_gateway(command, cli.json).await,
        Command::DaemonRun {
            workspace,
            service,
            tunnel,
        } => run_daemon(&workspace, service, tunnel).await.map(|_| 0),
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
        println!(
            "没有已配置的 workspace。请使用 `anchor workspace register PATH` 或 GUI 创建 profile。"
        );
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
    owner: String,
    endpoint: String,
}

#[derive(Serialize)]
struct WorkspaceStatus {
    id: String,
    name: String,
    path: String,
    daemon: daemon::DaemonInspection,
    mcp: PortStatus,
    actions: PortStatus,
}

async fn show_status(options: StatusOptions, as_json: bool) -> AppResult<()> {
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        let status = workspace_status(&options.workspace)?;
        if as_json && options.watch {
            print_json_line(&status)?;
        } else {
            print_workspace_status(&status, as_json)?;
        }
        if !options.watch {
            return Ok(());
        }
        tokio::select! {
            result = &mut shutdown_signal => return result,
            _ = tokio::time::sleep(Duration::from_secs(options.interval_seconds)) => {}
        }
    }
}

fn workspace_status(selector: &str) -> AppResult<WorkspaceStatus> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?.clone();
    let daemon = daemon::inspect(&profile)?;
    let daemon_pid = daemon
        .state
        .as_ref()
        .filter(|_| daemon.running)
        .map(|state| state.pid);
    let status = WorkspaceStatus {
        id: profile.id.clone(),
        name: profile.name.clone(),
        path: profile.path.clone(),
        daemon,
        mcp: port_status(
            "mcp",
            profile.runtime.local_port,
            profile.local_endpoint(),
            daemon_pid,
        )?,
        actions: port_status(
            "actions",
            profile.actions.local_port,
            profile.actions_local_base_url(),
            daemon_pid,
        )?,
    };

    Ok(status)
}

fn print_workspace_status(status: &WorkspaceStatus, as_json: bool) -> AppResult<()> {
    if as_json {
        print_json(&status)?;
    } else {
        println!("{} ({})", status.name, status.id);
        println!("daemon\t{}", status.daemon.detail);
        print_port_status(&status.mcp);
        print_port_status(&status.actions);
    }
    Ok(())
}

fn port_status(
    service: &'static str,
    port: u16,
    endpoint: String,
    daemon_pid: Option<u32>,
) -> AppResult<PortStatus> {
    let pid = platform().find_pid_listening_on_port(port)?;
    let owner = port_owner(pid, daemon_pid);
    Ok(PortStatus {
        service,
        port,
        listening: pid.is_some(),
        pid,
        owner: owner.into(),
        endpoint,
    })
}

fn print_port_status(status: &PortStatus) {
    if let Some(pid) = status.pid {
        println!(
            "{}\tlistening\t{}\tpid={}",
            status.service, status.endpoint, pid
        );
        println!("  owner={}", status.owner);
    } else {
        println!("{}\tstopped\t{}", status.service, status.endpoint);
    }
}

async fn serve_workspace(
    selector: &str,
    service: ServiceSelection,
    with_tunnel: bool,
    as_json: bool,
    foreground: bool,
) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = resolve_workspace(store.list(), selector)?.clone();
    ensure_workspace_directory(&profile)?;
    if store.settings().mcp_gateway.enabled && service.includes_mcp() {
        return Err(AppError::Message(
            "MCP Gateway 模式请使用 `anchor gateway serve <workspace ...>`；单工作区 serve 不会创建共享路由。"
                .into(),
        ));
    }

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
                        print_runtime_line(
                            &format!("{} tunnel\t{url}", tunnel_label(kind)),
                            foreground,
                            false,
                        );
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
                            print_runtime_line(
                                &format!(
                                    "{} tunnel 暂未连接，{} 秒后自动重试：{error}",
                                    tunnel_label(kind),
                                    delay.as_secs()
                                ),
                                foreground,
                                true,
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
        print_runtime_line(
            &format!("workspace {} 已启动：", profile.name),
            foreground,
            false,
        );
        for kind in &started_services {
            print_runtime_line(
                &format!(
                    "{}\t{}",
                    service_label(*kind),
                    endpoint_for(&profile, *kind)
                ),
                foreground,
                false,
            );
        }
        if foreground {
            println!("前台运行中，按 Ctrl+C 停止。");
        } else {
            print_runtime_line("daemon 运行中，等待 SIGTERM/SIGINT 停止。", false, false);
        }
    }

    let mut maintenance = tokio::time::interval(std::time::Duration::from_secs(2));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    maintenance.tick().await;
    let mut last_states = std::collections::HashMap::new();
    let mut terminal_error = None;
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    loop {
        tokio::select! {
            signal = &mut shutdown_signal => {
                if let Err(error) = signal {
                    terminal_error = Some(error);
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
                            print_runtime_line(&status.local_message, foreground, true);
                        } else if status.state == "running" && previous.as_deref() == Some("recovering") {
                            print_runtime_line(
                                &format!("{} 已自动恢复", service_label(kind)),
                                foreground,
                                false,
                            );
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
                                    print_runtime_line(
                                        &format!("{} tunnel 已自动恢复", tunnel_label(kind)),
                                        foreground,
                                        false,
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            if is_quick_tunnel_url_change_error(&error) {
                                tunnel_retries.remove(&kind);
                                terminal_error = Some(error);
                                break;
                            }
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
                                print_runtime_line(
                                    &format!(
                                        "{} tunnel 自动重连失败，{} 秒后重试：{error}",
                                        tunnel_label(kind),
                                        delay.as_secs()
                                    ),
                                    foreground,
                                    true,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    if !as_json && foreground {
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

fn print_runtime_line(message: &str, foreground: bool, error: bool) {
    let message = if foreground {
        message.to_string()
    } else {
        crate::logging::timestamped_line(message)
    };
    if error {
        eprintln!("{message}");
    } else {
        println!("{message}");
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

fn print_json_line(value: &impl Serialize) -> AppResult<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

async fn wait_for_shutdown_signal() -> AppResult<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|error| AppError::Message(format!("无法监听 SIGTERM：{error}")))?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|error| AppError::Message(format!("无法监听 SIGINT：{error}")))?;
        tokio::select! {
            _ = terminate.recv() => Ok(()),
            _ = interrupt.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|error| AppError::Message(format!("无法监听 Ctrl+C：{error}")))
    }
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
    fn port_owner_distinguishes_daemon_external_and_empty_ports() {
        assert_eq!(port_owner(Some(42), Some(42)), "daemon");
        assert_eq!(port_owner(Some(84), Some(42)), "external");
        assert_eq!(port_owner(None, Some(42)), "none");
    }

    #[test]
    fn log_tail_returns_only_requested_lines() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("daemon.log");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").expect("log fixture");

        let tail = read_tail_lines(&path, 2).expect("tail");

        assert_eq!(tail, "three\nfour\n");
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
