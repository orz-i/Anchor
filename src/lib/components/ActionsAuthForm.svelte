<script lang="ts">
  import { message } from "$lib/platform/dialog";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import SecretInput from "$lib/components/SecretInput.svelte";
  import {
    getWorkspaceSecret,
    regenerateWorkspaceSecret,
    getSharedSecret,
    regenerateSharedSecret,
  } from "$lib/api/secrets";
  import type { ActionsAuthDraft } from "$lib/types";

  export const ACTIONS_AUTH_OPTIONS = [
    { value: "api_key", label: "API Key / Bearer" },
    { value: "none", label: "不启用认证" },
    { value: "oauth", label: "OAuth" },
  ] as const;

  export type { ActionsAuthDraft } from "$lib/types";

  interface Props {
    workspaceId: string;
    authType: string;
    oauthClientId: string;
    oauthRedirectUris: string;
    oauthRedirectHosts: string;
    oauthScopes: string;
    openapiUrl: string;
    privacyUrl: string;
    oauthAuthorizeUrl: string;
    oauthTokenUrl: string;
    useSharedSecrets?: boolean;
    onSave: (
      draft: ActionsAuthDraft,
      options: { callbackPolicyOnly: boolean },
    ) => void | Promise<void>;
  }

  function validateRedirectUris(raw: string) {
    const values = raw.split(/\r?\n/).map((value) => value.trim()).filter(Boolean);
    for (const value of values) {
      if (value.includes("*")) throw new Error(`OAuth Callback URL 不允许通配符：${value}`);
      let parsed: URL;
      try {
        parsed = new URL(value);
      } catch {
        throw new Error(`OAuth Callback URL 无效：${value}`);
      }
      const loopback = ["localhost", "127.0.0.1", "[::1]", "::1"].includes(parsed.hostname);
      if (parsed.protocol !== "https:" && !(parsed.protocol === "http:" && loopback)) {
        throw new Error(`OAuth Callback URL 必须使用 HTTPS 或 HTTP loopback：${value}`);
      }
      if (parsed.hash) throw new Error(`OAuth Callback URL 不能包含 fragment：${value}`);
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

  let {
    workspaceId,
    authType,
    oauthClientId,
    oauthRedirectUris,
    oauthRedirectHosts,
    oauthScopes,
    openapiUrl,
    privacyUrl,
    oauthAuthorizeUrl,
    oauthTokenUrl,
    useSharedSecrets = false,
    onSave,
  }: Props = $props();

  let draftAuthType = $state("api_key");
  let draftOauthClientId = $state("");
  let draftOauthRedirectUris = $state("");
  let draftOauthRedirectHosts = $state("");
  let draftOauthScopes = $state("");
  let draftUseShared = $state(false);
  let apiKey = $state("");
  let loadedApiKey = $state("");
  let oauthClientSecret = $state("");
  let loadedOauthClientSecret = $state("");
  let oauthPassword = $state("");
  let loadedOauthPassword = $state("");
  let oauthTokenSecret = $state("");
  let loadedOauthTokenSecret = $state("");
  let loadingKey = $state(true);
  let loadingOAuthSecret = $state(true);
  let loadingOAuthPassword = $state(true);
  let loadingOAuthTokenSecret = $state(true);
  let regenerating = $state(false);
  let regeneratingOAuthSecret = $state(false);
  let regeneratingOAuthPassword = $state(false);
  let regeneratingOAuthTokenSecret = $state(false);
  let saving = $state(false);
  let secretsLoadSeq = 0;
  let suppressSecretsReload = $state(false);

  const secretsDirty = $derived(
    apiKey !== loadedApiKey ||
      oauthClientSecret !== loadedOauthClientSecret ||
      oauthPassword !== loadedOauthPassword ||
      oauthTokenSecret !== loadedOauthTokenSecret,
  );

  const dirty = $derived(
    draftAuthType !== authType ||
      draftOauthClientId !== oauthClientId ||
      draftOauthRedirectUris !== oauthRedirectUris ||
      draftOauthRedirectHosts !== oauthRedirectHosts ||
      draftOauthScopes !== oauthScopes ||
      draftUseShared !== useSharedSecrets ||
      secretsDirty,
  );
  const showApiKey = $derived(draftAuthType === "api_key");
  const showOAuth = $derived(draftAuthType === "oauth");

  $effect(() => {
    draftAuthType = authType;
    draftOauthClientId = oauthClientId;
    draftOauthRedirectUris = oauthRedirectUris;
    draftOauthRedirectHosts = oauthRedirectHosts;
    draftOauthScopes = oauthScopes;
    draftUseShared = useSharedSecrets;
  });

  $effect(() => {
    if (suppressSecretsReload) return;
    workspaceId;
    draftUseShared;
    void loadSecrets();
  });

  async function loadSecrets() {
    const seq = ++secretsLoadSeq;
    loadingKey = true;
    loadingOAuthSecret = true;
    loadingOAuthPassword = true;
    loadingOAuthTokenSecret = true;
    try {
      const [key, secret, password, tokenSecret] = await Promise.all([
        draftUseShared
          ? getSharedSecret("actions_api_key")
          : getWorkspaceSecret(workspaceId, "actions_api_key"),
        draftUseShared
          ? getSharedSecret("actions_oauth_client_secret")
          : getWorkspaceSecret(workspaceId, "actions_oauth_client_secret"),
        draftUseShared
          ? getSharedSecret("actions_oauth_password")
          : getWorkspaceSecret(workspaceId, "actions_oauth_password"),
        draftUseShared
          ? getSharedSecret("actions_oauth_token_secret")
          : getWorkspaceSecret(workspaceId, "actions_oauth_token_secret"),
      ]);
      if (seq !== secretsLoadSeq) return;
      apiKey = key ?? "";
      loadedApiKey = key ?? "";
      oauthClientSecret = secret ?? "";
      loadedOauthClientSecret = secret ?? "";
      oauthPassword = password ?? "";
      loadedOauthPassword = password ?? "";
      oauthTokenSecret = tokenSecret ?? "";
      loadedOauthTokenSecret = tokenSecret ?? "";
    } finally {
      if (seq !== secretsLoadSeq) return;
      loadingKey = false;
      loadingOAuthSecret = false;
      loadingOAuthPassword = false;
      loadingOAuthTokenSecret = false;
    }
  }

  async function save() {
    if (saving || !dirty) return;
    saving = true;
    suppressSecretsReload = true;
    try {
      if (draftAuthType === "oauth") {
        validateRedirectUris(draftOauthRedirectUris);
        validateRedirectHosts(draftOauthRedirectHosts);
      }
      const callbackPolicyChanged =
        draftOauthRedirectUris !== oauthRedirectUris ||
        draftOauthRedirectHosts !== oauthRedirectHosts;
      const callbackPolicyOnly =
        draftAuthType === "oauth" &&
        authType === "oauth" &&
        callbackPolicyChanged &&
        draftOauthClientId === oauthClientId &&
        draftOauthScopes === oauthScopes &&
        draftUseShared === useSharedSecrets &&
        !secretsDirty;
      await onSave({
        authType: draftAuthType,
        oauthClientId: draftOauthClientId.trim(),
        oauthRedirectUris: draftOauthRedirectUris.trim(),
        oauthRedirectHosts: draftOauthRedirectHosts.trim(),
        oauthScopes: draftOauthScopes.trim(),
        useSharedSecrets: draftUseShared,
      }, { callbackPolicyOnly });
      loadedApiKey = apiKey;
      loadedOauthClientSecret = oauthClientSecret;
      loadedOauthPassword = oauthPassword;
      loadedOauthTokenSecret = oauthTokenSecret;
    } catch (error) {
      await message(String(error), { title: "认证配置保存失败", kind: "error" });
    } finally {
      suppressSecretsReload = false;
      saving = false;
    }
  }

  async function regenerate() {
    if (regenerating) return;
    regenerating = true;
    try {
      apiKey = draftUseShared
        ? await regenerateSharedSecret("actions_api_key")
        : await regenerateWorkspaceSecret(workspaceId, "actions_api_key");
    } catch (error) {
      await message(String(error), { title: "重新生成失败", kind: "error" });
    } finally {
      regenerating = false;
    }
  }

  async function regenerateOAuthSecret() {
    if (regeneratingOAuthSecret) return;
    regeneratingOAuthSecret = true;
    try {
      oauthClientSecret = draftUseShared
        ? await regenerateSharedSecret("actions_oauth_client_secret")
        : await regenerateWorkspaceSecret(workspaceId, "actions_oauth_client_secret");
    } catch (error) {
      await message(String(error), { title: "重新生成失败", kind: "error" });
    } finally {
      regeneratingOAuthSecret = false;
    }
  }

  async function regenerateOAuthPassword() {
    if (regeneratingOAuthPassword) return;
    regeneratingOAuthPassword = true;
    try {
      oauthPassword = draftUseShared
        ? await regenerateSharedSecret("actions_oauth_password")
        : await regenerateWorkspaceSecret(workspaceId, "actions_oauth_password");
    } catch (error) {
      await message(String(error), { title: "重新生成失败", kind: "error" });
    } finally {
      regeneratingOAuthPassword = false;
    }
  }

  async function regenerateOAuthTokenSecret() {
    if (regeneratingOAuthTokenSecret) return;
    regeneratingOAuthTokenSecret = true;
    try {
      oauthTokenSecret = draftUseShared
        ? await regenerateSharedSecret("actions_oauth_token_secret")
        : await regenerateWorkspaceSecret(workspaceId, "actions_oauth_token_secret");
    } catch (error) {
      await message(String(error), { title: "重新生成失败", kind: "error" });
    } finally {
      regeneratingOAuthTokenSecret = false;
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
    复制 OpenAPI、密钥等请用上方「GPT 配置」卡片；此处仅修改认证方式与密钥。
  </p>

  <label class="grid gap-1">
    <span class="text-xs text-[var(--text-muted)]">认证方式</span>
    <select
      class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
      bind:value={draftAuthType}
    >
      {#each ACTIONS_AUTH_OPTIONS as option}
        <option value={option.value}>{option.label}</option>
      {/each}
    </select>
  </label>

  <label class="flex items-center gap-2">
    <input
      type="checkbox"
      class="h-4 w-4"
      bind:checked={draftUseShared}
    />
    <span class="text-xs text-[var(--text-muted)]">使用全局共享密钥（在「设置 → 共享密钥」中管理）</span>
  </label>

  {#if showApiKey}
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">API Key（Bearer）</span>
      <SecretInput
        value={loadingKey ? "加载中…" : apiKey}
        readonly
        disabled={loadingKey}
        showCopy={!!apiKey}
        onRegenerate={() => void regenerate()}
        regenerating={regenerating}
      />
    </label>
    <p class="text-xs text-[var(--text-muted)]">
      在 GPT Actions 认证里选 API Key → Bearer，Key 填这里的值。
    </p>
  {:else if showOAuth}
    <div class="tx-alert tx-alert--warning" role="note">
      OAuth 访问令牌有效期为 1 小时；Refresh Token 每次续约都会轮换并重新获得 90 天有效期。超过 90
      天未续约，或重新生成 OAuth Token Secret 后，现有连接需要重新授权。
    </div>

    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">OAuth Client ID（填到 GPT）</span>
      <div class="flex gap-2">
        <input
          type="text"
          class="min-w-0 flex-1 rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-sm"
          bind:value={draftOauthClientId}
        />
        {#if draftOauthClientId}
          <CopyButton value={draftOauthClientId} label="复制" />
        {/if}
      </div>
    </label>
    <div class="tx-alert" role="note">
      ChatGPT 官方 callback 会自动识别并登记，无需中途返回 GUI 填写 Callback URL。
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
            bind:value={draftOauthRedirectUris}
          ></textarea>
        </label>
        <label class="grid gap-1">
          <span class="text-xs text-[var(--text-muted)]">附加 Callback 域名白名单（可选）</span>
          <textarea
            rows="3"
            class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-xs"
            bind:value={draftOauthRedirectHosts}
            placeholder="oauth.example.com&#10;*.example.com"
          ></textarea>
          <span class="text-xs text-[var(--text-muted)]">
            匹配域名会自动登记精确 callback，不显示确认项，也不重启服务。
          </span>
        </label>
      </div>
    </details>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">OAuth Client Secret（填到 GPT）</span>
      <SecretInput
        value={loadingOAuthSecret ? "加载中…" : oauthClientSecret}
        readonly
        disabled={loadingOAuthSecret}
        showCopy={!!oauthClientSecret}
        onRegenerate={() => void regenerateOAuthSecret()}
        regenerating={regeneratingOAuthSecret}
      />
    </label>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">OAuth Password（服务端校验）</span>
      <SecretInput
        value={loadingOAuthPassword ? "加载中…" : oauthPassword}
        readonly
        disabled={loadingOAuthPassword}
        showCopy={!!oauthPassword}
        onRegenerate={() => void regenerateOAuthPassword()}
        regenerating={regeneratingOAuthPassword}
      />
    </label>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">OAuth Token Secret（JWT 签名）</span>
      <SecretInput
        value={loadingOAuthTokenSecret ? "加载中…" : oauthTokenSecret}
        readonly
        disabled={loadingOAuthTokenSecret}
        showCopy={!!oauthTokenSecret}
        onRegenerate={() => void regenerateOAuthTokenSecret()}
        regenerating={regeneratingOAuthTokenSecret}
      />
    </label>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">Authorization URL（填到 GPT）</span>
      <div class="flex gap-2">
        <input
          type="text"
          readonly
          class="min-w-0 flex-1 rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-xs"
          value={oauthAuthorizeUrl}
        />
        {#if oauthAuthorizeUrl}
          <CopyButton value={oauthAuthorizeUrl} label="复制" />
        {/if}
      </div>
    </label>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">Token URL（填到 GPT）</span>
      <div class="flex gap-2">
        <input
          type="text"
          readonly
          class="min-w-0 flex-1 rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-xs"
          value={oauthTokenUrl}
        />
        {#if oauthTokenUrl}
          <CopyButton value={oauthTokenUrl} label="复制" />
        {/if}
      </div>
    </label>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">Scope（填到 GPT，空格分隔）</span>
      <input
        type="text"
        class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
        placeholder="例如：anchor"
        bind:value={draftOauthScopes}
      />
    </label>
    <p class="text-xs text-[var(--text-muted)]">
      GPT 编辑器会生成 Callback URL（<code>https://chatgpt.com/aip/g-…/oauth/callback</code>），无需在本应用配置。Token
      交换方式选默认即可。
    </p>
  {:else}
    <p class="text-xs text-[var(--text-muted)]">
      不校验请求认证；GPT 侧选 None。仅建议本机调试，公网暴露请用 API Key 或 OAuth。
    </p>
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
