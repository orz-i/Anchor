import { useEffect, useState } from "react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Textarea } from "@/components/ui/textarea";

const EXAMPLE_CONFIG = `{
  "mcpServers": {
    "codegraph": {
      "type": "stdio",
      "command": "codegraph",
      "args": ["serve", "--mcp", "--path", "\${workspaceFolder}"],
      "maxTools": 16
    },
    "remote": {
      "type": "streamable-http",
      "url": "https://mcp.example.com/mcp",
      "headers": { "Authorization": "Bearer \${env:REMOTE_MCP_TOKEN}" },
      "maxConcurrentRequests": 16
    }
  }
}`;

function normalize(value: string): string { if (!value.trim()) return ""; const parsed: unknown = JSON.parse(value); if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("配置必须是 JSON 对象"); return JSON.stringify(parsed, null, 2); }

export function McpProxyConfigForm({ config, onSave }: { config: string; onSave: (config: string) => void | Promise<void> }) {
  const [draft, setDraft] = useState(config);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => { setDraft(config); setError(""); }, [config]);
  const dirty = draft !== config;
  const format = () => { try { setDraft(normalize(draft)); setError(""); } catch (cause) { setError(String(cause)); } };
  const save = async () => { if (!dirty || saving) return; try { const next = normalize(draft); setSaving(true); setError(""); await onSave(next); } catch (cause) { setError(String(cause)); } finally { setSaving(false); } };
  return <form className="grid gap-4" onSubmit={(event) => { event.preventDefault(); void save(); }}><Field><FieldLabel htmlFor="mcp-proxy-config">下游 MCP 配置（JSON）</FieldLabel><Textarea id="mcp-proxy-config" className="min-h-72 font-mono text-xs leading-5" value={draft} placeholder={EXAMPLE_CONFIG} spellCheck={false} onChange={(event) => setDraft(event.target.value)} /><FieldDescription>支持 stdio 与 Streamable HTTP；工具按 服务器名__工具名 聚合。敏感头建议使用 env 变量引用，避免把令牌写进配置。</FieldDescription></Field>{error && <Alert variant="destructive"><AlertTitle>JSON 配置无效</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}<div className="flex flex-wrap justify-between gap-2"><div className="flex gap-2"><Button type="button" variant="outline" onClick={() => { setDraft(EXAMPLE_CONFIG); setError(""); }}>填入示例</Button><Button type="button" variant="outline" onClick={format}>格式化 JSON</Button></div><Button type="submit" disabled={saving || !dirty}>{saving ? "保存中…" : "保存 MCP 聚合配置"}</Button></div></form>;
}
