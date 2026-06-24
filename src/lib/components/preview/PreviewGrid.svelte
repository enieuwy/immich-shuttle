<script lang="ts">
  import type { Action } from "svelte/action";
  import { CheckCircle2, Circle, FileVideo, Camera, Image, Loader2, Calendar } from "@lucide/svelte";

  import type { CaptureDate, MediaFile, ThumbResult } from "$lib/types";
  import { selectionState } from "$lib/state/selection";
  import { createThumbnailLoader } from "$lib/state/thumbnailLoader";
  import { previewDates, previewThumbnails } from "$lib/api";
  import { Button } from "$lib/components/ui/button";
  import { cn } from "$lib/utils.js";

  let { files }: { files: MediaFile[] } = $props();

  // Sort mode for the grid. "date" uses EXIF capture date (mtime fallback).
  let sortMode = $state<"name" | "date">("name");
  // Capture dates (epoch seconds) keyed by path; fetched lazily once per file set.
  let dates = $state(new Map<string, number | null>());
  let datesFetchedFor = "";

  $effect(() => {
    const paths = files.map((f) => f.path);
    const key = paths.join("\n");
    if (files.length === 0 || key === datesFetchedFor) {
      return;
    }
    datesFetchedFor = key;
    void previewDates(paths).then((rows: CaptureDate[]) => {
      const next = new Map<string, number | null>();
      for (const row of rows) {
        next.set(row.path, row.captured_at);
      }
      dates = next;
    });
  });

  const sortedFiles = $derived.by(() => {
    const list = [...files];
    if (sortMode === "name") {
      return list.sort((a, b) => a.name.localeCompare(b.name));
    }
    // Newest first; files with no known date sort last, then by name.
    return list.sort((a, b) => {
      const da = dates.get(a.path) ?? null;
      const db = dates.get(b.path) ?? null;
      if (da === null && db === null) return a.name.localeCompare(b.name);
      if (da === null) return 1;
      if (db === null) return -1;
      return db - da;
    });
  });

  // Loaded thumbnails keyed by file path. Reassigned (new Map) on every merge so
  // Svelte reactivity fires for the affected tiles.
  let thumbs = $state(new Map<string, ThumbResult>());

  // --- Selection summary -----------------------------------------------------

  const selStats = $derived.by(() => {
    let count = 0;
    let size = 0;
    for (const f of files) {
      if ($selectionState.selected.has(f.path)) {
        count++;
        size += f.size_bytes;
      }
    }
    return { count, size };
  });

  function fmtSize(bytes: number): string {
    const mb = bytes / 1024 / 1024;
    if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
    return `${Math.round(mb)} MB`;
  }

  function selectAll() {
    selectionState.selectOnly(files.map((f) => f.path));
  }

  // --- Placeholder typing ----------------------------------------------------

  // immich-shuttle stores extensions dotted + lowercased (e.g. ".cr3").
  const RAW_EXTS = new Set([
    ".cr3", ".cr2", ".nef", ".arw", ".raf", ".rw2", ".orf", ".dng", ".heic", ".heif",
  ]);

  function isRaw(extension: string): boolean {
    const e = extension.toLowerCase();
    return RAW_EXTS.has(e.startsWith(".") ? e : `.${e}`);
  }

  function extLabel(extension: string): string {
    return extension.replace(/^\./, "").toUpperCase();
  }

  // --- Lazy, ordered, incremental thumbnail loading --------------------------

  // Loads requested tiles in small ordered chunks, merging each chunk the moment
  // it resolves so tiles paint incrementally top-down and one slow RAW/video
  // can't gate the whole viewport.
  const loader = createThumbnailLoader({
    fetch: previewThumbnails,
    onResults: (results) => {
      const next = new Map(thumbs);
      for (const r of results) next.set(r.path, r);
      thumbs = next;
    },
  });

  let observer: IntersectionObserver | null = null;
  const tileNodes = new Set<HTMLElement>();
  const nodePath = new WeakMap<HTMLElement, string>();

  const observeTile: Action<HTMLElement, string> = (node, path) => {
    nodePath.set(node, path);
    tileNodes.add(node);
    observer?.observe(node);
    return {
      update(nextPath: string) {
        nodePath.set(node, nextPath);
      },
      destroy() {
        observer?.unobserve(node);
        tileNodes.delete(node);
        nodePath.delete(node);
      },
    };
  };

  $effect(() => {
    const obs = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          const path = nodePath.get(entry.target as HTMLElement);
          if (path) loader.request(path);
        }
      },
      { rootMargin: "200px" },
    );
    observer = obs;
    // Observe any tiles that mounted before this effect ran.
    for (const node of tileNodes) obs.observe(node);
    return () => {
      obs.disconnect();
      observer = null;
      loader.dispose();
    };
  });
