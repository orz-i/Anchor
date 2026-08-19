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
- Windows/Linux Workspace 后台 daemon、Windows/Linux 独立 Gateway daemon，以及适合外部 supervisor 监督的前台 `serve`；
- Rust `RuntimeSupervisor`、Tunnel Supervisor 和 Gateway；
- Tauri GUI 对上述运行时的直接进程内调用。

主要架构缺口：

1. daemon 原先位于 CLI 私有模块，桌面端无法复用其状态模型；
2. GUI 历史上直接编排 listener、tunnel 和 Gateway，形成第二个运行权威；主路径现已迁移到对应 daemon 控制客户端；
3. CLI、GUI 分别组装部分状态，存在语义漂移风险；
4. Workspace/Gateway daemon 已在 Windows/Linux 提供服务端：Windows 使用 owner/System protected-DACL Named Pipe，Linux 使用私有 UDS；Windows SCM 可按配置域监督两类 daemon 的开机恢复；
5. Windows GUI 的 process-local Runtime/Tunnel/Gateway 兼容运行权威已移除；仍需继续收缩跨平台遗留接口、增强字段级 hot reload 与升级治理。

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

- Workspace 协议版本为 `6`，请求和响应都包含 `protocolVersion` 与 `requestId`；
- 单连接单请求，使用最大 64 KiB 的换行分隔 JSON 帧；
- Unix daemon 在私有运行目录创建权限为 `0600` 的 UDS；其父目录保持 `0700`；
- Windows Workspace daemon 使用按用户和配置域派生名称的 Named Pipe；服务端拒绝远程客户端，DACL 仅授予对象 owner 与 LocalSystem。Windows state v2 保存 daemon executable path，并与 PID 实际镜像共同校验归属；
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
- Windows Workspace GUI 已切到 Workspace daemon Named Pipe；Gateway 也使用独立 Windows Gateway daemon。Windows GUI 不再以进程内 `RuntimeSupervisor`/Tunnel Supervisor/Gateway 作为运行权威；
- Gateway 已明确为独立全局控制域。GUI `get/set_mcp_gateway` 使用专用 Gateway control client；运行中配置由 daemon 事务应用，GUI 不创建共享 listener 或 Gateway tunnel；
- 若升级时检测到旧桌面进程仍持有 process-local listener，Windows GUI 将其报告为冲突并拒绝接管，防止旧进程与 daemon 控制域同时成为运行权威。

尚未完成：除 OAuth Callback 策略外的更多字段级 hot reload、跨控制域统一日志视图/历史事件持久化、Linux/macOS 原生 service manager 集成，以及崩溃报告/升级编排的更高层自动化。Windows SCM install/uninstall/开机计划与 Workspace/Gateway daemon 已落地；本阶段已补 build identity、只读版本探测和 lifecycle-only 旧协议排空边界，真实 Windows reboot 后自动恢复与安装包升级后的实机滚动切换仍属于发布验收项。

### 阶段 2：CLI 能力闭环

- CLI 所有运行命令改为调用 daemon，而不是本地构造 RuntimeSupervisor；
- `config get/set/diff/apply` 已完成；Workspace per-user daemon 的 `start/stop/restart/status` 已覆盖 Windows/Linux。剩余 service lifecycle 工作主要是 Windows SCM / Linux systemd / macOS launchd 的 install/uninstall/status 集成，而不是再新增一套 Workspace 运行编排；
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

当前第 1–5 项在 Windows 主路径均已完成：Workspace/Gateway 状态、日志、事件、生命周期、Tunnel、配置应用都走版本化 daemon 控制面；CLI `config get/set/diff/apply` staging/apply 闭环已完成；OAuth Callback 策略是首个无需 listener 重建的字段级 hot reload。Windows Workspace/Gateway daemon 都使用 Named Pipe + protected DACL + state v2/PID image ownership，SCM supervisor 负责按 `windows-service.json` 恢复所选控制域。Windows GUI 中最后的 process-local Server 兼容运行编排已删除；`RuntimeSupervisor` 仅保留给显式前台 `serve`/非 GUI 运行入口等仍需要的场景。下一步优先推进更多字段级 hot reload、统一运维可观察性与升级治理。

### 阶段 3.5：Web 管理面与 Tauri 退役边界

桌面配置壳不是长期管理入口。下一步将同一套 Svelte 管理 UI 迁移为浏览器可直接访问的 Web 管理面，并让 Tauri 仅作为过渡启动壳，最终删除。迁移必须遵守以下边界：

