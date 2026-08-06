# Anchor v1.7 任务闭环反馈复核与修复

日期：2026-08-06  
复核基线：`c92f2fadcd69a65437589f4af59c9b0d12660d01` / Catalog v27  
目标版本：Catalog v28

## 结论

反馈中的主要问题不是单一工具缺失，而是任务状态、验收证据和失败恢复之间缺少一份不可绕过的持久契约。Catalog v27 已具备命令终态消费、Git 工作树、结构化 verification、History outbox、Patch 事务和环境指纹等基础能力；本次在其上补齐 Task Contract、Phase、Slice、Working Set、Recovery 和统一 completion gate。

## 逐项复核

| 反馈主题 | v27 状态 | v28 处理 | 结果 |
| --- | --- | --- | --- |
| 任务可在整体目标未完成时提前结束 | 仅检查命令、工作树和通用 verification | 增加持久 Task Contract、全部缺失项 completion gate、`no_early_stop` 强制策略 | 已修复 |
| 缺少明确工程阶段，恢复后容易跳步 | 只有 TaskStatus，没有工程 Phase | 增加 TaskPhase 状态机，禁止 planning 直接跳到 ready_to_close | 已修复 |
| 大任务只能靠字符串步骤，无法逐 Slice 验收 | 仅有 completed/pending steps | 增加一等 Slice、文件范围、acceptance checks、blocker 和 commit 证据 | 已修复 |
| 验收清单未持久化，模型可只跑部分测试 | verification 有证据但没有声明式要求 | Task/Slice 都可声明 VerificationRequirement，门禁逐项匹配有效证据 | 已修复 |
| 旧 verification 可满足后来创建的 Slice | 无 Slice 时序语义 | Slice 只接受 created_at 之后产生的 verification | 已修复 |
| 已建立的 Contract 或 Slice 可被后续更新放宽/替换 | 无结构化 Contract | Contract 只能单调增强；已有 Slice 必须通过专用工具更新 | 已修复 |
| 工具失败后没有“从原步骤继续”的结构化状态 | Operation/Verification 可审计，但无当前 recovery 对象 | 保存失败步骤、类型、变更、回滚、建议和恢复目标；同一步骤成功自动解除 | 已修复 |
| 同名工具的无关成功可能误解除失败 | 无恢复步骤身份 | 使用 step fingerprint / recovery_key 精确匹配；首个开放 recovery 不被后续无关失败覆盖 | 已修复 |
| 预期失败被审计接受后仍可能阻塞 | verification disposition 已有，但与恢复状态无关联 | recovery 精确绑定 verification id；非 active failure disposition 仅解除对应 recovery | 已修复 |
| 完成检查只返回第一个错误，修复过程反复往返 | `finish_task` 分支式返回 | `task_gate_status` 和 `finish_task.completion_gate` 一次返回全部缺失项 | 已修复 |
| 最终 Task 关闭与 History checkpoint 可分离 | `close_work_session` 已有可恢复 outbox | 新增严格 `complete_work_session`，固定要求已验证并标记 Session completed | 已增强 |
| 后台命令未消费就进入最终回复 | v27 已在 `finish_task`、History checkpoint 和 close outbox 阻塞 | 保留并纳入统一 completion gate | 已解决，无回归 |
| 命令执行时长与保留时长混淆 | v27 已分离 execution duration、session age 和 retained age | 保留现有契约与回归测试 | 已解决，无回归 |
| Patch 失败可能无明确终态或污染工作区 | v27 Patch 已事务写盘、超时、取消和回滚，并有 `patch_check` | Patch 失败再进入任务 recovery，completion gate 阻止带债务结束 | 已增强 |
| Git 提交与任务证据脱节 | v27 普通 `git_commit` 和 `stage_commit` 已生成 ChangeSet/verification | Slice 可额外要求 commit 证据 | 已增强 |
| History 恢复依赖摘要推断 | v27 已绑定 task id、session key/path 和结构化事件 | `task_context` 直接返回 contract、phase、Slice、working set、recovery 和 gate | 已增强 |
| 工具 schema 反复动态发现 | v27 已提供 `anchor-core/files/command/git` 分组与 Catalog 指纹 | 新工具进入 Catalog v28 快照；宿主的“下一条返回 schema”提示仍属于宿主 lazy-schema 行为 | 服务端已尽可能缓解 |
| 浏览器旧构建或缓存导致定位失真 | v27 已有 `browser_build_info`、`browser_wait_for_build`、SW/Cache 清理和无缓存 reload | reconnect 后完成生产页面烟测；v28 在无显式版本元数据时增加资源清单 SHA-256 指纹 fallback | 已修复并实测 |
| 环境变化难以复现 | v27 `server_info` 和 `check_exec_environment` 已返回 Git/Catalog、工具链、包管理器、linker 和沙箱状态 | completion/recovery 证据继续引用环境状态 | 已解决，无回归 |

