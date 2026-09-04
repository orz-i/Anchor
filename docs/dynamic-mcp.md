# Dynamic MCP：下游 MCP Server 管理与懒发现

Anchor 可以把当前 Workspace 配置成一个 **MCP runtime host**：连接多个下游 MCP server，但不会把下游全部工具直接注入 Anchor 的一级 `tools/list`。

对 Agent 公开的入口始终是一个 `mcp` facade：

```text
mcp
 ├─ list
 ├─ get
 ├─ search_tools
 ├─ get_tool
 ├─ call
 └─ advanced lifecycle
    ├─ register
    ├─ enable
    ├─ disable
    ├─ refresh
    └─ remove
```

这套模型的目标是让下游工具数量可以增长，而不让 Anchor 主 Catalog 随所有远端 schema 一起膨胀。

## Tool profile 权限

当前源码中的 profile 权限是：

| profile | 可用 operation |
| --- | --- |
| `read-only` | `list`、`get`、`search_tools`、`get_tool` |
| `core` | read-only 全部 + `call` |
| `advanced` | core 全部 + `register`、`enable`、`disable`、`refresh`、`remove` |

动态注册和生命周期修改只在 `advanced` profile 暴露。下游 operation handler（如内部 `mcp_register_server`）只是 facade 实现细节，不应通过 MCP `tools/call` 直接调用。

## 推荐调用流程

不要先加载所有下游工具 schema。推荐：

```text
1. mcp { operation: "list" }
2. mcp { operation: "search_tools", query: "..." }
3. mcp { operation: "get_tool", server: "...", tool: "..." }
4. mcp { operation: "call", server: "...", tool: "...", arguments: {...} }
```

### 查看 server

```json
{
  "operation": "list"
}
```

查看一个 server：

```json
{
  "operation": "get",
  "server": "docs"
}
```

### 懒搜索工具

跨所有 server 搜索：

```json
{
  "operation": "search_tools",
  "query": "browser page",
  "max_results": 20
}
```

只搜索指定 server：

```json
{
  "operation": "search_tools",
  "server": "browser",
  "query": "navigate"
}
```

`search_tools` 返回紧凑 metadata；只有真正准备调用时再使用 `get_tool` 读取完整 definition/schema。

### 调用下游工具

```json
{
  "operation": "call",
  "server": "browser",
  "tool": "navigate_page",
  "arguments": {
    "url": "https://example.com"
  }
}
```

Anchor 会保留下游 schema validation、并发限制、超时、取消和连接恢复边界；失败的写/副作用调用不会被盲目重放。

## 动态注册

`register` 只接受当前严格 runtime schema，并且**注册即启用**。如果希望 server 暂停使用，应先成功注册，再执行 `disable`。`register` 不接受 `disabled` 字段。

### stdio server

```json
{
  "operation": "register",
  "server": "docs",
  "config": {
    "type": "stdio",
    "command": "node",
    "args": ["tools/docs-mcp.mjs"],
    "cwd": ".",
    "env": {
      "DOCS_MODE": "local"
    },
    "maxConcurrentRequests": 4,
    "requestTimeoutSeconds": 60
  }
}
```

相对 `cwd` 以当前 Workspace 根目录为基准。

### Streamable HTTP server

```json
{
  "operation": "register",
  "server": "remote-docs",
  "config": {
    "type": "streamable-http",
    "url": "https://mcp.example.com/mcp",
    "headers": {
      "Authorization": "Bearer ${env:REMOTE_MCP_TOKEN}"
    },
    "maxConcurrentRequests": 8,
    "requestTimeoutSeconds": 90
  }
}
```

远程 URL 必须使用 HTTPS；只有 `localhost` 或 loopback IP 允许 HTTP。URL 不能带 user info 或 fragment。

Anchor 自己管理以下 HTTP Header，配置中不能覆盖：

- `Accept`
- `Connection`
- `Content-Length`
- `Content-Type`
- `Host`
- `MCP-Protocol-Version`
- `MCP-Session-Id`
- `Transfer-Encoding`

Legacy SSE transport 已拒绝；请使用 Streamable HTTP。

## 占位符

当前 runtime 支持：

```text
${workspaceFolder}
${workspaceRoot}
${workspace}
${env:VARIABLE_NAME}
```

`${env:...}` 在运行时解析；环境变量不存在时配置失败。不要把 secret 展开后的真实值重新写进 Workspace 配置。

## 生命周期语义

### Enable / Disable

```json
{ "operation": "disable", "server": "docs" }
```

```json
{ "operation": "enable", "server": "docs" }
```

`enable` 采用 **activate-first** 语义：先确认能够连接并加载合法 catalog，再持久化 `enabled=true`。激活失败时持久配置保持原状态，不会留下“配置显示 enabled、运行态却不可用”的半成功状态。

动态 `register` 同样先连接，再持久化和发布运行态；如果持久化失败，临时连接会关闭。

### Refresh

```json
{ "operation": "refresh", "server": "docs" }
```

`refresh` 重新连接并读取下游 catalog。Anchor 不允许下游在重连期间静默改变已经接受的工具 contract；发现 catalog drift 时会明确失败，避免缓存客户端对旧 schema 继续调用。

### Remove

```json
{ "operation": "remove", "server": "docs" }
```

删除会从 Workspace 的 canonical MCP runtime config 中移除该 server，并关闭连接。

## 配置和状态权威

动态 MCP 配置最终持久化回当前 `WorkspaceProfile.runtime.mcp_config`。运行中的 proxy registry 是该配置的执行态，不是第二套长期配置数据库。

下游 tool definition 永远不会自动发布为 Anchor 一级工具；即使下游有数百个工具，Anchor 主 Catalog 仍只暴露 `mcp` facade。

历史字段如 `includeTools`、`excludeTools`、`maxTools`、`exposureMode`、`toolPrefix`、`managementTools` 等不属于当前动态注册 schema，不应继续写入新配置。

## 故障排查

| 现象 | 检查方向 |
| --- | --- |
| `MCP_OPERATION_NOT_ALLOWED` | 当前 Workspace 是否使用允许该 operation 的 tool profile |
| `MCP_RUNTIME_INITIALIZING` | runtime 是否仍在加载初始配置；等待后重试只读 discovery |
| register/enable 失败 | command/url、cwd、环境变量、HTTPS、下游 MCP handshake 是否有效 |
| `MCP_TOOL_NOT_FOUND` | 先重新 `search_tools` / `get_tool`，确认 server 与工具名 |
| refresh 失败 | 下游 catalog 是否发生不兼容 drift |
| HTTP header 配置被拒绝 | 是否试图设置 Anchor 自管的 protocol/header 字段 |

## 安全边界

- `read-only` profile 不能调用下游工具，也不能变更 server 生命周期。
- `core` 可以调用下游工具，但不能动态修改 server 配置。
- `advanced` 才能 register/enable/disable/refresh/remove。
- 下游工具自身不会扩大 Anchor Workspace 文件权限、command allowlist 或 Harness 权限。
- Streamable HTTP 不使用系统代理，并限制 endpoint 与 protocol metadata。
- 下游配置不会改变 Anchor 一级 MCP Catalog 的工具数量。
