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

# 预检并把运行中的 daemon 切换到当前 CLI 构建
anchor upgrade PROFILE_ID --dry-run
anchor upgrade PROFILE_ID

# 一次升级所有运行中的 Workspace/Gateway daemon
anchor upgrade --all

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

### `upgrade`

```bash
anchor upgrade <workspace> [workspace ...] [--gateway] \
  [--timeout SECONDS] [--force] [--dry-run] [--allow-no-rollback]

anchor upgrade --gateway [--dry-run]
anchor upgrade --all [--dry-run]
```

`upgrade` 是 **runtime rollout**，不是 CLI 下载器：先安装或运行目标 Anchor CLI 构建，再用该命令把正在运行的 Workspace/Gateway daemon 切换到当前 CLI 的 `BuildIdentity`。

- 所有目标先整体执行 dry-run preflight；任何目标无法安全排空或无法准备 rollback 时，在停止第一个 daemon 前就 fail-closed；
- 新客户端继续只对受支持的旧协议使用 `version` / `shutdown` / `prepare_restart` lifecycle 兼容，不扩大普通写权限；
- 旧 daemon 完全退出后才启动新 daemon，因此不会让两代 listener 竞争同一固定端口；
- readiness 同时验证 state PID、业务端口 owner 与本地 control ping；readiness 通过后还必须确认新 daemon `BuildIdentity` 与当前 CLI 一致；
- Linux 在旧进程仍存活时从 `/proc/<pid>/exe` 保存真实运行映像。即使磁盘上的原路径已经被新版替换，新构建启动失败时仍能从该快照恢复旧 daemon；
- 非 Linux 平台默认要求旧 state 的 `executablePath` 能被证明与当前 CLI 不是同一二进制，否则拒绝升级；只有显式 `--allow-no-rollback` 才允许放弃自动回滚；
- 任一目标启动失败但旧构建恢复成功时状态为 `rolled_back`，命令仍返回非零退出码，自动化不会把“已回滚”误判成升级成功；
- `--all` 选择所有正在运行的 Workspace daemon，并包含正在运行的 Gateway；显式 Workspace selector 可与 `--gateway` 组合；
- Windows SCM 正在管理所选 runtime 时，普通 CLI 不与 supervisor 竞争启动权。先使用管理员权限执行 `anchor service install` 将 SCM supervisor 更新到当前构建，再由 Service 排空/恢复 desired state。

当前实现是 **bounded-outage rolling replacement**：停机窗口从旧 PID 完全退出到新 generation readiness 完成。固定端口架构尚未引入 listener FD/handle handoff 或稳定前置代理，因此不宣称 zero-downtime。

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

## 自适应执行资源治理

Workspace daemon 会对 `exec_command` 启动的子进程应用自适应资源预算，避免构建、测试或多个
并发命令直接吃满宿主机。该预算只约束**正在运行的子进程**；CommandSession 的结果留存数量
仍是独立概念，不能再被解释为可同时运行的进程数。

- CPU 基线来自运行时实际可见的并行度；Linux 还会读取 cgroup CPU quota，并采用更严格的值；
- 默认最多使用有效 CPU 的 75%，且有效 CPU 大于 1 时至少保留 1 个逻辑 CPU 给操作系统和交互进程；
- live exec 并发会根据 CPU 与可检测的物理/cgroup 内存自动收缩到 1–4 个；例如 2 CPU 环境默认只允许 1 个 live exec；
- `cargo build/test/check/clippy`、常见前端 build/test、`make`、`ninja`、CMake、Go/.NET 测试构建等已识别重任务会在共享 Harness 数据域内跨 Workspace daemon 串行；
- 常见构建/数值运行时会继承并行度上限，包括 Cargo/Rust tests、Rayon、Go、CMake、libuv、OpenMP、OpenBLAS、MKL 和 NumExpr；显式更低的数值限制会保留；
- Windows 子进程使用 below-normal priority；Unix 在 spawn 成功后立即对该 child PID 降低调度优先级。优先级调整失败不会放宽前面的硬并发/并行度限制；
- 资源预算和 CommandSession 容量都会在 child spawn 前检查；资源队列默认最多等待 15 秒，超时返回可重试错误且不会启动子进程；
- `environment check` 的 `execution_resources` 字段会输出探测到的 CPU/内存、保留 CPU、有效执行预算、live exec 上限和重任务策略。

