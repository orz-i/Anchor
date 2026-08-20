import { useCallback, useEffect, useState } from "react";
import { Activity, Gauge, Network, Settings2, Trash2 } from "lucide-react";
import { useNavigate, useParams } from "react-router-dom";
import { toast } from "sonner";

import { ActionsAuthForm } from "@/components/admin/ActionsAuthForm";
import { ActionsPolicyForm, type ActionsPolicyDraft } from "@/components/admin/ActionsPolicyForm";
import { useAdmin } from "@/components/admin/AdminProvider";
import { CanvsPanel } from "@/components/admin/CanvsPanel";
import { ChatGptSessionPrompt } from "@/components/admin/ChatGptSessionPrompt";
import { GptQuickCopy } from "@/components/admin/GptQuickCopy";
import { HealthPanel } from "@/components/admin/HealthPanel";
import { LogViewer } from "@/components/admin/LogViewer";
import { McpAuthForm } from "@/components/admin/McpAuthForm";
import { McpProxyConfigForm } from "@/components/admin/McpProxyConfigForm";
import { PageLayout } from "@/components/admin/PageLayout";
import { RuntimePolicyForm, type RuntimePolicyDraft } from "@/components/admin/RuntimePolicyForm";
import { ServicePanel } from "@/components/admin/ServicePanel";
import { SkillServiceConfigForm } from "@/components/admin/SkillServiceConfigForm";
import { TunnelConfigForm, type SaveTunnelOptions, type TunnelFormConfig } from "@/components/admin/TunnelConfigForm";
import { WorkspaceMetaForm } from "@/components/admin/WorkspaceMetaForm";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { listFrpProfiles, setLastWorkspace, type FrpProfileDto } from "@/lib/api/settings";
import { restartTunnel, stopTunnel } from "@/lib/api/tunnel";
import {
  deleteWorkspace,
  getActionsRuntimeStatus,
  getRuntimeStatus,
  listWorkspaces,
  startActionsRuntime,
  startRuntime,
  stopActionsRuntime,
  stopRuntime,
  updateWorkspace,
} from "@/lib/api/workspaces";
import { notifyStartFailure, runServiceToggle } from "@/lib/runtime/service";
import type { ActionsAuthDraft, AuthConfig, McpActivity, RuntimeRecovery, RuntimeState, RuntimeStatus, WorkspaceProfile } from "@/lib/types";
import { actionsConfig, actionsLocalEndpoint, actionsOAuthAuthorizeUrl, actionsOAuthTokenUrl, actionsOpenApiUrl, actionsPrivacyUrl, frpPublicUrl, mcpLocalEndpoint } from "@/lib/types";

type ServiceTab = "mcp" | "actions" | "canvs";
type SubTab = "config" | "logs" | "health";

const EMPTY_RECOVERY: RuntimeRecovery = { enabled: false, attempt: 0, maxAttempts: 5, retryInMs: null, recoveredCount: 0, lastError: "" };

function tunnelForm(profile: WorkspaceProfile): TunnelFormConfig {
  return {
    type: profile.tunnel.type,
    public_url: profile.tunnel.public_url,
    frp_server: profile.tunnel.frp_server,
    frp_subdomain: profile.tunnel.frp_subdomain,
    frp_profile_id: profile.tunnel.frp_profile_id ?? "",
    frp_server_port: profile.tunnel.frp_server_port ?? 7000,
    frp_proxy_type: profile.tunnel.frp_proxy_type ?? "http",
    frp_cert_path: profile.tunnel.frp_cert_path ?? "",
    frp_key_path: profile.tunnel.frp_key_path ?? "",
    cloudflare_mode: profile.tunnel.cloudflare_mode,
    use_proxy: profile.tunnel.use_proxy ?? true,
  };
}

