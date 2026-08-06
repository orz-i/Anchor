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
│  Workspace 卡片 / 配置 / 日志 / 健康检查  │
├─────────────────────────────────────────┤
│  GUI 配置壳 / CLI                        │
│  配置、展示、脚本化运维                   │
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
| 进程管理 | Workspace daemon 为主；RuntimeSupervisor 仅保留 Gateway/Tunnel 与旧会话兼容路径 |
| UI | Tauri 2 + SvelteKit 设计系统 |
| 密钥 | 受保护凭据封装；Windows 使用当前用户 DPAPI |
| 分发 | 桌面安装包与独立 `anchor` CLI |

目标方向是让 daemon 成为运行时、Gateway 和 Tunnel 的唯一权威，CLI 提供完整运维能力，GUI 逐步收缩为配置与状态壳。渐进路线见 [../cli-daemon-roadmap.md](../cli-daemon-roadmap.md)。

## 核心模块

### workspace/
- **职责**: Workspace 配置的 CRUD、持久化、密钥分离存储
- **实现**: `src-tauri/src/workspace/` 与 `src-tauri/src/data/`

### runtime/
- **职责**: MCP 运行时生命周期状态机（Stopped → Starting → Running → Stopping → Error）
- **实现**: `src-tauri/src/runtime/`

### control/ 与 daemon.rs
- **职责**: CLI/GUI 共用的控制状态、版本化本地 IPC、协议协商、daemon 状态文件与进程生命周期
- **传输**: Unix Domain Socket；Windows Named Pipe 抽象
- **安全边界**: 本地用户隔离、显式协议版本、只读查询的受控回退；生命周期写操作禁止回退
- **GUI 接入**: Workspace 状态、日志、启停、重启、删除和密钥应用均通过共享 daemon 客户端；Gateway/Tunnel 尚处兼容迁移阶段

### mcp/
- **职责**: MCP 协议、OAuth、Session、工具目录、代理聚合与 Streamable HTTP transport
- **实现**: `src-tauri/src/mcp/` 与 `src-tauri/src/tools/`

### tunnel/
- **职责**: FRP 配置生成、Cloudflare 隧道进程监督
- **实现**: `src-tauri/src/tunnel/`

## 入口文件

- **Tauri 入口**: `src-tauri/src/main.rs`
- **CLI 入口**: `src-tauri/src/bin/anchor.rs`
- **前端入口**: `src/routes/+page.svelte`
- **Agent 入口**: `AGENTS.md`

---
*返回索引: [../project-context.md](../project-context.md)*
