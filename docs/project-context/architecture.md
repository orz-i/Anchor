# 架构设计

> 本文档描述 Anchor Rust 的架构和项目结构。

## 项目结构

```
anchor/
├── src-tauri/              # Tauri Rust 后端
│   ├── src/
│   │   ├── main.rs         # 入口
│   │   ├── lib.rs          # 库入口
│   │   ├── commands/       # Tauri IPC 命令
│   │   ├── workspace/      # Workspace 配置存储
│   │   ├── data/           # profiles / protected secrets 持久化
│   │   ├── runtime/        # Runtime 状态机
│   │   ├── mcp/            # 内嵌 MCP 协议 + 工具
│   │   ├── harness/        # Task / baseline / verification / journal
│   │   ├── tools/          # 共享开发工具内核
│   │   ├── tunnel/         # FRP / Cloudflare 隧道管理
│   │   ├── auth/           # OAuth / Bearer 认证
│   │   └── health/         # 健康检查
│   └── Cargo.toml
├── src/                    # Svelte 前端
│   ├── lib/
│   │   ├── components/     # UI 组件
│   │   └── stores/         # 状态管理
│   └── routes/             # 页面路由
├── docs/                   # 项目文档
│   ├── specs/              # 功能规格
│   ├── project-context/    # 项目上下文
│   └── graph-insights/     # 代码图谱
├── scripts/                # 构建、验证与桌面打包辅助脚本
└── AGENTS.md               # Agent 入口
```

## 当前状态

Anchor 已形成可构建的 Rust/Tauri 桌面应用、独立 Linux CLI、MCP/Actions 服务、Gateway、隧道监督、Agent Skills 和持久 Harness。当前实现与测试均以 Rust 源码为权威，不依赖外部参考实现。

## 架构模式

### 分层设计

```
┌─────────────────────────────────────────┐
│  UI Layer (Svelte)                      │
│  Web 管理面 / 过渡期 Tauri 壳            │
├─────────────────────────────────────────┤
│  Admin Transport / CLI                  │
│  Web HTTP adapter / Tauri adapter       │
├─────────────────────────────────────────┤
│  Shared Control Plane                   │
│  版本化状态模型与控制客户端               │
├─────────────────────────────────────────┤
│  Anchor Daemon / App Orchestrator       │
│  Workspace Store / Runtime State Machine│
├─────────────────────────────────────────┤
│  MCP Core (内嵌, Rust)                  │
│  axum HTTP /mcp + 工具实现               │
├─────────────────────────────────────────┤
│  Tunnel Supervisor (Rust)               │
│  管理 cloudflared / frp 外部进程         │
└─────────────────────────────────────────┘
```

### 关键架构特征

| 维度 | 当前实现 |
|------|----------|
| MCP 运行时 | 内嵌 Rust + axum Streamable HTTP |
| 进程管理 | Windows/Linux：Workspace daemon 管理各自 listener + Workspace Tunnel，独立 Gateway daemon 管理全局 Gateway listener/routes/tunnel；Windows 可由配置域级 SCM supervisor 按持久化计划开机恢复这些 daemon |
| UI | SvelteKit 管理 UI；Tauri 2 作为过渡本机壳，目标入口为 Web 管理面 |
| 密钥 | 受保护凭据封装；Windows 使用当前用户 DPAPI |
| 分发 | 桌面安装包与独立 `anchor` CLI |

目标方向是按运行控制域建立唯一权威：每个 Workspace daemon 管理该 Workspace 的 listener 与 Tunnel；跨 Workspace Gateway 使用独立全局控制域。CLI 提供完整运维能力；Svelte 管理 UI 通过 transport-neutral 管理客户端访问同一控制语义，Tauri 仅作为过渡配置壳，后续由本机 Web 管理面取代并最终移除。渐进路线见 [../cli-daemon-roadmap.md](../cli-daemon-roadmap.md)。

### 管理面传输边界

- `src/` 业务页面不应直接依赖 Tauri IPC；平台调用统一经管理 transport 与 dialog adapter。
- Tauri adapter 只用于迁移期兼容；Web adapter 对接本机版本化管理 HTTP API。
- 管理 HTTP handler 只做认证、参数编解码和共享控制客户端调用，不复制 daemon/CLI 业务编排。
- 桌面进程不得维护 listener、Tunnel 或 Gateway；退出桌面壳不得影响后台 daemon 运行态。

当前实现已经把 `/api/v1` Web Admin 从 bootstrap 推进为带认证的本机管理入口。`anchor admin serve` 仅绑定 loopback，并同源托管嵌入 CLI 的生产 Svelte 静态站点。浏览器使用独立进程内管理 session：session ID 是 `HttpOnly; SameSite=Strict` cookie，CSRF token 由 session bootstrap 响应返回；command 请求必须同时通过 canonical Host、精确同源 Origin、same-origin Fetch、session 与 CSRF 校验。管理会话完全独立于 MCP/Actions 公网认证。Session/health 同时发布正向 `supportedCommands`、全部 `privilegedCommands`、已评审 `privilegedExecutors` 和显式 `unavailableCommands`；Web transport 只执行正向 manifest 中存在的命令，避免 UI 与 dispatcher 版本漂移形成可点击死入口。

