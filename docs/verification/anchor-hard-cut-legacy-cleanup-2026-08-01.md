# Anchor 改名与兼容层硬切验证

日期：2026-08-01（UTC+8）

## 目标

在 Harness Schema 5 硬切基础上，彻底移除 Coding Tools MCP 改名遗留、旧配置路径和旧兼容桥接；采用代码硬切，不迁移旧数据，不修改或推送远端仓库。

## 分段提交

| Commit | 内容 |
|---|---|
| `a47c19a` | 删除旧 CLI、daemon、mutex 和 macOS Bundle 运行时桥接 |
| `75f8e0f` | 删除旧品牌配置环境变量、目录扫描与 Python 配置导入 |
| `e533478` | 硬切旧配置文件、明文/内联凭据、Gateway/Profile 迁移格式 |
| `7f9fb84` | 删除旧 Python/Coding Tools 归档并迁移必要测试 fixture |
| `fccac02` | 删除旧 MCP 工具、参数、响应和 output_ref 桥接 |
| `43cc804` | 硬切 Harness Schema 5 Task expected_state |
| `2613361` | 删除旧 CSS Token 与失效文档兼容层 |
| `972b335` | 修复最终严格 Clippy 发现的写法问题 |

## 已删除边界

- `coding-tools-mcp` 二进制入口。
- `CODING_TOOLS_MCP_CONFIG_DIR` 和 `coding-tools-mcp-desktop` 配置根。
- 旧 Linux daemon 运行目录、Windows 旧 mutex、macOS 旧 Bundle ID/名称识别。
- 旧目录复制、根目录 profiles/settings 导入和 Python 桌面配置导入。
- 内联 secrets、明文 `secrets.json`、Gateway URL 版本和旧 Tool Profile 自动迁移。
- `grep` / `grep_text`、`glob` 参数、`allowed_commands` 响应和 `session:<id>:full` 兼容入口。
- Harness Task 的旧 `expected_fingerprint` 回退字段和 verification `category` 别名。
- 前端 `--color-*` CSS Token 别名。
- 仓库中的完整 `old/` Python 归档。

## 当前唯一配置契约

- 配置根：平台 `anchor` 目录；显式覆盖仅为 `ANCHOR_CONFIG_DIR`。
- Profile：`data/profiles.json`。
- Secret：`data/secrets.json` 受保护封装；明文文件直接拒绝。
- Harness：Schema 5、`anchor/harness-v5`、稳定 Workspace UUID、内容寻址 baseline、校验 journal 和 close outbox。
- 工具：统一 registry；文本搜索只公开 `search_text`。

## 有意保留

- MCP 当前/较早协议版本协商，用于标准互操作。
- downstream MCP 结果 fallback，用于缺失 outputSchema 时的安全规范化。
- 对旧 SSE transport 的明确拒绝和诊断。
- README 中当前远端 Release 地址 `mybolide/coding-tools-mcp`；用户明确要求本轮不动远端仓库。

## 自动化验证

### Rust

- `cargo test --all-features --tests -- --test-threads=1`
  - library：321 passed、1 ignored。
  - call_tool_contract：23 passed。
  - call_tool_security：25 passed。
  - harness_state：4 passed。
  - harness_tool_contract：20 passed。
  - history_session：18 passed。
  - tool_output_schema_contract：3 passed。
- `cargo clippy --all-targets --all-features -- -D warnings`：通过。
- `cargo check --no-default-features --features cli --bin anchor`：通过。

### 前端与桌面

- `svelte-check`：0 errors、0 warnings。
- Vite/SvelteKit 生产构建：通过。
- `corepack pnpm desktop:build`：通过。

产物：

- `src-tauri/target/release/anchor-desktop.exe`
- `src-tauri/target/release/bundle/msi/Anchor_0.1.23_x64_en-US.msi`
- `src-tauri/target/release/bundle/nsis/Anchor_0.1.23_x64-setup.exe`

## 全局残留审计

对业务源码、测试、README 和当前文档扫描旧品牌、旧路径、旧工具别名与 CSS Token：

- 业务代码中的旧品牌运行时/配置标识：0。
- `grep_text`、`compat-readonly-all`、`session:<id>:full`、`--color-*` 当前契约引用：0。
- 唯一旧 slug 命中为 README 当前远端 Release 链接，按要求保留。

## 硬切影响

- 旧安装的配置不会自动迁移；需要在当前 Anchor 中重新注册工作区和凭据。
- 当前运行 listener 仍是安装前二进制；安装本次 EXE/MSI/NSIS 并重启后，源码中的 Schema 5、Catalog 10 和硬切行为才会实际生效。
- 未执行 `git push`，未修改 remote URL、远端分支或 GitHub 仓库。
