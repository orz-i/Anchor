import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import type { TaskSnapshot, TaskStatus } from "@/lib/api/tasks";

export function taskStatusLabel(status: TaskStatus): string {
  return ({
    active: "进行中",
    paused: "已暂停",
    verifying: "验证中",
    failed: "失败",
    incomplete: "未完成终止",
    completed: "已完成",
    completed_unverified: "完成未验证",
    rolled_back: "已回滚",
    unknown: "未知",
  })[status];
}

export function formatTaskTime(raw: string | null): string {
  if (!raw) return "—";
  const date = raw.startsWith("unix:")
    ? new Date(Number(raw.slice(5)) * 1000)
    : /^\d{13}$/.test(raw)
      ? new Date(Number(raw))
      : /^\d{10}$/.test(raw)
        ? new Date(Number(raw) * 1000)
        : new Date(raw);
  return Number.isNaN(date.getTime()) ? raw : date.toLocaleString();
}

export function taskTimeValue(raw: string | null): number {
  if (!raw) return 0;
  const rendered = raw.startsWith("unix:")
    ? Number(raw.slice(5)) * 1000
    : /^\d{13}$/.test(raw)
      ? Number(raw)
      : /^\d{10}$/.test(raw)
        ? Number(raw) * 1000
        : new Date(raw).getTime();
  return Number.isFinite(rendered) ? rendered : 0;
}

function shortHash(value: string | null): string {
  return value ? value.slice(0, 10) : "—";
}

function duration(value: number | null): string {
  if (value === null) return "";
  return value < 1000 ? `${value} ms` : `${(value / 1000).toFixed(value < 10000 ? 1 : 0)} s`;
}

function taskStatusVariant(status: TaskStatus): "default" | "secondary" | "destructive" | "outline" {
  if (status === "failed" || status === "incomplete") return "destructive";
  if (status === "active" || status === "completed") return "secondary";
  return "outline";
}

