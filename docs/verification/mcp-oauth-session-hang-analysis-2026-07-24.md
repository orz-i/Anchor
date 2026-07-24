# MCP / OAuth 会话卡死分析

日期：2026-07-24

## 原始结论（问题发现时）

审计发现时的实现中存在多项足以独立造成“连接卡住、反复授权、工具调用长期无响应”的服务端缺口。上游客户端或 Cloudflare 仍可能放大故障，但不能把问题只归因于上游。当前状态以“修复状态复审”一节为准。

优先级最高的四类问题是：

1. Streamable HTTP 响应语义不完整。
2. OAuth 401 缺少 MCP 要求的发现挑战头。
3. 工具执行没有统一的服务端截止时间，部分内部超时实际未生效。
4. 下游 stdio MCP 初始化发生在 HTTP 开始接收请求之前，但运行时已经显示 Running。

## 修复状态复审（2026-07-24）

本节是对当前工作区代码的复审结果。原“黑盒验证”和风险描述保留为发现问题时的基线，不代表所有行为仍可复现。本次复审遵循不启动现有 MCP、桌面应用或 dev server 的约束，仅执行静态代码审计、Rust 编译检查和正式 `pnpm desktop:build`。

| 问题 | 状态 | 当前实现与剩余缺口 |
|---|---|---|
| 通知返回 `200 null` | 已修复 | listener 在无 `id` 的 `notifications/*` 请求上直接返回 `202 Accepted` 空响应。 |
| GET `/mcp` 返回普通 JSON | 已修复 | GET 现在返回 `405 Method Not Allowed`，并声明 `Allow: POST`。 |
| OAuth 401 缺少发现挑战 | 已修复 | MCP OAuth 失败响应包含 `WWW-Authenticate: Bearer resource_metadata="..."`。Actions OAuth 也已同步 protected-resource metadata 路由与 challenge 参数。 |
| 没有统一 MCP 请求截止时间 | 部分修复 | listener 已增加 90 秒总超时，可避免 HTTP 请求无限等待；但 `spawn_blocking` 内正在执行的同步工具不会因外层 future 超时而自动停止。 |
| `git::run_git` 忽略 timeout | 未修复 | 仍使用阻塞式 `Command::output()`，并以 `let _ = limit` 丢弃超时参数。 |
| history lock 无限等待 | 未修复 | `lock_directory` 仍调用阻塞式 `lock_exclusive`，没有重试或截止时间。 |
| 代理初始化阻塞 HTTP 就绪 | 部分修复 | Axum 现在可在后台代理初始化期间开始接收请求，多个下游也改为并发初始化；但 Runtime 仍在 bind 成功后标记 Running，没有 ready/degraded 状态。 |
| 代理队列等待不计入超时 | 已修复 | timeout 已包住 mutex 获取和实际请求，错误信息明确包含 queue wait。 |
| 代理超时后保留失效工具 | 部分修复 | 调用失败后会移除对应路由和工具；尚无自动重连，也没有工具目录变更通知。 |
| 代理忽略下游 server request | 已修复 | 对带 `id` 的下游 server request 显式返回 JSON-RPC `-32601`，不再静默等待。 |
| OAuth resource / audience 不完整 | 已修复（MCP 主链路） | authorize/token 已接收并校验 `resource`，JWT `aud` 绑定 canonical `/mcp` resource，`iss` 绑定授权服务器基地址。 |
| Quick Tunnel URL 可能陈旧 | 已修复（代理请求主链路） | `external_base_url` 现在优先采用受校验的 `X-Forwarded-Host` / `Forwarded`，再回退到配置 URL，因此经当前 tunnel 到达的 OAuth/MCP 请求不会被旧配置 hostname 覆盖。没有转发头的本地请求仍会回退到启动时配置。 |
| `mcp-requests.log` 不在 UI | 未修复 | 请求日志已包含 `duration_ms` 和 outcome，但 UI 日志列表仍未读取 `mcp-requests.log`，也缺少客户端地址、Ray ID 与代理排队指标。 |
| redirect URI 精确注册 | 未修复 | token 交换会与授权码中的 URI 精确比较，但 authorize 阶段仍接受任意 redirect URI，开放重定向风险仍在。 |
| Accept / 协议版本 / Origin / HTTP 限制 | 未修复 | 仍使用 `CorsLayer::permissive()`；未见 MCP `Accept`、`MCP-Protocol-Version`、Origin、并发、body limit 或 cancellation 的完整处理。 |

