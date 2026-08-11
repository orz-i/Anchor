# Session 生命周期与内容管理

Anchor 同时存在三类用途不同的 Session。它们必须分开管理，不能共享标识、过期策略或内容预算。

## 1. MCP 传输 Session

Streamable HTTP 客户端通过 `initialize` 获取 `MCP-Session-Id`，发送 `notifications/initialized` 后进入可操作状态。后续请求必须携带相同的 Session ID；客户端结束连接时应发送 `DELETE /mcp`。

服务器边界：

| 项目 | 限制 |
| --- | ---: |
| 未初始化 Session TTL | 5 分钟 |
| 已初始化 Session 空闲 TTL | 24 小时 |
| 单 listener 最大 Session 数 | 512 |
| 单 Session 历史 request ID 数 | 16,384 |

行为：

- 未完成 `notifications/initialized` 时，只接受 `ping` 请求和 `notifications/initialized` 通知。
- 非法 Header、非法阶段请求和过早通知不会延长 Session 生命周期。
- request ID 在同一 Session 内必须唯一；达到历史预算时 Session 会终止，客户端应重新初始化。
- Session 过期、容量淘汰或显式 DELETE 时，会同时取消 in-flight 请求并清理该 Session 的默认工作目录等关联状态。
- 访问未知或已过期 Session 返回 HTTP 404，客户端应重新执行初始化流程。

这些限制只约束传输状态，不等同于 ChatGPT 对话、历史归档或命令进程 Session。

## 2. 命令执行 Session

`exec_command` 对仍在运行、交互式或提前返回的进程创建命令 Session。后续通过：

```text
write_stdin
read_output
kill_session
```

进行管理。

服务器边界：

| 项目 | 限制 |
| --- | ---: |
| 最大并发/占槽命令 Session | 64 |
| 已消费终态的 Session 槽位保留 | 60 秒 |
| 已消费终态的输出/日志保留 | 30 分钟 |
| stdout 环形缓冲区 | 1 MiB |
| stderr 环形缓冲区 | 1 MiB |

已结束命令只有在终态通过 `wait_command` / `kill_session` 等路径被明确消费后才进入回收计时。容量槽位与日志保留从 Catalog 35 起分离：已消费终态在短暂的 60 秒槽位保留后不再占用 64-session 容量，但其输出仍可在 30 分钟日志保留期内通过 `read_output` / `wait_command` 读取。后台 `list_command_sessions`、任务完成门禁等只读刷新不会延长日志保留；显式再次读取该 Session 会更新最后访问时间。未消费终态始终继续占槽且不会自动删除，避免绕过任务完成前必须消费命令结果的约束。

每次登记新命令前、查询 Session 前和列举 Session 前都会清理超过日志保留期的“已结束 + 已消费”记录；容量检查只统计仍运行、尚未消费终态或仍在短暂槽位保留窗口内的 Session。达到 64 个有效槽位且没有可释放记录时，服务器会终止刚启动但无法登记的子进程，然后返回可重试的 `SESSION_LIMIT_REACHED`，避免产生孤儿进程。日志自然过期后的 `wait_command`、`read_output` 或 `kill_session` 返回 `SESSION_NOT_FOUND`；这属于正常淘汰，不会为无工作区变更的 Harness Task 创建 Recovery。

### 输出 Offset

`read_output.offset` 是从进程输出流开始计算的绝对字节位置，不是当前环形缓冲区内的相对下标。

返回值包含：

```text
requested_offset
offset
retained_start_offset
next_offset
total_retained_bytes
total_stream_bytes
```

当客户端请求的旧 offset 已被环形缓冲区淘汰时：

- `offset` 会移动到 `retained_start_offset`；
- `truncated=true`；
- `warnings` 明确说明旧内容已不再保留；
- `next_offset` 仍是绝对 offset，可继续稳定分页。

## 3. 持久历史 Session

历史 Session 通过以下工具管理：

```text
history_session_bootstrap
history_session_checkpoint
history_session_validate
```

