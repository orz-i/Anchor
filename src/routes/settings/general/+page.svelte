<script lang="ts">
  import { onMount } from "svelte";
  import { message } from "$lib/platform/dialog";
  import {
    getMcpGateway,
    getMcpGatewayStatus,
    getProxy,
    getWindowsServiceStatus,
    installWindowsService,
    restartWindowsService,
    setMcpGateway,
    setMcpGatewayRoute,
    setProxy,
    startWindowsService,
    stopWindowsService,
    syncWindowsServicePlan,
    type McpGatewayConfigDto,
    type McpGatewayStatusDto,
    type ProxyConfigDto,
    type WindowsScmServiceStatusDto,
    uninstallWindowsService,
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
  let gatewayRouteBusy = $state<Record<string, boolean>>({});
  let gatewayRefreshing = false;
  let gatewayEventFault = $state("");
  let gatewayLog = $state<GatewayLogChunk | null>(null);
  let gatewayLogError = $state("");
  let windowsService = $state<WindowsScmServiceStatusDto | null>(null);
  let windowsServiceBusy = $state(false);

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

  async function toggleGatewayRoute(workspace: WorkspaceProfile, enabled: boolean) {
    if (gatewayRouteBusy[workspace.id] || gatewayChanged || gatewaySaving) return;
    gatewayRouteBusy[workspace.id] = true;
    try {
      gatewayStatus = await setMcpGatewayRoute(workspace.id, enabled);
      await refreshGatewayLog();
      await message(
        enabled ? `${workspace.name} 已加入 Gateway routes。` : `${workspace.name} 已移出 Gateway routes。`,
        { title: "Gateway route", kind: "info" },
      );
    } catch (e) {
      await message(String(e), { title: "Gateway route 操作失败", kind: "error" });
    } finally {
      gatewayRouteBusy[workspace.id] = false;
    }
  }

  async function runWindowsServiceAction(
    action: () => Promise<WindowsScmServiceStatusDto>,
    success: string,
  ) {
    windowsServiceBusy = true;
    try {
      windowsService = await action();
      await message(success, { title: "Windows Service", kind: "info" });
    } catch (e) {
      await message(String(e), { title: "Windows Service 操作失败", kind: "error" });
    } finally {
      windowsServiceBusy = false;
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
      const [nextProxy, nextGateway, nextGatewayStatus, nextWorkspaces, nextWindowsService] =
        await Promise.all([
          getProxy(),
          getMcpGateway(),
          getMcpGatewayStatus(),
          listWorkspaces(),
          getWindowsServiceStatus(),
        ]);
      proxy = nextProxy;
      gateway = nextGateway;
      gatewayStatus = nextGatewayStatus;
      workspaces = nextWorkspaces;
      windowsService = nextWindowsService;
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
              Gateway {gatewayStatus && !gatewayStatus.daemonSupported ? "Server" : "daemon"} 日志{gatewayLog.truncated ? " · 已截断" : ""}
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

    {#if windowsService?.supported}
      <div class="tx-card p-4">
        <div class="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h3 class="text-sm font-semibold">Windows 后台服务</h3>
            <p class="mt-1 max-w-3xl text-xs leading-5 text-[var(--text-muted)]">
              使用 Windows SCM 在开机时监督 Workspace daemon 与 Gateway daemon。安装后 GUI 只负责修改运行计划和控制面，不需要保持桌面窗口常驻。
            </p>
          </div>
          <span class="rounded-full border border-[var(--border)] px-2.5 py-1 text-xs">
            {windowsService.installed
              ? `${windowsService.state}${windowsService.autoStart ? " · 自动启动" : ""}`
              : "未安装"}
          </span>
        </div>

        <div class="mt-4 grid gap-3 text-xs text-[var(--text-muted)] md:grid-cols-2">
          <div class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] p-3">
            <p><span class="font-medium text-[var(--text)]">Service</span> · {windowsService.serviceName}</p>
            <p class="mt-1 break-all">配置域：{windowsService.configDir}</p>
            <p class="mt-1 break-all">启动计划：{windowsService.planPath}</p>
          </div>
          <div class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] p-3">
            <p>
              开机 Workspace · {windowsService.plan.workspaces.length}；Gateway routes · {windowsService.plan.gatewayWorkspaceIds.length}
            </p>
            <p class="mt-1">
              配置所有者 · {windowsService.plan.ownerUsername || "未记录"}
            </p>
            <p class="mt-1">
              运行构建 · {windowsService.buildState === "current"
                ? `${windowsService.runtime?.buildIdentity.packageVersion ?? windowsService.currentBuild.packageVersion} · ${(windowsService.runtime?.buildIdentity.gitSha ?? windowsService.currentBuild.gitSha).slice(0, 8)}`
                : windowsService.buildState === "different"
                  ? `待更新 · ${windowsService.runtime?.buildIdentity.gitSha.slice(0, 8) ?? "unknown"} → ${windowsService.currentBuild.gitSha.slice(0, 8)}`
                  : windowsService.buildState === "unknown"
                    ? "未知（旧版 Service 未发布 build identity）"
                    : windowsService.buildState === "stopped"
                      ? "Service 已停止"
                      : "未安装"}
              {windowsService.processId ? ` · PID ${windowsService.processId}` : ""}
            </p>
            <p class="mt-1">
              安装、卸载和服务启停会触发标准 Windows UAC，仅提升该次 SCM 操作。
            </p>
          </div>
        </div>

        <div class="mt-4 flex flex-wrap justify-end gap-2">
          <button
            type="button"
            class="rounded-md border border-[var(--border)] px-3 py-1.5 text-sm disabled:opacity-50"
            disabled={windowsServiceBusy}
            onclick={() => void runWindowsServiceAction(syncWindowsServicePlan, "已将当前 daemon/Gateway 运行态同步为开机启动计划。")}
          >同步当前运行态</button>
          {#if !windowsService.installed}
            <button
              type="button"
              class="rounded-md bg-[var(--primary)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
              disabled={windowsServiceBusy}
              onclick={() => void runWindowsServiceAction(installWindowsService, "Windows SCM Service 已安装并设置为自动启动。")}
            >安装并自动启动</button>
          {:else}
            <button
              type="button"
              class="rounded-md border border-[var(--border)] px-3 py-1.5 text-sm disabled:opacity-50"
              disabled={windowsServiceBusy}
              onclick={() => void runWindowsServiceAction(installWindowsService, "Windows SCM Service 已更新到当前构建并完成重启。")}
            >更新服务版本</button>
            {#if windowsService.state === "running"}
              <button
                type="button"
                class="rounded-md border border-[var(--border)] px-3 py-1.5 text-sm disabled:opacity-50"
                disabled={windowsServiceBusy}
                onclick={() => void runWindowsServiceAction(stopWindowsService, "Windows SCM Service 已停止。")}
              >停止</button>
            {:else}
              <button
                type="button"
                class="rounded-md border border-[var(--border)] px-3 py-1.5 text-sm disabled:opacity-50"
                disabled={windowsServiceBusy}
                onclick={() => void runWindowsServiceAction(startWindowsService, "Windows SCM Service 已启动。")}
              >启动</button>
            {/if}
            <button
              type="button"
              class="rounded-md border border-[var(--border)] px-3 py-1.5 text-sm disabled:opacity-50"
              disabled={windowsServiceBusy}
              onclick={() => void runWindowsServiceAction(restartWindowsService, "Windows SCM Service 已重启。")}
            >重启</button>
            <button
              type="button"
              class="rounded-md border border-red-500/40 px-3 py-1.5 text-sm text-red-600 disabled:opacity-50"
              disabled={windowsServiceBusy}
              onclick={() => void runWindowsServiceAction(uninstallWindowsService, "Windows SCM Service 已卸载；启动计划配置仍保留，可再次安装。")}
            >卸载</button>
          {/if}
        </div>
      </div>
    {/if}

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
              ? `${gatewayStatus.daemonSupported ? "daemon" : "Server"} 运行中 · ${gatewayStatus.routeCount} 条路由${gatewayStatus.pid ? ` · PID ${gatewayStatus.pid}` : ""}`
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
                <div class="flex items-start justify-between gap-3 rounded-md border border-[var(--border)] bg-[var(--page-bg)] p-2">
                  <div class="grid min-w-0 gap-0.5 text-xs">
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
                  <button
                    type="button"
                    class="shrink-0 rounded-md border border-[var(--border)] px-2.5 py-1 text-xs disabled:opacity-50"
                    disabled={gatewayChanged || gatewaySaving || gatewayRouteBusy[workspace.id]}
                    onclick={() => void toggleGatewayRoute(workspace, !gatewayRouteActive(workspace.id))}
                    title={gatewayChanged ? "请先保存 Gateway 配置" : undefined}
                  >
                    {gatewayRouteBusy[workspace.id]
                      ? "处理中…"
                      : gatewayRouteActive(workspace.id)
                        ? "停止路由"
                        : "启动路由"}
                  </button>
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

        {#if gatewayStatus && !gatewayStatus.daemonSupported}
          <p class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] p-2 text-xs text-[var(--text-muted)]">
            当前平台不支持独立 Gateway daemon。配置可以保存，但后台 Gateway 需要在支持 Windows/Linux daemon 的环境中运行。
          </p>
        {:else if gatewayStatus?.state === "configured"}
          <p class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] p-2 text-xs text-[var(--text-muted)]">
            Gateway 配置已保存。后台运行由独立 Gateway daemon 控制。
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
