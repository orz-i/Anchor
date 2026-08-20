import { useEffect, useMemo, useState } from "react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Textarea } from "@/components/ui/textarea";
import { inspectWorkspaceSkills } from "@/lib/api/workspaces";
import type { SkillInspection } from "@/lib/types";

const DEFAULT_ROOTS = ".agents/skills\n.codex/skills\nskills";
function normalizeRoots(value: string): string { const next = value.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).join("\n"); return next || DEFAULT_ROOTS; }

export function SkillServiceConfigForm({ workspaceId, enabled, roots, onSave }: { workspaceId: string; enabled: boolean; roots: string; onSave: (config: { enabled: boolean; roots: string }) => void | Promise<void> }) {
  const initial = useMemo(() => ({ enabled, roots: normalizeRoots(roots) }), [enabled, roots]);
  const [draftEnabled, setDraftEnabled] = useState(enabled);
  const [draftRoots, setDraftRoots] = useState(initial.roots);
  const [saving, setSaving] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState("");
  const [inspection, setInspection] = useState<SkillInspection | null>(null);
  useEffect(() => { setDraftEnabled(enabled); setDraftRoots(normalizeRoots(roots)); setInspection(null); setError(""); }, [enabled, roots]);
  const dirty = draftEnabled !== enabled || normalizeRoots(draftRoots) !== normalizeRoots(roots);
  const scan = async () => { if (scanning) return; setScanning(true); setError(""); try { setInspection(await inspectWorkspaceSkills(workspaceId, draftEnabled, normalizeRoots(draftRoots))); } catch (cause) { setError(String(cause)); } finally { setScanning(false); } };
  const save = async () => { if (!dirty || saving) return; setSaving(true); setError(""); try { const normalized = normalizeRoots(draftRoots); await onSave({ enabled: draftEnabled, roots: normalized }); setDraftRoots(normalized); } catch (cause) { setError(String(cause)); } finally { setSaving(false); } };
  return <form className="grid gap-4" onSubmit={(event) => { event.preventDefault(); void save(); }}><div className="flex items-start justify-between gap-4 rounded-xl border p-4"><div><p className="text-sm font-medium">通过 MCP 提供 Agent Skills</p><p className="mt-1 text-xs leading-5 text-muted-foreground">客户端可调用 list_skills、load_skill、read_skill_resource，并读取 skill://index.json。</p></div><label className="flex shrink-0 items-center gap-2 text-xs text-muted-foreground"><Checkbox checked={draftEnabled} onCheckedChange={(checked) => setDraftEnabled(Boolean(checked))} />{draftEnabled ? "已启用" : "已关闭"}</label></div><Field><FieldLabel htmlFor="skill-roots">Skill 根目录（每行一个）</FieldLabel><Textarea id="skill-roots" className="min-h-32 font-mono text-xs" disabled={!draftEnabled} value={draftRoots} onChange={(event) => setDraftRoots(event.target.value)} /><FieldDescription>相对路径以当前 workspace 为基准，支持 ~/；每个 Skill 必须包含合法 SKILL.md。</FieldDescription></Field>{error && <Alert variant="destructive"><AlertTitle>Skill 配置错误</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}{inspection && <Card><CardContent className="grid gap-3 p-4"><div className="flex items-center justify-between gap-3"><p className="text-sm font-medium">{inspection.enabled ? `发现 ${inspection.skills.length} 个 Skill` : "Skill 服务已关闭"}</p><Badge variant="outline">脚本策略 · {inspection.scriptExecutionPolicy}</Badge></div>{inspection.skills.map((skill) => <div key={`${skill.sourceId}/${skill.relativePath}`} className="rounded-lg border p-3"><div className="flex justify-between gap-3"><code className="text-xs font-semibold">{skill.name}</code><span className="text-[11px] text-muted-foreground">{skill.resources.length} resources · {skill.scripts.length} scripts</span></div><p className="mt-1 text-xs text-muted-foreground">{skill.description}</p><p className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{skill.uri}</p></div>)}{inspection.warnings.map((warning) => <p key={warning} className="text-xs text-amber-600">{warning}</p>)}<p className="border-t pt-2 font-mono text-[10px] text-muted-foreground">snapshot={inspection.snapshotMode} · catalog={inspection.catalogDigest.slice(0, 26)}…</p></CardContent></Card>}<div className="flex justify-end gap-2"><Button type="button" variant="outline" disabled={scanning} onClick={() => void scan()}>{scanning ? "扫描中…" : "扫描目录"}</Button><Button type="submit" disabled={saving || !dirty}>{saving ? "保存中…" : "保存 Skill 服务"}</Button></div></form>;
}
