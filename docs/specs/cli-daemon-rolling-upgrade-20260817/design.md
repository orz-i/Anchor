# CLI / daemon 滚动升级设计（2026-08-17）

## 目标

把当前“新版 CLI 先停止旧 daemon，再从 `current_exe()` 启动新 daemon”的安全重启，提升为可观察、可验证、失败可回滚的 **runtime rollout**。

第一版不下载或替换 CLI 本体；操作者先安装/运行当前构建，再执行 `anchor upgrade ...` 把正在运行的 Workspace/Gateway daemon 切换到当前构建。CLI 自更新下载器属于后续独立阶段。

## 现状边界

当前 Workspace/Gateway 都占用稳定业务端口并通过单实例 lock/state 保证唯一运行权威。`prepare_restart` 已允许新客户端对受支持的旧协议执行 lifecycle drain，但现有 restart 路径为：

```text
old daemon -> prepare_restart -> wait PID exit -> spawn current_exe -> readiness
```

因此存在三个缺口：

1. 新构建启动失败后不会自动恢复旧构建；
2. restart 输出没有 build identity、切换耗时和 rollback 状态；
3. CLI 没有面向多个 Workspace/Gateway 的统一升级事务入口。

固定端口下如果不引入 socket/FD handoff 或稳定代理层，新旧 listener 不能同时绑定同一地址。因此本轮明确实现 **bounded-outage rolling replacement**，不宣称 zero-downtime。停机窗口从旧 daemon 释放端口开始，到新 daemon readiness 通过结束。

## CLI 契约

新增：

```text
anchor upgrade <workspace> [workspace ...] [--gateway] [--timeout SECONDS] [--force] [--dry-run] [--allow-no-rollback]
anchor upgrade --gateway [--timeout SECONDS] [--force] [--dry-run] [--allow-no-rollback]
anchor upgrade --all [--timeout SECONDS] [--force] [--dry-run] [--allow-no-rollback]
```

- 显式 Workspace selector 只升级对应正在运行的 daemon；未运行视为 skipped。
- `--gateway` 把全局 Gateway daemon 加入目标。
- `--all` 选择所有正在运行的 Workspace daemon，并包含正在运行的 Gateway；不能与显式 Workspace selector 混用。
- `--dry-run` 只输出 build/rollback/SCM ownership 计划，不发送 lifecycle 写请求。
- 默认要求存在可信 rollback executable；无法准备 rollback 时 fail-closed。
- `--allow-no-rollback` 是显式降级开关，只允许在操作者接受“新构建失败后可能保持停止状态”时继续。
- `--force` 只控制旧/失败进程在超时后的强制终止，不等于批准无 rollback。

## Rollback executable

升级前必须在旧 daemon 仍存活时准备 rollback executable：

### Linux

从 `/proc/<pid>/exe` 复制真实运行映像到 Anchor 私有 runtime 目录。即使磁盘上的原路径已被新版原子替换，`/proc/<pid>/exe` 仍指向旧进程实际映像，因此可提供可信 rollback。

成功切换到新构建后删除临时 rollback 副本；新构建失败并成功回滚时保留副本，因为回滚 daemon 的 `current_exe()` 将指向该副本，后续再次升级仍可继续使用。

### Windows / 其他受支持平台

优先使用旧 state 记录的 `executablePath`。若它与当前 CLI 是同一文件且 build identity 不同，则无法证明路径仍指向旧映像，默认 fail-closed；仅 `--allow-no-rollback` 可继续。

Windows SCM 正在管理目标时不能由普通 CLI 与 supervisor 并行争夺启动权。此场景升级计划必须 fail-closed 并要求先使用 `anchor service install` 更新 SCM supervisor；SCM 继续作为唯一恢复权威。

## 单目标 rollout 状态机

```text
inspect
  -> already_current / stopped
  -> prepare rollback executable
  -> prepare_restart (允许受控旧协议 lifecycle fallback)
  -> wait old PID exit
  -> spawn current executable
  -> readiness: state PID + owned ports + control ping
  -> verify BuildIdentity == current
  -> success: discard temporary rollback

new spawn/readiness/build verification failure
  -> terminate failed new PID if necessary
  -> spawn rollback executable with original service/tunnel/routes
  -> readiness
  -> report rolled_back and preserve rollback artifact
```

如果 rollback 自身失败，返回同时包含 primary failure 与 rollback failure 的错误，禁止把目标误报为 upgraded。

## 可观察性

每个目标结果至少包含：

- target kind / workspace ID or gateway；
- status: `planned | skipped | already_current | upgraded | rolled_back | failed`；
- previous/new PID；
- previous/current build identity；
- rollback available / attempted / succeeded；
- outage milliseconds（实际 rollout）；
- executable source（current / rollback snapshot）；
- failure / rollback failure（如有）。

JSON 模式返回完整结果数组；文本模式逐目标输出摘要。任一目标 `failed` 或 `rolled_back` 时命令返回非零退出码，避免自动化把部分升级当作成功。

## 安全与协议

1. 不扩大旧协议权限：只有既有 `version` 与 lifecycle drain 可以兼容旧协议；普通写请求继续 exact-version fail-closed。
2. 不并行启动两个固定端口 listener；新进程只能在旧 PID 完全退出后启动。
3. rollback 必须恢复旧 daemon 原有 service/tunnel/routes 选择。
4. readiness 不只看进程存活，必须同时验证端口归属和 control ping。
5. Windows SCM 运行时保持单一 supervisor 权威，不允许普通 CLI 绕过其 reconcile loop 做竞争式 rollout。

## 后续真正零停机阶段

真正 zero-downtime 需要额外基础设施之一：

- Unix socket/listener FD 继承与 Windows 等价 handle handoff；或
- 稳定前置代理监听固定端口，backend daemon 使用可变内部端口并原子切流。

这两种都会改变当前端口 ownership 和故障域，本轮不把它们隐藏在普通 restart 中。
