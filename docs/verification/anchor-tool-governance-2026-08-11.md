# Anchor 工具与下游 MCP 治理优化

- 日期：2026-08-11
- 目标版本：Catalog 36
- 范围：Anchor 本地工具目录、下游 MCP 聚合、profile、目录预算、可观测性与配置入口

## 审计结论

Catalog 35 已完成 Git、Task、Slice、staged commit 与 Skill 的 facade 收敛，但仍存在两类膨胀来源：

1. 本地环境诊断与 cwd 仍以多个 leaf 直接公开；这些 leaf 属于同一领域，公开层没有必要重复占用工具槽位。
2. 下游 MCP 默认按 `一个下游 tool = 一个上游 tool` 全量聚合。`includeTools`、`excludeTools`、`maxTools` 只能由操作者手工配置；在未配置时，一个高 fan-out MCP 可以一次把几十到数百个 schema 注入公共目录。Browser 在 advanced profile 下就是当前的真实例子：Catalog 35 为 33 个本地工具 + 32 个 Browser proxy，共 65 个工具。

此外确认了三个治理问题：

- 多个下游并发连接时，旧实现按连接完成顺序合并，公共名称冲突的胜出者理论上受时序影响；最终排序只能稳定显示顺序，不能稳定冲突归属。
- 下游 `title`/`description` 虽已有单工具总大小保护，但 description 最多可进入 8 KiB，大量工具时纯说明文字仍会占用显著目录上下文。
- 下游 `notifications/tools/list_changed` 只记录“重启 listener”日志；公共目录在 listener 生命周期内固定，这是安全的，但说明工具目录治理必须在首次发布前完成，不能依赖后续分页或刷新补救。

## Catalog 36 治理规则

### 1. 本地 domain facade

新增两个公开 facade，并保留原 leaf 作为内部 canonical contract：

- `environment`
  - `check` → `check_exec_environment`
  - `health` → `exec_health_check`
  - `cost` → `command_cost_explain`
- `cwd`
  - `get` → `get_default_cwd`
  - `set` → `set_default_cwd`

Profile 仍从 leaf 权限计算 operation enum，因此 read-only 的 `cwd` 只有 `get`，`environment` 只有 `check`/`cost`；facade 不产生权限提升。

本地公开数量由：

| Profile | Catalog 35 | Catalog 36 | 变化 |
| --- | ---: | ---: | ---: |
| advanced | 33 | 30 | -3 |
| core | 28 | 26 | -2 |
| read-only | 15 | 14 | -1 |

`anchor-core` lazy-schema 分组继续保持最多 20 个工具；`environment` 归入更窄的 `anchor-command` 分组，避免 facade 收敛反而扩大默认 schema bundle。

本轮没有继续把所有本地工具机械合并：文件读取/搜索/Patch、retained command session、History/Work Session 和 Browser build 等工具具有不同生命周期、权限或高频强类型 schema。将它们塞进一个大型 union facade 虽然会降低“工具个数”，却可能扩大单 schema、降低参数可发现性并混淆副作用边界。治理指标因此同时看工具数量、schema 体积、领域内聚性和权限一致性，而不是追求最少工具数。

### 2. 下游 exposure policy

新增 `exposureMode`：

- `auto`：默认。优先使用显式 `includeTools`/`maxTools`；否则应用自动治理。
- `full`：操作者明确审阅后请求完整下游目录，仍受每服务器 256 和全局/ChatGPT 目录预算保护。

`auto` 下：

- 配置了 `includeTools`：按显式 allowlist 发布。
- 配置了 `maxTools`：按稳定工具名排序后发布前 N 个。
- 已知 `my-agent-browser`：默认只发布 18 个常用下游交互工具；加上 3 个 Anchor management tool，共 21 个 Browser proxy。
- 其他 MCP：未配置 `includeTools`/`maxTools` 且过滤后超过 24 个工具时拒绝初始化，并要求显式选择或 `exposureMode: full`。不进行无语义的静默截断。