1. **桌面进程不持有运行权威**：Tauri 进程不得创建 `RuntimeSupervisor`、Tunnel Supervisor 或 Gateway listener，也不得在窗口退出时主动关闭 daemon/Gateway/Tunnel。运行资源只归 Workspace/Gateway daemon 或显式前台 CLI `serve` 所有。
2. **业务 UI 不直接依赖 Tauri**：`src/` 中的业务页面和 API 模块不得直接调用 `@tauri-apps/api` 或 `@tauri-apps/plugin-dialog`。平台差异必须经过统一 transport/dialog 适配层；Tauri adapter 只是迁移期实现。
3. **Web 管理 API 是可复用控制客户端**：浏览器端通过版本化、本地优先的管理 HTTP API 调用与 CLI/Tauri 相同的 Rust 控制语义，不允许在 HTTP handler 中复制 Workspace、daemon、Tunnel、Gateway 或 secret 业务规则。
4. **管理 HTTP 默认本机安全**：默认只监听 loopback；写请求必须具备独立管理会话认证，并校验 Origin/CSRF。不得复用 MCP/Actions 对公网暴露的认证入口，也不得把 secrets 放入 URL、日志或静态资源。
5. **离线配置与运行控制分离**：Web 管理面可以在 daemon 未运行时编辑磁盘配置；启动、停止、重载、Tunnel、Gateway 等运行操作仍必须通过对应 daemon 控制域，禁止 HTTP 层创建第二套运行时。

退役顺序固定为：

1. 删除桌面进程残留的 process-local runtime/maintenance/退出清理；
2. 将 Svelte 前端切到 transport-neutral API，并提供 Tauri/Web 两个 adapter；
3. 提供 `anchor admin`（或等价常驻管理入口）承载静态 Web UI 与版本化 `/api`；
4. Tauri 壳只负责本机安装期引导、打开管理页和必要的系统集成，不再暴露业务 invoke 命令；
5. Web 管理面、CLI 与系统 service manager 达成功能等价后，停止发布桌面包；
6. 经过一个明确弃用窗口后删除 `desktop` feature、`commands/`、Tauri 依赖和桌面构建脚本。

在第 5 步之前，桌面包仍属于兼容入口；在第 6 步之前，不得删除用户仍只能通过桌面完成的系统级能力。

截至 2026-08-19，本阶段已完成第四轮 Web Admin 实用化：

- Tauri `AppState` 已不再持有 `RuntimeSupervisor`；desktop maintenance 与窗口退出时的 Gateway/Tunnel 清理已删除，桌面进程退出不再影响 daemon 所有的运行资源；
- Svelte 业务页面/API 已不再直接依赖 Tauri 包，统一经 `invokeAdmin` 与 platform dialog adapter；Tauri 依赖只允许存在于这两个适配边界内，并由架构守卫测试约束；
- `anchor admin serve [--port PORT]` 仍固定绑定 `127.0.0.1`，现在由同一进程同时托管版本化 `/api/v1` 与生产 Svelte 静态站点；`pnpm cli:build` 会先执行 adapter-static 构建，再把 `build/` 作为只读资源嵌入 CLI 二进制，因此运行时不依赖单独的 Node/Vite 服务；
- 浏览器必须先 `POST /api/v1/session` 建立进程内独立管理会话。会话 ID 只通过 `HttpOnly; SameSite=Strict` cookie 发送，CSRF token 单独返回给同源页面；所有管理 command 都要求精确 `Host`、精确 `Origin`、same-origin Fetch 标记、有效 session 与 CSRF token，不复用 MCP/Actions 的公网认证凭据；
- Web Admin 已迁移工作区/控制面状态与事件、MCP/Actions runtime 状态、Workspace/Gateway 日志、Gateway 状态/事件、FRP profile 列表、software 状态、secret **读取**和 Windows Service 状态读取，用于现有管理 UI 的首屏和诊断展示；
- 普通管理能力已继续补齐：Workspace 创建/删除、目录打开、Skill inspection、Health checks、Canvs snapshot/task、FRP 非敏感 metadata 保存与 profile 删除均进入共享 `management.rs` 并由 Web dispatcher 暴露；FRP metadata 与 Token 写入已经拆成两个独立权限域，浏览器先持久化非敏感 metadata，再对稳定 profile ID 单独执行高权限 Token 确认；
- Workspace 配置更新已切换为共享 `preview/stage/apply` 事务：浏览器提交时携带加载时的 `baseProfile`，服务端在 staging 前要求它仍与 active 配置一致，防止旧页面覆盖 CLI/Tauri 的并发修改；pending/apply 继续复用 CLI 既有字段级 diff、资源校验、apply plan、daemon hot reload 与 stale-base 保护；
- Web Admin 已迁移 Workspace MCP/Actions daemon 启停/重启和 Tunnel start/restart/stop/test，全部经 `management.rs` 委托现有 `control` daemon 协议；Tunnel test 的临时服务运行态用 `reconcile_daemon` 恢复，不会把探测动作写成持久化 autostart desired-state；Web HTTP handler 不直接拥有 listener、RuntimeSupervisor 或 Tunnel Supervisor；
- Gateway 配置保存继续使用共享热应用/关闭语义。Gateway protocol v1 以 additive `set_routes` 增加 per-workspace route 生命周期：已有 Gateway daemon 在同一 PID 内重建内部 Workspace MCP/routes/tunnel 并使用 accepted → operation status 反馈；失败会恢复旧 route 集合。首个 route 在 daemon 停止时可启动 Gateway，最后一个 route 移除走受控 shutdown。Web 管理页可逐 Workspace 启停 route，存在未保存 Gateway 配置草稿时禁止 route mutation；
- Gateway 启用时，Web Admin 仍不允许绕过 Gateway 控制域直接启停单 Workspace MCP daemon/Tunnel；Tauri 的 Windows route helper 也已改为调用同一 `management.rs` 语义，不再维护第二套 route restart 编排；
- Web Admin 高权限确认已进入 Secret/FRP、Software 与 Windows Service 三个真实执行域：prepare ticket 绑定当前 HttpOnly session + allowlisted action + 非敏感 target fingerprint，批准后得到短 TTL、一次性 grant。Windows Service 的浏览器请求不提供可信 target；服务端会按 action 重建 `serviceName + opaque revision`，revision 哈希当前构建/可执行文件、配置域、SCM 注册与状态、配置 owner，以及 install/sync 需要的 desired/running plan 快照。执行前再次重建，任一相关状态漂移都会要求重新确认；
- Windows Service install/uninstall/start/stop/restart/sync 仅在 Windows capability manifest 中发布；Web 二次确认是 Anchor 应用级前置门槛，实际 SCM 变更仍使用既有 Windows UAC helper，未绕过操作系统提权。非 Windows 平台继续在 `unavailableCommands` 中 fail closed。Tauri command 已收缩为共享 `management.rs` adapter，不再维护第二套 Service lifecycle；
- 结构化 audit journal 仍只记录 session 指纹、action、phase/outcome，并使用有界大小/轮转与私有文件权限；不写 command args、payload、Secret/Token、下载 URL、owner、路径、target 或 opaque revision。grant 消费及 executor 成功/失败都会进入审计；
- 当前仍保留 unavailable 的旧高权限兼容命令主要是 `save_frp_profile`，避免重新把 FRP metadata 与 Token 合并为单一权限面。Web Admin session/health 按平台发布 `supportedCommands`、`mutationCommands`、全部 `privilegedCommands`、已评审 `privilegedExecutors` 与 `unavailableCommands`；
- Web adapter 会自动建立/缓存管理 session，并在 401 后最多重新 bootstrap 一次。业务页面仍无需感知 Tauri/Web transport 差异；架构守卫会扫描前端 API command，要求每个调用要么存在于正向 Web manifest，要么明确列入 privileged 集合。

