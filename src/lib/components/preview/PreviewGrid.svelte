<script lang="ts">
  import type { Action } from "svelte/action";
  import {
    CheckCircle2,
    Circle,
    FileVideo,
    Camera,
    Image,
    Loader2,
    Calendar,
    CalendarRange,
  } from "@lucide/svelte";

  import type { CaptureDate, MediaFile, ThumbResult } from "$lib/types";
  import { selectionState } from "$lib/state/selection";
  import { createThumbnailLoader } from "$lib/state/thumbnailLoader";
  import {
    dayEndEpoch,
    dayStartEpoch,
    filterFiles,
    presetRange,
    type DatePreset,
    type MediaTypeFilter,
  } from "$lib/state/previewFilter";
  import { previewDates, previewThumbnails } from "$lib/api";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { cn } from "$lib/utils.js";

  let { files }: { files: MediaFile[] } = $props();

  // Sort mode for the grid. "date" uses EXIF capture date (mtime fallback).
  let sortMode = $state<"name" | "date">("name");
  // Capture dates (epoch seconds) keyed by path; fetched lazily once per file set.
  let dates = $state(new Map<string, number | null>());
  let datesFetchedFor = "";
  let datesGeneration = 0;


  $effect(() => {
    const paths = files.map((f) => f.path);
    const key = paths.join("\n");
    if (key === datesFetchedFor) {
      return;
    }
    datesFetchedFor = key;
    const generation = ++datesGeneration;
    // Tauri's invoke promises cannot be cancelled. Dispose the loader to halt
    // queued chunks and use the generation below to drop the pending IPC result.
    loader.dispose();
    thumbs = new Map();
    if (files.length === 0) {
      dates = new Map();
      return;
    }
    loader = makeLoader();
    let disposed = false;
    void previewDates(paths)
      .then((rows: CaptureDate[]) => {
        // A newer file set may have superseded this fetch; only apply the dates if
        // the key we requested for is still the active dataset.
        if (disposed || generation !== datesGeneration || key !== datesFetchedFor) return;
        const next = new Map<string, number | null>();
        for (const row of rows) {
          next.set(row.path, row.captured_at);
        }
        dates = next;
      })
      .catch(() => {
        // Preview metadata is optional; a failed or discarded IPC result leaves
        // date sorting/filtering on its existing fallback behavior.
      });
    return () => {
      disposed = true;
      loader.dispose();
    };
  });

  // --- Filters ---------------------------------------------------------------

  let typeFilter = $state<MediaTypeFilter>("all");
  // The date inputs are the source of truth for the date window; presets simply
  // fill them, and they stay keyboard-editable so a date can be typed directly.
  let fromInput = $state("");
  let toInput = $state("");

  const fromEpoch = $derived(fromInput ? dayStartEpoch(fromInput) : null);
  const toEpoch = $derived(toInput ? dayEndEpoch(toInput) : null);
  const invalidRange = $derived(
    fromEpoch !== null && toEpoch !== null && fromEpoch > toEpoch,
  );

  const activePreset = $derived.by<DatePreset>(() => {
    if (!fromInput && !toInput) return "all";
    for (const p of ["7d", "30d", "year"] as const) {
      const r = presetRange(p);
      if (r && r.from === fromInput && r.to === toInput) return p;
    }
    return "custom";
  });

  function applyPreset(preset: DatePreset) {
    if (preset === "all") {
      fromInput = "";
      toInput = "";
      return;
    }
    const r = presetRange(preset);
    if (r) {
      fromInput = r.from;
      toInput = r.to;
    }
  }

  const visibleFiles = $derived(
    filterFiles(files, dates, {
      type: typeFilter,
      // Ignore a backwards range rather than hiding everything.
      fromEpoch: invalidRange ? null : fromEpoch,
      toEpoch: invalidRange ? null : toEpoch,
    }),
  );

  const sortedFiles = $derived.by(() => {
    const list = [...visibleFiles];
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

  const filterActive = $derived(typeFilter !== "all" || fromInput !== "" || toInput !== "");

  function fmtSize(bytes: number): string {
    const mb = bytes / 1024 / 1024;
    if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
    return `${Math.round(mb)} MB`;
  }

  function selectAllVisible() {
    selectionState.add(visibleFiles.map((f) => f.path));
  }

  function invertVisible() {
    selectionState.invert(visibleFiles.map((f) => f.path));
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
  function makeLoader() {
    return createThumbnailLoader({
      fetch: previewThumbnails,
      onResults: (results) => {
        const next = new Map(thumbs);
        for (const r of results) next.set(r.path, r);
        thumbs = next;
      },
    });
  }
  let loader = makeLoader();

  // Loaded thumbnails keyed by file path. Reassigned (new Map) on every merge so
  // Svelte reactivity fires for the affected tiles.
  let thumbs = $state(new Map<string, ThumbResult>());

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
      class="sticky top-0 z-10 flex flex-col gap-2 border-b border-border bg-card/95 px-3 py-2 backdrop-blur"
    >
      <!-- Filters -->
      <div class="flex flex-wrap items-center gap-x-3 gap-y-2">
        <div class="flex items-center gap-1">
          <Button
            variant={typeFilter === "all" ? "secondary" : "ghost"}
            size="sm"
            onclick={() => (typeFilter = "all")}
          >
            All
          </Button>
          <Button
            variant={typeFilter === "photo" ? "secondary" : "ghost"}
            size="sm"
            onclick={() => (typeFilter = "photo")}
          >
            <Image class="h-3.5 w-3.5" /> Photos
          </Button>
          <Button
            variant={typeFilter === "video" ? "secondary" : "ghost"}
            size="sm"
            onclick={() => (typeFilter = "video")}
          >
            <FileVideo class="h-3.5 w-3.5" /> Videos
          </Button>
        </div>

        <div class="flex items-center gap-1">
          <CalendarRange class="h-3.5 w-3.5 text-muted-foreground" />
          <Button
            variant={activePreset === "all" ? "secondary" : "ghost"}
            size="sm"
            onclick={() => applyPreset("all")}
          >
            All dates
          </Button>
          <Button
            variant={activePreset === "7d" ? "secondary" : "ghost"}
            size="sm"
            onclick={() => applyPreset("7d")}
          >
            7d
          </Button>
          <Button
            variant={activePreset === "30d" ? "secondary" : "ghost"}
            size="sm"
            onclick={() => applyPreset("30d")}
          >
            30d
          </Button>
          <Button
            variant={activePreset === "year" ? "secondary" : "ghost"}
            size="sm"
            onclick={() => applyPreset("year")}
          >
            Year
          </Button>
        </div>

        <div class="flex items-center gap-1">
          <Input
            type="date"
            class="h-8 w-[8.75rem]"
            bind:value={fromInput}
            aria-label="From date"
            aria-invalid={invalidRange}
          />
          <span class="text-xs text-muted-foreground">to</span>
          <Input
            type="date"
            class="h-8 w-[8.75rem]"
            bind:value={toInput}
            aria-label="To date"
            aria-invalid={invalidRange}
          />
        </div>
      </div>

      <!-- Actions + count -->
      <div class="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" onclick={selectAllVisible}>
          {filterActive ? "Select shown" : "Select all"}
        </Button>
        <Button variant="ghost" size="sm" onclick={invertVisible}>Invert</Button>
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
      </div>

      <div class="text-xs text-muted-foreground">
        {#if invalidRange}
          <span class="text-destructive">From date must be on or before To date.</span>
        {:else}
          Showing {visibleFiles.length} of {files.length} · {selStats.count} selected · {fmtSize(
            selStats.size,
          )}
        {/if}
      </div>
    </div>

    <div class="flex-1 overflow-y-auto p-3">
      {#if sortedFiles.length === 0}
        <div
          class="flex h-full flex-col items-center justify-center gap-1 text-center text-sm text-muted-foreground"
        >
          <CalendarRange class="h-7 w-7" />
          <p>No files match the current filter.</p>
        </div>
      {:else}
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
      {/if}
    </div>
  </div>
{/if}
