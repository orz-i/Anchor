# MCP Agent Skills 服务

Anchor 可以从当前 workspace/profile 配置的目录中发现 Agent Skills。Skill 有两条明确分离的发布路径：

- **MCP runtime compatibility**：Anchor 继续声明 MCP Skills extension，并提供 `skills/list`、`skills/get`、`resources/read` 给支持该扩展的宿主；
- **ChatGPT/Codex Plugin package**：当前 OpenAI Plugin 架构要求 Skill 作为插件目录中的静态 `skills/` 文件夹，由 `.codex-plugin/plugin.json` 声明，并通过 `.app.json` 绑定已注册的 Anchor MCP app。

旧的 Skill helper 工具仍保留为兼容调用入口，但不再发布到 `tools/list`，因此不会重新占用 ChatGPT 的工具目录配额。

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
  policy:
    strict: true
  priority: 5
---

# Code review

Read the changed files and report concrete findings with file references.
```

除 `name`、`description`、`license`、`compatibility`、`metadata` 和
`allowed-tools` 外，Anchor 也兼容 `risk`、`category`、`user-invocable`
等生态扩展字段。扩展字段会作为只读元数据返回；若与 `metadata` 中的同名键冲突，
显式 `metadata` 值优先。扩展字段不会授予工具权限、启用脚本执行或绕过现有策略。

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

根目录可以直接是单个 Skill，也可以在两层以内包含多个 Skill 子目录。配置顺序代表优先级；同名 Skill 出现多次时保留最先发现的版本，并在扫描预览和兼容 Skill 目录中返回警告。

## GUI 配置

打开 workspace 的 **MCP → 配置 → Agent Skills**：

1. 开启或关闭 Skill 服务；
2. 每行填写一个 Skill 根目录；
3. 点击“扫描目录”进行只读预览；
4. 点击“保存 Skill 服务”；
5. MCP 已运行时，停止并重新启动 MCP 服务；若 Skill 还需要出现在 ChatGPT Plugin 中，重新执行后文的 `anchor plugin package` 生成插件静态快照。

“扫描目录”只读取文件，不启动 MCP、Actions、脚本或隧道。

## MCP Skills extension（兼容路径）

Skill 服务启用时，MCP `initialize` 会声明：

```json
{
  "capabilities": {
    "extensions": {
      "io.modelcontextprotocol/skills": {}
    }
  }
}
```

Anchor 实现 `skills/list`、`skills/get` 与 `resources/read`：

- `skills/list` 返回 canonical `skill://anchor/<name>/SKILL.md` URI、完整 YAML frontmatter 的 JSON 投影，以及 `SKILL.md` 和全部可导入支持文件的资源清单；`anchor` 是 MCP server namespace，`<name>` 是 Skill 目录名，并且必须与 frontmatter `name` 完全一致；
- 每个资源条目包含文件级 `sha256:<64 lowercase hex>` digest；`SKILL.md` 文件摘要与 Anchor 的整包 `SkillSummary.digest` 分开计算；
- `skills/get` 通过 canonical `SKILL.md` URI 返回与目录相同的完整 Skill manifest；
- `resources/read` 对 manifest 中的 canonical URI 返回完整文件内容，以便宿主核对 digest。

这条 MCP extension 是兼容能力，不是当前 ChatGPT Plugin 的 Skill 打包入口。Anchor 仍对该目录执行保守完整性约束：`SKILL.md` 不超过 256 KiB、单个支持文件不超过 1 MiB、单个 Skill 最多 100 个文件且总资源不超过 5 MiB；符号链接、资源扫描截断、未进入受控清单的额外文件或不可读取资源不会进入可导出的安全快照。

## ChatGPT/Codex Plugin package（当前 ChatGPT 主路径）

仅在 Developer mode 中把 Anchor MCP URL 注册成一个 app，会得到工具连接，但**不会自动把 Workspace Skill 变成该 app 详情页中的 Plugin Skills**。要让详情页出现 Skill，需要把 app 与静态 Skill 目录组装成真正的 Plugin package。

先在 ChatGPT Developer mode 注册 Anchor MCP，并从浏览器 URL 复制 `plugin_asdk_app...` technical ID。然后执行：

```bash
anchor plugin package PROFILE_ID --app-id plugin_asdk_app_xxx
```

默认生成：

```text
<workspace>/.anchor/chatgpt-plugin-marketplace/
├── marketplace.json
└── plugins/
    └── anchor-<workspace>/
        ├── .codex-plugin/
        │   └── plugin.json
        ├── .app.json
        └── skills/
            └── <skill-name>/
                ├── SKILL.md
                └── ...supporting files
```

`plugin.json` 声明 `"skills": "./skills/"` 与 `"apps": "./.app.json"`；`.app.json` 将逻辑 app 名 `anchor` 映射到传入的 `plugin_asdk_app...`。打包只复制**位于当前 workspace 内**且通过 Anchor 现有 Skill 完整性/路径/资源校验的目录；`~/.codex/skills` 等 home/external 来源默认不会被复制到 Plugin package，避免无意把用户级私有资源打包出去。所有被跳过 Skill 都会作为 warning 返回。

