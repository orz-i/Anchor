import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ListTodo, RefreshCw, Search } from "lucide-react";

import { useAdmin } from "@/components/admin/AdminProvider";
import {
  formatTaskTime,
  TaskDetailPanel,
  taskStatusLabel,
  taskTimeValue,
} from "@/components/admin/TaskDetailPanel";
import { PageLayout } from "@/components/admin/PageLayout";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import {
  getTaskSnapshot,
  listTasks,
  type AdminTaskEntry,
  type TaskListResult,
  type TaskSnapshot,
  type TaskStatus,
} from "@/lib/api/tasks";
import { cn } from "@/lib/utils";

const REFRESH_MS = 2000;
type StatusFilter = "all" | TaskStatus | "worktree";

function taskKey(entry: AdminTaskEntry): string {
  return `${entry.workspaceId}:${entry.task.id}`;
}

function statusVariant(status: TaskStatus): "default" | "secondary" | "destructive" | "outline" {
  if (status === "failed" || status === "incomplete") return "destructive";
  if (status === "active" || status === "completed") return "secondary";
  return "outline";
}

export function TasksPage() {
  const { workspaces } = useAdmin();
  const [result, setResult] = useState<TaskListResult | null>(null);
  const [snapshot, setSnapshot] = useState<TaskSnapshot | null>(null);
  const [selectedKey, setSelectedKey] = useState("");
  const [workspaceFilter, setWorkspaceFilter] = useState("all");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [detailBusy, setDetailBusy] = useState(false);
  const [error, setError] = useState("");
  const [detailError, setDetailError] = useState("");
  const [autoRefresh, setAutoRefresh] = useState(false);
  const listGeneration = useRef(0);
  const detailGeneration = useRef(0);
  const listBusy = useRef(false);
  const detailBusyRef = useRef(false);

  const refreshList = useCallback(async (force = false) => {
    if (!force && listBusy.current) return;
    const generation = ++listGeneration.current;
    listBusy.current = true;
    setBusy(true);
    setError("");
    try {
      const next = await listTasks();
      if (generation === listGeneration.current) setResult(next);
    } catch (cause) {
      if (generation === listGeneration.current) setError(String(cause));
    } finally {
      if (generation === listGeneration.current) {
        listBusy.current = false;
        setBusy(false);
      }
    }
  }, []);

  const entries = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return (result?.tasks ?? [])
      .filter((entry) => workspaceFilter === "all" || entry.workspaceId === workspaceFilter)
      .filter((entry) => {
        if (statusFilter === "all") return true;
        if (statusFilter === "worktree") return entry.task.workspaceMode === "worktree";
        return entry.task.status === statusFilter;
      })
      .filter((entry) => {
        if (!normalizedQuery) return true;
        return [
          entry.workspaceName,
          entry.task.objective,
          entry.task.id,
          entry.task.branch ?? "",
          entry.task.expectedHead ?? "",
        ].some((value) => value.toLowerCase().includes(normalizedQuery));
      })
      .sort((left, right) => {
        const rightTime = taskTimeValue(right.task.lastActivityAt ?? right.task.updatedAt);
        const leftTime = taskTimeValue(left.task.lastActivityAt ?? left.task.updatedAt);
        return rightTime - leftTime || taskKey(right).localeCompare(taskKey(left));
      });
  }, [query, result?.tasks, statusFilter, workspaceFilter]);

  const selected = useMemo(
    () => entries.find((entry) => taskKey(entry) === selectedKey) ?? null,
    [entries, selectedKey],
  );
  const selectedWorkspaceId = selected?.workspaceId ?? "";
  const selectedTaskId = selected?.task.id ?? "";

  const refreshDetail = useCallback(async (workspaceId: string, taskId: string, force = false) => {
    if (!force && detailBusyRef.current) return;
    const generation = ++detailGeneration.current;
    detailBusyRef.current = true;
    setDetailBusy(true);
    setDetailError("");
    try {
      const next = await getTaskSnapshot(workspaceId, taskId);
      if (generation === detailGeneration.current) setSnapshot(next);
    } catch (cause) {
      if (generation === detailGeneration.current) {
        setSnapshot(null);
        setDetailError(String(cause));
      }
    } finally {
      if (generation === detailGeneration.current) {
        detailBusyRef.current = false;
        setDetailBusy(false);
      }
    }
  }, []);

  useEffect(() => { void refreshList(true); }, [refreshList]);

  useEffect(() => {
    if (!entries.length) {
      setSelectedKey("");
      setSnapshot(null);
      return;
    }
    if (!selected) setSelectedKey(taskKey(entries[0]));
  }, [entries, selected]);

  useEffect(() => {
    setSnapshot(null);
    if (!selectedWorkspaceId || !selectedTaskId) return;
    void refreshDetail(selectedWorkspaceId, selectedTaskId, true);
  }, [refreshDetail, selectedTaskId, selectedWorkspaceId]);

  useEffect(() => {
    if (!autoRefresh) return;
    const timer = window.setInterval(() => {
      if (document.hidden) return;
      void refreshList();
      if (selectedWorkspaceId && selectedTaskId) {
        void refreshDetail(selectedWorkspaceId, selectedTaskId);
      }
    }, REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [autoRefresh, refreshDetail, refreshList, selectedTaskId, selectedWorkspaceId]);

  const totalTasks = result?.tasks.length ?? 0;
  const activeTasks = result?.tasks.filter((entry) => entry.task.active || entry.task.status === "active").length ?? 0;
  const verifyingTasks = result?.tasks.filter((entry) => entry.task.status === "verifying").length ?? 0;
  const completedTasks = result?.tasks.filter((entry) => entry.task.status === "completed" || entry.task.status === "completed_unverified").length ?? 0;

  return (
    <PageLayout
      kicker="Tasks"
      title="任务"
      description="统一查看所有 Workspace 的 Harness 任务、进度、操作、验证和 ChangeSet。Workspace 仅作为筛选维度，不再承载独立任务页面。"
      actions={
        <div className="flex items-center gap-3">
          <label className="flex items-center gap-2 text-xs text-muted-foreground">
            <Checkbox checked={autoRefresh} onCheckedChange={(checked) => setAutoRefresh(Boolean(checked))} />
            自动刷新（2 秒）
          </label>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={busy || detailBusy}
            onClick={() => {
              void refreshList(true);
              if (selectedWorkspaceId && selectedTaskId) {
                void refreshDetail(selectedWorkspaceId, selectedTaskId, true);
              }
            }}
          >
            <RefreshCw data-icon="inline-start" className={busy || detailBusy ? "animate-spin" : undefined} />
            刷新
          </Button>
        </div>
      }
    >
      <div className="grid gap-5">
        {error && <Alert variant="destructive"><AlertTitle>无法读取任务</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}
        {!!result?.workspaceErrors.length && (
          <Alert>
            <AlertTitle>部分 Workspace 暂时不可读取</AlertTitle>
            <AlertDescription>
              {result.workspaceErrors.map((item) => `${item.workspaceName}: ${item.message}`).join("；")}
            </AlertDescription>
          </Alert>
        )}

        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          <StatCard label="总任务" value={totalTasks} />
          <StatCard label="进行中" value={activeTasks} />
          <StatCard label="验证中" value={verifyingTasks} />
          <StatCard label="已完成" value={completedTasks} />
        </div>

        <Card>
          <CardContent className="flex flex-wrap items-center gap-3 p-4">
            <div className="relative min-w-56 flex-1">
              <Search className="absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                className="pl-9"
                placeholder="搜索目标、Task ID、分支或 Commit…"
              />
            </div>
            <Select value={workspaceFilter} onValueChange={(value) => setWorkspaceFilter(value ?? "all")}>
              <SelectTrigger className="w-56"><SelectValue placeholder="Workspace" /></SelectTrigger>
              <SelectContent>
                <SelectItem value="all">全部 Workspace</SelectItem>
                {workspaces.map((workspace) => (
                  <SelectItem key={workspace.id} value={workspace.id}>{workspace.name}</SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select value={statusFilter} onValueChange={(value) => setStatusFilter((value ?? "all") as StatusFilter)}>
              <SelectTrigger className="w-44"><SelectValue placeholder="任务状态" /></SelectTrigger>
              <SelectContent>
                <SelectItem value="all">全部状态</SelectItem>
                <SelectItem value="active">进行中</SelectItem>
                <SelectItem value="verifying">验证中</SelectItem>
                <SelectItem value="paused">已暂停</SelectItem>
                <SelectItem value="completed">已完成</SelectItem>
                <SelectItem value="incomplete">未完成终止</SelectItem>
                <SelectItem value="failed">失败</SelectItem>
                <SelectItem value="worktree">Git Worktree</SelectItem>
              </SelectContent>
            </Select>
            <Badge variant="secondary">{entries.length} 条</Badge>
          </CardContent>
        </Card>

        <div className="grid min-h-[34rem] gap-4 xl:grid-cols-[23rem_minmax(0,1fr)]">
          <Card className="min-h-0">
            <CardHeader>
              <CardTitle className="flex items-center gap-2"><ListTodo className="size-4" />任务列表</CardTitle>
              <CardDescription>{result ? `刷新于 ${formatTaskTime(result.refreshedAt)}` : "正在读取任务…"}</CardDescription>
            </CardHeader>
            <CardContent className="min-h-0">
              <ScrollArea className="h-[36rem] pr-3">
                <div className="grid gap-2">
                  {entries.length ? entries.map((entry) => {
                    const key = taskKey(entry);
                    const isSelected = key === selectedKey;
                    return (
                      <button
                        key={key}
                        type="button"
                        className={cn(
                          "w-full rounded-xl border p-3 text-left transition-colors hover:bg-muted/50",
                          isSelected && "border-primary/50 bg-primary/5",
                        )}
                        onClick={() => setSelectedKey(key)}
                      >
                        <div className="flex items-start justify-between gap-2">
                          <Badge variant={statusVariant(entry.task.status)}>{taskStatusLabel(entry.task.status)}</Badge>
                          <span className="truncate text-[11px] text-muted-foreground">{entry.workspaceName}</span>
                        </div>
                        <p className="mt-2 line-clamp-2 text-sm font-medium leading-5">{entry.task.objective}</p>
                        <div className="mt-3 flex items-center justify-between gap-2 text-[11px] text-muted-foreground">
                          <code>{entry.task.id.slice(0, 10)}</code>
                          <span>{entry.task.progressPercent}%</span>
                        </div>
                        <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted">
                          <div className="h-full rounded-full bg-primary" style={{ width: `${entry.task.progressPercent}%` }} />
                        </div>
                        <p className="mt-2 truncate text-[11px] text-muted-foreground">
                          {entry.task.branch ?? "无分支"} · {formatTaskTime(entry.task.lastActivityAt ?? entry.task.updatedAt)}
                        </p>
                      </button>
                    );
                  }) : (
                    <div className="rounded-xl border border-dashed p-6 text-center text-sm text-muted-foreground">
                      {busy ? "正在读取任务…" : "没有匹配当前筛选条件的任务。"}
                    </div>
                  )}
                </div>
              </ScrollArea>
            </CardContent>
          </Card>

          <div className="min-w-0">
            {detailError && <Alert variant="destructive"><AlertTitle>无法读取任务详情</AlertTitle><AlertDescription>{detailError}</AlertDescription></Alert>}
            {selected && snapshot ? (
              <TaskDetailPanel snapshot={snapshot} workspaceName={selected.workspaceName} />
            ) : !detailError ? (
              <Card className="grid min-h-[20rem] place-items-center">
                <CardContent className="text-center text-sm text-muted-foreground">
                  {detailBusy ? "正在读取任务详情…" : "选择左侧任务查看完整 Harness 记录。"}
                </CardContent>
              </Card>
            ) : null}
          </div>
        </div>
      </div>
    </PageLayout>
  );
}

function StatCard({ label, value }: { label: string; value: number }) {
  return (
    <Card>
      <CardContent className="p-4">
        <p className="text-xs text-muted-foreground">{label}</p>
        <p className="mt-1 text-2xl font-semibold tracking-tight">{value}</p>
      </CardContent>
    </Card>
  );
}
