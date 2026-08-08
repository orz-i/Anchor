# CLI Daemon 与运维命令

长期控制面演进方案见 [CLI、Daemon 与 GUI 控制面演进路线](cli-daemon-roadmap.md)。Workspace 后台 daemon 当前支持 Windows 与 Linux；Gateway 后台 daemon 仍只支持 Linux。两类控制域共享状态/协议基础，但互不接管运行资源。

`anchor` 提供两种运行方式：

| 模式 | 命令 | 适用场景 |
| --- | --- | --- |
| 前台 | `serve` | 调试、容器、systemd 直接监督 |
| 后台 daemon | `start` | SSH 会话、人工运维、无需保持终端 |

两种模式都读取同一个 WorkspaceProfile；一个 workspace 仍只对应一个 profile。

Workspace 后台 daemon 提供版本化本地控制端点：Unix 使用 UDS，Windows 使用 Named Pipe。CLI 与桌面端读取控制状态时会优先通过该端点查询；只有端点明确不可用时才执行本地只读探测。协议错误或版本不兼容会直接报告，避免把损坏或过期 daemon 隐藏为“正常离线状态”。

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

# 查看 daemon 事件快照
anchor events PROFILE_ID

# 持续消费 daemon 事件
anchor events PROFILE_ID --follow

# 只重新加载 MCP 配置，不重启整个 Workspace daemon
anchor reload PROFILE_ID --service mcp

# 重启并沿用当前 service/tunnel 参数
anchor restart PROFILE_ID

# 优雅停止
anchor stop PROFILE_ID

# 检查环境、端口、状态和隧道依赖
anchor doctor PROFILE_ID

# 启动独立全局 Gateway daemon
anchor gateway start PROFILE_A PROFILE_B

# 查看 / reload / 重启 / 停止 Gateway daemon
anchor gateway status
anchor gateway logs --follow
anchor gateway events --follow
anchor gateway reload
anchor gateway restart
anchor gateway stop
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
- Linux 通过 `/proc/<pid>/cmdline` 验证目标命令；Windows state v2 记录 daemon `executablePath`，并与目标 PID 的进程镜像路径匹配；
- 正常 `stop` 始终先通过版本化 IPC 发送 `shutdown`，由 daemon 自己停止 MCP、Actions、下游 MCP 和隧道；Windows 不提供绕过 Named Pipe 的本地 signal/kill 写路径；
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

### `events`

```bash
anchor events <workspace> [--follow|-f] [--wait SECONDS]
```

- 读取 Workspace daemon 的版本化运行事件；
- 每条事件包含 daemon stream ID 对应的单调递增 sequence；客户端使用 `streamId + sequence` 游标断线续读；
- daemon 重启或 retained window 被越过时返回 `reset=true`，客户端从当前可用事件窗口继续；
- daemon 内最多保留 256 条事件，单批最多返回 32 条，单条 state/message 也有长度预算，保证最坏 JSON 转义内容仍不突破 64 KiB 控制帧；
- follow 使用最长 25 秒的长轮询，而不是常驻复用连接，保持当前“单连接单请求”的 IPC 模型；
- `--json -f` 一行输出一个事件；游标重置时额外输出 `cursor_reset` 标记；
- endpoint 不存在时命令直接失败，不把事件语义伪装成本地文件或状态轮询。

### `reload`

```bash
anchor reload <workspace> [--service mcp|actions|all]
```

- 默认 reload MCP；
- 请求先返回异步 operation accepted，daemon 主循环随后重新读取最新 WorkspaceProfile 并只重建目标 listener；
- Workspace daemon PID 不变，另一服务和现有 Tunnel ownership 不因单服务 reload 被重启；
- 新 listener 启动失败时会尝试恢复 reload 前的旧 listener；如果恢复也失败，则明确返回两层错误；
- reload 属于写操作：IPC 不可用、协议不兼容、Workspace/PID 不匹配时直接失败，不会回退到 CLI/GUI 本地 RuntimeSupervisor；
- `restart` 仍表示完整 daemon 生命周期重启；`reload` 用于配置应用和单服务重建。

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

Daemon 使用独占锁、状态 JSON 和 PID 文件。运行目录按平台选择：

1. 使用 `--config-dir` 时，Windows/Linux 都使用 `<config-dir>/run/`；
2. Linux 设置 `XDG_RUNTIME_DIR` 时使用 `$XDG_RUNTIME_DIR/anchor/`；否则回退 `/tmp/anchor-<uid>/`；
3. Windows 未指定 `--config-dir` 时使用当前用户 Anchor 配置目录下的 `run/`。

