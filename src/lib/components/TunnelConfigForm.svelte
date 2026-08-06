<script lang="ts">
  import { onMount } from "svelte";
  import { listFrpProfiles, type FrpProfileDto } from "$lib/api/settings";
  import { testTunnel as invokeTunnelTest } from "$lib/api/tunnel";
  import SecretTokenField from "$lib/components/SecretTokenField.svelte";
  import { showToast } from "$lib/stores/toast";

  export interface TunnelFormConfig {
    type: string;
    public_url: string;
    frp_server: string;
    frp_subdomain: string;
    frp_profile_id: string;
    frp_server_port: number;
    frp_proxy_type: string;
    frp_cert_path: string;
    frp_key_path: string;
    cloudflare_mode: string;
    use_proxy: boolean;
  }

  export interface SaveTunnelOptions {
    skipTunnelRestart?: boolean;
    skipServicePrompt?: boolean;
  }

  interface Props {
    workspaceId: string;
    service: "mcp" | "actions";
    config: TunnelFormConfig;
    onSave: (config: TunnelFormConfig, options?: SaveTunnelOptions) => void | Promise<void>;
  }

  let { workspaceId, service, config, onSave }: Props = $props();

  let draft = $state<TunnelFormConfig>({
    type: "none",
    public_url: "",
    frp_server: "",
    frp_subdomain: "",
    frp_profile_id: "",
    frp_server_port: 7000,
    frp_proxy_type: "http",
    frp_cert_path: "",
    frp_key_path: "",
    cloudflare_mode: "named",
    use_proxy: true,
  });
  let saving = $state(false);
  let testing = $state(false);
  let tokenField = $state<SecretTokenField | null>(null);
  let tokenPending = $state(false);
  let frpProfiles = $state<FrpProfileDto[]>([]);
  let manualFrpOpen = $state(false);

  const secretKey = $derived(
    service === "mcp"
      ? draft.type === "frp"
        ? ("frp_token" as const)
        : ("cloudflare_token" as const)
      : draft.type === "frp"
        ? ("actions_frp_token" as const)
        : ("actions_cloudflare_token" as const),
  );

  const selectedProfile = $derived(
    frpProfiles.find((profile) => profile.id === draft.frp_profile_id) ?? null,
  );

  const useGlobalProfile = $derived(Boolean(draft.frp_profile_id && selectedProfile));

  const dirty = $derived(
    draft.type !== config.type ||
      draft.public_url !== config.public_url ||
      draft.frp_server !== config.frp_server ||
      draft.frp_subdomain !== config.frp_subdomain ||
      draft.frp_profile_id !== config.frp_profile_id ||
      draft.frp_server_port !== config.frp_server_port ||
      draft.frp_proxy_type !== config.frp_proxy_type ||
      draft.frp_cert_path !== config.frp_cert_path ||
      draft.frp_key_path !== config.frp_key_path ||
      draft.cloudflare_mode !== config.cloudflare_mode ||
      draft.use_proxy !== config.use_proxy ||
      tokenPending,
  );

  const showFrp = $derived(draft.type === "frp");
  const showCloudflare = $derived(draft.type === "cloudflare");
  const showCloudflareToken = $derived(showCloudflare && draft.cloudflare_mode === "named");
  const showManualFrpToken = $derived(showFrp && !useGlobalProfile);
  const canTest = $derived(draft.type === "frp" || draft.type === "cloudflare");

  $effect(() => {
    draft = {
      ...config,
      frp_profile_id: config.frp_profile_id ?? "",
      use_proxy: config.use_proxy ?? true,
    };
  });

  onMount(async () => {
    frpProfiles = await listFrpProfiles();
  });

  async function saveDraft(options?: SaveTunnelOptions) {
    if (tokenField && (showManualFrpToken || showCloudflareToken)) {
      await tokenField.saveIfDirty();
    }
    await onSave({ ...draft }, options);
  }

  async function save() {
    if (saving || !dirty) return;
    saving = true;
    try {
      await saveDraft();
      showToast("隧道配置已保存。", { title: "保存成功", kind: "success" });
    } catch (error) {
      showToast(String(error), { title: "保存失败", kind: "error", duration: 8000 });
    } finally {
      saving = false;
    }
  }

  async function testTunnelConnection() {
    if (!canTest || testing) return;
    testing = true;
    try {
      if (dirty) {
        await saveDraft({ skipTunnelRestart: true, skipServicePrompt: true });
      }

      const result = await invokeTunnelTest(workspaceId, service);
      if (result.publicUrl && draft.cloudflare_mode === "quick") {
        draft.public_url = result.publicUrl;
      }

      if (result.success && result.publicUrl) {
        const detail = `${result.message}\n${result.publicUrl}${
          result.keptRunning ? "" : "\n\n如需长期使用，请先启动服务。"
        }`;
        showToast(detail, { title: "测试成功", kind: "success", duration: 8000 });
      } else if (result.success) {
        showToast(result.message, { title: "测试成功", kind: "success" });
      } else {
        showToast(result.message, { title: "测试未完成", kind: "warning", duration: 7000 });
      }
    } catch (error) {
      showToast(String(error), { title: "测试失败", kind: "error", duration: 8000 });
    } finally {
      testing = false;
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
    <span class="text-xs text-[var(--text-muted)]">隧道类型</span>
    <select
      class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
      value={draft.type}
      onchange={(event) => {
        const nextType = event.currentTarget.value;
        if (nextType !== draft.type) {
          draft.type = nextType;
          draft.public_url = "";
        }
      }}
    >
      <option value="none">未配置</option>
      <option value="frp">FRP</option>
      <option value="cloudflare">Cloudflare</option>
    </select>
  </label>

  {#if canTest}
    <label class="flex items-start gap-2 rounded-md border border-[var(--border)] bg-[var(--card-bg)] px-3 py-2.5">
      <input
        type="checkbox"
        class="mt-0.5 h-4 w-4"
        bind:checked={draft.use_proxy}
      />
      <span class="grid gap-0.5">
        <span class="text-xs font-medium text-[var(--text-secondary)]">使用网络代理</span>
        <span class="text-[11px] text-[var(--text-muted)]">
          启用后通过「设置 → 通用」中的全局代理连接隧道；关闭则直连（适合海外或已全局翻墙的环境）。
        </span>
      </span>
    </label>
  {/if}

  {#if showFrp}
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">FRP 配置</span>
      <select
        class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
        bind:value={draft.frp_profile_id}
      >
        <option value="">手动填写</option>
        {#each frpProfiles as profile (profile.id)}
          <option value={profile.id}>
            {profile.name} · {profile.server}:{profile.serverPort}
          </option>
        {/each}
      </select>
      {#if frpProfiles.length === 0}
        <p class="text-[11px] text-[var(--text-muted)]">
          请先在侧边栏「FRP 配置」中添加全局服务器配置。
        </p>
      {/if}
    </label>

    {#if useGlobalProfile && selectedProfile}
      <div class="rounded-md border border-[var(--border)] bg-[var(--card-bg)] px-3 py-2 text-xs">
        <p class="text-[var(--text-secondary)]">
          服务器：{selectedProfile.server}:{selectedProfile.serverPort}
        </p>
        <p class="mt-1 text-[var(--text-muted)]">
          Token：{selectedProfile.hasToken ? "已配置" : "未配置"}
        </p>
      </div>
    {/if}

    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">子域名</span>
      <input
        type="text"
        class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-sm"
        placeholder="my-mcp"
        bind:value={draft.frp_subdomain}
      />
      <p class="text-[11px] text-[var(--text-muted)]">
        每个工作区使用独立子域名；保存后若隧道已连接会自动重启 frpc。控制服务器为 IP 或专用控制域名时，
        还必须在下方填写实际公网 URL。
      </p>
    </label>

    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">公网协议</span>
      <select
        class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
        bind:value={draft.frp_proxy_type}
      >
        <option value="http">HTTP（FRPS vhostHTTPPort）</option>
        <option value="https2http">HTTPS → 本地 HTTP（FRPS vhostHTTPSPort）</option>
      </select>
      <p class="text-[11px] text-[var(--text-muted)]">
        HTTPS → HTTP 使用 frpc 的 https2http 插件在本机终止 TLS，适合本地服务仅监听 HTTP 的 MCP 与 Actions。
      </p>
    </label>

    {#if draft.frp_proxy_type === "https2http"}
      <label class="grid gap-1">
        <span class="text-xs text-[var(--text-muted)]">证书路径</span>
        <input
          type="text"
          class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-sm"
          placeholder=".anchor/cert/taoyan.icu.pem（留空自动发现）"
          bind:value={draft.frp_cert_path}
        />
      </label>
      <label class="grid gap-1">
        <span class="text-xs text-[var(--text-muted)]">私钥路径</span>
        <input
          type="text"
          class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-sm"
          placeholder=".anchor/cert/taoyan.icu.key（留空自动发现）"
          bind:value={draft.frp_key_path}
        />
        <p class="text-[11px] text-[var(--text-muted)]">
          路径必须位于当前工作区内。两项留空时，Anchor 会从 .anchor/cert 中选择唯一的同名证书与 .key 文件；私钥内容不会写入配置数据库。
        </p>
      </label>
    {/if}

    {#if !useGlobalProfile}
      <button
        type="button"
        class="text-left text-xs text-[var(--primary)] hover:underline"
        onclick={() => {
          manualFrpOpen = !manualFrpOpen;
        }}
      >
        {manualFrpOpen ? "收起" : "展开"}手动 FRP 配置
      </button>
    {/if}

    {#if !useGlobalProfile && manualFrpOpen}
      <label class="grid gap-1">
        <span class="text-xs text-[var(--text-muted)]">FRP 服务器</span>
        <input
          type="text"
          class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-sm"
          placeholder="example.com"
          bind:value={draft.frp_server}
        />
      </label>

      <label class="grid gap-1">
        <span class="text-xs text-[var(--text-muted)]">FRP 服务器端口</span>
        <input
          type="number"
          min="1"
          max="65535"
          class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
          bind:value={draft.frp_server_port}
        />
      </label>

      {#if showManualFrpToken}
        <SecretTokenField
          bind:this={tokenField}
          bind:hasPending={tokenPending}
          {workspaceId}
          secretKey={secretKey}
          label="FRP Token（可选）"
        />
      {/if}
    {/if}
  {/if}

  {#if showCloudflare}
    <label class="grid gap-1">
      <span class="text-xs text-[var(--text-muted)]">Cloudflare 模式</span>
      <select
        class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 text-sm"
        bind:value={draft.cloudflare_mode}
      >
        <option value="quick">Quick Tunnel</option>
        <option value="named">Named Tunnel</option>
      </select>
    </label>

    {#if draft.cloudflare_mode === "quick"}
      <p class="text-xs text-[var(--warning)]">
        Quick Tunnel 仅适合临时测试；服务重启后公网地址可能变化，ChatGPT 中保存的连接不会自动迁移。
      </p>
    {:else}
      <p class="text-xs text-[var(--text-muted)]">
        Named Tunnel 使用固定公网地址，适合长期连接。需要 Tunnel Token 和已配置的公网 URL。
      </p>
    {/if}

    {#if showCloudflareToken}
      <SecretTokenField
        bind:this={tokenField}
        bind:hasPending={tokenPending}
        {workspaceId}
        secretKey={secretKey}
      />
    {/if}
  {/if}

  <label class="grid gap-1">
    <span class="text-xs text-[var(--text-muted)]">
      公网 URL
      {#if service === "actions"}
        <span class="text-[var(--text-muted)]">（OpenAPI 根地址）</span>
      {/if}
    </span>
    <input
      type="url"
      class="rounded-md border border-[var(--border)] bg-[var(--page-bg)] px-2.5 py-1.5 font-mono text-sm"
      placeholder="https://..."
      bind:value={draft.public_url}
    />
    {#if showFrp}
      <p class="text-[11px] text-[var(--text-muted)]">
        例如 https://my-mcp.taoyan.icu。该地址与 FRP 控制服务器地址相互独立；使用 Cloudflare 橙云时，
        控制连接应使用服务器 IP 或 DNS-only 专用域名。
      </p>
    {/if}
  </label>

  <div class="flex justify-end gap-2 pt-1">
    {#if canTest}
      <button
        type="button"
        class="tx-btn-ghost px-3 py-1.5 text-sm disabled:opacity-50"
        disabled={testing || saving}
        onclick={() => void testTunnelConnection()}
      >
        {testing ? "测试中…" : "测试连接"}
      </button>
    {/if}
    <button
      type="submit"
      class="rounded-md bg-[var(--primary)] px-3 py-1.5 text-sm font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-50"
      disabled={saving || testing || !dirty}
    >
      {saving ? "保存中…" : "保存配置"}
    </button>
  </div>
</form>
