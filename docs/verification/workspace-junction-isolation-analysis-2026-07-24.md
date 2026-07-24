# Workspace Junction 隔离分析（2026-07-24）

## 现象

当前工作区 `D:\coding-tools-mcp-edit` 根目录中出现了：

```text
DEEIX-Chat -> D:\DEEIX-Chat
```

Windows 将其识别为 NTFS junction。`D:\DEEIX-Chat` 是另一个应当隔离的 workspace。

该 junction 已从 `D:\coding-tools-mcp-edit` 中移除；目标目录 `D:\DEEIX-Chat` 仍然存在。

## 已确认风险

内部文件工具与子进程工具的边界不同：

- `read_file`、`list_files`、`search_text` 等内部工具会 canonicalize 路径，并拒绝解析到当前 workspace 外部的链接。
- `exec_command` 启动的 Python、Node、PowerShell、Git 等普通子进程目前是 `policy_only`，没有操作系统级文件系统沙箱。
- 当外部 junction 位于 workspace 内时，子进程可以使用相对路径穿透到另一个 workspace。

在移除 junction 前，以下诊断命令能够从当前 workspace 列出另一个 workspace：

```text
cmd /c dir DEEIX-Chat
```

因此这是实际的隔离逃逸面，而不仅是 Git 显示了一个未跟踪目录。

## 创建来源分析

当前证据不足以确定 junction 的创建者：

- 生产代码中没有搜索到 `DEEIX-Chat`、`mklink`、junction 或目录 reparse point 创建逻辑。
- 仓库中唯一的 Windows `symlink_dir` 调用位于测试夹具复制辅助函数。
- 合规测试夹具中不存在指向 `D:\DEEIX-Chat` 的链接。
- 测试辅助函数只在临时目录中复制或创建测试链接，不应在仓库根目录生成该 junction。

因此，junction 可能来自工作区挂载/映射层、其他工具操作或人工操作，但目前不能据此做确定归因。

## 本次缓解

在 `exec_command` 启动任何普通子进程前，新增 workspace 根目录边界检查：

1. 检查顶层 symlink 和 Windows reparse point；
2. canonicalize 链接目标；
3. 目标位于当前 workspace 外部时，拒绝启动子进程；
4. 返回 `WORKSPACE_LINK_ESCAPE`，并只报告链接名称，不读取目标内容。

新增安全测试会在临时 workspace 创建一个外部目录链接，确认子进程在启动前被阻止。

## 剩余边界

本次是针对已观察到的顶层 junction 的即时缓解，不等价于完整文件系统沙箱：

- 子进程运行期间自行创建链接并立即访问，仍需要操作系统级沙箱才能完全阻止。
- 深层目录中的外部链接尚未通过全树扫描阻断；每次执行前扫描整个大型 workspace 会带来明显性能成本。
- Windows 原生进程令牌、ACL/AppContainer 或其他文件系统授权仍是最终隔离方案。

在完整沙箱落地前，应保持：

- 不在 workspace 根目录放置指向其他 workspace 的 junction/symlink；
- 将 `sandbox_enforced=false` 视为真实安全边界提示；
- 发现 `WORKSPACE_LINK_ESCAPE` 时先移除链接本身，不删除其目标目录。
