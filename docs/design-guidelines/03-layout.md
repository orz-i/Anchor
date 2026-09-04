# 布局规范

本文描述当前 `AppShell`、`PageLayout` 和 Workspace 页面已经实现的 Web 布局，不再沿用 Tauri titlebar、固定桌面窗口尺寸或旧 Desktop sidebar 规格。

## App Shell

当前 `src/components/admin/AppShell.tsx` 使用两列网格：

```text
┌──────────────────┬─────────────────────────────────────────────┐
│ Sidebar 16rem    │ Main                                        │
│                  │                                             │
│ Anchor / theme   │ PageLayout header                            │
│ Add workspace    │ ─────────────────────────────────────────── │
│                  │ scrollable page content                      │
│ Workspace nav    │                                             │
│ Settings nav     │                                             │
│                  │                                             │
│ Version          │                                             │
└──────────────────┴─────────────────────────────────────────────┘
```

实现要点：

- 根容器使用 `h-dvh`，页面自身管理内部滚动；
- desktop sidebar 宽度为 `16rem`，与主内容通过 border 分隔；
- `< md` 时当前 sidebar 隐藏、主区域切换为单列；因此新增关键管理入口时不能假定小屏始终可见 sidebar；
- Main 使用 `min-w-0` + `overflow-hidden`，具体页面由 `PageLayout` 提供纵向滚动区；
- Web Admin 不存在自定义原生 titlebar，也不规定 1280×800 之类桌面窗口尺寸。

## PageLayout

`src/components/admin/PageLayout.tsx` 统一页面头和正文：

```text
PageLayout
├── header
│   ├── kicker
│   ├── title
│   ├── description
│   └── actions
└── scrollable content
```

当前 header 使用 `px-7 py-5`，正文使用 `px-7 py-6`。新增一级管理页应优先复用 `PageLayout`，避免每个页面复制一套 header/scroll 行为。

## Workspace 列表

`src/pages/WorkspacesPage.tsx` 当前包含：

- 搜索与 runtime 状态筛选；
- page size / pagination；
- loading skeleton；
- empty / no-result state；
- Workspace card grid；
- MCP / Actions 快捷启停。

卡片网格断点为：

```text
base: 1 column
md:   2 columns
xl:   3 columns
gap:  1rem
```

每张卡片按当前实现展示名称、路径、MCP/Actions 状态、端口摘要、隧道摘要和快捷操作。路径等长文本必须允许 truncate，并通过 title/copy 等方式保留完整值的访问路径。

## Workspace 详情

详情页按业务能力分区，而不是把所有配置平铺到一个长表单：

- MCP / Actions 服务状态与 Endpoint；
- 配置 Tab；
- 日志；
- 健康检查；
- 隧道、认证、运行策略、Agent Skills 等配置卡片。

需要两列时使用响应式 grid（例如 `lg:grid-cols-2` / `xl:grid-cols-2`）；窄屏应自然回落为单列。

## Spacing / radius / elevation

不要在文档里维护一套与 Tailwind/shadcn 分离的像素表。当前约定是：

- spacing 使用 Tailwind scale；
- 全局基础 radius 来自 `src/app.css` 的 `--radius: 0.625rem`；
- Card、Button、Input 等圆角由 shadcn primitive 派生；
- 默认优先 border 和轻量 `shadow-sm`，避免大面积高 elevation；
- overlay/z-index 由 Dialog/Popover/Tooltip 等 primitive 管理，不在页面层手写全局 z-index 表。

## Responsive change checklist

调整 shell 或页面结构时至少复核：

1. `h-dvh` 下是否出现双重滚动；
2. 主区域是否保留 `min-w-0`，长 Endpoint 是否撑破布局；
3. md/xl breakpoint 的卡片/表单是否自然回落；
4. 小屏 sidebar 隐藏后是否仍存在完成关键任务的路径；
5. Dialog/Select/Tooltip 是否仍由 shared primitive 控制 overlay。

---
*返回: [README.md](./README.md)*
