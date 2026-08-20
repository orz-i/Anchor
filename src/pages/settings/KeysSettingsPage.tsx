import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";

import { PageLayout } from "@/components/admin/PageLayout";
import { SecretField } from "@/components/admin/SecretField";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { isPrivilegedActionCancelled } from "@/lib/api/admin-security";
import { supportsAdminCommand } from "@/lib/api/invoke";
import {
  getSharedSecret,
  regenerateSharedSecret,
  setSharedSecret,
  type SharedSecretKey,
} from "@/lib/api/secrets";

const MCP_KEYS: { key: SharedSecretKey; label: string }[] = [
  { key: "oauth_client_id", label: "MCP OAuth Client ID" },
  { key: "bearer_token", label: "MCP Bearer Token" },
  { key: "oauth_client_secret", label: "MCP OAuth 客户端密钥" },
  { key: "oauth_password", label: "MCP 授权口令" },
  { key: "oauth_token_secret", label: "MCP Token Secret" },
];

const ACTIONS_KEYS: { key: SharedSecretKey; label: string }[] = [
  { key: "actions_api_key", label: "Actions API Key" },
  { key: "actions_oauth_client_secret", label: "Actions OAuth 客户端密钥" },
  { key: "actions_oauth_password", label: "Actions 授权口令" },
  { key: "actions_oauth_token_secret", label: "Actions Token Secret" },
];

const ALL_KEYS = [...MCP_KEYS, ...ACTIONS_KEYS];

export function KeysSettingsPage() {
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const [originals, setOriginals] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [regenerating, setRegenerating] = useState<SharedSecretKey | null>(null);
  const [mutationsSupported, setMutationsSupported] = useState(false);

  const dirty = useMemo(
    () => ALL_KEYS.some(({ key }) => secrets[key] !== undefined && secrets[key] !== originals[key]),
    [originals, secrets],
  );

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [canSet, canRegenerate, entries] = await Promise.all([
          supportsAdminCommand("set_shared_secret"),
          supportsAdminCommand("regenerate_shared_secret"),
          Promise.all(
            ALL_KEYS.map(async ({ key }) => {
              try {
                return [key, (await getSharedSecret(key)) ?? ""] as const;
              } catch {
                return [key, ""] as const;
              }
            }),
          ),
        ]);
        if (cancelled) return;
        const values = Object.fromEntries(entries);
        setMutationsSupported(canSet && canRegenerate);
        setSecrets(values);
        setOriginals(values);
      } catch (error) {
        if (!cancelled) toast.error("加载共享密钥失败", { description: String(error) });
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const regenerate = async (key: SharedSecretKey) => {
    if (!mutationsSupported || regenerating) return;
    setRegenerating(key);
    try {
      const value = await regenerateSharedSecret(key);
      setSecrets((current) => ({ ...current, [key]: value }));
      setOriginals((current) => ({ ...current, [key]: value }));
      toast.success("密钥已重新生成");
    } catch (error) {
      if (!isPrivilegedActionCancelled(error)) toast.error("重新生成失败", { description: String(error) });
    } finally {
      setRegenerating(null);
    }
  };

  const save = async () => {
    if (!mutationsSupported) return;
    setSaving(true);
    try {
      for (const { key } of ALL_KEYS) {
        if (secrets[key] !== undefined && secrets[key] !== originals[key]) {
          await setSharedSecret(key, secrets[key]);
        }
      }
      setOriginals({ ...secrets });
      toast.success("共享密钥已保存");
    } catch (error) {
      if (!isPrivilegedActionCancelled(error)) toast.error("保存失败", { description: String(error) });
    } finally {
      setSaving(false);
    }
  };

  const renderGroup = (title: string, description: string, entries: typeof MCP_KEYS) => (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent>
        <FieldGroup>
          {entries.map(({ key, label }) => (
            <Field key={key}>
              <FieldLabel htmlFor={`shared-${key}`}>{label}</FieldLabel>
              <SecretField
                value={secrets[key] ?? ""}
                disabled={loading || !mutationsSupported}
                onChange={(value) => setSecrets((current) => ({ ...current, [key]: value }))}
                onRegenerate={mutationsSupported ? () => regenerate(key) : undefined}
                regenerating={regenerating === key}
              />
            </Field>
          ))}
        </FieldGroup>
      </CardContent>
    </Card>
  );

  return (
    <PageLayout
      kicker="全局设置"
      title="共享密钥"
      description="统一管理 MCP 与 Actions 的共享认证材料。工作区启用共享密钥后，ChatGPT 可以复用同一组凭据。"
    >
      <div className="flex flex-col gap-5">
        {!mutationsSupported && !loading && (
          <Alert>
            <AlertTitle>只读模式</AlertTitle>
            <AlertDescription>当前 Web 管理 API 未开放共享密钥写入与重新生成功能。</AlertDescription>
          </Alert>
        )}
        {renderGroup("MCP 认证密钥", "MCP OAuth、Bearer 与 Token Secret。", MCP_KEYS)}
        {renderGroup("Actions 认证密钥", "ChatGPT Actions 的 API Key 与 OAuth 凭据。", ACTIONS_KEYS)}
        <div className="flex justify-end">
          <Button type="button" disabled={!mutationsSupported || !dirty || saving} onClick={() => void save()}>
            {saving ? "保存中…" : "保存更改"}
          </Button>
        </div>
      </div>
    </PageLayout>
  );
}