Unix 运行目录权限为 `0700`，状态、PID 和锁文件使用当前用户权限。Windows 同样按当前用户配置域分离，每个 workspace 使用独立文件名；state v2 额外保存实际 daemon executable path，用于 PID 镜像归属校验。

日志继续存放在 profile 日志目录：

```text
~/.config/anchor-desktop/logs/<profile-id>/daemon.log
```

正常退出会移除 PID 和状态文件；锁文件保留并复用。崩溃留下的状态会在下一次 `start/stop` 时识别为 stale 并安全重建或清理。

每工作区还使用一个控制端点：

- Unix：`<runtime-dir>/<profile-id>.sock`，父目录 `0700`、socket `0600`；
- Windows：`\\.\pipe\anchor-<user-config-scope>-<profile-id>`。服务端拒绝远程客户端，并使用受保护 DACL `D:P(A;;GA;;;SY)(A;;GA;;;OW)`，只授予 LocalSystem 与对象 owner 完全控制，不添加 Everyone/Anonymous ACE。

Workspace 控制协议当前版本为 `6`，支持 `ping`、`version`、`workspace_status`、`logs`、`events`、`reload`、`apply_config`、`update_oauth_redirect_policy`、`shutdown`、`prepare_restart`、`tunnel_control` 和 `operation_status`。每条消息是最大 64 KiB 的单行 JSON；一个连接只处理一个请求，响应必须回显请求 ID。

Workspace daemon 的 listener 与 tunnel ownership 分开记录。`service` 仍表示 `mcp|actions|all` listener 选择，`tunnelServices` 表示由该 daemon 实际管理的 `mcp|actions|all` 隧道集合。旧状态中的 `tunnel=true` 继续按“所选 listener 全部启用隧道”解释，保证升级兼容。

Tunnel 写控制是异步操作：daemon 先返回 `OperationAccepted` 并完整关闭本次响应帧，然后主循环才执行 Tunnel Supervisor 的 start/stop/restart；客户端使用 `operation_status` 查询 `pending/running/succeeded/failed`。这保证协议响应不会被正在执行的 tunnel 替换操作截断。FRP restart 继续使用 supervisor 的原子 route replacement，失败时保留旧线路；非 FRP restart 失败时也尝试恢复上一线路和持久配置。

- reload 只允许应用到当前由 daemon 运行的目标服务；未运行的服务会 fail-closed，避免另一活动 listener 仍使用旧配置而 daemon 内存 profile 已提前切到新配置。CLI `--service all` 会先整体校验两个服务都处于活动状态，再执行任何 reload，避免半成功。

事件控制面使用有界内存 journal。daemon 启动时创建新的 `streamId`，事件按 sequence 单调递增；当前事件包括 daemon ready/stopping、service state、Tunnel state、MCP activity 与 reload 结果。客户端可用 `events` 长轮询等待变化，daemon 重启或 retained window 越界时通过 `reset` 显式要求重建游标。

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
- MCP/Actions Tunnel 的状态、启动、停止、重载和测试均通过 Workspace daemon；daemon 运行时失败不会回退到 GUI 进程自己的 Tunnel Supervisor；
- 保存 tunnel 配置后只执行 daemon 内 tunnel reload，不再追加一次完整 Workspace daemon restart；实时公网 URL 由 daemon `workspace_status` 返回，GUI 优先使用该值；
- Workspace 页面状态刷新优先使用 daemon `events` 长轮询，只有控制端点明确不存在时才回退到旧状态轮询；protocol/remote 错误会进入显式 fault 状态，不静默降级；fallback polling 会周期性重新探测事件端点，因此外部 CLI 启动 daemon 后可自动恢复 event-first 模式；
- Workspace 配置保存由 Rust 控制层根据旧/新 profile 计算 apply plan；GUI 不再根据 `running` 状态自行调用 restart。名称等纯元数据变化不触发 listener，MCP/Actions 运行参数和认证身份变化只 reload 对应活动 listener，失败会恢复旧磁盘配置并回滚此前已成功触及的运行态；
- Workspace control protocol v6 保留 `update_oauth_redirect_policy` 并新增异步 `apply_config`。OAuth Callback URI/Host 变化在 daemon 进程内直接更新活动 OAuth runtime；若 runtime 尚未加载则由控制层受控 fallback 到单 listener reload，不允许 GUI/CLI 进程修改自己的 registry 后伪装 daemon 已热更新；`apply_config` 则由 daemon 使用其当前内存 profile 与磁盘 desired profile 计算同一份 apply plan，并原子协调活动 listener / direct tunnel，失败时回滚已触及运行态；
- Tunnel 仍保持独立事务语义：保存 tunnel 配置后由 tunnel control 执行 start/stop/restart；Workspace profile 更新本身不会把 tunnel 字段误判为 listener 配置。Gateway 仅在其实际使用的 MCP tunnel/owner 字段变化时 reload，不再因 Workspace 名称等无关配置变化重建；
- Gateway 不归属于任何 Workspace daemon。Linux GUI/CLI 使用独立 Gateway daemon；Windows Gateway daemon 尚未迁移完成，因此只在 **Gateway 控制域** 保留 GUI Server 兼容运行时。该 Gateway 兼容域会读取 Workspace daemon 的 MCP 活动状态并把 route 指向 daemon-owned 本地端口，不会恢复 Workspace 进程内 listener。

