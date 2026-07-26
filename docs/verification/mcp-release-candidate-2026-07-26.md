# MCP / ChatGPT Apps 发布候选验证（2026-07-26）

## 结论

发布候选状态：**自动化远程 MCP、Cloudflare Quick Tunnel、OAuth PKCE、Tool catalog 和读写回滚通过；ChatGPT 网页端应用扫描与真实用户确认、稳定生产隧道、真实 FRPS 握手仍是发布前人工/环境门禁。**

本次所有运行时验证使用独立配置目录、独立临时 Workspace 和独立端口 `28767`，未读取、停止或修改现有 GUI、MCP、Actions、Cloudflare 或 FRP 实例。

## 代码与契约门禁

### 高价值 Tool 成功输出

以下 Tool 已增加条件式成功 `outputSchema`，真实成功结果必须携带关键字段和正确类型；`ok=false` 必须携带结构化 `error`：

- `server_info`
- `read_file`
- `search_text` / `grep_text`
- `apply_patch` / `patch_check`
- `exec_command`
- `write_stdin` / `kill_session` / `read_output`
- `git_status`
- `history_session_bootstrap`
- `history_session_checkpoint`
- `history_session_validate`
- `view_image`

集成测试 `src-tauri/tests/tool_output_schema_contract.rs` 使用真实 Tool 调用验证这些成功载荷，而不是只检查静态 Schema。

### Effective catalog

统一构建入口：`src-tauri/src/tools/catalog.rs`。

该入口由以下消费者共同使用：

- MCP `tools/list`
- `server_info`
- Skill `allowed-tools` 依赖解析
- Actions core catalog

门禁：

- 最多 1024 个最终 Tool；
- 最终 catalog 最大 8 MiB；
- 单 Tool definition 最大 128 KiB；
- 名称和输入/输出 JSON Schema 再验证；
- 禁止外部 `$ref` / `$dynamicRef`；
- 本地/代理重名 fail-closed；
- 按公共名称稳定排序；
- 对规范化完整 definition 计算 SHA-256；
- 32 组 proptest 随机顺序和重名 fuzz；
- 三档目录提交 snapshot：`src-tauri/tests/snapshots/effective_catalog.json`。

当前目录：

| Profile | Tool 数 | Catalog digest |
|---|---:|---|
| `core` | 26 | `a3e3150111918dab27af33f83068d6cc2ce4e921e96187ff97147b1b47b6244a` |
| `read-only` | 18 | `05bfb68c257cea73eaeb3fb8ced98f7634abc31633c7a1a9824b964708892ed4` |
| `advanced` | 39 | `22d3bdd1ccc5aa5853f38279c6e785b2aaea3c3decaa2d61765fb93907a16432` |

## 真实 Cloudflare 公网验证

### 隔离环境

- 临时 Workspace：`src-tauri/target/rc-validation/.../workspace`
- 独立 MCP 端口：`28767`
- Tool profile：`core`
- 严格 Workspace 读取边界：启用
- Cloudflare：Quick Tunnel，`cloudflared 2026.5.2`
- 连接协议：QUIC
- Cloudflare connectivity pre-check：DNS、UDP/QUIC、TCP/HTTP2、API 全部 PASS
- 边缘位置：LAX

### 无认证公网验证

证据：`docs/verification/mcp-release-candidate-2026-07-26.json`。

通过项：

- `GET /mcp` 返回 405，`Allow` 包含 POST/DELETE；
- 错误 Origin 返回 403；
- MCP `2025-11-25` initialize 和 Session 建立；
- `tools/list` 返回 26 个有序、唯一且包含输入/输出 Schema 的 Tool；
- 客户端计算的 catalog digest 与 `server_info` 一致；
- `read_file(README.md)`；
- 隔离 `apply_patch` 新建文件、读取验证、删除回滚；
- MCP Session DELETE 返回 204。

### OAuth 公网验证

证据：`docs/verification/mcp-release-candidate-oauth-2026-07-26.json`。

通过项：

- Authorization Server Metadata；
- Protected Resource Metadata；
- 未认证 initialize 返回 401 和 `resource_metadata` challenge；
- 授权页面可访问；
- ChatGPT 内置 callback 形式自动登记，无额外 callback 勾选步骤；
- S256 PKCE authorization code；
- Authorization Code Token 交换；
- Bearer MCP Session；
- Tool catalog、读调用和隔离写入/回滚；
- refresh token 轮换；
- 新 access token 可继续调用；
- 旧 refresh token 重放返回 `invalid_grant`；
- Session DELETE 返回 204。

