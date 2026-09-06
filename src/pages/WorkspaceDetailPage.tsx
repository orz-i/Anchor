import { useCallback, useEffect, useState } from "react";
import { Activity, ChevronLeft, Gauge, Network, Trash2 } from "lucide-react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { toast } from "sonner";

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
import { TunnelConfigForm, type TunnelFormConfig } from "@/components/admin/TunnelConfigForm";
import { WorkspaceMetaForm } from "@/components/admin/WorkspaceMetaForm";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { setLastWorkspace } from "@/lib/api/settings";
import {
  deleteWorkspace,
  getRuntimeStatus,
  listWorkspaces,
  startRuntime,
  stopRuntime,
  updateWorkspace,
} from "@/lib/api/workspaces";
import { notifyStartFailure, runServiceToggle } from "@/lib/runtime/service";
import type { AuthConfig, McpActivity, RuntimeRecovery, RuntimeState, RuntimeStatus, WorkspaceProfile } from "@/lib/types";
import { mcpLocalEndpoint } from "@/lib/types";

type ServiceTab = "mcp" | "canvs";
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

function canvsWebUrl(endpoint: string): string {
  const value = endpoint.trim().replace(/\/$/, "");
  return value ? `${value.replace(/\/mcp$/, "")}/canvs` : "";
}

