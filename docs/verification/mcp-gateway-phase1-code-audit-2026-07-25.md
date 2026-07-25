# MCP Gateway 第一阶段代码审计（2026-07-25）

## 审计结论

结论：**有条件通过，不建议直接标记为生产完成。**

当前实现已经建立正确的路径级隔离骨架。本轮静态审计没有发现可以通过 A 工作区路径直接取得 B 工作区 `SharedState` 的代码路径，也没有发现未知工作区回退到默认工作区的实现。

主要问题集中在公网入口资源治理、URL 状态模型、长连接关闭、错误传播和端到端测试完整性。

## 审计范围

- `src-tauri/src/mcp/gateway.rs`
- `src-tauri/src/tunnel/access.rs`
- `src-tauri/src/runtime/maintenance.rs`
- `src-tauri/src/runtime/supervisor.rs`
- `src-tauri/src/commands/runtime.rs`
- `src-tauri/src/commands/tunnel.rs`
- `src-tauri/src/commands/workspace.rs`
- `src-tauri/src/cli/mod.rs`
- `src-tauri/src/cli/workspace.rs`
- `src-tauri/src/workspace/model.rs`
- `src-tauri/src/auth/oauth_flow.rs`
- `src-tauri/src/mcp/listener.rs`

基线提交：`3650ffc`、`0b92851`、`c4fb70b`。

## 已确认有效的安全边界

- 工作区由 `/w/<workspace-id>/...` 服务端路径确定。
- 非法 ID、未知工作区和非协议路径 fail-closed 返回 `404`。
- Gateway 只保存 `workspace_id -> local_port`，不保存 WorkspaceState、Token、Session 或工具上下文。
- 每个内部 listener 继续维护独立 OAuth runtime、MCP Session、工具目录、Skill、下游 MCP 和 cwd。
- OAuth resource 使用带工作区路径的精确 URL，并以精确字符串比较拒绝跨 resource Token。
- Gateway 仅绑定 `127.0.0.1`，公网暴露由受管隧道完成。
- Gateway 端口冲突、owner 删除和手动 MCP 隧道绕过已被阻止。
- 响应体采用流式转发，可保留 MCP SSE 长连接。

## P1 问题

### P1-1：公网 Gateway 缺少自身的并发与慢请求治理

位置：`src-tauri/src/mcp/gateway.rs:254-318`。

当前只有 1 MiB `DefaultBodyLimit`，没有 Gateway 级并发上限、header/body 读取截止时间、上游连接超时、非 SSE 总截止时间或速率限制。内部 listener 的限流发生在 Gateway 已接收并缓冲请求之后。

影响：单一公网入口可能成为所有工作区共享的 DoS 放大点。

建议：为非 SSE 路径增加连接、header/body 和总请求截止时间；为 SSE 增加独立连接配额与 idle timeout；增加全局及每工作区 Semaphore、令牌桶和 header bytes 上限。

### P1-2：公网基础 URL 校验不足

位置：`src-tauri/src/mcp/gateway.rs:350-364`。

当前允许远程 `http://` 和带非根路径的 URL。远程 HTTP 会允许 OAuth code/Token 经明文传输；`https://example.com/base` 会生成 `/base/w/...`，但 Router 只监听根 `/w/...`。

建议：非 loopback 强制 HTTPS；loopback 才允许 HTTP；第一阶段强制 path 为 `/`；拒绝 dot segments、query、fragment 和 userinfo。

### P1-3：`public_url` 同时承担用户配置与运行时观测值

位置：

- `src-tauri/src/settings/model.rs:27-31`
- `src-tauri/src/tunnel/access.rs:270-279`
- `src-tauri/src/tunnel/supervisor.rs:802-815`
- `src-tauri/src/commands/runtime.rs:104-116`

同一字段同时表示固定 FRP/Named 地址、Quick Tunnel 观测地址和 OAuth issuer/resource 基础地址。reconcile 会用全局值覆盖 owner 的 `tunnel.public_url`，可能在 owner 切换、Quick/Named 切换或固定域名修改后复用旧地址。

建议拆分：`configured_public_base_url`、`observed_public_base_url`、`observed_owner_workspace_id`、`observed_tunnel_signature`。owner 或模式变化时清除不兼容的 observed URL。

### P1-4：长连接关闭超时后未强制终止

位置：`src-tauri/src/mcp/gateway.rs:234-241`。

graceful shutdown 等待 3 秒后，超时结果被忽略，JoinHandle 被消费并丢弃，没有 `abort()`。存在 SSE 或卡住连接时，后台任务和旧连接可能继续 detached 运行，且 supervisor 无法再追踪。

建议：超时后调用 `abort()` 并等待 JoinError；记录 draining/timeout 状态和活跃 SSE 连接数。