## 独立 Gateway daemon

Gateway 是用户配置域级别的独立控制域，不绑定到任何单一 Workspace daemon。后台入口：

```bash
anchor gateway status
anchor gateway start <workspace> [workspace ...] [--wait SECONDS]
anchor gateway reload
anchor gateway logs [--lines N] [--follow]
anchor gateway events [--follow] [--wait SECONDS]
anchor gateway restart [--timeout SECONDS] [--force]
anchor gateway stop [--timeout SECONDS] [--force]
```

`anchor gateway serve <workspace ...>` 仍保留为前台调试、容器或外部 supervisor 入口；内置后台 daemon 与前台 serve 不应同时拥有同一 Gateway 端口。

Gateway daemon 使用独立协议 v1，不复用 Workspace daemon protocol v6。每个请求都包含 `protocolVersion`、`requestId` 和 `configScope`；scope 根据当前应用配置目录派生，用于拒绝错误配置域的 PID/socket。Linux 运行文件位于与 Workspace daemon 相同的私有 runtime 根目录，但使用全局名称：

```text
gateway.lock
gateway.pid
gateway.json
gateway.sock
```

状态文件记录 daemon PID、配置域、route Workspace IDs、Gateway 本地端口和版本。Unix socket 父目录保持 `0700`、socket 为 `0600`。readiness 同时要求 Gateway 本地端口属于目标 PID 且 control `ping` 成功。

Gateway protocol v1 的可观察性方法采用 additive tag 扩展，没有修改已有 v1 请求/响应形状：

- `logs` 只读取 daemon 派生的 `gateway/daemon.log`，客户端不能指定任意路径；初始 tail 最多扫描 1 MiB、最多 5000 行，单响应日志正文预算 8 KiB；cursor 使用字节 offset，文件缩短/轮转时从 0 重新读取；
- `events` 使用独立的进程内 journal，每个配置域最多保留 256 条事件、单批最多 32 条，游标为 `streamId + sequence`，最长长轮询 25 秒；daemon 重启、游标跨 stream 或越过 retained window 时显式返回 `reset`；
- 当前 Gateway 事件覆盖 daemon ready/stopping、Gateway/route 状态、tunnel recovery/error、reload 和 config apply 结果；单条 state/message 分别限制为 64/512 字节，保证最坏 JSON 转义后仍小于 64 KiB 控制帧；
- 新客户端连接只实现旧 v1 方法的 daemon 时，新方法失败会作为明确的 protocol/I/O 能力错误上抛，不会被伪装成“daemon 离线”。

运行中的 `gateway logs` / GUI Gateway 日志必须经过 IPC；只有 Gateway daemon 明确停止且 endpoint 不可用时，才允许使用同一有界读取器查看历史日志。`gateway events` 仅存在于活动 daemon 的内存 journal，不提供文件或 polling 语义回退。

Gateway 写控制遵循 fail-closed：

