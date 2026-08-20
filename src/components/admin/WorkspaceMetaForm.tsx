import { FolderInput, FolderOpen } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { openWorkspaceDirectory } from "@/lib/api/workspaces";
import { open } from "@/lib/platform/dialog";

export function WorkspaceMetaForm({ name, path, onSave, onUpdatePath }: { name: string; path: string; onSave: (name: string) => void | Promise<void>; onUpdatePath: (path: string) => void | Promise<void> }) {
  const [draftName, setDraftName] = useState(name);
  const [saving, setSaving] = useState(false);
  const [opening, setOpening] = useState(false);
  const [updatingPath, setUpdatingPath] = useState(false);

  useEffect(() => setDraftName(name), [name]);
  const dirty = draftName.trim().length > 0 && draftName.trim() !== name;

  const save = async () => {
    if (!dirty || saving) return;
    setSaving(true);
    try { await onSave(draftName.trim()); } finally { setSaving(false); }
  };

  const openDirectory = async () => {
    setOpening(true);
    try { await openWorkspaceDirectory(path); } catch (error) { toast.error("无法打开目录", { description: String(error) }); } finally { setOpening(false); }
  };

  const updateDirectory = async () => {
    setUpdatingPath(true);
    try {
      const selected = await open({ directory: true, multiple: false, defaultPath: path || undefined });
      if (!selected || Array.isArray(selected)) return;
      const next = selected.trim().replace(/[\\/]+$/, "");
      const current = path.trim().replace(/[\\/]+$/, "");
      if (next && next !== current) await onUpdatePath(next);
    } catch (error) {
      toast.error("无法更新目录", { description: String(error) });
    } finally {
      setUpdatingPath(false);
    }
  };

  return (
    <form className="grid gap-3 xl:grid-cols-[minmax(12rem,0.7fr)_minmax(20rem,1.3fr)_auto] xl:items-end" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <Field><FieldLabel htmlFor="workspace-name">工作区名称</FieldLabel><Input id="workspace-name" value={draftName} onChange={(event) => setDraftName(event.target.value)} /></Field>
      <Field>
        <FieldLabel>路径</FieldLabel>
        <div className="flex min-w-0 gap-2">
          <Input readOnly title={path} value={path} className="min-w-0 flex-1 font-mono text-xs" />
          <Button type="button" variant="outline" disabled={opening || !path} onClick={() => void openDirectory()}><FolderOpen data-icon="inline-start" />{opening ? "打开中…" : "打开目录"}</Button>
          <Button type="button" variant="outline" disabled={updatingPath} onClick={() => void updateDirectory()}><FolderInput data-icon="inline-start" />{updatingPath ? "选择中…" : "更新目录"}</Button>
        </div>
      </Field>
      <Button type="submit" disabled={!dirty || saving}>{saving ? "保存中…" : "保存名称"}</Button>
    </form>
  );
}
