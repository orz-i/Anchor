import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import { SecretField } from "@/components/admin/SecretField";
import { validateRedirectHosts, validateRedirectUris } from "@/components/admin/auth-validation";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { isPrivilegedActionCancelled } from "@/lib/api/admin-security";
import { supportsAdminCommand } from "@/lib/api/invoke";
import { getSharedSecret, getWorkspaceSecret, regenerateSharedSecret, regenerateWorkspaceSecret, setSharedSecret, type SharedSecretKey, type WorkspaceSecretKey } from "@/lib/api/secrets";
import type { AuthConfig } from "@/lib/types";

export function McpAuthForm({ workspaceId, auth, onSaveProfile }: { workspaceId: string; auth: AuthConfig; onSaveProfile: (auth: AuthConfig, options: { callbackPolicyOnly: boolean }) => void | Promise<void> }) {
  const initial = useMemo<AuthConfig>(() => ({ type: auth.type, oauth_client_id: auth.oauth_client_id, oauth_redirect_uris: auth.oauth_redirect_uris ?? "", oauth_redirect_hosts: auth.oauth_redirect_hosts ?? "", use_shared_secrets: !!auth.use_shared_secrets }), [auth]);
  const [draft, setDraft] = useState<AuthConfig>(initial);
  const [secrets, setSecrets] = useState<Partial<Record<WorkspaceSecretKey, string>>>({});
  const [loadedSecrets, setLoadedSecrets] = useState<Partial<Record<WorkspaceSecretKey, string>>>({});
  const [loadedSharedClientId, setLoadedSharedClientId] = useState("");
  const [saving, setSaving] = useState(false);
  const [regenerating, setRegenerating] = useState<WorkspaceSecretKey | null>(null);
  const [workspaceMutation, setWorkspaceMutation] = useState(false);
  const [sharedMutation, setSharedMutation] = useState(false);
  const sequence = useRef(0);

  useEffect(() => setDraft(initial), [initial]);
  useEffect(() => { void (async () => { const [workspace, sharedRegenerate, sharedWrite] = await Promise.all([supportsAdminCommand("regenerate_workspace_secret"), supportsAdminCommand("regenerate_shared_secret"), supportsAdminCommand("set_shared_secret")]); setWorkspaceMutation(workspace); setSharedMutation(sharedRegenerate && sharedWrite); })(); }, []);
  useEffect(() => {
    const seq = ++sequence.current;
    void (async () => {
      const useShared = !!draft.use_shared_secrets;
      const keys: WorkspaceSecretKey[] = draft.type === "oauth" ? ["oauth_client_secret", "oauth_password"] : draft.type === "bearer" ? ["bearer_token"] : [];
      const clientId = draft.type === "oauth" && useShared ? (await getSharedSecret("oauth_client_id")) ?? "" : "";
      const entries = await Promise.all(keys.map(async (key) => [key, (useShared ? await getSharedSecret(key as SharedSecretKey) : await getWorkspaceSecret(workspaceId, key)) ?? ""] as const));
      if (seq !== sequence.current) return;
      const next = Object.fromEntries(entries);
      setSecrets(next); setLoadedSecrets(next); setLoadedSharedClientId(clientId);
      if (draft.type === "oauth" && useShared) setDraft((current) => ({ ...current, oauth_client_id: clientId }));
    })().catch((error) => { if (seq === sequence.current) toast.error("加载认证 Secret 失败", { description: String(error) }); });
  }, [draft.type, draft.use_shared_secrets, workspaceId]);

  const mutationSupported = draft.use_shared_secrets ? sharedMutation : workspaceMutation;
  const secretsDirty = Object.keys(secrets).some((key) => secrets[key as WorkspaceSecretKey] !== loadedSecrets[key as WorkspaceSecretKey]);
  const clientIdChanged = draft.use_shared_secrets ? draft.oauth_client_id !== loadedSharedClientId : draft.oauth_client_id !== auth.oauth_client_id;
  const dirty = draft.type !== auth.type || clientIdChanged || (draft.oauth_redirect_uris ?? "") !== (auth.oauth_redirect_uris ?? "") || (draft.oauth_redirect_hosts ?? "") !== (auth.oauth_redirect_hosts ?? "") || !!draft.use_shared_secrets !== !!auth.use_shared_secrets || secretsDirty;

  const regenerate = async (key: WorkspaceSecretKey) => {
    if (!mutationSupported || regenerating) return;
    setRegenerating(key);
    try {
      const value = draft.use_shared_secrets ? await regenerateSharedSecret(key as SharedSecretKey) : await regenerateWorkspaceSecret(workspaceId, key);
      setSecrets((current) => ({ ...current, [key]: value })); setLoadedSecrets((current) => ({ ...current, [key]: value }));
    } catch (error) { if (!isPrivilegedActionCancelled(error)) toast.error("重新生成失败", { description: String(error) }); } finally { setRegenerating(null); }
  };

  const save = async () => {
    if (!dirty || saving) return;
    setSaving(true);
    try {
      if (draft.type === "oauth") { validateRedirectUris(draft.oauth_redirect_uris ?? ""); validateRedirectHosts(draft.oauth_redirect_hosts ?? ""); }
      if (draft.type === "oauth" && draft.use_shared_secrets && clientIdChanged) {
        const clientId = draft.oauth_client_id.trim();
        if (!clientId) throw new Error("OAuth Client ID 不能为空");
        if (!sharedMutation) throw new Error("Web 管理面尚未开放共享 Secret 写入。");
        await setSharedSecret("oauth_client_id", clientId); setLoadedSharedClientId(clientId);
      }
      const callbackChanged = (draft.oauth_redirect_uris ?? "") !== (auth.oauth_redirect_uris ?? "") || (draft.oauth_redirect_hosts ?? "") !== (auth.oauth_redirect_hosts ?? "");
      const callbackPolicyOnly = draft.type === "oauth" && auth.type === "oauth" && callbackChanged && !clientIdChanged && !!draft.use_shared_secrets === !!auth.use_shared_secrets && !secretsDirty;
      await onSaveProfile({ ...draft }, { callbackPolicyOnly }); setLoadedSecrets({ ...secrets }); toast.success("MCP 认证配置已保存");
    } catch (error) { if (!isPrivilegedActionCancelled(error)) toast.error("认证配置保存失败", { description: String(error) }); } finally { setSaving(false); }
  };

  return <form className="grid gap-5" onSubmit={(event) => { event.preventDefault(); void save(); }}><FieldGroup><Field><FieldLabel>认证类型</FieldLabel><Select value={draft.type} onValueChange={(value) => setDraft((current) => ({ ...current, type: value ?? "oauth" }))}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="oauth">OAuth</SelectItem><SelectItem value="bearer">Bearer Token</SelectItem><SelectItem value="noauth">不启用认证</SelectItem></SelectContent></Select></Field><label className="flex items-center gap-2 text-sm"><Checkbox checked={!!draft.use_shared_secrets} onCheckedChange={(checked) => setDraft((current) => ({ ...current, use_shared_secrets: Boolean(checked) }))} />使用全局共享密钥</label>{!mutationSupported && <Alert><AlertTitle>Secret 只读</AlertTitle><AlertDescription>当前 Web 管理 API 允许修改认证配置，但未开放 Secret 重新生成。</AlertDescription></Alert>}
    {draft.type === "oauth" && <><Alert><AlertTitle>OAuth Callback</AlertTitle><AlertDescription>ChatGPT 官方 callback 会自动识别并登记；高级白名单仅用于其他 OAuth 客户端。</AlertDescription></Alert><Field><FieldLabel htmlFor="mcp-client-id">OAuth Client ID</FieldLabel><Input id="mcp-client-id" className="font-mono" readOnly={!!draft.use_shared_secrets} value={draft.oauth_client_id} onChange={(event) => setDraft((current) => ({ ...current, oauth_client_id: event.target.value }))} /></Field><details className="rounded-xl border p-3"><summary className="cursor-pointer text-sm font-medium">高级 Callback 策略</summary><div className="mt-4 grid gap-4"><Field><FieldLabel>附加精确 Callback URL</FieldLabel><Textarea className="font-mono text-xs" value={draft.oauth_redirect_uris ?? ""} onChange={(event) => setDraft((current) => ({ ...current, oauth_redirect_uris: event.target.value }))} /></Field><Field><FieldLabel>附加 Callback 域名白名单</FieldLabel><Textarea className="font-mono text-xs" placeholder={"oauth.example.com\n*.example.com"} value={draft.oauth_redirect_hosts ?? ""} onChange={(event) => setDraft((current) => ({ ...current, oauth_redirect_hosts: event.target.value }))} /></Field></div></details><Field><FieldLabel>OAuth Client Secret</FieldLabel><SecretField value={secrets.oauth_client_secret ?? ""} readOnly onRegenerate={mutationSupported ? () => regenerate("oauth_client_secret") : undefined} regenerating={regenerating === "oauth_client_secret"} /></Field><Field><FieldLabel>授权口令</FieldLabel><SecretField value={secrets.oauth_password ?? ""} readOnly onRegenerate={mutationSupported ? () => regenerate("oauth_password") : undefined} regenerating={regenerating === "oauth_password"} /></Field></>}
    {draft.type === "bearer" && <Field><FieldLabel>Bearer Token</FieldLabel><SecretField value={secrets.bearer_token ?? ""} readOnly onRegenerate={mutationSupported ? () => regenerate("bearer_token") : undefined} regenerating={regenerating === "bearer_token"} /></Field>}
    {draft.type === "noauth" && <FieldDescription>仅建议本机调试；公网暴露应使用 OAuth 或 Bearer Token。</FieldDescription>}
  </FieldGroup><div className="flex justify-end"><Button type="submit" disabled={saving || !dirty}>{saving ? "保存中…" : "保存配置"}</Button></div></form>;
}
