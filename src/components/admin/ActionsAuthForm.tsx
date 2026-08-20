import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import { CopyField } from "@/components/admin/CopyField";
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
import { getSharedSecret, getWorkspaceSecret, regenerateSharedSecret, regenerateWorkspaceSecret, type SharedSecretKey, type WorkspaceSecretKey } from "@/lib/api/secrets";
import type { ActionsAuthDraft } from "@/lib/types";

interface ActionsAuthProps {
  workspaceId: string;
  authType: string;
  oauthClientId: string;
  oauthRedirectUris: string;
  oauthRedirectHosts: string;
  oauthScopes: string;
  openapiUrl: string;
  privacyUrl: string;
  oauthAuthorizeUrl: string;
  oauthTokenUrl: string;
  useSharedSecrets?: boolean;
  onSave: (draft: ActionsAuthDraft, options: { callbackPolicyOnly: boolean }) => void | Promise<void>;
}

type SecretName = "actions_api_key" | "actions_oauth_client_secret" | "actions_oauth_password" | "actions_oauth_token_secret";

export function ActionsAuthForm(props: ActionsAuthProps) {
  const initial = useMemo(() => ({ authType: props.authType, oauthClientId: props.oauthClientId, oauthRedirectUris: props.oauthRedirectUris, oauthRedirectHosts: props.oauthRedirectHosts, oauthScopes: props.oauthScopes, useSharedSecrets: !!props.useSharedSecrets }), [props.authType, props.oauthClientId, props.oauthRedirectHosts, props.oauthRedirectUris, props.oauthScopes, props.useSharedSecrets]);
  const [draft, setDraft] = useState(initial);
  const [secrets, setSecrets] = useState<Record<SecretName, string>>({ actions_api_key: "", actions_oauth_client_secret: "", actions_oauth_password: "", actions_oauth_token_secret: "" });
  const [loading, setLoading] = useState(true);
  const [regenerating, setRegenerating] = useState<SecretName | null>(null);
  const [saving, setSaving] = useState(false);
  const [workspaceMutation, setWorkspaceMutation] = useState(false);
  const [sharedMutation, setSharedMutation] = useState(false);
  const sequence = useRef(0);

  useEffect(() => setDraft(initial), [initial]);
  useEffect(() => { void (async () => { const [workspace, shared] = await Promise.all([supportsAdminCommand("regenerate_workspace_secret"), supportsAdminCommand("regenerate_shared_secret")]); setWorkspaceMutation(workspace); setSharedMutation(shared); })(); }, []);
  useEffect(() => {
    const seq = ++sequence.current;
    void (async () => {
      setLoading(true);
      const shared = draft.useSharedSecrets;
      const read = (key: SecretName) => shared ? getSharedSecret(key as SharedSecretKey) : getWorkspaceSecret(props.workspaceId, key as WorkspaceSecretKey);
      const [apiKey, clientSecret, password, tokenSecret] = await Promise.all([read("actions_api_key"), read("actions_oauth_client_secret"), read("actions_oauth_password"), read("actions_oauth_token_secret")]);
      if (seq !== sequence.current) return;
      setSecrets({ actions_api_key: apiKey ?? "", actions_oauth_client_secret: clientSecret ?? "", actions_oauth_password: password ?? "", actions_oauth_token_secret: tokenSecret ?? "" });
    })().catch((error) => { if (seq === sequence.current) toast.error("加载 Actions Secret 失败", { description: String(error) }); }).finally(() => { if (seq === sequence.current) setLoading(false); });
  }, [draft.useSharedSecrets, props.workspaceId]);

  const mutationSupported = draft.useSharedSecrets ? sharedMutation : workspaceMutation;
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);

  const regenerate = async (key: SecretName) => {
    if (!mutationSupported || regenerating) return;
    setRegenerating(key);
    try {
      const value = draft.useSharedSecrets ? await regenerateSharedSecret(key as SharedSecretKey) : await regenerateWorkspaceSecret(props.workspaceId, key as WorkspaceSecretKey);
      setSecrets((current) => ({ ...current, [key]: value }));
    } catch (error) { if (!isPrivilegedActionCancelled(error)) toast.error("重新生成失败", { description: String(error) }); } finally { setRegenerating(null); }
  };

  const save = async () => {
    if (!dirty || saving) return;
    setSaving(true);
    try {
      if (draft.authType === "oauth") { validateRedirectUris(draft.oauthRedirectUris); validateRedirectHosts(draft.oauthRedirectHosts); }
      const callbackChanged = draft.oauthRedirectUris !== props.oauthRedirectUris || draft.oauthRedirectHosts !== props.oauthRedirectHosts;
      const callbackPolicyOnly = draft.authType === "oauth" && props.authType === "oauth" && callbackChanged && draft.oauthClientId === props.oauthClientId && draft.oauthScopes === props.oauthScopes && draft.useSharedSecrets === !!props.useSharedSecrets;
      await props.onSave({ authType: draft.authType, oauthClientId: draft.oauthClientId.trim(), oauthRedirectUris: draft.oauthRedirectUris.trim(), oauthRedirectHosts: draft.oauthRedirectHosts.trim(), oauthScopes: draft.oauthScopes.trim(), useSharedSecrets: draft.useSharedSecrets }, { callbackPolicyOnly });
      toast.success("Actions 认证配置已保存");
    } catch (error) { if (!isPrivilegedActionCancelled(error)) toast.error("认证配置保存失败", { description: String(error) }); } finally { setSaving(false); }
  };

  return <form className="grid gap-5" onSubmit={(event) => { event.preventDefault(); void save(); }}><FieldGroup><Field><FieldLabel>认证方式</FieldLabel><Select value={draft.authType} onValueChange={(value) => setDraft((current) => ({ ...current, authType: value ?? "api_key" }))}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="api_key">API Key / Bearer</SelectItem><SelectItem value="none">不启用认证</SelectItem><SelectItem value="oauth">OAuth</SelectItem></SelectContent></Select></Field><label className="flex items-center gap-2 text-sm"><Checkbox checked={draft.useSharedSecrets} onCheckedChange={(checked) => setDraft((current) => ({ ...current, useSharedSecrets: Boolean(checked) }))} />使用全局共享密钥</label>{!mutationSupported && <Alert><AlertTitle>Secret 只读</AlertTitle><AlertDescription>当前 Web 管理 API 未开放 Actions Secret 重新生成。</AlertDescription></Alert>}
    {draft.authType === "api_key" && <><Field><FieldLabel>API Key（Bearer）</FieldLabel><SecretField value={loading ? "" : secrets.actions_api_key} placeholder={loading ? "加载中…" : undefined} readOnly onRegenerate={mutationSupported ? () => regenerate("actions_api_key") : undefined} regenerating={regenerating === "actions_api_key"} /></Field><FieldDescription>GPT Actions 认证选择 API Key → Bearer。</FieldDescription></>}
    {draft.authType === "oauth" && <><Alert><AlertTitle>OAuth Callback</AlertTitle><AlertDescription>ChatGPT 官方 callback 自动识别并登记，无需中途返回控制面填写 Callback URL。</AlertDescription></Alert><Field><FieldLabel htmlFor="actions-client-id">OAuth Client ID</FieldLabel><Input id="actions-client-id" className="font-mono" value={draft.oauthClientId} onChange={(event) => setDraft((current) => ({ ...current, oauthClientId: event.target.value }))} /></Field><details className="rounded-xl border p-3"><summary className="cursor-pointer text-sm font-medium">高级 Callback 策略</summary><div className="mt-4 grid gap-4"><Field><FieldLabel>附加精确 Callback URL</FieldLabel><Textarea className="font-mono text-xs" value={draft.oauthRedirectUris} onChange={(event) => setDraft((current) => ({ ...current, oauthRedirectUris: event.target.value }))} /></Field><Field><FieldLabel>附加 Callback 域名白名单</FieldLabel><Textarea className="font-mono text-xs" value={draft.oauthRedirectHosts} placeholder={"oauth.example.com\n*.example.com"} onChange={(event) => setDraft((current) => ({ ...current, oauthRedirectHosts: event.target.value }))} /></Field></div></details><Field><FieldLabel>OAuth Client Secret</FieldLabel><SecretField value={secrets.actions_oauth_client_secret} readOnly onRegenerate={mutationSupported ? () => regenerate("actions_oauth_client_secret") : undefined} regenerating={regenerating === "actions_oauth_client_secret"} /></Field><Field><FieldLabel>OAuth Password</FieldLabel><SecretField value={secrets.actions_oauth_password} readOnly onRegenerate={mutationSupported ? () => regenerate("actions_oauth_password") : undefined} regenerating={regenerating === "actions_oauth_password"} /></Field><Field><FieldLabel>OAuth Token Secret</FieldLabel><SecretField value={secrets.actions_oauth_token_secret} readOnly onRegenerate={mutationSupported ? () => regenerate("actions_oauth_token_secret") : undefined} regenerating={regenerating === "actions_oauth_token_secret"} /></Field><div className="grid gap-4 md:grid-cols-2"><CopyField label="Authorization URL" value={props.oauthAuthorizeUrl} /><CopyField label="Token URL" value={props.oauthTokenUrl} /></div><Field><FieldLabel htmlFor="actions-scope">Scope</FieldLabel><Input id="actions-scope" value={draft.oauthScopes} onChange={(event) => setDraft((current) => ({ ...current, oauthScopes: event.target.value }))} /><FieldDescription>空格分隔；GPT 编辑器 Token 交换方式使用默认即可。</FieldDescription></Field><div className="grid gap-4 md:grid-cols-2"><CopyField label="OpenAPI Schema URL" value={props.openapiUrl} /><CopyField label="隐私政策 URL" value={props.privacyUrl} /></div></>}
    {draft.authType === "none" && <FieldDescription>仅建议本机调试；公网暴露请使用 API Key 或 OAuth。</FieldDescription>}
  </FieldGroup><div className="flex justify-end"><Button type="submit" disabled={saving || !dirty}>{saving ? "保存中…" : "保存配置"}</Button></div></form>;
}
