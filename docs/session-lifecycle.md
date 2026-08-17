# Session 生命周期与隔离治理

Anchor 使用三类完全不同的 Session 概念。开发 Session、命令进程 Session 和旧式 MCP transport Session 不共享标识、生命周期或持久化边界。

## 1. 开发 Session

开发 Session 是 ChatGPT/Agent 一次独立开发对话的持久状态。公开入口统一为一个 `session` facade：

```text
session operation=open
session operation=checkpoint
session operation=list
session operation=get
session operation=validate
```

### 默认隔离原则

每个新对话首次调用 `session operation=open` 时，Anchor 只创建或恢复该对话自己的 Session。`open` 不会：

- 扫描或读取其他 Session Markdown；
- 汇总历史 Session；
- 返回 latest handoff、继承摘要或全局 resume state；
- 自动暂停其他 active Session；
- 自动选择或恢复其他 Session 的 Harness Task；
- 扫描、迁移或改写 `docs/history-session/`。

因此 Session 数量增长不会线性扩大 `open` 的模型上下文。只有用户明确要求恢复、查找、比较或引用以前的工作时，调用方才应先通过 `list` 获取有限元数据，再对一个明确相关的 `session_id` 使用 `get`。

### 标识与存储

Anchor 为开发 Session 生成 opaque handle：

```text
ses_<32 hex UUIDv4>
```

新存储位于：

```text
docs/session/
├── index.json
├── ses_<id>.md
└── ...
```

`index.json` 只保存 Session metadata 和可选 host-conversation 映射，不保存 checkpoint 正文、摘要或 handoff。Session Markdown 只包含自己的元数据和 checkpoint。

默认容量：

| 项目 | 限制 |
| --- | ---: |
| 单个 Session Markdown | 4 MiB |
| Session store 总量 | 64 MiB |
| Session 文档数 | 4,096 |
| `index.json` | 1 MiB |
| `session list` 默认页大小 | 20 |
| `session list` 最大页大小 | 100 |
| `session get` 默认读取 | 64 KiB |
| `session get` 最大读取 | 256 KiB |

`session get` 在 UTF-8 字符边界截断，避免返回无效文本。

### Host conversation 映射

ChatGPT 提供的 `_meta["openai/session"]` 只在 MCP server 完成公开 input schema 校验后作为内部 host-conversation hint 注入 `open`。它不是开发 Session 的持久主键，也不会直接暴露为 Session handle。

同一个 host conversation 重复 `open` 时可幂等恢复同一个 opaque `session_id`；新的 host conversation 默认获得新的开发 Session。

### Checkpoint

显式 checkpoint 必须携带：

```text
session_id
expected_path
```

`expected_path` 必须与 `open` 返回的 `session_path` 完全一致。这样即使调用方拿到另一个 Session 的路径，也不能把 checkpoint 跨 Session 写入。

显式 checkpoint 属于 Session metadata 持久化，不是业务工作树 mutation。它不会继承 workspace 默认 Task，也不会要求 unrelated peer Task 的 Git/worktree baseline 当前；但当前调用方拥有 running 或 terminal-unconsumed command 时仍会返回 `SESSION_COMMAND_RESULTS_PENDING`。

单次 checkpoint 内容预算：

| 字段 | 限制 |
| --- | ---: |
| `turn_id` | 128 字符 |
| `timestamp` | 128 字符 |
| `user_intent` | 4,000 字符 |
| `notes` | 8,000 字符 |
| 每个数组 | 64 项 |
| 每个数组项 | 2,000 字符 |
| Checkpoint JSON 总量 | 64 KiB |

Checkpoint 在写入前脱敏；相同 `turn_id` 与相同内容重复提交保持幂等。

Session Markdown 的职责是**可恢复 handoff snapshot**，不是 Harness operation journal。为避免长任务把每一次补丁、格式化命令和验证重试永久复制进开发 Session：

- 顶部 `已确认事实 / 已完成修改 / 关键设计决定 / 测试结果` 只投影最近一次 `close_work_session` 之后的当前 handoff 窗口；若 Session 当前正好停在一个已关闭任务，则投影该关闭 checkpoint；
- `runtime_state / remaining_issues / next_actions` 始终只取最新 checkpoint；
- 自动进度 checkpoint 使用稳定的 task/tool 槽位更新，不再用 command session、operation id 等瞬时标识制造新记录；
- blocking/required verification 的失败和后续成功使用稳定验证身份更新，因此恢复后的成功会清除旧失败的当前状态；
- 当某 Harness Task 已存在 `close-work-session-<task_id>` 最终 checkpoint 时，该任务此前的 `auto-*` 执行细节在下一次 Session 写入时被最终摘要替代；
- 对尚未关闭的任务，旧版本遗留的重复 `auto-*` 记录按 task + commit / verification / progress 槽位合并，只保留最新恢复所需状态；
- 手工 checkpoint、任务最终 close checkpoint 和 commit milestone 不因上述自动噪音压缩策略而被当作普通执行细节删除。

完整的命令、补丁、verification 和 Recovery 审计仍属于 Harness journal；开发 Session 不复制这份事件日志。`session checkpoint` 返回 `raw_checkpoint_count`、`compacted_checkpoint_count` 和 `checkpoint_compaction`，便于确认一次写入是否发生了历史噪音收敛。

