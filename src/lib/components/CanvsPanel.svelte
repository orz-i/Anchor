<script lang="ts">
  import {
    getCanvsSnapshot,
    type CanvsSnapshot,
    type CanvsTaskStatus,
  } from "$lib/api/canvs";
  import CopyButton from "$lib/components/CopyButton.svelte";

  const AUTO_REFRESH_INTERVAL_MS = 2_000;

  interface Props {
    workspaceId: string;
    localUrl?: string;
    publicUrl?: string;
    onTaskStatusChange?: (status: CanvsTaskStatus | null) => void;
  }

  let { workspaceId, localUrl = "", publicUrl = "", onTaskStatusChange }: Props = $props();

  let snapshot = $state<CanvsSnapshot | null>(null);
  let busy = $state(false);
  let error = $state("");
  let autoRefreshEnabled = $state(true);
  let requestGeneration = 0;

  const task = $derived(snapshot?.task ?? null);
  const completedCount = $derived(task?.completedSteps.length ?? 0);
  const pendingCount = $derived(task?.pendingSteps.length ?? 0);

  function taskStatusLabel(status: CanvsTaskStatus): string {
    switch (status) {
      case "active":
        return "进行中";
      case "paused":
        return "已暂停";
      case "verifying":
        return "验证中";
      case "failed":
        return "失败";
      case "completed":
        return "已完成";
      case "completed_unverified":
        return "完成未验证";
      case "rolled_back":
        return "已回滚";
      default:
        return "未知";
    }
  }

  function statusClass(status: CanvsTaskStatus): string {
    if (status === "failed") {
      return "border-[var(--danger)]/30 bg-[var(--danger)]/10 text-[var(--danger)]";
    }
    if (status === "verifying") {
      return "border-[var(--warning)]/30 bg-[var(--warning)]/10 text-[var(--warning)]";
    }
    if (status === "active" || status === "completed") {
      return "border-[var(--success)]/30 bg-[var(--success)]/10 text-[var(--success)]";
    }
    return "border-[var(--border)] bg-[var(--surface-hover)] text-[var(--text-secondary)]";
  }

  function dispositionLabel(disposition: string): string {
    switch (disposition) {
      case "passed":
        return "通过";
      case "active_failure":
      case "failed":
        return "失败";
      case "diagnostic_only":
        return "诊断";
      case "expected_failure":
        return "预期失败";
      case "superseded":
        return "已取代";
      case "waived":
        return "已豁免";
      default:
        return disposition || "未知";
    }
  }

  function outcomeClass(ok: boolean | null): string {
    if (ok === true) return "text-[var(--success)]";
    if (ok === false) return "text-[var(--danger)]";
    return "text-[var(--text-muted)]";
  }

  function formatTime(raw: string): string {
    if (!raw) return "—";
    let date: Date;
    if (raw.startsWith("unix:")) {
      date = new Date(Number(raw.slice(5)) * 1_000);
    } else if (/^\d{13}$/.test(raw)) {
      date = new Date(Number(raw));
    } else if (/^\d{10}$/.test(raw)) {
      date = new Date(Number(raw) * 1_000);
    } else {
      date = new Date(raw);
    }
    if (Number.isNaN(date.getTime())) return raw;
    return date.toLocaleString([], {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  function shortHash(value: string | null): string {
    return value ? value.slice(0, 10) : "—";
  }

  function formatDuration(durationMs: number | null): string {
    if (durationMs === null) return "";
    if (durationMs < 1_000) return `${durationMs} ms`;
    return `${(durationMs / 1_000).toFixed(durationMs < 10_000 ? 1 : 0)} s`;
  }

  async function refresh(targetWorkspaceId = workspaceId, force = false) {
    if ((!force && busy) || !targetWorkspaceId) return;
    const generation = ++requestGeneration;
    busy = true;
    error = "";
    try {
      const next = await getCanvsSnapshot(targetWorkspaceId);
      if (generation !== requestGeneration) return;
      snapshot = next;
      onTaskStatusChange?.(next.task?.status ?? null);
    } catch (err) {
      if (generation !== requestGeneration) return;
      error = String(err);
      onTaskStatusChange?.(null);
    } finally {
      if (generation === requestGeneration) busy = false;
    }
  }

  function toggleAutoRefresh(event: Event) {
    autoRefreshEnabled = (event.currentTarget as HTMLInputElement).checked;
    if (autoRefreshEnabled) void refresh(workspaceId, true);
  }

  $effect(() => {
    const targetWorkspaceId = workspaceId;
    queueMicrotask(() => {
      void refresh(targetWorkspaceId, true);
    });
  });

  $effect(() => {
    if (!autoRefreshEnabled) return;
    const timer = window.setInterval(() => {
      if (!document.hidden) void refresh();
    }, AUTO_REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  });
</script>

<section class="grid gap-4" aria-live="polite">
  <div class="tx-card p-5">
    <div class="flex flex-wrap items-start justify-between gap-4">
      <div>
        <p class="page-kicker">Canvs</p>
        <h3 class="text-lg font-semibold">当前 Harness 任务</h3>
        <p class="mt-1 text-sm text-[var(--text-muted)]">
          实时读取当前 Workspace 的步骤、操作、提交和验证状态。
        </p>
      </div>
      <div class="flex flex-wrap items-center gap-3">
        <label class="inline-flex items-center gap-2 text-xs text-[var(--text-secondary)]">
          <input
            type="checkbox"
            class="h-4 w-4"
            checked={autoRefreshEnabled}
            onchange={toggleAutoRefresh}
          />
          <span>自动刷新（2 秒）</span>
        </label>
        <button
          type="button"
          class="tx-btn-ghost disabled:opacity-50"
          disabled={busy}
          onclick={() => refresh(workspaceId, true)}
        >
          {busy ? "刷新中…" : "刷新"}
        </button>
      </div>
    </div>

    <div class="mt-4 grid gap-2 rounded-xl border border-[var(--border)] bg-[var(--page-bg)] p-3">
      <div class="flex min-w-0 items-center gap-3">
        <span class="w-16 shrink-0 text-xs text-[var(--text-muted)]">本地网页</span>
        <code class="min-w-0 flex-1 truncate text-xs">{localUrl || "—"}</code>
        {#if localUrl}
          <CopyButton value={localUrl} label="复制" />
        {/if}
      </div>
      <div class="flex min-w-0 items-center gap-3">
        <span class="w-16 shrink-0 text-xs text-[var(--text-muted)]">公网网页</span>
        <code class="min-w-0 flex-1 truncate text-xs">{publicUrl || "隧道未连接"}</code>
        {#if publicUrl}
          <CopyButton value={publicUrl} label="复制" />
        {/if}
      </div>
      <p class="text-xs leading-5 text-[var(--text-muted)]">
        网页入口与 MCP 共用当前工作区的 listener 和隧道。OAuth 模式使用授权口令，Bearer 模式使用 Bearer Token。
      </p>
    </div>

    {#if error}
      <div class="tx-alert tx-alert--error mt-4" role="alert">
        <p class="font-medium">无法读取 Canvs 状态</p>
        <p class="mt-1 break-words text-xs opacity-80">{error}</p>
      </div>
    {/if}

    {#if task}
      <div class="mt-5 flex flex-wrap items-start justify-between gap-4">
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <span class={`rounded-full border px-2.5 py-1 text-xs font-medium ${statusClass(task.status)}`}>
              {taskStatusLabel(task.status)}
            </span>
            <span class="font-mono text-xs text-[var(--text-muted)]">{task.id}</span>
          </div>
          <p class="mt-3 text-sm font-medium leading-6">{task.objective}</p>
          <p class="mt-2 text-xs text-[var(--text-muted)]">
            更新于 {formatTime(task.updatedAt)} · 分支 {task.branch ?? "—"} · HEAD {shortHash(task.expectedHead)}
          </p>
        </div>
        <div class="w-full max-w-xs shrink-0">
          <div class="flex items-center justify-between text-xs text-[var(--text-muted)]">
            <span>步骤进度</span>
            <span>{completedCount}/{completedCount + pendingCount}</span>
          </div>
          <div class="mt-2 h-2 overflow-hidden rounded-full bg-[var(--surface-hover)]">
            <div
              class="h-full rounded-full bg-[var(--primary)] transition-[width] duration-300"
              style={`width: ${task.progressPercent}%`}
            ></div>
          </div>
          <p class="mt-2 text-right text-xs font-medium">{task.progressPercent}%</p>
        </div>
      </div>
    {:else if !busy && !error}
      <div class="mt-5 rounded-xl border border-dashed border-[var(--border)] p-6 text-center">
        <p class="font-medium">当前没有活动 Harness 任务</p>
        <p class="mt-2 text-sm text-[var(--text-muted)]">
          在该 Workspace 中创建或恢复任务后，Canvs 会自动显示实时进度。
        </p>
      </div>
    {/if}
  </div>

  {#if task && snapshot}
    <div class="grid gap-4 xl:grid-cols-2">
      <section class="tx-card p-5">
        <div class="flex items-center justify-between gap-3">
          <h3 class="font-semibold">步骤</h3>
          <span class="text-xs text-[var(--text-muted)]">{completedCount} 完成 · {pendingCount} 待办</span>
        </div>
        <div class="mt-4 grid gap-4 md:grid-cols-2">
          <div>
            <p class="tx-section-label">已完成</p>
            {#if task.completedSteps.length > 0}
              <ol class="grid gap-2">
                {#each task.completedSteps as step, index}
                  <li class="flex gap-2 text-sm leading-5">
                    <span class="mt-0.5 text-[var(--success)]">✓</span>
                    <span>{step}</span>
                  </li>
                {/each}
              </ol>
            {:else}
              <p class="text-sm text-[var(--text-muted)]">尚无已完成步骤</p>
            {/if}
          </div>
          <div>
            <p class="tx-section-label">待处理</p>
            {#if task.pendingSteps.length > 0}
              <ol class="grid gap-2">
                {#each task.pendingSteps as step, index}
                  <li class="flex gap-2 text-sm leading-5">
                    <span class="mt-0.5 font-mono text-xs text-[var(--text-muted)]">{index + 1}.</span>
                    <span>{step}</span>
                  </li>
                {/each}
              </ol>
            {:else}
              <p class="text-sm text-[var(--text-muted)]">没有待处理步骤</p>
            {/if}
          </div>
        </div>
      </section>

      <section class="tx-card p-5">
        <h3 class="font-semibold">任务基线</h3>
        <dl class="mt-4 grid gap-3 text-sm sm:grid-cols-2">
          <div class="rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
            <dt class="text-xs text-[var(--text-muted)]">初始 HEAD</dt>
            <dd class="mt-1 font-mono">{shortHash(task.head)}</dd>
          </div>
          <div class="rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
            <dt class="text-xs text-[var(--text-muted)]">当前预期 HEAD</dt>
            <dd class="mt-1 font-mono">{shortHash(task.expectedHead)}</dd>
          </div>
          <div class="rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
            <dt class="text-xs text-[var(--text-muted)]">最新变更</dt>
            <dd class="mt-1 break-all font-mono text-xs">{task.latestChangeId ?? "—"}</dd>
          </div>
          <div class="rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
            <dt class="text-xs text-[var(--text-muted)]">最新验证</dt>
            <dd class="mt-1 break-all font-mono text-xs">{task.latestVerificationId ?? "—"}</dd>
          </div>
        </dl>
      </section>
    </div>

    <div class="grid gap-4 xl:grid-cols-2">
      <section class="tx-card min-w-0 p-5">
        <div class="flex items-center justify-between gap-3">
          <h3 class="font-semibold">最近操作</h3>
          <span class="text-xs text-[var(--text-muted)]">最近 {snapshot.recentOperations.length} 条</span>
        </div>
        {#if snapshot.recentOperations.length > 0}
          <div class="mt-4 grid gap-2">
            {#each snapshot.recentOperations as operation, index (operation.id + operation.createdAt + index)}
              <div class="rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
                <div class="flex min-w-0 items-start justify-between gap-3">
                  <div class="min-w-0">
                    <p class="truncate text-sm font-medium">{operation.tool}</p>
                    <p class="mt-1 text-xs text-[var(--text-muted)]">
                      {operation.kind} · {formatTime(operation.createdAt)}
                    </p>
                  </div>
                  <span class={`shrink-0 text-xs font-medium ${outcomeClass(operation.ok)}`}>
                    {operation.status}
                  </span>
                </div>
                {#if operation.affectedFiles > 0 || operation.durationMs !== null}
                  <p class="mt-2 text-xs text-[var(--text-muted)]">
                    {operation.affectedFiles > 0 ? `${operation.affectedFiles} 个文件` : ""}
                    {operation.affectedFiles > 0 && operation.durationMs !== null ? " · " : ""}
                    {formatDuration(operation.durationMs)}
                  </p>
                {/if}
              </div>
            {/each}
          </div>
        {:else}
          <p class="mt-4 text-sm text-[var(--text-muted)]">当前任务还没有操作记录</p>
        {/if}
      </section>

      <section class="tx-card min-w-0 p-5">
        <div class="flex items-center justify-between gap-3">
          <h3 class="font-semibold">有效验证</h3>
          <span class="text-xs text-[var(--text-muted)]">按命令折叠最新结果</span>
        </div>
        {#if snapshot.verifications.length > 0}
          <div class="mt-4 grid gap-2">
            {#each snapshot.verifications as verification (verification.id)}
              <div class="rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
                <div class="flex min-w-0 items-start justify-between gap-3">
                  <div class="min-w-0">
                    <p class="truncate font-mono text-xs">{verification.command}</p>
                    <p class="mt-1 text-xs text-[var(--text-muted)]">
                      {verification.kind} · {verification.level} · {formatTime(verification.createdAt)}
                    </p>
                  </div>
                  <span class={`shrink-0 text-xs font-medium ${outcomeClass(verification.passed)}`}>
                    {dispositionLabel(verification.disposition)}
                  </span>
                </div>
                {#if verification.exitCode !== null || verification.durationMs !== null}
                  <p class="mt-2 text-xs text-[var(--text-muted)]">
                    {verification.exitCode !== null ? `退出码 ${verification.exitCode}` : ""}
                    {verification.exitCode !== null && verification.durationMs !== null ? " · " : ""}
                    {formatDuration(verification.durationMs)}
                  </p>
                {/if}
              </div>
            {/each}
          </div>
        {:else}
          <p class="mt-4 text-sm text-[var(--text-muted)]">当前任务还没有验证记录</p>
        {/if}
      </section>
    </div>

    <div class="grid gap-4 xl:grid-cols-2">
      <section class="tx-card min-w-0 p-5">
        <div class="flex items-center justify-between gap-3">
          <h3 class="font-semibold">分段提交</h3>
          <span class="text-xs text-[var(--text-muted)]">{snapshot.changes.length} 条</span>
        </div>
        {#if snapshot.changes.length > 0}
          <div class="mt-4 grid gap-2">
            {#each snapshot.changes as change (change.id)}
              <div class="rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
                <div class="flex items-start justify-between gap-3">
                  <span class="font-mono text-xs">{shortHash(change.commitSha ?? change.id)}</span>
                  <span class="text-xs text-[var(--text-muted)]">{formatTime(change.createdAt)}</span>
                </div>
                <p class="mt-2 text-xs text-[var(--text-muted)]">
                  {change.committedFiles.length} 个文件 · {change.verificationCount} 条验证
                </p>
                {#if change.committedFiles.length > 0}
                  <p class="mt-2 truncate font-mono text-xs text-[var(--text-secondary)]">
                    {change.committedFiles.slice(0, 3).join(" · ")}
                  </p>
                {/if}
              </div>
            {/each}
          </div>
        {:else}
          <p class="mt-4 text-sm text-[var(--text-muted)]">当前任务还没有分段提交</p>
        {/if}
      </section>

      <section class="tx-card min-w-0 p-5">
        <div class="flex items-center justify-between gap-3">
          <h3 class="font-semibold">任务事件</h3>
          <span class="text-xs text-[var(--text-muted)]">最近 {snapshot.recentEvents.length} 条</span>
        </div>
        {#if snapshot.recentEvents.length > 0}
          <div class="mt-4 grid gap-2">
            {#each snapshot.recentEvents as event (event.id)}
              <div class="flex items-start justify-between gap-3 rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
                <div class="min-w-0">
                  <p class="truncate text-sm font-medium">{event.kind}</p>
                  <p class="mt-1 text-xs text-[var(--text-muted)]">
                    {event.toolName ?? "Harness"} · {formatTime(event.createdAt)}
                  </p>
                </div>
                <span class={`shrink-0 text-xs font-medium ${outcomeClass(event.ok)}`}>
                  {event.affectedFiles > 0 ? `${event.affectedFiles} 文件` : event.ok === false ? "失败" : ""}
                </span>
              </div>
            {/each}
          </div>
        {:else}
          <p class="mt-4 text-sm text-[var(--text-muted)]">当前任务还没有事件记录</p>
        {/if}
      </section>
    </div>

    <p class="px-1 text-right text-xs text-[var(--text-muted)]">
      最近刷新：{formatTime(snapshot.refreshedAt)}
    </p>
  {/if}
</section>
