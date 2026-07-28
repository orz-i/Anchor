# MCP 工具聚合与 ChatGPT 安装兼容修复验证

- 日期：2026-07-28
- 工作区：`D:\anchor`
- ChatGPT 应用 ID：`asdk_app_6a67f79bee588191b6c55e8ea9e276a9`
- 验证方式：源码、单元测试、契约测试、CLI 编译与前端生产构建
- 运行中服务：未重启、未替换，避免影响当前 GUI、MCP、OAuth 与隧道连接

## 修复范围

### 下游 MCP 工具选择

每个 `mcpServers.<name>` 支持：

- `includeTools`：仅发布指定的下游原始工具名。
- `excludeTools`：从候选工具中排除指定名称；与 `includeTools` 同时使用时，排除优先。
- `maxTools`：在工具名稳定排序后限制发布数量，保证结果可重复。

工具名均使用下游 MCP 在 `tools/list` 中返回的原始名称，不包含 Anchor 添加的 `服务器名__` 前缀。重复、空白、过长名称以及大于 256 的 `maxTools` 会在配置加载时返回明确错误。`includeTools` 中不存在于下游目录的名称也会明确报错，避免因下游版本变化而静默发布错误的工具集合。

示例：

```json
{
  "mcpServers": {
    "browser": {
      "command": "browser-mcp",
      "includeTools": ["navigate", "click", "screenshot", "get_text"],
      "excludeTools": ["evaluate"],
      "maxTools": 12
    }
  }
}
```

## ChatGPT 兼容目录预算

Anchor 在完整本地工具与过滤后的下游工具合并后执行统一预算校验：

| 指标 | 兼容保护值 |
| --- | ---: |
| 总工具数 | 128 |
| 序列化目录大小 | 512 KiB |
| 估算 token | 96 Ki token |
| token 估算 | `ceil(catalog_bytes / 4)` |
| 单工具定义硬上限 | 128 KiB |

这些数值是 Anchor 的保守兼容保护值，不被表述为 ChatGPT 的公开协议上限。保护目标是在上游安装或刷新前阻断明显过大的目录，并提供可操作诊断，而不是把超大目录继续交给上游后显示通用安装失败。

## 代理 Tool 输出架构兼容

基于当前最新工作树核对发现，本地 Anchor Tool 均发布 `outputSchema`，但代理 Tool 此前只在下游 MCP 原本声明 `outputSchema` 时透传。下游只提供文本结果或未声明输出架构时，聚合目录会出现没有输出架构的 Tool，这与 ChatGPT Action 控制中显示的“建议添加输出架构”一致。

当前修复将代理 Tool 规范化为始终发布输出架构：

- 下游已经声明 `outputSchema` 时，继续保留并严格校验其 `structuredContent`。
- 下游未声明时，Anchor 发布稳定的 fallback object schema，至少要求布尔字段 `ok`。
- 下游只返回文本、没有 `structuredContent` 时，Anchor 自动生成 `{ "ok": true, "result": {} }` 形式的结构化结果。
- 下游返回错误时，现有 `{ "ok": false, ... }` 错误对象同样符合 fallback schema。

该修复消除了启用工具聚合后“部分 Tool 没有输出契约”的目录差异。MCP 规范本身允许省略 `outputSchema`，因此这里属于面向 ChatGPT Action 刷新链路的兼容规范化，而不是对下游 MCP 的协议违规判定。真实 `refresh_actions` 成功仍需在新版构建、重启和 ChatGPT Refresh 后确认。

超预算时，`tools/list` 返回 JSON-RPC 错误 `-32004`，错误数据代码为 `EFFECTIVE_CATALOG_CHATGPT_BUDGET_EXCEEDED`，并包含：

- `local_tool_count`
- `proxy_tool_count`
- `tool_count`
- `catalog_bytes`
- `estimated_tokens`
- 当前预算值
- 使用 `includeTools`、`excludeTools`、`maxTools` 或较小本地工具档位的修复建议

## 可观测性

预算内的每次 `tools/list` 会在 `mcp-requests.log` 记录：

- `local_tool_count`
- `proxy_tool_count`
- `tool_count`
- `catalog_bytes`
- `estimated_tokens`
- 当前分页起止位置
- 是否存在下一页

超预算拒绝也会写入相同计数与预算信息。`server_info` 同步返回 `catalog_bytes`、`catalog_estimated_tokens`、`local_tool_count` 和 `proxy_tool_count`。

## tools/list 分页

对外 `tools/list` 增加基于目录 digest 的不透明游标：

- 每页最多 64 个工具。
- 每页最多约 192 KiB。
- 返回 `nextCursor` 时客户端可继续请求。
- 游标绑定当前目录 digest；配置变化或使用其他目录的游标会返回 `-32602` 和 `invalid_tools_list_cursor`。

分页只是响应体控制措施。工具过滤和总目录预算仍在分页前针对完整有效目录执行，因此客户端即使不完整支持分页，也不会收到一个被静默截断或明显超预算的目录。

## 组合回归

使用代表性的 browser 下游工具定义验证以下组合：

| 组合 | 本地工具 | browser 工具 | 结果 |
| --- | ---: | ---: | --- |
| advanced + browser | 39 | 48 | 预算内，通过 |
| core + browser | 26 | 48 | 预算内，通过 |
| core + 受限 browser | 26 | 8 | 预算内，通过 |
| advanced + 超量 browser | 39 | 100 | 被专用预算错误拒绝，通过 |

另外验证了过滤顺序、缺失 include 项、非法配置、两页目录遍历、跨目录游标拒绝和下游重连目录契约。

## 已执行验证

- `cargo test --manifest-path src-tauri/Cargo.toml tools::catalog::tests --lib`：8 passed。
- `cargo test --manifest-path src-tauri/Cargo.toml mcp::server::tests --lib`：11 passed。
- `cargo test --manifest-path src-tauri/Cargo.toml mcp::proxy::tests --lib`：14 passed。
- 追加输出架构修复后，`cargo test --manifest-path src-tauri/Cargo.toml mcp::proxy::tests --lib`：15 passed。
- `cargo test --manifest-path src-tauri/Cargo.toml mcp::listener::tests --lib`：10 passed。
- `cargo test --manifest-path src-tauri/Cargo.toml --test call_tool_contract`：21 passed。
- `cargo test --manifest-path src-tauri/Cargo.toml --lib`：249 passed，1 ignored。
- 追加输出架构修复后，`cargo test --manifest-path src-tauri/Cargo.toml --lib`：250 passed，1 ignored。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings`：通过。
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features cli --bin anchor`：通过。
- `pnpm check`：0 errors，0 warnings。
- `pnpm build`：通过。
- `git diff --check`：通过，仅有工作区 LF/CRLF 提示。
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`：仍被仓库既有全局格式差异阻塞；本任务涉及的 MCP listener/server 格式差异已按 rustfmt 输出修正。

## 发布与应用刷新说明

当前运行中的 Anchor MCP 仍是修改前的二进制。需要在维护窗口构建并重启 Anchor，确保新的下游过滤、预算诊断、目录指标和分页行为生效。随后应使用应用 ID `asdk_app_6a67f79bee588191b6c55e8ea9e276a9` 对应的 ChatGPT 应用执行刷新；若上游仍保留旧工具快照，则重新创建应用连接并重新授权。

本轮没有直接操作 ChatGPT 网页界面，因此未声称已完成该应用的真实安装或刷新。
