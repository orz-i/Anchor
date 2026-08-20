import { useEffect, useState } from "react";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { PageLayout } from "@/components/admin/PageLayout";
import { SecretField } from "@/components/admin/SecretField";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { isPrivilegedActionCancelled } from "@/lib/api/admin-security";
import { supportsAdminCommand } from "@/lib/api/invoke";
import {
  deleteFrpProfile,
  listFrpProfiles,
  saveFrpProfile,
  type FrpProfileDto,
} from "@/lib/api/settings";

export function FrpSettingsPage() {
  const [profiles, setProfiles] = useState<FrpProfileDto[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [server, setServer] = useState("");
  const [serverPort, setServerPort] = useState(7000);
  const [token, setToken] = useState("");
  const [tokenMutationSupported, setTokenMutationSupported] = useState(false);

  const refresh = async () => {
    setLoading(true);
    try {
      setProfiles(await listFrpProfiles());
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const supported = await supportsAdminCommand("set_frp_profile_token");
        if (!cancelled) setTokenMutationSupported(supported);
        await refresh();
      } catch (error) {
        if (!cancelled) toast.error("加载 FRP 配置失败", { description: String(error) });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const reset = () => {
    setEditingId(null);
    setName("");
    setServer("");
    setServerPort(7000);
    setToken("");
  };

  const edit = (profile: FrpProfileDto) => {
    setEditingId(profile.id);
    setName(profile.name);
    setServer(profile.server);
    setServerPort(profile.serverPort);
    setToken("");
  };

  const save = async () => {
    if (!name.trim() || !server.trim()) {
      toast.warning("请填写配置名称和服务器地址");
      return;
    }
    setSaving(true);
    try {
      await saveFrpProfile(
        { id: editingId ?? "", name: name.trim(), server: server.trim(), serverPort },
        token.trim() || undefined,
      );
      reset();
      await refresh();
      toast.success("FRP 配置已保存");
    } catch (error) {
      if (!isPrivilegedActionCancelled(error)) toast.error("保存失败", { description: String(error) });
    } finally {
      setSaving(false);
    }
  };

  const remove = async (profile: FrpProfileDto) => {
    if (!window.confirm(`确定删除 FRP 配置「${profile.name}」？`)) return;
    try {
      await deleteFrpProfile(profile.id);
      if (editingId === profile.id) reset();
      await refresh();
      toast.success("FRP 配置已删除");
    } catch (error) {
      if (!isPrivilegedActionCancelled(error)) toast.error("删除失败", { description: String(error) });
    }
  };

  return (
    <PageLayout
      kicker="全局设置"
      title="FRP 配置"
      description="管理 FRP 控制服务器、端口与 Token。工作区只保存配置引用、子域名和公网 URL。"
    >
      <div className="grid gap-5 xl:grid-cols-[minmax(20rem,0.8fr)_minmax(24rem,1.2fr)]">
        <Card>
          <CardHeader>
            <CardTitle>{editingId ? "编辑配置" : "新建配置"}</CardTitle>
            <CardDescription>控制服务器可使用 IP 或 DNS-only 专用域名。</CardDescription>
          </CardHeader>
          <CardContent>
            <form className="flex flex-col gap-5" onSubmit={(event) => { event.preventDefault(); void save(); }}>
              <FieldGroup>
                <Field>
                  <FieldLabel htmlFor="frp-name">名称</FieldLabel>
                  <Input id="frp-name" value={name} placeholder="公司 FRP" onChange={(event) => setName(event.target.value)} />
                </Field>
                <Field>
                  <FieldLabel htmlFor="frp-server">控制服务器地址</FieldLabel>
                  <Input id="frp-server" className="font-mono" value={server} placeholder="frps-control.example.com" onChange={(event) => setServer(event.target.value)} />
                </Field>
                <Field>
                  <FieldLabel htmlFor="frp-port">端口</FieldLabel>
                  <Input id="frp-port" type="number" min={1} max={65535} value={serverPort} onChange={(event) => setServerPort(Number(event.target.value))} />
                </Field>
                <Field>
                  <FieldLabel>Token</FieldLabel>
                  <SecretField value={token} disabled={!tokenMutationSupported} placeholder="frp auth token" onChange={setToken} />
                  <FieldDescription>
                    {tokenMutationSupported
                      ? editingId
                        ? "留空保持不变；写入会触发目标绑定的二次确认。"
                        : "写入会触发目标绑定的二次确认。"
                      : "当前 Web 管理 API 仅允许编辑非敏感 FRP 元数据。"}
                  </FieldDescription>
                </Field>
              </FieldGroup>
              <div className="flex gap-2">
                <Button type="submit" disabled={saving}>
                  <Plus data-icon="inline-start" />
                  {saving ? "保存中…" : editingId ? "更新" : "添加"}
                </Button>
                {editingId && <Button type="button" variant="outline" onClick={reset}>取消</Button>}
              </div>
            </form>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>已保存的配置</CardTitle>
            <CardDescription>这些配置可被多个工作区复用。</CardDescription>
          </CardHeader>
          <CardContent>
            {loading ? (
              <p className="text-sm text-muted-foreground">加载中…</p>
            ) : profiles.length === 0 ? (
              <Empty>
                <EmptyHeader>
                  <EmptyTitle>暂无 FRP 配置</EmptyTitle>
                  <EmptyDescription>在左侧创建第一条服务器配置。</EmptyDescription>
                </EmptyHeader>
              </Empty>
            ) : (
              <div className="flex flex-col gap-2">
                {profiles.map((profile) => (
                  <div key={profile.id} className="flex items-center justify-between gap-4 rounded-xl border bg-card p-3">
                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <p className="truncate text-sm font-medium">{profile.name}</p>
                        <Badge variant="outline">Token {profile.hasToken ? "已配置" : "未配置"}</Badge>
                      </div>
                      <p className="mt-1 truncate font-mono text-xs text-muted-foreground">{profile.server}:{profile.serverPort}</p>
                    </div>
                    <div className="flex shrink-0 gap-1">
                      <Button type="button" variant="ghost" size="icon" aria-label={`编辑 ${profile.name}`} onClick={() => edit(profile)}><Pencil /></Button>
                      <Button type="button" variant="ghost" size="icon" aria-label={`删除 ${profile.name}`} onClick={() => void remove(profile)}><Trash2 /></Button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </PageLayout>
  );
}
