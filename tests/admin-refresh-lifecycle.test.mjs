import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const adminProviderPath = new URL("../src/components/admin/AdminProvider.tsx", import.meta.url);
const workspaceDetailPath = new URL("../src/pages/WorkspaceDetailPage.tsx", import.meta.url);
const workspaceCompatPath = new URL("../src/pages/WorkspacePage.tsx", import.meta.url);
const canvsPanelPath = new URL("../src/components/admin/CanvsPanel.tsx", import.meta.url);
const logViewerPath = new URL("../src/components/admin/LogViewer.tsx", import.meta.url);

test("AdminProvider keeps runtime setters stable and avoids no-op context churn", async () => {
  const source = await readFile(adminProviderPath, "utf8");

  assert.match(source, /const setMcpRuntimeState = useCallback/, "MCP runtime setter should have stable identity");
  assert.match(source, /const setActionsRuntimeState = useCallback/, "Actions runtime setter should have stable identity");
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

test("high-frequency Canvs and log refreshes are opt-in", async () => {
  const [canvsSource, logSource] = await Promise.all([
    readFile(canvsPanelPath, "utf8"),
    readFile(logViewerPath, "utf8"),
  ]);

  assert.match(canvsSource, /useState\(false\)/, "Canvs auto refresh should be disabled by default");
  assert.match(logSource, /autoRefresh = false/, "log auto refresh should be disabled by default");
  assert.match(canvsSource, /自动刷新（2 秒）/, "Canvs users should still be able to opt into live refresh");
  assert.match(logSource, /自动刷新（3 秒）/, "log users should still be able to opt into live refresh");
});

test("Workspace detail refreshes runtime details from control-plane events instead of a fixed timer", async () => {
  const sources = await Promise.all([
    readFile(workspaceDetailPath, "utf8"),
    readFile(workspaceCompatPath, "utf8"),
  ]);

  for (const source of sources) {
    assert.doesNotMatch(source, /setInterval\(/, "workspace detail should not continuously poll runtime endpoints");
    assert.doesNotMatch(source, /\[admin\]/, "workspace detail callbacks must not depend on the whole context object");
    assert.match(source, /controlPlaneRevision/, "runtime detail refresh should follow control-plane events");
    assert.match(
      source,
      /\[setActionsRuntimeState, setMcpRuntimeState\]/,
      "runtime refresh callback should depend only on stable setters",
    );
  }
});