- `shutdown` / `prepare_restart` 先由 daemon IPC 接受；只有已接受退出请求后，CLI 的 `--force` 才能在超时后终止再次确认归属的 PID；
- `reload` 保留当前 route IDs，重新读取持久化 Gateway/Workspace 配置并在 daemon 内重建运行态；失败时尝试恢复旧 listener/routes/tunnel；
- 运行中 `gateway configure` / GUI `set_mcp_gateway` 使用 `apply_config`：daemon 先应用新运行态并更新 state，成功后才持久化配置；中间失败会停止新运行态并恢复旧运行态；
- 禁用运行中的 Gateway 不使用 `apply_config`，而是先优雅 shutdown，确认退出后再持久化 disabled；
- endpoint 不可用、协议不兼容、scope/PID 不匹配时所有写请求直接失败，不会回退为 CLI/GUI 进程内 Gateway 或 Tunnel Supervisor。

Gateway daemon 正在使用的 route Workspace 会被视为 live MCP 运行态。只有影响 Gateway 所持 MCP tunnel/owner identity 的 Workspace 配置保存才触发 Gateway reload；失败时桌面端恢复旧 Workspace/settings 并再次对齐旧运行态。名称、Actions 或普通 MCP listener 策略变化不会无谓重建 Gateway。活动 route 在 Gateway daemon 停止前不能删除或注销。

GUI Gateway 页面直接使用 `routeWorkspaceIds` 展示活动路由，不再逐个轮询 Workspace runtime。桌面 AppState 在每次数据操作前重新加载磁盘配置，避免覆盖 Gateway daemon 在后台写入的 observed public URL。

## CLI 配置闭环

Workspace 配置提供独立于运行启停的脚本化 staging/apply 工作流：

```bash
anchor --json config get <workspace> [--pending] [--key PATH]
anchor --json config diff <workspace> [--set PATH=VALUE ...]
anchor --json config set <workspace> --set PATH=VALUE [--set PATH=VALUE ...]
anchor --json config apply <workspace> [--wait SECONDS]
```

- `config get` 和 `config diff` 是只读操作；`--key` 与 `--set` 使用 `WorkspaceProfile` 的序列化字段路径，例如 `runtime.local_port`、`auth.oauth_redirect_hosts`、`tunnel.type`。
- `config set` 不修改活动 `profiles.json`，只写入配置目录下受保护的 `pending-config/<workspace>.json`；pending 同时保存 staging 时的 base profile，活动配置被其他 GUI/CLI 进程修改后会检测 stale base 并拒绝覆盖。
- `config diff` 默认比较活动 profile 与当前 pending candidate，也可追加临时 `--set` 预览；输出 field-level changes 和共享 `applyPlan`，不会写磁盘或运行态。
- `config apply` 才把 pending candidate 提升为活动配置。Workspace daemon 运行时必须通过 protocol v6 `apply_config`；endpoint、协议或 PID 归属错误直接失败，不回退为 CLI 本地 `RuntimeSupervisor`。Gateway route/owner 需要更新时只通过独立 Gateway control reload。
- 任一运行态应用失败时会恢复旧 Workspace/settings，并对已经成功触及的 Workspace/Gateway 运行态执行受控回滚；pending 文件保留，便于修正后重试。全部成功后才删除 pending。
- 对已停止的 Workspace，`apply` 只持久化配置，不会隐式启动 listener 或 tunnel。如果 Workspace daemon 未运行但相关 GUI Server/外部 listener 仍在监听，CLI 会 fail-closed，避免活动运行态继续使用旧配置；应先停止 listener，或先由 Workspace daemon 接管运行态。
- pending 文件不包含独立 secret store 内容，单文件限制为 2 MiB；Unix staging 目录/文件分别使用 `0700` / `0600` 权限。

Gateway 设置页已改为 event-first：活动 daemon 通过 Gateway `events` 唤醒状态与有界日志刷新；只有 endpoint 明确 unavailable 时才以 2 秒间隔读取 configured/stopped 状态并重新探测 event endpoint。协议或远端错误会显示显式 fault，不会静默降级到轮询。

## 跨控制域只读聚合

Workspace daemon 与 Gateway daemon 仍保持独立运行权威。共享 `ControlPlaneStatus` / `ControlPlaneEventBatch` 只在客户端库层并发组合各控制域的只读 IPC 结果，不把 Gateway 状态塞回任何 Workspace daemon。

CLI 提供显式聚合入口：

```bash
anchor status --control-plane
anchor status --control-plane --watch
anchor events --control-plane
anchor events --control-plane --follow --wait 15
```

聚合状态为每个 Workspace 返回原始 `WorkspaceControlStatus` 加 canonical `mcpState/actionsState`。当某 Workspace 是活动 Gateway route 时，MCP 只有在监听 PID 与 Gateway daemon PID 匹配时才标记为 running；错误 PID 会标记 error，route 已选但端口暂未监听则为 recovering。这样 GUI 不再把 Gateway daemon 持有的 MCP listener 误判为“外部进程”。

