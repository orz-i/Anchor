# 跨平台配置迁移

Anchor CLI 提供 `export` / `import` 命令，用于在 Windows、Linux 等平台之间迁移完整配置，同时保留已有 Workspace 与 ChatGPT/GPT 注册所依赖的身份和认证材料。

等价命令入口：

```text
anchor export ...
anchor import ...

anchor config export ...
anchor config import ...
```

## 为什么不能直接复制配置目录

`profiles.json` 保存 Workspace/Profile、OAuth client ID、公开 URL 等非敏感配置；`secrets.json` 保存 OAuth password、API key、Bearer token 等敏感值。

Windows 的 `secrets.json` 使用当前用户 DPAPI 保护。把它直接复制到 Linux 后，Linux 无法解密 Windows DPAPI 密文，会出现类似错误：

```text
unsupported secret protection: windows-dpapi-current-user-v1
```

迁移命令不会复制源平台的 secret envelope。`export` 必须在**能够正常读取源配置的源平台/源用户**下执行：Anchor 先通过源平台保护机制解密 secrets，再把完整迁移 payload 放入独立的加密迁移包。`import` 在目标平台解密迁移包后，再通过目标平台原生机制保存 `secrets.json`。

因此典型迁移为：

```text
Windows DPAPI -> 加密迁移包 -> Linux private-file-permissions
Linux private-file-permissions -> 加密迁移包 -> Windows DPAPI
```

不要把 `profiles.json` / `secrets.json` 的直接复制作为跨平台迁移方案。

## 迁移包安全模型

迁移包包含完整配置和 secrets，因此必须视为敏感备份：

- payload 使用 AES-256-GCM 加密；
- 密钥通过 Argon2id 从 passphrase 派生；
- 格式版本、导出时间、源平台和源 Anchor 版本均参与 AEAD 认证，篡改会导致导入失败；
- CLI 不提供 `--passphrase <明文>` 参数，只接受 `--passphrase-file` 或 `--passphrase-stdin`，避免 passphrase 进入 shell history / 进程参数；
- passphrase 至少 12 bytes；
- Unix 上导出文件权限设置为 `0600`；
- 默认拒绝覆盖已存在的导出文件，只有显式 `--force` 才覆盖。

迁移完成后，应按敏感备份的生命周期管理迁移包和 passphrase；不再需要时安全删除。

## 源平台导出

先查看需要迁移的 Workspace ID：

```bash
anchor list
```

准备一个只由当前用户读取的 passphrase 文件，例如：

```text
/secure/anchor-migration.pass
```

然后导出：

```bash
anchor export /secure/anchor-migration.json \
  --passphrase-file /secure/anchor-migration.pass
```

也可以从 stdin 读取 passphrase：

```bash
anchor export /secure/anchor-migration.json --passphrase-stdin
```

导出成功会返回 `registrationIdentityPreserved: true` 和 Workspace 数量。迁移包本身不会以明文暴露 Workspace ID、OAuth client ID 或 secret。

## 目标平台导入

正式导入前，先停止目标机器上的 Anchor daemon、桌面端或其他会写入同一配置目录的 Anchor 进程，避免迁移过程中旧运行态再次落盘覆盖新配置。

### 1. 先准备 Workspace 目录

`import` **不会复制项目目录**。目标机器上必须先准备每个 Workspace 的实际目录。

跨平台时源路径通常不能直接使用，例如：

```text
D:\Anchor\project
```

需要映射为目标平台路径：

```text
/srv/anchor/project
```

使用重复的 `--workspace-path` 参数：

```bash
anchor import /secure/anchor-migration.json \
  --passphrase-file /secure/anchor-migration.pass \
  --workspace-path <workspace-id>=/srv/anchor/project \
  --dry-run
```

`WORKSPACE` selector 可以使用：

- Workspace ID（推荐，稳定且唯一）；
- 原始 Workspace 路径；
- 唯一 Workspace 名称。

目标路径必须满足以下条件：

- 是绝对路径；
- 已存在；
- 是目录且当前用户可访问；
- 不允许多个 Workspace 映射到同一个目标目录。

