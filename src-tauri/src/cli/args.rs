use std::collections::VecDeque;
use std::path::PathBuf;

pub use crate::daemon::ServiceSelection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub config_dir: Option<PathBuf>,
    pub json: bool,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayConfigureOptions {
    pub enabled: Option<bool>,
    pub local_port: Option<u16>,
    pub owner_workspace: Option<String>,
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayCommand {
    Show,
    Configure(GatewayConfigureOptions),
    Serve { workspaces: Vec<String> },
}

fn parse_gateway_command(args: &mut VecDeque<String>) -> Result<GatewayCommand, String> {
    match args.pop_front().as_deref() {
        Some("show" | "status") => {
            ensure_empty(args, "gateway show")?;
            Ok(GatewayCommand::Show)
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
        Some(other) => Err(format!("未知 gateway 命令：{other}\n\n{}", gateway_usage())),
        None => Err(format!("gateway 缺少子命令\n\n{}", gateway_usage())),
    }
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
    let mut watch = false;
    let mut interval_seconds = 2;
    while let Some(option) = args.pop_front() {
        match option.as_str() {
            "--all" => all = true,
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
    Ok(StatusOptions {
        workspace: if all { None } else { workspace },
        watch,
        interval_seconds,
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
    Doctor {
        workspace: String,
    },
    Workspace(WorkspaceCommand),
    Gateway(GatewayCommand),
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
        Some("doctor") => {
            let workspace = pop_value(&mut args, "doctor")?;
            ensure_empty(&args, "doctor")?;
            Command::Doctor { workspace }
        }
        Some("workspace" | "ws") => Command::Workspace(parse_workspace_command(&mut args)?),
        Some("gateway" | "gw") => Command::Gateway(parse_gateway_command(&mut args)?),
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
  anchor [--config-dir PATH] [--json] status [<workspace>|--all] [--watch]\n\
  anchor [--config-dir PATH] [--json] serve <workspace> [--service mcp|actions|all] [--tunnel]\n\n\
  anchor [--config-dir PATH] [--json] start <workspace> [--service mcp|actions|all] [--tunnel]\n\
  anchor [--config-dir PATH] [--json] stop <workspace> [--timeout SECONDS] [--force]\n\
  anchor [--config-dir PATH] [--json] restart <workspace> [--service mcp|actions|all] [--tunnel]\n\
  anchor [--config-dir PATH] [--json] logs <workspace> [--service daemon|mcp|actions|all] [--lines N] [-f]\n\
  anchor [--config-dir PATH] [--json] doctor <workspace>\n\n\
  anchor [--config-dir PATH] [--json] workspace <command> ...\n\n\
  anchor [--config-dir PATH] [--json] gateway <command> ...\n\n\
workspace 可使用 profile ID、唯一名称或项目路径。\n\
不指定 workspace 的 status 会显示全部工作区。serve 为前台调试模式；start/stop/restart 管理 Linux 后台 daemon。CLI 不会接管 GUI 或其他进程占用的端口。"
}

pub fn gateway_usage() -> &'static str {
    "Gateway 命令：\n\
  anchor gateway show\n\
  anchor gateway configure [--enable|--disable] [--port PORT] [--owner WORKSPACE] [--public-url URL|--clear-public-url]\n\
  anchor gateway serve <workspace> [workspace ...]\n\n\
gateway serve 在一个前台进程内启动所选工作区的 MCP listener、共享 Gateway 和唯一 MCP 隧道，适合由 systemd 监督。"
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
            })
        );

        let implicit_all = parse(strings(&["status"])).expect("implicit all workspace status");
        assert_eq!(
            implicit_all.command,
            Command::Status(StatusOptions {
                workspace: None,
                watch: false,
                interval_seconds: 2,
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
}
