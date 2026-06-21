<script lang="ts">
  import { AlertTriangle, CheckCircle2, X, XCircle } from "@lucide/svelte";
  import { fly, fade } from "svelte/transition";

  import { Button } from "$lib/components/ui/button";
  import { errorsState, type UiError } from "$lib/state/errors";

  const toastMeta = {
    info: {
      border: "border-l-emerald-500",
      iconClass: "text-emerald-600 dark:text-emerald-400",
      Icon: CheckCircle2,
    },
    warning: {
      border: "border-l-amber-500",
      iconClass: "text-amber-600 dark:text-amber-400",
      Icon: AlertTriangle,
    },
    error: {
      border: "border-l-destructive",
      iconClass: "text-destructive",
      Icon: XCircle,
    },
  } as const;

  function getToastMeta(level: UiError["level"]) {
    return toastMeta[level];
  }
</script>

<div class="fixed bottom-4 right-4 z-50 flex w-[min(420px,90vw)] flex-col gap-2" role="status" aria-live="polite" aria-atomic="false">
  {#each $errorsState as item (item.id)}
    {@const meta = getToastMeta(item.level)}
    {@const Icon = meta.Icon}
    <div
      class={`rounded-lg border border-l-4 bg-card shadow-lg p-3 flex items-start gap-3 ${meta.border}`}
      in:fly={{ y: 12, duration: 200 }}
      out:fade={{ duration: 150 }}
    >
      <Icon class={`mt-0.5 size-4 shrink-0 ${meta.iconClass}`} />
      <p class="min-w-0 flex-1 text-sm text-card-foreground">{item.message}</p>
      <Button
        variant="ghost"
        size="icon-sm"
        class="shrink-0"
        onclick={() => errorsState.dismissError(item.id)}
        aria-label="Dismiss notification"
      >
        <X class="size-4" />
      </Button>
    </div>
  {/each}
</div>
