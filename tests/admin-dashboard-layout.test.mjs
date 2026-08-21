import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const appPath = new URL("../src/App.tsx", import.meta.url);
const appShellPath = new URL("../src/components/admin/AppShell.tsx", import.meta.url);
const workspacesPagePath = new URL("../src/pages/WorkspacesPage.tsx", import.meta.url);
const workspaceDetailPath = new URL("../src/pages/WorkspaceDetailPage.tsx", import.meta.url);

test("App 配置了工作区与系统设置的多路由结构", async () => {
  const source = await readFile(appPath, "utf8");

  assert.match(source, /path="workspaces"/, "应配置 /workspaces 路由");
  assert.match(source, /path="workspaces\/:id"/, "应配置 /workspaces/:id 路由");
  assert.match(source, /path="settings\/general"/, "应配置 /settings/general 路由");
  assert.match(source, /path="settings\/keys"/, "应配置 /settings/keys 路由");
  assert.match(source, /path="settings\/frp"/, "应配置 /settings/frp 路由");
  assert.match(source, /path="settings\/software"/, "应配置 /settings/software 路由");
});

test("AppShell 侧边栏包含工作区管理与系统设置栏目", async () => {
  const source = await readFile(appShellPath, "utf8");

  assert.match(source, /工作区管理/, "应包含工作区管理分组");
  assert.match(source, /系统设置/, "应包含系统设置分组");
  assert.match(source, /to="\/workspaces"/, "应有指向 /workspaces 的导航链接");
  assert.match(source, /\/settings\/general/, "应包含通用设置导航");
  assert.match(source, /\/settings\/keys/, "应包含共享密钥导航");
  assert.match(source, /\/settings\/frp/, "应包含 FRP 配置导航");
  assert.match(source, /\/settings\/software/, "应包含软件管理导航");
});

test("工作区卡片列表页包含卡片网格、分页组件、搜索过滤与快捷启停", async () => {
  const source = await readFile(workspacesPagePath, "utf8");

  assert.match(source, /Pagination/, "工作区列表页应引入分页组件");
  assert.match(source, /searchQuery/, "工作区列表页应支持搜索过滤");
  assert.match(source, /statusFilter/, "工作区列表页应支持状态筛选");
  assert.match(source, /toggleService/, "工作区卡片应支持快捷启停服务");
  assert.match(source, /handleDeleteWorkspace/, "工作区卡片应支持删除操作");
});

test("工作区详情页提供返回工作区列表导航", async () => {
  const source = await readFile(workspaceDetailPath, "utf8");

  assert.match(source, /to="\/workspaces"/, "详情页应提供返回 /workspaces 的链接");
  assert.match(source, /工作区列表/, "详情页应展示工作区列表面包屑或返回按钮");
});