可用 `--output PATH` 指定独立 marketplace 根目录，或用 `--name stable-kebab-name` 固定 plugin name。相对 `--output` 以 Workspace 根目录解析。

本地验证流程：

1. 确认 Workspace Skill 服务已启用并能扫描到 `<skill-name>/SKILL.md`；
2. 在 ChatGPT Developer mode 注册 Anchor MCP app，取得 `plugin_asdk_app...`；
3. 执行 `anchor plugin package ... --app-id ...`；
4. 按命令输出运行 `codex plugin marketplace add "<marketplace-root>"`；
5. 重启 ChatGPT desktop app，在 Plugins Directory 选择该 local marketplace 并安装生成的 Plugin；
6. 新建聊天，打开 Plugin 详情确认 Skills 列表，再测试 Skill activation/use。

`plugin_asdk_app...` 属于 ChatGPT 对当前注册 app 分配的宿主技术 ID，Anchor 不静态猜测或硬编码。修改 Workspace Skill 后，需重新运行 `anchor plugin package` 更新插件静态快照；仅重启 MCP daemon 不会修改已经安装的 Plugin Skill 文件。

## 兼容 Skill helper 工具

以下旧接口继续保留在服务器中，供已经缓存过 schema 的旧客户端显式调用；**它们不再出现在 `tools/list`**。

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

按名称加载正文指令。较长 Skill 支持按行和返回字节预算渐进加载：

```json
{
  "name": "code-review",
  "start_line": 1,
  "end_line": 400,
  "max_bytes": 65536
}
```

返回值包含 `startLine`、`endLine`、`totalLines`、`totalBytes`、`returnedBytes`、`truncated` 和 `nextStartLine`。如果 `truncated=true`，使用 `nextStartLine` 继续读取。`max_bytes` 最大为 262144。

对不支持原生 MCP Skills extension 的旧客户端，推荐工作流仍是先调用 `list_skills`，仅在任务确实需要时调用 `load_skill`。

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

canonical `resources/read` 请求（不带查询参数）会一次返回完整 `SKILL.md`，用于原生 Skill 导入的 digest 校验。旧客户端仍可显式使用查询参数分页，例如：

```text
skill://code-review/SKILL.md?start_line=401&max_bytes=65536
```

仅允许 `start_line`、`end_line` 和 `max_bytes` 三个查询参数，且 `max_bytes` 不得超过 131072。

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

Skill 服务关闭时，MCP 不声明 Skills extension 或 resources capability。Skill 服务开启时，旧 Skill helper 工具同样不会进入 `tools/list`；原生 Skill 发现不再消耗工具槽位。

## Linux CLI

Linux 无界面模式读取同一个 workspace/profile：

```bash
anchor list
anchor serve PROFILE_ID --service mcp
```

Skill 服务不需要额外 CLI 参数。MCP 启动时会读取 profile 中的 `skill_service_enabled` 和 `skill_roots`。使用 systemd 时，确保服务用户对 Skill 目录具有读取权限。

## 安全边界

- Skill 服务当前是 **MCP-only**，不会加入 Actions OpenAPI 网关。
- `scripts/` 中的文件只作为源码列出和读取，服务器不会执行 Skill 脚本。
- Skill 内容是任务指令，不构成权限授予，也不能绕过现有命令白名单、路径策略、确认门禁或 workspace 边界。
- 扫描不跟随目录遍历中的符号链接；Skill 目录必须保留在配置根目录内。
- 资源读取会再次 canonicalize 路径，拒绝 `..`、绝对路径和符号链接越界。
- `SKILL.md` 和单个资源默认受大小限制，工具参数可以在上限内进一步收紧返回大小。
- `SKILL.md` 的 128 KiB 文件上限是安全硬边界。正文超过建议的 500 行或估算 5000 tokens 时不会被拒绝，而是在 `list_skills` 和 `load_skill` 中返回 `oversized`、大小统计及 `qualityWarnings`。
- token 数量是用于上下文预算提示的近似估算，不作为格式有效性、权限或执行判断依据。
- 不要把密钥、Token、客户数据或其他敏感信息放入 Skill 文件。
- 外部来源的 Skill 应先审查其 `SKILL.md`、resources 和 scripts，再加入共享目录。

## 客户端建议

支持 MCP Skills extension 的 Plugin 应优先使用原生 Skill 导入。旧客户端若只能使用兼容接口，则采用渐进式加载：

1. 使用 `list_skills` 或 `skill://index.json` 获取名称和描述；
2. 选择与当前任务匹配的 Skill；
3. 调用 `load_skill` 读取指令；较长正文按 `nextStartLine` 分页；
4. 仅在指令引用关联文件时调用 `read_skill_resource`；
5. 所有实际文件、Git 和命令操作仍通过原有 Anchor 工具执行。
