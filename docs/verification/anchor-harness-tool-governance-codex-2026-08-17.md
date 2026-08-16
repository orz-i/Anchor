# Anchor Harness 工具治理复核（Codex 对标，2026-08-17）

## 范围

本轮在独立 worktree `feature/harness-governance-codex-20260817` 中进行，目标不是继续机械合并工具，而是治理公开工具契约与 Harness 失败生命周期：

1. domain facade 的公开 schema 必须更准确地表达 operation 参数边界；
2. 无实际执行、无工作区变更的命令前置失败不应污染 durable Task Recovery；
3. 保持既有 profile、facade 名称、调用形状和错误码兼容；
4. 不以元数据伪装尚未具备的 OS 级 sandbox 能力。

设计依据见 `docs/specs/harness-tool-governance-codex-20260817/design.md`。

## Codex 对标结论

参考 OpenAI 公开的 Codex harness / sandbox / App Server 资料，本轮采用以下可迁移原则：

- execution boundary 与 approval/review policy 是两个独立层次；
- shell/file/MCP/skill 应落在一致的 Harness 策略模型下；
- 低风险控制面/探测不应被等同于已经执行的业务动作；
- public tool contract 应尽量在执行前帮助 agent 选择合法动作，而不是依赖隐藏的后置失败；
- 持久化 Recovery 应用于需要恢复的失败动作，而不是充当所有工具错误的通用日志。

Anchor 当前命令执行仍明确报告 `execution_boundary=policy_only`、`sandbox_enforced=false`。本轮未把策略边界包装成 OS sandbox；Linux Landlock/seccomp/bubblewrap 与 Windows 进程级 sandbox 需要单独的跨平台安全设计。

## 实现

### Catalog 38：operation-aware facade schema

- `CATALOG_VERSION`：37 → 38，用于让缓存工具定义的客户端重新获取 schema。
- 不增加 facade，不增加公开工具数量。
- 保持顶层 object schema，不引入 `oneOf` / `anyOf` / `$ref`，继续满足现有 ChatGPT 工具目录兼容约束。
- facade 每个合并参数增加 operation applicability 描述，例如 Git `include_ignored` 明确只适用于 `clean`。
- operation 描述明确要求调用者不要为当前 operation 发送无关参数。
- delegated canonical validator 仍是最终权威；失败时额外返回：
  - `stage=facade_operation_schema`
  - `reason=facade_operation_arguments_invalid`
  - `allowed_arguments`
  - `required_arguments`
  - `delegated_tool`
  - `canonical_error`
- 顶层错误码继续保持既有 `INVALID_TOOL_ARGUMENTS`，避免破坏按错误码分支的旧客户端。

### execution-aware Task Recovery

`exec_command` 如果在返回错误前没有进入已规范化的执行结果，现在会先补齐 preflight 终态：

- `execution_started=false`
- `status=rejected`
- `termination_reason=command_rejected`

因此现有 Recovery eligibility 能正确跳过“没有工作区变更、没有显式 retry identity、进程明确未启动”的输入/环境前置失败。

真实启动后的命令失败仍保持原行为：即使没有文件变更，只要进程已经启动并失败，仍会打开 Task Recovery；发生变更或 verification failure 的路径也没有放宽。

## Live 发现与修复证据

治理前通过真实工具调用复现：

- `git` facade 的联合 schema 暴露 `include_ignored`，但 `operation=status` 的 canonical `git_status` 不接受该参数，出现“公共 schema 看似允许、leaf validator 拒绝”的错配。
- `which bwrap` 用于探测 Linux sandbox backend；本机未安装 `bwrap`，命令没有真正启动且没有工作区变更，但旧路径仍生成 Task Recovery。该 Recovery 已作为诊断性探测手工闭环，本轮实现随后补上统一 preflight normalization。

## S2 / S3 验证

- `cargo check --no-default-features --features cli --lib`：passed。
- `cargo fmt --all -- --check`：passed。
- `git diff --check`：passed（S2 实现提交前）。
- `cargo test --no-default-features --features cli --test call_tool_contract -- --test-threads=1`：**36 passed / 0 failed**。

新增/强化的 integration contract 覆盖：

- facade schema 暴露参数适用 operation；
- `git.status + include_ignored` 返回 operation-scoped 诊断，同时保留 `INVALID_TOOL_ARGUMENTS`；
- Git/Task/commit_stage 缺少 operation 必填参数时返回 `required_arguments`；
- `which <missing-program>` 返回 `execution_started=false`，且不生成 Task Recovery；
- 已启动并 exit non-zero 的命令仍生成 Task Recovery；
- 既有 profile/tool exposure、命令 session、Git facade、环境/cwd facade 等整套 call-tool contract 同时通过。

## 当前 Linux 环境的既有验证限制

这些失败均发生在本轮代码验证之外，且未被伪装成通过：

1. `cargo check --all-targets --all-features` 在 `glib-sys` build script 处停止：当前 Linux 环境缺少 `glib-2.0.pc` / GTK 桌面构建系统依赖。
2. `cargo check --no-default-features --features cli --all-targets` 会编译 library `cfg(test)`，命中仓库既有 Linux 测试编译问题：
   - `src/data/store.rs` 测试代码找不到 `shared_value_for_key`；
   - `src/tunnel/supervisor.rs` 测试代码调用当前平台下不存在的 `frp_route_matches`。
3. 改为 production library 与 integration target 后，本轮相关实现和公共调用链均可编译并通过测试。

最终收口阶段会继续运行当前环境可执行的 Harness、catalog/contract、Clippy、前端 check/build 与 Git 完整性验证，并把上述平台前置条件作为独立限制保留。