已知 Browser 默认工作流保留：页面列表/选择、新建/关闭、导航、snapshot/screenshot、evaluate、click、fill/fill_form、wait、键盘/文本/hover/dialog/upload/resize。性能 trace、heap snapshot、Lighthouse、console/network 明细等低频诊断能力不再默认占用工具槽位；需要时可使用显式 allowlist 或 `exposureMode: full`。

### 3. 下游 schema 上下文预算

- 保留完整、经验证的 `inputSchema`/`outputSchema`，不为压缩 token 破坏强类型参数合同。
- 下游 title 最多保留 256 bytes。
- 下游 description 最多保留 2048 bytes，并按 UTF-8 字符边界截断。
- annotations 继续按保守策略发布，不信任下游自报的无副作用声明。

### 4. 确定性与可观测性

- 下游仍可并发连接，但所有连接结果先收集，再按 server name 稳定排序后合并；同名公共工具的冲突归属不再受连接完成时序影响。
- `server_info.downstream_mcp.servers[]` 增加：
  - `exposure_mode`
  - `selection_source`
  - `discovered_tool_count`
  - `selected_downstream_tool_count`
  - `filtered_tool_count`
  - `truncated_tool_count`
- 初始化失败或被 exposure policy 拒绝的下游不会只留在日志里；`downstream_mcp.unavailable_server_count` 与 `unavailable_servers[]` 返回服务器名和错误原因，便于定位需要补 allowlist/max/full 的配置。
- 启动日志同步记录 exposure mode 和 selection source。

## 真实目录预期

以当前 Catalog 35 live Browser 目录为基准：

- 旧：33 local + 32 Browser proxy = 65 tools。
- Catalog 36 默认：30 local + 21 Browser proxy = 51 tools。
- 工具数量减少 14，约 21.5%。

这是默认治理结果，不是硬删能力：低频 Browser 工具仍可通过显式 exposure policy 发布。

## 协议边界

Anchor 继续在一个 listener 生命周期内固定公共 `tools/list`，不在调用过程中动态改变已发布 schema；下游 catalog drift 需要重新协商 listener。工具选择发生在公共目录发布前，分页只承担响应传输，不作为工具治理机制。

## 验证要求

- facade profile operation 枚举和隐藏 leaf 契约。
- generic >24 auto 拒绝；显式 full 允许。
- Browser auto 过滤低频工具。
- title/description 上下文预算。
- Catalog 36 快照与 30/26/14 本地数量。
- advanced + 默认 Browser = 51。
- Rust 全量 check/test/Clippy、前端 check/build、rustfmt、diff check。

## 验证结果

- MCP proxy 单元测试：38/38 passed，覆盖 auto/full、Browser workflow、metadata budget、catalog drift、并发/取消、确定性重复名归属与 unavailable server 可观测性。
- Registry：7/7 passed；Catalog：12/12 passed；MCP server：23/23 passed；call-tool contract：34/34 passed。
- `cargo check --all-targets --all-features` passed。
- `cargo clippy --all-targets --all-features -- -D warnings` passed。
- `cargo test --all-targets --all-features -- --test-threads=1` passed：主库 540 passed / 1 ignored；其余集成套件 34/34、26/26、4/4、Harness 61/61、History 21/21、output-schema 7/7。
- `pnpm check` passed：0 errors / 0 warnings。
- `pnpm build` passed。
- Rust `rustfmt --check` passed。

全量测试过程中发现两类旧测试合同仍直接调用已经隐藏的 `get_default_cwd`/`set_default_cwd` leaf，以及 `server_info` 新增 unavailable-server 字段未同步 outputSchema。均已改为公开 `cwd` facade / 补齐 schema，并通过同一 blocking verification key 重试；旧失败已被成功验证 supersede，对应 Recovery 已闭环。

