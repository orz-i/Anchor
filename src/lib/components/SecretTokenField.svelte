<script lang="ts">
  import { supportsAdminCommand } from "$lib/api/invoke";
  import {
    secretIsSet,
    setWorkspaceSecret,
    type WorkspaceSecretKey,
  } from "$lib/api/secrets";
  import SecretInput from "$lib/components/SecretInput.svelte";

  interface Props {
    workspaceId: string;
    secretKey: WorkspaceSecretKey;
    label?: string;
    onSaved?: () => void;
    hasPending?: boolean;
  }

  let {
    workspaceId,
    secretKey,
    label = "Cloudflare Tunnel Token",
    onSaved,
    hasPending = $bindable(false),
  }: Props = $props();

  let draft = $state("");
  let saved = $state(false);
  let loading = $state(true);
  let mutationSupported = $state(false);

  const placeholder = $derived(saved && !draft ? "已保存（点击更新）" : "粘贴 Tunnel Token");

  $effect(() => {
    hasPending = draft.trim().length > 0;
  });

  $effect(() => {
    workspaceId;
    secretKey;
    void load();
  });

  async function load() {
    loading = true;
    try {
      draft = "";
      const [nextSaved, canMutate] = await Promise.all([
        secretIsSet(workspaceId, secretKey),
        supportsAdminCommand("set_workspace_secret"),
      ]);
      saved = nextSaved;
      mutationSupported = canMutate;
    } finally {
      loading = false;
    }
  }

  export async function saveIfDirty(): Promise<boolean> {
    if (!draft.trim()) return false;
    if (!mutationSupported) {
      throw new Error("Web 管理面尚未开放 Secret 写入。");
    }
    await setWorkspaceSecret(workspaceId, secretKey, draft.trim());
    saved = true;
    draft = "";
    onSaved?.();
    return true;
  }

  export function hasPendingValue(): boolean {
    return hasPending;
  }
</script>

<label class="grid gap-1">
  <span class="text-xs text-[var(--text-muted)]">
    {label}{mutationSupported ? "" : "（Web 当前只读）"}
  </span>
  <SecretInput
    bind:value={draft}
    {placeholder}
    disabled={loading || !mutationSupported}
    showCopy={false}
  />
</label>
