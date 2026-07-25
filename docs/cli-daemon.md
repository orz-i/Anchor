# Linux CLI Daemon 与运维命令

`coding-tools-mcp` 提供两种运行方式：

| 模式 | 命令 | 适用场景 |
| --- | --- | --- |
| 前台 | `serve` | 调试、容器、systemd 直接监督 |
| 后台 daemon | `start` | SSH 会话、人工运维、无需保持终端 |

两种模式都读取同一个 WorkspaceProfile；一个 workspace 仍只对应一个 profile。

## 快速开始

```bash
# 后台启动 MCP
coding-tools-mcp start PROFILE_ID

# 后台启动 MCP + Actions + 隧道
coding-tools-mcp start PROFILE_ID --service all --tunnel

# 查看 daemon、PID 与端口所有权
coding-tools-mcp status PROFILE_ID

# 持续观察状态
coding-tools-mcp status PROFILE_ID --watch

# 查看 daemon 日志
coding-tools-mcp logs PROFILE_ID

# 跟随 MCP 日志
coding-tools-mcp logs PROFILE_ID --service mcp --follow

# 重启并沿用当前 service/tunnel 参数
coding-tools-mcp restart PROFILE_ID

# 优雅停止
coding-tools-mcp stop PROFILE_ID

# 检查环境、端口、状态和隧道依赖
coding-tools-mcp doctor PROFILE_ID
```

## 命令语义

### `start`

```bash
coding-tools-mcp start <workspace> \
  [--service mcp|actions|all] \
  [--tunnel|--no-tunnel] \
  [--wait SECONDS]
```

- 默认启动 MCP，不启动隧道；
- 创建独立 session，stdin 关闭，stdout/stderr 写入 `daemon.log`；
- 等待选中服务的本地端口由 daemon PID 监听后返回；
- 默认最多等待 10 秒；
- 隧道未立即就绪不会阻止本地服务进入 running，daemon 会继续按恢复策略重试；
- 同参数重复 `start` 是幂等操作；
- daemon 已运行但参数不同，会提示使用 `restart`；
- 端口属于 GUI 或其他进程时拒绝启动，不会接管或停止它。

### `stop`

```bash
coding-tools-mcp stop <workspace> [--timeout SECONDS] [--force]
```

- 根据状态文件读取 PID；
- 读取 `/proc/<pid>/cmdline`，确认该 PID 确实是当前 workspace 的 `daemon-run`；
- 默认发送 SIGTERM，让 MCP、Actions、下游 MCP 和隧道优雅停止；
- 超时后只有显式 `--force` 才终止完整子进程树；
- PID 不匹配时只清理过期状态，不会误杀其他进程；
- daemon 未运行时返回 already stopped，不视为错误。

### `restart`

```bash
coding-tools-mcp restart <workspace> [--service ...] [--tunnel|--no-tunnel]
```

未提供 service/tunnel 参数时，沿用当前 daemon 状态文件中的参数。没有当前状态时回退为 MCP、无隧道。

### `status`

```bash
coding-tools-mcp status <workspace> [--watch] [--interval SECONDS]
```

输出同时包含：

- daemon 是否支持、是否存活、PID 是否匹配；
- daemon 的 service/tunnel 参数；
- MCP 与 Actions 端口；
- 每个端口的 owner：
  - `daemon`：由当前 CLI daemon 监听；
  - `external`：由 GUI 或其他进程监听；
  - `none`：未监听。

`--json --watch` 使用一行一个 JSON 对象的 NDJSON 格式，适合脚本持续消费。

### `logs`

```bash
coding-tools-mcp logs <workspace> \
  [--service daemon|mcp|actions|all] \
  [--lines N] \
  [--follow|-f]
```

- 默认显示 `daemon.log` 最后 100 行；
- MCP/Actions 会同时显示 listener 与对应隧道日志；
- `--service all` 可一次读取全部日志；
- follow 模式要求选择单个服务，避免多文件输出难以辨认；
- `--json -f` 输出 `log_snapshot` 和 `log_append` NDJSON 事件。

### `doctor`

`doctor` 是只读诊断，不启动或停止服务。检查：

- 当前平台是否支持 daemon；
- workspace 路径是否有效；
- daemon 状态是否过期或损坏；
- MCP/Actions 端口是否空闲或由 daemon 所有；
- profile 日志目录是否可写；
- 已配置 FRP/Cloudflare 时对应二进制是否存在。

任一关键检查失败时退出码为 1，`--json` 返回完整 checks 数组。

## 运行文件

Daemon 使用独占锁、状态 JSON 和 PID 文件。运行目录按以下优先级选择：

1. 使用 `--config-dir` 时：`<config-dir>/run/`；
2. 设置 `XDG_RUNTIME_DIR` 时：`$XDG_RUNTIME_DIR/coding-tools-mcp/`；
3. 回退：`/tmp/coding-tools-mcp-<uid>/`。

目录权限为 `0700`，状态、PID 和锁文件使用当前用户权限。每个 workspace 使用独立文件名。

日志继续存放在 profile 日志目录：

```text
~/.config/coding-tools-mcp-desktop/logs/<profile-id>/daemon.log
```

正常退出会移除 PID 和状态文件；锁文件保留并复用。崩溃留下的状态会在下一次 `start/stop` 时识别为 stale 并安全重建或清理。

## 与 systemd 的关系

内置 daemon 解决的是“命令退出后继续运行”和日常人工运维，不负责开机自启或进程崩溃后的操作系统级拉起。

生产服务器仍推荐 systemd 直接监督前台 `serve`：

```ini
[Service]
Type=simple
ExecStart=/usr/local/bin/coding-tools-mcp serve PROFILE_ID --service all --tunnel
Restart=on-failure
RestartSec=3
```

不要在 systemd 的 `ExecStart` 中使用 `start`，否则 systemd 只会监督短暂存在的启动命令，而不是实际 daemon。

## 自动化

所有运维命令支持全局 `--json`：

```bash
coding-tools-mcp --json start PROFILE_ID
coding-tools-mcp --json status PROFILE_ID
coding-tools-mcp --json logs PROFILE_ID --service daemon
coding-tools-mcp --json doctor PROFILE_ID
```

失败时返回非零退出码，并输出：

```json
{"ok":false,"error":"..."}
```

## 安全边界

- Daemon 目前仅支持 Linux；Windows/macOS 使用 `serve` 或 GUI；
- PID 所有权校验失败时拒绝发送信号；
- `stop --force` 只在确认 daemon PID 后终止其进程树；
- 端口被 GUI 或外部进程占用时拒绝启动；
- `daemon-run` 是内部命令，不应直接调用；
- 状态文件不是远程控制接口，不监听公网端口。

## 当前验证边界

Windows 开发机已完成 headless feature 编译、参数测试和严格 Clippy。当前 Rust 工具链未安装 `x86_64-unknown-linux-gnu` 标准库，因此 Linux 专用 `setsid`、`/proc` PID 校验和真实后台生命周期仍需在 Linux CI 或 Linux 主机执行 smoke：

```bash
coding-tools-mcp start PROFILE_ID
coding-tools-mcp status PROFILE_ID
coding-tools-mcp logs PROFILE_ID --follow
coding-tools-mcp restart PROFILE_ID
coding-tools-mcp stop PROFILE_ID
```
