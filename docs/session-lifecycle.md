# Session 生命周期与内容管理

Coding Tools MCP 同时存在三类用途不同的 Session。它们必须分开管理，不能共享标识、过期策略或内容预算。

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
| 最大保留命令 Session | 64 |
| 已结束 Session 无访问保留期 | 30 分钟 |
| stdout 环形缓冲区 | 1 MiB |
| stderr 环形缓冲区 | 1 MiB |

达到 Session 容量时，服务器会终止刚启动但无法登记的子进程，然后返回可重试的 `SESSION_LIMIT_REACHED`，避免产生孤儿进程。

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

## 4. 验证与恢复

`history_session_validate` 返回：

- 编号缺口、重复 session key、无效文件和空文件；
- active、paused、completed、unknown 状态数量；
- 文档数、总字节数和最大单文件大小；
- 当前容量上限；
- 索引状态及是否重建。

未知历史状态不会被静默当作 completed。验证会给出警告；下次按相同 key bootstrap 时会重新激活为 active。

## 5. 兼容与演进

当前实现遵循已发布的 Streamable HTTP Session 模型，同时把业务状态放在显式工具参数和持久文件中。未来协议若进一步弱化传输 Session，命令 Session 与历史 Session 不需要随传输标识迁移：

- 命令进程继续使用自身 `session_id`；
- 历史归档继续使用稳定 `session_key`；
- 工作区与权限边界继续由 WorkspaceProfile 和工具策略控制。
