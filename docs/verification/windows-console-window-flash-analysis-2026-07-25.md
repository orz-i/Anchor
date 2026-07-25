# Windows GUI 子进程命令行窗口闪现分析（2026-07-25）

## 现象

Windows 桌面 GUI 中存在两类可见干扰：

1. 启动 MCP 服务时，某些下游 MCP、Git 检测或隧道相关控制台进程会弹出黑色命令行窗口；
2. 远程调用 `exec_command`、Git 或其他短命令时，本机会出现瞬间闪现并立即消失的控制台窗口。

命令越短，窗口越像一次闪烁；长命令则可能保持在前台更久，打断用户当前工作。

## 根因

Windows release GUI 使用 `windows_subsystem = "windows"`，应用自身没有控制台窗口。但当它通过 `Command` 启动 `cmd.exe`、PowerShell、Git、Python、Node、Cargo 或其他 console-subsystem 程序时，如果没有指定进程创建标志，Windows 会为子进程分配新的控制台窗口。

stdout/stderr 即使已经重定向到管道，也不会自动阻止控制台窗口创建。需要显式传入：

```text
CREATE_NO_WINDOW = 0x08000000
```

长时间运行且需要独立管理的隧道进程同时保留：

```text
CREATE_NEW_PROCESS_GROUP = 0x00000200
```

## 进程入口审计

| 入口 | 用户场景 | 修复前 | 修复后 |
| --- | --- | --- | --- |
| `tools/exec.rs` | 远程执行 Python、Node、PowerShell、Cargo 等 | 可能弹窗/闪现 | 无窗口，输出继续进入执行结果和 session |
| `mcp/proxy.rs` | 启动或重连下游 stdio MCP | 可能在服务启动时弹窗 | 无窗口，stderr 继续进入 MCP 日志 |
| `tools/git.rs` | 远程 Git 状态、diff、log、blame | Git 控制台可能闪现 | 无窗口，输出继续返回工具结果 |
| `harness/state.rs` | 读取项目分支和 HEAD | GUI 状态刷新时可能闪现 | 无窗口 |
| `tunnel/cloudflare.rs` | Cloudflare 隧道 | 已单独设置无窗口 | 改用统一策略，行为不变 |
| `tunnel/frp/client.rs` | FRP 隧道 | 已单独设置无窗口 | 改用统一策略，行为不变 |
| `platform/open.rs` | 用户主动点击打开工作区目录 | 启动 Explorer | 保留；这是用户请求的可见 GUI 行为 |
| `platform/macos/net.rs` | macOS 端口检测 | 不适用于 Windows | 不变 |

## 产品体验决定

内部子进程默认静默运行，不增加“显示命令行窗口”设置。

原因：

- 控制台窗口是内部实现泄漏，不是用户任务的一部分；
- 窗口闪现无法帮助判断命令是否成功；
- 错误、stdout、stderr、恢复状态和日志已有应用内承载位置；
- 增加开关会让用户承担实现细节，并造成不同入口行为不一致。

静默只抑制 Windows console window，不吞掉输出：

- `exec_command` 仍通过 stdout/stderr 管道返回；
- 长任务仍可通过 session 读取输出和写入 stdin；
- 下游 MCP stderr 仍写入 profile 日志；
- FRP/Cloudflare 日志与进程监督保持不变；
- GUI-subsystem 程序如果本身需要显示窗口，`CREATE_NO_WINDOW` 不会隐藏其正常 GUI。

## 实现

新增统一模块：

```text
src-tauri/src/platform/child_process.rs
```

提供三种策略：

```text
hide_std_console
hide_tokio_console
configure_supervised_tokio_process
```

所有内部控制台子进程在 Windows 使用统一入口。隧道不再各自复制 Windows 常量和 cfg 逻辑。

## 验证

Windows 专项测试会启动当前 Rust 测试二进制作为子进程，并在子进程中调用 `GetConsoleWindow()`：

- blocking `std::process::Command`：`hidden`；
- async `tokio::process::Command`：`hidden`；
- supervised Tokio process group：`hidden`。

该探针直接验证子进程没有获得控制台窗口，而不只是检查常量或代码路径。

还需回归：

- 远程 exec 的 stdout/stderr 和 session；
- 下游 MCP initialize、tools/list、tools/call；
- Git 与 Harness 状态；
- FRP/Cloudflare 编译和现有测试。

## 验收方式

在包含本修复的新版 GUI 中：

1. 启动配置了下游 MCP 的 workspace；
2. 从远程客户端调用一个短命令，例如 `python -c "print('ok')"`；
3. 调用 Git 状态或其他短工具；
4. 启动 FRP/Cloudflare 隧道；
5. 确认桌面没有黑色窗口或任务栏窗口闪现；
6. 确认应用内仍能看到输出、错误和日志。

本轮开发验证不启动第二个 GUI，以免干扰用户当前运行实例；实际安装版视觉验收需要在升级后的 GUI 中完成。
