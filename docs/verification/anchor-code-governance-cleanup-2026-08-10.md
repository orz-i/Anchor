# Anchor 代码治理清理（2026-08-10）

## 目标

本轮不是新增功能，而是收缩长期迁移后留下的代码表面积：删除确认无调用的死代码、结束已经完成迁移使命的兼容桥、把 feature 边界显式化，并让公开工具目录重新成为唯一调用权威。

治理原则：

1. 只有已经存在正式替代路径、且当前运行契约已有测试保护的兼容层才删除。
2. 名称包含 `bridge`、`fallback`、`compat` 不等于死代码；仍承担滚动升级、Browser worktree 工件映射或运行时恢复职责的路径继续保留。
3. 测试不能成为 production dead wrapper 的唯一调用者；测试应覆盖真实生产入口。
4. 不使用 crate 级 `allow(dead_code)` / `allow(unused_imports)` 隐藏 feature 边界问题。
5. `tools/list`、Actions OpenAPI 与 MCP `tools/call` 必须共享同一个公开能力边界。

## Catalog 34：结束 facade 迁移兼容期

Catalog 32–33 采用“公开 domain facade + 隐藏 legacy leaf 仍可被缓存客户端直调”的迁移结构。Catalog 34 删除后半段兼容桥：

- `git`、`task`、`slice`、`commit_stage`、`skill` 继续作为公开 facade；
- 原 leaf handler 仅保留为 facade 内部 operation 实现，不进入公开 catalog；
- 未发布的内部 handler 通过 MCP `tools/call` 直接调用时返回 unpublished-tool 错误；
- registry 直接过滤内部 operation handler，effective catalog 不再维护第二套“再隐藏一次”的 legacy 过滤逻辑；
- Actions 可调用集合直接从 `advanced` 公开 catalog 派生，不再维护独立 `ALLOWED_TOOLS` 镜像清单。

这使“公开什么”和“实际允许直接调用什么”重新一致，避免三个名单（registry、effective catalog、MCP direct-call exception）长期漂移。

## Skill 路径精简

Developer Mode 的正式路径保持单一只读 `skill` facade；标准 MCP Skills extension/resources 继续保留。

迁移期 helper 中：

- `list_skill_resources` 已完全删除；
- `list_skills`、`load_skill`、`read_skill_resource` 只作为 `skill` facade 内部 operation handler；
- 旧 helper 不再拥有 direct-call 例外；
- `SKILL_RESOURCE_INVALID` 恢复建议改为公开 `skill { operation: "get" }`，不再返回不可发现工具名。

Plugin package/local marketplace 路径不受影响。

## Task / Harness 清理

- 删除已经无语义的 `pause_current_and_start` 与 `pause_current` schema 参数；多任务生命周期由当前 writer lease/worktree 规则决定。
- 删除仅转发到 `start_task_configured`、且忽略 pause 参数的 `start_task_with_handoff` wrapper。
- 修复非变更输入错误的 Recovery 语义：没有显式 retry identity、没有工作区 mutation 的 validation/policy/permission 失败不再创建持久 Task Recovery。
- 对旧版本已经持久化的非变更 `PATCH_*` Recovery 提供兼容收口：它们不再阻塞 completion，并在 task completion 时自动 resolve、保留审计记录。
- 修复 `task operation_log` 的 facade outputSchema：`summary` 可为结构化 object，不再因真实聚合摘要触发 `TOOL_OUTPUT_SCHEMA_VIOLATION`。

## Dead code 与 feature 边界

CLI-only 构建此前通过 crate 级 `allow(dead_code, unused_imports)` 压制差异。本轮移除该 suppression 后暴露了 43 个 warning，并逐项分类：