export function WorkspaceDetailPage() {
  const params = useParams();
  const navigate = useNavigate();
  const {
    workspaces,
    mcpRuntimeStates,
    controlPlaneRevision,
    refreshWorkspaces,
    setWorkspaces,
    setMcpRuntimeState,
  } = useAdmin();
  const [profile, setProfile] = useState<WorkspaceProfile | null>(null);
  const [loading, setLoading] = useState(true);
  const [backendError, setBackendError] = useState("");
  const [activeService, setActiveService] = useState<ServiceTab>("mcp");
  const [mcpSubTab, setMcpSubTab] = useState<SubTab>("config");
  const [mcpBusy, setMcpBusy] = useState(false);
  const [mcpRuntime, setMcpRuntime] = useState<RuntimeStatus | null>(null);

  const workspaceId = params.id || workspaces[0]?.id || "";

  const refreshStatuses = useCallback(async (id: string) => {
    const mcp = await getRuntimeStatus(id);
    setMcpRuntime(mcp);
    setMcpRuntimeState(id, mcp.state);
  }, [setMcpRuntimeState]);

  const load = useCallback(async () => {
    if (!workspaceId) {
      setProfile(null);
      setLoading(false);
      return;
    }
    setLoading(true);
    setBackendError("");
    try {
      const items = await listWorkspaces();
      const next = items.find((item) => item.id === workspaceId) ?? null;
      setProfile(next);
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
    if (!workspaceId || controlPlaneRevision === 0) return;
    void refreshStatuses(workspaceId).catch((error) => setBackendError(String(error)));
  }, [controlPlaneRevision, refreshStatuses, workspaceId]);

  const mcpState: RuntimeState = mcpRuntime?.state ?? mcpRuntimeStates[workspaceId] ?? "stopped";
  const mcpLocal = mcpRuntime?.localEndpoint || (profile ? mcpLocalEndpoint(profile.runtime.local_port) : "");
  const mcpPublic = mcpRuntime?.publicEndpoint || "";

  const toggleService = async () => {
    if (!workspaceId) return;
    setMcpBusy(true);
    try {
      const result = await runServiceToggle(mcpState === "running", () => startRuntime(workspaceId), () => stopRuntime(workspaceId), "MCP");
      if (result) {
        setMcpRuntime(result);
        setMcpRuntimeState(workspaceId, result.state);
        if (mcpState !== "running" && result.state === "error") notifyStartFailure("MCP", result);
      }
    } finally { setMcpBusy(false); }
  };

  const persist = async (next: WorkspaceProfile, reload = false) => {
    if (!profile) return;
    await updateWorkspace(next, profile);
    setProfile(next);
    setWorkspaces((items) => items.map((item) => item.id === next.id ? next : item));
    if (reload) await load();
  };

  const saveMcpTunnel = async (config: TunnelFormConfig) => {
    if (!profile) return;
    const next: WorkspaceProfile = { ...profile, tunnel: { ...profile.tunnel, type: config.type, public_url: config.public_url, frp_server: config.frp_server, frp_subdomain: config.frp_subdomain, frp_profile_id: config.frp_profile_id, frp_server_port: config.frp_server_port, frp_proxy_type: config.frp_proxy_type, frp_cert_path: config.frp_cert_path, frp_key_path: config.frp_key_path, cloudflare_mode: config.cloudflare_mode, use_proxy: config.use_proxy } };
    await persist(next);
  };

  const removeWorkspace = async () => {
    if (!profile || !window.confirm(`确定删除工作区「${profile.name}」？不会删除磁盘目录。`)) return;
    await deleteWorkspace(profile.id);
    await refreshWorkspaces();
    toast.success("工作区已删除");
    navigate("/workspaces", { replace: true });
  };

  if (loading && !profile) return <div className="grid h-full place-items-center text-sm text-muted-foreground">正在加载工作区…</div>;
  if (!workspaceId || !profile) return (
    <div className="grid h-full place-items-center p-8">
      <Empty>
        <EmptyHeader>
          <EmptyTitle>工作区不存在或已删除</EmptyTitle>
          <EmptyDescription>未找到指定的工作区信息，请返回工作区列表查看。</EmptyDescription>
        </EmptyHeader>
        <Button className="mt-4" onClick={() => navigate("/workspaces")}>
          返回工作区列表
        </Button>
      </Empty>
    </div>
  );

  return (
    <PageLayout
      kicker={
        <div className="flex items-center gap-1.5">
          <Link
            to="/workspaces"
            className="inline-flex items-center gap-1 text-muted-foreground transition-colors hover:text-foreground"
          >
            <ChevronLeft className="size-3.5" />
            工作区列表
          </Link>
          <span>/</span>
          <span>工作区详情</span>
        </div>
      }
      title={profile.name}
      description={profile.path}
      actions={
        <div className="flex items-center gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => navigate("/workspaces")}
          >
            <ChevronLeft data-icon="inline-start" />
            返回列表
          </Button>
          <Button type="button" variant="destructive" size="sm" onClick={() => void removeWorkspace()}>
            <Trash2 data-icon="inline-start" />
            删除工作区
          </Button>
        </div>
      }
    >
      <div className="grid gap-5">
        {backendError && <Alert variant="destructive"><AlertTitle>控制面连接异常</AlertTitle><AlertDescription>{backendError}</AlertDescription></Alert>}
        <Card><CardContent className="p-4"><WorkspaceMetaForm name={profile.name} path={profile.path} onSave={async (name) => { await persist({ ...profile, name }); toast.success("工作区名称已更新"); }} onUpdatePath={async (path) => { await persist({ ...profile, path }, true); toast.success("工作区目录已更新"); }} /></CardContent></Card>
        <ChatGptSessionPrompt />

        <Tabs value={activeService} onValueChange={(value) => setActiveService((value ?? "mcp") as ServiceTab)}>
          <TabsList variant="line" className="w-full justify-start border-b">
            <TabsTrigger value="mcp"><Network data-icon="inline-start" />MCP <Badge variant="outline" className="ml-1">{mcpState}</Badge></TabsTrigger>
            <TabsTrigger value="canvs"><Gauge data-icon="inline-start" />Canvs</TabsTrigger>
          </TabsList>

          <TabsContent value="mcp" className="mt-4 grid gap-4">
            <div className="grid gap-4 xl:grid-cols-2">
              <ServicePanel title="MCP 服务" subtitle="ChatGPT Connector / MCP Server" status={mcpState} statusMessage={mcpRuntime?.localMessage ?? ""} recovery={mcpRuntime?.recovery ?? EMPTY_RECOVERY} activity={(mcpRuntime?.activity as McpActivity | null | undefined) ?? null} port={profile.runtime.local_port} portEditable busy={mcpBusy} tunnelType={profile.tunnel.type} localEndpoint={mcpLocal} publicEndpoint={mcpPublic} publicLabel="公网 MCP" onToggle={() => toggleService()} onPortChange={async (port) => { if (port === profile.runtime.local_port) return; await persist({ ...profile, runtime: { ...profile.runtime, local_port: port } }, true); }} />
              <GptQuickCopy workspaceId={workspaceId} profile={profile} publicMcpEndpoint={mcpPublic} />
            </div>
            <Tabs value={mcpSubTab} onValueChange={(value) => setMcpSubTab((value ?? "config") as SubTab)}><TabsList><TabsTrigger value="config">配置</TabsTrigger><TabsTrigger value="logs">日志</TabsTrigger><TabsTrigger value="health"><Activity data-icon="inline-start" />健康</TabsTrigger></TabsList>
              <TabsContent value="config" className="mt-4 grid gap-4 lg:grid-cols-2">
                <ConfigCard title="隧道" description="MCP 公网入口与隧道保活"><TunnelConfigForm workspaceId={workspaceId} config={tunnelForm(profile)} onSave={saveMcpTunnel} /></ConfigCard>
                <ConfigCard title="认证" description="OAuth、Bearer 与共享 Secret"><McpAuthForm workspaceId={workspaceId} auth={profile.auth} onSaveProfile={async (auth: AuthConfig, options) => { await persist({ ...profile, auth }); if (mcpState === "running" && options.callbackPolicyOnly) toast.success("OAuth Callback 信任策略已热更新"); }} /></ConfigCard>
                <ConfigCard title="运行策略" description="工具档位、Shell 与命令边界"><RuntimePolicyForm toolProfile={profile.runtime.tool_profile} permissionMode={profile.runtime.permission_mode} preferredShell={profile.runtime.preferred_shell ?? "auto"} allowedCommands={profile.runtime.allowed_commands ?? ""} workspaceLocalEntries={profile.runtime.workspace_local_entries ?? true} workspaceScriptExtensions={profile.runtime.workspace_script_extensions ?? ".exe,.bat,.cmd,.ps1"} externalPaidCommandsEnabled={profile.runtime.external_paid_commands_enabled ?? false} externalPaidMaxRunsPerDay={profile.runtime.external_paid_max_runs_per_day ?? 1} externalPaidMaxDurationSeconds={profile.runtime.external_paid_max_duration_seconds ?? 1800} onSave={async (draft: RuntimePolicyDraft) => persist({ ...profile, runtime: { ...profile.runtime, tool_profile: draft.toolProfile, permission_mode: draft.permissionMode, preferred_shell: draft.preferredShell, allowed_commands: draft.allowedCommands, workspace_local_entries: draft.workspaceLocalEntries, workspace_script_extensions: draft.workspaceScriptExtensions, external_paid_commands_enabled: draft.externalPaidCommandsEnabled, external_paid_max_runs_per_day: draft.externalPaidMaxRunsPerDay, external_paid_max_duration_seconds: draft.externalPaidMaxDurationSeconds } }, true)} /></ConfigCard>
                <ConfigCard title="Agent Skills" description="管理不可变 Skill packages 与激活 channel"><SkillServiceConfigForm workspaceId={workspaceId} enabled={profile.runtime.skill_service_enabled ?? true} onSave={async (config) => persist({ ...profile, runtime: { ...profile.runtime, skill_service_enabled: config.enabled } }, true)} /></ConfigCard>
                <div className="lg:col-span-2"><ConfigCard title="下游 MCP 聚合" description="统一接入 stdio 与 Streamable HTTP MCP"><McpProxyConfigForm config={profile.runtime.mcp_config ?? ""} onSave={async (config) => persist({ ...profile, runtime: { ...profile.runtime, mcp_config: config } }, true)} /></ConfigCard></div>
              </TabsContent>
              <TabsContent value="logs" className="mt-4"><LogViewer workspaceId={workspaceId} service="mcp" /></TabsContent>
              <TabsContent value="health" className="mt-4"><HealthPanel workspaceId={workspaceId} /></TabsContent>
            </Tabs>
          </TabsContent>

          <TabsContent value="canvs" className="mt-4"><CanvsPanel workspaceId={workspaceId} localUrl={canvsWebUrl(mcpLocal)} publicUrl={canvsWebUrl(mcpPublic)} /></TabsContent>
        </Tabs>
      </div>
    </PageLayout>
  );
}

function ConfigCard({ title, description, children }: { title: string; description: string; children: React.ReactNode }) {
  return <Card><CardHeader><CardTitle>{title}</CardTitle><CardDescription>{description}</CardDescription></CardHeader><CardContent>{children}</CardContent></Card>;
}

// 兼容别名
export const WorkspacePage = WorkspaceDetailPage;
