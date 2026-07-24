# 开发会话卡顿分析与防复发规范（2026-07-24，第二版）

## 1. 摘要

本次复盘涉及两个不同层面的“卡住”，必须分开判断：

| 事件 | 表现 | 根因 | 影响范围 |
| --- | --- | --- | --- |
| 开发会话停滞 | 最后一次工具输出后约 43 分钟没有继续执行 | 单轮内部分析失去边界，没有返回“小改动 → 验证 → 提交”的执行循环 | 开发进度停滞，工作区保留未提交修改 |
| GUI 工作区无限连接 | 启动新版 GUI 后持续显示“正在连接工作区” | Svelte `$effect` 误追踪 `statusPolling`，初始化流程自触发、自取消 | 用户无法进入 workspace 详情页 |

两者没有直接运行时因果关系，但暴露了同一种工程问题：**可靠性任务同时跨越过多子系统，缺少阶段边界和针对状态机的最小回归验证。**

当前结论：

- 开发会话停滞不是 GUI、MCP、Actions、隧道、Cargo 或 Vite 子进程卡死；
- GUI 无限连接是后来新增恢复体验时引入的独立前端状态机回归；
- 两个问题均已恢复并形成独立提交；
- 后续开发必须执行本文第 8 节的量化门禁。

## 2. 证据与结论边界

### 2.1 已证实

- 最后一次正常工具输出后，助手在内部分析阶段停留约 43 分钟；
- 期间没有保留中的 Cargo、Vite、Tauri 或测试命令会话；
- 最后计划修改的 `commands/runtime.rs` 补丁没有落盘；
- 其他已完成修改仍完整保留在 Git 工作区；
- 恢复后首次组合 `cargo check` 通过，不存在半截 Rust 文件；
- 后续专项测试和完整测试均可正常执行；
- GUI 无限连接由 `$effect` 与 `statusPolling` 的响应式依赖循环造成，已由提交 `7bd1a91` 修复。

### 2.2 高置信推断

- 开发停滞的主要原因是任务切片过大和分析阶段没有时间/步骤上限；
- 未及时提交可编译增量增加了恢复时的核对成本；
- 同一轮同时处理 Runtime、Tunnel、OAuth、Proxy、CLI 和 GUI，显著提高了上下文切换成本；
- GUI 无限连接没有在初次实现时被发现，是因为验证集中在类型检查和生产构建，没有覆盖 Svelte 生命周期的运行时行为。

### 2.3 尚不能证明

- 不能证明底层模型或 MCP 服务发生了系统级死锁；
- 不能仅凭时间间隔确定内部推理具体停在某一行代码或某一分支；
- 不能把 `DEEIX-Chat` junction 的出现归因于本次开发停滞；其来源仍未确定；
- 不能把静态构建通过视为前端响应式状态机正确。

## 3. 中断前状态

中断前已经完成：

- 运行时恢复状态模型初稿；
- OAuth Refresh Token 初稿；
- GUI 后台轮询与恢复提示初稿；
- 多次 `cargo check`；
- Runtime、OAuth 与 MCP proxy 专项测试；
- `pnpm check`。

未发生：

- 启动新的 Tauri GUI；
- 停止或重启用户当前运行中的 GUI；
- 启动新的 MCP/Actions listener；
- 启动 FRP 或 Cloudflare 隧道。

## 4. 因果链

### 4.1 开发会话停滞

```text
用户提出跨层可靠性优化
  ↓
同时展开 Runtime / Tunnel / OAuth / Proxy / CLI / GUI
  ↓
已有可编译增量，但未先提交
  ↓
继续进行跨模块边界推演
  ↓
缺少单轮时间上限、文件上限和停止条件
  ↓
内部分析长期没有返回工具执行
  ↓
会话被终止，留下大范围未提交工作区
```

### 4.2 GUI 无限连接回归

