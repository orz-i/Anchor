# CLI Daemon 与运维命令

长期控制面演进方案见 [CLI、Daemon 与 GUI 控制面演进路线](cli-daemon-roadmap.md)。当前后台 daemon 仍只支持 Linux，但 daemon 状态模型已经提升为 CLI 与桌面端共享的库层能力。

`anchor` 提供两种运行方式：

| 模式 | 命令 | 适用场景 |
| --- | --- | --- |
| 前台 | `serve` | 调试、容器、systemd 直接监督 |
| 后台 daemon | `start` | SSH 会话、人工运维、无需保持终端 |

两种模式都读取同一个 WorkspaceProfile；一个 workspace 仍只对应一个 profile。

Linux 后台 daemon 还提供版本化本地控制端点。CLI 与桌面端读取控制状态时会优先通过该端点查询；只有端点明确不可用时才执行本地只读探测。协议错误或版本不兼容会直接报告，避免把损坏或过期 daemon 隐藏为“正常离线状态”。

Workspace 注册、注销和 GPT 连接配置见 [Workspace CLI 注册与 GPT 连接运维](workspace-cli.md)。

## 快速开始

```bash
# 后台启动 MCP
anchor start PROFILE_ID

# 后台启动 MCP + Actions + 隧道
anchor start PROFILE_ID --service all --tunnel

# 查看 daemon、PID 与端口所有权
anchor status PROFILE_ID

# 查看全部工作区
anchor status

# 持续观察状态
anchor status PROFILE_ID --watch

# 查看 daemon 日志
anchor logs PROFILE_ID

# 跟随 MCP 日志
anchor logs PROFILE_ID --service mcp --follow

# 重启并沿用当前 service/tunnel 参数
anchor restart PROFILE_ID

# 优雅停止
anchor stop PROFILE_ID

# 检查环境、端口、状态和隧道依赖
anchor doctor PROFILE_ID
```

## 命令语义

### `start`

```bash
anchor start <workspace> \
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
anchor stop <workspace> [--timeout SECONDS] [--force]
```

- 根据状态文件读取 PID；
- 读取 `/proc/<pid>/cmdline`，确认该 PID 确实是当前 workspace 的 `daemon-run`；
- 默认发送 SIGTERM，让 MCP、Actions、下游 MCP 和隧道优雅停止；
- 超时后只有显式 `--force` 才终止完整子进程树；
- PID 不匹配时只清理过期状态，不会误杀其他进程；
- daemon 未运行时返回 already stopped，不视为错误。

### `restart`

```bash
anchor restart <workspace> [--service ...] [--tunnel|--no-tunnel]
```

未提供 service/tunnel 参数时，沿用当前 daemon 状态文件中的参数。没有当前状态时回退为 MCP、无隧道。

### `status`

```bash
anchor status [<workspace>|--all] [--watch] [--interval SECONDS]
```

输出同时包含：

- daemon 是否支持、是否存活、PID 是否匹配；
- daemon 的 service/tunnel 参数；
- MCP 与 Actions 端口；
- 每个端口的 owner：
  - `daemon`：由当前 CLI daemon 监听；
  - `external`：由 GUI 或其他进程监听；
  - `none`：未监听。

不指定 workspace 或显式使用 `--all` 时，CLI 会输出全部 WorkspaceProfile 的控制面状态。非 watch JSON 模式返回数组；watch JSON 模式输出带 `status_snapshot` 事件名的 NDJSON。

`--json --watch` 使用一行一个 JSON 对象的 NDJSON 格式，适合脚本持续消费。

### `logs`

