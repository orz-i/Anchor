<script lang="ts">
  interface Props {
    config: string;
    onSave: (config: string) => void | Promise<void>;
  }

  const EXAMPLE_CONFIG = `{
  "mcpServers": {
    "codegraph": {
      "type": "stdio",
      "command": "codegraph",
      "args": ["serve", "--mcp", "--path", "\${workspaceFolder}"]
    },
    "remote": {
      "type": "streamable-http",
      "url": "https://mcp.example.com/mcp",
      "headers": {
        "Authorization": "Bearer \${env:REMOTE_MCP_TOKEN}"
      },
      "maxConcurrentRequests": 16
    },
    "browser": {
      "command": "node",
      "args": [
        "C:\\\\Users\\\\mouta\\\\.agents\\\\skills\\\\my-agent-browser\\\\scripts\\\\start-mcp.js"
      ],
      "includeTools": ["navigate", "click", "screenshot", "get_text"],
      "excludeTools": ["evaluate"],
      "maxTools": 12
    }
  }
}`;

  let { config, onSave }: Props = $props();
  let draft = $state("");
  let saving = $state(false);
  let error = $state("");

  const dirty = $derived(draft !== config);

  $effect(() => {
    draft = config;
    error = "";
  });

  function validateAndFormat(value: string): string {
    if (!value.trim()) return "";
    const parsed: unknown = JSON.parse(value);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      throw new Error("配置必须是 JSON 对象");
    }
    return JSON.stringify(parsed, null, 2);
  }

  function formatConfig() {
    try {
      draft = validateAndFormat(draft);
      error = "";
    } catch (cause) {
      error = cause instanceof Error ? cause.message : "JSON 格式无效";
    }
  }

  async function save() {
    if (saving || !dirty) return;
    try {
      const normalized = validateAndFormat(draft);
      saving = true;
      error = "";
      await onSave(normalized);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : "JSON 格式无效";
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
  <label class="grid gap-1.5">
    <span class="text-xs text-[var(--color-text-muted)]">下游 MCP 配置（JSON）</span>
    <textarea
      class="min-h-72 resize-y rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-3 py-2 font-mono text-xs leading-5"
      placeholder={EXAMPLE_CONFIG}
      spellcheck="false"
      bind:value={draft}
    ></textarea>
  </label>

  <p class="text-xs leading-5 text-[var(--color-text-muted)]">
    当前支持 stdio 与 Streamable HTTP MCP。stdio 会在启动 MCP 服务时拉起本地进程；
    Streamable HTTP 支持 JSON/SSE 响应、MCP 会话和自定义认证头。工具会以
    <code class="font-mono">服务器名__工具名</code> 合并到公网 MCP；
    <code class="font-mono">{"${workspaceFolder}"}</code> 会替换为当前工作区路径。
    敏感头建议引用 <code class="font-mono">{"${env:VARIABLE}"}</code>，避免将令牌直接写入配置文件。
    可用 <code class="font-mono">includeTools</code>、<code class="font-mono">excludeTools</code>
    和 <code class="font-mono">maxTools</code> 控制下游工具数量；
    <code class="font-mono">maxConcurrentRequests</code> 用于限制单个下游的并发压力。工具名使用下游 MCP
    原始名称，不包含服务器前缀。
  </p>

  {#if error}
    <p class="text-xs text-[var(--color-danger)]">{error}</p>
  {/if}

  <div class="flex flex-wrap justify-between gap-2 pt-1">
    <div class="flex gap-2">
      <button
        type="button"
        class="rounded-md border border-[var(--color-border)] px-3 py-1.5 text-sm"
        onclick={() => {
          draft = EXAMPLE_CONFIG;
          error = "";
        }}
      >
        填入示例
      </button>
      <button
        type="button"
        class="rounded-md border border-[var(--color-border)] px-3 py-1.5 text-sm"
        onclick={formatConfig}
      >
        格式化 JSON
      </button>
    </div>
    <button
      type="submit"
      class="rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
      disabled={saving || !dirty}
    >
      {saving ? "保存中…" : "保存 MCP 聚合配置"}
    </button>
  </div>
</form>