export function TaskDetailPanel({
  snapshot,
  workspaceName,
}: {
  snapshot: TaskSnapshot;
  workspaceName: string;
}) {
  const task = snapshot.task;
  if (!task) {
    return (
      <Card>
        <CardContent className="p-8 text-center text-sm text-muted-foreground">任务不存在或已被清理。</CardContent>
      </Card>
    );
  }

  const completed = task.completedSteps.length;
  const pending = task.pendingSteps.length;

  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-2">
                <Badge variant={taskStatusVariant(task.status)}>{taskStatusLabel(task.status)}</Badge>
                {task.active && <Badge variant="outline">活动任务</Badge>}
                {task.current && <Badge variant="outline">默认任务</Badge>}
                <Badge variant="outline">{workspaceName}</Badge>
              </div>
              <CardTitle className="mt-3 text-lg leading-7">{task.objective}</CardTitle>
              <CardDescription className="mt-2 break-all font-mono text-xs">{task.id}</CardDescription>
            </div>
            <div className="min-w-44 text-right text-xs text-muted-foreground">
              <p>{formatTaskTime(task.lastActivityAt ?? task.updatedAt)}</p>
              <p className="mt-1">{task.workspaceMode === "worktree" ? "Git Worktree" : "共享工作区"}</p>
            </div>
          </div>
        </CardHeader>
        <CardContent className="grid gap-4">
          <div>
            <div className="flex justify-between text-xs text-muted-foreground">
              <span>任务进度</span>
              <span>{completed}/{completed + pending} · {task.progressPercent}%</span>
            </div>
            <div className="mt-2 h-2 overflow-hidden rounded-full bg-muted">
              <div className="h-full rounded-full bg-primary" style={{ width: `${task.progressPercent}%` }} />
            </div>
          </div>
          <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
            {[
              ["分支", task.branch ?? "—"],
              ["初始 HEAD", shortHash(task.head)],
              ["预期 HEAD", shortHash(task.expectedHead)],
              ["最新变更", task.latestChangeId ? shortHash(task.latestChangeId) : "—"],
            ].map(([label, value]) => (
              <div key={label} className="rounded-xl border bg-muted/20 p-3">
                <p className="text-xs text-muted-foreground">{label}</p>
                <p className="mt-1 break-all font-mono text-xs">{value}</p>
              </div>
            ))}
          </div>
        </CardContent>
      </Card>

      <div className="grid gap-4 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>步骤</CardTitle>
            <CardDescription>{completed} 完成 · {pending} 待办</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-5 md:grid-cols-2">
            <StepList title="已完成" steps={task.completedSteps} completed />
            <StepList title="待处理" steps={task.pendingSteps} />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>有效验证</CardTitle>
            <CardDescription>按任务快照读取最新有效结果</CardDescription>
          </CardHeader>
          <CardContent>
            <ScrollArea className="h-72">
              <div className="grid gap-2 pr-3">
                {snapshot.verifications.length ? snapshot.verifications.map((verification) => (
                  <div key={verification.id} className="rounded-xl border p-3">
                    <div className="flex justify-between gap-3">
                      <div className="min-w-0">
                        <p className="truncate font-mono text-xs">{verification.command}</p>
                        <p className="mt-1 text-xs text-muted-foreground">
                          {verification.kind} · {verification.level} · {formatTaskTime(verification.createdAt)}
                        </p>
                      </div>
                      <Badge variant={verification.passed ? "secondary" : "destructive"}>
                        {verification.disposition || verification.status}
                      </Badge>
                    </div>
                    {(verification.exitCode !== null || verification.durationMs !== null) && (
                      <p className="mt-2 text-xs text-muted-foreground">
                        {verification.exitCode !== null ? `退出码 ${verification.exitCode}` : ""}
                        {verification.exitCode !== null && verification.durationMs !== null ? " · " : ""}
                        {duration(verification.durationMs)}
                      </p>
                    )}
                  </div>
                )) : <p className="text-sm text-muted-foreground">当前任务还没有验证记录。</p>}
              </div>
            </ScrollArea>
          </CardContent>
        </Card>
      </div>

      <div className="grid gap-4 xl:grid-cols-2">
        <TimelineCard title="最近操作" count={snapshot.recentOperations.length}>
          {snapshot.recentOperations.map((operation) => (
            <div key={operation.id} className="rounded-xl border p-3">
              <div className="flex justify-between gap-3">
                <div className="min-w-0">
                  <p className="truncate text-sm font-medium">{operation.tool}</p>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {operation.kind} · {formatTaskTime(operation.createdAt)}
                  </p>
                </div>
                <Badge variant={operation.ok === false ? "destructive" : "outline"}>{operation.status}</Badge>
              </div>
              {(operation.affectedFiles > 0 || operation.durationMs !== null) && (
                <p className="mt-2 text-xs text-muted-foreground">
                  {operation.affectedFiles ? `${operation.affectedFiles} 个文件` : ""}
                  {operation.affectedFiles && operation.durationMs !== null ? " · " : ""}
                  {duration(operation.durationMs)}
                </p>
              )}
            </div>
          ))}
        </TimelineCard>

        <TimelineCard title="任务事件" count={snapshot.recentEvents.length}>
          {snapshot.recentEvents.map((event) => (
            <div key={event.id} className="flex justify-between gap-3 rounded-xl border p-3">
              <div className="min-w-0">
                <p className="truncate text-sm font-medium">{event.kind}</p>
                <p className="mt-1 text-xs text-muted-foreground">
                  {event.toolName ?? "Harness"} · {formatTaskTime(event.createdAt)}
                </p>
              </div>
              {event.affectedFiles > 0 && (
                <Badge variant={event.ok === false ? "destructive" : "outline"}>{event.affectedFiles} 文件</Badge>
              )}
            </div>
          ))}
        </TimelineCard>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>分段提交</CardTitle>
          <CardDescription>{snapshot.changes.length} 条 ChangeSet</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
          {snapshot.changes.length ? snapshot.changes.map((change) => (
            <div key={change.id} className="rounded-xl border p-3">
              <div className="flex justify-between gap-3">
                <code className="text-xs">{shortHash(change.commitSha ?? change.id)}</code>
                <span className="text-xs text-muted-foreground">{formatTaskTime(change.createdAt)}</span>
              </div>
              <p className="mt-2 text-xs text-muted-foreground">
                {change.committedFiles.length} 个文件 · {change.verificationCount} 条验证
              </p>
            </div>
          )) : <p className="text-sm text-muted-foreground">当前任务还没有分段提交。</p>}
        </CardContent>
      </Card>
    </div>
  );
}

function StepList({ title, steps, completed = false }: { title: string; steps: string[]; completed?: boolean }) {
  return (
    <div>
      <p className="mb-2 text-xs font-medium text-muted-foreground">{title}</p>
      <ol className="grid gap-2">
        {steps.length ? steps.map((step, index) => (
          <li key={`${index}-${step}`} className="flex gap-2 text-sm">
            <span className={completed ? "text-emerald-600" : "font-mono text-xs text-muted-foreground"}>
              {completed ? "✓" : `${index + 1}.`}
            </span>
            <span>{step}</span>
          </li>
        )) : <li className="text-sm text-muted-foreground">暂无记录</li>}
      </ol>
    </div>
  );
}

function TimelineCard({ title, count, children }: { title: string; count: number; children: React.ReactNode }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>最近 {count} 条</CardDescription>
      </CardHeader>
      <CardContent>
        <ScrollArea className="h-72">
          <div className="grid gap-2 pr-3">
            {count ? children : <p className="text-sm text-muted-foreground">当前任务还没有记录。</p>}
          </div>
        </ScrollArea>
      </CardContent>
    </Card>
  );
}
