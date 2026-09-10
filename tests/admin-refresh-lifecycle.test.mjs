import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const adminProviderPath = new URL("../src/components/admin/AdminProvider.tsx", import.meta.url);
const workspaceDetailPath = new URL("../src/pages/WorkspaceDetailPage.tsx", import.meta.url);
const tasksPagePath = new URL("../src/pages/TasksPage.tsx", import.meta.url);
const logViewerPath = new URL("../src/components/admin/LogViewer.tsx", import.meta.url);

test("AdminProvider keeps runtime setters stable and avoids no-op context churn", async () => {
  const source = await readFile(adminProviderPath, "utf8");

  assert.match(source, /const setMcpRuntimeState = useCallback/, "MCP runtime setter should have stable identity");
  assert.doesNotMatch(source, /ActionsRuntime|actionsRuntime|setActionsRuntimeState/, "retired Actions runtime state must not return to AdminProvider");
  assert.match(
    source,
    /current\[workspaceId\] === state \? current : \{ \.\.\.current, \[workspaceId\]: state \}/,
    "unchanged runtime state should not allocate a new context state object",
  );
  assert.equal(
    source.match(/listWorkspaces\(\)/g)?.length ?? 0,
    1,
    "workspace profiles should not be refetched after every empty control-plane long poll",
  );
  assert.match(
    source,
    /getControlPlaneEvents\(cursor, 25_000\)/,
    "control-plane observation should use the server's bounded long-poll window",
  );
});

test("high-frequency task and log refreshes are opt-in", async () => {
  const [tasksSource, logSource] = await Promise.all([
    readFile(tasksPagePath, "utf8"),
    readFile(logViewerPath, "utf8"),
  ]);

  assert.match(tasksSource, /useState\(false\)/, "Tasks auto refresh should be disabled by default");
  assert.match(logSource, /autoRefresh = false/, "log auto refresh should be disabled by default");
  assert.match(tasksSource, /自动刷新（2 秒）/, "Tasks users should still be able to opt into live refresh");
  assert.match(logSource, /自动刷新（3 秒）/, "log users should still be able to opt into live refresh");
});

test("Workspace detail refreshes runtime details from control-plane events instead of a fixed timer", async () => {
  const source = await readFile(workspaceDetailPath, "utf8");

  assert.doesNotMatch(source, /setInterval\(/, "workspace detail should not continuously poll runtime endpoints");
  assert.doesNotMatch(source, /\[admin\]/, "workspace detail callbacks must not depend on the whole context object");
  assert.match(source, /controlPlaneRevision/, "runtime detail refresh should follow control-plane events");
  assert.match(
    source,
    /\}, \[setMcpRuntimeState\]\);/,
    "runtime refresh callback should depend only on the stable MCP setter",
  );
});
