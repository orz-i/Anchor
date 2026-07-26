# Anchor 更名与升级迁移

## 新标识

| 项目 | Anchor 标识 |
|---|---|
| 产品名 | `Anchor` |
| MCP server name | `anchor` |
| Actions service name | `anchor-actions` |
| CLI | `anchor` |
| 桌面可执行文件 | `anchor-desktop` |
| Rust package | `anchor` |
| Rust library crate | `anchor_lib` |
| NPM package | `anchor-desktop` |
| 配置目录 | `anchor` |
| 配置目录环境变量 | `ANCHOR_CONFIG_DIR` |
| Tauri bundle identifier | `com.anchor.desktop` |

## 自动迁移

首次使用新配置目录启动 Anchor 时，如果新目录尚无 profiles/secrets，应用会从旧品牌目录复制以下数据：

- `data/profiles.json` 与备份；
- `data/secrets.json` 与备份；
- 旧版顶层 `profiles.json`、`app_settings.json`；
- 已缓存的 `bin/` 软件；
- 受管 `frpc/` 配置和 PID 记录。

迁移是复制操作，不删除旧目录，不复制日志和 daemon runtime。目录复制有文件数量与总字节上限，并跳过符号链接。新目录已有配置时不会覆盖。

Linux 上，`anchor status/stop` 会读取新旧 daemon runtime 目录，并能发现仍以旧命令运行的 daemon；新启动只写 Anchor runtime 目录。Windows 单实例锁和 macOS 端口归属检查同时识别 Anchor 与旧版桌面进程，避免升级期间两个版本同时管理同一 Workspace/FRP 进程。

## 兼容入口

- `coding-tools-mcp` 暂时作为弃用 CLI 别名保留，调用时会输出迁移提示并执行 Anchor CLI。
- `CODING_TOOLS_MCP_CONFIG_DIR` 暂时作为 `ANCHOR_CONFIG_DIR` 的后备环境变量读取。
- 旧配置目录名、旧 Bundle ID 和旧桌面可执行文件名只用于迁移与安全归属检查，不再作为展示品牌。

新脚本、服务和文档应使用 `anchor` 与 `ANCHOR_CONFIG_DIR`。

## MCP 与 ChatGPT App

MCP `initialize.serverInfo` 和 `server_info` 现在返回：

```json
{
  "name": "anchor",
  "title": "Anchor"
}
```

`server_info` 的公开 output schema 常量同步变化，因此 effective Tool catalog digest 会更新。已经冻结 Tool snapshot 的 ChatGPT App 必须显式 Refresh；OAuth callback URL 和 PKCE 流程不因品牌变化而改变。

## 外部仓库地址

代码中的产品、包、运行时和文档品牌均改为 Anchor。现有 GitHub 仓库 slug 仍可能保留旧路径以保证 Release 和 badge 链接可用；仓库在 GitHub 侧完成重命名后，再更新这些外部 URL。