### 本次构建修复

首次运行 `pnpm desktop:build` 时，前端生产构建成功，但 Rust 编译因以下问题失败：

- Actions listener 和鉴权中 4 处调用仍使用旧版 OAuth 函数签名。
- 下游 MCP 通知 flush 的错误类型无法推断。

当前修复包括：

- Actions OAuth 使用明确的 issuer、resource 和 protected-resource metadata URL。
- Actions 增加 `/.well-known/oauth-protected-resource` 路由。
- 下游通知写入与 flush 显式映射 I/O 错误，消除类型推断错误。

复验结果：`cargo check`、OAuth 单元测试、MCP listener/server/proxy 单元测试、全部 Rust 测试目标的 `cargo test --no-run` 编译、`pnpm check` 和完整 `pnpm desktop:build` 均通过。正式构建生成 x64 MSI 与 NSIS 安装包；复验过程中未启动 dev server、桌面应用或 MCP listener。

## 原始黑盒验证（问题发现时）

当前服务：OAuth，MCP 协议版本 `2025-06-18`，本地端口 `28766`。

### GET /mcp

localhost 与公网 Cloudflare 地址均返回：

```http
HTTP/1.1 200 OK
Content-Type: application/json

{"name":"coding-tools-mcp","protocolVersion":"2025-06-18","version":"0.1.23"}
```

这不是 Streamable HTTP 对 GET 的合法响应。若服务不提供 SSE，GET 应返回 405；若提供，则应返回 `text/event-stream`。

### 未认证 POST /mcp

localhost 与公网地址均返回：

```http
HTTP/1.1 401 Unauthorized
Content-Type: text/plain

Missing Authorization header
```

响应没有 `WWW-Authenticate`，也没有 `resource_metadata` 参数。公网与本地结果一致，说明该行为来自源站服务，而不是 Cloudflare 改写。

## 风险清单

### P0：通知响应不符合 Streamable HTTP

位置：

- `src-tauri/src/mcp/server.rs:13-20`
- `src-tauri/src/mcp/listener.rs:193-266`

通知在 `handle_request` 中返回 JSON `null`，随后 listener 统一包装成 `200 application/json`。协议要求成功接收通知时返回 `202 Accepted` 且无响应体。

可能表现：

- 客户端等待一个本不应存在的 JSON-RPC 响应。
- 客户端初始化状态机无法完成 `notifications/initialized`。
- 严格客户端断开后重试，表现为连接卡住。

### P0：GET /mcp 返回普通 JSON

位置：`src-tauri/src/mcp/listener.rs:177-187`

当前 GET 用于返回自定义发现 JSON。Streamable HTTP 中，GET 只能用于 SSE 监听；不提供 SSE 时必须返回 405。

可能表现：

- 客户端把 GET 当 SSE 通道并等待事件。
- 客户端检测到错误 Content-Type 后持续重连。
- 旧 HTTP+SSE 与新 Streamable HTTP 探测逻辑误判。

### P0：OAuth 401 缺少发现挑战

位置：

- `src-tauri/src/auth/oauth_flow.rs:94-122`
- `src-tauri/src/auth/bearer.rs:1-27`

OAuth 失败只返回纯文本 401，没有：

```http
WWW-Authenticate: Bearer resource_metadata="https://host/.well-known/oauth-protected-resource"
```

MCP 客户端依赖该挑战发现授权服务器。缺少挑战时，上游只能依赖私有兼容逻辑或手工配置。

可能表现：

- 重复请求 `/mcp` 并持续收到 401。
- 客户端 UI 一直停留在 Connecting 或 Authorizing。
- 不同客户端表现不一致，容易误判为上游故障。

### P0：没有统一 MCP 请求截止时间

位置：`src-tauri/src/mcp/server.rs:59-105`

本地工具通过 `spawn_blocking` 执行，但外层没有超时。只要内部工具阻塞，HTTP POST 就不会产生最终响应。

