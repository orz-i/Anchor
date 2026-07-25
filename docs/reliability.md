# 连接恢复、自动重试与 OAuth 续约

Coding Tools MCP 对本地服务、隧道、下游 MCP 和桌面后台连接采用分层恢复。恢复遵循两个原则：

1. 能安全重复的连接与读取操作自动重试；
2. 结果可能已经生效的写入和工具调用不盲目重放。

## MCP 与 Actions 本地服务

桌面端每两秒维护一次当前 WorkspaceProfile 对应的 MCP 和 Actions 服务。

以下情况会进入 `recovering`：

- listener 异步任务意外结束；
- 已运行服务连续三次未检测到监听端口；
- 启动流程超过 10 秒仍停留在 `starting`。

自动恢复最多尝试五次：

```text
1s → 2s → 4s → 8s → 16s
```

恢复成功后：

- 服务状态恢复为 `running`；
- GUI 显示恢复成功提示；
- `recoveredCount` 增加；
- 日志写入 `[recovery]` 记录。

五次失败后进入 `error`，停止自动重试，等待用户修正配置后手动启动。

## 隧道恢复

本地服务保持运行时，桌面端会确保对应 FRP 或 Cloudflare 隧道仍然存在。

- 隧道进程退出后自动重新创建；
- 重连失败使用指数退避，避免断网时持续刷进程和日志；
- 本地服务进入最终错误状态后，清理其孤儿隧道；
- 恢复后的公网 URL 会重新保存到当前 WorkspaceProfile。

Linux CLI 的 `serve --tunnel` 同样会持续维护隧道，重试间隔最高为 60 秒。

## Linux CLI

`coding-tools-mcp serve` 不再只等待 `Ctrl+C`，而是持续维护已启动服务：

```bash
coding-tools-mcp serve PROFILE_ID --service all --tunnel
```

状态变化会输出：

- `recovering`：正在自动恢复；
- `running`：恢复成功；
- `error`：五次恢复均失败。

使用 `--json` 时输出结构化 `service_state`、`tunnel_retry_scheduled` 和 `tunnel_reconnected` 事件。

本地服务恢复耗尽后，CLI 会优雅停止已启动服务和隧道，并以非零状态退出。配合 systemd 的 `Restart=on-failure`，可以再由系统服务管理器进行进程级重启。

## 下游 MCP 聚合

stdio 下游 MCP 首次连接最多尝试三次。已经成功加载过工具目录的下游进程断联后：

- 保留已公开的工具路由；
- 后台进行最多五次连接恢复；
- 下一次工具调用也会先尝试建立新连接；
- 防止多个请求同时创建重复重连任务。

### 不自动重放工具调用

若下游在 `tools/call` 期间断开，服务端无法确认工具是否已产生副作用，因此该调用会返回错误：

```json
{
  "retryable": true,
  "request_replayed": false,
  "reconnect_scheduled": true
}
```

服务只恢复连接，不自动重复原工具调用。客户端应根据工具是否只读、是否幂等决定是否重新提交。

## 桌面后台连接

Workspace 页面会周期读取 MCP 和 Actions 状态：

- 正常时每五秒同步；
- 失败后使用 1、2、4、8、15 秒退避；
- 页面恢复可见或系统重新联网时立即同步；
- 页面隐藏时暂停轮询；
- 初始加载失败时保留恢复页面，而不是永久空白；
- 连续失败只在关键阶段显示通知，避免 Toast 风暴。

幂等读取类 Tauri IPC 最多自动尝试三次。保存配置、启动、停止、密钥轮换等变更操作不会由前端自动重放。

## OAuth 续约

MCP 和 Actions OAuth authorization server metadata 现在声明：

```json
{
  "grant_types_supported": ["authorization_code", "refresh_token"]
}
```

令牌周期：

- Access Token：1 小时；
- Refresh Token：90 天；
- 每次成功刷新都会返回新的 Refresh Token，并重新获得 90 天有效期。

刷新令牌与 Client ID、issuer 和 resource 绑定。刷新时不能扩大 scope。

成功和错误响应均返回：

```text
Cache-Control: no-store
Pragma: no-cache
```

同一进程内，已经使用过的 Refresh Token 再次提交会返回 `invalid_grant`，提示重新授权。

### 当前限制

Refresh Token 的已使用列表目前保存在服务进程内存中。服务重启后不会保留历史重放记录，但令牌签名、过期时间、Client ID、issuer 和 resource 校验仍然有效。后续可将 token family 状态持久化，以实现跨进程重启的完整轮换重放保护。

重新生成 OAuth Token Secret 会立即使现有 Access Token 和 Refresh Token 全部失效，客户端需要重新授权。

## 日志

主要恢复日志前缀：

```text
[recovery]
[mcp-proxy:<server>]
```

自动重试只处理连接建立、listener 重启、隧道恢复和只读状态同步。业务写入与可能产生副作用的 MCP 工具调用保持显式失败。

## Windows 桌面子进程

Windows GUI 启动的内部控制台程序默认使用无窗口模式：远程命令、Git、Harness 检测、下游 MCP、FRP 和 Cloudflare 不应弹出或闪现命令行窗口。stdout/stderr 仍通过工具结果、session 和日志读取。

详细审计见：

```text
docs/verification/windows-console-window-flash-analysis-2026-07-25.md
```