- 真正无调用：删除 Platform `os_name` / `resolve_executable` 等 API；
- 测试续命 wrapper：删除旧 workspace 全量资源校验与无 reserved allocator wrapper，测试改走真实生产入口；
- desktop-only：health、open-in-file-manager、部分 Canvs/DataStore/SecretStore、Runtime/Tunnel 生命周期与软件管理显式 `cfg(feature = "desktop")`；
- CLI/runtime 共用：保留 daemon、listener、FRP/Cloudflare 实际运行路径。

同时显式补齐 Windows `Win32_System_Registry` feature，避免 CLI-only 构建依赖桌面 feature 的传递性 Windows API 开关。

结果：`cargo check --no-default-features --features cli --lib` 从 43 个被隐藏/暴露 warning 收敛到零 warning，源码中不再存在 `allow(dead_code)` 或 `allow(unused_imports)`。

## 保留的兼容/桥接能力

以下路径仍有明确现役职责，本轮有意保留：

- Workspace/Gateway daemon 的受控滚动升级和协议版本协商；
- Windows SCM/Named Pipe 的当前运行权威；
- Browser worktree artifact bridge；
- 运行时 process-local/daemon fallback 中仍被平台边界实际使用的路径；
- 原生 MCP Skills extension/resources 与 ChatGPT Plugin package 支持。

## 验证门禁

最终收口要求至少覆盖：

- CLI-only zero-warning check；
- Catalog/registry/facade/Skill/Harness 专项契约；
- `cargo check --all-targets --all-features`；
- `cargo clippy --all-targets --all-features -- -D warnings`；
- `cargo test --all-targets --all-features`；
- `pnpm check` 与 `pnpm build`；
- 修改文件 rustfmt check 与 `git diff --check`；
- Catalog 34 committed snapshot。

## 实测结果

清理主体（不含本说明文档本身）覆盖 34 个已跟踪文件，`git diff --stat` 为 **399 insertions / 737 deletions**，实现侧净减少 338 行；新增本验证文档单独记录治理边界和迁移结论。

- CLI-only：`cargo check --no-default-features --features cli --lib` 通过，**0 warnings**；不再依赖 crate 级 dead-code/unused-import suppression。
- Registry：7/7 passed；Catalog 34：11/11 passed；MCP server：23/23 passed。
- Harness contract：57/57 passed，其中覆盖新版本无变更 Patch validation 不建持久 Recovery，以及旧 `PATCH_*` Recovery 非阻塞并在完成时自动 resolve。
- `call_tool_contract`：33/33 passed；`tool_output_schema_contract`：7/7 passed；Skills：32/32 passed。
- `cargo check --all-targets --all-features` passed。
- `cargo clippy --all-targets --all-features -- -D warnings` passed；过程中捕获的 `manual_find` 已改为 iterator `find`，未新增 lint allow。
- `cargo test --all-targets --all-features -- --test-threads=1` passed。首次等待全量测试结果时出现一次上游 502，但原 command session 未重启，继续消费后确认原进程 exit code 0。
- `pnpm check` passed：Svelte **0 errors / 0 warnings**；`pnpm build` passed。
- 本轮修改 Rust 文件 stable rustfmt `--check` passed；`git diff --check` passed。
- `git diff --check` 仍会提示当前 Windows 仓库既有 `core.autocrlf` 下的 LF→CRLF 转换警告；exit code 为 0，未发现 whitespace error，本轮未借机修改仓库级行尾策略。
- Catalog 34 committed snapshot 已更新并通过 snapshot contract。

当前源码验证完成后仍有一个**运行中旧版本治理状态**需要区分：本任务由 live Catalog 33 Harness 启动，早期一次 `PATCH_CONTEXT_MISMATCH` 已被旧逻辑持久化为 open Recovery。Catalog 34 源码已修复该类别的新建与旧记录收口语义，但在不主动重启 Windows SCM/Workspace daemon 的约束下，当前 live Catalog 33 进程不会立即获得该修复。因此源码/提交可完成，Harness 是否能在本轮直接正式 close 以 live gate 实测为准；不得通过人工修改 journal 或 verification waiver 绕过。
