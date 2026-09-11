# Tunnel 统一管理

Tunnel 是 Anchor 的顶级网络资源。Workspace 只提供本地 MCP 运行目标，不再保存 Tunnel 配置、Token 或启用状态。

当前一个 Workspace 的 MCP 服务最多绑定一个 Tunnel。Tunnel 支持 FRP 与 Cloudflare；FRP 服务器连接仍可复用全局 FRP Profile。

## Web Admin

浏览器管理面左侧 **Network → 隧道** 提供统一入口，可完成：

- 创建 Tunnel 并选择目标 Workspace；
- 配置 FRP / Cloudflare、固定公网 URL 与代理策略；
- 管理 Tunnel 自有 Token；
- 启用或禁用 Workspace daemon 自动托管；
- 查看状态并执行启动、停止、测试；
- 删除 Tunnel。

Workspace 详情页不再提供第二套 Tunnel 配置入口。

## CLI

### 创建与配置

```bash
# 为 Workspace 创建 MCP Tunnel
anchor tunnel create Example --name example-mcp

# Cloudflare Named Tunnel
anchor tunnel configure example-mcp \
  --type cloudflare \
  --cloudflare-mode named \
  --public-url https://mcp.example.com
anchor tunnel secret set example-mcp cloudflare --token-stdin

# 或使用全局 FRP Profile
anchor tunnel configure example-mcp \
  --type frp \
  --frp-profile prod \
  --subdomain example \
  --public-url https://example.example.com

# 允许目标 Workspace daemon 自动托管
anchor tunnel enable example-mcp
```

`anchor tunnel secret set` 推荐使用 `--token-stdin` 或 `--token-file`；`--token` 会进入 shell history，只适合受控场景。

### 查看与运行

```bash
anchor tunnel list
anchor tunnel show example-mcp
anchor tunnel status example-mcp
anchor tunnel test example-mcp
anchor tunnel start example-mcp
anchor tunnel stop example-mcp
anchor tunnel restart example-mcp
```

`start/stop/test` 直接操作指定 Tunnel；通常目标 Workspace daemon 应已经运行。`enable/disable` 控制 Workspace daemon 启动或重启时是否自动托管 Tunnel，不再通过 `workspace start --tunnel` 或 `serve --tunnel` 临时决定。

### 修改与删除

```bash
anchor tunnel configure example-mcp --no-proxy
anchor tunnel secret clear example-mcp cloudflare
anchor tunnel disable example-mcp
anchor tunnel delete example-mcp
```

Tunnel 的目标 Workspace/service 创建后不可原地更换；需要重新绑定时删除并重新创建，避免运行期出现双重所有权。

## Workspace 与 daemon 生命周期

当某个 Tunnel 已启用且目标 Workspace daemon 启动时，daemon 会自动托管该 Tunnel，并继续负责断线恢复和 Quick Tunnel 公网 URL 回写。公网 URL、运行身份 revision 与 Secret 都属于 Tunnel，而不是 WorkspaceProfile。

因此 Workspace 启动命令保持单一职责：

```bash
anchor start Example --service mcp
anchor workspace start Example --service mcp
anchor serve Example --service mcp
```

这些命令不再接受公开的 `--tunnel` / `--no-tunnel` 参数。

## MCP Gateway

Gateway 直接引用一个顶级 Tunnel：

```bash
anchor gateway configure --enable --port 28765 --tunnel example-mcp
```

Tunnel 的目标 Workspace 只提供本地运行上下文，不再是“Tunnel owner”。Gateway 启用后，选中的 Tunnel 由 Gateway control domain 管理；需要删除该 Tunnel 时，应先切换 Gateway Tunnel 或禁用 Gateway。

## 配置升级

profiles 配置 schema 当前为 v2。首次读取 v1 配置时，Anchor 会执行一次性迁移：

- 每个旧 `WorkspaceProfile.tunnel` 转为顶级 Tunnel；
- 旧 Workspace `frp_token` / `cloudflare_token` 移入对应 Tunnel secret scope；
- Gateway 的旧 workspace owner 引用转换为 `tunnelId`；
- 成功迁移后立即按 v2 重写配置，不保留运行期双格式读取路径。

无 `schema_version` 的更早配置仍不受支持。
