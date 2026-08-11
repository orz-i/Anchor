# Anchor 长任务稳定性反馈复核（2026-08-11）

本轮针对另一个工作区在长任务中反馈的 10 项问题复核当前 Catalog 34 运行态与 `df264fb` 源码，并在隔离 worktree 中形成 Catalog 35 修正。未推送远端，也未主动停止 Windows SCM / Workspace / Gateway 服务。

## 结论矩阵

| # | 反馈 | Catalog 34 复核 | Catalog 35 处理 |
| --- | --- | --- | --- |
| 1 | Recovery 无法闭环 | 普通 `workspace_mutated=false` 且无稳定 identity 的 `PATCH_CONTEXT_MISMATCH` 已不会创建持久 Recovery；但持久 Recovery 的修正重试仍过度依赖调用方 identity/参数指纹 | 失败响应返回服务端 `recovery_key`；修正参数后可复用 key 自动 resolve；新增 task facade `resolve_recovery`，允许用后续提交/verification 证据审计式闭环 |
| 2 | `task.operation_log` output schema 错 | 当前 live Catalog 34 已实测通过，`summary` 为结构化对象，不是 string；属于旧版本反馈 | 保持结构化契约，并继续由 outputSchema contract 覆盖 |
| 3 | active Task 长期堆积 | 当前工作区存在多个长期 active task，压力反馈成立；全局自动 TTL pause 会误伤真实并行任务 | 同一 History Session 的旧 shared writer 在新 writer 创建时安全降级；worktree/独立 History peer 保留；>=8 active 和 >=12h stale 提供显式告警 |
| 4 | peer `outbox_recovery` 污染 | live 普通 `operation_log` 可复现 peer/task_id:null outbox | task-scoped 响应只附当前 task outbox；workspace-wide outbox 仅 `project_state` 等诊断入口保留 |
| 5 | Task 查询返回体过大 | `task.status` 已较紧凑，但 `task_gate_status` 默认展开完整 task + verification + gate | gate 默认 `detail=compact`；`detail=full` 保留完整诊断路径 |
| 6 | ready task 被诊断降回 verifying | verification 记录本身不会改 phase；直接 finish 在协议门禁失败时会调用 `mark_verifying` | 已验证、`ready_to_close` 且仅缺 `complete_work_session_required` 时保持状态，不因关闭协议要求倒退 |
| 7 | 已消费 terminal Session 占槽过久 | 30 分钟 retention 同时承担日志和 64-slot 生命周期，问题成立 | slot retention 60 秒、日志 retention 30 分钟分离；已消费 session 可继续读日志但不再长期占 64 槽 |
| 8 | Browser 状态瞬时不一致 | `server_info` 已做 live CDP TCP probe，并非纯 stale cache；但无采样时间，自恢复竞态难解释 | downstream status 增加 `observed_at` / freshness；health 管理响应增加 `recovered_during_probe` |
| 9 | Catalog 太重 | advanced 当前 live 为 33 local + 32 Browser proxy = 65，约 138 KB / 34.6k tokens | advanced 保持完整能力；core/read-only 按 Browser workflow 白名单收敛，非 Browser MCP proxy 不受影响；server_info 在高成本 advanced 下建议 core |
| 10 | Windows shell 默认体验 | 仅支持调用方逐次显式 `shell:"pwsh"`，反馈成立 | Workspace RuntimeConfig 新增 `preferred_shell`（auto/pwsh/powershell/cmd）；仅未显式 shell 的简单 cmd 形式采用默认值，显式调用始终优先 |

## 关键不变量

- 不以 no-op mutation、waive 或手工修改 Harness journal 解决 Recovery。
- 不通过全局 TTL 静默 pause 所有旧 active task；并行 worktree 是合法工作流。
- Task 响应瘦身必须有显式 full-detail 逃生口。
- command output retention 与 session capacity slot 分离，但未消费终态仍必须阻塞任务关闭和占用容量。
- Browser 状态必须标明采样时点/探测语义，不能把相邻时刻的状态变化伪装成同一快照。
- Tool profile 收敛只针对 Browser proxy 暴露；第三方/非 Browser MCP proxy 不因 profile 被意外裁掉。
