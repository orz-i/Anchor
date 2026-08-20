# 如何开发

> 本文档描述 Anchor Rust 的开发流程。

## 概述

本项目使用 MCP Probe Kit 工作流驱动开发。新功能必须先走规格流程，通过闸门后再写实现代码。

## 新功能开发流程

### 第一步：启动功能编排

调用 `start_feature` MCP 工具：

```json
{
  "feature_name": "my-feature",
  "description": "功能描述",
  "project_root": "e:/workspace/github/anchor-rust"
}
```

### 第二步：生成并校验规格

1. 调用 `add_feature` 生成规格模板
2. Agent 按模板填写 `docs/specs/<feature>/requirements.md`、`design.md`、`tasks.md`
3. 调用 `check_spec` 校验规格完整性
4. **未通过前不要写实现代码**

### 第三步：工作量估算

调用 `estimate` 获取故事点和时间区间。

### 第四步：按 tasks.md 实现

1. 每条任务先写证据块（读相关代码）
2. 实现后对照验收标准核验
3. 单文件不超过 500 行，超出需拆分

## 默认开发命令

```bash
# Web Admin + CLI 默认入口
pnpm start

# 默认发布产物：CLI + 嵌入式 Web Admin
pnpm release:build

# Rust 默认 feature 已是 cli
cd crates/anchor && cargo build

# 前端开发/构建
pnpm dev
pnpm build
```

Tauri desktop、安装包配置和 legacy desktop 构建脚本已经物理删除。不要再新增 desktop-only 入口；管理能力必须进入 shared management / CLI / Web Admin。

## 版本与发布规则

### 版本递增

- 默认递增 patch 版本，例如 `0.1.7` → `0.1.8`。
- 新功能或不兼容变更按语义化版本递增 minor 或 major。
- 仅文档、索引或不产生安装包的维护变更可不递增版本。

### 必须同步的版本源

每次递增时，以下位置必须保持为同一版本：

1. `package.json`
2. `crates/anchor/Cargo.toml`
3. `crates/anchor/Cargo.lock` 中 `anchor` 包的 `version`

不要修改依赖自身恰好相同的版本号；只更新本项目包的版本字段。

### 构建门禁

构建前必须：

1. 搜索上述版本源，确认没有旧的项目版本残留。
2. 运行 `pnpm check`、`cargo check`；修复类变更还需运行相关 Rust 测试。
3. 提交版本升级与功能/修复代码，再从该提交构建。

构建后校验 `anchor --version` / build identity 与当前提交一致，并确保 `pnpm release:build` 只生成 Web Admin + `anchor` CLI 产物。

## Rust 后端开发约定

### 管理能力边界

新业务能力先进入共享 Rust management/control 层，再由 CLI 和 Web Admin 调用；不得恢复 desktop-only command/AppState 层。

### 状态机模式

Runtime 生命周期使用显式 enum，不用字符串状态：

```rust
enum RuntimeState {
    Stopped,
    Starting { since: Instant },
    Running { pid: u32, port: u16 },
    Stopping,
    Error { message: String },
}
```

## 当前行为契约

开发 MCP 工具时，以 `crates/anchor/src/tools/registry.rs` 发布的 Schema、`crates/anchor/src/mcp/protocol.rs` 的协议约束和 `crates/anchor/tests/` 的合约/安全测试为准。修改目录或结果结构时，必须同步更新 snapshot 与 outputSchema 测试。

---
*返回索引: [../project-context.md](../project-context.md)*
