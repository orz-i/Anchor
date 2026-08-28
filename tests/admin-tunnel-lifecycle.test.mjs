import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const tunnelFormPath = new URL("../src/components/admin/TunnelConfigForm.tsx", import.meta.url);
const workspaceDetailPath = new URL("../src/pages/WorkspaceDetailPage.tsx", import.meta.url);
const workspaceCompatPath = new URL("../src/pages/WorkspacePage.tsx", import.meta.url);

test("Tunnel 表单仅在持久化配置或工作区上下文真实变化时重置 draft", async () => {
  const source = await readFile(tunnelFormPath, "utf8");
  const dependencyLine = source
    .split("\n")
    .find((line) => line.includes("}), [workspaceId, service, config.type"));

  assert.ok(dependencyLine, "TunnelConfigForm 应按字段值而不是 config 对象身份 memoize initial config");
  assert.match(dependencyLine, /config\.use_proxy/, "Tunnel memo 依赖应覆盖最后一个持久化字段");
  assert.doesNotMatch(source, /\[config\]\)/, "父组件创建新 config 对象时不能重置未保存 draft");
  assert.match(source, /useEffect\(\(\) => setDraft\(initial\), \[initial\]\)/, "真实持久化配置变化仍应同步到 draft");
});

test("普通 Tunnel 保存只持久化配置，不显式 start/restart/stop tunnel", async () => {
  for (const path of [workspaceDetailPath, workspaceCompatPath]) {
    const source = await readFile(path, "utf8");

    assert.match(source, /const saveMcpTunnel = async \(config: TunnelFormConfig\)[\s\S]*?await persist\(next\);\n  };/, "MCP Tunnel 保存应通过统一 config stage/apply 路径");
    assert.match(source, /const saveActionsTunnel = async \(config: TunnelFormConfig\)[\s\S]*?await persist\(next\);\n  };/, "Actions Tunnel 保存应通过统一 config stage/apply 路径");
    assert.doesNotMatch(source, /restartTunnelIfConfigured/, "普通保存不应维护第二套 tunnel restart 生命周期");
    assert.doesNotMatch(source, /@\/lib\/api\/tunnel/, "工作区详情保存路径不应直接调用 tunnel lifecycle API");
  }
});

test("显式启动和测试仍会先保存最新 Tunnel draft", async () => {
  const source = await readFile(tunnelFormPath, "utf8");

  assert.match(source, /if \(dirty\) await saveDraft\(\); const result = await startTunnel\(workspaceId, service\)/, "启动 Tunnel 前应保存最新 draft");
  assert.match(source, /if \(dirty\) await saveDraft\(\); const result = await testTunnel\(workspaceId, service\)/, "测试 Tunnel 前应保存最新 draft");
});
