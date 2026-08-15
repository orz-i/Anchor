use std::collections::VecDeque;
use std::path::PathBuf;

pub use crate::daemon::ServiceSelection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub config_dir: Option<PathBuf>,
    pub json: bool,
    pub command: Command,
}

fn set_frp_token_input(
    target: &mut Option<FrpTokenInput>,
    value: FrpTokenInput,
) -> Result<(), String> {
    if target.is_some() {
        return Err(
            "FRP token 输入方式只能选择一种：--token、--token-file 或 --token-stdin".into(),
        );
    }
    *target = Some(value);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceCommand {
    Status,
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Sync,
}

fn parse_service_command(args: &mut VecDeque<String>) -> Result<ServiceCommand, String> {
    let command = match args.pop_front().as_deref() {
        Some("status") => ServiceCommand::Status,
        Some("install") => ServiceCommand::Install,
        Some("uninstall") => ServiceCommand::Uninstall,
        Some("start") => ServiceCommand::Start,
        Some("stop") => ServiceCommand::Stop,
        Some("restart") => ServiceCommand::Restart,
        Some("sync") => ServiceCommand::Sync,
        Some(other) => return Err(format!("未知 service 命令：{other}\n\n{}", service_usage())),
        None => return Err(format!("service 缺少子命令\n\n{}", service_usage())),
    };
    ensure_empty(args, "service")?;
    Ok(command)
}

fn parse_service_run(args: &mut VecDeque<String>) -> Result<Command, String> {
    let config_dir = PathBuf::from(pop_value(args, "service-run")?);
    let owner_sid = args.pop_front();
    let owner_username = args.pop_front();
    if owner_sid.is_some() != owner_username.is_some() {
        return Err("service-run owner SID 与 username 必须同时提供".into());
    }
    ensure_empty(args, "service-run")?;
    Ok(Command::ServiceRun {
        config_dir,
        owner_sid,
        owner_username,
    })
}

fn parse_service_admin_run(args: &mut VecDeque<String>) -> Result<Command, String> {
    let action = pop_value(args, "service-admin-run")?;
    if !matches!(
        action.as_str(),
        "install" | "uninstall" | "start" | "stop" | "restart"
    ) {
        return Err(format!("service-admin-run 不支持操作：{action}"));
    }
    let config_dir = PathBuf::from(pop_value(args, "service-admin-run")?);
    let owner_sid = pop_value(args, "service-admin-run")?;
    let owner_username = pop_value(args, "service-admin-run")?;
    ensure_empty(args, "service-admin-run")?;
    Ok(Command::ServiceAdminRun {
        action,
        config_dir,
        owner_sid,
        owner_username,
    })
}

pub fn config_usage() -> &'static str {
    "Config 命令：\n\
  anchor config get <workspace> [--pending] [--key PATH]\n\
  anchor config diff <workspace> [--set PATH=VALUE ...]\n\
  anchor config set <workspace> --set PATH=VALUE [--set PATH=VALUE ...]\n\
  anchor config apply <workspace> [--wait SECONDS]\n\n\
config set 只写入待应用配置，不改变活动配置或运行态；config apply 才会持久化并协调 daemon/Gateway 运行态。PATH 使用序列化字段名，例如 runtime.local_port、auth.oauth_redirect_hosts、tunnel.type。"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigGetOptions {
    pub workspace: String,
    pub pending: bool,
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigAssignment {
    pub path: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMutationOptions {
    pub workspace: String,
    pub assignments: Vec<ConfigAssignment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigApplyOptions {
    pub workspace: String,
    pub wait_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigCommand {
    Get(ConfigGetOptions),
    Diff(ConfigMutationOptions),
    Set(ConfigMutationOptions),
    Apply(ConfigApplyOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrpTokenInput {
    Inline(String),
    File(PathBuf),
    Stdin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrpAddOptions {
    pub name: String,
    pub server: String,
    pub server_port: u16,
    pub token: Option<FrpTokenInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrpUpdateOptions {
    pub profile: String,
    pub name: Option<String>,
    pub server: Option<String>,
    pub server_port: Option<u16>,
    pub token: Option<FrpTokenInput>,
    pub clear_token: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrpDeleteOptions {
    pub profile: String,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrpCommand {
    List,
    Show { profile: String },
    Add(FrpAddOptions),
    Update(FrpUpdateOptions),
    Delete(FrpDeleteOptions),
}

fn parse_frp_command(args: &mut VecDeque<String>) -> Result<FrpCommand, String> {
    match args.pop_front().as_deref() {
        Some("list") => {
            ensure_empty(args, "frp list")?;
            Ok(FrpCommand::List)
        }
        Some("show") => {
            let profile = pop_value(args, "frp show")?;
            ensure_empty(args, "frp show")?;
            Ok(FrpCommand::Show { profile })
        }
        Some("add") => {
            let name = pop_value(args, "frp add")?;
            let mut server = None;
            let mut server_port = 7000;
            let mut token = None;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--server" => server = Some(pop_value(args, "--server")?),
                    "--port" => {
                        server_port = parse_u64(args, "--port", 1, 65_535)? as u16;
                    }
                    "--token" => set_frp_token_input(
                        &mut token,
                        FrpTokenInput::Inline(pop_value(args, "--token")?),
                    )?,
                    "--token-file" => set_frp_token_input(
                        &mut token,
                        FrpTokenInput::File(PathBuf::from(pop_value(args, "--token-file")?)),
                    )?,
                    "--token-stdin" => {
                        set_frp_token_input(&mut token, FrpTokenInput::Stdin)?;
                    }
                    other => return Err(format!("frp add 不支持参数：{other}")),
                }
            }
            Ok(FrpCommand::Add(FrpAddOptions {
                name,
                server: server.ok_or_else(|| "frp add 缺少 --server".to_string())?,
                server_port,
                token,
            }))
        }
        Some("update") => {
            let profile = pop_value(args, "frp update")?;
            let mut name = None;
            let mut server = None;
            let mut server_port = None;
            let mut token = None;
            let mut clear_token = false;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--name" => name = Some(pop_value(args, "--name")?),
                    "--server" => server = Some(pop_value(args, "--server")?),
                    "--port" => {
                        server_port = Some(parse_u64(args, "--port", 1, 65_535)? as u16);
                    }
                    "--token" => set_frp_token_input(
                        &mut token,
                        FrpTokenInput::Inline(pop_value(args, "--token")?),
                    )?,
                    "--token-file" => set_frp_token_input(
                        &mut token,
                        FrpTokenInput::File(PathBuf::from(pop_value(args, "--token-file")?)),
                    )?,
                    "--token-stdin" => {
                        set_frp_token_input(&mut token, FrpTokenInput::Stdin)?;
                    }
                    "--clear-token" => clear_token = true,
                    other => return Err(format!("frp update 不支持参数：{other}")),
                }
            }
            if token.is_some() && clear_token {
                return Err("frp update 的 --token 与 --clear-token 不能同时使用".into());
            }
            if name.is_none()
                && server.is_none()
                && server_port.is_none()
                && token.is_none()
                && !clear_token
            {
                return Err("frp update 至少需要一个修改参数".into());
            }
            Ok(FrpCommand::Update(FrpUpdateOptions {
                profile,
                name,
                server,
                server_port,
                token,
                clear_token,
            }))
        }
        Some("delete" | "remove") => {
            let profile = pop_value(args, "frp delete")?;
            let mut force = false;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--force" => force = true,
                    other => return Err(format!("frp delete 不支持参数：{other}")),
                }
            }
            Ok(FrpCommand::Delete(FrpDeleteOptions { profile, force }))
        }
        Some(other) => Err(format!("未知 frp 命令：{other}\n\n{}", frp_usage())),
        None => Err(frp_usage().to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelShowOptions {
    pub workspace: String,
    pub service: ServiceSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelConfigureOptions {
    pub workspace: String,
    pub service: ServiceSelection,
    pub tunnel_type: Option<String>,
    pub frp_profile: Option<String>,
    pub clear_frp_profile: bool,
    pub frp_server: Option<String>,
    pub frp_server_port: Option<u16>,
    pub frp_subdomain: Option<String>,
    pub public_url: Option<String>,
    pub frp_proxy_type: Option<String>,
    pub frp_cert_path: Option<String>,
    pub frp_key_path: Option<String>,
    pub cloudflare_mode: Option<String>,
    pub use_proxy: Option<bool>,
    pub apply: bool,
    pub wait_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelCommand {
    Show(TunnelShowOptions),
    Configure(Box<TunnelConfigureOptions>),
}

fn parse_tunnel_command(args: &mut VecDeque<String>) -> Result<TunnelCommand, String> {
    match args.pop_front().as_deref() {
        Some("show") => {
            let workspace = pop_value(args, "tunnel show")?;
            let mut service = ServiceSelection::Mcp;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--service" => {
                        service = ServiceSelection::parse(&pop_value(args, "--service")?)?;
                    }
                    other => return Err(format!("tunnel show 不支持参数：{other}")),
                }
            }
            Ok(TunnelCommand::Show(TunnelShowOptions {
                workspace,
                service,
            }))
        }
        Some("configure" | "config" | "set") => {
            let workspace = pop_value(args, "tunnel configure")?;
            let mut service = ServiceSelection::Mcp;
            let mut tunnel_type = None;
            let mut frp_profile = None;
            let mut clear_frp_profile = false;
            let mut frp_server = None;
            let mut frp_server_port = None;
            let mut frp_subdomain = None;
            let mut public_url = None;
            let mut frp_proxy_type = None;
            let mut frp_cert_path = None;
            let mut frp_key_path = None;
            let mut cloudflare_mode = None;
            let mut use_proxy = None;
            let mut apply = false;
            let mut wait_seconds = 30;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--service" => {
                        service = ServiceSelection::parse(&pop_value(args, "--service")?)?;
                    }
                    "--type" => {
                        let value = pop_value(args, "--type")?;
                        if !matches!(value.as_str(), "frp" | "cloudflare") {
                            return Err("--type 仅支持 frp 或 cloudflare".into());
                        }
                        tunnel_type = Some(value);
                    }
                    "--frp-profile" => frp_profile = Some(pop_value(args, "--frp-profile")?),
                    "--clear-frp-profile" => clear_frp_profile = true,
                    "--frp-server" => frp_server = Some(pop_value(args, "--frp-server")?),
                    "--frp-port" => {
                        frp_server_port = Some(parse_u64(args, "--frp-port", 1, 65_535)? as u16);
                    }
                    "--subdomain" => frp_subdomain = Some(pop_value(args, "--subdomain")?),
                    "--public-url" => public_url = Some(pop_value(args, "--public-url")?),
                    "--clear-public-url" => public_url = Some(String::new()),
                    "--proxy-type" => {
                        let value = pop_value(args, "--proxy-type")?;
                        if !matches!(value.as_str(), "http" | "https2http") {
                            return Err("--proxy-type 仅支持 http 或 https2http".into());
                        }
                        frp_proxy_type = Some(value);
                    }
                    "--cert" => frp_cert_path = Some(pop_value(args, "--cert")?),
                    "--clear-cert" => frp_cert_path = Some(String::new()),
                    "--key" => frp_key_path = Some(pop_value(args, "--key")?),
                    "--clear-key" => frp_key_path = Some(String::new()),
                    "--cloudflare-mode" => {
                        let value = pop_value(args, "--cloudflare-mode")?;
                        if !matches!(value.as_str(), "quick" | "named") {
                            return Err("--cloudflare-mode 仅支持 quick 或 named".into());
                        }
                        cloudflare_mode = Some(value);
                    }
                    "--use-proxy" => use_proxy = Some(true),
                    "--no-proxy" => use_proxy = Some(false),
                    "--apply" => apply = true,
                    "--wait" => wait_seconds = parse_u64(args, "--wait", 1, 300)?,
                    other => return Err(format!("tunnel configure 不支持参数：{other}")),
                }
            }
            if frp_profile.is_some() && clear_frp_profile {
                return Err("--frp-profile 与 --clear-frp-profile 不能同时使用".into());
            }
            if frp_profile.is_some() && frp_server.is_some() {
                return Err(
                    "--frp-profile 与 --frp-server 不能同时使用；全局 profile 与手动服务器二选一"
                        .into(),
                );
            }
            let has_change = tunnel_type.is_some()
                || frp_profile.is_some()
                || clear_frp_profile
                || frp_server.is_some()
                || frp_server_port.is_some()
                || frp_subdomain.is_some()
                || public_url.is_some()
                || frp_proxy_type.is_some()
                || frp_cert_path.is_some()
                || frp_key_path.is_some()
                || cloudflare_mode.is_some()
                || use_proxy.is_some();
            if !has_change && !apply {
                return Err(
                    "tunnel configure 至少需要一个配置参数，或使用 --apply 应用既有待配置".into(),
                );
            }
            Ok(TunnelCommand::Configure(Box::new(TunnelConfigureOptions {
                workspace,
                service,
                tunnel_type,
                frp_profile,
                clear_frp_profile,
                frp_server,
                frp_server_port,
                frp_subdomain,
                public_url,
                frp_proxy_type,
                frp_cert_path,
                frp_key_path,
                cloudflare_mode,
                use_proxy,
                apply,
                wait_seconds,
            })))
        }
        Some(other) => Err(format!("未知 tunnel 命令：{other}\n\n{}", tunnel_usage())),
        None => Err(tunnel_usage().to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoftwareCommand {
    List,
    Install { kind: String },
    Uninstall { kind: String },
}

fn parse_software_kind(args: &mut VecDeque<String>, command: &str) -> Result<String, String> {
    let kind = pop_value(args, command)?;
    if !matches!(kind.as_str(), "frpc" | "cloudflared") {
        return Err(format!(
            "{command} 仅支持 frpc 或 cloudflared，收到：{kind}"
        ));
    }
    ensure_empty(args, command)?;
    Ok(kind)
}

fn parse_software_command(args: &mut VecDeque<String>) -> Result<SoftwareCommand, String> {
    match args.pop_front().as_deref() {
        Some("list" | "status") => {
            ensure_empty(args, "software list")?;
            Ok(SoftwareCommand::List)
        }
        Some("install") => Ok(SoftwareCommand::Install {
            kind: parse_software_kind(args, "software install")?,
        }),
        Some("uninstall" | "remove") => Ok(SoftwareCommand::Uninstall {
            kind: parse_software_kind(args, "software uninstall")?,
        }),
        Some(other) => Err(format!(
            "未知 software 命令：{other}\n\n{}",
            software_usage()
        )),
        None => Err(software_usage().to_string()),
    }
}

fn parse_config_assignment(raw: String) -> Result<ConfigAssignment, String> {
    let Some((path, value)) = raw.split_once('=') else {
        return Err("--set 必须使用 PATH=VALUE 格式".into());
    };
    let path = path.trim();
    if path.is_empty() {
        return Err("--set 的 PATH 不能为空".into());
    }

    Ok(ConfigAssignment {
        path: path.to_string(),
        value: value.to_string(),
    })
}

fn parse_config_mutation(
    args: &mut VecDeque<String>,
    command: &str,
    require_assignment: bool,
) -> Result<ConfigMutationOptions, String> {
    let workspace = pop_value(args, command)?;
    let mut assignments = Vec::new();
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--set" => assignments.push(parse_config_assignment(pop_value(args, "--set")?)?),
            other => return Err(format!("{command} 不支持参数：{other}")),
        }
    }
    if require_assignment && assignments.is_empty() {
        return Err(format!("{command} 至少需要一个 --set PATH=VALUE"));
    }
    Ok(ConfigMutationOptions {
        workspace,
        assignments,
    })
}

fn parse_config_command(args: &mut VecDeque<String>) -> Result<ConfigCommand, String> {
    match args.pop_front().as_deref() {
        Some("get") => {
            let workspace = pop_value(args, "config get")?;
            let mut pending = false;
            let mut key = None;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--pending" => pending = true,
                    "--key" => key = Some(pop_value(args, "--key")?),
                    other => return Err(format!("config get 不支持参数：{other}")),
                }
            }
            Ok(ConfigCommand::Get(ConfigGetOptions {
                workspace,
                pending,
                key,
            }))
        }
        Some("diff") => Ok(ConfigCommand::Diff(parse_config_mutation(
            args,
            "config diff",
            false,
        )?)),
        Some("set") => Ok(ConfigCommand::Set(parse_config_mutation(
            args,
            "config set",
            true,
        )?)),
        Some("apply") => {
            let workspace = pop_value(args, "config apply")?;
            let mut wait_seconds = 30;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--wait" => wait_seconds = parse_u64(args, "--wait", 1, 300)?,
                    other => return Err(format!("config apply 不支持参数：{other}")),
                }
            }
            Ok(ConfigCommand::Apply(ConfigApplyOptions {
                workspace,
                wait_seconds,
            }))
        }
        Some(other) => Err(format!("未知 config 命令：{other}\n\n{}", config_usage())),
        None => Err(config_usage().to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayLogsOptions {
    pub lines: usize,
    pub follow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayEventsOptions {
    pub follow: bool,
    pub wait_seconds: u64,
}

fn parse_gateway_daemon_run(args: &mut VecDeque<String>) -> Result<Command, String> {
    let config_scope = pop_value(args, "gateway-daemon-run")?;
    let mut workspaces = Vec::new();
    while let Some(workspace) = args.pop_front() {
        if workspace.starts_with('-') {
            return Err(format!("gateway-daemon-run 不支持参数：{workspace}"));
        }
        workspaces.push(workspace);
    }
    if workspaces.is_empty() {
        return Err("gateway-daemon-run 至少需要一个 workspace".into());
    }
    Ok(Command::GatewayDaemonRun {
        config_scope,
        workspaces,
    })
}

fn parse_events(args: &mut VecDeque<String>) -> Result<EventsOptions, String> {
    let target = match pop_value(args, "events")?.as_str() {
        "--control-plane" => EventsTarget::ControlPlane,
        workspace => EventsTarget::Workspace(workspace.to_string()),
    };
    let mut follow = false;
    let mut wait_seconds = 15;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--follow" | "-f" => follow = true,
            "--wait" => wait_seconds = parse_u64(args, "--wait", 1, 25)?,
            other => return Err(format!("events 不支持参数：{other}")),
        }
    }
    Ok(EventsOptions {
        target,
        follow,
        wait_seconds,
    })
}

fn parse_reload(args: &mut VecDeque<String>) -> Result<ReloadOptions, String> {
    let workspace = pop_value(args, "reload")?;
    let mut service = ServiceSelection::Mcp;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--service" => service = ServiceSelection::parse(&pop_value(args, "--service")?)?,
            other => return Err(format!("reload 不支持参数：{other}")),
        }
    }
    Ok(ReloadOptions { workspace, service })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayConfigureOptions {
    pub enabled: Option<bool>,
    pub local_port: Option<u16>,
    pub owner_workspace: Option<String>,
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayStartOptions {
    pub workspaces: Vec<String>,
    pub wait_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayStopOptions {
    pub timeout_seconds: u64,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayCommand {
    Show,
    Status,
    Configure(GatewayConfigureOptions),
    Serve { workspaces: Vec<String> },
    Start(GatewayStartOptions),
    Stop(GatewayStopOptions),
    Restart(GatewayStopOptions),
    Reload,
    Logs(GatewayLogsOptions),
    Events(GatewayEventsOptions),
}

fn parse_gateway_command(args: &mut VecDeque<String>) -> Result<GatewayCommand, String> {
    match args.pop_front().as_deref() {
        Some("show") => {
            ensure_empty(args, "gateway show")?;
            Ok(GatewayCommand::Show)
        }
        Some("status") => {
            ensure_empty(args, "gateway status")?;
            Ok(GatewayCommand::Status)
        }
        Some("configure" | "config") => {
            let mut enabled = None;
            let mut local_port = None;
            let mut owner_workspace = None;
            let mut public_url = None;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--enable" => enabled = Some(true),
                    "--disable" => enabled = Some(false),
                    "--port" => local_port = Some(parse_u64(args, "--port", 1, 65_535)? as u16),
                    "--owner" => owner_workspace = Some(pop_value(args, "--owner")?),
                    "--public-url" => public_url = Some(pop_value(args, "--public-url")?),
                    "--clear-public-url" => public_url = Some(String::new()),
                    other => return Err(format!("gateway configure 不支持参数：{other}")),
                }
            }
            if enabled.is_none()
                && local_port.is_none()
                && owner_workspace.is_none()
                && public_url.is_none()
            {
                return Err("gateway configure 至少需要一个配置参数".into());
            }
            Ok(GatewayCommand::Configure(GatewayConfigureOptions {
                enabled,
                local_port,
                owner_workspace,
                public_url,
            }))
        }
        Some("serve") => {
            let mut workspaces = Vec::new();
            while let Some(value) = args.pop_front() {
                if value.starts_with('-') {
                    return Err(format!("gateway serve 不支持参数：{value}"));
                }
                workspaces.push(value);
            }
            if workspaces.is_empty() {
                return Err("gateway serve 至少需要一个 workspace".into());
            }
            Ok(GatewayCommand::Serve { workspaces })
        }
        Some("start") => {
            let mut workspaces = Vec::new();
            let mut wait_seconds = 10;
            while let Some(value) = args.pop_front() {
                match value.as_str() {
                    "--wait" => wait_seconds = parse_u64(args, "--wait", 1, 120)?,
                    _ if value.starts_with('-') => {
                        return Err(format!("gateway start 不支持参数：{value}"))
                    }
                    _ => workspaces.push(value),
                }
            }
            if workspaces.is_empty() {
                return Err("gateway start 至少需要一个 workspace".into());
            }
            Ok(GatewayCommand::Start(GatewayStartOptions {
                workspaces,
                wait_seconds,
            }))
        }
        Some("stop") => Ok(GatewayCommand::Stop(parse_gateway_stop_options(
            args,
            "gateway stop",
        )?)),
        Some("restart") => Ok(GatewayCommand::Restart(parse_gateway_stop_options(
            args,
            "gateway restart",
        )?)),
        Some("reload") => {
            ensure_empty(args, "gateway reload")?;
            Ok(GatewayCommand::Reload)
        }
        Some("logs") => {
            let mut lines = 100usize;
            let mut follow = false;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--lines" => lines = parse_u64(args, "--lines", 1, 10_000)? as usize,
                    "--follow" | "-f" => follow = true,
                    other => return Err(format!("gateway logs 不支持参数：{other}")),
                }
            }
            Ok(GatewayCommand::Logs(GatewayLogsOptions { lines, follow }))
        }
        Some("events") => {
            let mut follow = false;
            let mut wait_seconds = 15;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--follow" | "-f" => follow = true,
                    "--wait" => wait_seconds = parse_u64(args, "--wait", 1, 25)?,
                    other => return Err(format!("gateway events 不支持参数：{other}")),
                }
            }
            Ok(GatewayCommand::Events(GatewayEventsOptions {
                follow,
                wait_seconds,
            }))
        }
        Some(other) => Err(format!("未知 gateway 命令：{other}\n\n{}", gateway_usage())),
        None => Err(format!("gateway 缺少子命令\n\n{}", gateway_usage())),
    }
}