```bash
anchor logs <workspace> \
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
2. 设置 `XDG_RUNTIME_DIR` 时：`$XDG_RUNTIME_DIR/anchor/`；
3. 回退：`/tmp/anchor-<uid>/`。

目录权限为 `0700`，状态、PID 和锁文件使用当前用户权限。每个 workspace 使用独立文件名。

日志继续存放在 profile 日志目录：

```text
~/.config/anchor-desktop/logs/<profile-id>/daemon.log
```

正常退出会移除 PID 和状态文件；锁文件保留并复用。崩溃留下的状态会在下一次 `start/stop` 时识别为 stale 并安全重建或清理。

每工作区还使用一个控制端点：

- Unix：`<runtime-dir>/<profile-id>.sock`，父目录 `0700`、socket `0600`；
- Windows：`\\.\pipe\anchor-<user-config-scope>-<profile-id>`。当前版本只提供客户端地址与协议抽象，Windows daemon 服务端将在 Windows Service 生命周期落地时启用。

控制协议当前版本为 `2`，支持 `ping`、`version`、`workspace_status`、`logs`、`shutdown` 和 `prepare_restart`。每条消息是最大 64 KiB 的单行 JSON；一个连接只处理一个请求，响应必须回显请求 ID。

CLI 生命周期语义：

- `start` 在 daemon 不存在时负责创建后台进程；若状态显示 daemon 已运行，必须先通过 IPC `ping` 验证目标控制面；
- `stop` 先发送 `shutdown`，由目标 daemon 完成 Runtime、Tunnel 和监听器清理后退出；
- `restart` 先发送 `prepare_restart`，等待原 PID 退出后再执行一次新的 `start` 引导；
- IPC 不可用、协议版本不兼容或响应 PID 不匹配时，写命令直接失败，不会回退到客户端直接发送信号或启动第二套运行时；
- `--force` 只在 daemon 已接受 IPC 退出请求但超过等待时间后生效，并再次验证 PID 仍属于目标 Workspace。

运行中 daemon 的 `logs` 和 `logs --follow` 通过 IPC 获取有界日志快照和增量游标。单次响应日志内容最多 8 KiB，避免日志内容突破控制帧上限。daemon 已停止时，CLI 仍允许直接读取已有历史日志文件。

GUI Workspace 控制使用同一生命周期客户端：

- MCP/Actions 状态来自 `workspace_status`，包括 daemon PID、端口所有权和 MCP 活跃度；
- 启动、停止、重启和 Workspace 删除不再创建或接管进程内 listener；
- MCP/Actions 独立开关会重新计算目标 daemon 服务选择，必要时协调重启整个 Workspace daemon；
- 运行中日志必须走 daemon IPC；控制端点失败时不会回退为 GUI 直接读取正在写入的日志文件；
- 密钥再生成会通过 IPC 重启真正使用该密钥的 daemon；
- 当前 Gateway/Tunnel 仍是桌面兼容路径。daemon 管理的 MCP 运行时，GUI 拒绝启用进程内 Gateway，防止形成第二套运行权威。

Windows 仍只有 Named Pipe 客户端和地址抽象。服务端与当前用户 ACL 未完成前，GUI 会显示 daemon 不受支持，写操作直接失败，不会落回进程内 Runtime。

## 与 systemd 的关系

内置 daemon 解决的是“命令退出后继续运行”和日常人工运维，不负责开机自启或进程崩溃后的操作系统级拉起。

生产服务器仍推荐 systemd 直接监督前台 `serve`：

```ini
[Service]
Type=simple
ExecStart=/usr/local/bin/anchor serve PROFILE_ID --service all --tunnel
Restart=on-failure
RestartSec=3
```

不要在 systemd 的 `ExecStart` 中使用 `start`，否则 systemd 只会监督短暂存在的启动命令，而不是实际 daemon。

## 自动化

所有运维命令支持全局 `--json`：

```bash
anchor --json start PROFILE_ID
anchor --json status PROFILE_ID
anchor --json logs PROFILE_ID --service daemon
anchor --json doctor PROFILE_ID
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
anchor start PROFILE_ID
anchor status PROFILE_ID
anchor logs PROFILE_ID --follow
anchor restart PROFILE_ID
anchor stop PROFILE_ID
```
