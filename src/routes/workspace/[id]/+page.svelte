<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import ActionsAuthForm from "$lib/components/ActionsAuthForm.svelte";
  import ActionsPolicyForm, {
    type ActionsPolicyDraft,
  } from "$lib/components/ActionsPolicyForm.svelte";
  import AuthConfigForm from "$lib/components/AuthConfigForm.svelte";
  import HealthPanel from "$lib/components/HealthPanel.svelte";
  import LogViewer from "$lib/components/LogViewer.svelte";
  import McpProxyConfigForm from "$lib/components/McpProxyConfigForm.svelte";
  import SkillServiceConfigForm from "$lib/components/SkillServiceConfigForm.svelte";
  import RuntimePolicyForm, {
    type RuntimePolicyDraft,
  } from "$lib/components/RuntimePolicyForm.svelte";
  import ChatGptSessionPrompt from "$lib/components/ChatGptSessionPrompt.svelte";
  import CanvsPanel from "$lib/components/CanvsPanel.svelte";
  import ServicePanel from "$lib/components/ServicePanel.svelte";
  import GptQuickCopy from "$lib/components/GptQuickCopy.svelte";
  import StatusOrb from "$lib/components/StatusOrb.svelte";
  import Tabs from "$lib/components/Tabs.svelte";
  import TunnelConfigForm, {
    type TunnelFormConfig,
    type SaveTunnelOptions,
  } from "$lib/components/TunnelConfigForm.svelte";
  import WorkspaceMetaForm from "$lib/components/WorkspaceMetaForm.svelte";
  import {
    deleteWorkspace,
    getActionsRuntimeStatus,
    getRuntimeStatus,
    getWorkspaceControlEvents,
    listWorkspaces,
    startActionsRuntime,
    startRuntime,
    restartRuntime,
    restartActionsRuntime,
    stopActionsRuntime,
    stopRuntime,
    updateWorkspace,
  } from "$lib/api/workspaces";
  import { listFrpProfiles, setLastWorkspace, type FrpProfileDto } from "$lib/api/settings";
  import { confirm } from "@tauri-apps/plugin-dialog";
  import { restartTunnel, stopTunnel } from "$lib/api/tunnel";
  import { runServiceToggle, notifyStartFailure } from "$lib/runtime/service";
  import { showToast } from "$lib/stores/toast";
  import { reloadServiceAfterConfigSave } from "$lib/runtime/restart-hint";
  import { actionsRuntimeStates, mcpRuntimeStates, workspaces } from "$lib/stores/app";
  import {
    actionsConfig,
    actionsLocalEndpoint,
    actionsOAuthAuthorizeUrl,
    actionsOAuthTokenUrl,
    actionsOpenApiUrl,
    actionsPrivacyUrl,
    frpPublicUrl,
    mcpLocalEndpoint,
    type AuthConfig,
    type ActionsAuthDraft,
    type ControlEventCursor,
    type McpActivity,
    type RuntimeRecovery,
    type RuntimeStatus,
    type RuntimeState,
    type WorkspaceProfile,
  } from "$lib/types";
  import type { CanvsTaskStatus } from "$lib/api/canvs";

  type ServiceTab = "mcp" | "actions" | "canvs";
  type RuntimeService = "mcp" | "actions";
  type SubTab = "config" | "logs" | "health";
  type BackendConnectionState = "connected" | "checking" | "recovering" | "offline";

  const INITIAL_CONNECT_TIMEOUT_MS = 12_000;

  const EMPTY_RECOVERY: RuntimeRecovery = {
    enabled: false,
    attempt: 0,
    maxAttempts: 5,
    retryInMs: null,
    recoveredCount: 0,
    lastError: "",
  };

  let profile = $state<WorkspaceProfile | null>(null);
  let mcpStatus = $state<RuntimeState>("stopped");
  let actionsStatus = $state<RuntimeState>("stopped");
  let mcpStatusMessage = $state("");
  let actionsStatusMessage = $state("");
  let mcpRecovery = $state<RuntimeRecovery>({ ...EMPTY_RECOVERY });
  let mcpActivity = $state<McpActivity | null>(null);
  const windowsServerMode = $derived(
    mcpStatusMessage.includes("Windows GUI Server 模式") ||
      actionsStatusMessage.includes("Windows GUI Server 模式"),
  );
  let actionsRecovery = $state<RuntimeRecovery>({ ...EMPTY_RECOVERY });
  let mcpBusy = $state(false);
  let actionsBusy = $state(false);
  let mcpLocal = $state("");
  let mcpPublic = $state("");
  let actionsLocal = $state("");
  let actionsPublic = $state("");
  let frpProfiles = $state<FrpProfileDto[]>([]);
  let backendConnection = $state<BackendConnectionState>("checking");
  let backendFailures = $state(0);
  let lastBackendSuccess = $state<number | null>(null);
  let statusPolling = $state(false);
  let statusPollTimer: number | null = null;
  let daemonEventCursor: ControlEventCursor | null = null;
  let daemonEventGeneration = 0;
  let daemonEventMode: "unknown" | "events" | "polling" | "fault" = "unknown";

  let activeService = $state<ServiceTab>("mcp");
  let canvsTaskStatus = $state<CanvsTaskStatus | null>(null);
  let mcpSubTab = $state<SubTab>("config");
  let actionsSubTab = $state<SubTab>("config");
  let loadGeneration = 0;

  const subTabs = [
    { value: "config", label: "配置" },
    { value: "logs", label: "日志" },
    { value: "health", label: "健康" },
  ];

  const workspaceId = $derived($page.params.id);
  const actions = $derived(profile ? actionsConfig(profile) : null);

  const mcpTunnelForm = $derived<TunnelFormConfig>({
    type: profile?.tunnel.type ?? "none",
    public_url: profile?.tunnel.public_url ?? "",
    frp_server: profile?.tunnel.frp_server ?? "",
    frp_subdomain: profile?.tunnel.frp_subdomain ?? "",
    frp_profile_id: profile?.tunnel.frp_profile_id ?? "",
    frp_server_port: profile?.tunnel.frp_server_port ?? 7000,
    frp_proxy_type: profile?.tunnel.frp_proxy_type ?? "http",
    frp_cert_path: profile?.tunnel.frp_cert_path ?? "",
    frp_key_path: profile?.tunnel.frp_key_path ?? "",
    cloudflare_mode: profile?.tunnel.cloudflare_mode ?? "named",
    use_proxy: profile?.tunnel.use_proxy ?? true,
  });

  const actionsTunnelForm = $derived<TunnelFormConfig>({
    type: actions?.tunnel_type ?? "none",
    public_url: actions?.public_url ?? "",
    frp_server: actions?.frp_server ?? "",
    frp_subdomain: actions?.frp_subdomain ?? "",
    frp_profile_id: actions?.frp_profile_id ?? "",
    frp_server_port: actions?.frp_server_port ?? 7000,
    frp_proxy_type: actions?.frp_proxy_type ?? "http",
    frp_cert_path: actions?.frp_cert_path ?? "",
    frp_key_path: actions?.frp_key_path ?? "",
    cloudflare_mode: actions?.cloudflare_mode ?? "named",
    use_proxy: actions?.use_proxy ?? true,
  });

  function stateLabel(state: RuntimeState): string {
    switch (state) {
      case "running":
        return "运行中";
      case "starting":
        return "启动中";
      case "recovering":
        return "恢复中";
      case "stopping":
        return "停止中";
      case "error":
        return "错误";
      default:
        return "已停止";
    }
  }

  async function reloadConfiguredService(service: RuntimeService): Promise<void> {
    const id = workspaceId;
    if (!id) return;
    const isMcp = service === "mcp";
    const running = (isMcp ? mcpStatus : actionsStatus) === "running";
    const runtime = await reloadServiceAfterConfigSave(
      running,
      isMcp ? "MCP 服务" : "Actions 服务",
      () => (isMcp ? restartRuntime(id) : restartActionsRuntime(id)),
    );
    if (!runtime || id !== workspaceId) return;
    if (isMcp) applyMcpRuntime(runtime, id);
    else applyActionsRuntime(runtime, id);
  }

  function canvsStateLabel(status: CanvsTaskStatus | null): string {
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
      case "completed_unverified":
        return "已完成";
      case "rolled_back":
        return "已回滚";
      default:
        return "当前任务";
    }
  }

  function canvsOrbState(status: CanvsTaskStatus | null): RuntimeState {
    switch (status) {
      case "active":
      case "completed":
        return "running";
      case "verifying":
        return "recovering";
      case "failed":
        return "error";
      default:
        return "stopped";
    }
  }

  async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
    let timer: number | null = null;
    try {
      return await Promise.race([
        promise,
        new Promise<T>((_, reject) => {
          timer = window.setTimeout(() => reject(new Error(message)), timeoutMs);
        }),
      ]);
    } finally {
      if (timer !== null) window.clearTimeout(timer);
    }
  }

  function lastSyncLabel(): string {
    if (lastBackendSuccess === null) return "尚未成功同步";
    return `上次同步 ${new Date(lastBackendSuccess).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    })}`;
  }

  function applyMcpRuntime(runtime: RuntimeStatus, id = workspaceId) {
    if (!id || id !== workspaceId) return;
    const previous = mcpStatus;
    const previousActivity = mcpActivity?.state;
    mcpStatus = runtime.state;
    mcpStatusMessage = runtime.localMessage ?? "";
    mcpRecovery = runtime.recovery ?? { ...EMPTY_RECOVERY };
    mcpActivity = runtime.activity ?? null;
    mcpLocal = runtime.localEndpoint;
    mcpPublic = runtime.publicEndpoint;
    mcpRuntimeStates.update((current) => ({ ...current, [id]: runtime.state }));
    if (previous === "recovering" && runtime.state === "running") {
      showToast("MCP 连接已自动恢复", { title: "连接已恢复", kind: "success" });
    }
    if (previousActivity !== "suspected_stalled" && mcpActivity?.state === "suspected_stalled") {
      const seconds = Math.max(1, Math.floor((mcpActivity.oldestInFlightMs ?? 0) / 1_000));
      showToast(`最早的 MCP 调用已持续 ${seconds} 秒，已超过正常请求窗口，请检查连接或工具执行状态`, {
        title: "MCP 调用疑似异常",
        kind: "warning",
        duration: 10_000,
      });
    } else if (
      previousActivity === "suspected_stalled" &&
      mcpActivity &&
      mcpActivity.state !== "suspected_stalled"
    ) {
      showToast("MCP 调用已结束或恢复活动", {
        title: "上游调用已恢复",
        kind: "success",
      });
    }
  }

  function applyActionsRuntime(runtime: RuntimeStatus, id = workspaceId) {
    if (!id || id !== workspaceId) return;
    const previous = actionsStatus;
    actionsStatus = runtime.state;
    actionsStatusMessage = runtime.localMessage ?? "";
    actionsRecovery = runtime.recovery ?? { ...EMPTY_RECOVERY };
    actionsLocal = runtime.localEndpoint;
    actionsPublic = runtime.publicEndpoint;
    actionsRuntimeStates.update((current) => ({ ...current, [id]: runtime.state }));
    if (previous === "recovering" && runtime.state === "running") {
      showToast("Actions 连接已自动恢复", { title: "连接已恢复", kind: "success" });
    }
  }

  function nextStatusPollDelay(): number {
    if (backendFailures === 0) return 5_000;
    return Math.min(15_000, 1_000 * 2 ** Math.min(backendFailures - 1, 4));
  }

  function scheduleStatusPoll(id: string, delay = nextStatusPollDelay()) {
    if (daemonEventMode !== "polling") return;
    if (statusPollTimer !== null) window.clearTimeout(statusPollTimer);
    statusPollTimer = window.setTimeout(() => {
      statusPollTimer = null;
      if (document.hidden) return;
      if (profile) {
        void pollRuntimeStatus(id);
      } else {
        void initializeWorkspace(id);
      }
    }, delay);
  }

  async function pollRuntimeStatus(id = workspaceId) {
    if (!id || id !== workspaceId || statusPolling || document.hidden) return;
    statusPolling = true;
    if (backendFailures > 0) backendConnection = "recovering";
    try {
      const [mcpResult, actionsResult] = await Promise.allSettled([
        getRuntimeStatus(id),
        getActionsRuntimeStatus(id),
      ]);
      if (id !== workspaceId) return;
      let succeeded = false;
      if (mcpResult.status === "fulfilled") {
        applyMcpRuntime(mcpResult.value, id);
        succeeded = true;
      }
      if (actionsResult.status === "fulfilled") {
        applyActionsRuntime(actionsResult.value, id);
        succeeded = true;
      }
      if (succeeded) {
        const wasDisconnected = backendFailures > 0;
        backendFailures = 0;
        backendConnection = "connected";
        lastBackendSuccess = Date.now();
        if (wasDisconnected) {
          showToast("后台连接已恢复，状态已同步", {
            title: "应用已重新连接",
            kind: "success",
          });
        }
      } else {
        backendFailures += 1;
        backendConnection = backendFailures >= 3 ? "offline" : "recovering";
      }
    } finally {
      statusPolling = false;
      if (id === workspaceId && daemonEventMode === "polling") startDaemonEventMonitor(id);
    }
  }

  function startDaemonEventMonitor(id: string) {
    const generation = ++daemonEventGeneration;
    daemonEventCursor = null;
    daemonEventMode = "unknown";
    if (statusPollTimer !== null) {
      window.clearTimeout(statusPollTimer);
      statusPollTimer = null;
    }
    void monitorDaemonEvents(id, generation);
  }

  async function monitorDaemonEvents(id: string, generation: number) {
    while (generation === daemonEventGeneration && id === workspaceId) {
      if (document.hidden) {
        await new Promise((resolve) => window.setTimeout(resolve, 500));
        continue;
      }
      try {
        const batch = await getWorkspaceControlEvents(id, daemonEventCursor, 15_000);
        if (generation !== daemonEventGeneration || id !== workspaceId) return;
        if (batch === null) {
          daemonEventMode = "polling";
          scheduleStatusPoll(id, nextStatusPollDelay());
          return;
        }
        daemonEventMode = "events";
        daemonEventCursor = batch.nextCursor;
        if (batch.reset || batch.events.length > 0) {
          await pollRuntimeStatus(id);
        }
      } catch (error) {
        if (generation !== daemonEventGeneration || id !== workspaceId) return;
        daemonEventMode = "fault";
        backendFailures += 1;
        backendConnection = backendFailures >= 3 ? "offline" : "recovering";
        showToast(error instanceof Error ? error.message : String(error), {
          title: "daemon 事件通道异常",
          kind: "error",
          duration: 8000,
        });
        return;
      }
    }
  }

  async function initializeWorkspace(id: string) {
    if (statusPolling || id !== workspaceId) return;
    statusPolling = true;
    backendConnection = "checking";
    try {
      const loaded = await withTimeout(
        load(id),
        INITIAL_CONNECT_TIMEOUT_MS,
        "连接应用后台超时，正在自动重试",
      );
      if (!loaded || id !== workspaceId) return;
      backendFailures = 0;
      backendConnection = "connected";
      lastBackendSuccess = Date.now();
    } catch (error) {
      if (id !== workspaceId) return;
      // Invalidate any late result from the timed-out load so it cannot overwrite
      // the next retry or a newly selected workspace.
      loadGeneration += 1;
      const previousFailures = backendFailures;
      backendFailures += 1;
      daemonEventMode = "polling";
      backendConnection = backendFailures >= 3 ? "offline" : "recovering";
      if (previousFailures === 0 || backendFailures === 3) {
        showToast(error instanceof Error ? error.message : String(error), {
          title: backendFailures >= 3 ? "后台连接已离线" : "后台连接暂不可用",
          kind: backendFailures >= 3 ? "error" : "warning",
          duration: 6000,
        });
      }
    } finally {
      statusPolling = false;
      if (id === workspaceId) {
        if (profile) startDaemonEventMonitor(id);
        else scheduleStatusPoll(id, nextStatusPollDelay());
      }
    }
  }

  function retryBackendNow() {
    const id = workspaceId;
    if (!id) return;
    if (daemonEventMode === "fault" && profile) {
      startDaemonEventMonitor(id);
      return;
    }
    if (profile) {
      void pollRuntimeStatus(id);
    } else {
      void initializeWorkspace(id);
    }
  }

  async function load(id = workspaceId): Promise<boolean> {
    if (!id) return false;
    const generation = ++loadGeneration;
    const items = await listWorkspaces();
    if (generation !== loadGeneration || id !== workspaceId) return false;
    workspaces.set(items);
    frpProfiles = await listFrpProfiles();
    if (generation !== loadGeneration || id !== workspaceId) return false;
    const nextProfile = items.find((item) => item.id === id) ?? null;
    if (generation !== loadGeneration || id !== workspaceId) return false;
    profile = nextProfile;
    if (nextProfile) {
      void setLastWorkspace(nextProfile.id).catch(() => {
        // Non-critical preference write. Runtime/config loading must continue.
      });
    }
    if (generation !== loadGeneration || id !== workspaceId) return false;
    if (!nextProfile) {
      await goto("/");
      return false;
    }

    const [mcpRuntime, actionsRuntime] = await Promise.all([
      getRuntimeStatus(id),
      getActionsRuntimeStatus(id),
    ]);
    if (generation !== loadGeneration || id !== workspaceId) return false;
    applyMcpRuntime(mcpRuntime, id);
    applyActionsRuntime(actionsRuntime, id);
    return true;
  }

  async function refreshProfile(id = workspaceId): Promise<WorkspaceProfile | null> {
    if (!id) return null;
    const items = await listWorkspaces();
    if (id !== workspaceId) return null;
    workspaces.set(items);
    const nextProfile = items.find((item) => item.id === id) ?? null;
    profile = nextProfile;
    return nextProfile;
  }

  function tunnelConfigured(type: string | undefined): boolean {
    return type === "cloudflare" || type === "frp";
  }

  async function afterServiceStart(
    service: RuntimeService,
    runtime: { state: RuntimeState; publicEndpoint: string },
    id: string,
  ) {
    const nextProfile = await refreshProfile(id);
    if (id !== workspaceId) return;
    const tunnelType =
      service === "mcp"
        ? nextProfile?.tunnel.type
        : nextProfile
          ? actionsConfig(nextProfile).tunnel_type
          : undefined;
    if (runtime.state === "running" && tunnelConfigured(tunnelType) && !runtime.publicEndpoint) {
      showToast(
        "本地服务已启动，但隧道未能自动连接。请检查代理设置与隧道配置，或查看日志。",
        { title: "隧道未连接", kind: "warning", duration: 8000 },
      );
    }
  }

  async function toggleService(service: RuntimeService) {
    const id = workspaceId;
    const isMcp = service === "mcp";
    if (!id || (isMcp ? mcpBusy : actionsBusy)) return;
    const label = isMcp ? "MCP" : "Actions";
    const wasRunning = (isMcp ? mcpStatus : actionsStatus) === "running";
    if (isMcp) mcpBusy = true;
    else actionsBusy = true;
    try {
      const runtime = await runServiceToggle(
        wasRunning,
        () => (isMcp ? startRuntime(id) : startActionsRuntime(id)),
        () => (isMcp ? stopRuntime(id) : stopActionsRuntime(id)),
        label,
      );
      if (runtime && id === workspaceId) {
        if (isMcp) applyMcpRuntime(runtime, id);
        else applyActionsRuntime(runtime, id);
        if (!wasRunning) {
          if (runtime.state === "running") {
            await afterServiceStart(service, runtime, id);
          } else {
            notifyStartFailure(label, runtime);
          }
        }
        startDaemonEventMonitor(id);
      }
    } finally {
      if (isMcp) mcpBusy = false;
      else actionsBusy = false;
    }
  }

  async function toggleMcp() {
    await toggleService("mcp");
  }

  async function toggleActions() {
    await toggleService("actions");
  }

  async function saveMcpPort(port: number) {
    if (!profile || profile.runtime.local_port === port) return;
    const next: WorkspaceProfile = {
      ...profile,
      runtime: { ...profile.runtime, local_port: port },
    };
    await updateWorkspace(next);
    profile = next;
    mcpLocal = mcpLocalEndpoint(port);
    await reloadConfiguredService("mcp");
    await load();
  }

  async function saveActionsPort(port: number) {
    if (!profile) return;
    const current = actionsConfig(profile);
    if (current.local_port === port) return;
    const next: WorkspaceProfile = {
      ...profile,
      actions: { ...current, local_port: port },
    };
    await updateWorkspace(next);
    profile = next;
    actionsLocal = actionsLocalEndpoint(port);
    await reloadConfiguredService("actions");
    await load();
  }

  function publicEndpointFromTunnel(config: TunnelFormConfig, suffix: string): string {
    const base = frpPublicUrl(
      config.type,
      config.frp_subdomain,
      config.frp_server,
      config.frp_profile_id,
      frpProfiles,
      config.public_url,
    );
    if (base) {
      return `${base.replace(/\/$/, "")}${suffix}`;
    }
    return "";
  }

  function canvsWebUrl(mcpEndpoint: string): string {
    const endpoint = mcpEndpoint.trim().replace(/\/$/, "");
    if (!endpoint) return "";
    return `${endpoint.replace(/\/mcp$/, "")}/canvs`;
  }

  async function restartTunnelIfConfigured(
    targetWorkspaceId: string,
    config: TunnelFormConfig,
    service: "mcp" | "actions",
  ) {
    if (config.type === "none") {
      await stopTunnel(targetWorkspaceId, service);
      return;
    }
    const status = await restartTunnel(targetWorkspaceId, service);
    if (workspaceId !== targetWorkspaceId) return;
    if (status.publicUrl) {
      if (service === "mcp") {
        mcpPublic = `${status.publicUrl.replace(/\/$/, "")}/mcp`;
      } else {
        actionsPublic = `${status.publicUrl.replace(/\/$/, "")}/openapi.json`;
      }
    }
  }

  async function saveMcpTunnel(config: TunnelFormConfig, options?: SaveTunnelOptions) {
    if (!profile) return;
    const targetWorkspaceId = workspaceId;
    if (!targetWorkspaceId) return;
    const next: WorkspaceProfile = {
      ...profile,
      tunnel: {
        ...profile.tunnel,
        type: config.type,
        public_url: config.public_url,
        frp_server: config.frp_server,
        frp_subdomain: config.frp_subdomain,
        frp_profile_id: config.frp_profile_id,
        frp_server_port: config.frp_server_port,
        frp_proxy_type: config.frp_proxy_type,
        frp_cert_path: config.frp_cert_path,
        frp_key_path: config.frp_key_path,
        cloudflare_mode: config.cloudflare_mode,
        use_proxy: config.use_proxy,
      },
    };
    await updateWorkspace(next);
    if (!options?.skipTunnelRestart) {
      await restartTunnelIfConfigured(targetWorkspaceId, config, "mcp");
    }
    if (workspaceId !== targetWorkspaceId) return;
    profile = next;
    mcpPublic = publicEndpointFromTunnel(config, "/mcp");
    if (!options?.skipTunnelRestart && !options?.skipServicePrompt) {
      await load();
      if (workspaceId !== targetWorkspaceId) return;
    }
  }

  async function saveActionsTunnel(config: TunnelFormConfig, options?: SaveTunnelOptions) {
    if (!profile) return;
    const targetWorkspaceId = workspaceId;
    if (!targetWorkspaceId) return;
    const current = actionsConfig(profile);
    const next: WorkspaceProfile = {
      ...profile,
      actions: {
        ...current,
        tunnel_type: config.type,
        public_url: config.public_url,
        frp_server: config.frp_server,
        frp_subdomain: config.frp_subdomain,
        frp_profile_id: config.frp_profile_id,
        frp_server_port: config.frp_server_port,
        frp_proxy_type: config.frp_proxy_type,
        frp_cert_path: config.frp_cert_path,
        frp_key_path: config.frp_key_path,
        cloudflare_mode: config.cloudflare_mode,
        use_proxy: config.use_proxy,
      },
    };
    await updateWorkspace(next);
    if (!options?.skipTunnelRestart) {
      await restartTunnelIfConfigured(targetWorkspaceId, config, "actions");
    }
    if (workspaceId !== targetWorkspaceId) return;
    profile = next;
    actionsPublic = publicEndpointFromTunnel(config, "/openapi.json");
    if (!options?.skipTunnelRestart && !options?.skipServicePrompt) {
      await load();
      if (workspaceId !== targetWorkspaceId) return;
    }
  }

  async function saveMcpPolicy(draft: RuntimePolicyDraft) {
    if (!profile) return;
    const next: WorkspaceProfile = {
      ...profile,
      runtime: {
        ...profile.runtime,
        tool_profile: draft.toolProfile,
        permission_mode: draft.permissionMode,
        allowed_commands: draft.allowedCommands,
        workspace_local_entries: draft.workspaceLocalEntries,
        workspace_script_extensions: draft.workspaceScriptExtensions,
        external_paid_commands_enabled: draft.externalPaidCommandsEnabled,
        external_paid_max_runs_per_day: draft.externalPaidMaxRunsPerDay,
        external_paid_max_duration_seconds: draft.externalPaidMaxDurationSeconds,
      },
    };
    await updateWorkspace(next);
    profile = next;
    await reloadConfiguredService("mcp");
    await load();
  }

  async function saveSkillService(config: { enabled: boolean; roots: string }) {
    if (!profile) return;
    const next: WorkspaceProfile = {
      ...profile,
      runtime: {
        ...profile.runtime,
        skill_service_enabled: config.enabled,
        skill_roots: config.roots,
      },
    };
    await updateWorkspace(next);
    profile = next;
    await reloadConfiguredService("mcp");
    await load();
  }

  async function saveMcpProxyConfig(config: string) {
    if (!profile) return;
    const next: WorkspaceProfile = {
      ...profile,
      runtime: {
        ...profile.runtime,
        mcp_config: config,
      },
    };
    await updateWorkspace(next);
    profile = next;
    await reloadConfiguredService("mcp");
    await load();
  }

  async function saveActionsPolicy(draft: ActionsPolicyDraft) {
    if (!profile) return;
    const current = actionsConfig(profile);
    const next: WorkspaceProfile = {
      ...profile,
      actions: {
        ...current,
        allowed_commands: draft.allowedCommands,
        max_patch_bytes: draft.maxPatchBytes,
        permission_mode: draft.permissionMode,
      },
    };
    await updateWorkspace(next);
    profile = next;
    await reloadConfiguredService("actions");
    await load();
  }

  async function saveMcpAuth(
    auth: AuthConfig,
    options: { callbackPolicyOnly: boolean },
  ) {
    if (!profile || !workspaceId) return;
    const next: WorkspaceProfile = { ...profile, auth };
    await updateWorkspace(next);
    profile = next;
    if (mcpStatus === "running" && !options.callbackPolicyOnly) {
      await reloadConfiguredService("mcp");
    } else if (mcpStatus === "running" && options.callbackPolicyOnly) {
      showToast("OAuth Callback 信任策略已热更新，当前授权流程不会中断", { kind: "success" });
    }
  }

  async function saveActionsAuth(
    draft: ActionsAuthDraft,
    options: { callbackPolicyOnly: boolean },
  ) {
    if (!profile || !workspaceId) return;
    const current = actionsConfig(profile);
    const next: WorkspaceProfile = {
      ...profile,
      actions: {
        ...current,
        auth_type: draft.authType,
        oauth_client_id: draft.oauthClientId || current.oauth_client_id,
        oauth_redirect_uris: draft.oauthRedirectUris,
        oauth_redirect_hosts: draft.oauthRedirectHosts,
        oauth_scopes: draft.oauthScopes,
        use_shared_secrets: draft.useSharedSecrets,
      },
    };
    await updateWorkspace(next);
    profile = next;
    if (actionsStatus === "running" && !options.callbackPolicyOnly) {
      await reloadConfiguredService("actions");
    } else if (actionsStatus === "running" && options.callbackPolicyOnly) {
      showToast("Actions OAuth Callback 信任策略已热更新，当前授权流程不会中断", { kind: "success" });
    }
  }

  async function saveWorkspaceName(name: string) {
    if (!profile || profile.name === name) return;
    const next: WorkspaceProfile = { ...profile, name };
    await updateWorkspace(next);
    profile = next;
    workspaces.update((items) =>
      items.map((item) => (item.id === next.id ? { ...item, name: next.name } : item)),
    );
    await reloadConfiguredService("mcp");
    await reloadConfiguredService("actions");
  }

  async function saveWorkspacePath(path: string) {
    if (!profile || profile.path === path) return;
    const next: WorkspaceProfile = { ...profile, path };
    await updateWorkspace(next);
    profile = next;
    showToast("工作区目录已更新", { kind: "success" });
    await reloadConfiguredService("mcp");
    await reloadConfiguredService("actions");
    await load();
  }

  async function removeWorkspace() {
    if (!profile || !workspaceId) return;
    const confirmed = await confirm(`确定删除工作区「${profile.name}」？此操作不可撤销。`, {
      title: "删除工作区",
      kind: "warning",
      okLabel: "删除",
      cancelLabel: "取消",
    });
    if (!confirmed) return;
    await deleteWorkspace(workspaceId);
    workspaces.update((items) => items.filter((item) => item.id !== workspaceId));
    goto("/");
  }

  $effect(() => {
    const id = workspaceId;
    if (!id) return;
    profile = null;
    daemonEventGeneration += 1;
    daemonEventCursor = null;
    daemonEventMode = "polling";
    backendFailures = 0;
    backendConnection = "checking";
    // initializeWorkspace reads and writes reactive connection state before its
    // first await. Running it in a microtask keeps those reads out of this
    // effect's dependency graph; otherwise statusPolling retriggers this effect,
    // clears profile, invalidates the load generation, and loops forever.
    queueMicrotask(() => void initializeWorkspace(id));

    const handleOnline = () => retryBackendNow();
    const handleVisibility = () => {
      if (!document.hidden) retryBackendNow();
    };
    window.addEventListener("online", handleOnline);
    document.addEventListener("visibilitychange", handleVisibility);

    return () => {
      loadGeneration += 1;
      daemonEventGeneration += 1;
      daemonEventCursor = null;
      if (statusPollTimer !== null) {
        window.clearTimeout(statusPollTimer);
        statusPollTimer = null;
      }
      window.removeEventListener("online", handleOnline);
      document.removeEventListener("visibilitychange", handleVisibility);
    };
  });
