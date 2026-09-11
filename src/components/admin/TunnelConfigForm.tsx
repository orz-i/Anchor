import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";

import { SecretField } from "@/components/admin/SecretField";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { supportsAdminCommand } from "@/lib/api/invoke";
import { listFrpProfiles, type FrpProfileDto } from "@/lib/api/settings";
import {
  getTunnelSecret,
  setTunnelSecret,
  type TunnelSecretKey,
} from "@/lib/api/tunnel";
import type { TunnelConfig } from "@/lib/types";

export interface TunnelFormConfig {
  type: string;
  public_url: string;
  frp_server: string;
  frp_subdomain: string;
  frp_profile_id: string;
  frp_server_port: number;
  frp_proxy_type: string;
  frp_cert_path: string;
  frp_key_path: string;
  cloudflare_mode: string;
  use_proxy: boolean;
}

export function tunnelFormConfig(config: TunnelConfig): TunnelFormConfig {
  return {
    type: config.type,
    public_url: config.public_url,
    frp_server: config.frp_server,
    frp_subdomain: config.frp_subdomain,
    frp_profile_id: config.frp_profile_id ?? "",
    frp_server_port: config.frp_server_port ?? 7000,
    frp_proxy_type: config.frp_proxy_type ?? "http",
    frp_cert_path: config.frp_cert_path ?? "",
    frp_key_path: config.frp_key_path ?? "",
    cloudflare_mode: config.cloudflare_mode,
    use_proxy: config.use_proxy ?? true,
  };
}