`management.rs` 是 Web/Tauri 共用的管理语义边界。除读取能力外，Workspace 创建/删除、目录打开、Skill inspection、Health、Canvs、FRP 非敏感 metadata CRUD 已从 desktop command 提升到共享层；Workspace 配置通过 CLI 共用的 `preview/stage/apply` 事务写入，浏览器使用 `baseProfile` 做 optimistic concurrency guard；MCP/Actions runtime 与 Tunnel start/restart/stop/test 都只通过 Workspace daemon 控制域执行。Gateway 配置热应用、关闭和 reload 同样进入共享层；per-workspace route selector 使用 Gateway control protocol 的 additive `set_routes`，运行中的 Gateway 在同一 daemon PID 内切换 route 集合并在失败时恢复旧运行态，不允许 HTTP/Tauri 直接写 Gateway runtime。HTTP handler 只负责安全校验、参数反序列化和调用共享层，不持有 daemon/Tunnel/Gateway 运行权威。

Gateway 启用时，单 Workspace MCP/Tunnel 直接控制继续 fail closed；对应运行权只能通过 Gateway route selector 变更。首 route 可启动 Gateway daemon，最后 route 移除走受控 shutdown，普通非空 route 变更使用 `set_routes` 而不是进程重启。Secret 写入/重新生成、FRP Token 写入以及 Software install/uninstall 已成为 Web privileged executor；Windows Service 生命周期仍留在不可执行的高权限边界，因此 Tauri 仍作为兼容配置壳发布。浏览器 UI 会依据 capability manifest 只开放已评审 executor，并继续将 service 控制切为只读/禁用；前端 API 架构守卫要求每个 command 要么由 Web 正向支持，要么显式属于 privileged 集合。

高权限 Web 管理采用独立安全层 `admin_security.rs`：prepare/confirm ticket 绑定当前管理 session、allowlisted action 与 action-specific 非敏感 target fingerprint，批准后生成短 TTL 的一次性 grant；executor 必须用服务端可信参数重建 target 并在业务写入前消费 grant。Secret/FRP 使用 `id/key`，Software install 使用 `kind + pinned target version`，Software uninstall 使用 `kind`；因此前端缓存旧版本时也会在执行阶段 fail closed。跨 session/action/target、版本漂移、过期和 replay 均拒绝，executor 失败也不能复用旧 grant。audit journal 只允许 `sessionFingerprint/action/phase/outcome`，不接受 command args、payload、secret、token、下载 URL 或 target 字段。未评审的 Windows Service action 仍位于 unavailable manifest，不能因为确认基础设施存在而自动获得 Web 暴露资格。

### management.rs
- **职责**: Tauri adapter、Web Admin 与后续其他管理客户端共用的配置/状态/日志读取、配置 staging/apply 与受控 daemon 生命周期语义。
- **边界**: 自身不创建 listener、RuntimeSupervisor、Tunnel Supervisor 或 Gateway runtime；运行读写委托 `control` / `gateway_control`，配置事务复用 CLI config engine，必要磁盘持久化继续使用原子 `DataStore`/Gateway config 路径。
- **安全**: secret key allowlist 在共享层统一校验；Web 传输层只在独立管理 session 校验后才能调用。Secret/FRP Token 与 Software install/uninstall 必须经过 `admin_security.rs` 的 target-bound 一次性 grant；Windows Service lifecycle 继续 fail closed。
- **迁移原则**: 新管理能力先进入共享层，再分别由 Tauri/Web adapter 暴露，禁止直接在 HTTP handler 复制桌面 command 业务逻辑。

## 核心模块

### workspace/
- **职责**: Workspace 配置的 CRUD、持久化、密钥分离存储
- **实现**: `src-tauri/src/workspace/` 与 `src-tauri/src/data/`

### runtime/
- **职责**: MCP 运行时生命周期状态机（Stopped → Starting → Running → Stopping → Error）
- **实现**: `src-tauri/src/runtime/`

