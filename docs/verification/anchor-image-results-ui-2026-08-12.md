# Anchor Image Results UI verification — 2026-08-12

## Goal

为 workspace `view_image` 和下游 Browser `take_screenshot` 增加共享的 MCP Apps Results UI，使图片可以在支持 UI 的 ChatGPT/MCP host 中直接查看，同时保持原有标准 MCP image content、模型视觉输入和无 UI host fallback。

## Design

- UI resource: `ui://anchor/image-viewer/v1.html`
- MIME: `text/html;profile=mcp-app`
- `view_image` 与受管 my-agent-browser `take_screenshot` tool descriptor 均发布：
  - `_meta.ui.resourceUri`
  - `_meta.ui.visibility = ["model", "app"]`
  - `_meta["openai/outputTemplate"]` compatibility alias
  - `_meta["openai/widgetAccessible"] = true`
- 不新增公开 MCP tool，Catalog tool count 不变。
- UI 直接消费 tool result 中的 `content[type=image]`，将 base64 解码成 Blob/Object URL；不会把图片 payload 再复制到 `structuredContent`。
- Browser screenshot 使用 `filePath` 时，下游不会 attach 图片。Anchor 已有 workspace artifact bridge；UI 从 `workspace_artifacts` 取得安全的 workspace 相对路径，再通过标准 `tools/call` 调用现有 `view_image` 获取预览。
- resources capability 与 Skills extension 解耦：UI resource 始终可读；Skills extension 仍只在 Skill service enabled 时声明。
- UI 没有外部脚本、样式、图片或网络依赖，resource CSP 的 connect/resource domains 为空。
- UI resource 从当前 workspace 的 HTTPS MCP public base URL 提取 origin，并同时发布：
  - `_meta.ui.domain`
  - `_meta["openai/widgetDomain"]` compatibility alias
  例如 `https://anchor.taoyan.icu/mcp` 会发布 `https://anchor.taoyan.icu`。这是 ChatGPT 提交带 UI plugin 时的必需字段，且 origin 必须对每个 plugin 唯一。
- 如果使用 Gateway 且多个独立 ChatGPT plugins 共享同一个公网 hostname，仅靠 `/w/<workspace-id>` 路径不能满足“unique origin per plugin”；提交带 UI 的多个 plugin 时应为每个 plugin 使用独立 HTTPS hostname/subdomain。

## Compatibility

- 标准 MCP Apps bridge：监听 `ui/notifications/tool-result`，需要内部工具读取时发送 `tools/call` JSON-RPC。
- ChatGPT compatibility：可从 `window.openai.toolResponseMetadata` 读取完整 MCP result envelope；fullscreen 使用 `window.openai.requestDisplayMode`。
- 不支持 UI 的 host 继续收到原有 MCP image content 和文本/结构化元数据。

## Verification targets

- UI resource 在 Skill service disabled 时仍能通过 `resources/list` / `resources/read` 发现和读取。
- `resources/read` 会把当前 HTTPS MCP public origin 发布为标准 widget domain 和 ChatGPT compatibility alias；HTTP/local-only URL 不伪造 submission domain。
- Skills disabled 时不发布 native Skills extension。
- 最终 effective `tools/list` 保留 `view_image` 的 UI metadata。
- Browser automatic exposure 的 `take_screenshot` 保留 UI metadata。
- 既有 image result normalization 继续保证二进制 payload 只出现一次。
- Catalog tool 数量和 committed effective catalog snapshot 不发生非预期变化。
- Rust check / Clippy / full tests、前端 check/build、targeted rustfmt 与 `git diff --check` 全部通过后提交。

## Live boundary

当前 ChatGPT 会话连接的是已安装运行实例。真正观察到 inline Results UI 需要把本提交构建并更新到该实例后刷新/重新连接 MCP app；在更新安装实例之前，本任务用真实 MCP request handler、effective tools/list、resources/list/read 与 proxy catalog 契约测试验证协议输出。
