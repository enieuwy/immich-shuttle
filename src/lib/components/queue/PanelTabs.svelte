<script lang="ts">
  import { ListChecks, History } from "@lucide/svelte";
  import { panelTab } from "$lib/state/ui";

  const base =
    "inline-flex items-center gap-1.5 rounded-[0.3rem] px-2.5 py-1 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
</script>

<div class="inline-flex items-center gap-0.5 rounded-md bg-muted p-0.5">
  <button
    type="button"
    class={`${base} ${$panelTab === "queue" ? "bg-card text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground"}`}
    aria-pressed={$panelTab === "queue"}
    onclick={() => panelTab.set("queue")}
  >
    <ListChecks class="size-4" /> Queue
  </button>
  <button
    type="button"
    class={`${base} ${$panelTab === "history" ? "bg-card text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground"}`}
    aria-pressed={$panelTab === "history"}
    onclick={() => panelTab.set("history")}
  >
    <History class="size-4" /> History
  </button>
</div>
