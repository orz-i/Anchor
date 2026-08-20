import { useEffect, useMemo, useState } from "react";

import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export interface RuntimePolicyDraft {
  toolProfile: string;
  permissionMode: string;
  preferredShell: string;
  allowedCommands: string;
  workspaceLocalEntries: boolean;
  workspaceScriptExtensions: string;
  externalPaidCommandsEnabled: boolean;
  externalPaidMaxRunsPerDay: number;
  externalPaidMaxDurationSeconds: number;
}

function canonicalProfile(value: string) { return value === "advanced" || value === "read-only" ? value : "core"; }

export function RuntimePolicyForm(props: RuntimePolicyDraft & { onSave: (draft: RuntimePolicyDraft) => void | Promise<void> }) {
  const initial = useMemo<RuntimePolicyDraft>(() => ({
    toolProfile: canonicalProfile(props.toolProfile),
    permissionMode: props.permissionMode,
    preferredShell: props.preferredShell,
    allowedCommands: props.allowedCommands,
    workspaceLocalEntries: props.workspaceLocalEntries,
    workspaceScriptExtensions: props.workspaceScriptExtensions,
    externalPaidCommandsEnabled: props.externalPaidCommandsEnabled,
    externalPaidMaxRunsPerDay: props.externalPaidMaxRunsPerDay,
    externalPaidMaxDurationSeconds: props.externalPaidMaxDurationSeconds,
  }), [props.toolProfile, props.permissionMode, props.preferredShell, props.allowedCommands, props.workspaceLocalEntries, props.workspaceScriptExtensions, props.externalPaidCommandsEnabled, props.externalPaidMaxRunsPerDay, props.externalPaidMaxDurationSeconds]);
  const [draft, setDraft] = useState<RuntimePolicyDraft>(initial);
  const [saving, setSaving] = useState(false);
  useEffect(() => setDraft(initial), [initial]);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);
  const update = <K extends keyof RuntimePolicyDraft>(key: K, value: RuntimePolicyDraft[K]) => setDraft((current) => ({ ...current, [key]: value }));

  const save = async () => {
    if (!dirty || saving) return;
    setSaving(true);
    try { await props.onSave({ ...draft, allowedCommands: draft.allowedCommands.trim(), workspaceScriptExtensions: draft.workspaceScriptExtensions.trim(), externalPaidMaxRunsPerDay: Math.max(1, Math.floor(draft.externalPaidMaxRunsPerDay)), externalPaidMaxDurationSeconds: Math.max(1, Math.floor(draft.externalPaidMaxDurationSeconds)) }); } finally { setSaving(false); }
  };

  return <form className="grid gap-5" onSubmit={(event) => { event.preventDefault(); void save(); }}><FieldGroup>
    <Field><FieldLabel>工具档位</FieldLabel><Select value={draft.toolProfile} onValueChange={(value) => update("toolProfile", value ?? "core")}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="core">核心工具</SelectItem><SelectItem value="advanced">完整工具</SelectItem><SelectItem value="read-only">只读工具</SelectItem></SelectContent></Select></Field>
    <Field><FieldLabel>Windows 默认 Shell</FieldLabel><Select value={draft.preferredShell} onValueChange={(value) => update("preferredShell", value ?? "auto")}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="auto">自动 / 直接执行</SelectItem><SelectItem value="pwsh">PowerShell 7 (pwsh)</SelectItem><SelectItem value="powershell">Windows PowerShell</SelectItem><SelectItem value="cmd">cmd.exe</SelectItem></SelectContent></Select><FieldDescription>仅在 exec_command 未显式指定 shell 时生效。</FieldDescription></Field>
    <div className="grid gap-4 rounded-xl border p-4"><label className="flex items-center gap-2 text-sm font-medium"><Checkbox checked={draft.externalPaidCommandsEnabled} onCheckedChange={(checked) => update("externalPaidCommandsEnabled", Boolean(checked))} />允许执行已识别的真实付费命令</label><div className="grid gap-4 sm:grid-cols-2"><Field><FieldLabel htmlFor="paid-runs">每日最大运行次数</FieldLabel><Input id="paid-runs" type="number" min={1} value={draft.externalPaidMaxRunsPerDay} onChange={(event) => update("externalPaidMaxRunsPerDay", Number(event.target.value))} /></Field><Field><FieldLabel htmlFor="paid-duration">单次最长秒数</FieldLabel><Input id="paid-duration" type="number" min={1} max={3600} value={draft.externalPaidMaxDurationSeconds} onChange={(event) => update("externalPaidMaxDurationSeconds", Number(event.target.value))} /></Field></div><p className="text-xs leading-5 text-muted-foreground">该权限只能由受信任控制面启用；项目仍可通过 .anchor/command-policy.yml 进一步收紧。</p></div>
    <Field><FieldLabel htmlFor="allowed-commands">系统命令（逗号分隔）</FieldLabel><Input id="allowed-commands" className="font-mono" value={draft.allowedCommands} onChange={(event) => update("allowedCommands", event.target.value)} /></Field>
    <label className="flex items-center gap-2 text-sm"><Checkbox checked={draft.workspaceLocalEntries} onCheckedChange={(checked) => update("workspaceLocalEntries", Boolean(checked))} />允许执行 Workspace 内本地入口</label>
    <Field><FieldLabel htmlFor="script-exts">本地脚本扩展名（逗号分隔）</FieldLabel><Input id="script-exts" className="font-mono" disabled={!draft.workspaceLocalEntries} value={draft.workspaceScriptExtensions} onChange={(event) => update("workspaceScriptExtensions", event.target.value)} /></Field>
    <Field><FieldLabel>权限模式</FieldLabel><Select value={draft.permissionMode} onValueChange={(value) => update("permissionMode", value ?? "trusted")}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="trusted">受信任</SelectItem><SelectItem value="safe">安全受限</SelectItem><SelectItem value="dangerous">完全放开</SelectItem></SelectContent></Select><FieldDescription>dangerous 只能由操作者在此控制面启用，模型参数不能作为用户批准凭证。</FieldDescription></Field>
  </FieldGroup><div className="flex justify-end"><Button type="submit" disabled={saving || !dirty}>{saving ? "保存中…" : "保存策略"}</Button></div></form>;
}