### 生命周期状态

开发 Session 支持：

```text
active
paused
completed
```

同一个 Session 被显式 reopen 时可重新激活为 `active`。这只修改当前 Session，不会改变其他 Session 的 lifecycle。

## 2. 冻结的 legacy `docs/history-session/`

旧目录：

```text
docs/history-session/
```

是冻结历史归档，不进行数据迁移。新 Session 实现遵循以下硬边界：

- 不把它作为 `session_dir`；
- 不扫描它来创建新索引；
- 不把它计入 `docs/session` 容量；
- `session validate` 不校验它；
- 不自动读取、摘要或注入其中内容；
- 不删除、不重写旧文件。

只有用户明确要求查看 legacy 历史时，才通过 `read_file` 对一个精确旧路径进行按需读取。新工具不会提供自动 migration bridge。

## 3. Harness Work Session 绑定

`begin_work_session` 先打开当前开发 Session，再用明确的 `session_id + session_path` 绑定 Harness Task。

关键约束：

- 不再回退到 workspace default Task 来“猜测”当前 Session 的任务；
- 只有已绑定到相同 `session_id + session_path` 的 Task 才能作为当前 Session 的恢复候选；
- 同一开发 Session 的 shared writer handoff 可以暂停上一代同 Session writer；
- 不同开发 Session 可以同时保持 active；
- 一个 Session 的 checkpoint 不继承 peer Session Task 的 baseline；
- worktree Task 的 Session checkpoint 仍写到主工作区的 `docs/session` metadata store，而不是 worktree 内复制一份 Session archive。

Task 可保存 phase、contract、slices、working set、Recovery 和 verification。开发 Session 只是明确的会话归属，不替代 Harness 的工程状态机。

### 完成路径

`complete_work_session` 使用 recoverable outbox 把 Task 完成和开发 Session checkpoint 衔接起来。Outbox 持久字段使用：

```text
session_id
session_path
session_checkpoint
```

失败 phase 使用 `session_checkpoint`，不再使用 History Session 命名。

任务完成仍受以下门禁约束：

- running / terminal-unconsumed command；
- 未提交或无法归属的业务变更；
- verification failure / missing verification；
- pending steps；
- Slice acceptance；
- open Recovery；
- completion policy 指定的其他条件。

## 4. 命令进程 Session

`exec_command` 对仍运行、交互式或提前返回的进程创建独立的 command session。内部存储类型为 `CommandSessionStore`，与开发 Session store 无关。

后续通过：

```text
wait_command
read_output
write_stdin
kill_session
list_command_sessions
```

管理。

服务器边界：

| 项目 | 限制 |
| --- | ---: |
| 最大并发/占槽 command session | 64 |
| 已消费终态槽位保留 | 60 秒 |
| 已消费终态日志保留 | 30 分钟 |
| stdout 环形缓冲区 | 1 MiB |
| stderr 环形缓冲区 | 1 MiB |

命令工具的外部结果仍使用既有 `session_id` 表示 command session handle，以保持命令工具协议稳定；当命令信息被投影到 Harness operation log 时，字段明确命名为 `command_session_id`，而 `session_id` 专门表示开发 Session。

### 输出 offset

`read_output.offset` 是从进程输出流起点计算的绝对字节位置。返回值包含：

```text
requested_offset
offset
retained_start_offset
next_offset
total_retained_bytes
total_stream_bytes
```

若旧内容已被环形缓冲区淘汰，`offset` 会前移到 `retained_start_offset`、`truncated=true`，而 `next_offset` 仍可继续稳定分页。

## 5. Legacy MCP transport Session compatibility

部分当前连接路径仍保留 stateful MCP transport compatibility。实现类型明确命名为：

```text
LegacyMcpSession
LegacyMcpSessionInfo
LegacyMcpSessionStore
```

它只负责 transport 初始化状态、request-id 去重、TTL 和连接清理，不是 Anchor 开发 Session，也不参与 `docs/session` 持久化。

当前兼容边界：

| 项目 | 限制 |
| --- | ---: |
| 未初始化 transport Session TTL | 5 分钟 |
| 已初始化空闲 TTL | 24 小时 |
| 单 listener 最大 transport Session | 512 |
| 单 transport Session request ID 预算 | 16,384 |

将它标记为 `LegacyMcp*` 的目的，是防止应用层开发 Session 和 transport compatibility state 再次发生概念耦合。未来移除旧式 transport 状态时，不需要迁移开发 Session 或 command session。

## 6. Session 验证

`session operation=validate` 只验证新的 `docs/session` store：

- opaque Session ID 与文件名一致；
- metadata 可解析；
- 是否存在 duplicate Session / duplicate host mapping；
- 是否有无效或空文件；
- active、paused、completed、unknown 状态计数；
- 文档总量和容量边界；
- `index.json` 状态。

`repair=true` 只根据新的 Session Markdown 重建新的 metadata index，不接触 `docs/history-session/`。

## 7. 长期治理原则

Anchor 将三个概念保持独立：

```text
Development Session != Command Session != MCP Transport Compatibility Session
```

开发 Session 也不等于跨会话 Memory。若未来增加项目级长期知识，应建立独立的检索式 Memory/knowledge 层，由调用方按需检索，而不是重新让开发 Session 自动继承所有历史内容。
