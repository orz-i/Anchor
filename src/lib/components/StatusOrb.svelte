<script lang="ts">
  import type { RuntimeState } from "$lib/types";

  interface Props {
    state: RuntimeState;
  }

  let { state }: Props = $props();

  const colorClass: Record<RuntimeState, string> = {
    running: "bg-[var(--success)]",
    starting: "bg-[var(--warning)]",
    recovering: "bg-[var(--warning)]",
    stopping: "bg-[var(--warning)]",
    stopped: "bg-[var(--text-muted)]",
    error: "bg-[var(--danger)]",
  };
</script>

<span
  class="inline-block h-2.5 w-2.5 rounded-full {colorClass[state]}"
  class:animate-pulse={state === "running"}
  aria-label={state}
></span>
