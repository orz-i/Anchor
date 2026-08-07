<script lang="ts">
  import { onMount } from "svelte";
  import { message } from "@tauri-apps/plugin-dialog";
  import {
    getMcpGateway,
    getMcpGatewayStatus,
    getProxy,
    setMcpGateway,
    setProxy,
    type McpGatewayConfigDto,
    type McpGatewayStatusDto,
    type ProxyConfigDto,
  } from "$lib/api/settings";
  import {
    getGatewayControlEvents,
    listWorkspaces,
    readGatewayLogs,
  } from "$lib/api/workspaces";
  import type { GatewayEventCursor, GatewayLogChunk, WorkspaceProfile } from "$lib/types";

  let proxy = $state<ProxyConfigDto>({ mode: "none", url: "" });
  let changed = $state(false);
  let saving = $state(false);
  let workspaces = $state<WorkspaceProfile[]>([]);
  let gateway = $state<McpGatewayConfigDto>({
    urlModelVersion: 2,
    enabled: false,
    localPort: 28765,
    ownerWorkspaceId: "",
    publicUrl: "",
    observedPublicUrl: "",
    observedOwnerWorkspaceId: "",
    observedTunnelSignature: "",
  });
  let gatewayStatus = $state<McpGatewayStatusDto | null>(null);
  let gatewayChanged = $state(false);
  let gatewaySaving = $state(false);
  let gatewayRefreshing = false;
  let gatewayEventFault = $state("");
  let gatewayLog = $state<GatewayLogChunk | null>(null);
  let gatewayLogError = $state("");

  async function refreshGatewayRuntimeStatus() {
    if (gatewayRefreshing) return;
    gatewayRefreshing = true;
    try {
      const [nextGateway, nextStatus, nextWorkspaces] = await Promise.all([
        getMcpGateway(),
        getMcpGatewayStatus(),
        listWorkspaces(),
      ]);
      gatewayStatus = nextStatus;
      workspaces = nextWorkspaces;
      if (!gatewayChanged && !gatewaySaving) {
        gateway = nextGateway;
      }
    } finally {
      gatewayRefreshing = false;
    }
  }

  async function refreshGatewayLog() {
    try {
      gatewayLog = await readGatewayLogs(80);
      gatewayLogError = "";
    } catch (error) {
      gatewayLogError = String(error);
    }
  }

  function delay(ms: number): Promise<void> {
    return new Promise((resolve) => window.setTimeout(resolve, ms));
  }

  async function observeGateway(isCancelled: () => boolean) {
    let cursor: GatewayEventCursor | null = null;
    while (!isCancelled()) {
      try {
        const batch = await getGatewayControlEvents(cursor, 15_000);
        if (isCancelled()) return;
        if (batch === null) {
          // Endpoint unavailable is the only read-only fallback: refresh the
          // offline/configured status, then probe the event endpoint again.
          cursor = null;
          gatewayEventFault = "";
          await refreshGatewayRuntimeStatus();
          await refreshGatewayLog();
          await delay(2_000);
          continue;
        }
        cursor = batch.nextCursor;
        gatewayEventFault = "";
        if (batch.events.length > 0 || batch.reset) {
          await refreshGatewayRuntimeStatus();
          await refreshGatewayLog();
        }
      } catch (error) {
        if (isCancelled()) return;
        gatewayEventFault = String(error);
        // Protocol/remote errors stay explicit. Retry the same event endpoint;
        // do not silently downgrade to status polling.
        await delay(3_000);
      }
    }
  }

  async function refresh() {
    try {
      const [nextProxy, nextGateway, nextGatewayStatus, nextWorkspaces] =
        await Promise.all([
          getProxy(),
          getMcpGateway(),
          getMcpGatewayStatus(),
          listWorkspaces(),
        ]);
      proxy = nextProxy;
      gateway = nextGateway;
      gatewayStatus = nextGatewayStatus;
      workspaces = nextWorkspaces;
      changed = false;
      gatewayChanged = false;
      await refreshGatewayLog();
    } catch (e) {
      await message(String(e), { title: "加载失败", kind: "error" });
    }
  }

  async function save() {
    saving = true;
    try {
      await setProxy(proxy);
      changed = false;
      await message("代理设置已保存。", { title: "已保存", kind: "info" });
    } catch (e) {
      await message(String(e), { title: "保存失败", kind: "error" });
    } finally {
      saving = false;
    }
  }

  function handleChange() {
    changed = true;
  }

  function handleGatewayChange() {
    gatewayChanged = true;
  }

  async function saveGateway() {
    gatewaySaving = true;
    try {
      gatewayStatus = await setMcpGateway(gateway);
      gateway = await getMcpGateway();
      gatewayChanged = false;
      await refreshGatewayLog();
      await message("MCP Gateway 设置已保存。", {
        title: "已保存",
        kind: "info",
      });
    } catch (e) {
      await message(String(e), { title: "保存失败", kind: "error" });
    } finally {
      gatewaySaving = false;
    }
  }

  function gatewayBaseUrl(): string {
    return (
      gateway.observedPublicUrl.trim().replace(/\/$/, "") ||
      gateway.publicUrl.trim().replace(/\/$/, "") ||
      `http://127.0.0.1:${gateway.localPort}`
    );
  }

  function gatewayWorkspaceUrl(workspaceId: string): string {
    return `${gatewayBaseUrl()}/w/${workspaceId}/mcp`;
  }

  function gatewayRouteActive(workspaceId: string): boolean {
    return gatewayStatus?.routeWorkspaceIds.includes(workspaceId) ?? false;
  }

  onMount(() => {
    let cancelled = false;
    void (async () => {
      await refresh();
      if (!cancelled) void observeGateway(() => cancelled);
    })();
    return () => {
      cancelled = true;
    };
  });
