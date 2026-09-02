import { useEffect, useState } from "react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  activateWorkspaceSkillPackage,
  inspectWorkspaceSkills,
  installWorkspaceSkillPackage,
  removeWorkspaceSkillPackage,
  rollbackWorkspaceSkillPackage,
  setWorkspaceSkillChannel,
} from "@/lib/api/workspaces";
import type { SkillChannel, SkillInspection } from "@/lib/types";

const CHANNELS: SkillChannel[] = ["stable", "development", "canary", "pinned"];

export function SkillServiceConfigForm({
  workspaceId,
  enabled,
  onSave,
}: {
  workspaceId: string;
  enabled: boolean;
  onSave: (config: { enabled: boolean }) => void | Promise<void>;
}) {
  const [draftEnabled, setDraftEnabled] = useState(enabled);
  const [inspection, setInspection] = useState<SkillInspection | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [installPath, setInstallPath] = useState("");
  const [channel, setChannel] = useState<SkillChannel>("development");
  const [activateOnInstall, setActivateOnInstall] = useState(false);
  const [targetName, setTargetName] = useState("");
  const [targetVersion, setTargetVersion] = useState("");

  useEffect(() => {
    setDraftEnabled(enabled);
    setInspection(null);
    setError("");
  }, [enabled, workspaceId]);

  const refresh = async () => {
    setBusy(true);
    setError("");
    try {
      setInspection(await inspectWorkspaceSkills(workspaceId, draftEnabled));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  const mutate = async (action: () => unknown | Promise<unknown>) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await action();
      setInspection(await inspectWorkspaceSkills(workspaceId, draftEnabled));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className="grid gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        void mutate(() => onSave({ enabled: draftEnabled }));
      }}
    >
      <div className="flex items-start justify-between gap-4 rounded-xl border p-4">
        <div>
          <p className="text-sm font-medium">Agent Skill package runtime</p>
          <p className="mt-1 text-xs leading-5 text-muted-foreground">
            运行时只读取 .anchor/skills 中已安装且 active 的不可变 package；workspace 源目录不会被自动扫描。
          </p>
        </div>
        <label className="flex shrink-0 items-center gap-2 text-xs text-muted-foreground">
          <Checkbox checked={draftEnabled} onCheckedChange={(checked) => setDraftEnabled(Boolean(checked))} />
          {draftEnabled ? "已启用" : "已关闭"}
        </label>
      </div>

      <div className="grid gap-3 rounded-xl border p-4">
        <Field>
          <FieldLabel htmlFor="skill-package-path">安装 workspace 内 Skill package</FieldLabel>
          <Input
            id="skill-package-path"
            placeholder="例如 skills/code-review"
            value={installPath}
            onChange={(event) => setInstallPath(event.target.value)}
          />
          <FieldDescription>目录必须包含合法 SKILL.md；安装时计算完整内容摘要并复制到 content-addressed store。</FieldDescription>
        </Field>
        <div className="flex flex-wrap items-center gap-2">
          <select
            className="h-9 rounded-md border bg-background px-3 text-sm"
            value={channel}
            onChange={(event) => setChannel(event.target.value as SkillChannel)}
          >
            {CHANNELS.map((value) => <option key={value} value={value}>{value}</option>)}
          </select>
          <label className="flex items-center gap-2 text-xs text-muted-foreground">
            <Checkbox checked={activateOnInstall} onCheckedChange={(checked) => setActivateOnInstall(Boolean(checked))} />
            安装后立即激活
          </label>
          <Button
            type="button"
            disabled={busy || !installPath.trim()}
            onClick={() => void mutate(() => installWorkspaceSkillPackage(workspaceId, installPath.trim(), channel, activateOnInstall))}
          >
            安装 package
          </Button>
        </div>
      </div>

      <div className="grid gap-3 rounded-xl border p-4">
        <p className="text-sm font-medium">Channel / activation</p>
        <div className="grid gap-2 md:grid-cols-[1fr_1fr_auto]">
          <Input placeholder="Skill name" value={targetName} onChange={(event) => setTargetName(event.target.value)} />
          <Input placeholder="declared version 或 sha256:..." value={targetVersion} onChange={(event) => setTargetVersion(event.target.value)} />
          <select
            className="h-9 rounded-md border bg-background px-3 text-sm"
            value={channel}
            onChange={(event) => setChannel(event.target.value as SkillChannel)}
          >
            {CHANNELS.map((value) => <option key={value} value={value}>{value}</option>)}
          </select>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button type="button" variant="outline" disabled={busy || !targetName.trim() || !targetVersion.trim()} onClick={() => void mutate(() => setWorkspaceSkillChannel(workspaceId, targetName.trim(), channel, targetVersion.trim()))}>设置 channel</Button>
          <Button type="button" variant="outline" disabled={busy || !targetName.trim()} onClick={() => void mutate(() => activateWorkspaceSkillPackage(workspaceId, targetName.trim(), channel))}>激活 channel</Button>
          <Button type="button" variant="outline" disabled={busy || !targetName.trim()} onClick={() => void mutate(() => rollbackWorkspaceSkillPackage(workspaceId, targetName.trim()))}>回滚</Button>
          <Button type="button" variant="destructive" disabled={busy || !targetName.trim() || !targetVersion.trim()} onClick={() => void mutate(() => removeWorkspaceSkillPackage(workspaceId, targetName.trim(), targetVersion.trim()))}>删除版本</Button>
        </div>
      </div>

      {error && <Alert variant="destructive"><AlertTitle>Skill package 操作失败</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}

      {inspection && (
        <Card>
          <CardContent className="grid gap-3 p-4">
            <div className="flex items-center justify-between gap-3">
              <p className="text-sm font-medium">{inspection.catalog.enabled ? `${inspection.catalog.skills.length} 个 active Skill` : "Skill 服务已关闭"}</p>
              <Badge variant="outline">store · {inspection.packages.storeRoot}</Badge>
            </div>
            {inspection.packages.packages.map((pkg) => (
              <div key={pkg.name} className="rounded-lg border p-3">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <code className="text-xs font-semibold">{pkg.name}</code>
                  <span className="text-[11px] text-muted-foreground">{pkg.versions.length} versions · rollback {pkg.rollbackDepth}</span>
                </div>
                <p className="mt-1 font-mono text-[10px] text-muted-foreground">active={pkg.activeChannel ?? "none"} · {pkg.activeDigest?.slice(0, 26) ?? "none"}</p>
                <div className="mt-2 flex flex-wrap gap-1">{Object.entries(pkg.channels).map(([key, digest]) => <Badge key={key} variant="secondary">{key} · {digest.slice(0, 14)}…</Badge>)}</div>
              </div>
            ))}
            {inspection.catalog.warnings.map((warning) => <p key={warning} className="text-xs text-amber-600">{warning}</p>)}
            <p className="border-t pt-2 font-mono text-[10px] text-muted-foreground">snapshot={inspection.catalog.snapshotMode} · catalog={inspection.catalog.catalogDigest.slice(0, 26)}…</p>
          </CardContent>
        </Card>
      )}

      <div className="flex justify-end gap-2">
        <Button type="button" variant="outline" disabled={busy} onClick={() => void refresh()}>{busy ? "处理中…" : "刷新 package 状态"}</Button>
        <Button type="submit" disabled={busy || draftEnabled === enabled}>保存 Skill 服务</Button>
      </div>
    </form>
  );
}
