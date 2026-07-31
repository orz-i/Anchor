<script lang="ts">
  import { inspectWorkspaceSkills } from "$lib/api/workspaces";
  import type { SkillInspection } from "$lib/types";

  const DEFAULT_ROOTS = ".agents/skills\n.codex/skills\nskills";

  interface SkillServiceConfig {
    enabled: boolean;
    roots: string;
  }

  interface Props {
    workspaceId: string;
    enabled: boolean;
    roots: string;
    onSave: (config: SkillServiceConfig) => void | Promise<void>;
  }

  let { workspaceId, enabled, roots, onSave }: Props = $props();
  let draftEnabled = $state(true);
  let draftRoots = $state("");
  let saving = $state(false);
  let scanning = $state(false);
  let error = $state("");
  let inspection = $state<SkillInspection | null>(null);

  const dirty = $derived(draftEnabled !== enabled || normalizeRoots(draftRoots) !== normalizeRoots(roots));

  $effect(() => {
    draftEnabled = enabled;
    draftRoots = roots;
    inspection = null;
    error = "";
  });

  function normalizeRoots(value: string): string {
    const normalized = value
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean)
      .join("\n");
    return normalized || DEFAULT_ROOTS;
  }

  async function scan() {
    if (scanning) return;
    scanning = true;
    error = "";
    try {
      inspection = await inspectWorkspaceSkills(
        workspaceId,
        draftEnabled,
        normalizeRoots(draftRoots),
      );
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      scanning = false;
    }
  }

  async function save() {
    if (saving || !dirty) return;
    saving = true;
    error = "";
    try {
      const normalized = normalizeRoots(draftRoots);
      await onSave({ enabled: draftEnabled, roots: normalized });
      draftRoots = normalized;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      saving = false;
    }
  }
</script>

<form
  class="grid gap-4"
  onsubmit={(event) => {
    event.preventDefault();
    void save();
  }}
>
  <div class="flex items-start justify-between gap-5 rounded-lg border border-[var(--border)] bg-[var(--page-bg)] px-4 py-3">
    <div class="min-w-0">
      <p class="text-sm font-medium text-[var(--text-main)]">通过 MCP 提供 Agent Skills</p>
      <p class="mt-1 text-xs leading-5 text-[var(--text-muted)]">
        客户端可调用 <code class="font-mono">list_skills</code>、<code class="font-mono">load_skill</code>
        和 <code class="font-mono">read_skill_resource</code>，也可读取
        <code class="font-mono">skill://index.json</code>。
      </p>
    </div>
    <label class="inline-flex shrink-0 items-center gap-2 text-xs text-[var(--text-secondary)]">
      <input type="checkbox" bind:checked={draftEnabled} />
      {draftEnabled ? "已启用" : "已关闭"}
    </label>
  </div>

  <label class="grid gap-1.5">
    <span class="text-xs text-[var(--text-muted)]">Skill 根目录（每行一个）</span>
    <textarea
      class="min-h-32 resize-y rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-3 py-2 font-mono text-xs leading-5 disabled:opacity-50"
      placeholder={".agents/skills\n.codex/skills\nskills\n~/.codex/skills"}
      spellcheck="false"
      disabled={!draftEnabled}
      bind:value={draftRoots}
    ></textarea>
  </label>

  <p class="text-xs leading-5 text-[var(--text-muted)]">
    相对路径以当前 workspace 为基准，支持 <code class="font-mono">~/</code>。根目录可以直接是一个
    Skill，也可以包含两层以内的 Skill 子目录；每个 Skill 必须包含合法的
    <code class="font-mono">SKILL.md</code>。没有专用脚本执行器；通过
    <code class="font-mono">exec_command</code> 引用 Skill 脚本时必须显式确认，且脚本摘要必须与目录快照一致。
  </p>

  {#if error}
    <p class="text-xs text-[var(--danger)]">{error}</p>
  {/if}

  {#if inspection}
    <div class="grid gap-3 rounded-lg border border-[var(--border)] bg-[var(--page-bg)] p-3">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <p class="text-sm font-medium text-[var(--text-main)]">
          {inspection.enabled ? `发现 ${inspection.skills.length} 个 Skill` : "Skill 服务已关闭"}
        </p>
        <span class="text-xs text-[var(--text-muted)]">
          脚本策略：{inspection.scriptExecutionPolicy}
        </span>
      </div>

      {#if inspection.skills.length}
        <div class="grid max-h-64 gap-2 overflow-y-auto pr-1">
          {#each inspection.skills as skill}
            <div class="rounded-md border border-[var(--border)] px-3 py-2.5">
              <div class="flex flex-wrap items-baseline justify-between gap-2">
                <code class="font-mono text-xs font-semibold text-[var(--text-main)]">{skill.name}</code>
                <span class="text-[11px] text-[var(--text-muted)]">
                  {skill.resources.length} resources · {skill.scripts.length} scripts
                </span>
              </div>
              <p class="mt-1 text-xs leading-5 text-[var(--text-secondary)]">{skill.description}</p>
              <p class="mt-1 text-[11px] text-[var(--text-muted)]">
                来源：{skill.sourceId}/{skill.relativePath} · 包摘要：{skill.digest.slice(0, 22)}…
              </p>
              <p class="mt-1 truncate font-mono text-[10px] text-[var(--text-muted)]">{skill.uri}</p>
              {#if skill.resourceTruncated}
                <p class="mt-1 text-xs text-[var(--warning)]">资源清单已达到单 Skill 上限。</p>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
      <p class="border-t border-[var(--border)] pt-2 font-mono text-[10px] text-[var(--text-muted)]">
        snapshot={inspection.snapshotMode} · catalog={inspection.catalogDigest.slice(0, 26)}…
      </p>

      {#if inspection.warnings.length}
        <div class="grid gap-1 border-t border-[var(--border)] pt-2">
          {#each inspection.warnings as warning}
            <p class="text-xs leading-5 text-[var(--warning)]">{warning}</p>
          {/each}
        </div>
      {/if}
    </div>
  {/if}

  <div class="flex flex-wrap justify-end gap-2 pt-1">
    <button
      type="button"
      class="tx-btn-ghost px-3 py-1.5 text-sm disabled:opacity-50"
      disabled={scanning}
      onclick={() => void scan()}
    >
      {scanning ? "扫描中…" : "扫描目录"}
    </button>
    <button
      type="submit"
      class="tx-btn-primary px-3 py-1.5 text-sm disabled:opacity-50"
      disabled={saving || !dirty}
    >
      {saving ? "保存中…" : "保存 Skill 服务"}
    </button>
  </div>
</form>
