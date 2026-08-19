# Desktop / Tauri 退役门槛

自 2026-08-19 起，Anchor 的默认产品形态是 **CLI + Web Admin**。Tauri desktop 仅作为显式 legacy compatibility target 保留，不再属于默认开发、构建或发布路径。

## 当前已满足的 stop-publish gate

- Cargo `default` feature 已切换为 `cli`；普通 `cargo build/check/test` 不再启用 Tauri。
- `tauri-build` 已改为 optional build dependency，仅在显式 `desktop` feature 下启用。
- `pnpm start` 启动 Web Admin；`pnpm release:build` 构建 CLI + 嵌入式 Web Admin。
- Tauri 入口只保留在 `legacy:desktop*` / `legacy:tauri` 脚本。旧 `desktop*` / `tauri` 脚本仅作为带弃用提示的兼容别名。
- Tauri bootstrap、single-instance 与 invoke handler 注册已集中到 `src-tauri/src/legacy_desktop.rs`；核心 `lib.rs` 不再直接创建 Tauri Builder。
- 业务前端中的 Tauri import 只允许位于 `src/lib/api/invoke.ts` 与 `src/lib/platform/**` adapter 边界。
- Desktop `AppState` 不持有 Workspace/Gateway/Tunnel runtime supervisor；退出 desktop 不影响后台 runtime。
- Web Admin 已覆盖普通管理能力、Secret/FRP、Software 与 Windows Service privileged executor，并可由 persistent `anchor admin` service 独立托管。
- `src-tauri/tests/desktop_retirement_boundaries.rs` 对以上边界做机器校验，防止默认发布路径重新依赖 Tauri。

因此，**新的 release gate 不应再要求生成 `anchor-desktop`、MSI/NSIS/DMG 等 Tauri 产物**。需要兼容性回归时必须显式运行 legacy desktop target。

## Legacy desktop 验证

仅在具备 Tauri 系统依赖的主机上执行：

```bash
pnpm legacy:desktop
pnpm legacy:desktop:build
```

兼容别名仍可暂时使用，但会打印弃用提示：

```bash
pnpm desktop
pnpm desktop:build
```

Linux desktop 编译需要 GTK/GLib/WebKit 等 Tauri prerequisites；缺少这些系统包不影响 CLI/Web Admin 的默认 release gate。

## 彻底删除 Tauri 前仍需完成

1. 完成一个明确的 legacy 弃用窗口，并确认不再需要已安装 desktop 作为迁移入口。
2. 删除 Tauri transport/dialog adapter，令浏览器成为 Svelte UI 的唯一运行宿主。
3. 删除 `src-tauri/src/commands/`、`app_state.rs`、`legacy_desktop.rs`、desktop-only DataStore/runtime compatibility helpers。
4. 删除 Cargo `desktop` feature、`anchor-desktop` bin、`tauri` / `tauri-plugin-dialog` / `tauri-build` 依赖及 `tauri.conf.json`。
5. 删除 npm `@tauri-apps/*` 依赖、legacy desktop scripts、desktop build manifest/installer helper 与 `dev-desktop.cmd`。
6. 清理仅为历史 desktop executable/bundle identity 保留的品牌常量、macOS bundle ownership 与 Windows legacy executable 兼容测试。
7. 在 Windows 实机完成 CLI/Web Admin + SCM/UAC + Admin Task Scheduler 的发布验收，确认不依赖 desktop 安装包即可完成安装、升级、恢复和管理。

以上项目未完成前，legacy desktop 可以继续编译，但不得新增业务能力；新功能只能进入 CLI/shared management/Web Admin。
