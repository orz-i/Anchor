<script lang="ts">
  import { readWorkspaceLogs, type LogChunk, type LogService } from "$lib/api/logs";

  const AUTO_REFRESH_INTERVAL_MS = 3_000;

  interface Props {
    workspaceId: string;
    service: LogService;
    autoRefresh?: boolean;
    title?: string;
  }

  let { workspaceId, service, autoRefresh = true, title }: Props = $props();

  let chunks = $state<LogChunk[]>([]);
  let busy = $state(false);
  let error = $state("");
  let autoRefreshEnabled = $state(true);
  let requestGeneration = 0;

  const heading = $derived(title ?? (service === "mcp" ? "MCP 日志" : "Actions 日志"));

  async function refresh(
    targetWorkspaceId = workspaceId,
    targetService = service,
    force = false,
  ) {
    if ((!force && busy) || !targetWorkspaceId) return;
    const generation = ++requestGeneration;
    busy = true;
    error = "";
    try {
      const nextChunks = await readWorkspaceLogs(targetWorkspaceId, targetService);
      if (generation === requestGeneration) {
        chunks = nextChunks;
      }
    } catch (err) {
      if (generation === requestGeneration) {
        error = String(err);
        chunks = [];
      }
    } finally {
      if (generation === requestGeneration) {
        busy = false;
      }
    }
  }

  function toggleAutoRefresh(event: Event) {
    autoRefreshEnabled = (event.currentTarget as HTMLInputElement).checked;
    if (autoRefreshEnabled) {
      void refresh(workspaceId, service, true);
    }
  }

  $effect(() => {
    autoRefreshEnabled = autoRefresh;
  });

  $effect(() => {
    const targetWorkspaceId = workspaceId;
    const targetService = service;
    queueMicrotask(() => {
      void refresh(targetWorkspaceId, targetService, true);
    });
  });

  $effect(() => {
    if (!autoRefreshEnabled) return;

    const timer = window.setInterval(() => {
      void refresh();
    }, AUTO_REFRESH_INTERVAL_MS);

    return () => window.clearInterval(timer);
  });
</script>

<section class="tx-card p-5">
  <div class="flex items-start justify-between gap-3">
    <div>
      <h3 class="font-semibold">{heading}</h3>
      <p class="mt-1 text-sm text-[var(--text-muted)]">Daemon 有界日志快照（最多 8KB）</p>
    </div>
    <div class="flex shrink-0 items-center gap-3">
      <label class="inline-flex items-center gap-2 text-xs text-[var(--text-secondary)]">
        <input
          type="checkbox"
          class="h-4 w-4"
          checked={autoRefreshEnabled}
          onchange={toggleAutoRefresh}
        />
        <span>自动刷新（3 秒）</span>
      </label>
      <button
        type="button"
        class="tx-btn-ghost shrink-0 disabled:opacity-50"
        disabled={busy}
        onclick={() => refresh()}
      >
        {busy ? "刷新中…" : "刷新"}
      </button>
    </div>
  </div>

  {#if error}
    <p
      class="mt-4 rounded-lg border border-[var(--danger)]/30 bg-[var(--danger)]/10 px-3 py-2 text-sm text-[var(--danger)]"
    >
      {error}
    </p>
  {/if}

  {#if chunks.length > 0}
    <div class="mt-4 grid gap-3">
      {#each chunks as chunk (chunk.name)}
        <div class="overflow-hidden rounded-lg border border-[var(--border)] bg-[var(--page-bg)]">
          <p class="border-b border-[var(--border)] px-3 py-1.5 font-mono text-xs text-[var(--text-muted)]">
            {chunk.name}
          </p>
          <pre
            class="max-h-48 overflow-auto whitespace-pre-wrap break-words p-3 font-mono text-xs leading-relaxed"
          >{chunk.content || "（空）"}</pre>
        </div>
      {/each}
    </div>
  {:else if !busy && !error}
    <p class="mt-4 text-sm text-[var(--text-muted)]">当前还没有日志</p>
  {/if}
</section>
