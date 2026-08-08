# CLI、Daemon 与 GUI 控制面演进路线

## 目标

Anchor 的长期运行架构调整为按控制域建立唯一运行权威：

- 每个 Workspace daemon 是该 Workspace listener、Tunnel、日志、事件和健康状态的唯一权威；
- 每个用户配置域最多一个独立 Gateway daemon，负责跨 Workspace Gateway listener、route 集合和唯一公网 Gateway tunnel；
- `anchor` CLI 是完整、可脚本化的运维与配置客户端；
- GUI 只负责配置编辑、状态展示和操作确认，不再自行持有业务运行时；
- CLI 与 GUI 使用同一组版本化控制面模型和命令语义。

这是一项渐进式迁移。现有 GUI 功能在每个阶段都必须保持可用，不能通过一次性重写切换。

## 当前基线

现有实现已经具备：

- 独立 `anchor` CLI，包含 Workspace、Gateway、运行、日志和诊断命令；
- Linux 后台 daemon，以及适合 systemd 监督的前台 `serve`；
- Rust `RuntimeSupervisor`、Tunnel Supervisor 和 Gateway；
- Tauri GUI 对上述运行时的直接进程内调用。

主要架构缺口：

1. daemon 原先位于 CLI 私有模块，桌面端无法复用其状态模型；
2. GUI 历史上直接编排 listener、tunnel 和 Gateway，形成第二个运行权威；主路径现已迁移到对应 daemon 控制客户端；
3. CLI、GUI 分别组装部分状态，存在语义漂移风险；
4. 后台 daemon 服务端目前仍仅支持 Linux；Workspace 和 Gateway 均已有独立版本化本地控制协议，Windows 服务端/ACL 尚未落地；Windows GUI 在过渡期自动使用单一 process-local Server 模式保持完整可用性；
5. 配置写入、运行应用和进程监督尚未形成清晰的单写者边界。

## 目标分层

```text
┌──────────────────────────────────────────────┐
│ GUI 配置壳                                   │
│ 表单、状态展示、日志查看、显式操作确认       │
└──────────────────────┬───────────────────────┘
                       │ Control Client
┌──────────────────────▼───────────────────────┐
│ anchor CLI                                   │
│ 人工运维、脚本、诊断、导入导出、服务安装     │
└──────────────────────┬───────────────────────┘
                       │ 版本化本地 IPC
┌──────────────────────▼───────────────────────┐
│ Runtime Control Domains                      │
│ Workspace daemons + global Gateway daemon    │
│ 配置应用、Runtime、Tunnel、Gateway、事件     │
└──────────────────────┬───────────────────────┘
                       │ Shared Application Core
┌──────────────────────▼───────────────────────┐
│ Workspace / Data / Runtime / MCP / Tunnel    │
└──────────────────────────────────────────────┘
```

## 不变量

1. **按控制域单一运行权威**：每个 Workspace 只有一个 Workspace daemon；每个用户配置域只有一个独立 Gateway daemon。两者不能互相接管运行资源。
2. **共享语义**：CLI、GUI 不自行推导 daemon、端口或隧道状态。
3. **版本化协议**：请求、响应、事件和错误码都带协议版本，并支持兼容性检查。
4. **本地安全边界**：控制接口默认只绑定本地 IPC，使用当前用户权限和受保护凭据。
5. **配置与运行分离**：保存配置不隐式重启；应用配置必须是显式、可审计操作。
6. **可降级**：daemon 不可用时 GUI 仍可读取和编辑离线配置，但不得启动第二套运行时。

## 分阶段计划

### 阶段 0：共享控制面基础

- 将 daemon 从 CLI 私有模块提升到共享库模块；
- 建立 `WorkspaceControlStatus`，统一 daemon、端口、PID 和所有权状态；
- CLI 与 Tauri 命令复用同一个状态构建函数；
- `anchor status` 支持一次输出全部工作区。

