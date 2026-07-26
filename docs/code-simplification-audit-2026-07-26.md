# 全量代码审计与精简记录（2026-07-26）

## 审计范围

- Rust 产品代码：`src-tauri/src/**/*.rs`
- Rust 集成测试：`src-tauri/tests/**/*.rs`
- Svelte/TypeScript：`src/**/*.svelte`、`src/**/*.ts`
- 发布验证脚本：`scripts/**/*.py`
- 构建、依赖与 feature 组合：`Cargo.toml`、`package.json`

审计覆盖 108 个 Rust 源文件和 49 个前端源文件。检查内容包括：文件与函数体量、模块树可达性、静态 import 可达性、无引用导出、重复实现、`dead_code` 例外、跨 feature/平台代码以及公开协议契约。

## 本轮已实施

### 删除不可达代码

- 删除未接入模块树的 `settings/store.rs` 和 `workspace/store.rs`。
- 删除无任何静态或动态引用的 `Badge.svelte`、`TunnelStrip.svelte`。
- 删除只被上述废弃组件使用的 `getFrpSnippet`、`startTunnel` 前端包装。
- 删除无引用的 `isPortConflictError`、`runtimeStates` 兼容别名和 `overallRuntimeState`。

### 合并重复实现

- Actions 与 MCP/OAuth 共用 Bearer Header 解析和常量时间比较。
- MCP Session 的取消、默认工作目录清理统一进入关联清理函数。
- Workspace 页面 MCP/Actions 启停流程合并为一个类型化流程。
- MCP 与 Actions 共用 OAuth 请求体上限常量。

### 删除旧兼容包装

- 删除 `mcp_frp_snippet`、`actions_frp_snippet`、`build_frpc_toml` 包装，统一使用当前生产 API。
- 删除无调用者的 `frp_version` 和 `stop_frpc`。
- 删除授权码状态中从未读取的 `state` 副本；回调 state 仍由授权响应直接返回，协议行为不变。

## 精简结果

实施代码在加入本报告前的差异：

- 19 个文件发生代码变化。
- 96 行新增。
- 303 行删除。
- 净减少 207 行。
- Rust 源文件：108 → 105。
- 前端源文件：49 → 47。
- 普通前端不可达文件和无引用导出清零；SvelteKit 的 `ssr` 约定导出不属于死代码。

## 未机械拆分的大文件

以下文件仍然较大，但本轮不按行数强拆：

| 文件 | 主要原因 | 后续建议 |
|---|---|---|
| `tools/registry.rs` | Tool 输入/输出 Schema 的集中式协议定义 | 后续生成式 Schema builder，保持 effective catalog digest 可审计 |
| `cli/mod.rs` | 多级 CLI 路由与 daemon 生命周期 | 按 gateway/workspace/logs 命令域拆分 |
| `mcp/proxy.rs` | 子进程协议、Catalog 冻结、重连与测试夹具耦合 | 分离 transport、catalog、worker 状态机 |
| `mcp/gateway.rs` | 反向代理、安全边界与大型集成测试同文件 | 将测试夹具移入独立 integration test |
| `mcp/listener.rs` | HTTP 生命周期、OAuth 与 JSON-RPC 路由 | 分离 OAuth routes 和 session request pipeline |
| `auth/oauth_flow.rs` | 授权页、code/token/refresh 流程与测试同文件 | 分离 redirect policy、token service、HTTP handlers |
| `runtime/supervisor.rs` | 服务启动、恢复和资源冲突状态机 | 将启动计划与执行副作用分离 |
| `workspace/[id]/+page.svelte` | 页面状态、轮询、配置保存和视图集中 | 提取 runtime controller 与 tunnel form controller |
| `tools/history/mod.rs` | bootstrap 内容窗口和 checkpoint 状态机 | 将 bootstrap response builder 提取为纯数据模块 |

这些重构涉及状态机或公开协议，不应与死代码清理混在同一提交中。

## 保留项说明

- `mcp/gateway.rs`、`mcp/protocol.rs`、`mcp/proxy.rs`、`tools/schema.rs` 被 `pub(crate) mod` 或显式内部模块接入，并非孤立文件。
- 平台和 feature 条件下的部分 `dead_code` 例外用于 Windows/macOS/Linux 或 desktop/CLI 差异，本轮未凭当前宿主平台删除。
- `validate_tool_arguments` 虽不被产品路径直接调用，但由安全集成测试直接使用，应保留。
- Tool Schema 数量和名称未改变，本轮不应导致 effective catalog digest 变化。

## 自动化门禁

本轮要求通过：

- 全 feature Rust tests。
- 全 target/feature Clippy `-D warnings`。
- Headless CLI Clippy。
- Svelte check 与生产构建。
- effective catalog snapshot。
- 远程 MCP Streamable HTTP 发布候选验证。
- OAuth discovery、授权页、PKCE、token、refresh rotation 与旧 refresh replay 拒绝。
- ChatGPT 内置 callback 自动信任与回调兼容验证。

ChatGPT Workspace 网页界面不属于本轮自动化范围，不执行任何网页端用户操作。
