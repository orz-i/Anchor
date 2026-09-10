# Anchor 文档中心

本目录是 Anchor 当前产品文档的统一入口。**当前行为以仓库最新源码为事实源**；`docs/specs/`、`docs/verification/`、roadmap 和历史 audit 只用于设计追溯与验证记录，不应优先于当前代码和本手册判断产品行为。

当前文档基线：

- Anchor package：`0.1.23`
- MCP Catalog：`v48`
- Runtime capability contract：`anchor-runtime-capabilities-v1`
- Federation read contract：`anchor-federation-v2`
- Orchestration contract：`anchor-orchestration-v1`
- 管理面：浏览器 Web Admin（Vite + React）；Tauri desktop 已退役

## 从哪里开始

| 目标 | 推荐入口 |
| --- | --- |
| 第一次安装并连接 ChatGPT | [项目 README](../README.md) |
| 注册、查看、启动 Workspace | [Workspace CLI](workspace-cli.md) |
| Linux/headless 部署 | [Linux CLI](linux-cli.md) |
| daemon、upgrade、service 与故障恢复 | [CLI Daemon 与运维](cli-daemon.md) |
| 多 Workspace 共用一个 MCP Gateway | [单一 MCP Gateway](mcp-gateway.md) |
| Windows/Linux 之间迁移配置 | [跨平台配置迁移](config-migration.md) |
| 管理 Agent Skill package | [Agent Skill package lifecycle](skill-service.md) |
| 连接其他 MCP server | [Dynamic MCP](dynamic-mcp.md) |
| 多 Anchor Node 只读互联 | [Federation](federation.md) |
| 规划/观测多 Node、Workspace、Harness Task | [Orchestration](orchestration.md) |
| 连接恢复、自动重试、OAuth | [可靠性与恢复](reliability.md) |
| Harness Task 使用 Git worktree | [Git worktree](git-worktrees.md) |

## 当前 MCP 工具面

Anchor 的工具目录采用 **facade-first** 模型。多个相关 operation 聚合在一个一级工具下，内部 operation handler 不作为独立公开工具使用。

当前主要 facade：

| facade | 主要用途 | operation 摘要 |
| --- | --- | --- |
| `session` | 持久开发 Session | `open`、`checkpoint`、`list`、`get`、`validate` |
| `cwd` | Session 默认工作目录 | `get`、`set` |
| `environment` | 执行环境与成本策略 | `check`、`health`、`cost` |
| `git` | Git 读取、提交与 worktree 生命周期 | `status`、`diff`、`log`、`show`、`blame`、`commit`、`worktree_*` 等 |
| `skill` | Agent Skill package | `list`、`get`、`read_resource`、`packages`、`validate`；advanced 另有安装/激活生命周期 |
| `mcp` | 下游 MCP runtime | `list`、`get`、`search_tools`、`get_tool`；core 可 `call`；advanced 可动态管理 server |
| `task` | Harness Task | 状态、Recovery、gate、生命周期、change summary 等 |
| `slice` | Harness Slice | `start`、`update`、`complete` |
| `commit_stage` | 受控 staged commit | `run`、`status`、`wait` |

`server_info`、文件、Patch、command 等基础能力仍按各自公开工具提供。不要从历史文档中的内部 handler 名称推断可直接调用的工具；例如当前 Git 的推荐入口是 `git` facade，而不是依赖旧的 `git_status` / `git_diff` 名称。

## Session 与 Harness

当前 Session store 默认位于：

```text
docs/session/
```

推荐的新会话初始化流程：

```text
session { operation: "open" }
server_info
cwd { operation: "get" }
git { operation: "status" }
environment { operation: "check" }
```

旧目录 `docs/history-session/` 是冻结归档。当前 Session API 不扫描、不迁移、也不向该目录写入。

需要长期、可恢复的工程任务时，再使用 `begin_work_session` / `task` / `slice` / `complete_work_session` 等 Harness 能力。普通代码阅读和简单修改不要求先创建 Harness Task。

## 用户、管理员与开发者文档的边界

### 用户手册

以下文档以“如何使用产品”为主：

- [Workspace CLI](workspace-cli.md)
- [Dynamic MCP](dynamic-mcp.md)
- [Agent Skill package lifecycle](skill-service.md)
- [Git worktree](git-worktrees.md)

### 管理员手册

以下文档涉及部署、运行态与安全边界：

- [Linux CLI](linux-cli.md)
- [CLI Daemon 与运维](cli-daemon.md)
- [单一 MCP Gateway](mcp-gateway.md)
- [跨平台配置迁移](config-migration.md)
- [Federation](federation.md)
- [可靠性与恢复](reliability.md)

### 高级/集成能力

- [Orchestration](orchestration.md)
- [Federation](federation.md)
- [项目架构](project-context/architecture.md)

Federation 与 Orchestration 当前已有 Rust management contract 和 TypeScript API contract，但**尚无专用可视化管理页面，也没有对应一级 MCP tool 或 CLI command group**。相关手册会明确区分“已经实现的 backend/API 能力”和“当前可视化产品入口”，避免把内部 contract 写成不存在的 CLI 功能。

### 开发与架构

- [项目上下文](project-context.md)
- [架构](project-context/architecture.md)
- [开发](project-context/how-to-develop.md)
- [测试](project-context/how-to-test.md)
- [技术栈](project-context/tech-stack.md)
- [Design Guidelines](design-guidelines/README.md)

## 工程档案

这些目录用于追溯，不是“当前使用手册”：

- [`docs/specs/`](specs/README.md)：历史需求、设计与任务拆解；目录内出现的旧产品名、旧协议和旧工具只代表当时设计上下文
- [`docs/verification/`](verification/README.md)：阶段性验证记录；日期化验证结果不能覆盖当前源码和正式手册
- `docs/*roadmap*.md`：历史阶段规划与收口记录
- `docs/*audit*.md`：特定日期的代码/设计审计

当档案与当前手册不一致时，应先核对当前源码，再更新正式手册，而不是恢复旧兼容行为。

## 文档维护约定

1. CLI 示例必须能在当前 `anchor` 参数解析器中成立。
2. MCP 示例使用当前 facade + `operation` 形式，不使用隐藏的 operation handler。
3. Web Admin 能力区分“已有可视化页面”和“仅 backend/typed API contract”。
4. 不把 `specs` 中计划过但未进入当前代码的功能写成已实现。
5. 安全边界优先描述 fail-closed 行为，避免用“通常”“应该”掩盖权限限制。
6. 新增或重命名公开 facade、CLI command、wire contract 时，应同步更新本页和对应专题文档。