### P1-5：桌面端 Gateway 隧道恢复没有指数退避

位置：`src-tauri/src/runtime/maintenance.rs:85-109`。

桌面维护循环每 2 秒调用 `reconcile_mcp_gateway`。现有 `TunnelRetryState` 不覆盖 Gateway。持续失败会反复启动、写日志并占用全局 TunnelSupervisor 锁。CLI 已有退避，GUI 行为不一致。

建议增加独立 `GatewayRetryState`，使用 1/2/4/8/16/32/60 秒退避；Quick URL drift 进入人工干预状态。

### P1-6：工作区启动吞掉 Gateway reconcile 失败

位置：`src-tauri/src/commands/runtime.rs:231-235`。

Gateway listener 绑定或共享隧道启动失败时，仅写 `eprintln!`，`start_runtime` 仍返回内部 MCP running。

建议返回 `running_local/degraded_public`，或在 Gateway listener 失败时直接返回错误；ServicePanel 应显示 route/tunnel 状态。

### P1-7：缺少双真实 listener 的隔离端到端测试

现有测试覆盖路径白名单、未知工作区、Header/query 转发、端口冲突和 owner 删除，但没有直接验证：

1. 两个真实 listener 经 Gateway 分别完成 OAuth；
2. A Token 调 A 成功，调 B 被拒绝；
3. A Session ID 用于 B 被拒绝；
4. A 文件工具不能访问 B workspace；
5. 停止 A 后 A 为 `404`，B 不受影响。

该场景应成为发布前强制集成测试。

## P2 问题

### P2-1：CLI 关闭路径忽略共享隧道停止错误

位置：`src-tauri/src/cli/mod.rs:330-347`。

`shutdown_gateway_services` 返回 `()`，并使用 `let _ = reconcile_mcp_gateway(...).await`。停止失败仍会输出 `gateway_stopped` 并成功退出。

建议返回 `AppResult<()>`，聚合清理错误；best-effort 也应输出 warning 并返回非零状态。

### P2-2：Gateway 状态错误可能陈旧或为空

位置：`src-tauri/src/mcp/gateway.rs:204-224`、`266-273`。

正常 stop 不清除 `last_error`；Axum serve 返回值被丢弃；handle finished 时错误文本可能为空；GUI 只在加载和保存时刷新状态。

建议记录 server task 退出原因，区分 stopped/draining/recovering/error，并在正常停止时清除旧错误。

### P2-3：反向代理 Header 清理不完整

位置：`src-tauri/src/mcp/gateway.rs:305-329`、`366-379`。

未解析 `Connection` 中动态声明的 hop-by-hop header；请求 `Content-Length` 未明确移除重算；未处理 `Proxy-Connection`；响应重复 header 使用 `insert` 可能覆盖多值 Cookie 或 challenge。

### P2-4：GUI 地址列表包含尚未运行的工作区

GUI 为全部 profile 显示 Gateway URL，但路由表只包含 active workspace。建议同时显示 configured/routed/listener-state。

## 依赖与代理核对

当前 `reqwest 0.12.28` 使用 `default-features = false`，实际特性树只有 `json`、`rustls-tls` 和 `stream`，未观察到系统代理特性，因此未把 localhost 转发经环境代理列为当前确定漏洞。仍建议内部 client 显式 `no_proxy()`，防止未来依赖特性变化改变边界。

## 验证结果

- Gateway 专项测试：9 passed，0 failed。
- 全特性严格 Clippy：通过，0 warning。
- 审计开始时工作区 clean，HEAD=`c4fb70b`。

## 推荐整改顺序

第一批发布阻断项：

1. 强化公网 URL 校验；
2. 增加 Gateway 并发、header/body、连接和非 SSE 截止时间；
3. 拆分 configured URL 与 observed URL；
4. 增加双 workspace OAuth/Session/文件边界端到端测试。

第二批生命周期可靠性：

1. graceful timeout 后 abort；
2. 桌面 Gateway 隧道指数退避；
3. 启动错误进入 degraded/status；
4. CLI 清理错误向上传播。

第三批代理规范与可观测性：

1. 完整 hop-by-hop header 处理；
2. Gateway 请求日志、route、排队和上游耗时；
3. GUI 持续状态刷新；
4. SSE 活跃连接与 draining 指标。

## Production-ready 门禁

- 双 workspace OAuth Token、Session 和文件边界负向测试通过；
- 非 loopback HTTP 和带路径 public URL 被拒绝；
- 100+ 并发慢请求不能拖垮全部工作区；
- SSE 存在时禁用或换端口可在截止时间内完全关闭或强制中止；
- owner 从 Quick 切换到 Named、从 A 切换到 B 不复用旧 observed URL；
- Gateway listener 或 tunnel 故障进入明确 degraded 状态；
- 停止失败不会被报告为成功。
