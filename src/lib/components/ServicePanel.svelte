<script lang="ts">
  import CopyButton from "$lib/components/CopyButton.svelte";
  import StatusOrb from "$lib/components/StatusOrb.svelte";
  import type { McpActivity, McpActivityState, RuntimeRecovery, RuntimeState } from "$lib/types";

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
    activity?: McpActivity | null;
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
    activity = null,
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

  function activityLabel(state: McpActivityState): string {
    switch (state) {
      case "active":
        return "调用中";
      case "recent":
        return "刚刚活跃";
      case "suspected_stalled":
        return "疑似异常";
      case "idle":
        return "工具空闲";
      default:
        return "未知";
    }
  }

  function activityColor(state: McpActivityState): string {
    if (state === "suspected_stalled") return "var(--warning)";
    if (state === "active" || state === "recent") return "var(--success)";
    return "var(--text-muted)";
  }

  function durationLabel(milliseconds: number | null): string {
    if (milliseconds === null) return "-";
    if (milliseconds < 1_000) return `${milliseconds}ms`;
    const seconds = Math.floor(milliseconds / 1_000);
    if (seconds < 60) return `${seconds}s`;
    return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  }

  function activityAgeLabel(milliseconds: number | null): string {
    return milliseconds === null ? "尚无记录" : `${durationLabel(milliseconds)} 前`;
  }
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
      <p class="mt-1 text-sm text-[var(--text-muted)]">{subtitle}</p>
      {#if tunnelEnabled}
        <p class="mt-1 text-xs text-[var(--text-muted)]">
          {tunnelLabel} 隧道独立保活；配置重载不会更换公网链接，手动停止服务时才断开
        </p>
      {/if}
      {#if running && activity}
        <p class="mt-1 text-xs" style={`color: ${activityColor(activity.state)}`}>
          上游调用：{activityLabel(activity.state)}
        </p>
      {/if}
      {#if running && recovery.recoveredCount > 0}
        <p class="mt-1 text-xs text-[var(--success)]">
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

    {#if activity}
      <div class="tx-info-block">
        <div class="tx-info-row">
          <span class="tx-info-label">MCP 工具活动</span>
          <span class="text-sm font-semibold" style={`color: ${activityColor(activity.state)}`}>
            {activityLabel(activity.state)}
          </span>
        </div>
        <p class="mt-1.5 text-sm text-[var(--text-secondary)]">{activity.message}</p>
        <p class="mt-1 text-xs text-[var(--text-muted)]">
          在途 {activity.inFlightRequests} · 最久 {durationLabel(activity.oldestInFlightMs)} ·
          最近工具活动 {activityAgeLabel(activity.lastActivityAgeMs)}
        </p>
        <p class="mt-1 text-xs text-[var(--text-muted)]">
          MCP 服务 {running ? "运行中" : recovering ? "恢复中" : "未运行"} · 最近协议活动
          {activityAgeLabel(activity.lastTransportActivityAgeMs)}
          {#if activity.lastTransportMethod}· {activity.lastTransportMethod}{/if}
        </p>
        {#if activity.currentTool || activity.currentMethod}
          <p class="tx-mono mt-1 truncate text-xs text-[var(--text-muted)]">
            {activity.currentTool || activity.currentMethod}
          </p>
        {/if}
        <p class="mt-1.5 text-xs text-[var(--text-muted)]">
          工具活动仅统计 tools/call、resources/read、prompts/get；协议活动另行统计正常 MCP
          请求/通知。模型纯推理或 MCP 外等待仍无法识别。
        </p>
      </div>
    {/if}

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
        <p class="tx-mono mt-1.5 truncate text-sm text-[var(--text-secondary)]">
          {publicEndpoint || "未配置隧道"}
        </p>
      </div>
    {/if}
  </div>
</article>