```text
$effect 读取 workspaceId
  ↓
effect 内同步调用 initializeWorkspace()
  ↓
initializeWorkspace 在第一个 await 前读取并写入 statusPolling
  ↓
Svelte 将 statusPolling 加入 effect 依赖
  ↓
statusPolling true/false 反复触发 effect
  ↓
effect 每次清空 profile 并递增 loadGeneration
  ↓
当前加载结果持续被判定为过期
  ↓
页面永久停留在“正在连接工作区”
```

修复方式：

- 使用 `queueMicrotask` 将初始化调用移出 `$effect` 依赖收集；
- `load()` 返回明确成功状态；
- 首次连接增加 12 秒截止时间；
- 超时后递增 `loadGeneration`，使迟到结果失效；
- 真实失败进入 recovering/offline，而不是永久 checking。

## 5. 促成因素

### 5.1 范围管理

1. **任务面过宽**：一个增量同时修改六个子系统。
2. **提交边界过晚**：可编译状态没有立即固定为本地提交。
3. **主任务与新问题混行**：可靠性开发途中又插入 junction 安全分析。
4. **缺少显式非目标**：没有提前声明本阶段不处理哪些边界。

### 5.2 验证策略

1. **静态验证偏重**：`pnpm check` 和 `pnpm build` 无法发现响应式自触发循环。
2. **缺少生命周期测试**：没有覆盖“路由进入 → profile 加载 → polling 启动”的状态转换。
3. **测试集中在后期**：跨模块修改完成后才做完整组合验证。
4. **没有失败路径时限**：初始连接原先没有截止时间，真实 IPC 挂起也会表现为无限等待。

### 5.3 执行节奏

1. **分析与实现交织**：已有可执行下一步时仍继续长时间推演。
2. **无单轮预算**：没有规定多久必须产生工具调用、验证结果或提交。
3. **无降级条件**：连续遇到新边界时没有暂停并缩小范围。

## 6. 恢复过程

恢复时执行：

1. 使用同一 history session key 恢复 `docs/history-session/4.md`；
2. 检查 Git 状态和所有未提交文件；
3. 确认最后一个补丁未应用，而非部分应用；
4. 先运行组合 `cargo check` 恢复到已知可编译状态；
5. 按 Runtime、OAuth、Proxy、Frontend、安全边界重新拆分；
6. 每个子能力分别运行专项测试；
7. 每个子能力创建独立本地提交；
8. 最后运行完整回归并写入 history checkpoint。

恢复原则：**先证明工作区完整，再继续功能开发；不要根据记忆直接补写最后一个计划中的修改。**

## 7. 已完成的改进

### 7.1 开发流程

- junction 安全问题、Runtime 恢复、OAuth 续约、Proxy 重连、GUI 体验分别提交；
- 连续两到三次工具调用后提供进度状态；
- 所有配置测试使用隔离目录；
- 最终完整回归通过后再写 checkpoint；
- 上次中断与后续 GUI 回归均形成独立分析或提交记录。

### 7.2 产品状态机

- listener 和隧道恢复有最大尝试次数；
- GUI 后台连接有退避和 offline 状态；
- 初始 workspace 连接有 12 秒超时；
- 下游 MCP 断联恢复连接，但不自动重放可能产生副作用的调用；
- OAuth 续约使用短期 Access Token 和轮换 Refresh Token。

### 7.3 相关提交

```text
b65537c fix: block external workspace junctions
dc4652e feat: recover disconnected runtimes
d758dc9 feat: add OAuth token renewal
9b0d19a fix: reconnect proxied MCP servers
ab0c1da feat: improve reconnect experience
f749473 docs: document recovery and OAuth renewal
7bd1a91 fix: stop workspace connection loop
```

## 8. 防复发量化门禁

以下规则适用于后续跨模块开发。

### 8.1 任务切片预算