### control/ 与 daemon.rs
- **职责**: CLI/GUI 共用的 Workspace 控制状态、版本化本地 IPC、协议协商、daemon 状态文件、进程生命周期、Workspace Tunnel 异步写操作、事件 journal、单服务配置 reload，以及跨 Workspace/Gateway 的纯只读聚合
- **传输**: Linux Unix Domain Socket；Windows owner/System protected-DACL Named Pipe
- **安全边界**: 本地用户隔离、显式协议版本、只读查询的受控回退；生命周期写操作禁止回退
- **协议**: Workspace 当前 v6；事件使用 `streamId + sequence` 有界游标和最长 25 秒长轮询，reload/Tunnel/apply_config 写请求使用 accepted → operation status 异步状态机
- **升级协商**: daemon state 与 `version` additive 发布 build identity。普通写请求要求当前协议；新客户端只可用旧协议执行 read-only `version` 和稳定的 lifecycle drain（Workspace v2+、Gateway v1+），用于优雅退出旧运行权威后再由当前构建启动
- **GUI 接入**: Windows/Linux 上 Workspace 状态、日志、启停、重启、Tunnel、删除、密钥应用和事件唤醒均通过共享 daemon 客户端；Windows GUI 不再提供 process-local Server 回退，检测到旧 listener 时按冲突处理而不是接管
- **配置应用**: 已运行服务使用 daemon 内单 listener reload；daemon PID、另一 listener 与 Tunnel ownership 不因普通配置应用而重启，新 listener 失败时尝试恢复旧 listener
- **聚合读取**: `control::aggregate` 并发读取独立 Workspace/Gateway 控制域，返回 canonical MCP/Actions 状态和按 source 保留游标的事件批；聚合层不持有 Runtime/Tunnel/Gateway 运行权威

### Gateway 控制域
- **职责**: 多 Workspace 共享 MCP Gateway listener、路由集合和唯一公网 Gateway tunnel
- **后台运行入口**: `anchor gateway start <workspace ...>`；`status/stop/restart/reload` 使用专用 Gateway control client；`gateway serve` 保留前台调试/外部 supervisor 模式
- **实现**: `src-tauri/src/gateway_daemon.rs` + `src-tauri/src/gateway_control/`，与 Workspace `daemon.rs` / `control/` 分离
- **协议**: Gateway protocol v1；请求包含配置域 `configScope`，使用全局 `gateway.sock`/PID/state/lock，reload 与 apply_config 使用 accepted → operation status；v1 通过 additive tags 提供有界 logs/events，不破坏已有 v1 方法
- **可观察性**: daemon log tail/cursor 单响应正文最多 8 KiB；事件 journal 保留 256 条、单批 32 条、最长 25 秒 long-poll，使用独立 `streamId + sequence` 游标
- **配置事务**: 运行中配置由 daemon 先切换运行态、更新 state，再持久化；失败停止新运行态并恢复旧运行态；禁用则先 shutdown 后持久化
- **GUI 边界**: GUI 状态和 route 列表直接来自 Gateway control status，配置写入调用 daemon；Gateway 设置页使用 events 唤醒状态/日志刷新；不创建共享 listener/tunnel，不在协议错误时本地回退
- **Workspace 交互**: route/owner profile 更新触发 Gateway reload；活动 route 禁止删除/注销，避免 live Gateway 指向不存在的 Workspace
- **跨进程一致性**: AppState 每次数据访问前从磁盘刷新；Gateway observed URL 采用窄字段原子更新，避免后台 daemon 与桌面缓存互相覆盖
- **平台边界**: Windows/Linux Gateway daemon 服务端均已实现；Windows Named Pipe 使用配置 owner 与 LocalSystem 的受保护 DACL，Gateway state v2 校验 executable/PID ownership。Windows GUI 不保留 process-local Gateway 回退

### Windows SCM supervisor
- **职责**: 配置域级开机恢复与操作系统 supervisor；desired state 保存于 `windows-service.json`
- **权限边界**: SCM supervisor 自身以 LocalSystem 运行，但不再把该令牌继承给 Workspace/Gateway daemon。安装/更新 Service 时将配置 owner SID/username 固定在管理员保护的 SCM `ImagePath`；supervisor 只信任该 registration identity，而不信任用户可写 plan 的 owner 元数据来选择 token。它选择 SID 匹配的 Active Windows 登录会话，使用该用户 primary token 与用户环境启动受管 daemon；owner 未登录或 registration 仍是旧格式时 fail closed，禁止回退为 SYSTEM child
- **运行身份**: `windows-service-runtime.json` 保存 supervisor PID、启动时间、实际 executable path 与 build identity；状态读取再以 SCM `queryex` PID、存活性和进程镜像路径交叉校验
- **升级语义**: `service install` 对已运行 Service 是显式 update：先等待旧 supervisor `STOPPED`（其间优雅排空受管 Workspace/Gateway daemon），再从已更新 binPath 启动当前构建并等待 `RUNNING`。GUI “更新服务版本”使用同一路径；普通 GUI/CLI 操作不隐式升级 Service

### mcp/
- **职责**: MCP 协议、OAuth、Session、工具目录、代理聚合与 Streamable HTTP transport
- **实现**: `src-tauri/src/mcp/` 与 `src-tauri/src/tools/`

### tunnel/
- **职责**: FRP 配置生成、Cloudflare 隧道进程监督；Workspace live supervisor 由对应 daemon 持有
- **实现**: `src-tauri/src/tunnel/`

## 入口文件

- **Tauri 入口**: `src-tauri/src/main.rs`
- **CLI 入口**: `src-tauri/src/bin/anchor.rs`
- **前端入口**: `src/routes/+page.svelte`
- **Agent 入口**: `AGENTS.md`

---
*返回索引: [../project-context.md](../project-context.md)*
