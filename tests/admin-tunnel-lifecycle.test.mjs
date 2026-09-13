import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const tunnelFormPath = new URL("../src/components/admin/TunnelConfigForm.tsx", import.meta.url);
const tunnelsPagePath = new URL("../src/pages/TunnelsPage.tsx", import.meta.url);
const workspaceDetailPath = new URL("../src/pages/WorkspaceDetailPage.tsx", import.meta.url);

test("Tunnel 表单按持久化字段值同步 draft，而不是依赖 config 对象身份", async () => {
  const source = await readFile(tunnelFormPath, "utf8");

  assert.match(
    source,
    /const initial = useMemo\([\s\S]*?\[\s*config\.type,[\s\S]*?config\.use_proxy,[\s\S]*?\],\s*\);/,
    "TunnelConfigForm 应按持久化字段值 memoize initial config",
  );
  assert.doesNotMatch(source, /\[config\]\)/, "父组件创建新 config 对象时不能重置未保存 draft");
  assert.match(
    source,
    /useEffect\(\(\) => setDraft\(initial\), \[initial\]\)/,
    "真实持久化配置变化仍应同步到 draft",
  );
  assert.match(source, /getTunnelSecret\(tunnelId, secretKey\)/, "Tunnel Secret 应按顶级 tunnelId 读取");
  assert.match(source, /setTunnelSecret\(tunnelId, secretKey, token\)/, "Tunnel Secret 应按顶级 tunnelId 写入");
});

test("顶级 Tunnel 保存直接更新 Tunnel authority，Workspace Detail 不再承载 Tunnel 配置", async () => {
  const tunnels = await readFile(tunnelsPagePath, "utf8");
  const workspaceDetail = await readFile(workspaceDetailPath, "utf8");

  assert.match(
    tunnels,
    /const saveConfig = async \(config: TunnelFormConfig\) => \{[\s\S]*?const saved = await updateTunnel\(\{[\s\S]*?config: \{[\s\S]*?\}\s*,?\n\s*\}\);/,
    "Tunnel 配置应直接持久化到顶级 TunnelProfile",
  );
  assert.match(
    tunnels,
    /<TunnelConfigForm tunnelId=\{selected\.id\} config=\{tunnelFormConfig\(selected\.config\)\} onSave=\{saveConfig\} \/>/,
    "顶级 Tunnel 页面应以 tunnelId 驱动配置与 Secret",
  );
  assert.doesNotMatch(workspaceDetail, /TunnelConfigForm|@\/lib\/api\/tunnel|saveMcpTunnel/,
    "Workspace Detail 不得恢复 Tunnel 配置或生命周期入口");
});

test("显式 Tunnel runtime 操作只消费已持久化的顶级 Tunnel", async () => {
  const source = await readFile(tunnelsPagePath, "utf8");
  const runRuntime = source.match(
    /const runRuntime = async \(operation: "start" \| "stop" \| "test"\) => \{[\s\S]*?\n  \};/,
  )?.[0];

  assert.ok(runRuntime, "TunnelsPage 应保留显式 runtime lifecycle");
  assert.match(runRuntime, /await startTunnel\(selected\.id\)/, "启动应以顶级 tunnelId 为 authority");
  assert.match(runRuntime, /await stopTunnel\(selected\.id\)/, "停止应以顶级 tunnelId 为 authority");
  assert.match(runRuntime, /await testTunnel\(selected\.id\)/, "测试应以顶级 tunnelId 为 authority");
  assert.doesNotMatch(runRuntime, /saveConfig|updateTunnel|saveDraft/,
    "runtime lifecycle 不应隐式保存配置或维护 Workspace Tunnel 双线");
});
