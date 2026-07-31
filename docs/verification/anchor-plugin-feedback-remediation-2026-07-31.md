# Anchor 插件体验反馈复核与整改报告

日期：2026-07-31
工作区：`D:\anchor`
基线：`main@d18d36943015288812ef6de9a788fd66716de1bb`

## 结论

反馈总体成立，但最新代码在本轮开始前已经具备部分基础能力：高层 `begin_work_session` / `close_work_session`、长命令 `wait_command`、verification disposition、effective verification 折叠、结构化 stage commit 和基础补丁诊断。本轮针对仍存在的缺口完成了浏览器代理恢复、验证等级、统一审计、补丁诊断、Git 假修改识别、Windows 工具链预检、命令终态关联和工具组发现整改。

## 整改状态

### 1. 浏览器工具长会话稳定性：已整改

- 每个下游代理默认发布 `<prefix>__health_check`、`<prefix>__reconnect`、`<prefix>__reset_session`。
- 超时或传输断开后废弃故障连接并调度一次恢复；失败调用不自动重放，避免重复点击、提交等副作用。
- 错误区分页面加载超时、元素等待超时、工具服务超时、DevTools/CDP 通道断开、普通传输断开、下游错误和协议错误。
- 状态包含连接方式、进程 ID 或 HTTP MCP Session、协议版本、重连次数、最后错误、最后成功工具及有界页面/焦点/layer 摘要。
- 可通过 `managementTools: false` 关闭管理工具。

限制：Anchor 只聚合下游 MCP，不直接拥有浏览器 DOM 或 CDP。下游未返回焦点、dialog、popover、tooltip、dismissable layer 等字段时，Anchor 无法凭空生成这些数据。

### 2. 历史诊断失败阻断任务关闭：已整改

`exec_command` 支持：

- `verification_level`: `diagnostic`、`informational`、`required`、`blocking`。
- `supersede_previous_failures`，默认开启。
- diagnostic 失败自动标记为 `diagnostic_only`。
- informational 失败自动成为非阻断预期失败。
- 同一 `verification_kind` 的后续成功自动将早期有效失败标记为 `superseded`。
- `stage_commit.required_checks` 仍保持阻断等级。

任务关闭依据 effective verification，不再要求通过伪造一次成功执行来清除预期诊断失败。

### 3. Verification、Operation Log、浏览器事件不统一：已整改

本地工具、Harness 生命周期工具、长命令终态、代理 MCP 调用和关闭失败统一进入 Workspace Operation Log。关键字段包括：

- `trace_id` / `operation_id`
- `task_id`、History Session、MCP Session、命令 session
- `error_code`、`error_message`、错误详情
- `duration_ms`
- `verification_id`、`disposition`、`supersedes`
- `affected_task_status`

### 4. apply_patch 失败信息不足：已整改

上下文不匹配返回 `PATCH_CONTEXT_MISMATCH`，并包含：

- 文件和 hunk index
- 期望上下文与实际附近内容
- 最近匹配行和匹配置信度
- LF/CRLF 信息
- `can_retry_fuzzy`
- 推荐恢复方式

支持可选 `mode: "fuzzy"`。执行顺序仍是 exact 优先；fuzzy 只接受唯一高置信度匹配。成功结果返回 hunk 匹配模式及新增、修改、删除事务结果。

### 5. Git 假修改和基线漂移：已整改

`git_status` 可识别：

- `stat_cache_stale`
- `line_ending_or_clean_filter_only`
- 真实内容变化

诊断比较 index blob、Git clean filter 后的工作树 blob 和原始 blob。filter 后内容与 index 一致时，不再计入真实内容变化。`refresh_index: true` 仅对已确认内容一致的路径执行安全 index refresh。

Harness 基线原本已使用内容 fingerprint，本轮补齐的是用户可见的 Git 状态分类和安全恢复入口。

### 6. Windows pnpm/junction 兼容性预检：已整改

`check_exec_environment` 现在检查：

- `package.json#packageManager` 和声明版本
- Node、pnpm、Corepack、`corepack pnpm`
- `node_modules` junction/symlink 可遍历性
- `.bin` 下 Vite、TypeScript、ESLint 入口
- Cargo、Rustc、Rustup 和真实 toolchain 路径
- Docker CLI、daemon 和项目 Docker 配置
- Windows shell、架构和路径环境
- 推荐验证路线：`host`、`docker` 或 `repair_host`

本机复现了损坏的用户级 Cargo/Rustc shim：直接执行触发 Windows `os error 448`，而将真实 rustup toolchain 目录前置到 PATH 后，Cargo、Clippy、测试和正式 Tauri 构建均成功。

### 7. 浏览器 DOM 与焦点可观测性：部分整改

代理结果会提取并限制以下常见字段：当前 URL/标题、`activeElement`、`focusPath`、dialogs、popovers、tooltips、dismissable layer、visibility、bounding box、inert 和 aria-hidden ancestors。

能否得到完整 Radix layer 栈仍取决于浏览器 MCP 的返回能力。后续可在专用 browser MCP 内增加语义查询工具，再由 Anchor 原样聚合。

### 8. 会话、任务和检查点概念偏多：现有高层接口已覆盖

现有：

- `begin_work_session`：创建或恢复 History Session、创建 Harness Task、捕获基线并绑定生命周期。
- `close_work_session`：验证并关闭任务，同时持久化对应 checkpoint。

因此没有再增加语义重复的 `start_work` / `complete_work`。底层接口继续保留给恢复和高级场景。

### 9. 工具发现碎片化：已改善

`server_info` 返回紧凑工具组 manifest：

- core
- workspace
- git
- command
- task
- skills
- browser_proxy
- service

工具 Catalog 版本提升为 9。

### 10. 长命令终态与验证关联：已整改

终态结果明确包含：

- `exit_code`、`elapsed_ms`
- `stdout_complete`、`stderr_complete`
- 稳定 stdout/stderr `output_refs`
- `verification_id`、`supersedes`
- `affected_task_status`

分页读取也说明输出是否完整。

## 验证结果

- Rust 库测试：294 passed，1 ignored。
- Rust 全部测试目标和集成契约：通过。
- `cargo clippy --all-targets --all-features -- -D warnings`：通过。
- 本轮修改 Rust 文件 `rustfmt --check`：通过。
- `corepack pnpm check`：0 errors，0 warnings。
- `corepack pnpm build`：通过。
- 正式 `desktop:build`：通过。
- `git diff --check`：通过。

生成产物：

- `src-tauri/target/release/anchor-desktop.exe`
- `src-tauri/target/release/bundle/msi/Anchor_0.1.23_x64_en-US.msi`
- `src-tauri/target/release/bundle/nsis/Anchor_0.1.23_x64-setup.exe`

仓库级 `cargo fmt --check` 仍命中大量任务前既有格式差异。为避免扩大无关 diff，本轮只格式化并检查修改文件。

## 运行与发布注意事项

- 当前正在运行的 Anchor 进程仍是整改前二进制；Catalog 9、代理管理工具和新 verification 语义需安装新构建并重启后生效。
- 本轮未启动新的桌面应用、MCP listener 或隧道，也未推送远端。
- 真实浏览器长会话验收需在新构建重启后，使用实际 browser MCP 登录会话执行视口切换、页面跳转、超时恢复和管理工具调用。