聚合事件保留 Gateway cursor 与每个 Workspace cursor，最多返回 64 个按时间合并的事件；aggregate truncation 只推进实际返回事件对应的 source cursor，避免跨源丢事件。每次底层 long-poll slice 最长 1 秒；所有 endpoint 都 unavailable 时仍遵守该 cadence，避免 idle busy-loop。endpoint unavailable 只表示该 source 本轮没有事件；protocol/remote 错误直接终止聚合请求。

桌面全局 layout 使用一次 `get_control_plane_status` 代替原先每个 Workspace 的 MCP/Actions 双请求，并使用 `get_control_plane_events` 作为状态刷新唤醒。空长轮询只检查 Workspace 配置列表是否由外部 CLI 增删，不恢复旧的 N×service 状态 polling。

Windows Workspace 已使用真实后台 daemon：GUI 的 MCP/Actions 状态、启停、重启、Workspace Tunnel 和配置应用都走同一 Named Pipe 控制面，Workspace 进程内 `RuntimeSupervisor` 不再是 Windows 主路径。桌面二进制自身支持内部 `daemon-run` 分流，子进程会在进入 Tauri 单实例锁/窗口前直接执行 daemon 主循环，因此安装版 GUI 也能作为 per-user Workspace daemon 的启动镜像。

Windows Gateway 仍是独立的过渡边界：Gateway daemon 服务端暂未实现，所以只在 Gateway 域保留 process-local GUI Server。该兼容 Gateway 会把已由 Workspace daemon 启动的 MCP workspace 纳入 route 集合；Workspace daemon MCP 启停后只触发 Gateway 域 reconcile，不创建第二个 Workspace listener。后续 Gateway Windows daemon 落地后才能删除这部分剩余兼容运行时。

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
anchor --json events PROFILE_ID
anchor --json reload PROFILE_ID --service mcp
anchor --json doctor PROFILE_ID
anchor --json gateway status
anchor --json gateway start PROFILE_A PROFILE_B
anchor --json gateway logs
anchor --json gateway events
anchor --json status --control-plane
anchor --json events --control-plane
```

失败时返回非零退出码，并输出：

```json
{"ok":false,"error":"..."}
```

## 安全边界

- Workspace 后台 daemon 支持 Windows/Linux；Gateway 后台 daemon 当前仍仅支持 Linux。Windows Gateway GUI Server 是 Gateway 域的临时兼容路径，不代表 Gateway daemon 已支持 Windows；
- Windows Workspace Named Pipe 拒绝远程客户端，并使用 owner/System protected DACL；Unix UDS 继续使用私有目录/socket 权限；
- PID 所有权校验失败时拒绝生命周期写操作；Windows additionally 校验 state v2 `executablePath` 与实际 PID 镜像；
- `stop --force` 只在确认 daemon PID 后终止其进程树；
- 端口被 GUI 或外部进程占用时拒绝启动；
- `daemon-run` 是内部命令，不应直接调用；
- `gateway-daemon-run` 是内部命令，不应直接调用；
- 状态文件不是远程控制接口，不监听公网端口。

## 当前验证边界

Windows 开发机已完成真实 per-user Workspace daemon smoke：Actions-only `start` 经 Named Pipe readiness 返回后台 PID，`status` 确认 state v2 / PID image ownership / `owner=daemon`，`stop` 经 Named Pipe graceful shutdown 清理 state/PID 并释放端口；另有连续 Named Pipe round-trip、protected DACL 和 Windows 原子 state replace 专项测试。

Windows **SCM Service 安装/卸载尚未实现**。当前 `anchor start/stop/restart/status` 管理的是当前用户配置域下的后台 Workspace daemon 子进程，不注册 Windows Service，也不提供开机自启。Gateway Windows daemon 也仍待迁移。Linux 专用 `setsid`、`/proc` PID 校验和 Gateway 后台生命周期仍应在 Linux CI 或 Linux 主机执行 smoke：

```bash
anchor start PROFILE_ID
anchor status PROFILE_ID
anchor logs PROFILE_ID --follow
anchor restart PROFILE_ID
anchor stop PROFILE_ID

anchor gateway start PROFILE_A PROFILE_B
anchor gateway status
anchor gateway reload
anchor gateway restart
anchor gateway stop
```