仅提供两个可选环境变量用于**进一步收紧**自动策略：

```bash
# CPU 目标百分比；有效范围 25–75，高于 75 会被钳制到 75
ANCHOR_EXEC_CPU_PERCENT=50

# live exec 上限；有效范围 1–4，且不能高于自动计算值
ANCHOR_EXEC_MAX_CONCURRENT=1
```

因此，运维侧无需根据机器核数手工设置固定并发。低资源主机会自动收缩，高资源主机也保留
明确的响应性余量；如机器仍承担数据库、模型服务等高负载工作，可通过上述变量继续收紧。

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

`version` 与 daemon state 还以 additive optional 字段发布 `buildIdentity`（package version、Git SHA、dirty 标志和构建工作区）。旧 daemon 没有该字段时新客户端按 `None` 处理，因此同一 `0.1.x` 包版本下也能在新旧构建之间建立明确的升级可观察性，而不会把“package version 相同”误当成“运行构建相同”。

Workspace daemon 的 listener 与 tunnel ownership 分开记录。`service` 仍表示 `mcp|actions|all` listener 选择，`tunnelServices` 表示由该 daemon 实际管理的 `mcp|actions|all` 隧道集合。旧状态中的 `tunnel=true` 继续按“所选 listener 全部启用隧道”解释，保证升级兼容。

Tunnel 写控制是异步操作：daemon 先返回 `OperationAccepted` 并完整关闭本次响应帧，然后主循环才执行 Tunnel Supervisor 的 start/stop/restart；客户端使用 `operation_status` 查询 `pending/running/succeeded/failed`。这保证协议响应不会被正在执行的 tunnel 替换操作截断。FRP restart 继续使用 supervisor 的原子 route replacement，失败时保留旧线路；非 FRP restart 失败时也尝试恢复上一线路和持久配置。

- reload 只允许应用到当前由 daemon 运行的目标服务；未运行的服务会 fail-closed，避免另一活动 listener 仍使用旧配置而 daemon 内存 profile 已提前切到新配置。CLI `--service all` 会先整体校验两个服务都处于活动状态，再执行任何 reload，避免半成功。

事件控制面使用有界内存 journal。daemon 启动时创建新的 `streamId`，事件按 sequence 单调递增；当前事件包括 daemon ready/stopping、service state、Tunnel state、MCP activity 与 reload 结果。客户端可用 `events` 长轮询等待变化，daemon 重启或 retained window 越界时通过 `reset` 显式要求重建游标。

CLI 生命周期语义：

- `start` 在 daemon 不存在时负责创建后台进程；若状态显示 daemon 已运行，必须先通过 IPC `ping` 验证目标控制面；
- `stop` 先发送 `shutdown`，由目标 daemon 完成 Runtime、Tunnel 和监听器清理后退出；
- `restart` 先发送 `prepare_restart`，等待原 PID 退出后再执行一次新的 `start` 引导；
- 普通运行/配置写请求继续要求当前协议精确匹配。只有 `version` 只读探测，以及自 Workspace protocol v2 起未改变 wire shape 的 `shutdown` / `prepare_restart`，允许新客户端在发现**较旧且不低于 v2** 的 daemon 后以该旧协议版本重试；该兼容通道只用于识别和排空旧运行权威，不允许 tunnel/reload/apply_config 等新写语义跨版本执行；
- IPC 不可用、生命周期协议低于兼容下限、新 daemon 比客户端更新、响应 PID 不匹配或普通写请求协议不兼容时，命令直接失败，不会回退到客户端直接发送信号或启动第二套运行时；
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
- Gateway 不归属于任何 Workspace daemon。Windows/Linux GUI/CLI 都使用独立 Gateway daemon；Windows 使用配置域级 Named Pipe，Linux 使用私有 UDS。GUI 不再创建 process-local Gateway listener 或 Tunnel，route 始终由 Gateway daemon 持有并指向对应 Workspace 目标端口。

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

