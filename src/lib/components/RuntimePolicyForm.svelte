<script lang="ts">
  export interface RuntimePolicyDraft {
    toolProfile: string;
    permissionMode: string;
    allowedCommands: string;
    workspaceLocalEntries: boolean;
    workspaceScriptExtensions: string;
    externalPaidCommandsEnabled: boolean;
    externalPaidMaxRunsPerDay: number;
    externalPaidMaxDurationSeconds: number;
  }

  interface Props {
    toolProfile: string;
    permissionMode: string;
    allowedCommands: string;
    workspaceLocalEntries: boolean;
    workspaceScriptExtensions: string;
    externalPaidCommandsEnabled: boolean;
    externalPaidMaxRunsPerDay: number;
    externalPaidMaxDurationSeconds: number;
    onSave: (draft: RuntimePolicyDraft) => void | Promise<void>;
  }

  const TOOL_PROFILE_OPTIONS = [
    { value: "core", label: "核心工具" },
    { value: "advanced", label: "完整工具" },
    { value: "read-only", label: "只读工具" },
  ] as const;

  const PERMISSION_MODE_OPTIONS = [
    { value: "trusted", label: "受信任" },
    { value: "safe", label: "安全受限" },
    { value: "dangerous", label: "完全放开" },
  ] as const;

  let { toolProfile, permissionMode, allowedCommands, workspaceLocalEntries, workspaceScriptExtensions, externalPaidCommandsEnabled, externalPaidMaxRunsPerDay, externalPaidMaxDurationSeconds, onSave }: Props = $props();

  let draftProfile = $state("core");
  let draftMode = $state("trusted");
  let draftCommands = $state("");
  let draftLocalEntries = $state(true);
  let draftExtensions = $state(".exe,.bat,.cmd,.ps1");
  let draftExternalPaidEnabled = $state(false);
  let draftExternalPaidRuns = $state(1);
  let draftExternalPaidDuration = $state(1800);
  let saving = $state(false);

  function canonicalProfile(value: string) {
    if (value === "advanced" || value === "read-only") return value;
    return "core";
  }

  const dirty = $derived(
    draftProfile !== canonicalProfile(toolProfile) || draftMode !== permissionMode || draftCommands !== allowedCommands || draftLocalEntries !== workspaceLocalEntries || draftExtensions !== workspaceScriptExtensions || draftExternalPaidEnabled !== externalPaidCommandsEnabled || draftExternalPaidRuns !== externalPaidMaxRunsPerDay || draftExternalPaidDuration !== externalPaidMaxDurationSeconds,
  );

  $effect(() => {
    draftProfile = canonicalProfile(toolProfile);
    draftMode = permissionMode;
    draftCommands = allowedCommands;
    draftLocalEntries = workspaceLocalEntries;
    draftExtensions = workspaceScriptExtensions;
    draftExternalPaidEnabled = externalPaidCommandsEnabled;
    draftExternalPaidRuns = externalPaidMaxRunsPerDay;
    draftExternalPaidDuration = externalPaidMaxDurationSeconds;
  });

  async function save() {
    if (saving || !dirty) return;
    saving = true;
    try {
      await onSave({ toolProfile: draftProfile, permissionMode: draftMode, allowedCommands: draftCommands.trim(), workspaceLocalEntries: draftLocalEntries, workspaceScriptExtensions: draftExtensions.trim(), externalPaidCommandsEnabled: draftExternalPaidEnabled, externalPaidMaxRunsPerDay: Math.max(1, Math.floor(draftExternalPaidRuns)), externalPaidMaxDurationSeconds: Math.max(1, Math.floor(draftExternalPaidDuration)) });
    } finally {
      saving = false;
    }
  }
</script>

<form
  class="grid gap-3"
  onsubmit={(event) => {
    event.preventDefault();
    void save();
  }}
>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">工具档位</span>
    <select
      class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm"
      bind:value={draftProfile}
    >
      {#each TOOL_PROFILE_OPTIONS as option}
        <option value={option.value}>{option.label}</option>
      {/each}
    </select>
  </label>
  <fieldset class="grid gap-2 rounded-md border border-[var(--color-border)] p-3">
    <legend class="px-1 text-xs text-[var(--color-text-muted)]">真实付费命令</legend>
    <label class="flex items-center gap-2 text-sm">
      <input type="checkbox" bind:checked={draftExternalPaidEnabled} />
      <span>允许执行已识别的真实付费命令</span>
    </label>
    <div class="grid grid-cols-2 gap-2">
      <label class="grid gap-1">
        <span class="text-xs text-[var(--color-text-muted)]">每日最大运行次数</span>
        <input type="number" min="1" max="100" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm" bind:value={draftExternalPaidRuns} />
      </label>
      <label class="grid gap-1">
        <span class="text-xs text-[var(--color-text-muted)]">单次最长秒数</span>
        <input type="number" min="1" max="3600" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm" bind:value={draftExternalPaidDuration} />
      </label>
    </div>
    <p class="text-xs text-[var(--color-text-muted)]">此开关只能在受信任控制面保存。项目可在 .anchor/command-policy.yml 中进一步收紧命令匹配、次数和时长；模型参数不能启用该权限。</p>
  </fieldset>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">系统命令（逗号分隔）</span>
    <input type="text" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 font-mono text-sm" placeholder="python,git,curl,powershell,..." bind:value={draftCommands} />
  </label>
  <label class="flex items-center gap-2 text-sm">
    <input type="checkbox" bind:checked={draftLocalEntries} />
    <span>允许执行 Workspace 内本地入口</span>
  </label>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">本地脚本扩展名（逗号分隔）</span>
    <input type="text" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 font-mono text-sm" placeholder=".exe,.bat,.cmd,.ps1" bind:value={draftExtensions} disabled={!draftLocalEntries} />
  </label>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">权限模式</span>
    <select
      class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm"
      bind:value={draftMode}
    >
      {#each PERMISSION_MODE_OPTIONS as option}
        <option value={option.value}>{option.label}</option>
      {/each}
    </select>
  </label>
  <p class="text-xs text-[var(--color-text-muted)]">
    Workspace 本地入口按当前工作目录解析；系统命令与脚本类型均可按项目配置。dangerous 只能由操作者在此控制面启用，模型参数不能作为用户批准凭证。当前执行边界仍为 policy_only。
  </p>
  <div class="flex justify-end pt-1">
    <button
      type="submit"
      class="rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
      disabled={saving || !dirty}
    >
      {saving ? "保存中…" : "保存策略"}
    </button>
  </div>
</form>