</script>

{#if profile && actions}
  <section class="page-scroll">
    <header class="page-header">
      <div class="flex items-start justify-between gap-4">
        <div>
          <p class="page-kicker">工作区</p>
          <h2 class="page-title">{profile.name}</h2>
        </div>
        <button
          type="button"
          class="tx-btn-ghost text-[var(--danger)]"
          onclick={() => void removeWorkspace()}
        >
          删除工作区
        </button>
      </div>

      {#if windowsServerMode}
        <div class="mt-4 rounded-md border border-[var(--border)] bg-[var(--primary-soft)] p-3 text-xs leading-5 text-[var(--text-secondary)]">
          <strong>Windows GUI Server 模式已自动启用。</strong>
          当前版本尚未实现 Windows 后台 daemon/Named Pipe server，因此无需寻找额外的模式开关；MCP、Actions 与隧道由当前 Anchor 桌面进程统一管理。关闭 Anchor 桌面应用会同时停止这些 Server 服务。
        </div>
      {/if}

      <div class="mt-4">
        <WorkspaceMetaForm
          name={profile.name}
          path={profile.path}
          onSave={saveWorkspaceName}
          onUpdatePath={saveWorkspacePath}
        />
      </div>

      <div class="mt-4">
        <ChatGptSessionPrompt />
      </div>

      {#if backendConnection === "recovering" || backendConnection === "offline"}
        <div
          class="tx-alert mt-4"
          class:tx-alert--warning={backendConnection === "recovering"}
          class:tx-alert--error={backendConnection === "offline"}
          role="status"
        >
          <div class="flex flex-wrap items-center justify-between gap-3">
            <div>
              <p class="font-medium">
                {backendConnection === "offline" ? "后台连接暂不可用" : "正在恢复后台连接"}
              </p>
              <p class="mt-1 text-xs opacity-80">
                已连续重试 {backendFailures} 次 · {lastSyncLabel()}。当前页面数据会保留，连接恢复后自动同步。
              </p>
            </div>
            <button
              type="button"
              class="tx-btn-ghost shrink-0"
              disabled={statusPolling}
              onclick={retryBackendNow}
            >
              {statusPolling ? "连接中…" : "立即重试"}
            </button>
          </div>
        </div>
      {/if}

      <div class="mt-4 flex flex-wrap items-center gap-2">
        <button
          type="button"
          class="tx-status-pill"
          class:active={activeService === "mcp"}
          onclick={() => (activeService = "mcp")}
        >
          <StatusOrb state={mcpStatus} />
          <span class="font-medium">MCP</span>
          <span class="text-[var(--text-muted)]">{stateLabel(mcpStatus)}</span>
        </button>
        <button
          type="button"
          class="tx-status-pill"
          class:active={activeService === "actions"}
          onclick={() => (activeService = "actions")}
        >
          <StatusOrb state={actionsStatus} />
          <span class="font-medium">Actions</span>
          <span class="text-[var(--text-muted)]">{stateLabel(actionsStatus)}</span>
        </button>
        <button
          type="button"
          class="tx-status-pill"
          class:active={activeService === "canvs"}
          onclick={() => (activeService = "canvs")}
        >
          <StatusOrb state={canvsOrbState(canvsTaskStatus)} />
          <span class="font-medium">Canvs</span>
          <span class="text-[var(--text-muted)]">{canvsStateLabel(canvsTaskStatus)}</span>
        </button>
      </div>
    </header>

    <div class="page-body">
      {#if activeService === "mcp"}
        <div class="mt-4 flex flex-col gap-3">
          <ServicePanel
            title="MCP"
            subtitle="Streamable HTTP · 工具运行时"
            status={mcpStatus}
            statusMessage={mcpStatusMessage}
            recovery={mcpRecovery}
            activity={mcpActivity}
            port={profile.runtime.local_port}
            portEditable={true}
            busy={mcpBusy}
            tunnelType={profile.tunnel.type}
            localEndpoint={mcpLocal || mcpLocalEndpoint(profile.runtime.local_port)}
            publicEndpoint={mcpPublic}
            publicLabel="公网 MCP"
            onToggle={toggleMcp}
            onPortChange={saveMcpPort}
          />
          <GptQuickCopy
            workspaceId={workspaceId!}
            service="mcp"
            {profile}
            publicMcpEndpoint={mcpPublic}
            {frpProfiles}
          />
        </div>

        <div class="mt-5">
          <Tabs
            items={subTabs}
            value={mcpSubTab}
            onchange={(v) => {
              mcpSubTab = v as SubTab;
            }}
          />
        </div>

        {#if mcpSubTab === "config"}
          <div class="tx-card mt-4 grid gap-6 p-5">
            <div>
              <p class="tx-section-label">隧道</p>
              <TunnelConfigForm
                workspaceId={workspaceId!}
                service="mcp"
                config={mcpTunnelForm}
                onSave={saveMcpTunnel}
              />
            </div>
            <div>
              <p class="tx-section-label">认证</p>
              <AuthConfigForm
                workspaceId={workspaceId!}
                auth={profile.auth}
                onSaveProfile={saveMcpAuth}
              />
            </div>
            <div>
              <p class="tx-section-label">策略</p>
              <RuntimePolicyForm
                toolProfile={profile.runtime.tool_profile}
                permissionMode={profile.runtime.permission_mode}
                allowedCommands={profile.runtime.allowed_commands ?? ""}
                workspaceLocalEntries={profile.runtime.workspace_local_entries ?? true}
                workspaceScriptExtensions={profile.runtime.workspace_script_extensions ?? ".exe,.bat,.cmd,.ps1"}
                externalPaidCommandsEnabled={profile.runtime.external_paid_commands_enabled ?? false}
                externalPaidMaxRunsPerDay={profile.runtime.external_paid_max_runs_per_day ?? 1}
                externalPaidMaxDurationSeconds={profile.runtime.external_paid_max_duration_seconds ?? 1800}
                onSave={saveMcpPolicy}
              />
            </div>
            <div>
              <p class="tx-section-label">Agent Skills</p>
              <SkillServiceConfigForm
                workspaceId={workspaceId!}
                enabled={profile.runtime.skill_service_enabled ?? true}
                roots={profile.runtime.skill_roots ?? ".agents/skills\n.codex/skills\nskills"}
                onSave={saveSkillService}
              />
            </div>
            <div>
              <p class="tx-section-label">MCP 工具聚合</p>
              <McpProxyConfigForm
                config={profile.runtime.mcp_config ?? ""}
                onSave={saveMcpProxyConfig}
              />
            </div>
          </div>
        {:else if mcpSubTab === "logs"}
          <div class="mt-4">
            <LogViewer workspaceId={workspaceId!} service="mcp" />
          </div>
        {:else}
          <div class="mt-4">
            <HealthPanel workspaceId={workspaceId!} />
          </div>
        {/if}
      {:else if activeService === "actions"}
        <div class="mt-4 flex flex-col gap-3">
          <ServicePanel
            title="Actions"
            subtitle="OpenAPI 网关 · ChatGPT Actions"
            status={actionsStatus}
            statusMessage={actionsStatusMessage}
            recovery={actionsRecovery}
            port={actions.local_port}
            portEditable={true}
            busy={actionsBusy}
            tunnelType={actions.tunnel_type}
            localEndpoint={actionsLocal || actionsLocalEndpoint(actions.local_port)}
            publicEndpoint={actionsPublic || actionsOpenApiUrl(profile, frpProfiles)}
            publicLabel="OpenAPI"
            onToggle={toggleActions}
            onPortChange={saveActionsPort}
          />
          <GptQuickCopy
            workspaceId={workspaceId!}
            service="actions"
            {profile}
            {frpProfiles}
          />
        </div>

        <div class="mt-5">
          <Tabs
            items={subTabs}
            value={actionsSubTab}
            onchange={(v) => {
              actionsSubTab = v as SubTab;
            }}
          />
        </div>

        {#if actionsSubTab === "config"}
          <div class="tx-card mt-4 grid gap-6 p-5">
            <div>
              <p class="tx-section-label">隧道</p>
              <TunnelConfigForm
                workspaceId={workspaceId!}
                service="actions"
                config={actionsTunnelForm}
                onSave={saveActionsTunnel}
              />
            </div>
            <div>
              <p class="tx-section-label">认证</p>
              <ActionsAuthForm
                workspaceId={workspaceId!}
                authType={actions.auth_type}
                oauthClientId={actions.oauth_client_id ?? ""}
                oauthRedirectUris={actions.oauth_redirect_uris ?? ""}
                oauthRedirectHosts={actions.oauth_redirect_hosts ?? ""}
                oauthScopes={actions.oauth_scopes ?? ""}
                openapiUrl={actionsOpenApiUrl(profile, frpProfiles)}
                privacyUrl={actionsPrivacyUrl(profile, frpProfiles)}
                oauthAuthorizeUrl={actionsOAuthAuthorizeUrl(profile, frpProfiles)}
                oauthTokenUrl={actionsOAuthTokenUrl(profile, frpProfiles)}
                useSharedSecrets={actions.use_shared_secrets ?? false}
                onSave={saveActionsAuth}
              />
            </div>
            <div>
              <p class="tx-section-label">策略</p>
              <ActionsPolicyForm
                allowedCommands={actions.allowed_commands ?? ""}
                maxPatchBytes={actions.max_patch_bytes ?? 200_000}
                permissionMode={actions.permission_mode}
                onSave={saveActionsPolicy}
              />
            </div>
          </div>
        {:else if actionsSubTab === "logs"}
          <div class="mt-4">
            <LogViewer workspaceId={workspaceId!} service="actions" />
          </div>
        {:else}
          <div class="mt-4">
            <HealthPanel workspaceId={workspaceId!} />
          </div>
        {/if}
      {:else}
        <div class="mt-4">
          <CanvsPanel
            workspaceId={workspaceId!}
            localUrl={canvsWebUrl(mcpLocal)}
            publicUrl={canvsWebUrl(mcpPublic)}
            onTaskStatusChange={(status) => {
              canvsTaskStatus = status;
            }}
          />
        </div>
      {/if}
    </div>

    <footer class="border-t border-[var(--border)] px-8 py-4 text-xs text-[var(--text-muted)]">
      MCP 默认端口 28766，Actions 默认 8787；Canvs 网页与 MCP 共用工作区隔离的公网入口。
    </footer>
  </section>
{:else}
  <section class="page-scroll grid place-items-center p-8">
    <div class="tx-card w-full max-w-xl p-6 text-center">
      <div class="mx-auto flex w-fit items-center gap-2">
        <StatusOrb state={backendConnection === "offline" ? "error" : "recovering"} />
        <h2 class="text-base font-semibold">
          {backendConnection === "offline" ? "无法连接应用后台" : "正在连接工作区"}
        </h2>
      </div>
      <p class="mt-3 text-sm leading-6 text-[var(--text-muted)]">
        {backendConnection === "offline"
          ? "后台暂时没有响应。应用会继续自动重试，也可以立即重新连接。"
          : "正在读取配置和运行状态，短暂断联会自动恢复。"}
      </p>
      <button
        type="button"
        class="tx-btn-primary mt-5"
        disabled={statusPolling}
        onclick={retryBackendNow}
      >
        {statusPolling ? "连接中…" : "立即重试"}
      </button>
    </div>
  </section>
{/if}
