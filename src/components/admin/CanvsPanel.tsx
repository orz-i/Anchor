import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";

import { CopyField } from "@/components/admin/CopyField";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { ScrollArea } from "@/components/ui/scroll-area";
import { getCanvsSnapshot, type CanvsSnapshot, type CanvsTaskStatus } from "@/lib/api/canvs";

const REFRESH_MS = 2000;

function statusLabel(status: CanvsTaskStatus): string {
  return ({ active: "进行中", paused: "已暂停", verifying: "验证中", failed: "失败", completed: "已完成", completed_unverified: "完成未验证", rolled_back: "已回滚", unknown: "未知" })[status];
}
function formatTime(raw: string): string {
  if (!raw) return "—";
  const date = raw.startsWith("unix:") ? new Date(Number(raw.slice(5)) * 1000) : /^\d{13}$/.test(raw) ? new Date(Number(raw)) : /^\d{10}$/.test(raw) ? new Date(Number(raw) * 1000) : new Date(raw);
  return Number.isNaN(date.getTime()) ? raw : date.toLocaleString();
}
function shortHash(value: string | null): string { return value ? value.slice(0, 10) : "—"; }
function duration(value: number | null): string { return value === null ? "" : value < 1000 ? `${value} ms` : `${(value / 1000).toFixed(value < 10000 ? 1 : 0)} s`; }

