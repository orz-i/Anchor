# 项目图谱洞察

更新时间：2026-08-01（UTC+8）

## 分析状态

- 本次结论来自当前 Rust/Tauri/Svelte 源码、工具目录快照、全量测试、严格 Clippy、前端生产构建和正式桌面打包。
- 未依赖历史 Python 参考实现；仓库中的 `old/` 归档已删除。
- GitNexus 旧索引数据不再作为当前结构事实来源；需要代码关系时以当前源码和 Rust 合约测试为准。

## 项目定位

Anchor 是 Rust + Tauri 2 + SvelteKit 桌面应用，同时提供独立 `anchor` CLI。每个 WorkspaceProfile 可运行 MCP Streamable HTTP、ChatGPT Actions、OAuth/Bearer 认证和 FRP/Cloudflare 隧道；单一 Gateway 可按路径隔离多个工作区。

## 主执行链路

```text
SvelteKit 页面
  → src/lib/api/* 的 Tauri invoke
  → src-tauri/src/commands/*
  → AppState
      ├─ DataStore：当前 profiles / protected secrets
      ├─ RuntimeSupervisor：MCP / Actions / tunnel 生命周期
      └─ HarnessStore：Task / baseline / verification / journal / outbox
  → tools registry / dispatcher
      ├─ MCP Streamable HTTP
      ├─ Actions OpenAPI
      └─ downstream MCP proxy
```

## 核心模块

### 数据与配置

- `src-tauri/src/data/storage.rs` 只加载当前 `data/profiles.json` 和受保护的 `data/secrets.json`。
- 配置根目录只接受当前平台的 `anchor` 目录或显式 `ANCHOR_CONFIG_DIR`。
- 明文 secrets、内联 secrets、根目录 `profiles.json` / `app_settings.json`、旧品牌目录和旧 Python 桌面配置均不再导入。
- 配置与凭据写入保留原子替换、备份恢复和平台保护；Windows 使用当前用户 DPAPI。

### Workspace 与运行时

- `src-tauri/src/workspace/` 定义当前 WorkspaceProfile、资源冲突规则和默认配置，不再包含旧配置导入模块。
- `src-tauri/src/runtime/supervisor.rs` 管理 MCP/Actions 的启动、停止、恢复、活动度和隧道联动。
- Windows 单实例锁、macOS Bundle 归属和 Linux daemon 目录只识别 Anchor 当前标识。

### MCP、Actions 与工具目录

- `src-tauri/src/mcp/` 实现 MCP 2025-11-25、OAuth、Session、Gateway 和 downstream proxy。
- `src-tauri/src/tools/registry.rs` 是 MCP、server_info 与 Actions OpenAPI 的统一工具目录事实源。
- core 目录当前为 28 个工具；文本搜索只公开 `search_text`，不再保留 `grep` / `grep_text` 服务端别名。
- `glob` 参数别名、`allowed_commands` 输出别名和 `session:<id>:full` 输出引用均已移除。

### Harness 与 History

- 源码使用 Harness Schema 5，默认存储根为系统数据目录 `anchor/harness-v5`。
- Task 必须保存单一 `expected_state`；旧 `expected_fingerprint` 回退字段不再读取。
- Baseline 使用内容寻址对象；operation/event journal 带 sequence 和 checksum；close_work_session 使用持久 outbox 恢复 History checkpoint。
- 旧 Schema 4 或未标记 Harness Store 明确返回不兼容错误，不迁移、不桥接。

### 前端

- `src/app.css` 只定义当前 canonical design tokens，例如 `--page-bg`、`--card-bg`、`--text-main`、`--primary`。
- 组件不再通过 `--color-*` 别名访问设计系统。
- Workspace 页面、设置页、健康检查、日志、OAuth、隧道和 Skill 表单已通过 Svelte 静态检查与生产构建。

## 本次硬切结果

1. 删除旧 `coding-tools-mcp` CLI 二进制、旧 daemon 路径、旧 mutex 和旧 macOS App Bundle 识别。
2. 删除旧品牌环境变量、配置目录扫描、目录复制和 Python 配置导入。
3. 删除旧配置文件布局、内联/明文凭据迁移、Gateway URL 版本迁移和旧 Tool Profile 映射。
4. 删除完整旧 Python/Coding Tools 归档，仅将仍需要的最小测试 fixture 移入 `src-tauri/tests/fixtures/`。
5. 删除 MCP 工具名、参数、响应和输出引用兼容桥接。
6. 完成 Harness Schema 5 Task 状态结构硬切。
7. 删除前端 CSS Token 和失效文档兼容层。

## 有意保留的兼容性

- MCP 协议版本协商属于标准互操作能力，不是旧品牌桥接。
- downstream MCP 缺少 outputSchema 时的安全结果规范化属于协议防护，不是旧配置迁移。
- 对旧 SSE transport 的明确拒绝用于安全诊断，不表示继续支持该 transport。
- README 中 `mybolide/coding-tools-mcp` 仅是当前远端仓库与 Release 地址；本轮按要求未改远端仓库。

## 验证结果

- Rust 全量测试：库测试 321 passed、1 ignored；全部 integration、contract、security、History、Harness 和 outputSchema 目标通过。
- `cargo clippy --all-targets --all-features -- -D warnings`：通过。
- 独立 CLI：`cargo check --no-default-features --features cli --bin anchor` 通过。
- Svelte：0 errors、0 warnings；Vite/SvelteKit 生产构建通过。
- Tauri 正式构建通过，生成 release EXE、MSI 和 NSIS 安装包。

## 当前边界

- 这是有意的硬切：旧目录、旧配置文件、明文凭据和旧工具别名不会自动恢复，需在当前 Anchor 中重新注册配置。
- 当前正在运行的 Anchor listener 仍是安装前的旧二进制；新行为需要安装本次构建并重启后生效。
- 未执行 Git push，也未修改远端仓库配置。

---
*来源：当前源码、工具目录、Git 提交、全量自动化验证与桌面打包结果。*
