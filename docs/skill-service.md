# Agent Skill package lifecycle

Anchor 的 Skill 运行时使用**不可变 package + channel + active snapshot** 模型。workspace 中的普通目录只是显式 `validate/install` 的输入；目录出现、修改或删除都不会自动改变运行中的 Skill。

核心状态位于：

```text
<workspace>/.anchor/skills/
├── state.json
├── state.lock
└── packages/
    └── <skill-name>/
        └── <sha256>/
            └── <skill-name>/
                ├── SKILL.md
                ├── references/
                ├── scripts/
                └── assets/
```

`state.json` 是 canonical lifecycle state。package 目录以完整内容摘要寻址；`stable`、`development`、`canary`、`pinned` 是指向已安装摘要的 channel。每个 Skill 只有一个 active package；切换 active package 会记录有界 rollback history。

## 硬切行为

P1 不保留旧目录扫描兼容路径：

- 不再配置或读取 `runtime.skill_roots`；旧 profile 中仍含该字段时，严格配置反序列化会直接拒绝，不会静默忽略或自动迁移。
- 不再默认扫描 `.agents/skills`、`.codex/skills`、`skills` 或 home/external roots。
- 不再因为源目录变化自动刷新 Skill runtime。
- 不提供旧 operation alias、双写 package/root 状态或 fallback scan。
- Plugin package、MCP Skill extension、`skill` facade 全部只消费 active packages。

迁移方式是显式安装，而不是复制旧配置：

```bash
anchor skill validate PROFILE_ID skills/code-review
anchor skill install PROFILE_ID skills/code-review --channel stable --activate
```

`.agents/skills/...` 或 `.codex/skills/...` 仍可以作为**workspace 内的显式安装源目录**，但它们不再具有运行时特殊语义。

## Skill package 格式

源目录必须包含 `SKILL.md`，并满足现有 Skill parser 与资源安全约束。`name` 必须与源目录名一致：

```markdown
---
name: code-review
description: Review a code change for correctness, security, and regressions.
allowed-tools: read_file git search
metadata:
  version: "1.2.0"
---

# Code review

Read the changed files and report concrete findings with file references.
```

`metadata.version` 可选。若存在，它是不可变的声明版本：同一个 Skill 的同一 declared version 不能重新绑定到不同 package digest；需要变更内容时必须使用新版本号，或直接使用完整 `sha256:...` 标识版本。

支持目录：

```text
<skill-name>/
├── SKILL.md
├── references/
├── scripts/
└── assets/
```

安装前会复用 Anchor 现有的 frontmatter、manifest、路径、文件大小、摘要和敏感文件排除校验。符号链接、未进入受控 manifest 的额外文件、不可读取资源或超限资源都会阻止安全 package 安装。

## Lifecycle

### Validate

只检查 workspace 内源目录，不写 package store：

```bash
anchor skill validate PROFILE_ID skills/code-review
```

返回 Skill name、完整 package digest、declared version、文件数量、总字节数和 quality warnings。

### Install

```bash
anchor skill install PROFILE_ID skills/code-review --channel development
anchor skill install PROFILE_ID skills/code-review --channel stable --activate
```

安装流程：

1. 校验源目录和完整 manifest；
2. 复制到 store 内 staging；
3. 原子发布到 content-addressed package 目录；
4. 在跨进程 `state.lock` 下重新读取 canonical state；
5. 原子更新 channel / optional active pointer；
6. 原子写回 `state.json`。

安装失败不会把不完整 package 作为 active runtime 暴露。

### Channel

四个 channel：

- `stable`
- `development`
- `canary`
- `pinned`

将 channel 指向一个已安装 declared version 或完整 digest：

```bash
anchor skill set-channel PROFILE_ID code-review canary 1.3.0
anchor skill set-channel PROFILE_ID code-review pinned sha256:<64-hex>
```

移动 channel **不会自动切换 active package**。

### Activate

```bash
anchor skill activate PROFILE_ID code-review canary
```

激活只接受已有 channel；当前 active `(channel,digest)` 被写入 bounded history，随后原子切换 active pointer。

运行中的 MCP 不扫描源目录。它只检测 canonical `state.json` 是否变化；检测到 package lifecycle 状态更新后重建 active package snapshot。