fn parse_gateway_stop_options(
    args: &mut VecDeque<String>,
    command: &str,
) -> Result<GatewayStopOptions, String> {
    let mut timeout_seconds = 10;
    let mut force = false;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--timeout" => timeout_seconds = parse_u64(args, "--timeout", 1, 120)?,
            "--force" => force = true,
            other => return Err(format!("{command} 不支持参数：{other}")),
        }
    }
    Ok(GatewayStopOptions {
        timeout_seconds,
        force,
    })
}

fn parse_workspace_command(args: &mut VecDeque<String>) -> Result<WorkspaceCommand, String> {
    match args.pop_front().as_deref() {
        Some("list" | "ls") => {
            ensure_empty(args, "workspace list")?;
            Ok(WorkspaceCommand::List)
        }
        Some("register" | "add") => {
            let path = pop_value(args, "workspace register")?;
            let mut name = None;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--name" => name = Some(pop_value(args, "--name")?),
                    other => return Err(format!("workspace register 不支持参数：{other}")),
                }
            }
            Ok(WorkspaceCommand::Register(RegisterOptions { path, name }))
        }
        Some("unregister" | "delete" | "remove") => {
            let workspace = pop_value(args, "workspace unregister")?;
            let mut force = false;
            let mut timeout_seconds = 10;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--force" => force = true,
                    "--timeout" => {
                        timeout_seconds = parse_u64(args, "--timeout", 1, 300)?;
                    }
                    other => return Err(format!("workspace unregister 不支持参数：{other}")),
                }
            }
            Ok(WorkspaceCommand::Unregister(UnregisterOptions {
                workspace,
                force,
                timeout_seconds,
            }))
        }
        Some("show" | "view") => {
            let workspace = pop_value(args, "workspace show")?;
            ensure_empty(args, "workspace show")?;
            Ok(WorkspaceCommand::Show { workspace })
        }
        Some("start") => Ok(WorkspaceCommand::Start(parse_run_options(
            args,
            "workspace start",
        )?)),
        Some("stop") => Ok(WorkspaceCommand::Stop(parse_stop_named(
            args,
            "workspace stop",
        )?)),
        Some("gpt-config" | "gpt") => {
            let workspace = pop_value(args, "workspace gpt-config")?;
            let mut service = ServiceSelection::Mcp;
            let mut endpoint = EndpointSelection::Auto;
            let mut show_secrets = false;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--service" => {
                        service = ServiceSelection::parse(&pop_value(args, "--service")?)?;
                    }
                    "--endpoint" => {
                        endpoint = EndpointSelection::parse(&pop_value(args, "--endpoint")?)?;
                    }
                    "--local" => endpoint = EndpointSelection::Local,
                    "--public" => endpoint = EndpointSelection::Public,
                    "--show-secrets" => show_secrets = true,
                    other => return Err(format!("workspace gpt-config 不支持参数：{other}")),
                }
            }
            Ok(WorkspaceCommand::GptConfig(GptConfigOptions {
                workspace,
                service,
                endpoint,
                show_secrets,
            }))
        }
        Some("test") => {
            let workspace = pop_value(args, "workspace test")?;
            let mut service = ServiceSelection::Mcp;
            let mut endpoint = EndpointSelection::Auto;
            let mut timeout_seconds = 10;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--service" => {
                        service = ServiceSelection::parse(&pop_value(args, "--service")?)?;
                    }
                    "--endpoint" => {
                        endpoint = EndpointSelection::parse(&pop_value(args, "--endpoint")?)?;
                    }
                    "--local" => endpoint = EndpointSelection::Local,
                    "--public" => endpoint = EndpointSelection::Public,
                    "--timeout" => {
                        timeout_seconds = parse_u64(args, "--timeout", 1, 120)?;
                    }
                    other => return Err(format!("workspace test 不支持参数：{other}")),
                }
            }
            Ok(WorkspaceCommand::Test(WorkspaceTestOptions {
                workspace,
                service,
                endpoint,
                timeout_seconds,
            }))
        }
        Some(other) => Err(format!(
            "未知 workspace 命令：{other}\n\n{}",
            workspace_usage()
        )),
        None => Err(format!("workspace 缺少子命令\n\n{}", workspace_usage())),
    }
}

