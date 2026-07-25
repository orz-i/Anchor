# 单一 MCP 出入口、多 Client、多 Workspace 隔离调研（2026-07-25）

## 结论

目标可以实现，但需要区分三种方案：

| 方案 | 可行性 | 建议 |
| --- | --- | --- |
| 一个服务进程、一个公网域名/隧道、每个 workspace 使用不同 URL 路径 | 可行 | **推荐** |
| 完全相同的 MCP URL，通过每个 ChatGPT 连接的 OAuth `client_id` 区分 workspace | 有条件可行 | 可作为高级方案，不宜作为首选 |
| 完全相同的 MCP URL，通过 MCP `initialize.clientInfo` 区分 workspace | 不应采用 | `clientInfo` 是实现信息，不是可信身份 |

推荐最终形态：

```text
https://mcp.example.com/w/workspace-a/mcp
https://mcp.example.com/w/workspace-b/mcp
https://mcp.example.com/w/workspace-c/mcp
```

这些 URL 共用：

- 同一个服务进程；
- 同一个监听端口；
- 同一个域名；
- 同一个 Cloudflare/FRP/Secure MCP Tunnel 出入口。

ChatGPT 中仍创建多个独立 App/MCP 连接，每个连接填写一个不同的 workspace URL。这样能够减少隧道，同时保持清晰、稳定、可审计的隔离边界。

## 第一阶段实施结果

本调研提出的推荐方案已在本轮落地：

- 新增全局 `McpGatewayConfig`，默认关闭，保留单一 Gateway 端口；
- 新增受控本地反向代理，将 `/w/<workspace-id>/...` 路由到对应内部 MCP listener；
- 只允许 MCP、OAuth 和 OAuth metadata 协议路径，未知工作区与其他路径返回 `404`；
- Gateway 响应采用流式转发，保留 MCP Session、协议版本和 OAuth 相关请求/响应头；
- 每个内部 listener 的 issuer/resource 热更新为带工作区路径的外部 URL；
- 各工作区仍保持独立 Session Store、OAuth runtime、工具目录、Skill 快照、下游 MCP 和默认 cwd；
- 单一共享隧道复用指定 owner 工作区现有的 MCP 隧道配置与密钥；
- 启用 Gateway 后，工作区直连 MCP 隧道会被回收，手动 MCP 隧道操作也不能绕过 Gateway；
- GUI 提供 Gateway 配置、状态和每个工作区的 ChatGPT 连接地址；
- Linux/headless CLI 提供 `gateway configure`、`gateway show` 和 `gateway serve`；
- Quick Tunnel 首次 URL 可安全持久化，后续地址漂移会拒绝静默迁移；
- Gateway owner 删除、Gateway 端口与工作区服务端口冲突均被阻止。

第一阶段仍保留“一工作区一内部 listener”。本轮优化的是公网域名、入口与隧道数量，而不是内部监听端口数量。完整运维说明见 `docs/mcp-gateway.md`。

## 调研基线时的 Coding Tools 实现现状

当前版本仍是“一份 `WorkspaceProfile` 对应一个 MCP listener”的架构，不能直接在一个 MCP URL 内根据 client 切换 workspace。

### 1. Listener 在启动时固定绑定一个 workspace

`src-tauri/src/mcp/listener.rs` 的 `spawn_listener` 接收并固定：

- `workspace_path`；
- `workspace_id`；
- `AuthConfig`；
- `RuntimeConfig`；
- 一个 `SharedState`；
- 一个 OAuth runtime；
- 一个下游 MCP 代理目录。

所有 `/mcp` 请求最终都调用同一个：

```text
handle_request_with_protocol_and_cancellation(&state.mcp, ...)
```

请求处理过程中没有 workspace registry，也没有根据 URL、Token、OAuth client 或 MCP client 信息选择另一个 `SharedState`。

### 2. RuntimeSupervisor 为每个 WorkspaceProfile 启动独立端口

`src-tauri/src/runtime/supervisor.rs` 启动 MCP 时，将当前 profile 的：

- 本地端口；
- 项目路径；
- profile ID；
- OAuth 配置；
- 公网 URL；
- RuntimeConfig；

