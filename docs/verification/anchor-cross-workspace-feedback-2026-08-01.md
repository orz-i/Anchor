# Anchor 跨工作区反馈复核（2026-08-01）

## 结论

本轮按当前 `main` 源码逐项复核。旧反馈中的 Browser 输出 Schema、付费命令误判、Patch 诊断、任务活动自动恢复、任务关闭结构化错误等问题已在此前版本修复；本轮补齐了仍存在的恢复与并发缺口。

## 逐项状态

| # | 反馈 | 复核结果 | 本轮处理 |
|---|---|---|---|
| 1 | 异常 junction/symlink 导致命令自锁 | **原问题仍存在** | 新增 `remove_path`。它不启动子进程、不递归扫描工作区、不跟随最终链接，可删除失效链接本体并保留目标。链接扫描错误直接返回 `recovery_tool` 与参数。 |
| 2 | 缺少删除和 Git 回退 | **原问题仍存在** | 新增 `remove_path`、`git_reset`、`git_revert`、`git_clean`。Hard reset、全仓 clean 和删除 ignored 文件受 dangerous 模式保护；revert 默认 `--no-commit` 并支持 abort。 |
| 3 | symlink 语义不一致 | **部分存在** | 文件删除与 Git 路径解析统一使用“父目录规范化、最终链接不跟随”的词法语义。Patch 仍不编辑链接目标，链接恢复统一走 `remove_path`，避免 patch/git 工具各自猜测。 |
| 4 | 活动任务生命周期刚性 | **此前自动恢复已修复；多任务切换仍缺失** | `begin_work_session` 支持 `pause_current_and_start`，`start_task` 支持 `pause_current`；新增 `switch_task`，并由持久化 `active_task_id` 决定当前任务。目标不匹配仍默认 fail closed。 |
| 5 | 并发下基线 CAS 频繁过期 | **原问题仍存在** | 新增 `accept_latest_baseline`，在一次调用内双重捕获稳定状态并进行最多 10 次有界重试。持续写入时明确返回 `BASELINE_UNSTABLE`，不会错误接受变化中的状态。 |
| 6 | close/finish 空错误 | **当前版本已修复** | 现有统一错误对象包含 code、message、category、retryable 与 details；输出 Schema 合约覆盖 Harness 和 History 高价值工具。本轮未重复实现。 |
| 7 | Browser 代理不稳定 | **Schema/管理工具此前已修复；超时重连仍存在** | 请求超时现在只取消当前调用，不再销毁代理连接；区分 stale element、wait/navigation/script timeout，并返回页面、元素或连接层恢复提示。连接真实断开时才重连。Page ID 在真实重连后仍可能变化，调用者必须重新 `list_pages`；DOM UID 变化后必须重新 snapshot，Anchor 不自动重放可能有副作用的操作。 |
| 8 | 本地命令误判付费 | **当前版本已修复** | 分类已基于项目规则、可执行程序、精确 live 标记、已知付费 API 主机、`cost_intent` 与 `network_mode`；普通参数中的 live/model/credential 文本不再单独触发。 |
| 9 | Patch 对上下文/换行敏感 | **大部分已修复** | 现有 Patch 支持 CRLF 保留、exact/fuzzy、最近上下文/行列/片段诊断和写前事务验证。仍保持整包原子提交，不实现“跳过冲突 hunk”；也未引入语言专用 AST 重写，以避免静默部分成功。 |
| 10 | Skill 资源发现与读取不一致 | **协议资源清单已有，直接工具缺失** | 新增 `list_skill_resources`，返回 `read_skill_resource` 实际允许读取的精确资源/脚本清单、URI、digest、MIME、readArgs 和分页信息。 |
| 11 | 重复错误噪声、无根因 | **原问题仍存在** | `operation_log` 新增 `diagnostics`，按链接路径或错误代码聚合重复失败，返回根因、次数、受影响工具、样例消息和结构化恢复操作。 |

## 安全边界

- `remove_path` 只允许工作区内路径，拒绝 `.git`、`.github` 等保护路径；普通目录递归删除需要 dangerous 模式。
- `git_reset mode=hard`、仓库级 `git_clean`、删除 ignored 文件必须由受信任控制面启用 dangerous 模式。
- Browser 超时不会自动重放调用；真实连接重建后不会伪造旧 Page ID 或 DOM UID。
- `accept_latest_baseline` 只接受连续两次捕获完全一致的 branch、HEAD 和 fingerprint；持续变化时失败而非放宽一致性。
- Patch 继续保持事务性全有或全无，不自动跳过冲突 hunk。

## 分阶段提交

1. `f7d77ff3f2469c5ab7247c8caf6b26204b52e0c4` — `feat: add safe workspace recovery tools`
2. `940b7c2c0bb336d757ccf4cd750dc5f948839a61` — `feat: support task handoff and stable baselines`
3. `ee709f87d035daff358a3c97428cf1dff2da6029` — `feat: add resource and failure diagnostics`
4. Browser 上下文稳定性与本复核报告在同一收尾提交中。
