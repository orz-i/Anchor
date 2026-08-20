# Desktop / Tauri 退役记录

自 2026-08-19 起，Anchor 的产品形态是 **CLI + Web Admin**。Tauri desktop 已完成 stop-publish，并进一步完成物理删除；它不再是可构建的产品 target，也不再存在于前端、Rust manifest、bundle 配置或 npm 依赖中。

## 已完成的 physical removal

- React 管理 UI 只通过 Web Admin HTTP API 工作；`invokeAdmin` 不再包含 Tauri transport 分支。
- privileged mutation 始终经过 Web prepare/confirm/one-time grant；不再存在 Tauri 直通 bypass。
- Workspace 更新统一使用 Web `stage/apply` 并发保护，不再存在 desktop 直接写配置分支。
- dialog adapter 只使用浏览器能力，不再依赖 `@tauri-apps/plugin-dialog`。
- npm 已删除全部 `@tauri-apps/*` 依赖和 desktop/tauri/legacy desktop scripts。
- Cargo 已删除 `desktop` feature、`anchor-desktop` bin、`tauri`、`tauri-plugin-dialog` 和 `tauri-build`。
- 已删除 `tauri.conf.json`、Tauri capabilities、bundle icons、desktop bootstrap、`AppState` 与 `commands/` adapter 层。
- 已删除 desktop installer/build helpers 与 `dev-desktop.cmd`。
- runtime 已删除 macOS app bundle reclaim、desktop process shutdown/restart helpers 等 desktop-only ownership 逻辑。
- Windows SCM 测试与注册语义统一使用 `anchor.exe`，不再要求 `anchor-desktop.exe`。
- `crates/anchor/tests/no_tauri_boundaries.rs` 会机器校验 active source、package/Cargo manifest、锁文件和已删除路径，防止 Tauri 回流。

因此，默认和唯一 release 路径是：

```bash
pnpm release:build
```

该命令构建 Vite/React Web Admin，并产出嵌入静态资源的 `anchor` CLI；不生成 MSI/NSIS/DMG 或桌面 bundle。

## 兼容性边界

Tauri 删除不等于配置格式重置。以下兼容性继续由 CLI/Web Admin 维护：

- `ANCHOR_CONFIG_DIR` 与当前 Anchor 配置目录语义保持不变。
- Workspace ID、OAuth client ID、认证 secrets、secret protection envelope 和 import/export 迁移协议保持不变。
- 历史验证文档可以继续出现 `anchor-desktop`/Tauri 字样，它们记录过去的构建或故障证据，不代表当前运行入口。
- 已安装的旧 desktop 二进制不再属于受支持的新 release 产物；升级/迁移应切到 `anchor` CLI 与 persistent Web Admin service。

## 仍需平台发布验收

代码删除门槛已经闭合；剩余工作是平台实机验收，而不是恢复 desktop：

1. Windows 实机验证 `anchor` CLI 安装/升级后的 SCM owner-token、UAC install/uninstall/start/stop/restart/sync。
2. Windows 实机验证 persistent Admin Task Scheduler 的 logon/autostart、bounded restart、upgrade/recovery。
3. 验证旧 desktop 安装环境迁移到 CLI/Web Admin 后，现有配置与注册信息无需重新生成。
4. 完成发布/安装分发策略，使用户直接安装 `anchor` CLI，而不是 desktop installer。

任何后续变更都不得重新添加 Tauri dependency、desktop target 或 desktop-only business semantics。
