# 如何编写测试

> 本文档描述 Anchor Rust 的测试策略。

## 概述

测试分为 Rust 单元测试、工具/MCP 合约测试、安全测试、Harness/History 持久化测试和桌面生产构建验证。

## 测试框架

| 层级 | 框架 | 位置 |
|------|------|------|
| Rust 单元测试 | cargo test | `src-tauri/src/**` 内 `#[cfg(test)]` |
| Rust 集成测试 | cargo test | `src-tauri/tests/` |
| 工具与 MCP 合约测试 | cargo test | `src-tauri/tests/call_tool_contract.rs` 等 |
| 前端静态检查 | svelte-check | `src/**/*.svelte` / `src/**/*.ts` |

## MCP 合规测试（核心）

当前 `src-tauri/tests/` 覆盖：

- MCP 协议契约（initialize, tools/list, tools/call）
- 工具行为 golden test（read_file, apply_patch, exec_command 等）
- 安全边界（路径穿越、敏感环境变量、破坏性命令）
- Schema drift（工具定义与文档一致）
- 端到端场景（bugfix、stdin 交互）

### 合规测试运行

```bash
# 运行全部 Rust 测试目标
cargo test --all-features --tests -- --test-threads=1

# 运行工具合约和安全套件
cargo test --test call_tool_contract
cargo test --test call_tool_security
```

## Rust 单元测试示例

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_profile_endpoint() {
        let profile = WorkspaceProfile::new("/tmp/repo", "test");
        assert_eq!(profile.local_endpoint(), "http://127.0.0.1:28766/mcp");
    }
}
```

## 测试编写原则

1. **行为契约优先** — 以当前工具 Schema 和 Rust 合约测试为准，不猜测
2. **目录与结果同步** — 修改工具定义时同步更新 snapshot 和 outputSchema 验证
3. **安全测试不可跳过** — 路径穿越、命令注入等必须覆盖
4. **Windows 兼容** — 进程管理和路径处理需在 Windows 上验证

## 发布前验证

```bash
cargo clippy --all-targets --all-features -- -D warnings
pnpm check
pnpm build
pnpm desktop:build
```

---
*返回索引: [../project-context.md](../project-context.md)*