- 一个未提交增量只处理 **一个主子系统**；
- 单个增量原则上不超过 **6 个业务文件**；
- 未提交 diff 超过 **800 行**或 **12 个文件**时，必须停止扩展范围；
- 发现第二个独立缺陷时，先记录并决定：立即单独修复，或明确延期，不允许混入原提交。

### 8.2 时间与工具预算

- 已有明确下一步时，内部分析不得连续超过 **5 分钟**而没有工具调用或状态说明；
- 连续 **3 次工具调用**后，应重新确认当前阶段目标；
- 单个命令预计超过 **30 秒**时使用 retained session，并定期读取输出；
- 连续 **2 次相同方向修复失败**时，停止继续补丁，回到最小复现和根因验证。

### 8.3 验证门禁

每个增量必须依次通过：

```text
补丁
  → 最小编译/类型检查
  → 该子系统专项测试
  → diff 检查
  → 本地提交
```

跨层状态机修改还必须增加至少一种运行时验证：

- 单元状态转换测试；
- 组件测试；
- 隔离端口端到端 smoke；
- 无法运行时，明确记录未验证项和人工验收步骤。

仅通过 `pnpm check` 或 `pnpm build`，不能作为响应式生命周期正确的充分证据。

### 8.4 停止条件

遇到以下任一情况，必须暂停当前实现：

- 工作区出现来源不明的新文件、junction 或 symlink；
- 一个补丁同时需要修改第三个独立子系统；
- 当前状态无法判断是代码问题、工具问题还是环境问题；
- retained command 没有输出且超过预期时间；
- 当前修改无法用一个明确测试描述其预期行为。

暂停后只执行：状态核对、最小复现、记录风险、重新切片。

## 9. 标准执行模板

### 开始前

```text
[ ] 恢复 history session
[ ] git status 工作区来源明确
[ ] 写出本阶段唯一目标
[ ] 写出本阶段非目标
[ ] 选定最小验证命令
```

### 每个增量

```text
[ ] 修改范围 ≤ 1 个主子系统
[ ] 立即运行最小编译/检查
[ ] 运行专项测试
[ ] 检查 diff 和意外文件
[ ] 创建本地提交
[ ] 再切换到下一个子系统
```

### 结束前

```text
[ ] 完整回归
[ ] 严格 lint / 前端检查
[ ] 清理明确创建的临时目录
[ ] git status clean
[ ] 确认未 push
[ ] history checkpoint ok=true
```

## 10. 卡顿恢复 SOP

会话中断或长时间无响应后，不直接继续最后计划，按以下顺序恢复：

1. 调用 history bootstrap，固定 session key 和目标文件；
2. `git status` 确认工作区是否干净；
3. 查看最后提交和未提交 diff；
4. 检查是否存在保留中的命令 session；
5. 确认最后补丁是完整应用、完全未应用还是部分应用；
6. 运行最小组合编译；
7. 将未提交工作拆回单一子系统；
8. 先提交已验证部分，再继续新修改；
9. 在最终回复前写入 checkpoint。

## 11. 剩余改进项

- 为 workspace 页面增加可自动运行的生命周期/状态机测试；
- 为 Tauri IPC 增加可取消请求或统一请求标识，减少仅靠 generation 忽略迟到结果；
- 将跨模块任务预算写入贡献指南或开发检查脚本；
- 对长时间无工具调用的开发会话增加外部 watchdog 提示；
- 将 OAuth Refresh Token family 持久化，覆盖服务重启后的重放检测；
- 实现 Windows 子进程操作系统级文件系统沙箱，而非仅依赖 policy-only 防护。

## 12. 独立安全问题

恢复后发现当前 workspace 根目录曾存在指向 `D:\DEEIX-Chat` 的 junction。该问题与开发会话停滞没有因果证据，但暴露了 policy-only 子进程可以沿 workspace 内链接访问另一个 workspace 的隔离缺口。

独立分析见：

```text
docs/verification/workspace-junction-isolation-analysis-2026-07-24.md
```
