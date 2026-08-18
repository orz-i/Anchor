<script lang="ts">
  import { message } from "$lib/platform/dialog";
  import SecretInput from "$lib/components/SecretInput.svelte";
  import {
    getWorkspaceSecret,
    regenerateWorkspaceSecret,
    getSharedSecret,
    setSharedSecret,
    regenerateSharedSecret,
    type WorkspaceSecretKey,
    type SharedSecretKey,
  } from "$lib/api/secrets";
  import type { AuthConfig } from "$lib/types";

  interface Props {
    workspaceId: string;
    auth: AuthConfig;
    onSaveProfile: (
      auth: AuthConfig,
      options: { callbackPolicyOnly: boolean },
    ) => void | Promise<void>;
  }

  function validateRedirectUris(raw: string) {
    const values = raw.split(/\r?\n/).map((value) => value.trim()).filter(Boolean);
    for (const value of values) {
      if (value.includes("*")) throw new Error(`OAuth 回调 URL 不允许通配符：${value}`);
      let parsed: URL;
      try {
        parsed = new URL(value);
      } catch {
        throw new Error(`OAuth 回调 URL 无效：${value}`);
      }
      const loopback = ["localhost", "127.0.0.1", "[::1]", "::1"].includes(parsed.hostname);
      if (parsed.protocol !== "https:" && !(parsed.protocol === "http:" && loopback)) {
        throw new Error(`OAuth 回调 URL 必须使用 HTTPS 或 HTTP loopback：${value}`);
      }
      if (parsed.hash) throw new Error(`OAuth 回调 URL 不能包含 fragment：${value}`);
    }
  }

  function validateRedirectHosts(raw: string) {
    const values = raw.split(/\r?\n/).map((value) => value.trim()).filter(Boolean);
    for (const value of values) {
      if (!/^(\*\.)?[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/i.test(value) || value.includes("..")) {
        throw new Error(`Callback 域名格式无效：${value}`);
      }
      if (value.includes(":") || value.includes("/") || value.includes("?")) {
        throw new Error(`Callback 域名只能填写主机名或 *.域名：${value}`);
      }
    }
  }

  const AUTH_OPTIONS = [
    { value: "oauth", label: "OAuth" },
    { value: "bearer", label: "Bearer Token" },
    { value: "noauth", label: "不启用认证" },
  ] as const;

  let { workspaceId, auth, onSaveProfile }: Props = $props();

  let draft = $state<AuthConfig>({
    type: "oauth",
    oauth_client_id: "",
    oauth_redirect_uris: "",
    oauth_redirect_hosts: "",
    use_shared_secrets: false,
  });
  let saving = $state(false);
  let secrets = $state<Partial<Record<WorkspaceSecretKey, string>>>({});
  let loadedSecrets = $state<Partial<Record<WorkspaceSecretKey, string>>>({});
  let loadedSharedOauthClientId = $state("");
  let regenerating = $state<WorkspaceSecretKey | null>(null);
  let secretsLoadSeq = 0;
  let suppressSecretsReload = $state(false);

  const secretsDirty = $derived(
    (Object.keys(secrets) as WorkspaceSecretKey[]).some(
      (k) => secrets[k] !== loadedSecrets[k],
    ),
  );

  const dirty = $derived(
    draft.type !== auth.type ||
      (draft.use_shared_secrets
        ? draft.oauth_client_id !== loadedSharedOauthClientId
        : draft.oauth_client_id !== auth.oauth_client_id) ||
      (draft.oauth_redirect_uris ?? "") !== (auth.oauth_redirect_uris ?? "") ||
      (draft.oauth_redirect_hosts ?? "") !== (auth.oauth_redirect_hosts ?? "") ||
      draft.use_shared_secrets !== !!auth.use_shared_secrets ||
      secretsDirty,
  );

  const showOAuth = $derived(draft.type === "oauth");
  const showBearer = $derived(draft.type === "bearer");

  $effect(() => {
    draft = {
      type: auth.type,
      oauth_client_id: auth.oauth_client_id,
      oauth_redirect_uris: auth.oauth_redirect_uris ?? "",
      oauth_redirect_hosts: auth.oauth_redirect_hosts ?? "",
      use_shared_secrets: !!auth.use_shared_secrets,
    };
  });

  $effect(() => {
    if (suppressSecretsReload) return;
    const id = workspaceId;
    const authType = draft.type;
    const useShared = draft.use_shared_secrets ?? false;
    void loadSecrets(id, authType, useShared);
  });

  async function loadSecrets(id: string, authType: string, useShared: boolean) {
    const seq = ++secretsLoadSeq;
    const sharedClientId =
      authType === "oauth" && useShared ? await getSharedSecret("oauth_client_id") : null;
    const keys: WorkspaceSecretKey[] = [];
    if (authType === "oauth") {
      keys.push("oauth_client_secret", "oauth_password");
    } else if (authType === "bearer") {
      keys.push("bearer_token");
    }
    if (keys.length === 0) {
      if (seq !== secretsLoadSeq) return;
      secrets = {};
      loadedSecrets = {};
      return;
    }
    const loaded = await Promise.all(
      keys.map(async (key) => {
        const value = useShared
          ? await getSharedSecret(key as SharedSecretKey)
          : await getWorkspaceSecret(id, key);
        return [key, value ?? ""] as const;
      }),
    );
    if (seq !== secretsLoadSeq) return;
    if (authType === "oauth" && useShared) {
      draft = { ...draft, oauth_client_id: sharedClientId ?? "" };
      loadedSharedOauthClientId = sharedClientId ?? "";
    } else {
      loadedSharedOauthClientId = "";
    }
    secrets = Object.fromEntries(loaded);
    loadedSecrets = Object.fromEntries(loaded);
  }

  async function save() {
    if (saving || !dirty) return;
    saving = true;
    suppressSecretsReload = true;
    try {
      const clientIdChanged = draft.use_shared_secrets
        ? draft.oauth_client_id !== loadedSharedOauthClientId
        : draft.oauth_client_id !== auth.oauth_client_id;
      if (draft.type === "oauth") {
        validateRedirectUris(draft.oauth_redirect_uris ?? "");
        validateRedirectHosts(draft.oauth_redirect_hosts ?? "");
      }
      if (draft.type === "oauth" && draft.use_shared_secrets) {
        const clientId = draft.oauth_client_id.trim();
        if (!clientId) throw new Error("OAuth Client ID 不能为空");
        await setSharedSecret("oauth_client_id", clientId);
        loadedSharedOauthClientId = clientId;
      }
      const callbackPolicyChanged =
        (draft.oauth_redirect_uris ?? "") !== (auth.oauth_redirect_uris ?? "") ||
        (draft.oauth_redirect_hosts ?? "") !== (auth.oauth_redirect_hosts ?? "");
      const callbackPolicyOnly =
        draft.type === "oauth" &&
        auth.type === "oauth" &&
        callbackPolicyChanged &&
        !clientIdChanged &&
        draft.use_shared_secrets === !!auth.use_shared_secrets &&
        !secretsDirty;
      await onSaveProfile({ ...draft }, { callbackPolicyOnly });
      // Auth save only persists profile fields; secrets are already stored by regenerate.
      loadedSecrets = { ...secrets };
    } catch (error) {
      await message(String(error), { title: "认证配置保存失败", kind: "error" });
    } finally {
      suppressSecretsReload = false;
      saving = false;
    }
  }

  async function regenerate(key: WorkspaceSecretKey) {
    if (regenerating) return;
    regenerating = key;
    try {
      const value = draft.use_shared_secrets
        ? await regenerateSharedSecret(key as SharedSecretKey)
        : await regenerateWorkspaceSecret(workspaceId, key);
      secrets = { ...secrets, [key]: value };
    } catch (error) {
      await message(String(error), { title: "重新生成失败", kind: "error" });
    } finally {
      regenerating = null;
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
  <p class="text-xs text-[var(--text-muted)]">
    复制 Client ID / 密钥等请用上方「GPT 配置」卡片；此处可修改认证类型与重新生成密钥。
  </p>

  <label class="grid gap-1">
    <span class="text-xs text-[var(--text-muted)]">认证类型</span>
    <select
      class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
      bind:value={draft.type}
    >
      {#each AUTH_OPTIONS as option}
        <option value={option.value}>{option.label}</option>
      {/each}
    </select>
  </label>

  <label class="flex items-center gap-2">
    <input
      type="checkbox"
      class="h-4 w-4"
      bind:checked={draft.use_shared_secrets}
    />
    <span class="text-xs text-[var(--text-muted)]">使用全局共享密钥（在「设置 → 共享密钥」中管理）</span>
  </label>

  {#if showOAuth}
    <div class="tx-alert tx-alert--warning" role="note">
      OAuth 访问令牌有效期为 1 小时；Refresh Token 每次续约都会轮换并重新获得 90 天有效期。超过 90
      天未续约，或重新生成 Token Secret 后，客户端需要重新授权。
    </div>

    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">OAuth 客户端 ID</span>
      <input
        type="text"
        class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-sm"
        bind:value={draft.oauth_client_id}
        readonly={draft.use_shared_secrets}
      />
    </label>

    <div class="tx-alert" role="note">
      ChatGPT 官方 callback 会自动识别并登记，无需复制 Callback URL，也无需配置域名白名单。
    </div>

    <details class="rounded-md border border-[var(--border)] px-3 py-2">
      <summary class="cursor-pointer text-xs text-[var(--text-muted)]">
        高级：其他 OAuth 客户端 Callback 策略
      </summary>
      <div class="mt-3 grid gap-3">
        <label class="grid gap-1">
          <span class="text-xs text-[var(--text-muted)]">附加精确 Callback URL（可选）</span>
          <textarea
            rows="3"
            class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-xs"
            bind:value={draft.oauth_redirect_uris}
          ></textarea>
        </label>

        <label class="grid gap-1">
          <span class="text-xs text-[var(--text-muted)]">附加 Callback 域名白名单（可选）</span>
          <textarea
            rows="3"
            class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-xs"
            bind:value={draft.oauth_redirect_hosts}
            placeholder="oauth.example.com&#10;*.example.com"
          ></textarea>
          <span class="text-xs text-[var(--text-muted)]">
            匹配域名会自动登记本次精确 callback，不显示确认项，也不重启 listener。
          </span>
        </label>
      </div>
    </details>

    <div class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">OAuth 客户端密钥</span>
      <SecretInput
        value={secrets.oauth_client_secret ?? ""}
        placeholder="加载中…"
        readonly
        onRegenerate={() => void regenerate("oauth_client_secret")}
        regenerating={regenerating === "oauth_client_secret"}
      />
    </div>

    <div class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">授权口令</span>
      <SecretInput
        value={secrets.oauth_password ?? ""}
        placeholder="ChatGPT 首次授权时输入这个口令"
        readonly
        onRegenerate={() => void regenerate("oauth_password")}
        regenerating={regenerating === "oauth_password"}
      />
    </div>
  {/if}

  {#if showBearer}
    <div class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">Bearer Token</span>
      <SecretInput
        value={secrets.bearer_token ?? ""}
        placeholder="加载中…"
        readonly
        onRegenerate={() => void regenerate("bearer_token")}
        regenerating={regenerating === "bearer_token"}
      />
    </div>
  {/if}

  <div class="flex justify-end pt-1">
    <button
      type="submit"
      class="rounded-md bg-[var(--primary)] px-3 py-1.5 text-sm font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-50"
      disabled={saving || !dirty}
    >
      {saving ? "保存中…" : "保存配置"}
    </button>
  </div>
</form>