export function TunnelConfigForm({
  tunnelId,
  config,
  onSave,
}: {
  tunnelId: string;
  config: TunnelFormConfig;
  onSave: (config: TunnelFormConfig) => void | Promise<void>;
}) {
  const initial = useMemo(
    () => ({
      type: config.type,
      public_url: config.public_url,
      frp_server: config.frp_server,
      frp_subdomain: config.frp_subdomain,
      frp_profile_id: config.frp_profile_id ?? "",
      frp_server_port: config.frp_server_port,
      frp_proxy_type: config.frp_proxy_type,
      frp_cert_path: config.frp_cert_path,
      frp_key_path: config.frp_key_path,
      cloudflare_mode: config.cloudflare_mode,
      use_proxy: config.use_proxy ?? true,
    }),
    [
      config.type,
      config.public_url,
      config.frp_server,
      config.frp_subdomain,
      config.frp_profile_id,
      config.frp_server_port,
      config.frp_proxy_type,
      config.frp_cert_path,
      config.frp_key_path,
      config.cloudflare_mode,
      config.use_proxy,
    ],
  );
  const [draft, setDraft] = useState<TunnelFormConfig>(initial);
  const [profiles, setProfiles] = useState<FrpProfileDto[]>([]);
  const [token, setToken] = useState("");
  const [loadedToken, setLoadedToken] = useState("");
  const [tokenMutation, setTokenMutation] = useState(false);
  const [manualFrpOpen, setManualFrpOpen] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => setDraft(initial), [initial]);
  useEffect(() => {
    void Promise.all([listFrpProfiles(), supportsAdminCommand("set_tunnel_secret")])
      .then(([items, supported]) => {
        setProfiles(items);
        setTokenMutation(supported);
      })
      .catch((error) => toast.error("加载隧道设置失败", { description: String(error) }));
  }, []);

  const selectedProfile = profiles.find((profile) => profile.id === draft.frp_profile_id) ?? null;
  const useGlobalProfile = Boolean(draft.frp_profile_id && selectedProfile);
  const showFrp = draft.type === "frp";
  const showCloudflare = draft.type === "cloudflare";
  const secretKey: TunnelSecretKey = showFrp ? "frp_token" : "cloudflare_token";
  const needsToken = (showFrp && !useGlobalProfile) || (showCloudflare && draft.cloudflare_mode === "named");

  useEffect(() => {
    let cancelled = false;
    if (!needsToken) {
      setToken("");
      setLoadedToken("");
      return;
    }
    void getTunnelSecret(tunnelId, secretKey)
      .then((value) => {
        if (!cancelled) {
          setToken(value ?? "");
          setLoadedToken(value ?? "");
        }
      })
      .catch((error) => {
        if (!cancelled) toast.error("读取 Tunnel Token 失败", { description: String(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [needsToken, secretKey, tunnelId]);

  const tokenDirty = needsToken && token !== loadedToken;
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial) || tokenDirty;
  const update = <K extends keyof TunnelFormConfig>(key: K, value: TunnelFormConfig[K]) =>
    setDraft((current) => ({ ...current, [key]: value }));

  const save = async () => {
    if (!dirty || saving) return;
    setSaving(true);
    try {
      if (tokenDirty) {
        if (!tokenMutation) throw new Error("当前 Web 管理 API 尚未开放 Tunnel Token 写入。");
        await setTunnelSecret(tunnelId, secretKey, token);
        setLoadedToken(token);
      }
      await onSave({ ...draft });
      toast.success("Tunnel 配置已保存");
    } catch (error) {
      toast.error("保存 Tunnel 失败", { description: String(error) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <form className="grid gap-5" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <FieldGroup>
        <Field>
          <FieldLabel>隧道类型</FieldLabel>
          <Select
            value={draft.type}
            onValueChange={(value) =>
              setDraft((current) => ({
                ...current,
                type: value ?? "none",
                public_url: value === current.type ? current.public_url : "",
              }))
            }
          >
            <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="none">未配置</SelectItem>
              <SelectItem value="frp">FRP</SelectItem>
              <SelectItem value="cloudflare">Cloudflare</SelectItem>
            </SelectContent>
          </Select>
        </Field>

        {(showFrp || showCloudflare) && (
          <label className="flex items-start gap-3 rounded-xl border p-3">
            <Checkbox checked={draft.use_proxy} onCheckedChange={(checked) => update("use_proxy", Boolean(checked))} />
            <span>
              <span className="block text-sm font-medium">使用网络代理</span>
              <span className="mt-1 block text-xs text-muted-foreground">启用后使用“设置 → 通用”中的全局代理建立 Tunnel；关闭则直连。</span>
            </span>
          </label>
        )}

        {showFrp && (
          <>
            <Field>
              <FieldLabel>FRP 配置</FieldLabel>
              <Select
                value={draft.frp_profile_id || "manual"}
                onValueChange={(value) => update("frp_profile_id", value === "manual" || !value ? "" : value)}
              >
                <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="manual">手动填写</SelectItem>
                  {profiles.map((profile) => (
                    <SelectItem key={profile.id} value={profile.id}>
                      {profile.name} · {profile.server}:{profile.serverPort}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <FieldDescription>全局 FRP Profile 可被多个 Tunnel 复用。</FieldDescription>
            </Field>
            {selectedProfile && (
              <div className="flex flex-wrap items-center gap-2 rounded-xl border bg-muted/20 p-3 text-xs">
                <span className="font-mono">{selectedProfile.server}:{selectedProfile.serverPort}</span>
                <Badge variant="outline">Token {selectedProfile.hasToken ? "已配置" : "未配置"}</Badge>
              </div>
            )}
            <Field>
              <FieldLabel htmlFor={`frp-subdomain-${tunnelId}`}>子域名</FieldLabel>
              <Input id={`frp-subdomain-${tunnelId}`} className="font-mono" value={draft.frp_subdomain} onChange={(event) => update("frp_subdomain", event.target.value)} />
            </Field>
            <Field>
              <FieldLabel>公网协议</FieldLabel>
              <Select value={draft.frp_proxy_type} onValueChange={(value) => update("frp_proxy_type", value ?? "http")}>
                <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="http">HTTP（FRPS vhostHTTPPort）</SelectItem>
                  <SelectItem value="https2http">HTTPS → 本地 HTTP</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            {draft.frp_proxy_type === "https2http" && (
              <div className="grid gap-4 md:grid-cols-2">
                <Field><FieldLabel>证书路径</FieldLabel><Input className="font-mono" value={draft.frp_cert_path} placeholder=".anchor/cert/example.pem" onChange={(event) => update("frp_cert_path", event.target.value)} /></Field>
                <Field><FieldLabel>私钥路径</FieldLabel><Input className="font-mono" value={draft.frp_key_path} placeholder=".anchor/cert/example.key" onChange={(event) => update("frp_key_path", event.target.value)} /></Field>
              </div>
            )}
            {!useGlobalProfile && (
              <>
                <Button type="button" variant="link" className="w-fit px-0" onClick={() => setManualFrpOpen((current) => !current)}>
                  {manualFrpOpen ? "收起" : "展开"}手动 FRP 配置
                </Button>
                {manualFrpOpen && (
                  <div className="grid gap-4 rounded-xl border p-4">
                    <div className="grid gap-4 md:grid-cols-2">
                      <Field><FieldLabel>FRP 服务器</FieldLabel><Input className="font-mono" value={draft.frp_server} onChange={(event) => update("frp_server", event.target.value)} /></Field>
                      <Field><FieldLabel>服务器端口</FieldLabel><Input type="number" min={1} max={65535} value={draft.frp_server_port} onChange={(event) => update("frp_server_port", Number(event.target.value))} /></Field>
                    </div>
                    <Field>
                      <FieldLabel>FRP Token</FieldLabel>
                      <SecretField value={token} disabled={!tokenMutation} onChange={setToken} />
                      <FieldDescription>Token 归当前 Tunnel 所有，不再写入 Workspace Secret。</FieldDescription>
                    </Field>
                  </div>
                )}
              </>
            )}
          </>
        )}

        {showCloudflare && (
          <>
            <Field>
              <FieldLabel>Cloudflare 模式</FieldLabel>
              <Select value={draft.cloudflare_mode} onValueChange={(value) => update("cloudflare_mode", value ?? "named")}>
                <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="quick">Quick Tunnel</SelectItem>
                  <SelectItem value="named">Named Tunnel</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            {draft.cloudflare_mode === "quick" ? (
              <Alert><AlertTitle>临时地址</AlertTitle><AlertDescription>Quick Tunnel 仅适合临时测试；服务重启后公网地址可能变化。</AlertDescription></Alert>
            ) : (
              <Field>
                <FieldLabel>Cloudflare Tunnel Token</FieldLabel>
                <SecretField value={token} disabled={!tokenMutation} onChange={setToken} />
                <FieldDescription>Token 归当前 Tunnel 所有，不再绑定 Workspace。</FieldDescription>
              </Field>
            )}
          </>
        )}

        <Field>
          <FieldLabel htmlFor={`public-url-${tunnelId}`}>公网 URL</FieldLabel>
          <Input
            id={`public-url-${tunnelId}`}
            type="url"
            className="font-mono"
            value={draft.public_url}
            placeholder="https://..."
            onChange={(event) => update("public_url", event.target.value)}
          />
          <FieldDescription>FRP 控制连接地址与实际公网 URL 相互独立。</FieldDescription>
        </Field>
      </FieldGroup>
      <div className="flex justify-end">
        <Button type="submit" disabled={saving || !dirty}>{saving ? "保存中…" : "保存配置"}</Button>
      </div>
    </form>
  );
}
