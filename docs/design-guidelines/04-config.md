# 技术配置

Svelte + TailwindCSS 4 实现设计 Token。

## tailwind.config.js

```js
/** @type {import('tailwindcss').Config} */
export default {
  darkMode: 'class',
  content: ['./src/**/*.{html,js,svelte,ts}'],
  theme: {
    extend: {
      fontFamily: {
        sans: ['"Plus Jakarta Sans"', 'PingFang SC', 'Microsoft YaHei', 'sans-serif'],
        mono: ['"JetBrains Mono"', 'Cascadia Code', 'Consolas', 'monospace'],
      },
      colors: {
        bg: 'var(--page-bg)',
        surface: 'var(--card-bg)',
        border: 'var(--border)',
        accent: 'var(--primary)',
        muted: 'var(--text-muted)',
      },
      borderRadius: {
        sm: '6px',
        md: '10px',
        lg: '14px',
      },
      transitionTimingFunction: {
        out: 'cubic-bezier(0.16, 1, 0.3, 1)',
      },
    },
  },
  plugins: [],
};
```

## app.css — CSS Variables

```css
@import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500&family=Plus+Jakarta+Sans:wght@400;500;600;700&display=swap');

:root {
  --page-bg: oklch(0.98 0.004 260);
  --card-bg: oklch(1 0 0);
  --surface-hover: oklch(0.97 0.006 260);
  --border: oklch(0.90 0.008 260);
  --text-main: oklch(0.18 0.015 260);
  --text-secondary: oklch(0.42 0.015 260);
  --text-muted: oklch(0.58 0.012 260);
  --primary: oklch(0.52 0.20 275);
  --primary-hover: oklch(0.46 0.22 275);
  --success: oklch(0.55 0.17 155);
  --warning: oklch(0.62 0.14 75);
  --danger: oklch(0.55 0.20 25);
}

.dark {
  --page-bg: oklch(0.13 0.005 260);
  --card-bg: oklch(0.19 0.008 260);
  --surface-hover: oklch(0.22 0.010 260);
  --border: oklch(0.28 0.010 260);
  --text-main: oklch(0.97 0.005 260);
  --text-secondary: oklch(0.72 0.012 260);
  --text-muted: oklch(0.55 0.012 260);
  --primary: oklch(0.62 0.18 275);
  --primary-hover: oklch(0.68 0.20 275);
  --success: oklch(0.72 0.17 155);
  --warning: oklch(0.78 0.14 75);
  --danger: oklch(0.65 0.20 25);
}

body {
  font-family: 'Plus Jakarta Sans', 'PingFang SC', 'Microsoft YaHei', sans-serif;
  background: var(--page-bg);
  color: var(--text-main);
}
```

## 组件示例：StatusOrb.svelte

```svelte
<script lang="ts">
  export let state: 'running' | 'starting' | 'stopped' | 'error' = 'stopped';

  const colors = {
    running: 'bg-[var(--success)]',
    starting: 'bg-[var(--warning)] animate-spin-slow',
    stopped: 'bg-[var(--text-muted)]',
    error: 'bg-[var(--danger)]',
  };
</script>

<span
  class="inline-block h-2.5 w-2.5 rounded-full {colors[state]}"
  class:animate-pulse={state === 'running'}
  aria-label={state}
/>
```

## 组件示例：WorkspaceCard.svelte 结构

```svelte
<button
  class="group w-full rounded-lg border border-[var(--border)]
         bg-[var(--card-bg)] p-5 text-left
         transition-all duration-200 ease-out
         hover:border-[var(--primary)] hover:-translate-y-px
         focus-visible:outline-2 focus-visible:outline-offset-2
         focus-visible:outline-[var(--primary)]"
  on:click
>
  <div class="flex items-center gap-2">
    <StatusOrb {state} />
    <span class="font-semibold">{name}</span>
  </div>
  <p class="mt-2 truncate font-mono text-sm text-[var(--text-muted)]">{path}</p>
  <div class="mt-3 flex items-center justify-between">
    <Badge>{tunnelType}</Badge>
    <span class="truncate font-mono text-xs text-[var(--text-secondary)]">{endpoint}</span>
  </div>
</button>
```

## 图标

使用 [Lucide Svelte](https://lucide.dev/)：

```bash
pnpm add @lucide/svelte
```

常用图标：FolderOpen, Play, Square, Copy, Check, Settings, Activity, Terminal, Globe, Shield

## 主题切换

```svelte
<script lang="ts">
  import { onMount } from 'svelte';

  let dark = true;

  onMount(() => {
    dark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    apply();
  });

  function apply() {
    document.documentElement.classList.toggle('dark', dark);
  }

  function toggle() {
    dark = !dark;
    apply();
  }
</script>
```

---
*返回: [README.md](./README.md)*
