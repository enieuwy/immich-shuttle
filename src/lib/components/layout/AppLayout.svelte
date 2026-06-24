<script lang="ts">
  import type { Snippet } from "svelte";
  import logoUrl from "$lib/assets/logo.png";

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
  <header class="sticky top-0 z-10 border-b border-border bg-card px-4 py-3" data-tauri-drag-region>
    <div class="flex w-full items-center justify-between gap-4" data-titlebar data-tauri-drag-region>
      {#if brand}
        {@render brand()}
      {:else}
        <div class="flex min-w-0 items-center gap-2.5" data-tauri-drag-region>
          <img src={logoUrl} alt="" class="size-7 shrink-0 rounded-lg" draggable="false" />
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
    </div>
  </header>

  <main class="overflow-auto p-5">
    <div class="mx-auto w-full max-w-6xl">
      {@render children?.()}
    </div>
  </main>

  <footer class="w-full border-t border-border bg-card px-4 py-2 text-sm">
    <div class="w-full">
      {#if footer}
        {@render footer()}
      {:else}
        <span class="text-muted-foreground">Ready to import</span>
      {/if}
    </div>
  </footer>
</div>
