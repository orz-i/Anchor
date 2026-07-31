# Anchor Rust - 项目上下文

> 本文档是项目上下文的索引文件，提供项目概览和文档导航。

## 项目概览

| 属性 | 值 |
|------|-----|
| 项目名称 | Anchor Rust |
| 版本 | 0.1.23 |
| 语言 | Rust + TypeScript |
| 框架 | Tauri 2 + Svelte |
| 类型 | 桌面客户端 + 内嵌 MCP 运行时 |
| 描述 | 用 Rust/Tauri 重构 Anchor 桌面客户端，内嵌 MCP 核心，单二进制分发 |

## 文档导航

### [技术栈](./project-context/tech-stack.md)
项目使用的语言、框架、工具

### [架构设计](./project-context/architecture.md)
项目结构、目录说明、设计模式

### [如何开发](./project-context/how-to-develop.md)
开发新功能的基本步骤

### [如何编写测试](./project-context/how-to-test.md)
测试框架和测试编写规范

### [代码图谱洞察](./graph-insights/latest.md)
模块依赖、调用链和影响面摘要

### [设计系统](./design-system.md)
2026 开发者工具 UI 审美、色彩、字体、交互规范

## 权威实现与契约

- `src-tauri/src/mcp/` — MCP Streamable HTTP、OAuth、Session 与代理聚合
- `src-tauri/src/tools/` — 文件、Patch、Exec、Git、History 与 Skill 工具内核
- `src-tauri/tests/` — 工具结果契约、安全边界、输出 Schema 与 Harness 集成测试
- `docs/verification/` — 发布候选、协议、OAuth、远程连接与回归验证证据

## 快速开始

1. 阅读 [技术栈](./project-context/tech-stack.md) 了解项目使用的技术
2. 阅读 [架构设计](./project-context/architecture.md) 了解项目结构
3. 阅读 [代码图谱洞察](./graph-insights/latest.md) 理解模块边界
4. 查看 `docs/specs/rust-desktop-client/` 了解当前功能规格

## 开发时查看对应文档

### 新功能开发
- 使用 `begin_work_session` 或 `start_task` 建立基线
- 规格文档：`docs/specs/<feature>/`
- 使用 `stage_commit` 完成检查、分段提交和验证证据绑定

### 理解 MCP 协议行为
- `src-tauri/src/mcp/protocol.rs` — 协议版本与消息结构
- `src-tauri/src/tools/registry.rs` — 工具目录、inputSchema 与 outputSchema
- `src-tauri/tests/call_tool_contract.rs` — 工具行为契约

### 编写测试
- [how-to-test.md](./project-context/how-to-test.md)
- `src-tauri/tests/` — 当前回归基线

---
*生成时间: 2026-07-10*
*生成工具: MCP Probe Kit - init_project_context*
