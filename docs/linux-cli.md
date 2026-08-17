# Linux CLI

`anchor` 是面向 Linux 服务器和无桌面环境的运行入口，支持前台 `serve` 和内置后台 daemon。它与桌面端读取同一套配置模型：**一个 workspace 对应一个 WorkspaceProfile**。

## 构建

CLI 构建不启用 Tauri desktop feature：

```bash
cargo build \
  --manifest-path src-tauri/Cargo.toml \
  --release \
  --no-default-features \
  --features cli \
  --bin anchor
```

产物位于：

```text
src-tauri/target/release/anchor
```

安装到系统路径：

```bash
sudo install -m 0755 \
  src-tauri/target/release/anchor \
  /usr/local/bin/anchor
```

## 配置位置

Linux 默认使用：

```text
~/.config/anchor-desktop/data/profiles.json
~/.config/anchor-desktop/data/secrets.json
```

普通工作区配置和敏感值分别保存；两个文件均使用当前用户权限。可以通过全局参数覆盖配置根目录：

```bash
anchor --config-dir /etc/anchor list
```

不要为 CLI 创建第二套 profile。桌面端创建的 workspace/profile 可以直接由 CLI 按 ID、唯一名称或项目路径选择。

从 Windows 等其他平台迁移时，不要直接复制受平台保护的 `secrets.json`。请使用 `anchor export` / `anchor import` 让源平台解密 secrets、目标 Linux 重新按本机权限机制落盘，并通过 `--workspace-path` 映射项目目录。详见 [跨平台配置迁移](config-migration.md)。

CLI 也可以直接注册和注销 profile：

```bash
anchor workspace register /srv/projects/example --name Example
anchor workspace unregister Example --force
```

完整说明见 [Workspace CLI 注册与 GPT 连接运维](workspace-cli.md)。

## 常用命令

```bash
# 列出 workspace/profile
anchor list

# 查看配置；输出不包含 secrets.json 中的密钥
anchor show <workspace>

# 检查配置端口是否正在监听
anchor status <workspace>

# 后台启动并返回终端
anchor start <workspace> --service all --tunnel

# 查看日志和诊断
anchor logs <workspace> --service daemon
anchor doctor <workspace>

# 重启或停止后台 daemon
anchor restart <workspace>
anchor stop <workspace>

# 前台启动 MCP，Ctrl+C 优雅停止
anchor serve <workspace>

# 同时启动 MCP 与 Actions
anchor serve <workspace> --service all

# 按 profile 中的隧道配置一并启动隧道
anchor serve <workspace> --service all --tunnel

# 自动化使用结构化输出
anchor --json status <workspace>
```

`serve` 是前台常驻命令；`start` 创建 Linux 后台 daemon。若对应端口已被桌面 GUI 或其他进程占用，两种模式都会报错退出，不会停止、接管或替换现有服务。完整运维说明见 [CLI Daemon 与运维命令](cli-daemon.md)。

### 单 Gateway 多工作区

需要让多个工作区共用一条 MCP 隧道时：

```bash
anchor gateway configure --enable --port 28765 --owner PROJECT_A
anchor gateway show
anchor gateway serve PROJECT_A PROJECT_B PROJECT_C
```

`gateway serve` 在一个前台进程中管理所选工作区、Gateway 和唯一 MCP 隧道。Gateway 模式下不能再为各工作区分别使用 `start ... --service mcp`，生产环境应由 systemd 直接监督 `gateway serve`。详见 [单一 MCP Gateway 与多工作区](mcp-gateway.md)。

## systemd 用户服务

推荐由 systemd 负责后台化、重启和日志收集。先通过 `anchor list` 获取稳定的 profile ID，然后创建：

```text
~/.config/systemd/user/anchor.service
```

内容示例：

```ini
[Unit]
Description=Anchor
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/anchor serve PROFILE_ID --service mcp
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
```

加载并启动：

```bash
systemctl --user daemon-reload
systemctl --user enable --now anchor.service
systemctl --user status anchor.service
journalctl --user -u anchor.service -f
```

systemd 必须直接运行前台 `serve` 或 `gateway serve`，不要使用 `start`；内置 daemon 适合人工 SSH 运维，不替代 systemd 的开机自启和进程监督。

