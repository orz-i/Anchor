# CLI、Daemon 与 GUI 控制面演进路线

## 目标

Anchor 的长期运行架构调整为：

- `anchor daemon` 是运行时、隧道、Gateway、日志和健康状态的唯一权威；
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
2. GUI 仍直接编排 listener、tunnel 和 Gateway，形成第二个运行权威；
3. CLI、GUI 分别组装部分状态，存在语义漂移风险；
4. 后台 daemon 仅支持 Linux，且没有版本化本地控制协议；
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
│ Anchor Daemon                                │
│ 配置应用、Runtime、Gateway、Tunnel、日志     │
│ 健康检查、恢复、事件流、单写者协调           │
└──────────────────────┬───────────────────────┘
                       │ Shared Application Core
┌──────────────────────▼───────────────────────┐
│ Workspace / Data / Runtime / MCP / Tunnel    │
└──────────────────────────────────────────────┘
```

## 不变量

1. **单一运行权威**：同一用户配置域内只能有一个 daemon 负责运行状态。
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
- daemon 持有唯一的 `RuntimeSupervisor`、Tunnel Supervisor 和 Gateway；
- 增加 stale socket、PID 复用、重复 daemon 和协议不兼容测试。

### 阶段 2：CLI 能力闭环

- CLI 所有运行命令改为调用 daemon，而不是本地构造 RuntimeSupervisor；
- 补齐 `config get/set/diff/apply`、`daemon install/uninstall/start/stop/status`；
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