阶段 0 不改变 GUI 原有运行行为，只建立后续迁移所需的稳定接口。

### 阶段 1：Daemon 成为可连接服务

- 引入跨平台 daemon 入口和运行目录；
- 使用 Unix Domain Socket / Windows Named Pipe 提供本地 IPC；
- 实现 `ping`、`version`、`status`、`events`、`shutdown`；
- Workspace daemon 持有该 Workspace 唯一的 `RuntimeSupervisor` 与 Tunnel Supervisor；跨 Workspace Gateway 使用独立控制域，不归属于任一 Workspace daemon；
- 增加 stale socket、PID 复用、重复 daemon 和协议不兼容测试。

阶段 1 按两个子阶段实施：

1. **协议与只读接入**：先提供版本化 `ping`、`version`、`workspace_status`，CLI 与 GUI 的状态读取优先访问 daemon；仅在端点不存在、拒绝连接或超时时回退到本地只读探测。协议损坏、请求 ID 不匹配和版本不兼容不得静默回退。
2. **完整运行控制**：迁移事件流、日志、启停、重载和 shutdown，并移除 CLI/GUI 的第二套运行编排。

当前已完成只读接入、生命周期/Tunnel 写控制、事件消费和单服务配置 reload 的基础：

- 协议版本为 `4`，请求和响应都包含 `protocolVersion` 与 `requestId`；
- 单连接单请求，使用最大 64 KiB 的换行分隔 JSON 帧；
- Unix daemon 在私有运行目录创建权限为 `0600` 的 UDS；其父目录保持 `0700`；
- Windows 客户端使用按用户和配置域派生名称的 Named Pipe 地址。Windows daemon 服务端将在服务生命周期实现时启用，并必须配置当前用户专属 ACL；
- daemon readiness 同时要求所选端口归属正确 PID 且 `ping` 成功；
- `workspace_status`、`logs`、`events`、`reload`、`shutdown`、`prepare_restart`、`tunnel_control` 和 `operation_status` 请求必须与端点所属 Workspace ID 一致；
- 运行中 daemon 的日志读取使用有界游标 IPC，单响应日志内容预算为 8 KiB；daemon 已停止时仍可离线读取历史日志；
- `stop` 和 `restart` 必须先由目标 daemon 通过 IPC 接受并协调优雅退出；IPC 不可用时不得回退到客户端直接进程控制；
- `start` 在 daemon 不存在时仍是引导命令；若状态显示 daemon 已运行，则必须先通过 IPC `ping` 验证控制面。
- daemon 状态同时保存 `tunnelServices=mcp|actions|all`，MCP/Actions listener 与 tunnel ownership 可独立组合；公开 CLI `--tunnel` 仍保持“为所选服务启用隧道”的兼容语义；
- tunnel 写操作采用 `accepted → pending/running → succeeded/failed` 的异步操作模型：初始响应完整写回后，daemon 才在自身 Tunnel Supervisor 内执行 start/stop/restart；FRP 重载继续使用原子 route replacement，失败时恢复旧线路和旧配置。
- daemon 事件使用进程内有界 journal：每 Workspace 最多保留 256 条，单批最多 32 条，游标为 `streamId + sequence`；长轮询最长 25 秒，daemon 重启或游标越过 retained window 时显式返回 reset；
- `reload` 复用异步 operation 模型，运行中的目标服务只重建该 listener，Workspace daemon、另一 listener 和 Tunnel ownership 保持不变；新 listener 启动失败时尝试恢复旧 listener；
- CLI 新增 `anchor events <workspace> [-f]` 与 `anchor reload <workspace> --service ...`，为脚本提供原生事件消费和非整进程配置应用入口。

独立 Gateway 控制域现已具备后台 daemon 基础：