直接交给 `mcp::spawn_listener`。

因此当前运行模型是：

```text
WorkspaceProfile A -> listener A -> local port A -> tunnel/public URL A
WorkspaceProfile B -> listener B -> local port B -> tunnel/public URL B
```

### 3. `clientInfo` 只被校验，没有被保存或用于路由

`src-tauri/src/mcp/protocol.rs` 会验证：

```text
initialize.params.clientInfo.name
initialize.params.clientInfo.version
```

但当前 `Session` 只保存：

- 协议版本；
- 是否完成 initialized；
- 最后活动时间；
- 已使用的 request ID。

它不保存 `clientInfo`，也不把 `clientInfo` 与 workspace 绑定。

即使将其保存，也不应把它当作安全隔离依据。MCP 规范将 `clientInfo` 定义为客户端实现信息，用于交换名称、版本和展示信息；它不是认证凭据，客户端可以自行填写。

### 4. 当前 OAuth 只接受一个固定 client_id

`src-tauri/src/auth/oauth_flow.rs` 的 `OAuthRuntime` 只有一个配置的 `client_id`，`client_id_allowed` 使用精确相等比较。

`src-tauri/src/auth/oauth.rs` 当前发布的授权服务器元数据没有：

- `registration_endpoint`；
- `client_id_metadata_document_supported`。

因此当前服务既没有 Dynamic Client Registration（DCR），也没有 Client ID Metadata Documents（CIMD）注册流程。

此外，`verify_oauth_bearer_header` 当前只返回“允许或拒绝”，不会把已验证 Token 的 `client_id` 或其他 claims 返回给 MCP listener。即使 Token 内已经包含 `client_id`，listener 也无法利用它选择 workspace。

## ChatGPT 侧能力

### 已确认支持

OpenAI 官方文档确认：

1. 创建自定义 MCP App/连接时，需要填写 MCP endpoint 和认证方式；
2. 一个 ChatGPT 会话可以同时启用或调用多个 App；
3. 使用 OAuth 时，ChatGPT 支持 CIMD、DCR、预定义 OAuth client 和 PKCE；
4. DCR 会为一个 MCP server connection 创建并复用专用 `client_id`；
5. ChatGPT 会把 OAuth access token 放入后续 MCP 请求的 `Authorization: Bearer` 请求头；
6. 每个 App 的工具元数据会在扫描时读取，发布后使用独立快照。

这意味着 ChatGPT 具备创建多个逻辑 MCP 连接所需的基础能力。

### 官方没有提供的路由契约

官方配置流程只要求用户填写：

- 用户可见名称和描述；
- 公网 MCP URL；
- 认证配置。

官方文档没有声明 ChatGPT 会在每个 MCP 请求中发送以下任一字段：

- ChatGPT App ID；
- App 名称；
- 用户在设置页填写的连接名称；
- 自定义 workspace header；
- 可由服务端信任的 `clientInfo` workspace 标识。

因此不能假设“在 ChatGPT 中把同一个 URL 配置两次并起不同名字”，服务端就能知道本次调用来自哪个 App。

## 推荐方案：同一隧道，不同路径

### 路由模型

```text
Internet / ChatGPT
        |
        v
one public hostname + one tunnel
        |
        v
MCP Gateway / Multi-workspace Listener
        |
        +-- /w/workspace-a/mcp -> WorkspaceRuntime A
        +-- /w/workspace-b/mcp -> WorkspaceRuntime B
        +-- /w/workspace-c/mcp -> WorkspaceRuntime C
```

每个路径代表一个逻辑 MCP server。MCP 规范要求一个逻辑 server 提供单一 MCP endpoint，并不禁止同一个 HTTP 服务进程托管多个逻辑 endpoint。

### ChatGPT 配置

分别创建三个 App：

```text
Coding Tools - Project A
Endpoint: https://mcp.example.com/w/workspace-a/mcp

Coding Tools - Project B
Endpoint: https://mcp.example.com/w/workspace-b/mcp

Coding Tools - Project C
Endpoint: https://mcp.example.com/w/workspace-c/mcp
```