fn parse_stop_named(args: &mut VecDeque<String>, command: &str) -> Result<StopOptions, String> {
    let workspace = pop_value(args, command)?;
    let mut timeout_seconds = 10;
    let mut force = false;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--timeout" => timeout_seconds = parse_u64(args, "--timeout", 1, 300)?,
            "--force" => force = true,
            other => return Err(format!("{command} 不支持参数：{other}")),
        }
    }
    Ok(StopOptions {
        workspace,
        timeout_seconds,
        force,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSelection {
    Auto,
    Local,
    Public,
}

impl EndpointSelection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Local => "local",
            Self::Public => "public",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "local" => Ok(Self::Local),
            "public" => Ok(Self::Public),
            _ => Err(format!(
                "无效 endpoint：{value}；可选值为 auto、local、public"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterOptions {
    pub path: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisterOptions {
    pub workspace: String,
    pub force: bool,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GptConfigOptions {
    pub workspace: String,
    pub service: ServiceSelection,
    pub endpoint: EndpointSelection,
    pub show_secrets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceTestOptions {
    pub workspace: String,
    pub service: ServiceSelection,
    pub endpoint: EndpointSelection,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceCommand {
    List,
    Register(RegisterOptions),
    Unregister(UnregisterOptions),
    Show { workspace: String },
    Start(RunOptions),
    Stop(StopOptions),
    GptConfig(GptConfigOptions),
    Test(WorkspaceTestOptions),
}

fn parse_run_options(args: &mut VecDeque<String>, command: &str) -> Result<RunOptions, String> {
    let workspace = pop_value(args, command)?;
    let mut service = None;
    let mut tunnel = None;
    let mut wait_seconds = 10;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--service" => {
                service = Some(ServiceSelection::parse(&pop_value(args, "--service")?)?);
            }
            "--tunnel" => tunnel = Some(true),
            "--no-tunnel" => tunnel = Some(false),
            "--wait" => wait_seconds = parse_u64(args, "--wait", 1, 300)?,
            other => return Err(format!("{command} 不支持参数：{other}")),
        }
    }
    Ok(RunOptions {
        workspace,
        service,
        tunnel,
        wait_seconds,
    })
}

fn parse_stop(args: &mut VecDeque<String>) -> Result<StopOptions, String> {
    let workspace = pop_value(args, "stop")?;
    let mut timeout_seconds = 10;
    let mut force = false;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--timeout" => timeout_seconds = parse_u64(args, "--timeout", 1, 300)?,
            "--force" => force = true,
            other => return Err(format!("stop 不支持参数：{other}")),
        }
    }
    Ok(StopOptions {
        workspace,
        timeout_seconds,
        force,
    })
}

fn parse_status(args: &mut VecDeque<String>) -> Result<StatusOptions, String> {
    let mut workspace = None;
    let mut all = false;
    let mut control_plane = false;
    let mut watch = false;
    let mut interval_seconds = 2;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--all" => all = true,
            "--control-plane" => control_plane = true,
            "--watch" => watch = true,
            "--interval" => interval_seconds = parse_u64(args, "--interval", 1, 60)?,
            other if other.starts_with('-') => {
                return Err(format!("status 不支持参数：{other}"));
            }
            other if workspace.is_none() => workspace = Some(other.to_string()),
            other => return Err(format!("status 不支持多余参数：{other}")),
        }
    }
    if all && workspace.is_some() {
        return Err("status 不能同时指定 workspace 和 --all".into());
    }
    if control_plane && (all || workspace.is_some()) {
        return Err("status --control-plane 不能同时指定 workspace 或 --all".into());
    }
    Ok(StatusOptions {
        workspace: if all || control_plane {
            None
        } else {
            workspace
        },
        watch,
        interval_seconds,
        control_plane,
    })
}

fn parse_logs(args: &mut VecDeque<String>) -> Result<LogsOptions, String> {
    let workspace = pop_value(args, "logs")?;
    let mut service = LogSelection::Daemon;
    let mut lines = 100usize;
    let mut follow = false;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--service" => service = LogSelection::parse(&pop_value(args, "--service")?)?,
            "--lines" => lines = parse_u64(args, "--lines", 1, 10_000)? as usize,
            "--follow" | "-f" => follow = true,
            other => return Err(format!("logs 不支持参数：{other}")),
        }
    }
    if follow && service == LogSelection::All {
        return Err("logs --follow 暂不支持 --service all，请选择单个服务".into());
    }
    Ok(LogsOptions {
        workspace,
        service,
        lines,
        follow,
    })
}