已发现的明确实例：

- `src-tauri/src/tools/git.rs:470-483` 接收 `limit`，但以 `let _ = limit` 丢弃，实际 `Command::output()` 可无限等待。
- `src-tauri/src/tools/history/storage.rs:91-103` 使用阻塞式 `lock_exclusive`，没有获取锁的截止时间。

风险会被 Cloudflare 放大：Cloudflare 默认 Proxy Read Timeout 为 120 秒，源站超过该时间仍没有响应时会返回 524。用户看到的可能是客户端持续等待、工具失败或会话中断。

### P0：代理 MCP 初始化阻塞 HTTP 服务就绪

位置：

- `src-tauri/src/mcp/listener.rs:126-163`
- `src-tauri/src/mcp/proxy.rs:82-170`
- `src-tauri/src/runtime/supervisor.rs:349-361`

流程是：

1. 端口先 bind。
2. RuntimeSupervisor 立即标记 Running。
3. 后台任务逐个启动下游 MCP，执行 initialize 和 tools/list。
4. 全部代理配置结束后才调用 `axum::serve`。

一个无响应的下游默认会延迟 30 秒；多个服务串行累加。此时端口可能已经占用，UI 显示 Running，但 HTTP 尚未开始处理请求。

### P1：代理调用队列等待不计入超时

位置：`src-tauri/src/mcp/proxy.rs:302-343`

每个下游 MCP 共用一个 `Mutex<ProxyConnection>`。代码先等待 mutex，再启动请求 timeout。因此并发请求排队时间不受 30 秒限制。

若前面有 N 个慢请求，后面的调用可能等待约 `N × 30 秒`，看起来像整条 MCP 会话卡死。

超时后子进程会被 kill，但 registry 中的工具路由仍保留，也没有自动重连或工具目录变更通知。

### P1：代理忽略下游发起的请求

位置：`src-tauri/src/mcp/proxy.rs:368-405`

读取循环只寻找与当前 id 相同的 response。通知可以安全忽略，但如果下游发送 sampling、elicitation、roots/list 等需要代理响应的 server request，代理不会处理。下游可能等待代理答复，直到本地 30 秒超时。

### P1：OAuth resource / audience 模型不完整

位置：

- `src-tauri/src/auth/oauth_flow.rs:123-158`
- `src-tauri/src/auth/oauth_flow.rs:240-338`
- `src-tauri/src/auth/oauth.rs:144-171`

当前授权请求与 token 请求结构都没有 `resource` 字段，JWT 的 issuer/audience 使用公网 host 基地址，而实际 MCP endpoint 是 `/mcp`。

可能表现：

- 严格客户端认为 token 没有绑定到目标 MCP resource。
- 客户端重新发起授权，形成授权循环。
- 后续接入独立授权服务器时出现 audience 不匹配。

### P1：Cloudflare Quick Tunnel URL 可能在 listener 中陈旧

位置：

- `src-tauri/src/commands/runtime.rs:99-129`
- `src-tauri/src/runtime/supervisor.rs:260-289`
- `src-tauri/src/mcp/listener.rs:75-93`
- `src-tauri/src/workspace/model.rs:279-306, 370-383`

MCP listener 在新 tunnel 启动前创建，并捕获当时的 `effective_public_url`。Quick Tunnel 的新 URL 随后才生成和持久化。若 profile 中保留上一次的 trycloudflare URL，listener 会优先使用旧配置，而不是当前请求头。

可能表现：

- OAuth metadata 指向旧 URL。
- authorization code 或 access token 的 issuer/audience 与当前公网入口不一致。
- 重启 tunnel 后出现持续 401，重启 MCP 服务后又暂时恢复。

### P1：关键请求日志没有显示在 UI

位置：

- 写入：`src-tauri/src/mcp/listener.rs:201-260`
- UI 日志列表：`src-tauri/src/commands/logs.rs:28-51`

服务写入 `mcp-requests.log`，但日志查看器只读取 `stderr.log`、`stdout.log` 和 tunnel 日志。最有价值的“request 已开始但未 completed”证据在 UI 中不可见。

同时缺少：