如果源路径在目标平台仍然是有效的绝对目录，可以省略该 Workspace 的映射。Windows -> Linux 等真正跨平台迁移通常需要为所有 Workspace 显式映射。

### 2. 使用 dry-run 检查

建议先执行：

```bash
anchor import /secure/anchor-migration.json \
  --passphrase-file /secure/anchor-migration.pass \
  --workspace-path <workspace-id>=/srv/anchor/project \
  --dry-run
```

`--dry-run` 会完成迁移包认证、解密、Workspace selector 解析、目标目录 canonicalize 和配置校验，但不会写入目标配置。

### 3. 正式导入

目标配置不存在时：

```bash
anchor import /secure/anchor-migration.json \
  --passphrase-file /secure/anchor-migration.pass \
  --workspace-path <workspace-id>=/srv/anchor/project
```

如果目标已经存在 Anchor 配置，默认拒绝覆盖。确认已备份并确实要用迁移包完整替换目标配置时，显式使用：

```bash
anchor import /secure/anchor-migration.json \
  --passphrase-file /secure/anchor-migration.pass \
  --workspace-path <workspace-id>=/srv/anchor/project \
  --force
```

`--force` 是**完整替换**，不是 merge。

如果你已经把 Windows 配置目录直接复制到了 Linux，并因此遇到 `unsupported secret protection: windows-dpapi-current-user-v1`，无需先让目标 Linux 成功读取这份旧 secrets；只要手上有由 Windows 源机器正确 `export` 出来的迁移包，即可使用 `import ... --force` 覆盖不兼容的目标 secret envelope。

## 哪些信息会保持不变

导入会保持迁移包中的配置身份和 secrets，不会重新注册 Workspace，也不会重新生成认证材料，包括：

- Workspace ID；
- MCP OAuth client ID；
- Actions OAuth client ID；
- OAuth password / token secret；
- Actions API key / OAuth secret；
- Bearer token；
- OAuth refresh/replay 状态；
- tunnel/public URL 配置；
- Gateway / FRP / proxy 等全局配置。

只有通过 `--workspace-path` 显式迁移的 Workspace 根目录，以及可以确定属于该 Workspace 根目录的已知路径字段，会被重映射。

这意味着：如果 ChatGPT 中已有 Connector/插件注册指向一个**稳定且迁移后保持不变的公网 URL/路由**，并且 DNS/FRP/Cloudflare named tunnel 等基础设施仍将该入口转发到新机器，通常无需重新注册插件。

但 `registrationIdentityPreserved: true` 不代表公网基础设施自动迁移。以下情况仍需要更新 ChatGPT 侧入口或重新注册：

- hostname / public URL 改变；
- 使用 Cloudflare quick tunnel，迁移或重启后 URL 改变；
- Gateway 路由或反向代理路径改变；
- OAuth callback/public routing 与原注册配置不再匹配。

## 不会被迁移的文件

迁移包只包含 Anchor 配置数据，不包含 Workspace 文件系统内容。以下内容需要另外复制/部署：

- 项目代码和 Workspace 文件；
- `.anchor/cert` 或其他 FRP TLS 证书/私钥文件；
- 自定义 runtime 可执行文件；
- Workspace 外的 skill roots；
- systemd / Windows Service / FRP 服务端 / DNS / Cloudflare 等系统或公网基础设施。

如果 FRP 证书/私钥路径位于源 Workspace 根目录内，import 会随 Workspace 根目录一起重映射该路径；目标文件不存在时会返回 warning，但不会把证书内容放入迁移包。

跨平台迁移后还应复核 `runtime_command`、`mcp_config` 等可能嵌入源平台绝对路径的自由文本字段；发现源 Workspace 路径时，import 会给出 warning。

## 迁移后验证

至少执行：

```bash
anchor list
anchor show <workspace-id>
anchor workspace gpt-config <workspace-id> --service all
anchor doctor <workspace-id>
```

需要人工确认认证材料是否完全一致时，可以在受控终端临时使用：

```bash
anchor workspace gpt-config <workspace-id> \
  --service all \
  --show-secrets
```

该命令会输出敏感值，不要把结果粘贴到日志、工单或公共聊天。
