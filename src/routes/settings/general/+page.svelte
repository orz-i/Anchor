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
  import { listWorkspaces } from "$lib/api/workspaces";
  import type { WorkspaceProfile } from "$lib/types";

  let proxy = $state<ProxyConfigDto>({ mode: "none", url: "" });
  let changed = $state(false);
  let saving = $state(false);
  let workspaces = $state<WorkspaceProfile[]>([]);
  let gateway = $state<McpGatewayConfigDto>({
    enabled: false,
    localPort: 28765,
    ownerWorkspaceId: "",
    publicUrl: "",
  });
  let gatewayStatus = $state<McpGatewayStatusDto | null>(null);
  let gatewayChanged = $state(false);
  let gatewaySaving = $state(false);

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
      gateway.publicUrl.trim().replace(/\/$/, "") ||
      `http://127.0.0.1:${gateway.localPort}`
    );
  }

  function gatewayWorkspaceUrl(workspaceId: string): string {
    return `${gatewayBaseUrl()}/w/${workspaceId}/mcp`;
  }

  onMount(refresh);
</script>

<section class="page-scroll">
  <header class="page-header">
    <p class="page-kicker">全局设置</p>
    <h2 class="page-title">通用</h2>
    <p class="mt-2 max-w-2xl text-sm text-[var(--color-text-muted)]">
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
          <span class="text-xs text-[var(--color-text-muted)]">代理模式</span>
          <select
            class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm"
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
            <span class="text-xs text-[var(--color-text-muted)]">代理地址</span>
            <input
              type="text"
              class="tx-input tx-mono"
              placeholder="http://127.0.0.1:7890"
              bind:value={proxy.url}
              oninput={handleChange}
            />
            <span class="text-xs text-[var(--color-text-muted)]">
              支持 HTTP/HTTPS/SOCKS 代理，如 http://127.0.0.1:7890
            </span>
          </label>
        {/if}

        <div class="flex justify-end pt-1">
          <button
            type="submit"
            class="rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
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
          <p class="mt-1 max-w-3xl text-xs leading-5 text-[var(--color-text-muted)]">
            通过一个本地网关和一个公网隧道暴露多个工作区。每个工作区仍使用独立路径、OAuth resource、会话和工具上下文。
          </p>
        </div>
        {#if gatewayStatus}
          <span class="rounded-full border border-[var(--color-border)] px-2.5 py-1 text-xs">
            {gatewayStatus.state === "running"
              ? `运行中 · ${gatewayStatus.routeCount} 条路由`
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
        <label class="flex items-start gap-2 rounded-md border border-[var(--color-border)] p-3">
          <input
            type="checkbox"
            class="mt-0.5"
            bind:checked={gateway.enabled}
            onchange={handleGatewayChange}
          />
          <span>
            <span class="block text-sm font-medium">启用单一 Gateway</span>
            <span class="mt-0.5 block text-xs text-[var(--color-text-muted)]">
              启用后，各工作区原有 MCP 隧道会停止，仅保留下面所选工作区的隧道配置作为 Gateway 出入口。
            </span>
          </span>
        </label>

        <div class="grid gap-3 md:grid-cols-2">
          <label class="grid gap-1">
            <span class="text-xs text-[var(--color-text-muted)]">Gateway 本地端口</span>
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
            <span class="text-xs text-[var(--color-text-muted)]">隧道所有者工作区</span>
            <select
              class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm"
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
          <span class="text-xs text-[var(--color-text-muted)]">Gateway 公网基础地址</span>
          <input
            type="url"
            class="tx-input tx-mono"
            placeholder="启动 Quick Tunnel 后自动保存；固定隧道可预先填写"
            bind:value={gateway.publicUrl}
            oninput={handleGatewayChange}
          />
          <span class="text-xs text-[var(--color-text-muted)]">
            不包含 <code>/w/&lt;workspace&gt;/mcp</code>。留空时先使用本地 Gateway 地址；隧道成功后自动写入实际公网地址。
          </span>
        </label>

        {#if gateway.enabled}
          <div class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg-subtle)] p-3">
            <p class="text-xs font-medium">ChatGPT 工作区连接地址</p>
            <div class="mt-2 grid max-h-48 gap-2 overflow-auto">
              {#each workspaces as workspace}
                <div class="grid gap-0.5 text-xs">
                  <span class="text-[var(--color-text-muted)]">{workspace.name}</span>
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

        <div class="flex justify-end pt-1">
          <button
            type="submit"
            class="rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
            disabled={!gatewayChanged || gatewaySaving}
          >
            {gatewaySaving ? "保存中…" : "保存 Gateway 设置"}
          </button>
        </div>
      </form>
    </div>
  </div>
</section>
