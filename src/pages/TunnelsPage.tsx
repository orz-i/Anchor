import { useCallback, useEffect, useMemo, useState } from "react";
import { Network, Play, Plus, RefreshCw, Square, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { PageLayout } from "@/components/admin/PageLayout";
import { TunnelConfigForm, tunnelFormConfig, type TunnelFormConfig } from "@/components/admin/TunnelConfigForm";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import {
  createTunnel,
  deleteTunnel,
  getTunnelStatus,
  listTunnels,
  startTunnel,
  stopTunnel,
  testTunnel,
  updateTunnel,
  type TunnelStatus,
} from "@/lib/api/tunnel";
import { listWorkspaces } from "@/lib/api/workspaces";
import type { TunnelProfile, WorkspaceProfile } from "@/lib/types";

export function TunnelsPage() {
  const [tunnels, setTunnels] = useState<TunnelProfile[]>([]);
  const [workspaces, setWorkspaces] = useState<WorkspaceProfile[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [createWorkspaceId, setCreateWorkspaceId] = useState("");
  const [createName, setCreateName] = useState("");
  const [creating, setCreating] = useState(false);
  const [metadataName, setMetadataName] = useState("");
  const [metadataBusy, setMetadataBusy] = useState(false);
  const [runtimeBusy, setRuntimeBusy] = useState(false);
  const [status, setStatus] = useState<TunnelStatus | null>(null);
  const [statusError, setStatusError] = useState("");

  const selected = tunnels.find((tunnel) => tunnel.id === selectedId) ?? null;
  const workspaceById = useMemo(
    () => new Map(workspaces.map((workspace) => [workspace.id, workspace])),
    [workspaces],
  );
  const availableWorkspaces = useMemo(() => {
    const occupied = new Set(tunnels.map((tunnel) => tunnel.workspace_id));
    return workspaces.filter((workspace) => !occupied.has(workspace.id));
  }, [tunnels, workspaces]);

  const refreshStatus = useCallback(async (id: string) => {
    try {
      setStatus(await getTunnelStatus(id));
      setStatusError("");
    } catch (error) {
      setStatus(null);
      setStatusError(String(error));
    }
  }, []);

  const load = useCallback(async () => {
    const [nextTunnels, nextWorkspaces] = await Promise.all([listTunnels(), listWorkspaces()]);
    setTunnels(nextTunnels);
    setWorkspaces(nextWorkspaces);
    setSelectedId((current) => {
      if (current && nextTunnels.some((tunnel) => tunnel.id === current)) return current;
      return nextTunnels[0]?.id ?? "";
    });
  }, []);

  useEffect(() => {
    void load()
      .catch((error) => toast.error("加载 Tunnel 失败", { description: String(error) }))
      .finally(() => setLoading(false));
  }, [load]);

  useEffect(() => {
    if (!selected) {
      setMetadataName("");
      setStatus(null);
      setStatusError("");
      return;
    }
    setMetadataName(selected.name);
    void refreshStatus(selected.id);
  }, [refreshStatus, selected]);

  const refresh = async () => {
    setRefreshing(true);
    try {
      await load();
      if (selectedId) await refreshStatus(selectedId);
      toast.success("Tunnel 状态已刷新");
    } catch (error) {
      toast.error("刷新失败", { description: String(error) });
    } finally {
      setRefreshing(false);
    }
  };

  const create = async () => {
    if (!createWorkspaceId || creating) return;
    setCreating(true);
    try {
      const created = await createTunnel(createWorkspaceId, createName);
      await load();
      setSelectedId(created.id);
      setCreateWorkspaceId("");
      setCreateName("");
      toast.success("Tunnel 已创建");
    } catch (error) {
      toast.error("创建 Tunnel 失败", { description: String(error) });
    } finally {
      setCreating(false);
    }
  };

  const saveMetadata = async () => {
    if (!selected || metadataBusy) return;
    const name = metadataName.trim();
    if (!name) {
      toast.warning("Tunnel 名称不能为空");
      return;
    }
    setMetadataBusy(true);
    try {
      const saved = await updateTunnel({ ...selected, name });
      setTunnels((items) => items.map((item) => item.id === saved.id ? saved : item));
      toast.success("Tunnel 名称已更新");
    } catch (error) {
      toast.error("更新 Tunnel 失败", { description: String(error) });
    } finally {
      setMetadataBusy(false);
    }
  };

  const toggleEnabled = async (enabled: boolean) => {
    if (!selected || metadataBusy) return;
    setMetadataBusy(true);
    try {
      if (!enabled) {
        await stopTunnel(selected.id).catch(() => undefined);
      }
      const saved = await updateTunnel({ ...selected, enabled });
      setTunnels((items) => items.map((item) => item.id === saved.id ? saved : item));
      await refreshStatus(saved.id);
      toast.success(enabled ? "Tunnel 已启用；目标 Workspace 启动时将自动托管" : "Tunnel 已禁用");
    } catch (error) {
      toast.error("更新 Tunnel 启用状态失败", { description: String(error) });
    } finally {
      setMetadataBusy(false);
    }
  };

  const saveConfig = async (config: TunnelFormConfig) => {
    if (!selected) return;
    const saved = await updateTunnel({
      ...selected,
      config: {
        ...selected.config,
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
    });
    setTunnels((items) => items.map((item) => item.id === saved.id ? saved : item));
    await refreshStatus(saved.id);
  };

  const runRuntime = async (operation: "start" | "stop" | "test") => {
    if (!selected || runtimeBusy) return;
    setRuntimeBusy(true);
    try {
      if (operation === "start") {
        const result = await startTunnel(selected.id);
        setStatus(result);
        toast.success("Tunnel 已启动", { description: result.publicUrl || result.state });
      } else if (operation === "stop") {
        const result = await stopTunnel(selected.id);
        setStatus(result);
        toast.success("Tunnel 已停止");
      } else {
        const result = await testTunnel(selected.id);
        (result.success ? toast.success : toast.warning)(result.success ? "Tunnel 测试成功" : "Tunnel 测试未完成", {
          description: [result.message, result.publicUrl].filter(Boolean).join(" · "),
        });
        await refreshStatus(selected.id);
      }
    } catch (error) {
      toast.error("Tunnel 操作失败", { description: String(error) });
    } finally {
      setRuntimeBusy(false);
    }
  };

  const remove = async () => {
    if (!selected || !window.confirm(`确定删除 Tunnel「${selected.name}」？关联 Token 也会一并删除。`)) return;
    setMetadataBusy(true);
    try {
      await deleteTunnel(selected.id);
      await load();
      toast.success("Tunnel 已删除");
    } catch (error) {
      toast.error("删除 Tunnel 失败", { description: String(error) });
    } finally {
      setMetadataBusy(false);
    }
  };

  if (loading) {
    return <div className="grid h-full place-items-center text-sm text-muted-foreground">正在加载 Tunnel…</div>;
  }

  const target = selected ? workspaceById.get(selected.workspace_id) : null;

  return (
    <PageLayout
      kicker="网络入口"
      title="隧道"
      description="统一管理 MCP 公网 Tunnel。Workspace 仅作为运行目标，不再持有 Tunnel 配置、Token 或生命周期开关。"
      actions={
        <Button type="button" variant="outline" size="sm" disabled={refreshing} onClick={() => void refresh()}>
          <RefreshCw className={refreshing ? "animate-spin" : ""} data-icon="inline-start" />
          刷新
        </Button>
      }
    >
      <div className="grid gap-5 xl:grid-cols-[22rem_minmax(0,1fr)]">
        <div className="flex min-w-0 flex-col gap-5">
          <Card>
            <CardHeader>
              <CardTitle>新建 Tunnel</CardTitle>
              <CardDescription>每个 Workspace 的 MCP 服务最多绑定一个顶级 Tunnel。</CardDescription>
            </CardHeader>
            <CardContent>
              <FieldGroup>
                <Field>
                  <FieldLabel>目标 Workspace</FieldLabel>
                  <Select value={createWorkspaceId || null} onValueChange={(value) => setCreateWorkspaceId(value ?? "")}>
                    <SelectTrigger className="w-full"><SelectValue placeholder="请选择 Workspace" /></SelectTrigger>
                    <SelectContent>
                      {availableWorkspaces.map((workspace) => <SelectItem key={workspace.id} value={workspace.id}>{workspace.name}</SelectItem>)}
                    </SelectContent>
                  </Select>
                  {availableWorkspaces.length === 0 && <FieldDescription>所有 Workspace 都已有 MCP Tunnel。</FieldDescription>}
                </Field>
                <Field>
                  <FieldLabel>名称</FieldLabel>
                  <Input value={createName} placeholder="留空使用 Workspace MCP" onChange={(event) => setCreateName(event.target.value)} />
                </Field>
              </FieldGroup>
              <div className="mt-4 flex justify-end">
                <Button type="button" disabled={!createWorkspaceId || creating} onClick={() => void create()}>
                  <Plus data-icon="inline-start" />{creating ? "创建中…" : "创建 Tunnel"}
                </Button>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>全部 Tunnel</CardTitle>
              <CardDescription>{tunnels.length} 个统一管理的网络入口</CardDescription>
            </CardHeader>
            <CardContent className="flex flex-col gap-2">
              {tunnels.length === 0 ? (
                <Empty>
                  <EmptyHeader><EmptyTitle>尚未创建 Tunnel</EmptyTitle><EmptyDescription>选择一个 Workspace 创建第一个 Tunnel。</EmptyDescription></EmptyHeader>
                </Empty>
              ) : tunnels.map((tunnel) => {
                const workspace = workspaceById.get(tunnel.workspace_id);
                return (
                  <button
                    type="button"
                    key={tunnel.id}
                    className={`rounded-xl border p-3 text-left transition-colors ${selectedId === tunnel.id ? "border-primary bg-primary/5" : "hover:bg-muted/50"}`}
                    onClick={() => setSelectedId(tunnel.id)}
                  >
                    <div className="flex items-center justify-between gap-3">
                      <span className="truncate text-sm font-medium">{tunnel.name}</span>
                      <Badge variant={tunnel.enabled ? "default" : "secondary"}>{tunnel.enabled ? "启用" : "禁用"}</Badge>
                    </div>
                    <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                      <Network className="size-3.5" />
                      <span className="truncate">{workspace?.name ?? tunnel.workspace_id}</span>
                      <span>·</span>
                      <span className="uppercase">{tunnel.config.type}</span>
                    </div>
                  </button>
                );
              })}
            </CardContent>
          </Card>
        </div>

        {selected ? (
          <div className="flex min-w-0 flex-col gap-5">
            <Card>
              <CardHeader className="flex-row items-start justify-between gap-4">
                <div className="min-w-0">
                  <CardTitle>{selected.name}</CardTitle>
                  <CardDescription className="mt-1">目标 · {target?.name ?? selected.workspace_id} / {selected.service.toUpperCase()}</CardDescription>
                </div>
                <div className="flex flex-wrap items-center justify-end gap-2">
                  <Badge variant="outline">{status?.state ?? (selected.enabled ? "configured" : "disabled")}</Badge>
                  <Button type="button" variant="outline" size="sm" disabled={runtimeBusy} onClick={() => void runRuntime("start")}><Play data-icon="inline-start" />启动</Button>
                  <Button type="button" variant="outline" size="sm" disabled={runtimeBusy} onClick={() => void runRuntime("stop")}><Square data-icon="inline-start" />停止</Button>
                  <Button type="button" variant="outline" size="sm" disabled={runtimeBusy} onClick={() => void runRuntime("test")}>测试</Button>
                </div>
              </CardHeader>
              <CardContent className="grid gap-4">
                {statusError && <Alert><AlertTitle>运行态不可直接读取</AlertTitle><AlertDescription>{statusError}</AlertDescription></Alert>}
                {status?.publicUrl && <div className="rounded-xl border bg-muted/30 p-3"><p className="text-xs text-muted-foreground">当前公网地址</p><code className="mt-1 block break-all text-xs">{status.publicUrl}</code></div>}
                <div className="grid gap-4 md:grid-cols-[minmax(0,1fr)_auto] md:items-end">
                  <Field>
                    <FieldLabel>Tunnel 名称</FieldLabel>
                    <Input value={metadataName} onChange={(event) => setMetadataName(event.target.value)} />
                  </Field>
                  <Button type="button" variant="outline" disabled={metadataBusy || metadataName.trim() === selected.name} onClick={() => void saveMetadata()}>保存名称</Button>
                </div>
                <label className="flex items-start gap-3 rounded-xl border p-3">
                  <Checkbox checked={selected.enabled} disabled={metadataBusy} onCheckedChange={(checked) => void toggleEnabled(Boolean(checked))} />
                  <span><span className="block text-sm font-medium">启用自动托管</span><span className="mt-1 block text-xs text-muted-foreground">启用后，目标 Workspace daemon 启动时自动托管此 Tunnel；手动“启动/测试”仍是显式操作。</span></span>
                </label>
              </CardContent>
            </Card>

            <Card>
              <CardHeader><CardTitle>Tunnel 配置</CardTitle><CardDescription>类型、网络、FRP/Cloudflare 参数与 Tunnel 自有 Secret。</CardDescription></CardHeader>
              <CardContent><TunnelConfigForm tunnelId={selected.id} config={tunnelFormConfig(selected.config)} onSave={saveConfig} /></CardContent>
            </Card>

            <Card>
              <CardHeader><CardTitle>删除 Tunnel</CardTitle><CardDescription>删除配置与 Tunnel 自有 Token，不会删除目标 Workspace。</CardDescription></CardHeader>
              <CardContent className="flex justify-end"><Button type="button" variant="destructive" disabled={metadataBusy} onClick={() => void remove()}><Trash2 data-icon="inline-start" />删除 Tunnel</Button></CardContent>
            </Card>
          </div>
        ) : (
          <Card className="grid min-h-64 place-items-center"><CardContent><Empty><EmptyHeader><EmptyTitle>选择一个 Tunnel</EmptyTitle><EmptyDescription>从左侧选择已有 Tunnel，或先创建一个。</EmptyDescription></EmptyHeader></Empty></CardContent></Card>
        )}
      </div>
    </PageLayout>
  );
}