ChatGPT 可以在同一对话中选择多个 App；每个 App 仍有自己的工具扫描、授权连接和发布快照。

### OAuth 隔离

每个 workspace endpoint 应使用精确的 canonical resource：

```text
https://mcp.example.com/w/workspace-a/mcp
https://mcp.example.com/w/workspace-b/mcp
```

Access Token 至少应绑定并校验：

- `iss`：授权服务器；
- `aud`/`resource`：精确 workspace MCP URL；
- `exp`：有效期；
- `scope`：工具权限；
- 服务端自定义 `workspace_id` 或等价 tenant claim。

这样，即使 A 的 Token 泄漏或被错误发送到 B 路径，也会因 audience/resource 不匹配被拒绝。

### 相比“不同 client_id”的优势

- 路由在收到请求时即可由 URL 确定，不依赖 OAuth 注册状态；
- 无认证模式、Bearer 模式和 OAuth 模式都能使用相同结构；
- URL 本身就是稳定的 resource identifier；
- ChatGPT 工具扫描天然绑定到正确 workspace；
- 删除并重建 ChatGPT App 不会改变 workspace 路由；
- 日志、限流、权限和健康检查更容易按 workspace 归档；
- 更容易阻止 confused deputy 和跨 workspace Token 复用。

## 可选方案：完全相同 URL + DCR client_id

OpenAI 官方文档说明，DCR 可以让 ChatGPT 为每个 MCP server connection 获取并复用一个专用 `client_id`。因此在以下条件全部满足时，理论上可以让多个 ChatGPT 连接指向完全相同的 MCP URL，再由 OAuth client 映射 workspace：

1. ChatGPT 当前产品界面允许创建多个指向同一 endpoint 的独立连接；
2. App 创建者为这些连接选择 DCR；
3. 服务端实现 `registration_endpoint`；
4. 每次注册得到不同 `client_id`；
5. 管理员把注册后的 `client_id` 显式绑定到一个 workspace；
6. 授权服务器把该 client 身份或映射后的 `workspace_id` 写入 access token；
7. MCP resource server 验证 Token 后返回 claims，并据此选择 workspace；
8. 工具扫描和后续调用始终使用该连接对应的 Token。

建议将绑定模型设计为：

```text
oauth_client_registration_id -> workspace_id -> allowed_scopes
```

而不是直接把随机 `client_id` 当作永久 workspace ID。

### 该方案的主要问题

- OpenAI 文档确认 DCR 是“每个 MCP server connection”注册，但没有明确保证产品 UI 永远允许重复 endpoint；需要真实 ChatGPT 端到端验证；
- 删除并重建连接可能产生新的 DCR client，需要重新绑定；
- CIMD 通常提供稳定的 ChatGPT client identity，不适合用来区分同一服务下的多个 workspace；
- OAuth client 表示“谁在调用”，workspace 表示“允许访问什么资源”，两者概念不同；
- 无认证或共享 Bearer Token 模式无法使用该路由方式；
- 扫描工具前必须先完成 OAuth，失败时排障比路径路由复杂；
- 当前项目需要重构 OAuth 注册、Token claims、认证返回值和 listener 路由四个层面。

因此该方案可做，但不应成为第一版节省隧道的实现。

## 不推荐方案：使用 MCP clientInfo

MCP `clientInfo` 的用途是交换客户端实现详情，例如名称、版本、描述和图标。它存在以下问题：

- 不是认证信息；
- 没有服务端签名；
- 不保证每个 ChatGPT App 唯一；
- 同一 ChatGPT host 很可能对多个连接发送相同实现名称和版本；
- 客户端可以伪造；
- 当前项目只校验后丢弃；
- 使用它做隔离会让恶意客户端通过修改字符串切换 workspace。

它可以用于日志和兼容性诊断，但不能作为授权或 workspace 路由主键。

## 建议的项目改造路线

### 第一阶段：单隧道、多路径网关

1. 增加一个 `WorkspaceMcpRegistry`，保存：
   - workspace ID/slug；
   - `WorkspaceProfile`；
   - `SharedState`；
   - OAuth/Token verifier；
   - session store；
   - proxy registry；
   - readiness/status。
