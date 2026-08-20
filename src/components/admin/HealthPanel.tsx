import { useState } from "react";
import { Activity } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { runHealthChecks, type HealthItem } from "@/lib/api/health";

export function HealthPanel({ workspaceId }: { workspaceId: string }) {
  const [items, setItems] = useState<HealthItem[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const run = async () => {
    if (busy || !workspaceId) return;
    setBusy(true); setError("");
    try { setItems(await runHealthChecks(workspaceId)); } catch (cause) { setError(String(cause)); setItems([]); } finally { setBusy(false); }
  };

  return <Card><CardHeader className="flex-row items-start justify-between gap-4"><div><CardTitle>健康检查</CardTitle><CardDescription className="mt-1">MCP、Actions 本地/公网 endpoint 与 OAuth 元数据</CardDescription></div><Button type="button" variant="outline" disabled={busy} onClick={() => void run()}><Activity data-icon="inline-start" />{busy ? "检查中…" : "运行健康检查"}</Button></CardHeader><CardContent className="grid gap-3">{error && <Alert variant="destructive"><AlertTitle>检查失败</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}{items.length > 0 ? items.map((item) => <div key={item.label} className="flex items-start justify-between gap-4 rounded-xl border p-3"><div><p className="text-sm font-medium">{item.label}</p><p className="mt-1 text-xs text-muted-foreground">{item.detail}</p>{!item.ok && item.hint && <p className="mt-1 text-xs text-primary">{item.hint}</p>}</div><Badge variant={item.ok ? "secondary" : "destructive"}>{item.ok ? "通过" : "失败"}</Badge></div>) : !busy && !error ? <p className="text-sm text-muted-foreground">尚未运行检查。</p> : null}</CardContent></Card>;
}
