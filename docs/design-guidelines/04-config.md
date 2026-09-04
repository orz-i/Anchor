# 技术配置

当前 Web Admin 使用 **Vite + React + React Router + shadcn/ui + Tailwind CSS 4**。本文只记录仓库中已经存在的配置入口；不再使用 Svelte component、`@lucide/svelte` 或旧 `tailwind.config.js` 示例。

## Package 入口

根 `package.json` 当前关键依赖包括：

```text
react / react-dom
react-router-dom
shadcn
tailwindcss / @tailwindcss/vite
lucide-react
next-themes
sonner
@fontsource-variable/geist
```

前端开发与校验使用仓库现有 pnpm scripts，不额外创建第二套 frontend package。

## shadcn 配置

`components.json` 是当前组件生成/约定入口，关键配置为：

```json
{
  "style": "base-nova",
  "rsc": false,
  "tsx": true,
  "tailwind": {
    "config": "",
    "css": "src/app.css",
    "baseColor": "neutral",
    "cssVariables": true,
    "prefix": ""
  },
  "iconLibrary": "lucide"
}
```

`components.json` 中 `tailwind.config` 字段为空并不是遗漏：当前使用 Tailwind CSS 4 的 CSS-first 配置，theme/token 直接位于 `src/app.css`，仓库不需要额外维护一份 `tailwind.config.js` 主题定义。

## app.css

当前入口：

```css
@import "tailwindcss";
@import "tw-animate-css";
@import "shadcn/tailwind.css";
@import "@fontsource-variable/geist";

@custom-variant dark (&:is(.dark *));

@theme inline {
  --font-sans: "Geist Variable", sans-serif;
  /* semantic color/radius mappings */
}
```

`:root` 提供 light token，`.dark` 提供 dark token。业务组件使用 `bg-background`、`text-foreground`、`bg-card`、`text-muted-foreground`、`border-border` 等语义 class，不直接复制 OKLCH 数值。

## Theme provider

`src/main.tsx` 统一挂载主题：

```tsx
<ThemeProvider attribute="class" defaultTheme="system" enableSystem>
  <TooltipProvider>
    <BrowserRouter>
      <App />
    </BrowserRouter>
    <Toaster richColors position="top-right" />
  </TooltipProvider>
</ThemeProvider>
```

页面/组件不要自行读取 `prefers-color-scheme` 并维护另一份 dark state；需要切换主题时使用 `next-themes` 的 `useTheme()`。

## Components

基础组件位于：

```text
src/components/ui/
```

产品组合组件位于：

```text
src/components/admin/
```

新增 UI 优先复用已有 Button、Card、Dialog、Select、Tabs、Tooltip、Field、Empty 等 primitive。只有共享 primitive 确实缺失时再按当前 shadcn style 增加，不在业务页面复制一份近似组件。

Runtime 状态复用：

```tsx
import { RuntimeBadge, RuntimeDot } from "@/components/admin/RuntimeBadge";
```

图标直接使用：

```tsx
import { Copy, Settings2 } from "lucide-react";
```

## Routing

当前路由由 `src/App.tsx` 的 React Router `Routes` 管理，一级页面包括 Workspace 列表/详情和 General/Keys/FRP/Software settings。新增页面时应接入现有 `AppShell`，不要新建第二个独立管理壳。

## Source-of-truth rule

设计/技术文档出现以下变化时必须同步：

- `components.json` style/base color/icon library 改变；
- `src/app.css` token/font/radius 改变；
- `src/main.tsx` theme provider 改变；
- `AppShell` 布局 breakpoint 改变；
- `RuntimeState` 或 `RuntimeBadge` 状态语义改变；
- React/shadcn/Tailwind 版本或前端框架发生架构性迁移。

旧 Tauri/Svelte 规格只保留在历史 specs/verification 中，不应重新复制到当前 guideline。

---
*返回: [README.md](./README.md)*