fn parse_daemon_run(args: &mut VecDeque<String>) -> Result<Command, String> {
    let workspace = pop_value(args, "daemon-run")?;
    let mut service = ServiceSelection::Mcp;
    let mut tunnel_services = None;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--service" => {
                service = ServiceSelection::parse(&pop_value(args, "--service")?)?;
            }
            "--tunnel" => tunnel_services = Some(service),
            "--no-tunnel" => tunnel_services = None,
            "--tunnel-service" => {
                tunnel_services = Some(ServiceSelection::parse(&pop_value(
                    args,
                    "--tunnel-service",
                )?)?);
            }
            other => return Err(format!("daemon-run 不支持参数：{other}")),
        }
    }
    Ok(Command::DaemonRun {
        workspace,
        service,
        tunnel_services,
    })
}

fn parse_u64(
    args: &mut VecDeque<String>,
    option: &str,
    minimum: u64,
    maximum: u64,
) -> Result<u64, String> {
    let raw = pop_value(args, option)?;
    let value = raw
        .parse::<u64>()
        .map_err(|_| format!("{option} 必须是整数：{raw}"))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("{option} 必须在 {minimum}-{maximum} 之间"));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    pub workspace: String,
    pub service: Option<ServiceSelection>,
    pub tunnel: Option<bool>,
    pub wait_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopOptions {
    pub workspace: String,
    pub timeout_seconds: u64,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusOptions {
    pub workspace: Option<String>,
    pub watch: bool,
    pub interval_seconds: u64,
    pub control_plane: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSelection {
    Daemon,
    Mcp,
    Actions,
    All,
}

impl LogSelection {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "daemon" => Ok(Self::Daemon),
            "mcp" => Ok(Self::Mcp),
            "actions" => Ok(Self::Actions),
            "all" => Ok(Self::All),
            _ => Err(format!(
                "无效日志类型：{value}；可选值为 daemon、mcp、actions、all"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsOptions {
    pub workspace: String,
    pub service: LogSelection,
    pub lines: usize,
    pub follow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventsTarget {
    Workspace(String),
    ControlPlane,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventsOptions {
    pub target: EventsTarget,
    pub follow: bool,
    pub wait_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadOptions {
    pub workspace: String,
    pub service: ServiceSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPackageOptions {
    pub workspace: String,
    pub app_id: String,
    pub output: Option<PathBuf>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginCommand {
    Package(PluginPackageOptions),
}

fn parse_plugin_command(args: &mut VecDeque<String>) -> Result<PluginCommand, String> {
    match args.pop_front().as_deref() {
        Some("package") => {
            let workspace = pop_value(args, "plugin package")?;
            let mut app_id = None;
            let mut output = None;
            let mut name = None;
            while let Some(option) = args.pop_front() {
                match option.as_str() {
                    "--app-id" => app_id = Some(pop_value(args, "--app-id")?),
                    "--output" => output = Some(PathBuf::from(pop_value(args, "--output")?)),
                    "--name" => name = Some(pop_value(args, "--name")?),
                    other => return Err(format!("plugin package 不支持参数：{other}")),
                }
            }
            Ok(PluginCommand::Package(PluginPackageOptions {
                workspace,
                app_id: app_id.ok_or_else(|| {
                    "plugin package 缺少 --app-id；请使用 ChatGPT Developer mode 注册 MCP 后浏览器 URL 中的 plugin_asdk_app... technical ID".to_string()
                })?,
                output,
                name,
            }))
        }
        Some(other) => Err(format!("未知 plugin 命令：{other}\n\n{}", plugin_usage())),
        None => Err(plugin_usage().to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    List,
    Show {
        workspace: String,
    },
    Status(StatusOptions),
    Serve {
        workspace: String,
        service: ServiceSelection,
        tunnel: bool,
    },
    Start(RunOptions),
    Stop(StopOptions),
    Restart(RunOptions),
    Logs(LogsOptions),
    Events(EventsOptions),
    Reload(ReloadOptions),
    Doctor {
        workspace: String,
    },
    Config(ConfigCommand),
    Frp(FrpCommand),
    Tunnel(TunnelCommand),
    Software(SoftwareCommand),
    Workspace(WorkspaceCommand),
    Plugin(PluginCommand),
    Gateway(GatewayCommand),
    Service(ServiceCommand),
    ServiceRun {
        config_dir: PathBuf,
        owner_sid: Option<String>,
        owner_username: Option<String>,
    },
    ServiceAdminRun {
        action: String,
        config_dir: PathBuf,
        owner_sid: String,
        owner_username: String,
    },
    GatewayDaemonRun {
        config_scope: String,
        workspaces: Vec<String>,
    },
    DaemonRun {
        workspace: String,
        service: ServiceSelection,
        tunnel_services: Option<ServiceSelection>,
    },
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<CliArgs, String> {
    let mut args: VecDeque<String> = args.into_iter().collect();
    let mut config_dir = None;
    let mut json = false;

    loop {
        match args.front().map(String::as_str) {
            Some("--config-dir") => {
                args.pop_front();
                config_dir = Some(PathBuf::from(pop_value(&mut args, "--config-dir")?));
            }
            Some("--json") => {
                args.pop_front();
                json = true;
            }
            _ => break,
        }
    }

    let command = match args.pop_front().as_deref() {
        None | Some("help" | "--help" | "-h") => Command::Help,
        Some("version" | "--version" | "-V") => Command::Version,
        Some("list" | "workspaces") => {
            ensure_empty(&args, "list")?;
            Command::List
        }
        Some("show") => {
            let workspace = pop_value(&mut args, "show")?;
            ensure_empty(&args, "show")?;
            Command::Show { workspace }
        }
        Some("status") => Command::Status(parse_status(&mut args)?),
        Some("serve") => parse_serve(&mut args)?,
        Some("start") => Command::Start(parse_run_options(&mut args, "start")?),
        Some("stop") => Command::Stop(parse_stop(&mut args)?),
        Some("restart") => Command::Restart(parse_run_options(&mut args, "restart")?),
        Some("logs") => Command::Logs(parse_logs(&mut args)?),
        Some("events") => Command::Events(parse_events(&mut args)?),
        Some("reload") => Command::Reload(parse_reload(&mut args)?),
        Some("doctor") => {
            let workspace = pop_value(&mut args, "doctor")?;
            ensure_empty(&args, "doctor")?;
            Command::Doctor { workspace }
        }
        Some("config" | "cfg") => Command::Config(parse_config_command(&mut args)?),
        Some("frp") => Command::Frp(parse_frp_command(&mut args)?),
        Some("tunnel") => Command::Tunnel(parse_tunnel_command(&mut args)?),
        Some("software" | "sw") => Command::Software(parse_software_command(&mut args)?),
        Some("workspace" | "ws") => Command::Workspace(parse_workspace_command(&mut args)?),
        Some("plugin") => Command::Plugin(parse_plugin_command(&mut args)?),
        Some("gateway" | "gw") => Command::Gateway(parse_gateway_command(&mut args)?),
        Some("service" | "svc") => Command::Service(parse_service_command(&mut args)?),
        Some("service-run") => parse_service_run(&mut args)?,
        Some("service-admin-run") => parse_service_admin_run(&mut args)?,
        Some("gateway-daemon-run") => parse_gateway_daemon_run(&mut args)?,
        Some("daemon-run") => parse_daemon_run(&mut args)?,
        Some(other) => return Err(format!("未知命令：{other}\n\n{}", usage())),
    };

    Ok(CliArgs {
        config_dir,
        json,
        command,
    })
}

fn parse_serve(args: &mut VecDeque<String>) -> Result<Command, String> {
    let workspace = pop_value(args, "serve")?;
    let mut service = ServiceSelection::Mcp;
    let mut tunnel = false;

    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--service" => {
                service = ServiceSelection::parse(&pop_value(args, "--service")?)?;
            }
            "--tunnel" => tunnel = true,
            other => return Err(format!("serve 不支持参数：{other}")),
        }
    }

    Ok(Command::Serve {
        workspace,
        service,
        tunnel,
    })
}

fn pop_value(args: &mut VecDeque<String>, option: &str) -> Result<String, String> {
    args.pop_front()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{option} 缺少参数"))
}

fn ensure_empty(args: &VecDeque<String>, command: &str) -> Result<(), String> {
    if let Some(value) = args.front() {
        Err(format!("{command} 不支持多余参数：{value}"))
    } else {
        Ok(())
    }
}

pub fn usage() -> &'static str {
    "Anchor CLI\n\n\
用法：\n\
  anchor [--config-dir PATH] [--json] list\n\
  anchor [--config-dir PATH] [--json] show <workspace>\n\
  anchor [--config-dir PATH] [--json] status [<workspace>|--all|--control-plane] [--watch]\n\
  anchor [--config-dir PATH] [--json] serve <workspace> [--service mcp|actions|all] [--tunnel]\n\n\
  anchor [--config-dir PATH] [--json] start <workspace> [--service mcp|actions|all] [--tunnel]\n\
  anchor [--config-dir PATH] [--json] stop <workspace> [--timeout SECONDS] [--force]\n\
  anchor [--config-dir PATH] [--json] restart <workspace> [--service mcp|actions|all] [--tunnel]\n\
  anchor [--config-dir PATH] [--json] logs <workspace> [--service daemon|mcp|actions|all] [--lines N] [-f]\n\
  anchor [--config-dir PATH] [--json] events <workspace|--control-plane> [-f] [--wait SECONDS]\n\
  anchor [--config-dir PATH] [--json] reload <workspace> [--service mcp|actions|all]\n\
  anchor [--config-dir PATH] [--json] doctor <workspace>\n\n\
  anchor [--config-dir PATH] [--json] config <get|diff|set|apply> ...\n\n\
  anchor [--config-dir PATH] [--json] frp <list|show|add|update|delete> ...\n\n\
  anchor [--config-dir PATH] [--json] tunnel <show|configure> ...\n\n\
  anchor [--config-dir PATH] [--json] software <list|install|uninstall> ...\n\n\
  anchor [--config-dir PATH] [--json] workspace <command> ...\n\n\
  anchor [--config-dir PATH] [--json] plugin <command> ...\n\n\
  anchor [--config-dir PATH] [--json] gateway <command> ...\n\n\
  anchor [--config-dir PATH] [--json] service <command>\n\n\
workspace 可使用 profile ID、唯一名称或项目路径。\n\
不指定 workspace 的 status 会显示全部工作区。serve 为前台调试模式；start/stop/restart 管理 Windows/Linux 每工作区后台 daemon。CLI 不会接管 GUI 或其他进程占用的端口。"
}

pub fn frp_usage() -> &'static str {
    "FRP Profile 命令：\n\
  anchor frp list\n\
  anchor frp show <profile-id|name>\n\
  anchor frp add <name> --server HOST [--port PORT] [--token-file PATH|--token-stdin|--token TOKEN]\n\
  anchor frp update <profile-id|name> [--name NAME] [--server HOST] [--port PORT]\n\
      [--token-file PATH|--token-stdin|--token TOKEN|--clear-token]\n\
  anchor frp delete <profile-id|name> --force\n\n\
FRP profile 是全局服务器连接配置；token 作为受保护 secret 保存且不会在 list/show 输出中回显。优先使用 --token-file 或 --token-stdin，避免 secret 进入 shell history/进程参数；--token 仅为兼容便捷场景保留。workspace tunnel 通过 profile ID 引用它。"
}

pub fn tunnel_usage() -> &'static str {
    "Tunnel 配置命令：\n\
  anchor tunnel show <workspace> [--service mcp|actions|all]\n\
  anchor tunnel configure <workspace> [--service mcp|actions|all] [--type frp|cloudflare]\n\
      [--frp-profile PROFILE|--frp-server HOST] [--clear-frp-profile] [--frp-port PORT]\n\
      [--subdomain NAME] [--public-url URL|--clear-public-url]\n\
      [--proxy-type http|https2http] [--cert PATH|--clear-cert] [--key PATH|--clear-key]\n\
      [--cloudflare-mode quick|named] [--use-proxy|--no-proxy] [--apply] [--wait SECONDS]\n\n\
configure 复用 config pending/apply 事务模型；FRP 参数会自动切换为 frp 类型。使用 --apply 时会在落盘后协调正在运行的 Workspace daemon/Gateway，并在失败时回滚。"
}

pub fn software_usage() -> &'static str {
    "Tunnel Software 命令：\n\
  anchor software list\n\
  anchor software install <frpc|cloudflared>\n\
  anchor software uninstall <frpc|cloudflared>\n\n\
install 会把指定二进制下载到 Anchor 管理的缓存目录，并复用 download.github_mirror / download.proxy_mode 配置。uninstall 只删除 Anchor 自己缓存的副本，不会删除 PATH、apt、brew、winget 等系统安装。"
}

pub fn plugin_usage() -> &'static str {
    "ChatGPT Plugin 命令：\n\
  anchor plugin package <workspace> --app-id plugin_asdk_app... [--name NAME] [--output PATH]\n\n\
将当前 Workspace 配置的 Agent Skills 导出为 ChatGPT/Codex Plugin 静态快照，并生成 .codex-plugin/plugin.json、.app.json 与本地 marketplace.json。--app-id 来自 ChatGPT Developer mode 中已注册 MCP app 的浏览器 URL。默认输出到 Workspace 的 .anchor/chatgpt-plugin-marketplace。"
}

pub fn gateway_usage() -> &'static str {
    "Gateway 命令：\n\
  anchor gateway show\n\
  anchor gateway status\n\
  anchor gateway configure [--enable|--disable] [--port PORT] [--owner WORKSPACE] [--public-url URL|--clear-public-url]\n\
  anchor gateway start <workspace> [workspace ...] [--wait SECONDS]\n\
  anchor gateway stop [--timeout SECONDS] [--force]\n\
  anchor gateway restart [--timeout SECONDS] [--force]\n\
  anchor gateway reload\n\
  anchor gateway logs [--lines N] [--follow|-f]\n\
  anchor gateway events [--follow|-f] [--wait SECONDS]\n\
  anchor gateway serve <workspace> [workspace ...]\n\n\
gateway start/stop/restart 管理独立全局 Gateway daemon；gateway serve 保留为前台调试/外部 supervisor 模式。"
}

pub fn service_usage() -> &'static str {
    "Windows SCM Service 命令：\n\
  anchor service status\n\
  anchor service install\n\
  anchor service uninstall\n\
  anchor service start\n\
  anchor service stop\n\
  anchor service restart\n\
  anchor service sync\n\n\
install 会创建当前配置域专属的自动启动 SCM service，并保留/捕获当前 Workspace/Gateway 后台运行计划；sync 将当前运行态刷新为下次开机自动启动计划。install/uninstall 通常需要管理员权限。"
}

pub fn workspace_usage() -> &'static str {
    "Workspace 命令：\n\
  anchor workspace list\n\
  anchor workspace register <path> [--name NAME]\n\
  anchor workspace unregister <workspace> --force [--timeout SECONDS]\n\
  anchor workspace show <workspace>\n\
  anchor workspace start <workspace> [--service mcp|actions|all] [--tunnel]\n\
  anchor workspace stop <workspace> [--timeout SECONDS] [--force]\n\
  anchor workspace gpt-config <workspace> [--service mcp|actions|all] [--endpoint auto|local|public] [--show-secrets]\n\
  anchor workspace test <workspace> [--service mcp|actions|all] [--endpoint auto|local|public] [--timeout SECONDS]"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_tunnel_software_management_commands() {
        assert_eq!(
            parse(strings(&["software", "list"]))
                .expect("software list")
                .command,
            Command::Software(SoftwareCommand::List)
        );
        assert_eq!(
            parse(strings(&["software", "install", "frpc"]))
                .expect("software install")
                .command,
            Command::Software(SoftwareCommand::Install {
                kind: "frpc".into()
            })
        );
        assert_eq!(
            parse(strings(&["sw", "remove", "cloudflared"]))
                .expect("software remove")
                .command,
            Command::Software(SoftwareCommand::Uninstall {
                kind: "cloudflared".into()
            })
        );

        let error =
            parse(strings(&["software", "install", "other"])).expect_err("unknown software kind");
        assert!(error.contains("frpc 或 cloudflared"));
    }

    #[test]
    fn frp_token_input_modes_are_mutually_exclusive() {
        let parsed = parse(strings(&[
            "frp",
            "add",
            "prod",
            "--server",
            "frp.example.com",
            "--token-file",
            "token.txt",
        ]))
        .expect("token file parse");
        assert_eq!(
            parsed.command,
            Command::Frp(FrpCommand::Add(FrpAddOptions {
                name: "prod".into(),
                server: "frp.example.com".into(),
                server_port: 7000,
                token: Some(FrpTokenInput::File(PathBuf::from("token.txt"))),
            }))
        );

        let error = parse(strings(&[
            "frp",
            "add",
            "prod",
            "--server",
            "frp.example.com",
            "--token-stdin",
            "--token",
            "secret",
        ]))
        .expect_err("conflicting token sources");
        assert!(error.contains("只能选择一种"));
    }

    #[test]
    fn parses_frp_profile_crud_commands() {
        let add = parse(strings(&[
            "frp",
            "add",
            "prod",
            "--server",
            "43.157.17.95",
            "--port",
            "17001",
            "--token",
            "secret",
        ]))
        .expect("frp add");
        assert_eq!(
            add.command,
            Command::Frp(FrpCommand::Add(FrpAddOptions {
                name: "prod".into(),
                server: "43.157.17.95".into(),
                server_port: 17_001,
                token: Some(FrpTokenInput::Inline("secret".into())),
            }))
        );

        let update =
            parse(strings(&["frp", "update", "prod", "--clear-token"])).expect("frp update");
        assert_eq!(
            update.command,
            Command::Frp(FrpCommand::Update(FrpUpdateOptions {
                profile: "prod".into(),
                name: None,
                server: None,
                server_port: None,
                token: None,
                clear_token: true,
            }))
        );

        let delete = parse(strings(&["frp", "delete", "prod", "--force"])).expect("frp delete");
        assert_eq!(
            delete.command,
            Command::Frp(FrpCommand::Delete(FrpDeleteOptions {
                profile: "prod".into(),
                force: true,
            }))
        );
    }

    #[test]
    fn parses_tunnel_frp_configuration_and_apply() {
        let parsed = parse(strings(&[
            "tunnel",
            "configure",
            "demo",
            "--service",
            "all",
            "--frp-profile",
            "prod",
            "--subdomain",
            "anchor",
            "--proxy-type",
            "https2http",
            "--public-url",
            "https://anchor.taoyan.icu",
            "--cert",
            ".anchor/cert/server.pem",
            "--key",
            ".anchor/cert/server.key",
            "--no-proxy",
            "--apply",
            "--wait",
            "45",
        ]))
        .expect("tunnel configure");

        assert_eq!(
            parsed.command,
            Command::Tunnel(TunnelCommand::Configure(Box::new(TunnelConfigureOptions {
                workspace: "demo".into(),
                service: ServiceSelection::All,
                tunnel_type: None,
                frp_profile: Some("prod".into()),
                clear_frp_profile: false,
                frp_server: None,
                frp_server_port: None,
                frp_subdomain: Some("anchor".into()),
                public_url: Some("https://anchor.taoyan.icu".into()),
                frp_proxy_type: Some("https2http".into()),
                frp_cert_path: Some(".anchor/cert/server.pem".into()),
                frp_key_path: Some(".anchor/cert/server.key".into()),
                cloudflare_mode: None,
                use_proxy: Some(false),
                apply: true,
                wait_seconds: 45,
            })))
        );
    }

    #[test]
    fn parses_foreground_all_services_with_tunnel() {
        let parsed = parse(strings(&[
            "--config-dir",
            "/tmp/anchor",
            "--json",
            "serve",
            "workspace-a",
            "--service",
            "all",
            "--tunnel",
        ]))
        .expect("parse");

        assert_eq!(parsed.config_dir, Some(PathBuf::from("/tmp/anchor")));
        assert!(parsed.json);
        assert_eq!(
            parsed.command,
            Command::Serve {
                workspace: "workspace-a".into(),
                service: ServiceSelection::All,
                tunnel: true,
            }
        );
    }

    #[test]
    fn parses_windows_service_lifecycle_and_internal_service_run() {
        for (name, expected) in [
            ("status", ServiceCommand::Status),
            ("install", ServiceCommand::Install),
            ("uninstall", ServiceCommand::Uninstall),
            ("start", ServiceCommand::Start),
            ("stop", ServiceCommand::Stop),
            ("restart", ServiceCommand::Restart),
            ("sync", ServiceCommand::Sync),
        ] {
            let parsed = parse(strings(&["service", name])).expect("service command");
            assert_eq!(parsed.command, Command::Service(expected));
        }

        let internal = parse(strings(&[
            "service-run",
            r"C:\Users\Demo User\AppData\Roaming\anchor",
            "S-1-5-21-100-200-300-1001",
            "Demo User",
        ]))
        .expect("service-run");
        assert_eq!(
            internal.command,
            Command::ServiceRun {
                config_dir: PathBuf::from(r"C:\Users\Demo User\AppData\Roaming\anchor"),
                owner_sid: Some("S-1-5-21-100-200-300-1001".into()),
                owner_username: Some("Demo User".into()),
            }
        );

        let legacy = parse(strings(&[
            "service-run",
            r"C:\Users\Demo User\AppData\Roaming\anchor",
        ]))
        .expect("legacy service-run shape is parsed so runtime can reject it explicitly");
        assert_eq!(
            legacy.command,
            Command::ServiceRun {
                config_dir: PathBuf::from(r"C:\Users\Demo User\AppData\Roaming\anchor"),
                owner_sid: None,
                owner_username: None,
            }
        );

        let elevated = parse(strings(&[
            "service-admin-run",
            "install",
            r"C:\Users\Demo User\AppData\Roaming\anchor",
            "S-1-5-21-100-200-300-1001",
            "Demo User",
        ]))
        .expect("service-admin-run");
        assert_eq!(
            elevated.command,
            Command::ServiceAdminRun {
                action: "install".into(),
                config_dir: PathBuf::from(r"C:\Users\Demo User\AppData\Roaming\anchor"),
                owner_sid: "S-1-5-21-100-200-300-1001".into(),
                owner_username: "Demo User".into(),
            }
        );
    }

    #[test]
    fn serve_defaults_to_mcp_without_tunnel() {
        let parsed = parse(strings(&["serve", "workspace-a"])).expect("parse");

        assert_eq!(
            parsed.command,
            Command::Serve {
                workspace: "workspace-a".into(),
                service: ServiceSelection::Mcp,
                tunnel: false,
            }
        );
    }

    #[test]
    fn rejects_unknown_service() {
        let error = parse(strings(&["serve", "workspace-a", "--service", "unknown"]))
            .expect_err("invalid service");

        assert!(error.contains("无效服务类型"));
    }

    #[test]
    fn parses_gateway_configuration_and_multi_workspace_serve() {
        let configure = parse(strings(&[
            "gateway",
            "configure",
            "--enable",
            "--port",
            "29000",
            "--owner",
            "workspace-a",
        ]))
        .expect("gateway configure");
        assert_eq!(
            configure.command,
            Command::Gateway(GatewayCommand::Configure(GatewayConfigureOptions {
                enabled: Some(true),
                local_port: Some(29000),
                owner_workspace: Some("workspace-a".into()),
                public_url: None,
            }))
        );

        let serve = parse(strings(&["gateway", "serve", "workspace-a", "workspace-b"]))
            .expect("gateway serve");
        assert_eq!(
            serve.command,
            Command::Gateway(GatewayCommand::Serve {
                workspaces: vec!["workspace-a".into(), "workspace-b".into()],
            })
        );

        let status = parse(strings(&["gateway", "status"])).expect("gateway status");
        assert_eq!(status.command, Command::Gateway(GatewayCommand::Status));

        let start = parse(strings(&[
            "gateway",
            "start",
            "workspace-a",
            "workspace-b",
            "--wait",
            "30",
        ]))
        .expect("gateway start");
        assert_eq!(
            start.command,
            Command::Gateway(GatewayCommand::Start(GatewayStartOptions {
                workspaces: vec!["workspace-a".into(), "workspace-b".into()],
                wait_seconds: 30,
            }))
        );

        let stop = parse(strings(&["gateway", "stop", "--timeout", "12", "--force"]))
            .expect("gateway stop");
        assert_eq!(
            stop.command,
            Command::Gateway(GatewayCommand::Stop(GatewayStopOptions {
                timeout_seconds: 12,
                force: true,
            }))
        );

        let restart =
            parse(strings(&["gateway", "restart", "--timeout", "9"])).expect("gateway restart");
        assert_eq!(
            restart.command,
            Command::Gateway(GatewayCommand::Restart(GatewayStopOptions {
                timeout_seconds: 9,
                force: false,
            }))
        );

        let reload = parse(strings(&["gateway", "reload"])).expect("gateway reload");
        assert_eq!(reload.command, Command::Gateway(GatewayCommand::Reload));

        let logs = parse(strings(&["gateway", "logs", "--lines", "250", "--follow"]))
            .expect("gateway logs");
        assert_eq!(
            logs.command,
            Command::Gateway(GatewayCommand::Logs(GatewayLogsOptions {
                lines: 250,
                follow: true,
            }))
        );

        let events = parse(strings(&["gateway", "events", "--follow", "--wait", "20"]))
            .expect("gateway events");
        assert_eq!(
            events.command,
            Command::Gateway(GatewayCommand::Events(GatewayEventsOptions {
                follow: true,
                wait_seconds: 20,
            }))
        );

        let child = parse(strings(&[
            "gateway-daemon-run",
            "scope-a",
            "workspace-b",
            "workspace-a",
        ]))
        .expect("gateway daemon child");
        assert_eq!(
            child.command,
            Command::GatewayDaemonRun {
                config_scope: "scope-a".into(),
                workspaces: vec!["workspace-b".into(), "workspace-a".into()],
            }
        );
    }

    #[test]
    fn parses_daemon_operations() {
        let start = parse(strings(&[
            "start",
            "workspace-a",
            "--service",
            "all",
            "--tunnel",
            "--wait",
            "20",
        ]))
        .expect("start parse");
        assert_eq!(
            start.command,
            Command::Start(RunOptions {
                workspace: "workspace-a".into(),
                service: Some(ServiceSelection::All),
                tunnel: Some(true),
                wait_seconds: 20,
            })
        );

        let stop = parse(strings(&[
            "stop",
            "workspace-a",
            "--timeout",
            "15",
            "--force",
        ]))
        .expect("stop parse");
        assert_eq!(
            stop.command,
            Command::Stop(StopOptions {
                workspace: "workspace-a".into(),
                timeout_seconds: 15,
                force: true,
            })
        );

        let events = parse(strings(&[
            "events",
            "workspace-a",
            "--follow",
            "--wait",
            "20",
        ]))
        .expect("events parse");
        assert_eq!(
            events.command,
            Command::Events(EventsOptions {
                target: EventsTarget::Workspace("workspace-a".into()),
                follow: true,
                wait_seconds: 20,
            })
        );

        let control_plane_events = parse(strings(&[
            "events",
            "--control-plane",
            "--follow",
            "--wait",
            "10",
        ]))
        .expect("control plane events parse");
        assert_eq!(
            control_plane_events.command,
            Command::Events(EventsOptions {
                target: EventsTarget::ControlPlane,
                follow: true,
                wait_seconds: 10,
            })
        );

        let reload = parse(strings(&["reload", "workspace-a", "--service", "actions"]))
            .expect("reload parse");
        assert_eq!(
            reload.command,
            Command::Reload(ReloadOptions {
                workspace: "workspace-a".into(),
                service: ServiceSelection::Actions,
            })
        );
    }

    #[test]
    fn status_supports_one_workspace_or_all_workspaces() {
        let one =
            parse(strings(&["status", "workspace-a", "--watch"])).expect("single workspace status");
        assert_eq!(
            one.command,
            Command::Status(StatusOptions {
                workspace: Some("workspace-a".into()),
                watch: true,
                interval_seconds: 2,
                control_plane: false,
            })
        );

        let all =
            parse(strings(&["status", "--all", "--interval", "5"])).expect("all workspace status");
        assert_eq!(
            all.command,
            Command::Status(StatusOptions {
                workspace: None,
                watch: false,
                interval_seconds: 5,
                control_plane: false,
            })
        );

        let implicit_all = parse(strings(&["status"])).expect("implicit all workspace status");
        assert_eq!(
            implicit_all.command,
            Command::Status(StatusOptions {
                workspace: None,
                watch: false,
                interval_seconds: 2,
                control_plane: false,
            })
        );

        let control_plane = parse(strings(&["status", "--control-plane", "--watch"]))
            .expect("control plane status");
        assert_eq!(
            control_plane.command,
            Command::Status(StatusOptions {
                workspace: None,
                watch: true,
                interval_seconds: 2,
                control_plane: true,
            })
        );
    }

    #[test]
    fn status_rejects_workspace_and_all_together() {
        let error = parse(strings(&["status", "workspace-a", "--all"]))
            .expect_err("ambiguous status selection");
        assert!(error.contains("不能同时"));
    }

    #[test]
    fn rejects_following_all_logs() {
        let error = parse(strings(&[
            "logs",
            "workspace-a",
            "--service",
            "all",
            "--follow",
        ]))
        .expect_err("ambiguous follow");
        assert!(error.contains("暂不支持"));
    }

    #[test]
    fn parses_workspace_registration_and_gpt_commands() {
        let register = parse(strings(&[
            "workspace",
            "register",
            "/srv/project",
            "--name",
            "Project",
        ]))
        .expect("register");
        assert_eq!(
            register.command,
            Command::Workspace(WorkspaceCommand::Register(RegisterOptions {
                path: "/srv/project".into(),
                name: Some("Project".into()),
            }))
        );

        let gpt = parse(strings(&[
            "workspace",
            "gpt-config",
            "project",
            "--service",
            "all",
            "--public",
            "--show-secrets",
        ]))
        .expect("gpt config");
        assert_eq!(
            gpt.command,
            Command::Workspace(WorkspaceCommand::GptConfig(GptConfigOptions {
                workspace: "project".into(),
                service: ServiceSelection::All,
                endpoint: EndpointSelection::Public,
                show_secrets: true,
            }))
        );
    }

    #[test]
    fn unregister_requires_explicit_force_at_execution_not_parse_time() {
        let parsed = parse(strings(&["workspace", "delete", "project"])).expect("delete parse");
        assert_eq!(
            parsed.command,
            Command::Workspace(WorkspaceCommand::Unregister(UnregisterOptions {
                workspace: "project".into(),
                force: false,
                timeout_seconds: 10,
            }))
        );
    }

    #[test]
    fn parses_config_get_set_diff_and_apply() {
        let get = parse(strings(&[
            "config",
            "get",
            "project",
            "--pending",
            "--key",
            "runtime.local_port",
        ]))
        .expect("config get");
        assert_eq!(
            get.command,
            Command::Config(ConfigCommand::Get(ConfigGetOptions {
                workspace: "project".into(),
                pending: true,
                key: Some("runtime.local_port".into()),
            }))
        );

        let set = parse(strings(&[
            "cfg",
            "set",
            "project",
            "--set",
            "runtime.local_port=29123",
            "--set",
            "auth.oauth_redirect_hosts=*.chatgpt.com",
        ]))
        .expect("config set");
        assert_eq!(
            set.command,
            Command::Config(ConfigCommand::Set(ConfigMutationOptions {
                workspace: "project".into(),
                assignments: vec![
                    ConfigAssignment {
                        path: "runtime.local_port".into(),
                        value: "29123".into(),
                    },
                    ConfigAssignment {
                        path: "auth.oauth_redirect_hosts".into(),
                        value: "*.chatgpt.com".into(),
                    },
                ],
            }))
        );

        let diff = parse(strings(&["config", "diff", "project"]))
            .expect("config diff without extra patch");
        assert_eq!(
            diff.command,
            Command::Config(ConfigCommand::Diff(ConfigMutationOptions {
                workspace: "project".into(),
                assignments: Vec::new(),
            }))
        );

        let apply =
            parse(strings(&["config", "apply", "project", "--wait", "45"])).expect("config apply");
        assert_eq!(
            apply.command,
            Command::Config(ConfigCommand::Apply(ConfigApplyOptions {
                workspace: "project".into(),
                wait_seconds: 45,
            }))
        );
    }

    #[test]
    fn config_set_requires_assignment_and_valid_path_value_shape() {
        let missing = parse(strings(&["config", "set", "project"]))
            .expect_err("config set requires assignment");
        assert!(missing.contains("至少需要一个"));

        let malformed = parse(strings(&[
            "config",
            "set",
            "project",
            "--set",
            "runtime.local_port",
        ]))
        .expect_err("assignment requires equals");
        assert!(malformed.contains("PATH=VALUE"));
    }

    #[test]
    fn parses_chatgpt_plugin_package_options() {
        let parsed = parse(strings(&[
            "plugin",
            "package",
            "Anchor",
            "--app-id",
            "plugin_asdk_app_123",
            "--name",
            "anchor",
            "--output",
            ".anchor/plugin-marketplace",
        ]))
        .expect("plugin package");
        assert_eq!(
            parsed.command,
            Command::Plugin(PluginCommand::Package(PluginPackageOptions {
                workspace: "Anchor".into(),
                app_id: "plugin_asdk_app_123".into(),
                output: Some(PathBuf::from(".anchor/plugin-marketplace")),
                name: Some("anchor".into()),
            }))
        );

        let missing = parse(strings(&["plugin", "package", "Anchor"]))
            .expect_err("plugin app id is required");
        assert!(missing.contains("--app-id"));
    }
}