export function CanvsPanel({ workspaceId, localUrl = "", publicUrl = "", onTaskStatusChange }: { workspaceId: string; localUrl?: string; publicUrl?: string; onTaskStatusChange?: (status: CanvsTaskStatus | null) => void }) {
  const [snapshot, setSnapshot] = useState<CanvsSnapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [autoRefresh, setAutoRefresh] = useState(false);
  const generation = useRef(0);
  const busyRef = useRef(false);

  const refresh = useCallback(async (force = false) => {
    if (!workspaceId || (!force && busyRef.current)) return;
    const current = ++generation.current;
    busyRef.current = true;
    setBusy(true); setError("");
    try {
      const next = await getCanvsSnapshot(workspaceId);
      if (current !== generation.current) return;
      setSnapshot(next); onTaskStatusChange?.(next.task?.status ?? null);
    } catch (cause) {
      if (current !== generation.current) return;
      setError(String(cause)); onTaskStatusChange?.(null);
    } finally { if (current === generation.current) { busyRef.current = false; setBusy(false); } }
  }, [onTaskStatusChange, workspaceId]);

  useEffect(() => { void refresh(true); }, [refresh]);
  useEffect(() => {
    if (!autoRefresh) return;
    const timer = window.setInterval(() => { if (!document.hidden) void refresh(); }, REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [autoRefresh, refresh]);

  const task = snapshot?.task ?? null;
  const completed = task?.completedSteps.length ?? 0;
  const pending = task?.pendingSteps.length ?? 0;

  return <div className="grid gap-4">
    <Card>
      <CardHeader className="flex-row items-start justify-between gap-4"><div><CardTitle>当前 Harness 任务</CardTitle><CardDescription className="mt-1">实时读取当前 Workspace 的步骤、操作、提交和验证状态。</CardDescription></div><div className="flex items-center gap-3"><label className="flex items-center gap-2 text-xs text-muted-foreground"><Checkbox checked={autoRefresh} onCheckedChange={(checked) => setAutoRefresh(Boolean(checked))} />自动刷新（2 秒）</label><Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => void refresh(true)}><RefreshCw data-icon="inline-start" className={busy ? "animate-spin" : undefined} />刷新</Button></div></CardHeader>
      <CardContent className="grid gap-4">
        <div className="grid gap-3 md:grid-cols-2"><CopyField label="本地网页" value={localUrl} /><CopyField label="公网网页" value={publicUrl} hint={publicUrl ? undefined : "隧道未连接"} /></div>
        {error && <Alert variant="destructive"><AlertTitle>无法读取 Canvs 状态</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}
        {task ? <div className="grid gap-4 rounded-xl border p-4 lg:grid-cols-[minmax(0,1fr)_18rem]"><div className="min-w-0"><div className="flex flex-wrap items-center gap-2"><Badge variant={task.status === "failed" ? "destructive" : task.status === "active" || task.status === "completed" ? "secondary" : "outline"}>{statusLabel(task.status)}</Badge>{task.active && <Badge variant="outline">并行活动</Badge>}{task.current && <Badge variant="outline">默认任务</Badge>}<code className="text-xs text-muted-foreground">{task.id}</code></div><p className="mt-3 text-sm font-medium leading-6">{task.objective}</p><p className="mt-2 text-xs text-muted-foreground">更新于 {formatTime(task.updatedAt)} · {task.workspaceMode === "worktree" ? "Git Worktree" : "共享工作区"} · 分支 {task.branch ?? "—"} · HEAD {shortHash(task.expectedHead)}</p></div><div><div className="flex justify-between text-xs text-muted-foreground"><span>步骤进度</span><span>{completed}/{completed + pending}</span></div><div className="mt-2 h-2 overflow-hidden rounded-full bg-muted"><div className="h-full rounded-full bg-primary" style={{ width: `${task.progressPercent}%` }} /></div><p className="mt-2 text-right text-xs font-medium">{task.progressPercent}%</p></div></div> : !busy && !error ? <p className="rounded-xl border border-dashed p-6 text-center text-sm text-muted-foreground">当前没有活动 Harness 任务。</p> : null}
      </CardContent>
    </Card>

    {task && snapshot && <>
      <div className="grid gap-4 xl:grid-cols-2">
        <Card><CardHeader><CardTitle>步骤</CardTitle><CardDescription>{completed} 完成 · {pending} 待办</CardDescription></CardHeader><CardContent className="grid gap-5 md:grid-cols-2"><div><p className="mb-2 text-xs font-medium text-muted-foreground">已完成</p><ol className="grid gap-2">{task.completedSteps.length ? task.completedSteps.map((step) => <li key={step} className="flex gap-2 text-sm"><span className="text-emerald-600">✓</span>{step}</li>) : <li className="text-sm text-muted-foreground">尚无已完成步骤</li>}</ol></div><div><p className="mb-2 text-xs font-medium text-muted-foreground">待处理</p><ol className="grid gap-2">{task.pendingSteps.length ? task.pendingSteps.map((step, index) => <li key={`${index}-${step}`} className="flex gap-2 text-sm"><span className="font-mono text-xs text-muted-foreground">{index + 1}.</span>{step}</li>) : <li className="text-sm text-muted-foreground">没有待处理步骤</li>}</ol></div></CardContent></Card>
        <Card><CardHeader><CardTitle>任务基线</CardTitle></CardHeader><CardContent className="grid gap-3 sm:grid-cols-2">{[["初始 HEAD", shortHash(task.head)],["当前预期 HEAD", shortHash(task.expectedHead)],["最新变更", task.latestChangeId ?? "—"],["最新验证", task.latestVerificationId ?? "—"]].map(([label,value]) => <div key={label} className="rounded-xl border bg-muted/20 p-3"><p className="text-xs text-muted-foreground">{label}</p><p className="mt-1 break-all font-mono text-xs">{value}</p></div>)}</CardContent></Card>
      </div>
      <div className="grid gap-4 xl:grid-cols-2">
        <Card><CardHeader><CardTitle>最近操作</CardTitle><CardDescription>最近 {snapshot.recentOperations.length} 条</CardDescription></CardHeader><CardContent><ScrollArea className="h-80"><div className="grid gap-2 pr-3">{snapshot.recentOperations.length ? snapshot.recentOperations.map((operation, index) => <div key={`${operation.id}-${index}`} className="rounded-xl border p-3"><div className="flex justify-between gap-3"><div className="min-w-0"><p className="truncate text-sm font-medium">{operation.tool}</p><p className="mt-1 text-xs text-muted-foreground">{operation.kind} · {formatTime(operation.createdAt)}</p></div><Badge variant={operation.ok === false ? "destructive" : "outline"}>{operation.status}</Badge></div>{operation.affectedFiles > 0 || operation.durationMs !== null ? <p className="mt-2 text-xs text-muted-foreground">{operation.affectedFiles ? `${operation.affectedFiles} 个文件` : ""}{operation.affectedFiles && operation.durationMs !== null ? " · " : ""}{duration(operation.durationMs)}</p> : null}</div>) : <p className="text-sm text-muted-foreground">当前任务还没有操作记录。</p>}</div></ScrollArea></CardContent></Card>
        <Card><CardHeader><CardTitle>有效验证</CardTitle><CardDescription>按命令折叠最新结果</CardDescription></CardHeader><CardContent><ScrollArea className="h-80"><div className="grid gap-2 pr-3">{snapshot.verifications.length ? snapshot.verifications.map((verification) => <div key={verification.id} className="rounded-xl border p-3"><div className="flex justify-between gap-3"><div className="min-w-0"><p className="truncate font-mono text-xs">{verification.command}</p><p className="mt-1 text-xs text-muted-foreground">{verification.kind} · {verification.level} · {formatTime(verification.createdAt)}</p></div><Badge variant={verification.passed ? "secondary" : "destructive"}>{verification.disposition || verification.status}</Badge></div><p className="mt-2 text-xs text-muted-foreground">{verification.exitCode !== null ? `退出码 ${verification.exitCode}` : ""}{verification.exitCode !== null && verification.durationMs !== null ? " · " : ""}{duration(verification.durationMs)}</p></div>) : <p className="text-sm text-muted-foreground">当前任务还没有验证记录。</p>}</div></ScrollArea></CardContent></Card>
      </div>
      <div className="grid gap-4 xl:grid-cols-2">
        <Card><CardHeader><CardTitle>分段提交</CardTitle><CardDescription>{snapshot.changes.length} 条</CardDescription></CardHeader><CardContent className="grid gap-2">{snapshot.changes.length ? snapshot.changes.map((change) => <div key={change.id} className="rounded-xl border p-3"><div className="flex justify-between gap-3"><code className="text-xs">{shortHash(change.commitSha ?? change.id)}</code><span className="text-xs text-muted-foreground">{formatTime(change.createdAt)}</span></div><p className="mt-2 text-xs text-muted-foreground">{change.committedFiles.length} 个文件 · {change.verificationCount} 条验证</p></div>) : <p className="text-sm text-muted-foreground">当前任务还没有分段提交。</p>}</CardContent></Card>
        <Card><CardHeader><CardTitle>任务事件</CardTitle><CardDescription>最近 {snapshot.recentEvents.length} 条</CardDescription></CardHeader><CardContent className="grid gap-2">{snapshot.recentEvents.length ? snapshot.recentEvents.map((event) => <div key={event.id} className="flex justify-between gap-3 rounded-xl border p-3"><div className="min-w-0"><p className="truncate text-sm font-medium">{event.kind}</p><p className="mt-1 text-xs text-muted-foreground">{event.toolName ?? "Harness"} · {formatTime(event.createdAt)}</p></div>{event.affectedFiles > 0 && <Badge variant={event.ok === false ? "destructive" : "outline"}>{event.affectedFiles} 文件</Badge>}</div>) : <p className="text-sm text-muted-foreground">当前任务还没有事件记录。</p>}</CardContent></Card>
      </div>
    </>}
  </div>;
}
