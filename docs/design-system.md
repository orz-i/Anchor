# 设计系统：Anchor Web Admin

本文描述**当前仓库已经实现**的 Web Admin 设计基础，不再把已退役的 Desktop/Svelte 方案当作现行规范。视觉与组件事实源依次是：

```text
src/app.css
components.json
src/components/ui/**
src/components/admin/**
src/pages/**
```

如果本文与代码不一致，应先以这些实现为准，再同步文档。

## 当前技术栈

| 层级 | 当前实现 |
| --- | --- |
| 应用 | Vite + React 19 + React Router |
| 组件 | shadcn/ui，`base-nova` style |
| 样式 | Tailwind CSS 4 + CSS variables |
| 主题 | `next-themes`，默认跟随系统，可切换 light/dark |
| 字体 | `Geist Variable`（`@fontsource-variable/geist`） |
| 图标 | `lucide-react` |
| Toast | Sonner |

## 设计方向

Anchor 当前采用克制的 developer-tool Web Admin 风格：以中性背景、清晰层级、边框和少量阴影组织信息，优先表达 Workspace、runtime 状态和可执行操作，而不是用装饰性视觉制造层级。

核心原则：

- Workspace 是管理面的第一主对象；
- 配置按任务分区，避免在首屏一次展开全部高级字段；
- runtime 状态同时使用文字和状态点表达，不能只依赖颜色；
- 主题 token 统一来自 `src/app.css`，组件不得另建第二套硬编码 palette；
- 图标来自 Lucide，不使用 emoji 代替产品图标；
- 全局尊重 `prefers-reduced-motion`。

## Theme 与 Token

Tailwind 4 直接从 `src/app.css` 的 `@theme inline` 与 CSS variables 获取 token；项目没有以 `tailwind.config.js` 维护第二套主题。

主要语义 token 包括：

```text
background / foreground
card / card-foreground
popover / popover-foreground
primary / primary-foreground
secondary / secondary-foreground
muted / muted-foreground
accent / accent-foreground
destructive
border / input / ring
sidebar-*
chart-1 ... chart-5
```

基础圆角为 `--radius: 0.625rem`；派生 `sm/md/lg/xl/...` 由 `@theme inline` 统一计算。具体 OKLCH 值不要复制到业务组件，直接使用语义 class/token。

## Typography

UI 正文和标题使用 `Geist Variable`。路径、URL、端口和日志等技术信息使用 Tailwind `font-mono` 语义；当前 package 并未额外引入 JetBrains Mono，因此文档也不应把它描述成产品依赖。

## Component policy

- 基础 primitives 位于 `src/components/ui/`，由 shadcn/ui 约定维护；
- 产品级组合组件位于 `src/components/admin/`；
- 页面位于 `src/pages/`，优先组合现有 primitives，不复制 Button/Card/Dialog 等基础实现；
- 主题切换统一走 `next-themes`；
- runtime 状态统一复用 `RuntimeBadge` / `RuntimeDot`。

## 文档索引

| 文档 | 内容 |
| --- | --- |
| [设计原则](./design-guidelines/01-principles.md) | 产品与信息架构原则 |
| [交互规范](./design-guidelines/02-interaction.md) | 当前 runtime 状态、反馈与 reduced-motion 约定 |
| [布局规范](./design-guidelines/03-layout.md) | 当前 Web Admin shell 与 Workspace 页面结构 |
| [技术配置](./design-guidelines/04-config.md) | React/shadcn/Tailwind 4 的实现入口 |
| [设计 Token 摘要](./design-system.json) | 面向工具读取的当前设计元数据与事实源 |

`docs/specs/` 下旧 Desktop/Tauri UI 设计仅保留为历史规格，不属于当前设计系统。

## 维护检查

- 当前页面是否只使用现有语义 token / shadcn primitives；
- light/dark/system 三种主题路径是否都可读；
- loading/error/success/disabled 是否有明确反馈；
- 键盘 focus 与 reduced-motion 是否仍有效；
- 文档示例是否仍是 React/TSX，而不是已退役的 Svelte/Tauri API；
- 结构性 UI 变化是否同步本页、`design-system.json` 与对应 guideline。
