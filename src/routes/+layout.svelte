<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import { open } from "$lib/platform/dialog";
  import AppShell from "$lib/components/AppShell.svelte";
  import ToastHost from "$lib/components/ToastHost.svelte";
  import WorkspaceNavItem from "$lib/components/WorkspaceNavItem.svelte";
  import {
    createWorkspace,
    getControlPlaneEvents,
    getControlPlaneStatus,
    listWorkspaces,
  } from "$lib/api/workspaces";
  import { getLastWorkspaceId } from "$lib/api/settings";
  import { actionsRuntimeStates, mcpRuntimeStates, workspaces } from "$lib/stores/app";
  import { showToast } from "$lib/stores/toast";
  import type { ControlPlaneEventCursor, ControlPlaneStatus, RuntimeState } from "$lib/types";

  let { children } = $props();

  function applyControlPlaneStatus(status: ControlPlaneStatus) {
    const mcpStates: Record<string, RuntimeState> = {};
    const actionsStates: Record<string, RuntimeState> = {};
    for (const item of status.workspaces) {
      mcpStates[item.id] = item.mcpState;
      actionsStates[item.id] = item.actionsState;
    }
    mcpRuntimeStates.set(mcpStates);
    actionsRuntimeStates.set(actionsStates);
  }

  async function refreshControlPlaneStates() {
    applyControlPlaneStatus(await getControlPlaneStatus());
  }

  async function refreshWorkspaces() {
    const [items, status] = await Promise.all([
      listWorkspaces(),
      getControlPlaneStatus(),
    ]);
    workspaces.set(items);
    applyControlPlaneStatus(status);
  }

  function delay(ms: number): Promise<void> {
    return new Promise((resolve) => window.setTimeout(resolve, ms));
  }

  async function observeControlPlane(isCancelled: () => boolean) {
    let cursor: ControlPlaneEventCursor | null = null;
    let lastFault = "";
    while (!isCancelled()) {
      try {
        const batch = await getControlPlaneEvents(cursor, 15_000);
        if (isCancelled()) return;
        cursor = batch.nextCursor;
        lastFault = "";
        if (batch.events.length > 0 || batch.resetSources.length > 0) {
          await refreshControlPlaneStates();
        } else {
          // Empty long-poll timeouts only check for externally added/removed profiles;
          // runtime state remains event-driven and never falls back to N×service polling.
          const items = await listWorkspaces();
          if (isCancelled()) return;
          const currentIds = new Set($workspaces.map((item) => item.id));
          if (
            items.length !== currentIds.size ||
            items.some((item) => !currentIds.has(item.id))
          ) {
            workspaces.set(items);
            await refreshControlPlaneStates();
          }
        }
      } catch (error) {
        if (isCancelled()) return;
        const detail = String(error);
        if (detail !== lastFault) {
          lastFault = detail;
          showToast(detail, {
            title: "控制面事件异常",
            kind: "error",
            duration: 8000,
          });
        }
        // Protocol/remote errors stay explicit. Retry the event endpoint itself;
        // do not downgrade to legacy per-service status polling.
        await delay(3_000);
      }
    }
  }

  async function addWorkspace() {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected || Array.isArray(selected)) return;
      const profile = await createWorkspace(selected);
      await refreshWorkspaces();
      goto(`/workspace/${profile.id}`);
    } catch (error) {
      showToast(String(error), {
        title: "添加工作区失败",
        kind: "error",
        duration: 8000,
      });
    }
  }

  function openWorkspace(id: string) {
    goto(`/workspace/${id}`);
  }

  function openFrpSettings() {
    goto("/settings/frp");
  }

  function openSoftwareSettings() {
    goto("/settings/software");
  }

  function openGeneralSettings() {
    goto("/settings/general");
  }

  function openKeysSettings() {
    goto("/settings/keys");
  }

  onMount(() => {
    let cancelled = false;
    void (async () => {
      try {
        await refreshWorkspaces();
        if (cancelled) return;
        const path = $page.url.pathname;
        if (path === "/") {
          const lastId = await getLastWorkspaceId();
          if (lastId && $workspaces.some((item) => item.id === lastId)) {
            goto(`/workspace/${lastId}`);
          } else if ($workspaces.length > 0) {
            goto(`/workspace/${$workspaces[0].id}`);
          }
        }
      } catch (error) {
        showToast(String(error), {
          title: "加载控制面失败",
          kind: "error",
          duration: 8000,
        });
      }
      if (!cancelled) void observeControlPlane(() => cancelled);
    })();
    return () => {
      cancelled = true;
    };
  });
</script>

<AppShell onAddWorkspace={addWorkspace}>
  {#snippet settingsNav()}
    <button
      type="button"
      class="tx-settings-link {$page.url.pathname === '/settings/general' ? 'active' : ''}"
      onclick={openGeneralSettings}
    >
      通用
    </button>
    <button
      type="button"
      class="tx-settings-link {$page.url.pathname === '/settings/keys' ? 'active' : ''}"
      onclick={openKeysSettings}
    >
      共享密钥
    </button>
    <button
      type="button"
      class="tx-settings-link {$page.url.pathname === '/settings/frp' ? 'active' : ''}"
      onclick={openFrpSettings}
    >
      FRP 配置
    </button>
    <button
      type="button"
      class="tx-settings-link {$page.url.pathname === '/settings/software' ? 'active' : ''}"
      onclick={openSoftwareSettings}
    >
      软件管理
    </button>
  {/snippet}
  {#snippet sidebar()}
    <div class="space-y-1">
      {#each $workspaces as workspace (workspace.id)}
        <WorkspaceNavItem
          workspace={workspace}
          active={$page.url.pathname === `/workspace/${workspace.id}`}
          mcpState={$mcpRuntimeStates[workspace.id] ?? "stopped"}
          actionsState={$actionsRuntimeStates[workspace.id] ?? "stopped"}
          onClick={() => openWorkspace(workspace.id)}
        />
      {/each}
    </div>
  {/snippet}

  {#snippet children()}
    {@render children()}
  {/snippet}
</AppShell>

<ToastHost />