它使用稳定的 `session_key` 映射到 `docs/history-session/<number>.md`。`expected_path` 必须与 bootstrap 返回值完全一致，避免宿主会话标识变化导致跨文件串写。

### 生命周期状态

每个历史 Session 有三种有效状态：

```text
active
paused
completed
```

Checkpoint 可通过 `session_status` 修改状态。再次使用相同 `session_key` bootstrap 一个 `paused` 或 `completed` Session 时，服务器只更新 `Updated` 和 `Status` 元数据，将其重新激活为 `active`；既有 checkpoint 和继承摘要保持不变。

### 单次 Checkpoint 内容预算

| 字段 | 限制 |
| --- | ---: |
| `turn_id` | 128 字符 |
| `timestamp` | 128 字符 |
| `user_intent` | 4,000 字符 |
| `notes` | 8,000 字符 |
| 每个数组 | 64 项 |
| 每个数组项 | 2,000 字符 |
| Checkpoint JSON 总量 | 64 KiB |

敏感信息会先脱敏，再生成自动 `turn_id`。未显式提供 timestamp 时，重复提交相同内容会复用原有服务器 timestamp，因此跨秒重试仍保持幂等。

### 历史归档容量

| 项目 | 限制 |
| --- | ---: |
| 单个历史 Markdown | 4 MiB |
| 整个历史目录 | 64 MiB |
| 历史文档数量 | 4,096 |
| `index.json` | 1 MiB |

容量超限返回 `HISTORY_CAPACITY_EXCEEDED`，不会继续读取或覆盖归档。

### Bootstrap 响应窗口

历史文件可以长期保存，但 bootstrap 不再把整个归档无界注入模型上下文：

| 返回内容 | 响应预算 |
| --- | ---: |
| `historyNumbers` | 最近 256 个 |
| `sessionSummaries` | 最近 64 个 |
| 摘要总字符 | 48,000 |
| 单个摘要 | 3,000 字符 |
| `latestHandoff` | 64,000 字符，超限保留头尾 |
| 新文件内继承摘要 | 16,000 字符 |

客户端应检查：

```text
history_numbers_truncated
history_summaries_omitted
history_summary_truncated
latest_handoff_truncated
```

完整归档仍保存在 workspace 中；响应裁剪只影响本次上下文注入，不修改历史文件。

## 4. Harness Work Session 与任务闭环

`begin_work_session` 将持久 History Session 与 Harness Task 绑定。任务可以同时保存：

- `phase`：planning、implementing、verifying、deploying、browser_review、cleanup、ready_to_close 等工程阶段；
- `contract`：不可违反的约束、必需 verification 和 completion policy；
- `slices`：独立文件范围、验收检查、状态和提交证据；
- `working_set`：主要源码、测试、本地化文件和只读参考文件；
- `recovery`：失败步骤、失败类型、是否修改工作区、回滚状态、恢复动作和恢复目标。

Phase 使用显式状态机。普通 `update_task` 不能从 planning 直接跳到 ready_to_close；需要按实际工程阶段推进。系统内部的 Slice 和任务完成流程可以执行对应的受控状态迁移。

### 同一工作区的多任务

任务生命周期状态与共享工作树的默认路由相互独立：

- 切换已有任务只更新工作区的默认任务，不会把其他 active/verifying 任务改成 paused；
- 同一个 History Session 在 shared 工作区创建新的 writer Task 时，上一代同 History shared writer 若没有运行中命令会自动降级为 paused，避免长会话不断堆积等价 writer；独立 History 或 worktree 并行任务不受该规则影响；
- 除上述同 History writer handoff 外，任务只会通过显式 `pause_task` 或既有受控生命周期路径进入 paused；
- 已绑定 MCP 会话始终继续路由到自己的任务；新连接默认跟随工作区当前选中的任务；
- shared 模式的文件、命令和 Git 写操作通过工作区写锁串行执行；同一写域中已有运行命令时，其他任务仍可创建和读取，但写操作返回 `WORKSPACE_WRITER_BUSY`；
- 一次 shared 写入完成后，Anchor 同步同一写域内所有可继续任务的 expected baseline，避免下一任务把已归因变更误判为外部漂移；
- worktree 模式的任务使用独立目录、分支和命令租约，可同时保持 active 并独立写入。
- History bootstrap 保留所有 active/verifying Task 绑定的 History Session；仅显式暂停/终止任务或未绑定活动任务的旧会话会被回收为 paused。

