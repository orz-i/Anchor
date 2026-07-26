# Workspace CLI 注册与 GPT 连接运维

`anchor workspace` 将 WorkspaceProfile 的生命周期集中到一个命令组：注册、注销、查看、后台启停、输出 GPT 连接配置和连接测试。

数据模型保持不变：**一个 workspace 目录对应一个 WorkspaceProfile**。注册和注销只修改 Anchor 的配置，不创建、移动或删除项目文件。

## 命令概览

```bash
anchor workspace list
anchor workspace register PATH [--name NAME]
anchor workspace unregister WORKSPACE --force
anchor workspace show WORKSPACE
anchor workspace start WORKSPACE [--service mcp|actions|all] [--tunnel]
anchor workspace stop WORKSPACE [--timeout SECONDS] [--force]
anchor workspace gpt-config WORKSPACE [--service mcp|actions|all]
anchor workspace test WORKSPACE [--service mcp|actions|all]
```

别名：

```text
workspace add       = workspace register
workspace delete    = workspace unregister
workspace remove    = workspace unregister
workspace view      = workspace show
workspace ls        = workspace list
ws                   = workspace
```

全局 `--json` 适用于所有子命令。

## 注册

```bash
anchor workspace register /srv/projects/example --name Example
```

注册流程：

1. 解析并 canonicalize 项目目录；
2. 同一路径已注册时返回 `already_registered`，不创建重复 profile；
3. 名称必须唯一，避免名称选择器产生歧义；
4. 分配不与其他 profile 冲突、且当前没有进程监听的 MCP/Actions 端口；
5. 创建 WorkspaceProfile；
6. 初始化 OAuth Password、Token Secret、Bearer Token、Actions API Key 等 workspace 密钥；
7. profile 与密钥在一次配置保存中持久化。

默认 OAuth Client Secret 不自动生成，因为 MCP OAuth 支持公开 PKCE 客户端。

输出示例：

```json
{
  "event": "registered",
  "workspace": {
    "id": "...",
    "name": "Example",
    "path": "/srv/projects/example"
  },
  "mcpPort": 28768,
  "actionsPort": 8789,
  "projectFilesDeleted": false,
  "warnings": []
}
```

## 注销

注销是破坏性配置操作，必须显式确认：

```bash
anchor workspace unregister Example --force
```

行为：

- 删除 WorkspaceProfile 和关联 workspace 密钥；
- 清理 CLI daemon 状态和该 profile 的受管隧道状态；
- CLI daemon 正在运行时先优雅停止，超时后允许终止已验证的 daemon 进程树；
- 不删除项目目录、Git 仓库或源码；
- 不停止 GUI 或其他外部 PID；
- 若配置端口恰好被外部进程监听，只在结果中返回 warning。

`projectFilesDeleted` 始终为 `false`。

## 查看

```bash
anchor workspace list
anchor workspace show Example
```

`show` 输出 profile 配置，不读取或输出独立密钥文件。历史内联的 Cloudflare Token 也会被移除。

## 启动与停止

```bash
anchor workspace start Example --service all --tunnel
anchor workspace stop Example
```

这两个命令是顶层 `start/stop` 的 workspace 级别名，管理 Linux CLI daemon。Windows/macOS 使用 GUI 或 `serve` 前台模式。

完整 daemon 行为见 [CLI Daemon 与运维命令](cli-daemon.md)。

## GPT 连接配置

### MCP Connector

```bash
anchor workspace gpt-config Example
```

默认输出 MCP Connector 配置，包括：

- Connector URL；
- URL 来源：`local` 或 `public`；
- 认证类型；
- OAuth Client ID；
- Authorization URL、Token URL；
- OAuth metadata URL；
- Scope；
- 密钥是否已配置。

### GPT Actions

```bash
anchor workspace gpt-config Example --service actions
```

输出：

- OpenAPI Schema URL；
- Privacy Policy URL；
- API Key 或 OAuth 配置；
- GPT Editor 中的配置入口提示。

### 同时查看

```bash
anchor workspace gpt-config Example --service all --public
```

Endpoint 选择：

```text
--endpoint auto     有公网 URL 时优先公网，否则使用 localhost
--endpoint local    强制 localhost
--endpoint public   强制公网；未配置时返回错误
--local             --endpoint local 的别名
--public            --endpoint public 的别名
```

### 密钥脱敏

默认不会输出密钥值：

```json
{
  "clientSecret": {
    "available": true,
    "value": null
  }
}
```

确需复制完整配置时显式执行：

```bash
anchor workspace gpt-config Example --show-secrets
```

该输出可能包含 OAuth Client Secret、Authorization Password、Bearer Token 或 Actions API Key。不要写入日志、CI artifact、工单或公共聊天。

## 连接测试

```bash
anchor workspace test Example --service all --public --timeout 15
```

### MCP 检查

- `GET /mcp` 返回规范的 `405 Method Not Allowed`；
- `Allow` 包含 `POST`；
- OAuth Authorization Server Metadata；
- OAuth Protected Resource Metadata；
- OAuth 模式下未认证 initialize 返回 `401` 和 `resource_metadata` challenge；
- Bearer/无认证模式下实际发送 JSON-RPC initialize，并验证 `serverInfo`。

### Actions 检查

- `/health` 返回 200；
- `/openapi.json` 返回 OpenAPI 文档；
- `/privacy` 可访问；
- API Key 模式使用已保存 Bearer Key 实际调用只读 `server_info`；
- 无认证模式实际调用只读 `server_info`；
- OAuth 模式验证 metadata 和未认证 401 challenge。

测试内部可以使用已保存密钥，但报告不会输出密钥值。

退出码：

```text
0  所有选择的检查通过
1  至少一项检查失败
2  CLI 参数错误
```

`--json` 返回逐项 `checks`，适合 CI 和运维脚本。

## 推荐工作流

```bash
# 1. 注册
anchor workspace register /srv/projects/example --name Example

# 2. 查看配置
anchor workspace show Example

# 3. 后台启动
anchor workspace start Example --service all --tunnel

# 4. 测试本地服务
anchor workspace test Example --service all --local

# 5. 查看 GPT 公网配置
anchor workspace gpt-config Example --service all --public

# 6. 测试公网连接
anchor workspace test Example --service all --public

# 7. 停止
anchor workspace stop Example
```

