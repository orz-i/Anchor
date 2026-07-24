# Linux CLI

`coding-tools-mcp` 是面向 Linux 服务器和无桌面环境的前台运行入口。它与桌面端读取同一套配置模型：**一个 workspace 对应一个 WorkspaceProfile**。

## 构建

CLI 构建不启用 Tauri desktop feature：

```bash
cargo build \
  --manifest-path src-tauri/Cargo.toml \
  --release \
  --no-default-features \
  --features cli \
  --bin coding-tools-mcp
```

产物位于：

```text
src-tauri/target/release/coding-tools-mcp
```

安装到系统路径：

```bash
sudo install -m 0755 \
  src-tauri/target/release/coding-tools-mcp \
  /usr/local/bin/coding-tools-mcp
```

## 配置位置

Linux 默认使用：

```text
~/.config/coding-tools-mcp-desktop/data/profiles.json
~/.config/coding-tools-mcp-desktop/data/secrets.json
```

普通工作区配置和敏感值分别保存；两个文件均使用当前用户权限。可以通过全局参数覆盖配置根目录：

```bash
coding-tools-mcp --config-dir /etc/coding-tools-mcp list
```

不要为 CLI 创建第二套 profile。桌面端创建的 workspace/profile 可以直接由 CLI 按 ID、唯一名称或项目路径选择。

## 常用命令

```bash
# 列出 workspace/profile
coding-tools-mcp list

# 查看配置；输出不包含 secrets.json 中的密钥
coding-tools-mcp show <workspace>

# 检查配置端口是否正在监听
coding-tools-mcp status <workspace>

# 前台启动 MCP，Ctrl+C 优雅停止
coding-tools-mcp serve <workspace>

# 同时启动 MCP 与 Actions
coding-tools-mcp serve <workspace> --service all

# 按 profile 中的隧道配置一并启动隧道
coding-tools-mcp serve <workspace> --service all --tunnel

# 自动化使用结构化输出
coding-tools-mcp --json status <workspace>
```

`serve` 是前台常驻命令，不会脱离终端自行变成 daemon。若对应端口已被桌面 GUI 或其他进程占用，CLI 会报错退出，不会停止、接管或替换现有服务。

## systemd 用户服务

推荐由 systemd 负责后台化、重启和日志收集。先通过 `coding-tools-mcp list` 获取稳定的 profile ID，然后创建：

```text
~/.config/systemd/user/coding-tools-mcp.service
```

内容示例：

```ini
[Unit]
Description=Coding Tools MCP
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/coding-tools-mcp serve PROFILE_ID --service mcp
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
```

加载并启动：

```bash
systemctl --user daemon-reload
systemctl --user enable --now coding-tools-mcp.service
systemctl --user status coding-tools-mcp.service
journalctl --user -u coding-tools-mcp.service -f
```

需要在退出登录后继续运行时，可为该 Linux 用户启用 linger：

```bash
sudo loginctl enable-linger "$USER"
```

## GUI 与 CLI 并用

- 从旧版首次升级时，先停止仍在运行的旧 GUI，再执行一次新版 GUI 或 CLI 命令完成 `secrets.json` 迁移。旧进程不认识新的跨进程锁，不应与首次迁移并行。
- 可以共用同一个配置目录和 workspace/profile。
- 配置文件写入有跨进程锁和最近有效备份。
- 不要同时用 GUI 和 CLI 启动同一个 workspace 的同一种服务；端口检查会阻止重复启动。
- CLI 默认不启动隧道，只有显式传入 `--tunnel` 才使用 profile 中已保存的隧道配置。
- 修改配置后，应重启负责运行该 workspace 的 GUI 服务或 systemd 服务。
