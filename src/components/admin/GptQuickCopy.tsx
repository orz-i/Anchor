import { useEffect, useState } from "react";

import { CopyField } from "@/components/admin/CopyField";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { getSharedSecret, getWorkspaceSecret } from "@/lib/api/secrets";
import type { WorkspaceProfile } from "@/lib/types";

export function GptQuickCopy({
  workspaceId,
  profile,
  publicMcpEndpoint = "",
}: {
  workspaceId: string;
  profile: WorkspaceProfile;
  publicMcpEndpoint?: string;
}) {
  const [loading, setLoading] = useState(true);
  const [secrets, setSecrets] = useState<Record<string, string>>({});

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      setLoading(true);
      try {
        const next: Record<string, string> = {};
        const shared = !!profile.auth.use_shared_secrets;
        const read = (
          workspaceKey: Parameters<typeof getWorkspaceSecret>[1],
          sharedKey: Parameters<typeof getSharedSecret>[0],
        ) => shared ? getSharedSecret(sharedKey) : getWorkspaceSecret(workspaceId, workspaceKey);
        if (profile.auth.type === "oauth") {
          next.oauth_client_id = shared
            ? (await getSharedSecret("oauth_client_id")) ?? ""
            : profile.auth.oauth_client_id;
          next.oauth_client_secret = (await read("oauth_client_secret", "oauth_client_secret")) ?? "";
          next.oauth_password = (await read("oauth_password", "oauth_password")) ?? "";
        } else if (profile.auth.type === "bearer") {
          next.bearer_token = (await read("bearer_token", "bearer_token")) ?? "";
        }
        if (!cancelled) setSecrets(next);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [profile.auth.oauth_client_id, profile.auth.type, profile.auth.use_shared_secrets, workspaceId]);

  return <Card><CardHeader><CardTitle>GPT 配置</CardTitle><CardDescription>复制到 ChatGPT → 设置 → 连接器 / MCP</CardDescription></CardHeader><CardContent className="grid gap-4"><CopyField label="公网 MCP 地址" value={publicMcpEndpoint} hint="GPT 连接器里填这个 URL" />{profile.auth.type === "oauth" ? <><CopyField label="OAuth Client ID" value={secrets.oauth_client_id ?? profile.auth.oauth_client_id} loading={loading} /><CopyField label="OAuth Client Secret" value={secrets.oauth_client_secret ?? ""} loading={loading} /><CopyField label="授权口令" value={secrets.oauth_password ?? ""} hint="ChatGPT 首次授权时输入" loading={loading} /></> : profile.auth.type === "bearer" ? <CopyField label="Bearer Token" value={secrets.bearer_token ?? ""} loading={loading} /> : <p className="text-xs text-muted-foreground">当前未启用认证，仅本机调试可用。</p>}</CardContent></Card>;
}