不要在 `/etc/profile`、`~/.profile` 或 shell rc 文件中无条件执行 `anchor restart <workspace>` 来实现开机启动。这些文件按登录/交互 shell 加载，而不是“每次系统启动只执行一次”；SSH、多终端或自动化登录可能在很短时间内并发触发多次 restart。需要随系统/用户会话启动时应使用上面的 systemd unit；若只做人工恢复，使用显式的 `anchor start` / `anchor restart`。Anchor 的 daemon 生命周期会对并发启动、旧 PID 状态和 Unix 控制 socket 缺失做安全恢复，但 shell profile 仍不应承担进程监督职责。

`serve`/`gateway serve` 运行在 systemd 等非交互环境时不会读取 shell 启动脚本。Anchor 的命令执行层会在继承的 `PATH` 之后补充当前用户常见的稳定工具链目录，包括 `~/.local/bin`、`~/.cargo/bin`、`~/.local/share/pnpm`、Volta/asdf/mise/Bun、Go，并尊重已继承的 `NVM_BIN`、fnm multishell、`PNPM_HOME`、`CARGO_HOME`、`GOBIN`/`GOROOT` 等环境变量；当 `NVM_BIN` 未继承时，还会解析 `NVM_DIR`（默认 `~/.nvm`）中的 `alias/default`，只选择该默认别名明确指向的已安装 Node 版本；没有 default alias 时，仅在本机只安装了一个 NVM Node 版本时使用该唯一版本。不会在多个未指定版本之间自行挑选。相同的有效 `PATH` 也用于未显式覆盖 `PATH` 的 stdio 下游 MCP 子进程，避免 `npx`/`node` 等启动器只因 daemon 环境较窄而整组失效。对于无法由上述确定性规则解析的 fnm/NVM 自定义选择或其他安装位置，仍应通过 service 的 `Environment=PATH=...` 显式固定。

环境诊断只会在 Docker daemon 健康、项目存在 Docker/Compose 配置且当前 runtime 命令白名单明确允许 `docker` 时推荐 Docker 验证链路。默认不会因为“检测到 Docker”就推荐一个随后会被 `exec_command` 策略拒绝的路径；需要启用 Docker 命令时，应由操作者在 Workspace runtime 的 `allowed_commands` 中显式加入并接受 Docker daemon 带来的额外宿主机信任边界。

需要在退出登录后继续运行时，可为该 Linux 用户启用 linger：

```bash
sudo loginctl enable-linger "$USER"
```

## GUI 与 CLI 并用

- Anchor 只读取当前配置目录和受保护的 `secrets.json` 封装；早期产品目录、明文凭据和旧配置布局不会自动导入。需要保留的工作区应在当前版本中重新注册。
- 可以共用同一个配置目录和 workspace/profile。
- 配置文件写入有跨进程锁和最近有效备份。
- 不要同时用 GUI 和 CLI 启动同一个 workspace 的同一种服务；端口检查会阻止重复启动。
- CLI 默认不启动隧道，只有显式传入 `--tunnel` 才使用 profile 中已保存的隧道配置。
- 修改配置后，应重启负责运行该 workspace 的 GUI 服务或 systemd 服务。

## Agent Skills

Linux CLI 启动 MCP 时会读取同一个 WorkspaceProfile 中的 Skill 服务配置，不需要额外参数：

```bash
anchor serve PROFILE_ID --service mcp
```

确保 systemd 服务用户能够读取 profile 配置的 Skill 根目录。MCP 会稳定发布一个只读 `skill` facade tool（`list` / `get` / `read_resource`），供 ChatGPT Developer Mode 等以 `tools/list` 为可靠发现入口的宿主使用；四个旧 Skill helper 仅保留缓存客户端兼容，不再单独占工具槽。与此同时，MCP 仍声明 `io.modelcontextprotocol/skills` extension，并提供 `skills/list`、`skills/get` 和 `skill://anchor/<skill-name>/...` resources，作为兼容该扩展宿主的标准方向。若需要 ChatGPT Plugin 原生 Skill UI，可另用 `anchor plugin package PROFILE_ID --app-id plugin_asdk_app...` 生成静态 Plugin 快照。Skill 脚本只可读取、不会执行。详细说明见 [MCP Agent Skills 服务](skill-service.md)。

## 自动恢复

`serve` 会持续检测 MCP/Actions listener，而不是只等待 `Ctrl+C`：

- listener 意外退出后最多自动恢复五次；
- `starting` 超过 10 秒会进入恢复状态；
- 隧道重连采用指数退避，最高间隔 60 秒；
- `--json` 输出结构化恢复事件；
- 本地服务恢复耗尽后优雅停止，并以非零状态退出，适合 systemd `Restart=on-failure`。

完整行为见 [连接恢复、自动重试与 OAuth 续约](reliability.md)。
