# Anchor 全面更名验证（2026-07-27）

## 标识结果

- 产品与窗口标题：`Anchor`
- MCP `serverInfo.name`：`anchor`
- MCP `serverInfo.title`：`Anchor`
- Actions service：`anchor-actions`
- 主 CLI：`anchor`
- 桌面可执行文件：`anchor-desktop`
- Rust package：`anchor`
- Rust library：`anchor_lib`
- NPM package：`anchor-desktop`
- 配置环境变量：`ANCHOR_CONFIG_DIR`
- Tauri bundle identifier：`com.anchor.desktop`

旧 CLI、环境变量、配置目录、Bundle ID 和 macOS 可执行文件名仅作为迁移/归属兼容标识保留。

## 升级兼容

- 新配置目录为空时，从旧品牌目录复制 profiles、secrets、软件缓存和受管 FRP 状态；不删除旧目录。
- 迁移跳过符号链接，并限制文件数量和总字节数。
- Linux `status/stop` 同时读取新旧 daemon runtime 目录；新 daemon 只写 Anchor 目录。
- Windows 同时持有 Anchor 和旧版单实例 mutex。
- macOS 端口归属识别 Anchor 与旧版 App bundle。
- `coding-tools-mcp` 作为弃用别名执行 Anchor CLI。
- `CODING_TOOLS_MCP_CONFIG_DIR` 作为 `ANCHOR_CONFIG_DIR` 后备读取。

## Tool catalog

Tool 数量未变化，`server_info.server` 的公开常量改为 `anchor`，因此 digest 更新：

| Profile | Tool 数 | Digest |
|---|---:|---|
| `core` | 26 | `4dd2cf70826b6ac00b1e84950da64d2c54195eb3ec3e67c0fff8e4a16965f200` |
| `read-only` | 18 | `661741def400a7e3442b8cce998884d327ec8401e21b2afef470e266d19d6031` |
| `advanced` | 39 | `2719453b85026d5d986243c7ff219d2f222f1b523624fefc229ece55ec8a3b9d` |

冻结 Tool snapshot 的 ChatGPT App 需要显式 Refresh。

## 自动化门禁

- Rust library：251 passed，1 ignored。
- Rust integration：78 passed。
- 严格 Clippy：all targets/all features 通过。
- CLI-only Clippy：两个 CLI binary 通过。
- Svelte check：0 errors，0 warnings。
- 前端生产构建：通过。
- `anchor --version`：返回 `anchor 0.1.23`。
- 旧 CLI 别名：输出弃用提示并返回 `anchor 0.1.23`。
- 配置目录复制迁移测试：通过。
- 新旧 daemon runtime 路径测试：通过。
- 新旧 macOS bundle 识别测试：通过。

## 公网 MCP

两套验证均使用隔离配置、临时 Workspace、端口 `28768` 和临时 Cloudflare Quick Tunnel。验证完成后端口已释放，目标为 `127.0.0.1:28768` 的 cloudflared 已清理；未操作 ChatGPT Workspace 网页界面。

无认证证据：`anchor-rename-remote-noauth-2026-07-27.json`。

通过：

- GET/Origin 负例；
- MCP `2025-11-25` initialize；
- `serverInfo = anchor / Anchor`；
- 26 个 Tool 和新 core digest；
- `server_info`、`read_file`；
- 隔离写入和回滚；
- Session DELETE 204。

OAuth 证据：`anchor-rename-remote-oauth-2026-07-27.json`。

通过：

- Authorization Server / Protected Resource Metadata；
- 401 resource metadata challenge；
- 授权页；
- ChatGPT 内置 callback 返回 303；
- S256 PKCE 和 token exchange；
- Anchor MCP Session 与 Tool catalog；
- refresh token rotation；
- 刷新后的 access token 返回 `server=anchor`；
- 旧 refresh token replay 返回 `invalid_grant`；
- Session DELETE 204。

报告中不包含 Token、密码、Client Secret 或私钥。
