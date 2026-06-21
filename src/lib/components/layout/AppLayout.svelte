<script lang="ts">
  import type { Snippet } from "svelte";
  import { Send } from "@lucide/svelte";

  let {
    title = "Immich Shuttle",
    brand,
    profile,
    children,
    footer,
    actions,
  } = $props<{
    title?: string;
    brand?: Snippet;
    profile?: Snippet;
    children?: Snippet;
    footer?: Snippet;
    actions?: Snippet;
  }>();
</script>

<div class="grid min-h-screen grid-rows-[auto_1fr_auto] bg-background text-foreground">
  <header class="sticky top-0 z-10 flex items-center justify-between gap-4 border-b border-border bg-card px-4 py-3">
    {#if brand}
      {@render brand()}
    {:else}
      <div class="flex min-w-0 items-center gap-2.5">
        <div class="flex size-7 shrink-0 items-center justify-center rounded-lg brand-gradient">
          <Send class="size-4 text-white" />
        </div>
        <span class="truncate text-[0.95rem] font-semibold tracking-tight text-card-foreground">{title}</span>
      </div>
    {/if}
    <div class="flex items-center gap-3">
      {#if profile}
        <div class="flex items-center">
          {@render profile()}
        </div>
      {/if}
      {#if actions}
        <div class="flex items-center">
          {@render actions()}
        </div>
      {/if}
    </div>
  </header>

  <main class="overflow-auto p-5">
    {@render children?.()}
  </main>

  <footer class="w-full border-t border-border bg-card px-4 py-2 text-sm">
    {#if footer}
      {@render footer()}
    {:else}
      <span class="text-muted-foreground">Ready to import</span>
    {/if}
  </footer>
</div>