</script>

{#if files.length > 0}
  <div class="flex h-full flex-col">
    <div
      class="sticky top-0 z-10 flex items-center gap-2 border-b border-border bg-card/95 px-3 py-2 backdrop-blur"
    >
      <Button variant="outline" size="sm" onclick={selectAll}>Select all</Button>
      <Button variant="ghost" size="sm" onclick={() => selectionState.clear()}>Clear</Button>
      <div class="ml-auto flex items-center gap-1">
        <span class="text-[11px] text-muted-foreground">Sort</span>
        <Button
          variant={sortMode === "name" ? "secondary" : "ghost"}
          size="sm"
          onclick={() => (sortMode = "name")}
        >
          Name
        </Button>
        <Button
          variant={sortMode === "date" ? "secondary" : "ghost"}
          size="sm"
          onclick={() => (sortMode = "date")}
        >
          <Calendar class="h-3.5 w-3.5" /> Date
        </Button>
      </div>
      <span class="text-xs text-muted-foreground">
        {selStats.count} of {files.length} selected · {fmtSize(selStats.size)}
      </span>
    </div>

    <div class="flex-1 overflow-y-auto p-3">
      <div class="grid grid-cols-[repeat(auto-fill,minmax(140px,1fr))] gap-2">
        {#each sortedFiles as file (file.path)}
          {@const selected = $selectionState.selected.has(file.path)}
          {@const thumb = thumbs.get(file.path)}
          <button
            type="button"
            use:observeTile={file.path}
            onclick={() => selectionState.toggle(file.path)}
            aria-pressed={selected}
            aria-label={`Select ${file.name}`}
            class={cn(
              "group relative aspect-square overflow-hidden rounded-md border border-border bg-muted/40 text-left transition-shadow [content-visibility:auto] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
              selected && "ring-2 ring-primary",
            )}
          >
            {#if thumb?.data_url}
              <img
                src={thumb.data_url}
                alt={file.name}
                loading="lazy"
                draggable="false"
                class="h-full w-full rounded-md object-cover"
              />
            {:else if thumb}
              <div
                class="flex h-full w-full flex-col items-center justify-center gap-1.5 text-muted-foreground"
              >
                {#if file.is_video}
                  <FileVideo class="h-7 w-7" />
                {:else if isRaw(file.extension)}
                  <Camera class="h-7 w-7" />
                {:else}
                  <Image class="h-7 w-7" />
                {/if}
                <span class="text-[10px] font-medium uppercase tracking-wide">
                  {extLabel(file.extension)}
                </span>
              </div>
            {:else}
              <div class="flex h-full w-full items-center justify-center text-muted-foreground">
                <Loader2 class="h-6 w-6 animate-spin" />
              </div>
            {/if}

            <div
              class="pointer-events-none absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/60 via-black/20 to-transparent px-1.5 pb-1 pt-5"
            >
              <p class="truncate text-[10px] font-medium text-white">{file.name}</p>
            </div>

            <span class="pointer-events-none absolute right-1.5 top-1.5 drop-shadow">
              {#if selected}
                <CheckCircle2 class="h-5 w-5 fill-primary text-primary-foreground" />
              {:else}
                <Circle
                  class="h-5 w-5 text-white/70 opacity-0 transition-opacity group-hover:opacity-100"
                />
              {/if}
            </span>
          </button>
        {/each}
      </div>
    </div>
  </div>
{/if}
