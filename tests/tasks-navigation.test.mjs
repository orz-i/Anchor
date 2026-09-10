import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const appPath = new URL("../src/App.tsx", import.meta.url);
const appShellPath = new URL("../src/components/admin/AppShell.tsx", import.meta.url);
const workspaceDetailPath = new URL("../src/pages/WorkspaceDetailPage.tsx", import.meta.url);
const tasksPagePath = new URL("../src/pages/TasksPage.tsx", import.meta.url);
const tasksApiPath = new URL("../src/lib/api/tasks.ts", import.meta.url);
const retiredCanvsPanelPath = new URL("../src/components/admin/CanvsPanel.tsx", import.meta.url);
const retiredCanvsApiPath = new URL("../src/lib/api/canvs.ts", import.meta.url);

test("Tasks is a top-level Admin destination with workspace filtering", async () => {
  const [app, shell, tasksPage, tasksApi] = await Promise.all([
    readFile(appPath, "utf8"),
    readFile(appShellPath, "utf8"),
    readFile(tasksPagePath, "utf8"),
    readFile(tasksApiPath, "utf8"),
  ]);

  assert.match(app, /path="tasks" element=\{<TasksPage \/>\}/, "Admin should expose a top-level /tasks route");
  assert.match(shell, /to="\/tasks"/, "sidebar should link directly to /tasks");
  assert.match(tasksPage, /全部 Workspace/, "Tasks should provide an explicit all-workspace filter");
  assert.match(tasksPage, /workspaceFilter/, "workspace filtering should be first-class page state");
  assert.match(tasksPage, /listTasks\(\)/, "task list should use one Admin aggregate request");
  assert.match(tasksPage, /getTaskSnapshot/, "selected task detail should load through the Admin task API");
  assert.match(tasksApi, /"list_tasks"/, "frontend API should use the task-named aggregate command");
  assert.match(tasksApi, /"get_task_snapshot"/, "frontend API should use the task-named detail command");
});

test("Workspace Detail no longer carries Canvs and retired frontend files stay removed", async () => {
  const workspaceDetail = await readFile(workspaceDetailPath, "utf8");

  assert.doesNotMatch(workspaceDetail, /Canvs|canvs|canvas/i, "Workspace Detail must not retain Canvs UI or URL construction");
  await assert.rejects(readFile(retiredCanvsPanelPath, "utf8"));
  await assert.rejects(readFile(retiredCanvsApiPath, "utf8"));
});