</script>

<section class="page-scroll">
  <header class="page-header">
    <p class="page-kicker">全局设置</p>
    <h2 class="page-title">通用</h2>
    <p class="mt-2 max-w-2xl text-sm text-[var(--text-muted)]">
      配置全局网络代理。此代理将应用于 Cloudflare 隧道连接，不影响软件下载代理。
    </p>
  </header>

  <div class="page-body flex flex-col gap-6">
    <div class="tx-card p-4">
      <h3 class="text-sm font-semibold">网络代理</h3>
      <form
        class="mt-4 grid gap-3"
        onsubmit={(e) => { e.preventDefault(); void save(); }}
      >
        <label class="grid gap-1">
          <span class="text-xs text-[var(--text-muted)]">代理模式</span>
          <select
            class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
            bind:value={proxy.mode}
            onchange={handleChange}
          >
            <option value="none">无代理</option>
            <option value="system">系统代理</option>
            <option value="manual">手动代理地址</option>
          </select>
        </label>

        {#if proxy.mode === "manual"}
          <label class="grid gap-1">
            <span class="text-xs text-[var(--text-muted)]">代理地址</span>
            <input
              type="text"
              class="tx-input tx-mono"
              placeholder="http://127.0.0.1:7890"
              bind:value={proxy.url}
              oninput={handleChange}
            />
            <span class="text-xs text-[var(--text-muted)]">
              支持 HTTP/HTTPS/SOCKS 代理，如 http://127.0.0.1:7890
            </span>
          </label>
        {/if}

        {#if gatewayEventFault}
          <p class="rounded-md border border-red-500/30 bg-red-500/5 p-2 text-xs text-red-600">
            Gateway 事件控制异常：{gatewayEventFault}
          </p>
        {/if}

        {#if gatewayLogError}
          <p class="rounded-md border border-red-500/30 bg-red-500/5 p-2 text-xs text-red-600">
            Gateway 日志读取异常：{gatewayLogError}
          </p>
        {:else if gatewayLog?.exists}
          <details class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] p-3">
            <summary class="cursor-pointer text-xs font-medium">
              Gateway daemon 日志{gatewayLog.truncated ? " · 已截断" : ""}
            </summary>
            <pre class="mt-2 max-h-56 overflow-auto whitespace-pre-wrap break-all text-xs">{gatewayLog.content || "暂无新日志"}</pre>
          </details>
        {/if}

        <div class="flex justify-end pt-1">
          <button
            type="submit"
            class="rounded-md bg-[var(--primary)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
            disabled={!changed || saving}
          >
            {saving ? "保存中…" : "保存设置"}
          </button>
        </div>
      </form>
    </div>

    <div class="tx-card p-4">
      <div class="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h3 class="text-sm font-semibold">单一 MCP Gateway</h3>
          <p class="mt-1 max-w-3xl text-xs leading-5 text-[var(--text-muted)]">
            通过一个本地网关和一个公网隧道暴露多个工作区。每个工作区仍使用独立路径、OAuth resource、会话和工具上下文。
          </p>
        </div>
        {#if gatewayStatus}
          <span class="rounded-full border border-[var(--border)] px-2.5 py-1 text-xs">
            {gatewayStatus.state === "running"
              ? `运行中 · ${gatewayStatus.routeCount} 条路由${gatewayStatus.pid ? ` · PID ${gatewayStatus.pid}` : ""}`
              : gatewayStatus.state === "configured"
                ? gatewayStatus.daemonSupported
                  ? "已配置 · Gateway daemon 未启动"
                  : "已配置 · 当前平台不支持后台 daemon"
              : gatewayStatus.state === "error"
                ? "错误"
                : "已停止"}
          </span>
        {/if}
      </div>

      <form
        class="mt-4 grid gap-4"
        onsubmit={(e) => { e.preventDefault(); void saveGateway(); }}
      >
        <label class="flex items-start gap-2 rounded-md border border-[var(--border)] p-3">
          <input
            type="checkbox"
            class="mt-0.5"
            bind:checked={gateway.enabled}
            onchange={handleGatewayChange}
          />
          <span>
            <span class="block text-sm font-medium">启用单一 Gateway</span>
            <span class="mt-0.5 block text-xs text-[var(--text-muted)]">
              启用后，各工作区原有 MCP 隧道会停止，仅保留下面所选工作区的隧道配置作为 Gateway 出入口。
            </span>
          </span>
        </label>

        <div class="grid gap-3 md:grid-cols-2">
          <label class="grid gap-1">
            <span class="text-xs text-[var(--text-muted)]">Gateway 本地端口</span>
            <input
              type="number"
              min="1"
              max="65535"
              class="tx-input tx-mono"
              bind:value={gateway.localPort}
              oninput={handleGatewayChange}
            />
          </label>

          <label class="grid gap-1">
            <span class="text-xs text-[var(--text-muted)]">隧道所有者工作区</span>
            <select
              class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
              bind:value={gateway.ownerWorkspaceId}
              onchange={handleGatewayChange}
              disabled={!gateway.enabled}
            >
              <option value="">请选择工作区</option>
              {#each workspaces as workspace}
                <option value={workspace.id}>{workspace.name}</option>
              {/each}
            </select>
          </label>
        </div>

        <label class="grid gap-1">
          <span class="text-xs text-[var(--text-muted)]">Gateway 公网基础地址</span>
          <input
            type="url"
            class="tx-input tx-mono"
            placeholder="启动 Quick Tunnel 后自动保存；固定隧道可预先填写"
            bind:value={gateway.publicUrl}
            oninput={handleGatewayChange}
          />
          <span class="text-xs text-[var(--text-muted)]">
            不包含 <code>/w/&lt;workspace&gt;/mcp</code>。远程地址必须使用 HTTPS 且不能包含子路径。留空时由隧道运行结果决定。
          </span>
        </label>

        {#if gateway.observedPublicUrl}
          <div class="grid gap-1">
            <span class="text-xs text-[var(--text-muted)]">当前观测到的公网地址</span>
            <code class="break-all text-xs">{gateway.observedPublicUrl}</code>
          </div>
        {/if}

        {#if gateway.enabled}
          <div class="rounded-md border border-[var(--border)] bg-[var(--primary-soft)] p-3">
            <p class="text-xs font-medium">ChatGPT 工作区连接地址</p>
            <div class="mt-2 grid max-h-48 gap-2 overflow-auto">
              {#each workspaces as workspace}
                <div class="grid gap-0.5 text-xs">
                  <span class="flex flex-wrap items-center gap-2 text-[var(--text-muted)]">
                    <span>{workspace.name}</span>
                    <span>
                      {gatewayRouteActive(workspace.id)
                        ? "路由已注册"
                        : "未启动 · 当前返回 404"}
                    </span>
                  </span>
                  <code class="break-all">{gatewayWorkspaceUrl(workspace.id)}</code>
                </div>
              {/each}
            </div>
          </div>
        {/if}

        {#if gatewayStatus?.error}
          <p class="rounded-md border border-red-500/30 bg-red-500/5 p-2 text-xs text-red-600">
            {gatewayStatus.error}
          </p>
        {/if}

        {#if gatewayStatus?.state === "configured"}
          <p class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] p-2 text-xs text-[var(--text-muted)]">
            Gateway 配置已保存。桌面 GUI 不创建共享 listener 或隧道；后台运行请使用
            <code>anchor gateway start &lt;workspace ...&gt;</code>。<code>gateway serve</code>
            仅保留给前台调试或外部 supervisor。
          </p>
        {/if}

        <div class="flex justify-end pt-1">
          <button
            type="submit"
            class="rounded-md bg-[var(--primary)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
            disabled={!gatewayChanged || gatewaySaving}
          >
            {gatewaySaving ? "保存中…" : "保存 Gateway 设置"}
          </button>
        </div>
      </form>
    </div>
  </div>
</section>
