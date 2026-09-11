# Linux CLI

`anchor` 是面向 Linux 服务器和无图形环境的运行入口，支持前台 `serve`、内置后台 daemon 和原生 systemd-user control-plane。CLI 与浏览器 Web Admin 读取同一套配置模型：**一个 workspace 对应一个 WorkspaceProfile**。

## 构建

当前 Rust 产品只有 CLI/Web Admin 路径，不再包含 Tauri desktop feature：

```bash
cargo build \
  --manifest-path crates/anchor/Cargo.toml \
  --release \
  --no-default-features \
  --features cli \
  --bin anchor
```

产物位于：

```text
crates/anchor/target/release/anchor
```

安装到系统路径：

```bash
sudo install -m 0755 \
  crates/anchor/target/release/anchor \
  /usr/local/bin/anchor
```

## 配置位置

Linux 默认使用：

```text
~/.config/anchor/data/profiles.json
~/.config/anchor/data/secrets.json
```

普通工作区配置和敏感值分别保存；两个文件均使用当前用户权限。可以通过全局参数覆盖配置根目录：

```bash
anchor --config-dir /etc/anchor list
```

不要为 CLI 创建第二套 profile。浏览器 Web Admin 创建的 workspace/profile 可以直接由 CLI 按 ID、唯一名称或项目路径选择。

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

# 后台启动并返回终端；已启用的顶级 Tunnel 会由 daemon 自动托管
anchor start <workspace> --service all

# 查看日志和诊断
anchor logs <workspace> --service daemon
anchor doctor <workspace>

# 重启或停止后台 daemon
anchor restart <workspace>
anchor stop <workspace>

# 把运行中的 runtime 升级到当前 CLI build；先 dry-run
anchor upgrade --all --dry-run
anchor upgrade --all

# 前台启动 MCP，Ctrl+C 优雅停止
anchor serve <workspace>

# 同时启动 MCP
anchor serve <workspace> --service all

# 如需公网 Tunnel，单独创建/配置/启用
anchor tunnel create <workspace> --name example-mcp
anchor tunnel configure example-mcp --type cloudflare --cloudflare-mode named
anchor tunnel enable example-mcp
anchor serve <workspace> --service all

# 自动化使用结构化输出
anchor --json status <workspace>
```

`serve` 是前台常驻命令；`start` 创建 Linux 后台 daemon。若对应端口已被其他 Anchor runtime 或外部进程占用，两种模式都会报错退出，不会停止、接管或替换现有服务。完整运维说明见 [CLI Daemon 与运维命令](cli-daemon.md)。

当前 Linux runtime 支持 `anchor upgrade`。如果所选 Workspace/Gateway 由 Anchor 的 systemd-user control-plane 持有，upgrade 会识别 supervisor ownership 并刷新 service executable/build plan，再由 systemd reconcile desired state；不会先停 daemon 再和旧 service plan 竞争拉起。显式选择只覆盖 supervisor desired set 的一部分时会 fail-closed，优先使用 `anchor upgrade --all` 完整升级受管集合。

### 单 Gateway 多工作区

需要让多个工作区共用一条 MCP 隧道时：

```bash
anchor gateway configure --enable --port 28765 --tunnel TUNNEL_ID_OR_NAME
anchor gateway show
anchor gateway serve PROJECT_A PROJECT_B PROJECT_C
```

`gateway serve` 在一个前台进程中管理所选工作区、Gateway 和唯一 MCP 隧道。Gateway 模式下不能再为各工作区分别使用 `start ... --service mcp`。常规服务器推荐把 Gateway/Workspace desired state 交给 `anchor service install` 的 systemd-user control-plane；`gateway serve` 主要用于调试、容器或外部 supervisor。详见 [单一 MCP Gateway 与多工作区](mcp-gateway.md)。

如果使用 Anchor 原生 systemd-user control-plane，则先用正常 Gateway/Workspace 管理命令形成 desired state，再执行 `anchor service install`；不需要额外再写一个监督 `gateway serve` 的 unit。只有容器、外部 supervisor 或刻意采用 foreground 模式时，才需要让外部 systemd 直接监督 `gateway serve`。

## systemd 用户服务

Linux 现在由 Anchor 原生维护配置域专属的 systemd user control-plane service。先按需要启动 Workspace/Gateway，让 desired state 写入 service plan，然后安装：

```bash
anchor start Anchor --service mcp
anchor service install
anchor service status
```

`anchor service install` 会：

- 捕获当前 Workspace/Gateway 后台运行计划；
- 在 `~/.config/systemd/user/` 写入当前配置域专属 unit；
- `enable` 并启动该 unit；
- 自动启用当前 Linux 用户的 `linger`，使退出登录及节点重启后 user manager 仍可启动；
- 由一个长期 control-plane service 按 plan 恢复并监督 Workspace/Gateway daemon，而不是在 shell profile 中重复执行 `restart`。

以后通过 `anchor start` / `anchor stop` / `anchor restart`、Gateway route/config 管理或 Workspace 注销产生的 desired state 会同步到同一 plan。若需要把“当前实际正在运行的集合”覆盖为下一次启动计划，可显式执行 `anchor service sync`。

`anchor service status` 会同时报告 unit 是否 installed/enabled/running、plan、注册时的 build identity 和当前 CLI build。**更新源码并不等于更新已安装的 `/usr/local/bin/anchor`。** 应先确保自己正在运行目标新 build（常规做法是先替换 `/usr/local/bin/anchor`），然后可以：

```bash
# 推荐：预检后一次升级 systemd-owned desired set
anchor upgrade --all --dry-run
anchor upgrade --all

