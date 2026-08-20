import { useEffect, useState } from "react";

import { CopyField } from "@/components/admin/CopyField";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { getSharedSecret, getWorkspaceSecret } from "@/lib/api/secrets";
import type { FrpProfileDto } from "@/lib/api/settings";
import type { WorkspaceProfile } from "@/lib/types";
import { actionsConfig, actionsOAuthAuthorizeUrl, actionsOAuthTokenUrl, actionsOpenApiUrl, actionsPrivacyUrl } from "@/lib/types";

export function GptQuickCopy({ workspaceId, service, profile, publicMcpEndpoint = "", frpProfiles = [] }: { workspaceId: string; service: "mcp" | "actions"; profile: WorkspaceProfile; publicMcpEndpoint?: string; frpProfiles?: FrpProfileDto[] }) {
  const [loading, setLoading] = useState(true);
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const actions = actionsConfig(profile);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      setLoading(true);
      try {
        const next: Record<string, string> = {};
        if (service === "mcp") {
          const shared = !!profile.auth.use_shared_secrets;
          const read = (workspaceKey: Parameters<typeof getWorkspaceSecret>[1], sharedKey: Parameters<typeof getSharedSecret>[0]) => shared ? getSharedSecret(sharedKey) : getWorkspaceSecret(workspaceId, workspaceKey);
          if (profile.auth.type === "oauth") {
            next.oauth_client_id = shared ? (await getSharedSecret("oauth_client_id")) ?? "" : profile.auth.oauth_client_id;
            next.oauth_client_secret = (await read("oauth_client_secret", "oauth_client_secret")) ?? "";
            next.oauth_password = (await read("oauth_password", "oauth_password")) ?? "";
          } else if (profile.auth.type === "bearer") next.bearer_token = (await read("bearer_token", "bearer_token")) ?? "";
        } else {
          const shared = !!actions.use_shared_secrets;
          const read = (workspaceKey: Parameters<typeof getWorkspaceSecret>[1], sharedKey: Parameters<typeof getSharedSecret>[0]) => shared ? getSharedSecret(sharedKey) : getWorkspaceSecret(workspaceId, workspaceKey);
          if (actions.auth_type === "api_key") next.actions_api_key = (await read("actions_api_key", "actions_api_key")) ?? "";
          if (actions.auth_type === "oauth") next.actions_oauth_client_secret = (await read("actions_oauth_client_secret", "actions_oauth_client_secret")) ?? "";
        }
        if (!cancelled) setSecrets(next);
      } finally { if (!cancelled) setLoading(false); }
    })();
    return () => { cancelled = true; };
  }, [actions.auth_type, actions.use_shared_secrets, profile.auth.oauth_client_id, profile.auth.type, profile.auth.use_shared_secrets, service, workspaceId]);

  return <Card><CardHeader><CardTitle>GPT 配置</CardTitle><CardDescription>{service === "mcp" ? "复制到 ChatGPT → 设置 → 连接器 / MCP" : "复制到 GPT 编辑器 → Actions"}</CardDescription></CardHeader><CardContent className="grid gap-4">{service === "mcp" ? <><CopyField label="公网 MCP 地址" value={publicMcpEndpoint} hint="GPT 连接器里填这个 URL" />{profile.auth.type === "oauth" ? <><CopyField label="OAuth Client ID" value={secrets.oauth_client_id ?? profile.auth.oauth_client_id} loading={loading} /><CopyField label="OAuth Client Secret" value={secrets.oauth_client_secret ?? ""} loading={loading} /><CopyField label="授权口令" value={secrets.oauth_password ?? ""} hint="ChatGPT 首次授权时输入" loading={loading} /></> : profile.auth.type === "bearer" ? <CopyField label="Bearer Token" value={secrets.bearer_token ?? ""} loading={loading} /> : <p className="text-xs text-muted-foreground">当前未启用认证，仅本机调试可用。</p>}</> : <><CopyField label="OpenAPI Schema URL" value={actionsOpenApiUrl(profile, frpProfiles)} hint="Actions → Import from URL" /><CopyField label="隐私政策 URL" value={actionsPrivacyUrl(profile, frpProfiles)} />{actions.auth_type === "api_key" ? <CopyField label="API Key（Bearer）" value={secrets.actions_api_key ?? ""} loading={loading} /> : actions.auth_type === "oauth" ? <><CopyField label="OAuth Client ID" value={actions.oauth_client_id ?? ""} /><CopyField label="OAuth Client Secret" value={secrets.actions_oauth_client_secret ?? ""} loading={loading} /><CopyField label="Authorization URL" value={actionsOAuthAuthorizeUrl(profile, frpProfiles)} /><CopyField label="Token URL" value={actionsOAuthTokenUrl(profile, frpProfiles)} /><CopyField label="Scope" value={actions.oauth_scopes ?? ""} /></> : <p className="text-xs text-muted-foreground">当前未启用认证，公网暴露请改用 API Key 或 OAuth。</p>}</>}</CardContent></Card>;
}