- 请求耗时。
- 客户端地址和 Cloudflare Ray ID。
- OAuth 失败原因与 metadata URL。
- 下游 MCP 调用开始、排队时间、执行时间和退出状态。
- 活跃请求数与代理队列长度。

### P2：其他协议和安全缺口

- 未校验 POST 的 `Accept: application/json, text/event-stream`。
- 未校验 `MCP-Protocol-Version`。
- 使用 `CorsLayer::permissive()`，没有按 MCP 要求校验 Origin。
- OAuth redirect URI 未注册或精确匹配，存在开放重定向风险。
- 未设置 HTTP 层请求超时、并发限制或显式 body limit。
- 没有处理客户端 cancellation notification。
- 没有 Dynamic Client Registration；这不是强制项，但依赖固定 client ID 会降低兼容性。

## 上游与服务端的责任判断

### 可以确定来自服务端

- GET `/mcp` 的 200 JSON。
- 401 缺少 `WWW-Authenticate`。
- 通知返回 200 JSON null。
- 工具调用和 git 子进程缺少有效统一超时。
- 代理初始化阻塞 listener 就绪。

这些行为在 localhost 与公网入口一致。

### 可能来自或被上游放大

- Cloudflare 对长时间无首包响应的请求最终返回 524。
- Quick Tunnel 重连或 URL 变化会中断连接。
- 客户端对非标准响应的容错策略不同，可能表现为快速报错、静默重试或一直 Connecting。
- 客户端断开不会自动取消已经进入 `spawn_blocking` 的本地工具。

## 故障判别方法

| 现象 | 更可能的层级 |
|---|---|
| `mcp-requests.log` 有 request、长期没有 completed | 本地工具或下游 MCP 卡住 |
| 持续 401，未访问 OAuth metadata | 401 challenge 缺失或客户端不兼容 |
| OAuth 成功后继续 401 | issuer/audience/public URL 不一致 |
| localhost 正常、公网出现 52x | tunnel / Cloudflare / origin 可达性 |
| localhost 与公网都返回相同错误协议响应 | 服务实现 |
| 服务重启后恢复，tunnel 重启后再次失败 | listener 捕获了陈旧公网 URL |
| 加入 browser/codegraph 后启动明显变慢 | 下游代理 initialize/tools/list 阻塞就绪 |

## 建议修复顺序

### 第一批：协议止血

1. GET `/mcp` 暂时返回 405，除非实现完整 SSE。
2. 通知返回 202 且空 body。
3. 401 增加标准 `WWW-Authenticate` 和 `resource_metadata`。
4. MCP request 增加统一最大执行时间；在 Cloudflare 下建议小于其 120 秒默认读超时。
5. 修复 `git::run_git`，真正执行进程 timeout 并 kill/wait。
6. 将 `mcp-requests.log` 加入 UI，并记录 duration 和 correlation id。

### 第二批：代理可靠性

1. 先启动 Axum，再并发初始化各下游 MCP。
2. Runtime 增加 `ready/degraded`，不要 bind 成功即视为工具目录就绪。
3. 将锁排队时间包含在总 timeout 内。
4. 子进程退出或 timeout 后自动重连，失败时移除工具并更新目录。
5. 处理或明确拒绝下游 server requests。
6. 支持 cancellation，并避免客户端断线后继续占用阻塞线程。

### 第三批：OAuth 与边界加固

1. 统一 canonical MCP resource URI，建议明确是否包含 `/mcp`。
2. 在 authorize/token 中接收并校验 `resource`，JWT aud 与之绑定。
3. Quick Tunnel URL 更新后同步 listener 状态，或启动 tunnel 后再启动 OAuth/MCP listener。
4. 精确校验 redirect URI。
5. 校验 Origin、Accept、MCP-Protocol-Version，并增加并发/body/慢请求限制。

## 参考规范

- Model Context Protocol 2025-06-18：Transports、Lifecycle、Authorization、Tools。
- RFC 9728：OAuth 2.0 Protected Resource Metadata。
- RFC 8414：OAuth 2.0 Authorization Server Metadata。
- Cloudflare Error 524 文档：默认 120 秒 Proxy Read Timeout。