- Gateway control protocol 独立版本为 `1`，与 Workspace protocol v6 分离；每个请求携带 `configScope`，拒绝连接到其他配置域的 daemon；
- Linux 使用全局 `gateway.lock`、`gateway.pid`、`gateway.json`、`gateway.sock`；状态保存 PID、配置域、所选 route Workspace IDs、Gateway 本地端口和版本；
- `anchor gateway status/start/stop/restart/reload` 使用专用 Gateway control client；`gateway serve` 继续保留为前台调试/外部 supervisor 入口；
- `shutdown`、`prepare_restart`、`reload` 和运行中配置应用都禁止本地运行时回退；`reload`/`apply_config` 使用 accepted → operation status 异步模型；
- 运行中配置修改由 Gateway daemon 先切换运行态、更新 daemon state，再持久化；失败会停止新运行态并尝试恢复旧 listener/routes/tunnel；禁用配置采用 daemon shutdown → 确认退出 → 持久化 disabled；
- Gateway route/owner 实际使用的 MCP tunnel identity 变化会触发 Gateway daemon reload；名称、Actions 或普通 MCP listener 策略变化不会无谓 reload Gateway；失败时控制层恢复旧 Workspace/settings 并重新对齐旧运行态；活动 route Workspace 在 daemon 停止前禁止删除/注销；
- GUI Gateway 状态和 route 列表直接来自 Gateway daemon 状态，不再逐 Workspace 轮询来猜测 route；桌面配置缓存每次操作前从磁盘刷新，避免覆盖 daemon 的异步 observation 写入。
- Gateway protocol v1 已以 additive methods 增加有界 `logs` 与 `events`：日志正文单响应最多 8 KiB；事件 journal 保留 256 条、单批 32 条、最长 25 秒 long-poll，并使用 `streamId + sequence` 可恢复游标；
- CLI 新增 `gateway logs/events`；运行中日志和事件不允许在 protocol/remote 错误时回退到本地文件或状态 polling，只有 daemon 明确停止时可离线读取历史日志；
- 新增纯只读 `ControlPlaneStatus` / `ControlPlaneEventBatch` 聚合层，以独立 Gateway cursor + 每 Workspace cursor 组合各控制域；聚合 truncation 不推进未返回事件的 source cursor；
- CLI 新增 `status --control-plane` 与 `events --control-plane`；GUI 全局 layout 已从 N×2 runtime 请求切换为单 aggregate snapshot + aggregate event-first 唤醒；Gateway 设置页也改为 Gateway event-first，并显示有界 daemon log 尾部。

GUI 工作区控制迁移现状：

- Workspace 的 MCP/Actions 状态、启动、停止和重启已改为 daemon 控制客户端；GUI 不再为这些命令创建进程内 `RuntimeSupervisor` listener；
- MCP 与 Actions 的独立开关映射为 daemon 的 `mcp`、`actions` 或 `all` 服务选择；调整其中一个服务时可能需要协调重启整个 Workspace daemon；
- GUI 日志在 daemon 运行时强制使用有界 IPC，daemon 停止时才允许使用同一套有界本地读取器查看历史日志；
- Workspace 删除和密钥再生成会先通过 daemon 控制面停止或重启目标进程；
- MCP/Actions tunnel 状态、启动、停止、重载和测试已迁入 Workspace daemon；GUI 保存 tunnel 配置不再追加一次整 daemon 重启；
- Workspace 页面已从固定 5 秒双 runtime 轮询迁移为 daemon event-first 长轮询；只有 endpoint unavailable 才进入 polling fallback，协议/远端错误不会静默降级，fallback 会继续探测并自动恢复事件模式；
- Workspace 配置保存已把差异判断从 GUI 收回 Rust 控制层：纯元数据不 reload，MCP/Actions 运行参数与认证身份只 reload 对应活动 listener；GUI 不再自行读取运行状态后调用 restart；
- Workspace protocol 升级为 v6：OAuth Callback URI/Host 支持 daemon 进程内字段级 hot update；新增 daemon-owner `apply_config`，由运行权威以当前内存 profile 对比磁盘 desired profile，事务协调 listener/direct tunnel 并回滚失败操作；
- GUI 进程内 `RuntimeSupervisor` / Tunnel Supervisor 在 daemon 支持的平台只保留旧兼容路径；在 Windows daemon 服务端尚未实现期间，它们构成显式、自动选择的 GUI Server 模式唯一运行权威，不能与 daemon 同时启用；
- Gateway 已明确为独立全局控制域。GUI `get/set_mcp_gateway` 使用专用 Gateway control client；运行中配置由 daemon 事务应用，GUI 不创建共享 listener 或 Gateway tunnel；
- 若旧桌面进程仍持有兼容 Gateway listener，GUI 仍拒绝热改 Gateway 配置，防止旧进程与新 daemon 控制域同时成为运行权威。