function actionsTunnelForm(profile: WorkspaceProfile): TunnelFormConfig {
  const actions = actionsConfig(profile);
  return {
    type: actions.tunnel_type,
    public_url: actions.public_url,
    frp_server: actions.frp_server,
    frp_subdomain: actions.frp_subdomain,
    frp_profile_id: actions.frp_profile_id ?? "",
    frp_server_port: actions.frp_server_port ?? 7000,
    frp_proxy_type: actions.frp_proxy_type ?? "http",
    frp_cert_path: actions.frp_cert_path ?? "",
    frp_key_path: actions.frp_key_path ?? "",
    cloudflare_mode: actions.cloudflare_mode,
    use_proxy: actions.use_proxy ?? true,
  };
}

function canvsWebUrl(endpoint: string): string {
  const value = endpoint.trim().replace(/\/$/, "");
  return value ? `${value.replace(/\/mcp$/, "")}/canvs` : "";
}

export function WorkspacePage() {
  const params = useParams();
  const navigate = useNavigate();
  const admin = useAdmin();
  const [profile, setProfile] = useState<WorkspaceProfile | null>(null);
  const [frpProfiles, setFrpProfiles] = useState<FrpProfileDto[]>([]);
  const [loading, setLoading] = useState(true);
  const [backendError, setBackendError] = useState("");
  const [activeService, setActiveService] = useState<ServiceTab>("mcp");
  const [mcpSubTab, setMcpSubTab] = useState<SubTab>("config");
  const [actionsSubTab, setActionsSubTab] = useState<SubTab>("config");
  const [mcpBusy, setMcpBusy] = useState(false);
  const [actionsBusy, setActionsBusy] = useState(false);
  const [mcpRuntime, setMcpRuntime] = useState<RuntimeStatus | null>(null);
  const [actionsRuntime, setActionsRuntime] = useState<RuntimeStatus | null>(null);

  const workspaceId = params.id || admin.workspaces[0]?.id || "";

  const refreshStatuses = useCallback(async (id: string) => {
    const [mcp, actions] = await Promise.all([getRuntimeStatus(id), getActionsRuntimeStatus(id)]);
    setMcpRuntime(mcp);
    setActionsRuntime(actions);
    admin.setMcpRuntimeState(id, mcp.state);
    admin.setActionsRuntimeState(id, actions.state);
  }, [admin]);

  const load = useCallback(async () => {
    if (!workspaceId) {
      setProfile(null);
      setLoading(false);
      return;
    }
    setLoading(true);
    setBackendError("");
    try {
      const [items, profiles] = await Promise.all([listWorkspaces(), listFrpProfiles()]);
      const next = items.find((item) => item.id === workspaceId) ?? null;
      setProfile(next);
      setFrpProfiles(profiles);
      if (next) {
        void setLastWorkspace(next.id).catch(() => undefined);
        await refreshStatuses(next.id);
      }
    } catch (error) {
      setBackendError(String(error));
    } finally {
      setLoading(false);
    }
  }, [refreshStatuses, workspaceId]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => {
    if (!workspaceId) return;
    const timer = window.setInterval(() => { if (!document.hidden) void refreshStatuses(workspaceId).catch((error) => setBackendError(String(error))); }, 5000);
    return () => window.clearInterval(timer);
  }, [refreshStatuses, workspaceId]);

  const mcpState: RuntimeState = mcpRuntime?.state ?? admin.mcpRuntimeStates[workspaceId] ?? "stopped";
  const actionsState: RuntimeState = actionsRuntime?.state ?? admin.actionsRuntimeStates[workspaceId] ?? "stopped";
  const actions = profile ? actionsConfig(profile) : null;
  const mcpLocal = mcpRuntime?.localEndpoint || (profile ? mcpLocalEndpoint(profile.runtime.local_port) : "");
  const actionsLocal = actionsRuntime?.localEndpoint || (actions ? actionsLocalEndpoint(actions.local_port) : "");
  const mcpPublic = mcpRuntime?.publicEndpoint || "";
  const actionsPublic = actionsRuntime?.publicEndpoint || "";

  const applyRuntime = (service: "mcp" | "actions", runtime: RuntimeStatus) => {
    if (service === "mcp") { setMcpRuntime(runtime); admin.setMcpRuntimeState(workspaceId, runtime.state); }
    else { setActionsRuntime(runtime); admin.setActionsRuntimeState(workspaceId, runtime.state); }
  };

  const toggleService = async (service: "mcp" | "actions") => {
    if (!workspaceId) return;
    const current = service === "mcp" ? mcpState : actionsState;
    const setBusy = service === "mcp" ? setMcpBusy : setActionsBusy;
    setBusy(true);
    try {
      const result = await runServiceToggle(current === "running", service === "mcp" ? () => startRuntime(workspaceId) : () => startActionsRuntime(workspaceId), service === "mcp" ? () => stopRuntime(workspaceId) : () => stopActionsRuntime(workspaceId), service === "mcp" ? "MCP" : "Actions");
      if (result) {
        applyRuntime(service, result);
        if (current !== "running" && result.state === "error") notifyStartFailure(service === "mcp" ? "MCP" : "Actions", result);
      }
    } finally { setBusy(false); }
  };

  const persist = async (next: WorkspaceProfile, reload = false) => {
    if (!profile) return;
    await updateWorkspace(next, profile);
    setProfile(next);
    admin.setWorkspaces((items) => items.map((item) => item.id === next.id ? next : item));
    if (reload) await load();
  };

  const publicEndpointFromTunnel = (config: TunnelFormConfig, suffix: string) => {
    const base = frpPublicUrl(config.type, config.frp_subdomain, config.frp_server, config.frp_profile_id, frpProfiles, config.public_url);
    return base ? `${base.replace(/\/$/, "")}${suffix}` : "";
  };

  const restartTunnelIfConfigured = async (service: "mcp" | "actions", config: TunnelFormConfig) => {
    if (config.type === "none") { await stopTunnel(workspaceId, service); return; }
    const status = await restartTunnel(workspaceId, service);
    if (service === "mcp" && status.publicUrl) setMcpRuntime((current) => current ? { ...current, publicEndpoint: `${status.publicUrl.replace(/\/$/, "")}/mcp` } : current);
    if (service === "actions" && status.publicUrl) setActionsRuntime((current) => current ? { ...current, publicEndpoint: `${status.publicUrl.replace(/\/$/, "")}/openapi.json` } : current);
  };

  const saveMcpTunnel = async (config: TunnelFormConfig, options?: SaveTunnelOptions) => {
    if (!profile) return;
    const next: WorkspaceProfile = { ...profile, tunnel: { ...profile.tunnel, type: config.type, public_url: config.public_url, frp_server: config.frp_server, frp_subdomain: config.frp_subdomain, frp_profile_id: config.frp_profile_id, frp_server_port: config.frp_server_port, frp_proxy_type: config.frp_proxy_type, frp_cert_path: config.frp_cert_path, frp_key_path: config.frp_key_path, cloudflare_mode: config.cloudflare_mode, use_proxy: config.use_proxy } };
    await persist(next);
    if (!options?.skipTunnelRestart) await restartTunnelIfConfigured("mcp", config);
    if (!options?.skipTunnelRestart && !options?.skipServicePrompt) await refreshStatuses(workspaceId);
    if (!mcpRuntime?.publicEndpoint) setMcpRuntime((current) => current ? { ...current, publicEndpoint: publicEndpointFromTunnel(config, "/mcp") } : current);
  };

  const saveActionsTunnel = async (config: TunnelFormConfig, options?: SaveTunnelOptions) => {
    if (!profile) return;
    const current = actionsConfig(profile);
    const next: WorkspaceProfile = { ...profile, actions: { ...current, tunnel_type: config.type, public_url: config.public_url, frp_server: config.frp_server, frp_subdomain: config.frp_subdomain, frp_profile_id: config.frp_profile_id, frp_server_port: config.frp_server_port, frp_proxy_type: config.frp_proxy_type, frp_cert_path: config.frp_cert_path, frp_key_path: config.frp_key_path, cloudflare_mode: config.cloudflare_mode, use_proxy: config.use_proxy } };
    await persist(next);
    if (!options?.skipTunnelRestart) await restartTunnelIfConfigured("actions", config);
    if (!options?.skipTunnelRestart && !options?.skipServicePrompt) await refreshStatuses(workspaceId);
    if (!actionsRuntime?.publicEndpoint) setActionsRuntime((runtime) => runtime ? { ...runtime, publicEndpoint: publicEndpointFromTunnel(config, "/openapi.json") } : runtime);
  };

  const removeWorkspace = async () => {
    if (!profile || !window.confirm(`确定删除工作区「${profile.name}」？不会删除磁盘目录。`)) return;
    await deleteWorkspace(profile.id);
    await admin.refreshWorkspaces();
    toast.success("工作区已删除");
    navigate("/workspace", { replace: true });
  };

  if (loading && !profile) return <div className="grid h-full place-items-center text-sm text-muted-foreground">正在加载工作区…</div>;
  if (!workspaceId || !profile) return <div className="grid h-full place-items-center p-8"><Empty><EmptyHeader><EmptyTitle>暂无工作区</EmptyTitle><EmptyDescription>从左侧添加一个工作区后即可配置 MCP、Actions 与 Canvs。</EmptyDescription></EmptyHeader></Empty></div>;

  return <PageLayout kicker="工作区" title={profile.name} description={profile.path} actions={<Button type="button" variant="destructive" onClick={() => void removeWorkspace()}><Trash2 data-icon="inline-start" />删除工作区</Button>}>
    <div className="grid gap-5">
      {backendError && <Alert variant="destructive"><AlertTitle>控制面连接异常</AlertTitle><AlertDescription>{backendError}</AlertDescription></Alert>}
      <Card><CardContent className="p-4"><WorkspaceMetaForm name={profile.name} path={profile.path} onSave={async (name) => { await persist({ ...profile, name }); toast.success("工作区名称已更新"); }} onUpdatePath={async (path) => { await persist({ ...profile, path }, true); toast.success("工作区目录已更新"); }} /></CardContent></Card>
      <ChatGptSessionPrompt />

      <Tabs value={activeService} onValueChange={(value) => setActiveService((value ?? "mcp") as ServiceTab)}>
        <TabsList variant="line" className="w-full justify-start border-b">
          <TabsTrigger value="mcp"><Network data-icon="inline-start" />MCP <Badge variant="outline" className="ml-1">{mcpState}</Badge></TabsTrigger>
          <TabsTrigger value="actions"><Settings2 data-icon="inline-start" />Actions <Badge variant="outline" className="ml-1">{actionsState}</Badge></TabsTrigger>
          <TabsTrigger value="canvs"><Gauge data-icon="inline-start" />Canvs</TabsTrigger>
        </TabsList>

        <TabsContent value="mcp" className="mt-4 grid gap-4">
          <div className="grid gap-4 xl:grid-cols-2">
            <ServicePanel title="MCP 服务" subtitle="ChatGPT Connector / MCP Server" status={mcpState} statusMessage={mcpRuntime?.localMessage ?? ""} recovery={mcpRuntime?.recovery ?? EMPTY_RECOVERY} activity={(mcpRuntime?.activity as McpActivity | null | undefined) ?? null} port={profile.runtime.local_port} portEditable busy={mcpBusy} tunnelType={profile.tunnel.type} localEndpoint={mcpLocal} publicEndpoint={mcpPublic} publicLabel="公网 MCP" onToggle={() => toggleService("mcp")} onPortChange={async (port) => { if (port === profile.runtime.local_port) return; await persist({ ...profile, runtime: { ...profile.runtime, local_port: port } }, true); }} />
            <GptQuickCopy workspaceId={workspaceId} service="mcp" profile={profile} publicMcpEndpoint={mcpPublic} frpProfiles={frpProfiles} />
          </div>
          <Tabs value={mcpSubTab} onValueChange={(value) => setMcpSubTab((value ?? "config") as SubTab)}><TabsList><TabsTrigger value="config">配置</TabsTrigger><TabsTrigger value="logs">日志</TabsTrigger><TabsTrigger value="health"><Activity data-icon="inline-start" />健康</TabsTrigger></TabsList>
            <TabsContent value="config" className="mt-4 grid gap-4 lg:grid-cols-2">
              <ConfigCard title="隧道" description="MCP 公网入口与隧道保活"><TunnelConfigForm workspaceId={workspaceId} service="mcp" config={tunnelForm(profile)} onSave={saveMcpTunnel} /></ConfigCard>
              <ConfigCard title="认证" description="OAuth、Bearer 与共享 Secret"><McpAuthForm workspaceId={workspaceId} auth={profile.auth} onSaveProfile={async (auth: AuthConfig, options) => { await persist({ ...profile, auth }); if (mcpState === "running" && options.callbackPolicyOnly) toast.success("OAuth Callback 信任策略已热更新"); }} /></ConfigCard>
              <ConfigCard title="运行策略" description="工具档位、Shell 与命令边界"><RuntimePolicyForm toolProfile={profile.runtime.tool_profile} permissionMode={profile.runtime.permission_mode} preferredShell={profile.runtime.preferred_shell ?? "auto"} allowedCommands={profile.runtime.allowed_commands ?? ""} workspaceLocalEntries={profile.runtime.workspace_local_entries ?? true} workspaceScriptExtensions={profile.runtime.workspace_script_extensions ?? ".exe,.bat,.cmd,.ps1"} externalPaidCommandsEnabled={profile.runtime.external_paid_commands_enabled ?? false} externalPaidMaxRunsPerDay={profile.runtime.external_paid_max_runs_per_day ?? 1} externalPaidMaxDurationSeconds={profile.runtime.external_paid_max_duration_seconds ?? 1800} onSave={async (draft: RuntimePolicyDraft) => persist({ ...profile, runtime: { ...profile.runtime, tool_profile: draft.toolProfile, permission_mode: draft.permissionMode, preferred_shell: draft.preferredShell, allowed_commands: draft.allowedCommands, workspace_local_entries: draft.workspaceLocalEntries, workspace_script_extensions: draft.workspaceScriptExtensions, external_paid_commands_enabled: draft.externalPaidCommandsEnabled, external_paid_max_runs_per_day: draft.externalPaidMaxRunsPerDay, external_paid_max_duration_seconds: draft.externalPaidMaxDurationSeconds } }, true)} /></ConfigCard>
              <ConfigCard title="Agent Skills" description="通过 MCP 暴露工作区 Skills"><SkillServiceConfigForm workspaceId={workspaceId} enabled={profile.runtime.skill_service_enabled ?? true} roots={profile.runtime.skill_roots ?? ".agents/skills\n.codex/skills\nskills"} onSave={async (config) => persist({ ...profile, runtime: { ...profile.runtime, skill_service_enabled: config.enabled, skill_roots: config.roots } }, true)} /></ConfigCard>
              <div className="lg:col-span-2"><ConfigCard title="下游 MCP 聚合" description="统一接入 stdio 与 Streamable HTTP MCP"><McpProxyConfigForm config={profile.runtime.mcp_config ?? ""} onSave={async (config) => persist({ ...profile, runtime: { ...profile.runtime, mcp_config: config } }, true)} /></ConfigCard></div>
            </TabsContent>
            <TabsContent value="logs" className="mt-4"><LogViewer workspaceId={workspaceId} service="mcp" /></TabsContent>
            <TabsContent value="health" className="mt-4"><HealthPanel workspaceId={workspaceId} /></TabsContent>
          </Tabs>
        </TabsContent>

        <TabsContent value="actions" className="mt-4 grid gap-4">
          {actions && <>
            <div className="grid gap-4 xl:grid-cols-2"><ServicePanel title="Actions 服务" subtitle="GPT Actions / OpenAPI Gateway" status={actionsState} statusMessage={actionsRuntime?.localMessage ?? ""} recovery={actionsRuntime?.recovery ?? EMPTY_RECOVERY} port={actions.local_port} portEditable busy={actionsBusy} tunnelType={actions.tunnel_type} localEndpoint={actionsLocal} publicEndpoint={actionsPublic} publicLabel="公网 OpenAPI" onToggle={() => toggleService("actions")} onPortChange={async (port) => persist({ ...profile, actions: { ...actions, local_port: port } }, true)} /><GptQuickCopy workspaceId={workspaceId} service="actions" profile={profile} frpProfiles={frpProfiles} /></div>
            <Tabs value={actionsSubTab} onValueChange={(value) => setActionsSubTab((value ?? "config") as SubTab)}><TabsList><TabsTrigger value="config">配置</TabsTrigger><TabsTrigger value="logs">日志</TabsTrigger><TabsTrigger value="health">健康</TabsTrigger></TabsList>
              <TabsContent value="config" className="mt-4 grid gap-4 lg:grid-cols-2"><ConfigCard title="隧道" description="Actions OpenAPI 公网入口"><TunnelConfigForm workspaceId={workspaceId} service="actions" config={actionsTunnelForm(profile)} onSave={saveActionsTunnel} /></ConfigCard><ConfigCard title="认证" description="API Key / OAuth"><ActionsAuthForm workspaceId={workspaceId} authType={actions.auth_type} oauthClientId={actions.oauth_client_id ?? ""} oauthRedirectUris={actions.oauth_redirect_uris ?? ""} oauthRedirectHosts={actions.oauth_redirect_hosts ?? ""} oauthScopes={actions.oauth_scopes ?? ""} openapiUrl={actionsOpenApiUrl(profile, frpProfiles)} privacyUrl={actionsPrivacyUrl(profile, frpProfiles)} oauthAuthorizeUrl={actionsOAuthAuthorizeUrl(profile, frpProfiles)} oauthTokenUrl={actionsOAuthTokenUrl(profile, frpProfiles)} useSharedSecrets={actions.use_shared_secrets} onSave={async (draft: ActionsAuthDraft, options) => { const current = actionsConfig(profile); await persist({ ...profile, actions: { ...current, auth_type: draft.authType, oauth_client_id: draft.oauthClientId || current.oauth_client_id, oauth_redirect_uris: draft.oauthRedirectUris, oauth_redirect_hosts: draft.oauthRedirectHosts, oauth_scopes: draft.oauthScopes, use_shared_secrets: draft.useSharedSecrets } }); if (actionsState === "running" && options.callbackPolicyOnly) toast.success("Actions OAuth Callback 信任策略已热更新"); }} /></ConfigCard><div className="lg:col-span-2"><ConfigCard title="Actions 策略" description="命令白名单与 Patch 限制"><ActionsPolicyForm allowedCommands={actions.allowed_commands ?? ""} maxPatchBytes={actions.max_patch_bytes ?? 200000} permissionMode={actions.permission_mode} onSave={async (draft: ActionsPolicyDraft) => persist({ ...profile, actions: { ...actions, allowed_commands: draft.allowedCommands, max_patch_bytes: draft.maxPatchBytes, permission_mode: draft.permissionMode } }, true)} /></ConfigCard></div></TabsContent>
              <TabsContent value="logs" className="mt-4"><LogViewer workspaceId={workspaceId} service="actions" /></TabsContent><TabsContent value="health" className="mt-4"><HealthPanel workspaceId={workspaceId} /></TabsContent>
            </Tabs>
          </>}
        </TabsContent>

        <TabsContent value="canvs" className="mt-4"><CanvsPanel workspaceId={workspaceId} localUrl={canvsWebUrl(mcpLocal)} publicUrl={canvsWebUrl(mcpPublic)} /></TabsContent>
      </Tabs>
    </div>
  </PageLayout>;
}

function ConfigCard({ title, description, children }: { title: string; description: string; children: React.ReactNode }) {
  return <Card><CardHeader><CardTitle>{title}</CardTitle><CardDescription>{description}</CardDescription></CardHeader><CardContent>{children}</CardContent></Card>;
}
