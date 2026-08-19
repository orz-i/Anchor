# 项目图谱洞察

更新时间：2026-08-19（UTC+8）

## 当前定位

Anchor 是 **Rust CLI/daemon + SvelteKit Web Admin** 的本地 Workspace/MCP 控制平面。`anchor` CLI 是唯一 Rust 产品可执行目标；Web Admin 由 `anchor admin` 在 loopback 托管并嵌入生产静态资源。Tauri desktop、IPC adapter、desktop bin、bundle 配置和安装包构建链已经物理删除。

## 主执行链路

```text
Browser SvelteKit Web Admin
  → /api/v1 management HTTP
  → admin security / shared management
  → Workspace / Gateway control protocol
  → daemon-owned MCP / Actions / Tunnel runtime

anchor CLI
  → shared management / control / config engine
  → Workspace / Gateway daemon
```

MCP 工具执行仍由 Rust 工具目录、dispatcher 与 Harness 统一治理；Web Admin HTTP handler 不复制 Workspace、Gateway、Tunnel、Secret 或 Windows Service 业务规则。

## 当前关键边界

- `src/`：SvelteKit Web Admin，零 `@tauri-apps` 依赖。
- `src-tauri/src/bin/anchor.rs`：唯一产品 CLI 入口。
- `src-tauri/src/admin.rs` / `admin_daemon.rs` / `admin_service.rs`：本机 Web 管理 API 与持久托管。
- `src-tauri/src/management.rs`：CLI/Web Admin 共用管理语义。
- `src-tauri/src/daemon.rs` / `control/`：Workspace runtime 权威。
- `src-tauri/src/gateway_daemon.rs` / `gateway_control/`：Gateway runtime 权威。
- `src-tauri/src/windows_service.rs`：Windows SCM supervisor 与 owner-token 启动边界。
- `src-tauri/src/tools/` / `harness/`：开发工具、安全策略、Task/verification/journal。

## Tauri physical removal

以下内容已经删除：

- Cargo `desktop` feature、`anchor-desktop` bin、`tauri`/plugin/build dependencies。
- `tauri.conf.json`、Tauri capabilities 和 bundle icons。
- `main.rs` desktop bootstrap、`legacy_desktop.rs`、`AppState` 和 `commands/` IPC adapter。
- 前端 Tauri runtime detector、Tauri invoke/dialog adapter 与 npm `@tauri-apps/*`。
- desktop/legacy desktop npm scripts、desktop build manifest/installer helpers、`dev-desktop.cmd`。
- macOS desktop bundle reclaim 和 Windows `anchor-desktop.exe` runtime identity fixtures。

`src-tauri/tests/no_tauri_boundaries.rs` 对 active source、package/Cargo manifest、锁文件和应删除路径做机器验证。

## 分发与验证

默认且唯一 release 构建：

```bash
pnpm release:build
```

它产出 Svelte Web Admin 静态资源和 `anchor` CLI，不生成 MSI/NSIS/DMG/Tauri bundle。

发布门禁覆盖 Rust library/integration、Web Admin/privileged boundary、persistent Admin service、no-Tauri boundary、严格 Clippy、前端 check/build、release build、rustfmt 与 diff check。

## 剩余平台验收

代码迁移与 Tauri 删除已完成。仍需在 Windows 发布环境验证：

- `anchor` CLI 安装/升级后的 SCM owner-token 与 UAC Service lifecycle。
- persistent Admin Task Scheduler 的 logon/autostart、bounded restart、upgrade/recovery。
- 旧 desktop 安装环境迁移到 CLI/Web Admin 后沿用现有配置和注册信息。
- CLI 的正式安装/分发/卸载体验。

历史 `docs/specs/` 与 `docs/verification/` 中的 Tauri/desktop 字样保留为当时的规格和验证证据，不代表当前产品架构。
