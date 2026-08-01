# Anchor 兼容桥接代码精炼验证

日期：2026-08-01（UTC+8）

## 目标

在旧品牌、旧配置目录和主要兼容层已经硬切的基础上，继续删除无必要的版本协商、字段别名、缺失字段默认、静默降级、fail-open 设置加载、重复门面与测试副作用。所有修改均为本地分段提交；未修改远端仓库，也未执行 Git push。

## 分段提交

1. `63cfb93` `refactor: require current MCP protocol`
   - 删除 2025-06-18 / 2025-03-26 协议协商和未知版本回落。
   - 初始化只接受 MCP 2025-11-25，旧版本返回 `-32602`。
2. `831d48f` `refactor: enforce strict persisted data models`
   - `AppData` 改为纯内存状态。
   - 新增严格 `ProfilesData` / `SecretsData`，拒绝未知字段和内联秘密。
   - FRP 前端输入使用专用 camelCase DTO；磁盘结构不再接受 `serverPort` 别名。
3. `c10b7fc` `refactor: hard-cut workspace config defaults`
   - Workspace、Tunnel、Auth、Runtime、Actions 持久化结构要求完整当前字段。
   - 删除 Actions 内联 Cloudflare Token 及其运行时回退。
4. `d324f96` `refactor: align secret store implementation`
   - `secret/keyring_store.rs` 改名为符合真实实现的 `secret/store.rs`。
   - Secret/refresh-token/FRP 测试改为内存状态，不再读写用户配置目录。
5. `10fb233` `refactor: reject invalid tool profiles`
   - 删除未知工具档位自动回落 `core`。
   - DataStore load/read/update/register 统一验证 `core`、`advanced`、`read-only`。
6. `a92b727` `refactor: remove dead compatibility surfaces`
   - 删除无引用 Tunnel/Workspace 门面、未读取 Cloudflare ready 字段、多余 `dead_code` 抑制和 CLI 空操作分支。
7. `9618859` `refactor: fail closed on settings load`
   - 删除 `AppSettings::load_or_default()`。
   - 配置损坏在 Runtime、Tunnel、下载、CLI 和健康检查链路中明确上抛。
   - 状态计算和 Tunnel 测试使用显式设置注入，消除真实用户目录耦合。
8. `9932c20` `refactor: remove stale CLI redaction wrapper`
   - Workspace 模型不再含密钥字段后，CLI show 直接序列化，删除无职责包装和空测试。
9. `ee372d8` `refactor: gate CLI-only gateway helper`
   - CLI 专用 Gateway URL helper 仅在 `cli` 或测试特性下编译，不进入桌面默认 release。

## 有意保留

- downstream MCP 未声明 outputSchema 时的安全规范化：这是协议防护，不是旧版本桥接。
- 对 SSE、旧 Harness Schema、未标记 Store 和损坏配置的明确拒绝：这是 fail-closed 诊断，不表示继续支持。
- 异步运行时、镜像下载和包管理器检测的可靠性 fallback：这些用于当前运行环境容错，不是历史迁移路径。
- README 的 `mybolide/coding-tools-mcp`：这是尚未改名的当前远端仓库/Release slug，本轮按要求不动远端。

## 最终验证

- `cargo test --all-features --tests -- --test-threads=1`
  - Rust 库：321 passed，1 ignored。
  - integration、contract、security、Harness、History、outputSchema 全部通过。
- `cargo clippy --all-targets --all-features -- -D warnings`：通过。
- `cargo check --no-default-features --features cli --bin anchor`：通过。
- `cargo check --release`：通过，无 dead-code 警告。
- `svelte-check`：0 errors，0 warnings。
- Vite/SvelteKit production build：通过。
- `pnpm desktop:build`：通过，无 Rust warning；生成：
  - `src-tauri/target/release/anchor-desktop.exe`
  - `src-tauri/target/release/bundle/msi/Anchor_0.1.23_x64_en-US.msi`
  - `src-tauri/target/release/bundle/nsis/Anchor_0.1.23_x64-setup.exe`

## 行为边界

- 当前磁盘配置必须完整符合现行结构；未知字段、缺失字段、旧别名和旧工具档位会导致明确错误。
- 当前机器已有旧 `urlModelVersion` 字段的外部配置文件未被修改；安装并启动新构建时会按硬切策略拒绝该文件，需要使用当前 Anchor 重新注册或人工转换为当前结构。
- 本轮未修改 remote URL、未创建远端提交、未推送。
