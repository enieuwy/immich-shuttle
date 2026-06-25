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

<div class="relative isolate grid min-h-screen grid-rows-[auto_1fr_auto] bg-background text-foreground">
  <div
    class="pointer-events-none absolute inset-0 -z-10 opacity-0 transition-opacity dark:opacity-100"
    aria-hidden="true"
    style="background: radial-gradient(110% 70% at 50% -8%, oklch(0.5 0.17 273 / 0.16), transparent 60%), radial-gradient(80% 55% at 100% 0%, oklch(0.74 0.14 196 / 0.07), transparent 55%);"
  ></div>
  <header class="relative sticky top-0 z-10 border-b border-border bg-card px-4 py-3" data-tauri-drag-region>
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
    <div class="brand-gradient pointer-events-none absolute inset-x-0 bottom-0 h-px opacity-60" aria-hidden="true"></div>
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
