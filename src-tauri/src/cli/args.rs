use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub config_dir: Option<PathBuf>,
    pub json: bool,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    List,
    Show {
        workspace: String,
    },
    Status {
        workspace: String,
    },
    Serve {
        workspace: String,
        service: ServiceSelection,
        tunnel: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSelection {
    Mcp,
    Actions,
    All,
}

impl ServiceSelection {
    pub fn includes_mcp(self) -> bool {
        matches!(self, Self::Mcp | Self::All)
    }

    pub fn includes_actions(self) -> bool {
        matches!(self, Self::Actions | Self::All)
    }
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
        Some("status") => {
            let workspace = pop_value(&mut args, "status")?;
            ensure_empty(&args, "status")?;
            Command::Status { workspace }
        }
        Some("serve") => parse_serve(&mut args)?,
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
                service = match pop_value(args, "--service")?.as_str() {
                    "mcp" => ServiceSelection::Mcp,
                    "actions" => ServiceSelection::Actions,
                    "all" => ServiceSelection::All,
                    value => {
                        return Err(format!("无效服务类型：{value}；可选值为 mcp、actions、all"))
                    }
                };
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
    "Coding Tools MCP CLI\n\n\
用法：\n\
  coding-tools-mcp [--config-dir PATH] [--json] list\n\
  coding-tools-mcp [--config-dir PATH] [--json] show <workspace>\n\
  coding-tools-mcp [--config-dir PATH] [--json] status <workspace>\n\
  coding-tools-mcp [--config-dir PATH] [--json] serve <workspace> [--service mcp|actions|all] [--tunnel]\n\n\
workspace 可使用 profile ID、唯一名称或项目路径。\n\
serve 为前台常驻模式，按 Ctrl+C 优雅停止；默认只启动 MCP，本地端口已被 GUI 占用时不会接管。"
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
            "/tmp/coding-tools",
            "--json",
            "serve",
            "workspace-a",
            "--service",
            "all",
            "--tunnel",
        ]))
        .expect("parse");

        assert_eq!(parsed.config_dir, Some(PathBuf::from("/tmp/coding-tools")));
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
}
