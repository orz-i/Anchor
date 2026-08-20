import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw, Route, ServerCog } from "lucide-react";
import { toast } from "sonner";

import { PageLayout } from "@/components/admin/PageLayout";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { isPrivilegedActionCancelled } from "@/lib/api/admin-security";
import { supportsAdminCommand } from "@/lib/api/invoke";
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
  uninstallWindowsService,
  type McpGatewayConfigDto,
  type McpGatewayStatusDto,
  type ProxyConfigDto,
  type WindowsScmServiceStatusDto,
} from "@/lib/api/settings";
import { getGatewayControlEvents, listWorkspaces, readGatewayLogs } from "@/lib/api/workspaces";
import type { GatewayEventCursor, GatewayLogChunk, WorkspaceProfile } from "@/lib/types";

const DEFAULT_GATEWAY: McpGatewayConfigDto = {
  urlModelVersion: 2,
  enabled: false,
  localPort: 28765,
  ownerWorkspaceId: "",
  publicUrl: "",
  observedPublicUrl: "",
  observedOwnerWorkspaceId: "",
  observedTunnelSignature: "",
};

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

export function GeneralSettingsPage() {
  const [proxy, setProxyState] = useState<ProxyConfigDto>({ mode: "none", url: "" });
  const [proxyChanged, setProxyChanged] = useState(false);
  const [proxySaving, setProxySaving] = useState(false);
  const [gateway, setGatewayState] = useState<McpGatewayConfigDto>(DEFAULT_GATEWAY);
  const [gatewayStatus, setGatewayStatus] = useState<McpGatewayStatusDto | null>(null);
  const [gatewayChanged, setGatewayChanged] = useState(false);
  const [gatewaySaving, setGatewaySaving] = useState(false);
  const [gatewayRouteBusy, setGatewayRouteBusy] = useState<Record<string, boolean>>({});
  const [gatewayEventFault, setGatewayEventFault] = useState("");
  const [gatewayLog, setGatewayLog] = useState<GatewayLogChunk | null>(null);
  const [gatewayLogError, setGatewayLogError] = useState("");
  const [workspaces, setWorkspaces] = useState<WorkspaceProfile[]>([]);
  const [windowsService, setWindowsService] = useState<WindowsScmServiceStatusDto | null>(null);
  const [windowsServiceBusy, setWindowsServiceBusy] = useState(false);
  const [windowsServiceMutationsSupported, setWindowsServiceMutationsSupported] = useState(false);
  const gatewayDraftDirty = useRef(false);

  useEffect(() => {
    gatewayDraftDirty.current = gatewayChanged || gatewaySaving;
  }, [gatewayChanged, gatewaySaving]);

  const refreshGatewayLog = useCallback(async () => {
    try {
      setGatewayLog(await readGatewayLogs(80));
      setGatewayLogError("");
    } catch (error) {
      setGatewayLogError(String(error));
    }
  }, []);

  const refreshGatewayRuntime = useCallback(async () => {
    const [nextGateway, nextStatus, nextWorkspaces] = await Promise.all([
      getMcpGateway(),
      getMcpGatewayStatus(),
      listWorkspaces(),
    ]);
    setGatewayStatus(nextStatus);
    setWorkspaces(nextWorkspaces);
    if (!gatewayDraftDirty.current) setGatewayState(nextGateway);
  }, []);

  const refresh = useCallback(async () => {
    const [nextProxy, nextGateway, nextGatewayStatus, nextWorkspaces, nextWindowsService] = await Promise.all([
      getProxy(),
      getMcpGateway(),
      getMcpGatewayStatus(),
      listWorkspaces(),
      getWindowsServiceStatus(),
    ]);
    setProxyState(nextProxy);
    setGatewayState(nextGateway);
    setGatewayStatus(nextGatewayStatus);
    setWorkspaces(nextWorkspaces);
    setWindowsService(nextWindowsService);
    setProxyChanged(false);
    setGatewayChanged(false);
    await refreshGatewayLog();
  }, [refreshGatewayLog]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const serviceCommands = [
          "install_windows_service",
          "uninstall_windows_service",
          "start_windows_service",
          "stop_windows_service",
          "restart_windows_service",
          "sync_windows_service_plan",
        ];
        const supported = await Promise.all(serviceCommands.map(supportsAdminCommand));
        if (!cancelled) setWindowsServiceMutationsSupported(supported.every(Boolean));
        await refresh();
      } catch (error) {
        if (!cancelled) toast.error("加载通用设置失败", { description: String(error) });
      }
    })();

    const observeGateway = async () => {
      let cursor: GatewayEventCursor | null = null;
      while (!cancelled) {
        try {
          const batch = await getGatewayControlEvents(cursor, 15_000);
          if (cancelled) return;
          if (batch === null) {
            cursor = null;
            setGatewayEventFault("");
            await refreshGatewayRuntime();
            await refreshGatewayLog();
            await delay(2_000);
            continue;
          }
          cursor = batch.nextCursor;
          setGatewayEventFault("");
          if (batch.events.length > 0 || batch.reset) {
            await refreshGatewayRuntime();
            await refreshGatewayLog();
          }
        } catch (error) {
          if (cancelled) return;
          setGatewayEventFault(String(error));
          await delay(3_000);
        }
      }
    };

    void observeGateway();
    return () => {
      cancelled = true;
    };
  }, [refresh, refreshGatewayLog, refreshGatewayRuntime]);

  const updateProxy = (patch: Partial<ProxyConfigDto>) => {
    setProxyState((current) => ({ ...current, ...patch }));
    setProxyChanged(true);
  };

  const saveProxy = async () => {
    setProxySaving(true);
    try {
      await setProxy(proxy);
      setProxyChanged(false);
      toast.success("代理设置已保存");
    } catch (error) {
      toast.error("保存代理设置失败", { description: String(error) });
    } finally {
      setProxySaving(false);
    }
  };

  const updateGateway = (patch: Partial<McpGatewayConfigDto>) => {
    setGatewayState((current) => ({ ...current, ...patch }));
    setGatewayChanged(true);
  };

  const saveGateway = async () => {
    setGatewaySaving(true);
    try {
      setGatewayStatus(await setMcpGateway(gateway));
      setGatewayState(await getMcpGateway());
      setGatewayChanged(false);
      await refreshGatewayLog();
      toast.success("MCP Gateway 设置已保存");
    } catch (error) {
      toast.error("保存 Gateway 失败", { description: String(error) });
    } finally {
      setGatewaySaving(false);
    }
  };

  const gatewayBaseUrl =
    gateway.observedPublicUrl.trim().replace(/\/$/, "") ||
    gateway.publicUrl.trim().replace(/\/$/, "") ||
    `http://127.0.0.1:${gateway.localPort}`;

  const toggleGatewayRoute = async (workspace: WorkspaceProfile, enabled: boolean) => {
    if (gatewayRouteBusy[workspace.id] || gatewayChanged || gatewaySaving) return;
    setGatewayRouteBusy((current) => ({ ...current, [workspace.id]: true }));
    try {
      setGatewayStatus(await setMcpGatewayRoute(workspace.id, enabled));
      await refreshGatewayLog();
      toast.success(enabled ? `${workspace.name} 已加入 Gateway routes` : `${workspace.name} 已移出 Gateway routes`);
    } catch (error) {
      toast.error("Gateway route 操作失败", { description: String(error) });
    } finally {
      setGatewayRouteBusy((current) => ({ ...current, [workspace.id]: false }));
    }
  };

  const runWindowsServiceAction = async (
    action: () => Promise<WindowsScmServiceStatusDto>,
    successMessage: string,
  ) => {
    if (!windowsServiceMutationsSupported) return;
    setWindowsServiceBusy(true);
    try {
      setWindowsService(await action());
      toast.success(successMessage);
    } catch (error) {
      if (!isPrivilegedActionCancelled(error)) toast.error("Windows Service 操作失败", { description: String(error) });
    } finally {
      setWindowsServiceBusy(false);
    }
  };

  return (
    <PageLayout
      kicker="全局设置"
      title="通用"
      description="管理全局网络代理、单一 MCP Gateway 与 Windows SCM 后台服务。"
    >
      <div className="flex flex-col gap-5">
        <Card>
          <CardHeader>
            <CardTitle>网络代理</CardTitle>
            <CardDescription>应用于 Cloudflare 隧道连接；软件下载使用“软件管理”中的独立代理设置。</CardDescription>
          </CardHeader>
          <CardContent>
            <form className="flex flex-col gap-5" onSubmit={(event) => { event.preventDefault(); void saveProxy(); }}>
              <FieldGroup>
                <Field>
                  <FieldLabel>代理模式</FieldLabel>
                  <Select value={proxy.mode} onValueChange={(value) => updateProxy({ mode: value ?? "none" })}>
                    <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="none">无代理</SelectItem>
                      <SelectItem value="system">系统代理</SelectItem>
                      <SelectItem value="manual">手动代理地址</SelectItem>
                    </SelectContent>
                  </Select>
                </Field>
                {proxy.mode === "manual" && (
                  <Field>
                    <FieldLabel htmlFor="global-proxy-url">代理地址</FieldLabel>
                    <Input id="global-proxy-url" className="font-mono" value={proxy.url} placeholder="http://127.0.0.1:7890" onChange={(event) => updateProxy({ url: event.target.value })} />
                    <FieldDescription>支持 HTTP、HTTPS 与 SOCKS 代理。</FieldDescription>
                  </Field>
                )}
              </FieldGroup>
              <div className="flex justify-end"><Button type="submit" disabled={!proxyChanged || proxySaving}>{proxySaving ? "保存中…" : "保存代理设置"}</Button></div>
            </form>
          </CardContent>
        </Card>

        {windowsService?.supported && (
          <Card>
            <CardHeader className="flex-row items-start justify-between gap-4">
              <div>
                <CardTitle>Windows 后台服务</CardTitle>
                <CardDescription className="mt-1">使用 Windows SCM 在开机时监督 Workspace daemon 与 Gateway daemon，Web 管理面无需常驻桌面窗口。</CardDescription>
              </div>
              <Badge variant="outline">{windowsService.installed ? `${windowsService.state}${windowsService.autoStart ? " · 自动启动" : ""}` : "未安装"}</Badge>
            </CardHeader>
            <CardContent className="flex flex-col gap-4">
              <div className="grid gap-3 md:grid-cols-2">
                <div className="rounded-xl border bg-muted/30 p-3 text-xs leading-5 text-muted-foreground">
                  <p><span className="font-medium text-foreground">Service</span> · {windowsService.serviceName}</p>
                  <p className="break-all">配置域 · {windowsService.configDir}</p>
                  <p className="break-all">启动计划 · {windowsService.planPath}</p>
                </div>
                <div className="rounded-xl border bg-muted/30 p-3 text-xs leading-5 text-muted-foreground">
                  <p>开机 Workspace · {windowsService.plan.workspaces.length}</p>
                  <p>Gateway routes · {windowsService.plan.gatewayWorkspaceIds.length}</p>
                  <p>构建状态 · {windowsService.buildState}{windowsService.processId ? ` · PID ${windowsService.processId}` : ""}</p>
                </div>
              </div>
              {!windowsServiceMutationsSupported && (
                <Alert><AlertTitle>只读模式</AlertTitle><AlertDescription>当前 Web 管理 API 未开放 SCM 生命周期执行器。</AlertDescription></Alert>
              )}
              <div className="flex flex-wrap justify-end gap-2">
                <Button type="button" variant="outline" disabled={!windowsServiceMutationsSupported || windowsServiceBusy} onClick={() => void runWindowsServiceAction(syncWindowsServicePlan, "已同步当前 daemon/Gateway 运行态")}>同步当前运行态</Button>
                {!windowsService.installed ? (
                  <Button type="button" disabled={!windowsServiceMutationsSupported || windowsServiceBusy} onClick={() => void runWindowsServiceAction(installWindowsService, "Windows SCM Service 已安装并设置为自动启动")}>安装并自动启动</Button>
                ) : (
                  <>
                    <Button type="button" variant="outline" disabled={!windowsServiceMutationsSupported || windowsServiceBusy} onClick={() => void runWindowsServiceAction(installWindowsService, "Windows SCM Service 已更新到当前构建")}>更新服务版本</Button>
                    {windowsService.state === "running" ? (
                      <Button type="button" variant="outline" disabled={!windowsServiceMutationsSupported || windowsServiceBusy} onClick={() => void runWindowsServiceAction(stopWindowsService, "Windows SCM Service 已停止")}>停止</Button>
                    ) : (
                      <Button type="button" variant="outline" disabled={!windowsServiceMutationsSupported || windowsServiceBusy} onClick={() => void runWindowsServiceAction(startWindowsService, "Windows SCM Service 已启动")}>启动</Button>
                    )}
                    <Button type="button" variant="outline" disabled={!windowsServiceMutationsSupported || windowsServiceBusy} onClick={() => void runWindowsServiceAction(restartWindowsService, "Windows SCM Service 已重启")}>重启</Button>
                    <Button type="button" variant="destructive" disabled={!windowsServiceMutationsSupported || windowsServiceBusy} onClick={() => void runWindowsServiceAction(uninstallWindowsService, "Windows SCM Service 已卸载")}>卸载</Button>
                  </>
                )}
              </div>
            </CardContent>
          </Card>
        )}

        <Card>
          <CardHeader className="flex-row items-start justify-between gap-4">
            <div>
              <CardTitle>单一 MCP Gateway</CardTitle>
              <CardDescription className="mt-1">通过一个本地网关与一个公网隧道暴露多个工作区；每个工作区仍使用独立路径、OAuth resource、会话和工具上下文。</CardDescription>
            </div>
            {gatewayStatus && <Badge variant="outline">{gatewayStatus.state}{gatewayStatus.routeCount ? ` · ${gatewayStatus.routeCount} routes` : ""}</Badge>}
          </CardHeader>
          <CardContent>
            <form className="flex flex-col gap-5" onSubmit={(event) => { event.preventDefault(); void saveGateway(); }}>
              <FieldGroup>
                <Field orientation="horizontal">
                  <Checkbox checked={gateway.enabled} onCheckedChange={(checked) => updateGateway({ enabled: Boolean(checked) })} />
                  <div>
                    <FieldLabel>启用单一 Gateway</FieldLabel>
                    <FieldDescription>启用后，各工作区原有 MCP 隧道停止，仅由 Gateway 提供统一公网入口。</FieldDescription>
                  </div>
                </Field>
                <div className="grid gap-4 md:grid-cols-2">
                  <Field>
                    <FieldLabel htmlFor="gateway-port">Gateway 本地端口</FieldLabel>
                    <Input id="gateway-port" type="number" min={1} max={65535} value={gateway.localPort} onChange={(event) => updateGateway({ localPort: Number(event.target.value) })} />
                  </Field>
                  <Field>
                    <FieldLabel>隧道所有者工作区</FieldLabel>
                    <Select disabled={!gateway.enabled} value={gateway.ownerWorkspaceId || null} onValueChange={(value) => updateGateway({ ownerWorkspaceId: value ?? "" })}>
                      <SelectTrigger className="w-full"><SelectValue placeholder="请选择工作区" /></SelectTrigger>
                      <SelectContent>{workspaces.map((workspace) => <SelectItem key={workspace.id} value={workspace.id}>{workspace.name}</SelectItem>)}</SelectContent>
                    </Select>
                  </Field>
                </div>
                <Field>
                  <FieldLabel htmlFor="gateway-public-url">Gateway 公网基础地址</FieldLabel>
                  <Input id="gateway-public-url" type="url" className="font-mono" value={gateway.publicUrl} placeholder="https://anchor.example.com" onChange={(event) => updateGateway({ publicUrl: event.target.value })} />
                  <FieldDescription>不包含 <code>/w/&lt;workspace&gt;/mcp</code>；远程地址必须使用 HTTPS 且不能包含子路径。</FieldDescription>
                </Field>
              </FieldGroup>

              {gateway.observedPublicUrl && (
                <div className="rounded-xl border bg-muted/30 p-3 text-xs"><span className="text-muted-foreground">当前观测公网地址 · </span><code className="break-all">{gateway.observedPublicUrl}</code></div>
              )}

              {gateway.enabled && (
                <div className="rounded-xl border bg-muted/20 p-3">
                  <div className="mb-3 flex items-center gap-2 text-sm font-medium"><Route className="size-4" />ChatGPT 工作区路由</div>
                  <div className="flex flex-col gap-2">
                    {workspaces.map((workspace) => {
                      const active = gatewayStatus?.routeWorkspaceIds.includes(workspace.id) ?? false;
                      return (
                        <div key={workspace.id} className="flex items-center justify-between gap-3 rounded-lg border bg-background p-3">
                          <div className="min-w-0">
                            <div className="flex items-center gap-2"><p className="truncate text-sm font-medium">{workspace.name}</p><Badge variant="outline">{active ? "路由已注册" : "未启动"}</Badge></div>
                            <code className="mt-1 block break-all text-xs text-muted-foreground">{gatewayBaseUrl}/w/{workspace.id}/mcp</code>
                          </div>
                          <Button type="button" variant="outline" disabled={gatewayChanged || gatewaySaving || gatewayRouteBusy[workspace.id]} onClick={() => void toggleGatewayRoute(workspace, !active)}>{gatewayRouteBusy[workspace.id] ? "处理中…" : active ? "停止路由" : "启动路由"}</Button>
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}

              {gatewayEventFault && <Alert variant="destructive"><AlertTitle>Gateway 事件控制异常</AlertTitle><AlertDescription>{gatewayEventFault}</AlertDescription></Alert>}
              {gatewayStatus?.error && <Alert variant="destructive"><AlertTitle>Gateway 错误</AlertTitle><AlertDescription>{gatewayStatus.error}</AlertDescription></Alert>}
              {gatewayStatus && !gatewayStatus.daemonSupported && <Alert><ServerCog /><AlertTitle>当前平台不支持独立 Gateway daemon</AlertTitle><AlertDescription>配置可以保存，但后台 Gateway 需要在支持 daemon 的环境中运行。</AlertDescription></Alert>}

              <div className="flex justify-end"><Button type="submit" disabled={!gatewayChanged || gatewaySaving}>{gatewaySaving ? "保存中…" : "保存 Gateway 设置"}</Button></div>
            </form>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex-row items-start justify-between gap-4">
            <div><CardTitle>Gateway 日志</CardTitle><CardDescription>最近 80 行 Gateway 控制面日志。</CardDescription></div>
            <Button type="button" variant="outline" size="sm" onClick={() => void refreshGatewayLog()}><RefreshCw data-icon="inline-start" />刷新</Button>
          </CardHeader>
          <CardContent>
            {gatewayLogError ? (
              <Alert variant="destructive"><AlertTitle>日志读取失败</AlertTitle><AlertDescription>{gatewayLogError}</AlertDescription></Alert>
            ) : gatewayLog?.exists ? (
              <ScrollArea className="h-56 rounded-xl border bg-muted/30 p-3"><pre className="whitespace-pre-wrap break-all font-mono text-xs leading-5">{gatewayLog.content || "暂无新日志"}</pre></ScrollArea>
            ) : (
              <p className="text-sm text-muted-foreground">暂无 Gateway 日志。</p>
            )}
          </CardContent>
        </Card>
      </div>
    </PageLayout>
  );
}
