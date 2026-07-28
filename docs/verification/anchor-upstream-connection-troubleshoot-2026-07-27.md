# Anchor MCP 上游连接失败排查（2026-07-27）

## 结论

当前无法复现 OAuth 口令、callback、token exchange 或 MCP 初始化本身的失败。

最可能导致 ChatGPT 在输入授权口令后显示“连接到 Anchor MCP 时出现问题。请稍后再试。”的原因，是 **Cloudflare Quick Tunnel 地址在 ChatGPT 保存连接后发生轮换**：ChatGPT 应用保存的是创建时的固定 MCP endpoint，而 Quick Tunnel 每次重新创建都可能生成新的 `trycloudflare.com` 地址。旧地址不会自动迁移到新地址。

本次检查同时发现并修复了三个连接后可能被上游折叠为通用连接错误或假卡顿的服务端缺陷，但它们不太可能解释“口令提交后立即失败”的工具扫描阶段：

1. `search_text` 对含中文的长行按字节直接切片，可能在 UTF-8 字符中间 panic。
2. `exec_command` 的程序不存在、启动失败和阻塞超时结果缺少部分必填字段，会被覆盖为 `TOOL_OUTPUT_SCHEMA_VIOLATION`，隐藏原始错误。
3. 短命令恰好在 `yield_time` 边界退出时，可能被误报为 `running/command_ok=null`。

## 关键证据

### 1. Quick Tunnel 地址已经轮换

`cloudflared.log` 记录了两个不同的 Quick Tunnel 地址：

- 2026-07-27 07:33 UTC：`https://qualification-dana-attached-coordinator.trycloudflare.com`
- 2026-07-27 09:27 UTC：`https://takes-solaris-gale-silk.trycloudflare.com`

实时验证结果：

- 旧地址 `/mcp`：HTTP 530
- 当前地址 `/mcp`：HTTP 405，`Allow: POST, DELETE`

因此，若 ChatGPT 应用仍绑定旧地址，授权页面此前可以正常打开并不代表授权后的 token 或工具扫描仍能访问 MCP。地址在授权流程前后轮换时，上游只会看到远端连接失败。

### 2. 当前地址的完整 OAuth 授权后流程通过

使用 Anchor 保存的凭据在内存中执行完整 PKCE 验证，未输出或写入秘密。以下步骤全部通过：

- 未认证 MCP challenge：HTTP 401，包含 `scope="mcp"` 和 `resource_metadata`
- OAuth authorization page：HTTP 200
- 授权口令提交：HTTP 303，callback host 为 `chatgpt.com`
- authorization code 换 token：HTTP 200，返回 Bearer access token 和 refresh token
- MCP `initialize`：协议 `2025-11-25`，成功创建 session
- `tools/list`：23 个 core 工具，catalog digest 与 `server_info` 一致
- `read_file`：成功
- refresh token 轮换：成功
- 旧 refresh token 重用：HTTP 400 `invalid_grant`
- session DELETE：HTTP 204

机器可读结果：

- `docs/verification/anchor-upstream-oauth-post-passcode-2026-07-27.json`

### 3. 当前公网入口与 OAuth 元数据正常

`anchor workspace test Anchor --service mcp --endpoint public` 全部通过：

- Streamable HTTP GET 契约：HTTP 405
- Authorization Server Metadata：HTTP 200
- Protected Resource Metadata：HTTP 200
- 未认证 initialize challenge：HTTP 401

### 4. 日志中的既有授权请求均完成

现有 `mcp-oauth.log` 中两次 authorization-code 流程均为：

- authorize GET 200
- authorize POST 303
- token exchange 200

对应流程后均出现成功的 `initialize`，其中一次随后成功执行 `tools/list` 和 `resources/list`。未发现口令错误、redirect URI 拒绝、PKCE 失败或 token endpoint 5xx。

## 已修复的次要服务端缺陷

### `search_text` UTF-8 截断 panic

可复现请求在 `max_preview_bytes` 落入中文 UTF-8 字符中间时触发 worker panic：

```text
Local MCP tool worker failed
end byte index ... is not a char boundary
```

修复：`src-tauri/src/tools/file.rs` 的预览截断现在复用 UTF-8 安全的 `truncate_bytes`，并增加中文多字节边界回归测试。

### Tool execution error 被 outputSchema 覆盖

直接执行某些 workspace 本地可执行文件时，可复现：

```text
TOOL_OUTPUT_SCHEMA_VIOLATION: "suggestion" is a required property
```

根因位于 `execution_failure_result`：它将执行层失败转换为统一的 `ok=true / command_ok=false` 结果，但程序不存在、启动失败和阻塞超时路径没有补齐 `suggestion`、`duration_ms`、`elapsed_ms`、`warnings` 等声明为必填的字段。后续 outputSchema 校验因此用内部契约错误替换了原始执行错误。

修复：`src-tauri/src/tools/exec.rs` 现在为全部执行失败路径补齐统一字段，保留原始 `error.code`，并用 outputSchema 验证程序不存在和启动失败的单元测试；阻塞超时另有真实集成测试。

### 短命令退出边界竞态

全量测试复现了命令已经输出并即将退出，但执行器在最后一次状态刷新后、返回保留 session 前发生进程退出的竞态。结果会显示 `status=running`、`command_ok=null`，让上游误以为命令仍在执行。

修复：非交互命令在非零 `yield_time` 边界增加最多 50 毫秒的完成确认窗口；窗口内检测到退出时直接返回完整结果。显式 `yield_time_ms=0` 和 TTY 会话保持立即返回语义。

## 修复验证

- `cargo test --manifest-path src-tauri/Cargo.toml tools::exec::tests --lib`：8 passed。
- `cargo test --manifest-path src-tauri/Cargo.toml --test call_tool_contract`：21 passed。
- `cargo test --manifest-path src-tauri/Cargo.toml --lib`：238 passed，1 ignored。
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features cli --bin anchor`：通过。

## 建议

### 稳定连接

长期保存到 ChatGPT 的 MCP 应用应使用固定域名：

- Cloudflare Named Tunnel，或
- 固定 FRP 地址。

Quick Tunnel 适合临时验证，不适合需要跨服务重启持续可用的 ChatGPT 应用连接。

### 使用 Quick Tunnel 时

每次 Anchor、MCP listener 或 cloudflared 重启后：

1. 运行 `anchor workspace gpt-config Anchor --service mcp --endpoint public`。
2. 对比 `connectorUrl` 与 ChatGPT 中保存的 MCP endpoint。
3. 地址变化时，更新或重新创建 ChatGPT 应用，并重新授权。
4. 不要继续使用旧 `trycloudflare.com` 地址。

### 按日志定位授权后故障点

- 没有 `authorize_submitted`：授权页或口令提交未到达 Anchor。
- 有 303、没有 `token_exchange_received`：callback 或上游 token 请求未发生，优先检查 endpoint 是否已轮换。
- token 200、没有 `initialize`：授权完成后 MCP endpoint 不可达或上游中止扫描。
- `initialize` 成功、没有 `tools/list`：上游工具扫描中止或缓存状态异常。
- `tools/list` 成功后报错：检查具体工具调用、worker panic 和 outputSchema 日志。

## 未执行事项

- 未操作 ChatGPT 网页界面，无法读取其保存的实际 endpoint 或内部错误详情。
- 未重启当前 Anchor GUI、MCP listener 或 Cloudflare Tunnel，避免影响正在使用的连接。
- 保留了上一会话的三处未提交改动，未覆盖其内容；本轮另修改工具实现和测试文件。