状态文件记录 daemon PID、配置域、route Workspace IDs、Gateway 本地端口、版本和 optional `buildIdentity`。Unix socket 父目录保持 `0700`、socket 为 `0600`。readiness 同时要求 Gateway 本地端口属于目标 PID 且 control `ping` 成功。

Gateway lifecycle 从 protocol v1 起保持稳定，因此未来客户端协议升级后，只允许 `version` 与 `shutdown` / `prepare_restart` 对较旧且不低于 v1 的 daemon 使用其旧协议版本重试。其他 Gateway 写请求仍严格要求当前协议，避免“为了升级兼容”扩大运行控制权限或产生部分应用。

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

Web Admin 全局 layout 使用一次 `get_control_plane_status` 代替每个 Workspace 的 MCP/Actions 双请求，并使用 `get_control_plane_events` 作为状态刷新唤醒。空长轮询只检查 Workspace 配置列表是否由外部 CLI 增删，不恢复旧的 N×service 状态 polling。

Windows Workspace 使用真实后台 daemon：Web Admin/CLI 的 MCP/Actions 状态、启停、重启、Workspace Tunnel 和配置应用都走同一 Named Pipe 控制面。`anchor.exe` 自身承载内部 `daemon-run` / Gateway / Service 子命令，因此不依赖任何 desktop 启动镜像。

Windows Gateway 也使用独立后台 daemon 与 Named Pipe 控制面。Gateway 开启时，选中的 Workspace MCP listener 由 Gateway daemon PID 直接持有，Workspace 独立 daemon 不再同时持有同一 MCP route；Actions 仍可由 Workspace daemon 独立运行。Web Admin 的 MCP 启停在 Gateway 模式下变成 route 集合更新，并通过受控 reload 完成。

Windows 还提供配置域级 SCM Service，用于开机自动恢复已选择的 Workspace/Gateway 运行计划：

```powershell
anchor service status
anchor service sync
anchor service install
anchor service start
anchor service restart
anchor service stop
anchor service uninstall
```

`service sync` 把当前后台 Workspace daemon 与 Gateway route 集合写入 `windows-service.json`；后续 GUI/CLI 的 Workspace/Gateway 启停也会持续更新这个计划。`install` 注册配置目录专属的 `AnchorControlPlane-<scope>` 服务并设置 `start= auto`。安装、卸载通常需要管理员权限；服务本身运行在 Session 0，因此计划同时保存配置所有者 SID/用户名，用于复用用户态 Named Pipe 身份并给 owner/System 设置受保护 DACL。

Windows 凭据同时保留 CurrentUser DPAPI 主密文和 LocalMachine DPAPI 的 Service mirror。普通 GUI/CLI 始终使用 CurrentUser 主密文；`service install/start/restart` 会在进入 UAC 前由配置所有者刷新 Service mirror，SCM supervisor 与其子 daemon 通过显式 service context 读取 mirror。SCM 自身仅为恢复 desired-state 时读取不含秘密的 `profiles.json`，因此 Session 0 无法解密用户凭据时也不会阻塞 stale PID 清理和 Workspace 重拉。旧凭据 envelope 没有 Service mirror 时，新版用户进程首次成功读取会原地补充 mirror，同时保持原 `protection/payload` 不变，便于滚动升级期间旧用户态 daemon 继续读取主密文。用户侧后续保存配置刷新 mirror 时，会保留 Service 已写入的 OAuth refresh-token replay runtime scope，避免普通配置保存回滚服务侧防重放状态。

SCM supervisor 运行时额外写入 `windows-service-runtime.json`，记录当前 Service PID、启动时间、实际 executable path 和 `buildIdentity`。Windows 上 plan/runtime state 都使用可覆盖旧目标的 write-through 原子替换，避免系统重启后旧 runtime 文件导致新 Service 无法发布 build identity。`service status` 使用 `sc queryex` 获取真实 SCM PID，并同时校验 runtime state 的 PID 存活与进程镜像路径，输出 `buildState=not_installed|stopped|current|different|unknown`。旧 Service 没有 runtime state 时明确返回 `unknown`，不会因为 package version 相同而误报为 current。