下一轮重点转向持久化 `anchor admin` 的 service/autostart/status/upgrade/recovery 治理，以及桌面停止发布/弃用门槛复核。Windows Service Web executor 的真实 UAC/SCM 行为仍属于 Windows 发布验收项；当前 Linux 验证主机未安装 Windows Rust target，也无法替代真实 SCM/UAC live test。

### 阶段 4：运行与升级治理

- daemon 自恢复、崩溃报告和升级前排空；
- CLI 已新增 `anchor upgrade` runtime rollout：先对全部目标执行 preflight，再使用现有跨版本 `prepare_restart` 排空旧 Workspace/Gateway daemon；新 generation 必须通过 PID/端口/control readiness 和 `BuildIdentity` 校验。Linux 会从 `/proc/<pid>/exe` 保存真实旧映像并在新构建失败时自动回滚；Windows SCM 管理的 runtime 保持 supervisor 单一权威，普通 CLI fail-closed 并要求先更新 Service；
- 当前 rollout 明确是固定端口下的 bounded-outage replacement，不宣称 zero-downtime。真正无缝切换仍需 listener FD/handle handoff 或稳定前置代理层；
- Workspace/Gateway daemon state 与 `version` 已发布 additive `buildIdentity`；CLI doctor 可比较当前客户端构建与活动 Workspace daemon；Gateway canonical status 也携带 build identity；
- 协议兼容严格限制在只读 `version` 与稳定 lifecycle drain：Workspace 仅允许新客户端对 v2+ 旧 daemon 发送 `shutdown/prepare_restart`，Gateway 下限为 v1；其他写操作仍精确版本 fail-closed；
- Windows SCM 发布经过 PID/image 校验的 runtime build identity；`service status` 区分 current/different/unknown，显式 `service install` 更新已有 Service 时会等待旧 supervisor 停止后启动当前二进制；
- 服务安装状态、日志轮转和资源限制；
- 可重复的跨平台安装、升级、降级和卸载测试；Linux runtime rollout 自动回滚链已进入代码与回归范围，Windows SCM 安装包升级后的真实管理员/重启 smoke 仍属于发布验收项。

## 第一阶段完成标准

- CLI 与 Tauri 返回相同的 Workspace 控制状态 JSON；
- `anchor status`、`anchor status --all` 和 `anchor status <workspace>` 行为稳定；
- daemon 模型不再依赖 CLI 参数模块；
- desktop-only、cli-only 和 all-features 构建全部通过；
- 原 GUI 启停功能无回归；
- 文档明确后续不得向 GUI 新增运行编排逻辑。