2. 将 listener 路由改为或新增：
   - `/w/:workspace/mcp`；
   - workspace-aware protected resource metadata；
   - workspace-aware OAuth resource/audience。
3. TunnelSupervisor 只管理一个 gateway tunnel，不再为每个 profile 创建公网隧道。
4. 保留现有 profile 级启停和恢复状态，但公网入口归 gateway 所有。

### 第二阶段：严格隔离

1. 每个 MCP session 记录 `workspace_id`，并由路由确定，不接受客户端覆盖；
2. Token `aud/resource` 必须精确匹配当前路径；
3. 所有工具上下文只允许访问该 profile 的 canonical workspace root；
4. rate limit、并发、日志和取消状态按 workspace 分桶；
5. 下游 MCP 代理目录按 profile 隔离；
6. 禁止通过请求参数、`clientInfo` 或自定义 `_meta` 切换 workspace。

### 第三阶段：ChatGPT 端到端验证

至少创建两个 ChatGPT App，验证：

1. 两个 App 使用同一域名、不同路径；
2. Scan Tools 分别得到对应 workspace 的工具目录；
3. A 的 OAuth Token 调用 B 路径返回 401/403；
4. 同一对话同时选择两个 App 时，调用路由正确；
5. 重建其中一个 App 不影响另一个；
6. 更新 A 工具快照不会改变 B；
7. gateway 重启后 session、OAuth 和恢复行为符合预期。

### 第四阶段：可选 DCR 试验

在路径方案稳定后，再增加独立实验开关：

- 发布 `registration_endpoint`；
- 支持 DCR client 生命周期；
- 为注册 client 提供 workspace enrollment；
- Token 增加 `workspace_id`/tenant claim；
- 验证同一 exact URL 的两个 ChatGPT 连接能否稳定产生不同 client；
- 明确连接删除、重建、吊销和迁移行为。

只有真实 ChatGPT 端到端测试稳定后，才应把“同 URL + DCR client 路由”作为正式能力。

## 安全不变量

无论采用哪种方案，都应保证：

1. workspace 选择必须来自服务端可信路由或已验证 Token，不来自普通 JSON-RPC 字段；
2. 一个请求只能解析到一个 workspace，歧义时拒绝；
3. Token 必须绑定精确 resource/audience；
4. session ID 必须与 workspace 和认证主体绑定；
5. 不允许用同一个 session ID 跨 workspace 路径；
6. 工具目录、Skill 快照、下游 MCP 和默认 cwd 都必须来自同一 WorkspaceRuntime；
7. 日志不得记录 access token、authorization code、PKCE verifier 或完整敏感 client secret；
8. 停止一个 workspace 不得停止 gateway 或其他 workspace；
9. gateway 不得把未经认证的 workspace 路径存在性泄漏给调用方；
10. 任何 fallback 都必须 fail closed，不能落到默认 workspace。

## 最终建议

短期应实现：

```text
一个 Coding Tools MCP Gateway
+ 一个公网隧道
+ 多个 workspace 路径
+ ChatGPT 中多个 App 配置
```

不要把第一版建立在：

```text
同一个 exact URL
+ ChatGPT App 名称
+ MCP clientInfo
```

之上。

“同 exact URL + 不同 DCR client”有协议与 ChatGPT 基础能力支持，但属于需要新增 OAuth 多客户端注册、Token tenant claims 和实际产品端到端验证的第二阶段能力。

## 官方参考

- OpenAI Help Center: Developer mode and MCP apps in ChatGPT — https://help.openai.com/en/articles/12584461-developer-mode-and-full-mcp-connectors-in-chatgpt-beta
- OpenAI Developers: Authentication — https://developers.openai.com/plugins/build/auth
- OpenAI Developers: Connect and test your plugin — https://developers.openai.com/plugins/deploy/connect-chatgpt
- MCP Specification 2025-11-25: Lifecycle — https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle
- MCP Specification 2025-11-25: Authorization — https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization
- MCP Specification 2025-11-25: Transports — https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
