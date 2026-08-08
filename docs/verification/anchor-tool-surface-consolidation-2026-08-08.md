# Anchor 工具面收敛复核（2026-08-08）

## 背景

Catalog 31 已将旧 Skill helper 从 `tools/list` 移除，但本地工具仍按细粒度操作持续增长。调研时 effective catalog 的本地工具数量为：

- `advanced`: 67
- `core`: 40
- `read-only`: 19

其中 Git 单独展开为 15 个 `git_*` 工具；Harness 又分别公开 Task、Slice 和 staged-commit 生命周期工具。默认 Browser MCP 另外提供 32 个代理工具，因此 `advanced` 的完整公开面约为 99 个工具，容易增加宿主发现、schema 传输和模型工具选择成本。

## 收敛原则

本轮没有删除底层能力，而是采用“公开 domain facade + 隐藏 legacy leaf”的兼容结构：

1. 只有同领域、共享既有策略和状态机的操作才进入同一个 facade。
2. facade 的 `operation` 只负责选择操作；真正执行仍委托给原 leaf handler。
3. facade 公开 schema 汇总可用参数，但每次调用在委托前仍使用原 leaf input schema 做权威校验。
4. facade 的 operation 列表按原 profile 的 leaf allowlist 动态生成，不能借合并扩大 core/read-only 权限。
5. legacy leaf 继续保留 handler/schema，已缓存旧 schema 的客户端仍可直接调用；新 `tools/list` 不再发布它们。
6. Harness baseline、worktree 路由、writer lease、Recovery、verification、ChangeSet、危险模式和路径策略继续由原实现负责，不在 facade 中复制业务逻辑。
7. 被宿主协议和会话编排直接依赖的高价值工具保持独立，不为了减少计数而强行合并。

## 已整合领域

### Git: 15 → 1

公开工具：`git`

`operation` 包含 `status`、`diff`、`log`、`show`、`blame`、`stage`、`commit`、`restore`、`reset`、`revert`、`clean`、`worktree_list`、`worktree_create`、`worktree_remove`、`worktree_prune`。

read-only profile 只会发布原本允许的只读 Git operation，不会出现 stage/reset/clean/worktree_create 等写操作。

### Harness Task: 18 → 1

公开工具：`task`

覆盖状态、operation log、verification disposition、project state、start/update/pause/resume/switch/finish、baseline 接受/刷新、gate/context/events/change summary/export 等原 Task leaf 能力。

core 仅暴露它原来已经拥有的 Task leaf 对应 operation；例如不会因为存在 `task` facade 而获得 advanced-only `start` 或 `finish`。

### Slice: 3 → 1

公开工具：`slice`

`operation`: `start`、`update`、`complete`。

### Staged commit workflow: 3 → 1

公开工具：`commit_stage`

`operation`: `run`、`status`、`wait`。

## Catalog 32 结果

整合后的 effective local catalog：

| Profile | Catalog 31 | Catalog 32 | 减少 |
| --- | ---: | ---: | ---: |
| advanced | 67 | 32 | 35 |
| core | 40 | 27 | 13 |
| read-only | 19 | 14 | 5 |

默认 32 个 Browser proxy 与 `advanced` 32 个本地工具组合后正好为 **64 个工具**，可以完整落在当前单页预算内，不再依赖将关键 Anchor 工具挤到第一页来规避分页缺失。

## 保持独立的工具

以下类别本轮有意不合并：

- History Session：`history_session_bootstrap`、`history_session_checkpoint`、`history_session_validate`。这些工具直接参与 ChatGPT 会话恢复与持久化协议，名称和调用时机具有宿主级契约意义。
- Work Session 高层入口：`begin_work_session`、`close_work_session`、`complete_work_session`。它们是跨 History/Harness 的原子编排入口，不等价于普通 Task leaf。
- Command session：`exec_command`、`wait_command`、`read_output`、`write_stdin`、`kill_session`、`list_command_sessions`。它们对应长生命周期会话和消费语义，合并会让 schema 与终态协议更难辨识。
- 文件工具：读取、搜索、Patch、删除等语义和安全属性差异较大，暂不为了数量而合并。
- Browser proxy：下游 MCP 已提供强类型逐工具 schema；本轮优先通过 Anchor 本地工具收敛把默认总数压到 64。后续若继续缩减，应优先使用 Browser `includeTools`/profile 或基于 eval 的受控 facade，而不是丢失逐操作 schema。

## 兼容与安全边界

- MCP `tools/list` 和 Actions OpenAPI 都从 effective catalog 构建，因此新客户端只看到 facade，不会在 Actions 侧重新展开 legacy leaf。
- MCP 对隐藏 legacy facade leaf 提供与旧 Skill helper 相同类型的缓存兼容通道，但之后仍执行当前 profile 的 raw allowlist 检查；隐藏不等于授权。
- facade output 增加 `facade` 与 `operation`，便于日志、诊断和宿主确认实际路由。
- Harness `next_actions` 会先检查原 leaf 是否在当前 profile 允许，再映射到公开 facade 名称，避免返回不可发现的 legacy 名称或产生权限提升。
- Git revert/stage 等用户可见恢复建议已改为 facade 形式。

## 验证重点

- facade 调用实际穿透到原 Git/Harness/Slice/staged-commit contract，而非模拟结果。
- operation 缺少原 leaf 必填参数时仍由 canonical schema 拒绝。
- read-only/core 的 operation enum 不包含 advanced-only 或写操作。
- MCP `tools/list` 隐藏 legacy leaf，同时缓存客户端直接 `tools/call` legacy leaf 仍兼容。
- Actions OpenAPI 只发布 facade endpoint。
- Catalog 预算、digest/fuzz、snapshot 与默认 Browser 32-tool 组合均有回归测试。

本轮实际验证包括：Catalog 专项 11 项、registry/profile 专项 6 项、facade 路由专项 3 项、MCP server 专项 23 项、Actions OpenAPI 专项 3 项、outputSchema contract 7 项，以及 `cargo test --all-targets --all-features` 全量测试。严格 `cargo clippy --all-targets --all-features -- -D warnings`、`pnpm check`、`pnpm build` 与 `git diff --check` 均通过。

仓库级 `cargo fmt --all -- --check` 仍会命中本轮未修改文件中的既有格式漂移（例如 OAuth/FRP/health 等模块）。本轮没有借机大范围重排无关文件；所有本轮修改的 Rust 文件均单独通过 stable rustfmt `--check`，并且全量编译、测试与严格 Clippy 均通过。

## 后续建议

工具面以后应优先按“领域 facade + 原 leaf 内核”的模式演进，而不是每增加一个操作就增加一个公开工具。新增 facade 前应满足：操作共享安全域、状态机和主要参数语义；如果合并后只能依赖大量弱类型参数或会模糊权限/生命周期，则保持独立更合适。