# 也可显式刷新/重装 service registration
anchor service install
```

当正在运行的 systemd-user service 持有所选 runtime 时，`anchor upgrade` 会自动走 supervisor-aware lifecycle，而不是执行普通 bounded-outage daemon replacement。`--dry-run` 会在结果中报告 supervisor plan；若只选中了 service desired set 的一部分，则返回 `SUPERVISOR_UPGRADE_SCOPE_MISMATCH`，要求完整选择受管集合。

在 SSH、自动化 shell 或 root 登录中，PAM 可能没有注入 `XDG_RUNTIME_DIR`，即使 `user@<uid>.service` 已运行，裸 `systemctl --user` 也会报 `Failed to connect to bus: No medium found`。Anchor 会按当前有效 UID 显式使用 `/run/user/<uid>` 连接 systemd user manager，并忽略可能陈旧的 `DBUS_SESSION_BUS_ADDRESS`，因此正常情况下无需手工 `export XDG_RUNTIME_DIR=/run/user/0`。如果 user manager 本身不可用，错误会报告 UID、期望 runtime dir，并提示检查 `systemd-logind` / `user@<uid>.service`。

不要在 `/etc/profile`、`~/.profile` 或 shell rc 文件中无条件执行 `anchor restart <workspace>` 来实现开机启动。这些文件按登录/交互 shell 加载，而不是“每次系统启动只执行一次”；SSH、多终端或自动化登录可能在很短时间内并发触发多次 restart。需要随节点启动时使用 `anchor service install`；若只做人工恢复，使用显式的 `anchor start` / `anchor restart`。

control-plane service 与它拉起的 daemon 运行在 systemd 非交互环境，不会读取 shell 启动脚本。Anchor 的命令执行层会在继承的 `PATH` 之后补充当前用户常见的稳定工具链目录，包括 `~/.local/bin`、`~/.cargo/bin`、`~/.local/share/pnpm`、Volta/asdf/mise/Bun、Go，并尊重已继承的 `NVM_BIN`、fnm multishell、`PNPM_HOME`、`CARGO_HOME`、`GOBIN`/`GOROOT` 等环境变量；当 `NVM_BIN` 未继承时，还会解析 `NVM_DIR`（默认 `~/.nvm`）中的 `alias/default`，只选择该默认别名明确指向的已安装 Node 版本；没有 default alias 时，仅在本机只安装了一个 NVM Node 版本时使用该唯一版本。不会在多个未指定版本之间自行挑选。

环境诊断只会在 Docker daemon 健康、项目存在 Docker/Compose 配置且当前 runtime 命令白名单明确允许 `docker` 时推荐 Docker 验证链路。默认不会因为“检测到 Docker”就推荐一个随后会被 `exec_command` 策略拒绝的路径；需要启用 Docker 命令时，应由操作者在 Workspace runtime 的 `allowed_commands` 中显式加入并接受 Docker daemon 带来的额外宿主机信任边界。

`anchor service install` 会自动处理 linger；如需人工复核：

```bash
sudo loginctl enable-linger "$USER"
```

## Web Admin 与 CLI 并用

- Anchor 只读取当前配置目录和受保护的 `secrets.json` 封装；早期产品目录、明文凭据和旧配置布局不会自动导入。需要保留的工作区应在当前版本中重新注册。
- 可以共用同一个配置目录和 workspace/profile。
- 配置文件写入有跨进程锁和最近有效备份。
- 不要通过不同入口并发启动同一个 workspace 的同一种服务；control-plane/端口 ownership 检查会阻止重复接管。
- Tunnel 是顶级资源；Workspace daemon 是否自动托管由 `anchor tunnel enable/disable` 决定，公开的 Workspace `--tunnel` 参数已移除。
- 修改配置后应通过当前 Workspace daemon/control-plane 的受控 apply/reload/restart 路径生效；不要另起第二个 listener 试图覆盖活动运行态。

## Agent Skills

Linux CLI 启动 MCP 时会读取同一个 WorkspaceProfile 和 workspace-local `.anchor/skills` package store，不需要 Skill root 参数：

```bash
anchor serve PROFILE_ID --service mcp
```

`skill` 始终只占一个一级 MCP tool。`read-only` / `core` 可用 `list`、`get`、`read_resource`、`packages`、`validate`；`advanced` 另可 `install`、`set_channel`、`activate`、`rollback`、`remove`。内部 operation handler 不作为独立工具发布。与此同时，MCP 仍声明 `io.modelcontextprotocol/skills` extension，并提供 `skills/list`、`skills/get` 和 `skill://anchor/<skill-name>/...` resources；若需要 ChatGPT Plugin 静态快照，可使用 `anchor plugin package PROFILE_ID --app-id plugin_asdk_app...`。完整 package/channel/activation、安全与脚本 snapshot 边界见 [Agent Skill package lifecycle](skill-service.md)。

## 自动恢复

`serve` 会持续检测 MCP listener，而不是只等待 `Ctrl+C`：

- listener 意外退出后最多自动恢复五次；
- `starting` 超过 10 秒会进入恢复状态；
- 隧道重连采用指数退避，最高间隔 60 秒；
- `--json` 输出结构化恢复事件；
- 本地服务恢复耗尽后优雅停止，并以非零状态退出，适合 systemd `Restart=on-failure`。

完整行为见 [连接恢复、自动重试与 OAuth 续约](reliability.md)。
