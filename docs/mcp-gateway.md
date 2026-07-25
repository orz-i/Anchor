# 单一 MCP Gateway 与多工作区

MCP Gateway 允许多个 `WorkspaceProfile` 共用：

- 一个本地 Gateway 监听端口；
- 一个公网域名；
- 一个 Cloudflare 或 FRP 隧道；
- 多个彼此隔离的 ChatGPT App/MCP 连接。

每个工作区仍是一个独立的逻辑 MCP server：

```text
https://mcp.example.com/w/<workspace-id>/mcp
```

Gateway 不使用 ChatGPT App 名称或 MCP `clientInfo` 选择工作区。工作区只能由服务端 URL 路径确定。

## 架构

第一阶段保留每个工作区自己的内部 MCP listener 和端口，在本机增加一个受控反向代理：

```text
ChatGPT
   |
one public hostname / one tunnel
   |
127.0.0.1:<gateway-port>
   |
   +-- /w/workspace-a/mcp -> 127.0.0.1:<workspace-a-port>/mcp
   +-- /w/workspace-b/mcp -> 127.0.0.1:<workspace-b-port>/mcp
```

只转发以下协议路径：

- `mcp`；
- `.well-known/oauth-authorization-server`；
- `.well-known/oauth-protected-resource`；
- `oauth/authorize`；
- `oauth/token`。

其他路径、未知工作区和非法工作区 ID 均返回 `404`，不会回退到默认工作区。

## GUI 配置

打开 **设置 → 通用 → 单一 MCP Gateway**：

1. 选择“启用单一 Gateway”；
2. 设置一个未被任何 MCP/Actions 服务占用的本地端口，默认 `28765`；
3. 选择“隧道所有者工作区”；
4. 固定 FRP/Named Tunnel 可以预填公网基础地址；Quick Tunnel 可留空；
5. 保存后，启动需要暴露的工作区 MCP 服务。

“隧道所有者”只表示复用该工作区现有的 MCP 隧道配置和密钥，不会获得其他工作区的文件或工具权限。

启用后：

- 旧的工作区直连 MCP 隧道会停止；
- owner 的 MCP 隧道改为代理 Gateway 端口；
- 各工作区 listener 继续独立运行；
- 停止一个工作区只删除其 Gateway 路由；
- 最后一个工作区停止后，Gateway 和共享隧道才会停止。

## ChatGPT 配置

为每个工作区创建独立 App/MCP 连接，例如：

```text
Coding Tools - Project A
https://mcp.example.com/w/WORKSPACE_A_ID/mcp

Coding Tools - Project B
https://mcp.example.com/w/WORKSPACE_B_ID/mcp
```

不要把同一个 URL 配置多次后依赖 App 显示名称区分工作区。

## OAuth 隔离

每个工作区 listener 的 OAuth issuer/resource 使用带工作区路径的外部基础地址：

```text
issuer:   https://mcp.example.com/w/WORKSPACE_A_ID
resource: https://mcp.example.com/w/WORKSPACE_A_ID/mcp
```

因此 A 工作区签发的 Access Token 不能用于 B 工作区路径。Session Store、请求 ID、取消状态、工具目录、Skill 快照、下游 MCP 和默认 cwd 仍保存在各自 listener 内，不在 Gateway 中共享。

## Quick Tunnel 地址变化

首次启动 Quick Tunnel 时，实际公网基础地址会写入全局 Gateway 配置。运行时 URL 的首次持久化不会触发隧道误重启。

后续若 Quick Tunnel 地址发生变化，服务会拒绝静默迁移并停止无效线路，因为所有 ChatGPT 工作区连接都需要更新。需要稳定地址时应使用 FRP、Cloudflare Named Tunnel 或其他固定域名入口。

## Linux/headless CLI

查看配置和所有工作区 URL：

```bash
coding-tools-mcp gateway show
```

启用并指定 owner：

```bash
coding-tools-mcp gateway configure \
  --enable \
  --port 28765 \
  --owner PROJECT_A
```

启动多个工作区：

```bash
coding-tools-mcp gateway serve PROJECT_A PROJECT_B PROJECT_C
```

该命令在一个前台进程中管理全部所选 MCP listener、Gateway 和共享隧道，适合由 systemd 监督。Gateway 模式下不允许为每个工作区分别启动 MCP daemon，否则多个 daemon 会争用同一个 Gateway 端口。

systemd 示例：

```ini
[Service]
Type=simple
ExecStart=/usr/local/bin/coding-tools-mcp gateway serve PROJECT_A PROJECT_B
Restart=on-failure
RestartSec=3
```

## 安全不变量

- 工作区由 URL 路径决定，JSON-RPC 参数不能切换工作区；
- Gateway 路由只包含当前正在运行的工作区；
- OAuth Token 的 audience/resource 必须匹配当前工作区 URL；
- Session ID 只在对应工作区 listener 内有效；
- 不存在“未知工作区转默认工作区”的 fallback；
- Gateway 不记录或改写 Access Token、授权码和 PKCE verifier；
- Actions 服务与隧道不合并，仍按工作区独立运行；
- Gateway 端口不能与任何工作区 MCP/Actions 端口冲突。

## 当前第一阶段边界

- 内部仍是一工作区一 listener；第一阶段优化的是公网入口和隧道数量，而不是内部端口数量；
- Gateway 请求体上限为 1 MiB，响应使用流式转发以兼容 SSE；
- 路由成员由“当前正在运行的 MCP 工作区”决定，尚未提供独立的常驻成员列表；
- GUI 可以逐个启停工作区；headless 多工作区使用 `gateway serve`，不使用每工作区 daemon；
- 完全相同 URL 加 DCR `client_id` 的租户路由不属于本阶段。
