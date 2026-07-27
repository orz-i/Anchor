# Anchor 全量代码审计与低风险精简（2026-07-27）

## 1. 结论

本轮审计覆盖当前仓库的 108 个 Rust 源文件与 47 个前端 TypeScript/Svelte 文件，并复核配置、日志、CLI、MCP/OAuth、隧道、工具策略、前端 API 和设计系统边界。

结论为：当前主干没有发现可直接删除的大块不可达业务模块，也没有无引用 Svelte 组件；MCP 2025-11-25 Tool catalog、OAuth、Session 和隔离边界保持稳定。本轮只实施可证明行为等价或能够补齐既有行为漂移的低风险精简，没有拆分协议状态机、运行时 supervisor 或 workspace 大页面。

## 2. 审计方法

- 建立 Rust、TypeScript、Svelte 文件数量与大文件分布基线。
- 搜索 TODO/FIXME/HACK、生产代码中的 `unwrap`/`expect`、子进程创建、网络客户端、重复日志目录和无引用前端导出。
- 复核 MCP listener、Gateway、OAuth、工具 Schema、Session、Skill、CLI daemon、隧道与数据持久化边界。
- 使用 Product Design 审计规范检查设计系统一致性、交互语义与可访问性代码。
- 执行全量 Rust 测试、双 feature Clippy、Svelte 检查和生产构建。
- 使用独立配置、独立 workspace、独立端口和临时 Cloudflare Quick Tunnel 运行真实 noauth 与 OAuth 公网测试。

限制：当前 coding 会话没有浏览器渲染或截图工具，因此没有冒充完成像素级、响应式断点或真实键盘导航的视觉验收；Product Design 部分仅完成源码和构建级审计。

## 3. 已实施的精简

### 3.1 统一日志目录来源

GUI 与 CLI 原来分别维护 MCP、Actions 和隧道日志文件清单，已经产生行为漂移：GUI 能查看 `mcp-oauth.log`、`mcp-requests.log` 和 `actions-oauth.log`，CLI 却遗漏这些诊断日志。

本轮将 profile 日志清单集中到 `src-tauri/src/logging.rs`，GUI 与 CLI 共用同一目录定义，并增加契约测试。结果是删除两套重复分支，同时修复 CLI 日志可见性。

### 3.2 删除前端内部兼容别名

移除仅在仓库内部使用、且已经有明确新名称的弃用别名：

- `SecretKey`
- `getSecret`
- `setSecret`
- `regenerateSecret`

调用方统一改用 `WorkspaceSecretKey`、`getWorkspaceSecret`、`setWorkspaceSecret` 和 `regenerateWorkspaceSecret`。

同时将只在模块内部使用的 `errorText`、`isTransientInvokeError` 和 `serviceErrorMessage` 收回为私有函数，缩小前端公共 API 面。

### 3.3 缓存命令路径检测正则

`command_contains_external_path` 原来每次工具策略检查都重新编译两条正则。本轮改为 `OnceLock<Regex>`，保持判定语义不变，减少热路径重复构造。

## 4. 审计发现但未在本轮重构的事项

### P1：运行时隔离仍不是 OS 沙箱

`exec_command` 继续属于应用策略边界，Windows/Linux 尚未提供完整的低权限 token、AppContainer、容器或文件系统 sandbox。现有 canonical path、reparse point、命令白名单和危险模式是重要缓解，但不应宣称为操作系统级隔离。

### P2：设计系统文档与实际实现漂移

`docs/design-system.json` 描述暗色默认、OKLCH token、Plus Jakarta Sans/JetBrains Mono 和 refined-utilitarian 方向；当前 `src/app.css` 默认浅色、使用十六进制颜色与系统字体，并保留 “neo-tianxian inspired” 注释。产品本身能够正常构建，但文档不再是可靠的视觉事实来源。

应在后续独立设计任务中选择一个方向：以当前 UI 反向更新设计系统文档，或按已批准设计系统逐步迁移 token。不要在缺少截图基线时一次性改色、字体和布局。

### P2：大状态机和页面仍需分主题拆分

以下文件继续超过约 1,000 行，属于维护风险而不是本轮可安全删除的死代码：