## 新增持久模型

### Task Contract

- `no_early_stop`
- `constraints`
- `required_verifications`
- `completion_policy`

`no_early_stop=true` 会强制：待办清空、全部 Slice 完成、无开放 recovery、进入 ready_to_close、必须使用 complete_work_session、禁止未验证完成。

Contract 设置后只能单调增强，不能移除约束、修改既有必需 verification 或关闭已经启用的 completion policy。已有 Slice 不能通过 `update_task` 整体替换。

### Task Phase

```text
unspecified
planning
implementing
verifying
deploying
browser_review
cleanup
ready_to_close
blocked
paused
completed
```

普通任务更新必须满足状态机；最终 completed 只由任务完成事务设置。

### Slice

每个 Slice 保存 id、title、status、files、acceptance checks、commit SHA 和 blocker。`complete_slice` 是唯一完成入口，验收失败不会修改 Slice 状态。

Slice 使用 planned → in_progress → verifying → completed 的受控状态机；旧 verification 不会满足后来创建的 Slice。

### Recovery

失败对象保存：失败步骤、失败类型、错误码、关联 verification、是否修改工作区、回滚状态、建议动作、恢复目标和 resolved 状态。

恢复步骤使用稳定指纹。命令和 Patch 可通过 `recovery_key` 在修正参数后继续同一逻辑步骤；无关的同名工具成功不会解除 recovery。

## 兼容性

- 旧任务没有新字段时通过 serde defaults 读取。
- 普通旧任务的 pending steps 不自动成为完成门禁；只有显式 policy 或 `no_early_stop` 启用。
- `finish_task` 保留既有主要错误码，例如 `TASK_COMMAND_RESULTS_PENDING`、`TASK_WORKTREE_NOT_CLEAN`、`TASK_VERIFICATION_MISSING`。
- 审计接受的 expected failure 仍可完成任务，不会被新 recovery 机制错误阻塞。
- `close_work_session(session_status=completed)` 不能绕过 `require_complete_work_session`；只有严格入口可设置完成凭证，并能安全升级此前被门禁阻塞的 Prepared outbox。
- `phase=completed` 同时受输入 Schema 和 Harness 状态层拒绝，只能由任务完成事务设置。

## 验证结果

- `cargo check --all-targets`：通过。
- 严格 Clippy（`-D warnings`）：通过。
- 范围化 rustfmt：通过；全仓格式检查仍包含仓库既有、与本轮无关的格式债务。
- Rust 全 targets：库测试 383 通过、1 忽略；集成合约 30/26/4/50/21/7 全部通过。
- Harness 工具合约：50/50 通过。
- Tool output schema 合约：7/7 通过。
- Call tool 合约：30/30 通过。
- 安全合约：26/26 通过。
- History Session 合约：21/21 通过。
- `pnpm check`：0 errors / 0 warnings。
- `pnpm build`：通过。
- ChatGPT session prompt 布局：4/4 通过。
- Catalog v28 effective snapshot：实际执行 1 项并通过；之前的 0 项筛选结果被正确标记为 inconclusive，后续成功结果已 supersede。

## 运行态说明

复核开始时已安装实例仍是 Catalog v27、Git SHA `c92f2fad...`，Browser 调试端口 `46988` 不可达。一次受控 `browser__reconnect` 重新分配端口 `54988` 后恢复为 operational；生产预览页可导航、快照可读，Service Worker 与 Cache Storage 均为空。旧运行实例因没有显式 build metadata，`current_build` 仍为 null；v28 源码已增加排序资源清单的 SHA-256 `asset_fingerprint` 作为 fallback，并通过专项和全 targets 测试。Catalog v28 的安装态仍需在新构建安装后复核。
