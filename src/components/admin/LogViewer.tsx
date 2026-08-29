import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { ScrollArea } from "@/components/ui/scroll-area";
import { readWorkspaceLogs, type LogChunk, type LogService } from "@/lib/api/logs";

const AUTO_REFRESH_MS = 3000;

export function LogViewer({ workspaceId, service, autoRefresh = false, title }: { workspaceId: string; service: LogService; autoRefresh?: boolean; title?: string }) {
  const [chunks, setChunks] = useState<LogChunk[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [autoRefreshEnabled, setAutoRefreshEnabled] = useState(autoRefresh);
  const generation = useRef(0);
  const busyRef = useRef(false);

  const refresh = useCallback(async (force = false) => {
    if (!workspaceId || (!force && busyRef.current)) return;
    const current = ++generation.current;
    busyRef.current = true;
    setBusy(true);
    setError("");
    try {
      const next = await readWorkspaceLogs(workspaceId, service);
      if (current === generation.current) setChunks(next);
    } catch (cause) {
      if (current === generation.current) { setError(String(cause)); setChunks([]); }
    } finally {
      if (current === generation.current) {
        busyRef.current = false;
        setBusy(false);
      }
    }
  }, [service, workspaceId]);

  useEffect(() => { void refresh(true); }, [refresh]);
  useEffect(() => {
    if (!autoRefreshEnabled) return;
    const timer = window.setInterval(() => { if (!document.hidden) void refresh(); }, AUTO_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [autoRefreshEnabled, refresh]);

  return (
    <Card>
      <CardHeader className="flex-row items-start justify-between gap-4">
        <div><CardTitle>{title ?? (service === "mcp" ? "MCP 日志" : "Actions 日志")}</CardTitle><CardDescription className="mt-1">Daemon 有界日志快照（最多 8KB）</CardDescription></div>
        <div className="flex items-center gap-3"><label className="flex items-center gap-2 text-xs text-muted-foreground"><Checkbox checked={autoRefreshEnabled} onCheckedChange={(checked) => setAutoRefreshEnabled(Boolean(checked))} />自动刷新（3 秒）</label><Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => void refresh(true)}><RefreshCw data-icon="inline-start" className={busy ? "animate-spin" : undefined} />刷新</Button></div>
      </CardHeader>
      <CardContent>
        {error && <Alert variant="destructive"><AlertTitle>日志读取失败</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}
        {chunks.length > 0 ? <div className="grid gap-3">{chunks.map((chunk) => <div key={chunk.name} className="overflow-hidden rounded-xl border"><p className="border-b bg-muted/30 px-3 py-2 font-mono text-xs text-muted-foreground">{chunk.name}</p><ScrollArea className="h-48"><pre className="whitespace-pre-wrap break-words p-3 font-mono text-xs leading-5">{chunk.content || "（空）"}</pre></ScrollArea></div>)}</div> : !busy && !error ? <p className="text-sm text-muted-foreground">当前还没有日志。</p> : null}
      </CardContent>
    </Card>
  );
}