`service install` 同时承担显式“安装/更新到当前二进制”的语义：若 Service 已安装且仍运行，先请求 SCM 停止并等待真正进入 `STOPPED`，由 supervisor 优雅排空其管理的 Workspace/Gateway daemon，再启动刚写入 `binPath` 的当前构建并等待 `RUNNING`。GUI 的“更新服务版本”按钮复用同一路径并通过 UAC 执行。普通状态查询、Workspace 配置保存或桌面启动都不会隐式重启 SCM。

GUI 的安装/卸载/启停按钮会通过内部 `service-admin-run` helper 触发标准 Windows UAC，只提升该次 SCM 操作，不要求整个 Anchor 桌面进程长期以管理员身份运行；普通 CLI 命令仍要求从已提升的管理员终端执行。`service-admin-run` 与 `service-run` 都是内部入口，不应人工调用。

## 与 systemd 的关系

Windows 上可使用上述 SCM Service 提供开机自启和操作系统级 supervisor；Linux 生产服务器仍推荐 systemd 直接监督前台 `serve`。

Linux systemd 示例：

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
anchor --json service status
anchor --json service sync
```

失败时返回非零退出码，并输出：

```json
{"ok":false,"error":"..."}
```

## 安全边界

- Workspace 与 Gateway 后台 daemon 都支持 Windows/Linux；Windows 不再需要 process-local Gateway Server 作为正常运行路径；
- Windows Workspace/Gateway Named Pipe 拒绝远程客户端，并使用 owner/System protected DACL；SCM supervisor 本身保持 LocalSystem。安装/更新 Service 时会把配置 owner SID/username 固定到管理员保护的 SCM `ImagePath`，service-run 再以该可信身份匹配 Active Windows 登录会话，并通过 owner primary token + 用户环境启动 Workspace/Gateway daemon。用户可写的 `windows-service.json` 只保存 desired state/展示元数据，不能选择要 impersonate 的 Windows 用户。owner 未登录时 fail closed 并等待后续 reconcile，绝不以 LocalSystem 身份承载 Workspace 命令执行；旧 Service registration 未携带可信 owner 时也会 fail closed，必须先执行 service install/update；Unix UDS 继续使用私有目录/socket 权限；
- PID 所有权校验失败时拒绝生命周期写操作；Windows additionally 校验 state v2 `executablePath` 与实际 PID 镜像；
- Windows daemon state 还校验实际进程创建时间必须早于且接近 state 的 `startedAtUnix`，避免跨系统重启后旧 PID 被新的 `anchor.exe` 实例复用时误判为原 daemon；
- `stop --force` 只在确认 daemon PID 后终止其进程树；
- 端口被 GUI 或外部进程占用时拒绝启动；
- `daemon-run` 是内部命令，不应直接调用；
- `gateway-daemon-run` 是内部命令，不应直接调用；
- `service-run` 是 Windows SCM 内部入口，不应直接调用；
- `service-admin-run` 是 Windows UAC 内部入口，不应直接调用；
- 状态文件不是远程控制接口，不监听公网端口。

## 当前验证边界

Windows 开发机已完成真实 per-user Workspace daemon smoke，并完成真实 Gateway daemon `start/status/reload/restart/stop` smoke：Gateway route listener 的 PID 与 Gateway daemon PID 一致，Workspace 状态报告 `owner=gateway`，restart 完成 PID handoff，stop 后释放 route 并清空开机 Gateway 计划。Windows Gateway Named Pipe 连续请求和原生 control-plane 模式也有专项回归。

SCM `status` 与计划持久化已在标准用户 token 下真实验证；`install` 会调用 `sc.exe` 注册自动启动服务。当前开发测试进程不是管理员，因此真实 install probe 按预期返回 Windows SCM 错误 5，并明确提示需要管理员权限，没有创建服务。完整 `install → start → reboot/autostart → stop → uninstall` 仍需要从提升权限的 Windows 测试终端执行。Linux 专用 `setsid`、`/proc` PID 校验和后台生命周期仍应在 Linux CI 或 Linux 主机执行 smoke：

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
