import { useEffect, useState } from "react";
import { Download, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { PageLayout } from "@/components/admin/PageLayout";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { isPrivilegedActionCancelled } from "@/lib/api/admin-security";
import { supportsAdminCommand } from "@/lib/api/invoke";
import {
  getDownloadConfig,
  installSoftware,
  listSoftware,
  setDownloadConfig,
  uninstallSoftware,
  type DownloadConfig,
  type SoftwareStatus,
} from "@/lib/api/software";

export function SoftwareSettingsPage() {
  const [software, setSoftware] = useState<SoftwareStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [installing, setInstalling] = useState<string | null>(null);
  const [uninstalling, setUninstalling] = useState<string | null>(null);
  const [mutationSupported, setMutationSupported] = useState(false);
  const [config, setConfig] = useState<DownloadConfig>({ githubMirror: "https://gh-proxy.com", proxyMode: "system", proxyUrl: "" });
  const [configChanged, setConfigChanged] = useState(false);

  const refresh = async () => {
    setLoading(true);
    try {
      const [items, nextConfig] = await Promise.all([listSoftware(), getDownloadConfig()]);
      setSoftware(items);
      setConfig(nextConfig);
      setConfigChanged(false);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [canInstall, canUninstall] = await Promise.all([
          supportsAdminCommand("install_software"),
          supportsAdminCommand("uninstall_software"),
        ]);
        if (!cancelled) setMutationSupported(canInstall && canUninstall);
        await refresh();
      } catch (error) {
        if (!cancelled) toast.error("加载软件状态失败", { description: String(error) });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const install = async (item: SoftwareStatus) => {
    if (!mutationSupported) return;
    setInstalling(item.kind);
    try {
      await installSoftware(item.kind, item.targetVersion);
      await refresh();
      toast.success(`${item.name} 已安装`);
    } catch (error) {
      if (!isPrivilegedActionCancelled(error)) toast.error("安装失败", { description: String(error) });
    } finally {
      setInstalling(null);
    }
  };

  const uninstall = async (item: SoftwareStatus) => {
    if (!mutationSupported) return;
    setUninstalling(item.kind);
    try {
      await uninstallSoftware(item.kind);
      await refresh();
      toast.success(`${item.name} 已卸载`);
    } catch (error) {
      if (!isPrivilegedActionCancelled(error)) toast.error("卸载失败", { description: String(error) });
    } finally {
      setUninstalling(null);
    }
  };

  const saveConfig = async () => {
    try {
      await setDownloadConfig(config);
      setConfigChanged(false);
      toast.success("下载配置已保存");
    } catch (error) {
      toast.error("保存失败", { description: String(error) });
    }
  };

  const updateConfig = (patch: Partial<DownloadConfig>) => {
    setConfig((current) => ({ ...current, ...patch }));
    setConfigChanged(true);
  };

  return (
    <PageLayout kicker="全局设置" title="软件管理" description="安装和管理 frpc、cloudflared 等 Anchor 使用的隧道客户端。">
      <div className="flex flex-col gap-5">
        {!mutationSupported && !loading && (
          <Alert>
            <AlertTitle>只读模式</AlertTitle>
            <AlertDescription>当前 Web 管理 API 提供软件状态查看，但未开放安装与卸载执行器。</AlertDescription>
          </Alert>
        )}
        <Card>
          <CardHeader>
            <CardTitle>软件状态</CardTitle>
            <CardDescription>管理安装使用 Anchor 固定的目标版本，并在高权限执行前进行二次确认。</CardDescription>
          </CardHeader>
          <CardContent>
            {loading ? <p className="text-sm text-muted-foreground">加载中…</p> : (
              <div className="flex flex-col gap-2">
                {software.map((item) => (
                  <div key={item.kind} className="flex items-center justify-between gap-4 rounded-xl border p-3">
                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <p className="text-sm font-medium">{item.name}</p>
                        <Badge variant={item.installed ? "secondary" : "outline"}>{item.installed ? "已安装" : "未安装"}</Badge>
                        {item.installed && <Badge variant="outline">{item.managed ? "Anchor 管理" : "系统安装"}</Badge>}
                      </div>
                      <p className="mt-1 truncate font-mono text-xs text-muted-foreground">{item.installed ? item.path : `目标版本 ${item.targetVersion}`}</p>
                    </div>
                    {item.installed ? (
                      item.managed && <Button type="button" variant="outline" disabled={!mutationSupported || uninstalling === item.kind} onClick={() => void uninstall(item)}><Trash2 data-icon="inline-start" />{uninstalling === item.kind ? "卸载中…" : "卸载"}</Button>
                    ) : (
                      <Button type="button" disabled={!mutationSupported || installing === item.kind} onClick={() => void install(item)}><Download data-icon="inline-start" />{installing === item.kind ? "安装中…" : "安装"}</Button>
                    )}
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>下载设置</CardTitle>
            <CardDescription>独立于 Cloudflare 隧道网络代理，用于软件下载。</CardDescription>
          </CardHeader>
          <CardContent>
            <form className="flex flex-col gap-5" onSubmit={(event) => { event.preventDefault(); void saveConfig(); }}>
              <FieldGroup>
                <Field>
                  <FieldLabel htmlFor="github-mirror">GitHub 镜像</FieldLabel>
                  <Input id="github-mirror" className="font-mono" value={config.githubMirror} placeholder="https://gh-proxy.com" onChange={(event) => updateConfig({ githubMirror: event.target.value })} />
                  <FieldDescription>留空则直连 GitHub。</FieldDescription>
                </Field>
                <Field>
                  <FieldLabel>代理模式</FieldLabel>
                  <Select value={config.proxyMode} onValueChange={(value) => updateConfig({ proxyMode: value ?? "system" })}>
                    <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="system">系统代理</SelectItem>
                      <SelectItem value="none">无代理</SelectItem>
                      <SelectItem value="manual">手动代理地址</SelectItem>
                    </SelectContent>
                  </Select>
                </Field>
                {config.proxyMode === "manual" && (
                  <Field>
                    <FieldLabel htmlFor="download-proxy">代理地址</FieldLabel>
                    <Input id="download-proxy" className="font-mono" value={config.proxyUrl} placeholder="http://127.0.0.1:7890" onChange={(event) => updateConfig({ proxyUrl: event.target.value })} />
                  </Field>
                )}
              </FieldGroup>
              <div className="flex justify-end"><Button type="submit" disabled={!configChanged}>保存设置</Button></div>
            </form>
          </CardContent>
        </Card>
      </div>
    </PageLayout>
  );
}