尚未完成：除 OAuth Callback 策略外的更多字段级 hot reload、跨控制域统一日志视图/历史事件持久化、Windows Named Pipe 服务端与当前用户 ACL、系统服务安装入口，以及 Windows daemon 完成后删除 GUI Server 兼容 `RuntimeSupervisor`。

### 阶段 2：CLI 能力闭环

- CLI 所有运行命令改为调用 daemon，而不是本地构造 RuntimeSupervisor；
- `config get/set/diff/apply` 已完成：set 使用带 base 快照的受保护 staging，diff 输出字段级变化与共享 apply plan，apply 通过 Workspace protocol v6 / Gateway 专用 control 显式应用且不会启动停止态服务；剩余补齐 `daemon install/uninstall/start/stop/status`；
- 所有命令支持稳定 JSON 输出、错误码和自动化退出码；
- 增加配置导入导出、备份恢复和只读诊断命令；
- Linux systemd、macOS launchd、Windows Service 使用同一前台 daemon 入口。

### 阶段 3：GUI 收缩为配置壳

迁移顺序必须是：

1. GUI 状态读取改为 daemon 控制接口；
2. 日志和事件流改为 daemon 接口；
3. 启停、重载、隧道和 Gateway 操作改为 daemon 接口；
4. GUI 进程内 `RuntimeSupervisor` 进入兼容模式；
5. 删除 GUI 内的业务编排，仅保留配置、展示和控制客户端。

当前已完成第 1 项、日志读取、Workspace 级启停/重启、Workspace Tunnel 写控制、Workspace 事件消费、配置差异后端 apply plan、单服务 reload，以及 CLI `config get/set/diff/apply` staging/apply 闭环；OAuth Callback 策略已经是首个无需 listener 重建的字段级 hot reload，Workspace daemon 也可通过 v6 `apply_config` 统一应用磁盘 desired config。Gateway 已从 GUI 运行编排中拆出，并具备独立后台 daemon、状态/生命周期/reload/config-apply/logs/events 控制面；全局 layout 已使用跨控制域 aggregate status/events。Windows GUI 已补上自动 process-local Server 兼容模式，避免 Named Pipe daemon 服务端完成前桌面不可用。下一步优先推进更多字段级 hot reload、系统服务安装和 Windows Named Pipe server/ACL，并在该后台控制面可用后删除 Windows GUI Server 兼容 RuntimeSupervisor。

### 阶段 4：运行与升级治理

- daemon 自恢复、崩溃报告和升级前排空；
- CLI/GUI/daemon 版本协商；
- 服务安装状态、日志轮转和资源限制；
- 可重复的跨平台安装、升级、降级和卸载测试。

## 第一阶段完成标准

- CLI 与 Tauri 返回相同的 Workspace 控制状态 JSON；
- `anchor status`、`anchor status --all` 和 `anchor status <workspace>` 行为稳定；
- daemon 模型不再依赖 CLI 参数模块；
- desktop-only、cli-only 和 all-features 构建全部通过；
- 原 GUI 启停功能无回归；
- 文档明确后续不得向 GUI 新增运行编排逻辑。
