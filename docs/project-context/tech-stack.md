# 技术栈

> 本文档描述 Anchor Rust 的技术栈信息。

## 基本信息

| 属性 | 值 |
|------|-----|
| 项目名称 | Anchor Rust |
| 版本 | 0.1.23 |
| 语言 | Rust / TypeScript |
| 框架 | Rust + Vite/React Web Admin |

## 核心技术

| 类别 | 技术 | 用途 |
|------|------|------|
| 后端语言 | Rust | CLI/daemon、MCP、管理 API、进程管理、状态机 |
| 前端 | React + TypeScript + React Router | 浏览器 Web Admin |
| 异步运行时 | tokio | 异步 I/O、进程监督 |
| HTTP 服务 | axum | MCP Streamable HTTP 与本机 Web Admin API |
| Git 操作 | 系统 Git + 受控临时索引 | 状态、差异、提交与阶段工作流 |
| 密钥存储 | 平台保护封装 | Windows 当前用户 DPAPI；Unix 私有权限文件 |
| 序列化 | serde + serde_json | 配置持久化 |

## 开发工具

| 类别 | 工具 | 用途 |
|------|------|------|
| 包管理 | cargo / pnpm | Rust / 前端依赖 |
| 构建 | cargo / Vite | CLI 与嵌入式 Web Admin 构建 |
| 测试 | cargo test | Rust 单元/集成测试 |
| 代码检查 | clippy / rustfmt | Rust lint 与格式化 |
| 前端检查 | eslint / prettier | TypeScript 检查 |

## 主要依赖

### Rust (crates/anchor/Cargo.toml)

- `tokio` — 异步运行时
- `axum` — HTTP server
- `reqwest` — 下游 HTTP 与下载客户端
- `jsonschema` — Schema 校验
- `serde` / `serde_json` — 序列化

### 前端 (package.json)

- `react` / `react-dom` — UI 框架
- `react-router-dom` — 浏览器路由
- `shadcn` / `@base-ui/react` — UI 组件基础
- `vite` — 前端构建
- `tailwindcss` — 样式

---
*返回索引: [../project-context.md](../project-context.md)*