### Rollback

```bash
anchor skill rollback PROFILE_ID code-review
```

回到最近仍存在的 active package。没有 rollback target 时失败，不做 silent fallback。

### Remove

```bash
anchor skill remove PROFILE_ID code-review 1.2.0
```

以下情况拒绝删除：

- 版本当前 active；
- 任一 channel 仍指向该版本。

删除时先把 package tree 移入 store 内 `.trash`，成功持久化 lifecycle state 后再清理；持久化失败会恢复 package tree。

## MCP `skill` facade

对外仍只有一个一级 `skill` tool，不会为 package/version 增加 `tools/list` 条目。

`read-only` 与 `core` 可用：

- `list`
- `get`
- `read_resource`
- `packages`
- `validate`

`advanced` 额外开放：

- `install`
- `set_channel`
- `activate`
- `rollback`
- `remove`

示例：

```json
{
  "operation": "install",
  "path": "skills/code-review",
  "channel": "canary",
  "activate": false
}
```

```json
{
  "operation": "activate",
  "name": "code-review",
  "channel": "canary"
}
```

内部 lifecycle leaves 只是 facade implementation，不进入公开 `tools/list`，也不能作为兼容直调入口使用。

## Native MCP Skills extension

Skill 服务启用时 Anchor 继续声明 `io.modelcontextprotocol/skills`，并实现：

- `skills/list`
- `skills/get`
- `resources/list`
- `resources/read`

这些协议端点只导出 active packages。canonical URI 仍为：

```text
skill://anchor/<name>/SKILL.md
skill://anchor/<name>/references/<file>
skill://anchor/<name>/scripts/<file>
skill://anchor/<name>/assets/<file>
```

manifest 中的每个可读文件都有文件级 SHA-256。资源读取会再次 canonicalize 路径并复核当前文件 digest；package 文件被替换或篡改时读取失败，不会把变化后的内容当作已批准 snapshot。

## ChatGPT/Codex Plugin package

`anchor plugin package` 只快照**当前 active packages**：

```bash
anchor plugin package PROFILE_ID --app-id plugin_asdk_app_xxx
```

未安装、只安装未激活、或仅存在于 source directory 的 Skill 都不会进入 Plugin package。

默认输出：

```text
<workspace>/.anchor/chatgpt-plugin-marketplace/
├── marketplace.json
└── plugins/
    └── anchor-<workspace>/
        ├── .codex-plugin/plugin.json
        ├── .app.json
        └── skills/<active-skill>/...
```

active package 切换后需要重新执行 `anchor plugin package` 才会更新静态 Plugin bundle。

## Web Admin

Workspace 的 **Agent Skills** 面板提供同一 canonical package lifecycle：

- 开关 Skill runtime；
- 从 workspace-local path 安装 package；
- 选择 install channel 和 optional immediate activation；
- 设置 channel；
- 激活 channel；
- rollback；
- 删除 inactive/unreferenced version；
- 查看 installed versions、channel pointers、active digest 和 rollback depth。

Web Admin 与 CLI/MCP 不维护第二套状态；三者都操作 `.anchor/skills/state.json`。

## 安全边界

- Skill 内容仍只是指令和依赖元数据，不授予权限。
- `allowed-tools` 不会扩大 Anchor 的 tool profile、command allowlist、危险操作门禁或 workspace 权限。
- package source 必须位于当前 workspace；不接受 home/external source path。
- managed `.anchor/skills` 不能再次作为 install source，避免递归/自引用 package。
- package install 拒绝符号链接。
- store lifecycle mutation 使用跨进程文件锁；`state.json` 使用原子替换。
- store 状态损坏时 fail closed：active catalog 为空并报告错误，mutation 不会覆盖损坏状态。
- active scripts 延续已有 snapshot digest 校验；文件发生变化后不能借由原有授权继续执行。
- resources 读取持续执行 manifest + digest 校验。

## 运维

启动 MCP 不需要 Skill root 参数：

```bash
anchor serve PROFILE_ID --service mcp
```

唯一 profile 开关是 `runtime.skill_service_enabled`。Skill package 生命周期独立存放在 workspace `.anchor/skills` 中，因此 portable profile 配置不再携带 directory scan roots。
