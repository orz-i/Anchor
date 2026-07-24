<script lang="ts">
  import CopyButton from "$lib/components/CopyButton.svelte";
  import StatusOrb from "$lib/components/StatusOrb.svelte";
  import type { RuntimeRecovery, RuntimeState } from "$lib/types";

  const EMPTY_RECOVERY: RuntimeRecovery = {
    enabled: false,
    attempt: 0,
    maxAttempts: 5,
    retryInMs: null,
    recoveredCount: 0,
    lastError: "",
  };

  interface Props {
    title: string;
    subtitle: string;
    status: RuntimeState;
    statusMessage?: string;
    recovery?: RuntimeRecovery;
    port: number;
    portEditable?: boolean;
    busy?: boolean;
    tunnelType?: string;
    localEndpoint: string;
    publicEndpoint?: string;
    publicLabel?: string;
    onToggle: () => void | Promise<void>;
    onPortChange?: (port: number) => void | Promise<void>;
  }

  let {
    title,
    subtitle,
    status,
    statusMessage = "",
    recovery = EMPTY_RECOVERY,
    port,
    portEditable = false,
    busy = false,
    tunnelType = "none",
    localEndpoint,
    publicEndpoint = "",
    publicLabel = "公网",
    onToggle,
    onPortChange,
  }: Props = $props();

  let draftPort = $state(0);

  $effect(() => {
    draftPort = port;
  });

  const running = $derived(status === "running");
  const recovering = $derived(status === "recovering");
  const showError = $derived(status === "error" && Boolean(statusMessage));
  const canEditPort = $derived(
    portEditable && !running && !recovering && status !== "starting" && status !== "stopping",
  );
  const retrySeconds = $derived(
    recovery.retryInMs === null ? null : Math.max(1, Math.ceil(recovery.retryInMs / 1000)),
  );
  const tunnelEnabled = $derived(tunnelType === "cloudflare" || tunnelType === "frp");
  const tunnelLabel = $derived(
    tunnelType === "cloudflare" ? "Cloudflare" : tunnelType === "frp" ? "FRP" : "",
  );

  async function commitPort() {
    if (!onPortChange || draftPort === port) return;
    if (draftPort < 1024 || draftPort > 65535) {
      draftPort = port;
      return;
    }
    await onPortChange(draftPort);
  }
</script>

<article class="tx-card p-5">
  <div class="flex items-start justify-between gap-3">
    <div class="min-w-0">
      <div class="flex items-center gap-2">
        <StatusOrb state={status} />
        <h3 class="text-[15px] font-semibold tracking-tight">{title}</h3>
      </div>
      <p class="mt-1 text-sm text-[var(--color-text-muted)]">{subtitle}</p>
      {#if tunnelEnabled}
        <p class="mt-1 text-xs text-[var(--color-text-muted)]">
          {tunnelLabel} 隧道随服务自动连接，停止服务时一并断开
        </p>
      {/if}
      {#if running && recovery.recoveredCount > 0}
        <p class="mt-1 text-xs text-[var(--color-success)]">
          已自动恢复 {recovery.recoveredCount} 次
        </p>
      {/if}
    </div>
    <button
      type="button"
      class="tx-btn-primary shrink-0"
      class:tx-btn-danger={running}
      disabled={busy || status === "starting" || status === "stopping"}
      onclick={onToggle}
    >
      {#if busy}
        处理中…
      {:else if running}
        停止
      {:else if recovering}
        立即重试
      {:else}
        启动
      {/if}
    </button>
  </div>

  {#if showError}
    <div class="tx-alert tx-alert--error mt-4" role="alert">
      {statusMessage}
    </div>
  {/if}

  {#if recovering}
    <div class="tx-alert tx-alert--warning mt-4" role="status">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <span>{statusMessage || "连接中断，正在自动恢复"}</span>
        <span class="text-xs opacity-80">
          {#if retrySeconds !== null}
            {retrySeconds}s 后重试
          {:else}
            第 {Math.min(recovery.attempt + 1, recovery.maxAttempts)}/{recovery.maxAttempts} 次
          {/if}
        </span>
      </div>
    </div>
  {/if}

  <div class="mt-5 grid gap-3">
    <div class="tx-info-block">
      <div class="tx-info-row">
        <span class="tx-info-label">端口</span>
        {#if canEditPort}
          <input
            type="number"
            min="1024"
            max="65535"
            class="tx-input tx-input-inline"
            bind:value={draftPort}
            onchange={commitPort}
          />
        {:else}
          <span class="tx-mono text-sm">{port}</span>
        {/if}
      </div>
    </div>

    <div class="tx-info-block">
      <div class="tx-info-row">
        <span class="tx-info-label">本地地址</span>
        <CopyButton value={localEndpoint} />
      </div>
      <p class="tx-mono mt-1.5 truncate text-sm">{localEndpoint}</p>
    </div>

    {#if publicEndpoint || publicLabel}
      <div class="tx-info-block">
        <div class="tx-info-row">
          <span class="tx-info-label">{publicLabel}</span>
          {#if publicEndpoint}
            <CopyButton value={publicEndpoint} />
          {/if}
        </div>
        <p class="tx-mono mt-1.5 truncate text-sm text-[var(--color-text-secondary)]">
          {publicEndpoint || "未配置隧道"}
        </p>
      </div>
    {/if}
  </div>
</article>
