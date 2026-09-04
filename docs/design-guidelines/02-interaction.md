# 交互规范

本文记录当前 React Web Admin 已实现的交互约定。组件事实源主要是 `src/components/ui/`、`src/components/admin/RuntimeBadge.tsx`、`src/components/admin/ServicePanel.tsx` 和 `src/components/admin/CopyField.tsx`。

## Runtime 状态

Anchor 当前公开六种 Workspace runtime 状态：

| 状态 | 文案 | 当前视觉 |
| --- | --- | --- |
| `stopped` | 已停止 | muted 灰色状态点 |
| `starting` | 启动中 | amber 状态点 + pulse |
| `running` | 运行中 | emerald 状态点 |
| `recovering` | 恢复中 | amber 状态点 + pulse |
| `stopping` | 停止中 | amber 状态点 + pulse |
| `error` | 错误 | destructive 状态点 |

统一使用 `RuntimeDot` / `RuntimeBadge` 表达这些状态，不为不同页面重新定义一套颜色或文案。状态必须同时有文本或上下文标签，不能只依赖颜色。

## Runtime 主操作

`ServicePanel` 当前遵循以下行为：

- `running`：主操作为 destructive「停止」；
- `recovering`：主操作为「立即重试」；
- 其他可启动状态：主操作为「启动」；
- `starting` / `stopping` 或调用进行中：按钮 disabled，避免重复提交；
- 操作失败通过页面状态和 Sonner error toast 给出反馈。

服务启停、配置保存等 mutation 不应通过前端猜测结果；UI 应等待管理 API/control-plane 返回，再刷新 canonical runtime state。

## Copy / technical value

路径、Endpoint、Client ID 等技术值优先使用 `font-mono`。`CopyField` 的当前行为是：

1. 有值且非 loading 时才允许复制；
2. 成功后 Copy 图标切换为 Check，1.5 秒后恢复；
3. Clipboard 失败通过 Sonner error toast 显示错误；
4. Button 始终提供 `aria-label`。

不要用“复制成功”toast 淹没高频操作；现有图标反馈已经足够时保持安静。

## Loading / empty / error

- 页面级 lazy route 使用明确的 loading 文案；
- Workspace 列表首次加载使用 Card skeleton/pulse；
- 无 Workspace 与筛选无结果使用 shadcn `Empty` 组件，并提供直接下一步；
- 请求错误应给出可执行错误信息，不把失败伪装成空数据；
- destructive 操作需要沿用现有 Dialog/确认流程，不以普通按钮点击直接删除关键配置。

## Theme 与 focus

主题由 `next-themes` 统一管理，默认跟随系统。组件应使用 `focus-visible` / shadcn primitive 已提供的 focus ring，不手工移除键盘焦点样式。

全局 `src/app.css` 已实现：

```css
@media (prefers-reduced-motion: reduce) {
  *,
  *::before,
  *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important;
  }
}
```

新增动画必须兼容这条全局策略；不要再维护另一套独立 motion token 文档。

---
*返回: [README.md](./README.md)*