`active_task_ids` 表示仍在推进的任务集合；`default_task_id`/`active_task_id` 只表示无显式绑定时的默认路由目标，不再代表唯一 active 任务。`harness_status` 同时返回 `stale_active_task_ids` 和 `warnings`：active task 达到 8 个会提示生命周期压力，超过 12 小时没有活动的 active/verifying task 会被列为 stale 供调用方显式审阅，而不会被系统静默归档。

### Recovery 闭环

普通、未修改工作区且没有稳定重试身份的 preflight/Patch 验证失败不会创建持久 Recovery。需要持久 Recovery 时，失败响应会返回服务端生成的 `task_recovery.recovery_key`，调用方应在修正参数后重试同一个逻辑步骤时复用该 key；即使修正后的参数指纹发生变化，成功重试也能关联并关闭原 Recovery。

如果原失败步骤没有被机械重试，但后续独立步骤、提交和 verification 已经证明目标完成，可通过 `task(operation="resolve_recovery")` 提交 `recovery_id`、原因和至少一条证据。该操作是审计式 resolution，不会制造 no-op mutation，也不同于放宽 completion gate 的 waive。

### 完成门禁

`task_gate_status` 一次返回全部缺失条件，而不是只报告第一个错误。Catalog 35 默认返回 `detail="compact"`：仅包含 ready、缺失代码、当前 Slice、阻塞 verification、命令会话计数与当前阻塞 Recovery；需要完整 Task、全部 verification 和完整 gate 证据时显式传 `detail="full"`。门禁可覆盖：

```text
运行中或未消费的命令结果
未提交或无法归属的业务文件
结构化 verification 失败或缺失
Task Contract 指定的 verification
未清空的 pending steps
未完成的 Slice 或缺失的 Slice commit
Slice acceptance checks
未解除的 recovery
尚未进入 ready_to_close
必须使用 complete_work_session 的严格关闭策略
```

`no_early_stop=true` 会强制启用待办清空、全部 Slice 完成、无开放恢复、ready_to_close、严格工作会话关闭和禁止未验证完成。调用方不能通过同时传入宽松 completion policy 绕过该约束。

### Slice 生命周期

```text
start_slice
update_slice
complete_slice
```

`complete_slice` 是唯一可把 Slice 标记为 completed 的工具。它会检查声明的 acceptance checks、blocker 和可选 commit 要求；失败时 Slice 状态保持不变，并返回所有缺失证据。

### 严格关闭

`finish_task` 保留兼容入口，但始终经过统一 completion gate。需要最终 History checkpoint 和完整验收的任务应设置 `require_complete_work_session=true`，并通过：

```text
complete_work_session
```

完成。该路径固定要求 verification 通过、将 Session 标记为 completed，并使用可恢复 outbox 原子衔接 Task 关闭与 History checkpoint。

## 5. 验证与恢复

`history_session_validate` 返回：

- 编号缺口、重复 session key、无效文件和空文件；
- active、paused、completed、unknown 状态数量；
- 文档数、总字节数和最大单文件大小；
- 当前容量上限；
- 索引状态及是否重建。

未知历史状态不会被静默当作 completed。验证会给出警告；下次按相同 key bootstrap 时会重新激活为 active。

## 6. 兼容与演进

当前实现遵循已发布的 Streamable HTTP Session 模型，同时把业务状态放在显式工具参数和持久文件中。未来协议若进一步弱化传输 Session，命令 Session 与历史 Session 不需要随传输标识迁移：

- 命令进程继续使用自身 `session_id`；
- 历史归档继续使用稳定 `session_key`；
- 工作区与权限边界继续由 WorkspaceProfile 和工具策略控制。
