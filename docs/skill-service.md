# MCP Agent Skills 服务

Coding Tools MCP 可以从当前 workspace/profile 配置的目录中发现 Agent Skills，并通过 MCP 工具和资源按需提供给客户端。

数据模型保持不变：**一个 workspace 对应一个 WorkspaceProfile**。Skill 服务开关和目录列表保存在该 profile 的 `runtime` 配置中，GUI 与 Linux CLI 启动的 MCP 使用同一份配置。

## Skill 目录格式

每个 Skill 是一个包含 `SKILL.md` 的目录：

```text
skills/
└── code-review/
    ├── SKILL.md
    ├── references/
    │   └── review-checklist.md
    ├── scripts/
    │   └── inspect-diff.py
    └── assets/
        └── example.png
```

`SKILL.md` 必须以 YAML frontmatter 开始，`name` 必须与目录名一致：

```markdown
---
name: code-review
description: Review a code change for correctness, security, and regressions.
allowed-tools: read_file git_diff search_text
metadata:
  version: "1"
---

# Code review

Read the changed files and report concrete findings with file references.
```

名称规则：

- 1–64 个字符；
- 只允许小写字母、数字和单个连字符；
- 不能以连字符开头或结尾；
- 不能包含连续连字符。

## 默认扫描目录

每个 workspace/profile 默认扫描：

```text
.agents/skills
.codex/skills
skills
```

相对路径以 workspace 根目录为基准。也可以在 GUI 中添加绝对路径或 `~/` 路径，例如：

```text
~/.codex/skills
/opt/company-skills
```

根目录可以直接是单个 Skill，也可以在两层以内包含多个 Skill 子目录。配置顺序代表优先级；同名 Skill 出现多次时保留最先发现的版本，并在扫描预览和 `list_skills` 中返回警告。

## GUI 配置

打开 workspace 的 **MCP → 配置 → Agent Skills**：

1. 开启或关闭 Skill 服务；
2. 每行填写一个 Skill 根目录；
3. 点击“扫描目录”进行只读预览；
4. 点击“保存 Skill 服务”；
5. MCP 已运行时，停止并重新启动 MCP 服务；客户端缓存了工具目录时，重新连接 MCP。

“扫描目录”只读取文件，不启动 MCP、Actions、脚本或隧道。

## MCP 工具

### `list_skills`

返回有界的 Skill 元数据，用于先判断哪个 Skill 与任务相关：

```json
{
  "query": "review",
  "max_results": 100
}
```

主要返回字段：名称、描述、来源目录、`skill://` URI、SHA-256 摘要、关联 resources/scripts 和扫描警告。

### `load_skill`

按名称加载完整 `SKILL.md` 和正文指令：

```json
{
  "name": "code-review",
  "max_bytes": 262144
}
```

推荐工作流是先调用 `list_skills`，仅在任务确实需要时调用 `load_skill`。

### `read_skill_resource`

读取 Skill 的关联资源或脚本源码：

```json
{
  "name": "code-review",
  "path": "references/review-checklist.md",
  "start_line": 1,
  "end_line": 120,
  "max_bytes": 262144
}
```

文本资源返回 UTF-8；图片、PDF 等二进制资源返回 base64。资源路径必须位于对应 Skill 目录内。

## MCP Resources

服务启用时，MCP `resources/list` 同时提供：

```text
skill://index.json
skill://<skill-name>/SKILL.md
skill://<skill-name>/references/<file>
skill://<skill-name>/scripts/<file>
skill://<skill-name>/assets/<file>
```

`skill://index.json` 是渐进式发现索引，条目包含：

```json
{
  "name": "code-review",
  "type": "skill-md",
  "description": "Review a code change...",
  "url": "skill://code-review/SKILL.md",
  "digest": "sha256:..."
}
```

Skill 服务关闭时，Skill 工具不会出现在 `tools/list`，MCP 也不会声明 resources capability。

## Linux CLI

Linux 无界面模式读取同一个 workspace/profile：

```bash
coding-tools-mcp list
coding-tools-mcp serve PROFILE_ID --service mcp
```

Skill 服务不需要额外 CLI 参数。MCP 启动时会读取 profile 中的 `skill_service_enabled` 和 `skill_roots`。使用 systemd 时，确保服务用户对 Skill 目录具有读取权限。

## 安全边界

- Skill 服务当前是 **MCP-only**，不会加入 Actions OpenAPI 网关。
- `scripts/` 中的文件只作为源码列出和读取，服务器不会执行 Skill 脚本。
- Skill 内容是任务指令，不构成权限授予，也不能绕过现有命令白名单、路径策略、确认门禁或 workspace 边界。
- 扫描不跟随目录遍历中的符号链接；Skill 目录必须保留在配置根目录内。
- 资源读取会再次 canonicalize 路径，拒绝 `..`、绝对路径和符号链接越界。
- `SKILL.md` 和单个资源默认受大小限制，工具参数可以在上限内进一步收紧返回大小。
- 不要把密钥、Token、客户数据或其他敏感信息放入 Skill 文件。
- 外部来源的 Skill 应先审查其 `SKILL.md`、resources 和 scripts，再加入共享目录。

## 客户端建议

客户端收到 Skill 能力后应采用渐进式加载：

1. 使用 `list_skills` 或 `skill://index.json` 获取名称和描述；
2. 选择与当前任务匹配的 Skill；
3. 调用 `load_skill` 读取完整指令；
4. 仅在指令引用关联文件时调用 `read_skill_resource`；
5. 所有实际文件、Git 和命令操作仍通过原有 Coding Tools MCP 工具执行。