- `src-tauri/src/tools/registry.rs`
- `src-tauri/src/cli/mod.rs`
- `src-tauri/src/mcp/proxy.rs`
- `src-tauri/src/mcp/gateway.rs`
- `src-tauri/src/mcp/listener.rs`
- `src-tauri/src/auth/oauth_flow.rs`
- `src-tauri/src/cli/workspace.rs`
- `src/routes/workspace/[id]/+page.svelte`

建议后续按“纯计划/状态转换”与“副作用执行”分离，每个主题独立提交并保留现有契约快照；不建议一次性重写。

### P2：交互语义可继续加强

通用 `Tabs.svelte` 已使用 `tablist/tab/aria-selected`。workspace 页面部分状态筛选按钮仍主要依赖视觉选中样式，可在有真实浏览器验收时补 `aria-pressed` 或统一为 Tabs 语义，并验证键盘焦点、窄屏布局与高对比度模式。

### P3：依赖层观察项

`cargo tree -d` 中的多数重复版本来自 Tauri、WebView 和平台依赖树，当前没有安全的直接删除方案。`serde_yaml 0.9.34+deprecated` 仍是维护观察项，但本轮没有为“减少依赖”替换稳定解析器。

## 5. 自动化验证

最终代码状态通过：

- `cargo test --manifest-path src-tauri/Cargo.toml --all-features -- --test-threads=1`
  - 332 passed，1 ignored，0 failed。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings`
  - 通过，0 warning。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --features cli --bins -- -D warnings`
  - 通过，0 warning。
- `pnpm check`
  - 0 errors，0 warnings。
- `pnpm build`
  - SvelteKit adapter-static 生产构建通过。

## 6. 真实公网验证

测试使用 `.tmp-audit-20260727` 下的独立配置与 workspace、本地端口 `10180`、临时 Quick Tunnel；没有读取或修改现有 GUI profile，也没有停止现有 MCP、Actions、FRP 或其他 Cloudflare 实例。

### noauth

报告：`docs/verification/code-simplification-v2-remote-noauth-2026-07-27.json`

全部通过：

- GET `/mcp` 返回 405，Allow 包含 POST/DELETE。
- 错误 Origin 返回 403。
- initialize 协商 MCP `2025-11-25`，server identity 为 Anchor。
- core catalog 为 26 个 Tool。
- catalog digest 为 `4dd2cf70826b6ac00b1e84950da64d2c54195eb3ec3e67c0fff8e4a16965f200`。
- `read_file` 成功。
- 隔离写入与回滚成功。
- Session DELETE 返回 204。

### OAuth

报告：`docs/verification/code-simplification-v2-remote-oauth-2026-07-27.json`

除上述 MCP 契约外，全部通过：

- RFC 9728 protected-resource metadata。
- Authorization Server metadata。
- 未认证 401 challenge。
- 授权页与 `chatgpt.com/connector/oauth/<id>` 内置 callback 自动登记。
- S256 PKCE 授权码交换。
- Bearer MCP initialize 与 Tool 调用。
- Refresh Token 轮换。
- 轮换后的 Access Token 可用。
- 旧 Refresh Token 重放返回 `invalid_grant`。

### 日志真实验证

最终 noauth/OAuth 运行累计生成 260 行非空日志：

- `cloudflared.log`：172/172 带 UTC 毫秒时间戳。
- `mcp-oauth.log`：20/20 带 UTC 毫秒时间戳。
- `mcp-requests.log`：64/64 带 UTC 毫秒时间戳。
- `stdout.log`：4/4 带 UTC 毫秒时间戳。

CLI `logs --service mcp` 实际返回了 `mcp-cloudflare`、`mcp-oauth`、`mcp-requests` 和 `mcp-stdout`，确认共享目录修复已生效。

## 7. 清理与外部门禁

- 测试端口 `10180` 已释放。
- 不存在命令行目标为 `127.0.0.1:10180` 的 cloudflared。
- 临时 Quick Tunnel 地址已经停止，不应配置到 ChatGPT App。
- ChatGPT Developer Mode 的 Tool scan、人工 OAuth、真实聊天读写确认和冻结 catalog Refresh 仍需用户界面人工验收；本轮没有操作 ChatGPT Workspace 网页。
