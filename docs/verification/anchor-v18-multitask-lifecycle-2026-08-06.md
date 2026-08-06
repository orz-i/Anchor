# Anchor v1.8 多任务生命周期复核

日期：2026-08-06  
安装态基线：Catalog v28 / `64d26a8ac1ece085039158a97ebbf21c2aa94bb7`  
目标源码：Catalog v30

## 结论

同一工作区的新任务此前会把同写域的 active/verifying 任务自动改成 paused，将“任务是否仍在推进”与“共享工作树当前默认路由”错误耦合。修复后允许多个任务同时保持 active/verifying；共享工作树继续串行写入，worktree 任务继续独立并行。

## 行为矩阵

| 场景 | 修复前 | 修复后 |
| --- | --- | --- |
| shared 模式新建第二个任务 | 前序任务自动 paused | 两个任务均保持 active，第二个成为默认路由 |
| `switch_task` | 目标 active，其他同域任务 paused | 只切换默认路由，其他任务状态保持不变 |
| 显式暂停 | 与写者切换混用 | 仅 `pause_task` 改为 paused |
| 前序任务有运行命令时新建任务 | 新任务创建被拒绝 | 新任务可创建并读取；peer 写入暂时返回 `WORKSPACE_WRITER_BUSY` |
| shared 模式多任务写入 | 依赖任务暂停/恢复转移 | 工作区写锁串行化，成功写入后同步同域 expected baseline |
| worktree 模式 | 已有隔离实现，未覆盖本轮实机复核 | 两个任务均 active；命令租约、文件和 Git 路由互相隔离 |

## 实现要点

- `start_task_configured`、`switch_task` 和 paused 任务自动恢复不再修改 peer TaskStatus。
- Workspace state 的 `active_task_id` 保留为默认路由选择；`active_task_ids` 保存全部 active/verifying 任务。
- shared 任务创建不再被其他任务的运行命令阻止；真正的写工具仍在 dispatch 层执行同写域运行命令冲突检查。
- `parallel_tasks_preserved` 对 shared 与 worktree 切换均返回 true。
- Catalog 升级到 v30，并更新 `pause_current_and_start` 的废弃参数说明及 `wait_command` 终态输出契约。
- `wait_command` 现在返回原命令与 resolved cwd；同一 verification identity 的后续成功若 supersede 旧失败，会同步解除该失败绑定的开放 Recovery。

## 验证

- shared 双任务保持 active、会话绑定独立、并发 Patch 串行成功。
- 前序运行命令期间可创建 peer active 任务；peer Patch 被阻止，命令结束后可成功写入。
- worktree 双任务保持 active；一个任务运行命令时另一个 worktree 可独立 Patch。
- Harness 契约套件串行 50/50 通过。
- retained wait supersede Recovery 专项测试通过。
- Rust 全 targets/all features：library 406 passed、1 ignored；integration 30/26/4/51/21/7 全部通过。
- 严格 Clippy `-D warnings`、`pnpm check` 与 `pnpm build` 通过。
- Catalog v30 effective snapshot 通过。

Windows 并行测试曾出现一次临时 worktree 删除 `Access Denied`；该测试单独串行复测通过，完整 Harness 套件以 `--test-threads=1` 再次 50/50 通过，判定为临时文件句柄竞争而非生命周期逻辑回归。