验证器：

```bash
python scripts/validate_mcp_release_candidate.py \
  --base-url https://PUBLIC_HOST \
  --auth oauth \
  --oauth-metadata \
  --expected-profile core \
  --read-path README.md \
  --allow-write \
  --report docs/verification/mcp-release-candidate.json
```

OAuth 环境变量：

```text
MCP_OAUTH_CLIENT_ID
MCP_OAUTH_PASSWORD
MCP_OAUTH_CLIENT_SECRET   # 仅机密客户端需要
MCP_OAUTH_REDIRECT_URI    # 可选；默认使用 ChatGPT 内置 callback 形式
```

验证器不输出 Token、密码或 Client Secret。

## Tunnel 生命周期结果

- MCP listener 和 Quick Tunnel 实际启动成功；
- 公网 MCP 调用和 OAuth 流量实际穿过 Quick Tunnel；
- 验证完成后端口 `28767` 已释放；
- 指向 `127.0.0.1:28767` 的 cloudflared 已按精确 PID 清理；
- 另外两个既有 cloudflared（端口 `28764`、`28766`）未被停止或修改。

当前执行工具使用 Windows `TerminateProcess` 强制结束测试父进程，无法模拟真实控制台 Ctrl+C；因此该方式不会进入应用自身的 `shutdown()` tunnel 清理路径。真实控制台 Ctrl+C / Windows 服务停止 / Linux SIGTERM 的优雅清理仍需作为人工或目标平台 smoke 门禁，不能由本次强制终止结果替代。

## FRP 结果

通过自动化测试：

- FRP 配置、Proxy 命名、MCP/Actions 合并路由：14 项；
- Supervisor 多 Workspace 隔离、冲突、恢复和 PID 状态：8 项。

真实公网 FRP 握手未执行，原因：

- 当前主机 `frpc` 不在 PATH；
- 未提供可用 FRPS 地址、Token、域名或测试子域名。

发布前需要在隔离 FRPS 环境补充：登录、Proxy ready、MCP 公网请求、断线重连、进程停止和 PID 清理。不得把本地配置测试标记为真实 FRP 公网通过。

## ChatGPT Apps 人工验收门禁

当前执行环境无法操作用户 ChatGPT Workspace 的 Developer Mode 页面，因此以下步骤必须由有权限的用户在 ChatGPT Web 完成并保存截图/录屏：

1. 启动稳定 HTTPS MCP 地址；不要使用已经停止的本次 Quick Tunnel URL。
2. 在 ChatGPT Web 开启 Developer Mode，创建自定义 MCP App。
3. 使用 OAuth，填写 MCP 站点根 URL 和 Client ID；公开 PKCE 客户端不填写 Client Secret。
4. 扫描 Tool；预期 `core` 为 26 个，catalog digest 为 `a3e315...6244a`。
5. 完成 OAuth；授权页不应出现 callback 信任勾选或要求回到 GUI 登记 callback。
6. 新开 Chat，调用 `server_info` 和 `read_file`，确认结果来自目标 Workspace。
7. 执行一次隔离 `apply_patch`：只出现预期确认，写入只发生一次，随后回滚。
8. 在第二个 Chat/第二个 App 中验证 MCP Session cwd 和历史 Session 不串线。
9. 修改 Tool definition 后确认旧 App 使用冻结快照；显式 Refresh 后 digest 更新，不允许静默漂移。
10. 删除或禁用 App 后确认旧 Session/Token 不再产生新的调用。

## 发布判断

| 门禁 | 状态 |
|---|---|
| 高价值 Tool 成功 outputSchema | 通过 |
| Effective catalog 单一入口 | 通过 |
| Catalog snapshot | 通过 |
| Catalog property/fuzz | 通过 |
| Cloudflare Quick Tunnel 公网 MCP | 通过 |
| 公网 OAuth PKCE 与 refresh 轮换 | 通过 |
| 公网隔离写入与回滚 | 通过 |
| FRP 配置与 Supervisor | 通过 |
| 真实 FRPS 公网握手 | 阻断：缺少环境 |
| ChatGPT Web App 扫描/OAuth/读写 | 待人工 UI |
| 真实 Ctrl+C/SIGTERM tunnel 优雅清理 | 待目标平台 smoke |

因此当前可作为**自动化 RC 通过、生产发布有外部门禁**的候选版本，不能标记为 ChatGPT Apps / FRP 全链路最终验收完成。
