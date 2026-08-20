import { useEffect, useMemo, useState } from "react";

import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export interface ActionsPolicyDraft { allowedCommands: string; maxPatchBytes: number; permissionMode: string; }

export function ActionsPolicyForm({ allowedCommands, maxPatchBytes, permissionMode, onSave }: ActionsPolicyDraft & { onSave: (draft: ActionsPolicyDraft) => void | Promise<void> }) {
  const initial = useMemo(() => ({ allowedCommands, maxPatchBytes, permissionMode }), [allowedCommands, maxPatchBytes, permissionMode]);
  const [draft, setDraft] = useState(initial);
  const [saving, setSaving] = useState(false);
  useEffect(() => setDraft(initial), [initial]);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);
  const save = async () => { if (!dirty || saving) return; setSaving(true); try { await onSave({ ...draft, allowedCommands: draft.allowedCommands.trim() }); } finally { setSaving(false); } };
  return <form className="grid gap-5" onSubmit={(event) => { event.preventDefault(); void save(); }}><FieldGroup><Field><FieldLabel htmlFor="actions-commands">允许命令（逗号分隔）</FieldLabel><Input id="actions-commands" className="font-mono" value={draft.allowedCommands} onChange={(event) => setDraft((current) => ({ ...current, allowedCommands: event.target.value }))} /></Field><Field><FieldLabel htmlFor="actions-patch">最大 Patch 字节数</FieldLabel><Input id="actions-patch" type="number" min={1024} max={5000000} value={draft.maxPatchBytes} onChange={(event) => setDraft((current) => ({ ...current, maxPatchBytes: Number(event.target.value) }))} /></Field><Field><FieldLabel>权限模式</FieldLabel><Select value={draft.permissionMode} onValueChange={(value) => setDraft((current) => ({ ...current, permissionMode: value ?? "trusted" }))}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="trusted">受信任</SelectItem><SelectItem value="safe">安全受限</SelectItem><SelectItem value="dangerous">完全放开</SelectItem></SelectContent></Select><FieldDescription>作用于 Actions gateway 的 exec_command 白名单与 apply_patch 大小限制。</FieldDescription></Field></FieldGroup><div className="flex justify-end"><Button type="submit" disabled={saving || !dirty}>{saving ? "保存中…" : "保存策略"}</Button></div></form>;
}
