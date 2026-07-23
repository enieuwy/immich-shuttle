<script lang="ts">
  import { onMount } from "svelte";

  import { get } from "svelte/store";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open } from "@tauri-apps/plugin-dialog";
  import { FolderOpen, FileImage, HardDrive, History, LayoutGrid, Loader2, X, Zap, ChevronRight } from "@lucide/svelte";

  import { sourceState } from "$lib/state/source";
  import { autoImportState } from "$lib/state/auto-import";
  import { importOptionsState, isDateRangeInvalid } from "$lib/state/import-options";
  import { previewState } from "$lib/state/preview";
  import { selectionState } from "$lib/state/selection";
  import { historyState } from "$lib/state/history";
  import { historySourceLastImport } from "$lib/api";
  import { activeProfile } from "$lib/state/profiles";
  import type { RemovableDevice } from "$lib/types";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { Badge } from "$lib/components/ui/badge";
  import {
    Card,
    CardContent,
    CardHeader,
    CardTitle,
  } from "$lib/components/ui/card";
  import { Label } from "$lib/components/ui/label";
  import { Switch } from "$lib/components/ui/switch";
  import DeviceRuleControl from "$lib/components/source/DeviceRuleControl.svelte";

  // Pre-filter: coarse, server-side narrowing of the source (type / date /
  // extension) applied before you preview and hand-pick. Bound to the shared
  // import options so the values flow straight to the importer.
  let preFilterOpen = $state(false);
  const filterDateFrom = $derived($importOptionsState.dateFrom ?? "");
  const filterDateTo = $derived($importOptionsState.dateTo ?? "");
  const filterDateInvalid = $derived(isDateRangeInvalid(filterDateFrom, filterDateTo));
  const includeExtText = $derived($importOptionsState.includeExtensions.join(", "));
  const excludeExtText = $derived($importOptionsState.excludeExtensions.join(", "));
  function parseExtensions(raw: string): string[] {
    return raw
      .split(",")
      .map((e) => e.trim().replace(/^\.+/, "").toLowerCase())
      .filter((e) => e.length > 0)
      .map((e) => `.${e}`);
  }
  const mediaTypes: Array<{ value: "all" | "image" | "video"; label: string }> = [
    { value: "all", label: "All" },
    { value: "image", label: "Photos" },
    { value: "video", label: "Videos" },
  ];
  const activeFilterCount = $derived(
    ($importOptionsState.mediaType !== "all" ? 1 : 0) +
      (filterDateFrom !== "" || filterDateTo !== "" ? 1 : 0) +
      ($importOptionsState.onlyNewSinceLastImport ? 1 : 0) +
      ($importOptionsState.includeExtensions.length > 0 ? 1 : 0) +
      ($importOptionsState.excludeExtensions.length > 0 ? 1 : 0),
  );

  let manualPath = $state("");
  let showPathInput = $state(false);

  let lastImportedAt = $state<number | null>(null);

  // Count of previewed-and-selected files that still belong to the current scan.
  const selectedCount = $derived.by(() => {
    const files = $sourceState.scanResult?.files ?? [];
    if (files.length === 0) return 0;
    const valid = new Set(files.map((f) => f.path));
    let n = 0;
    for (const path of $selectionState.selected) {
      if (valid.has(path)) n++;
    }
    return n;
  });

  $effect(() => {
    const lastImportVersion = $historyState.lastImportVersion;
    const paths = $sourceState.selectedPaths;
    const profile = $activeProfile;
    if (paths.length === 0 || !profile) {
      lastImportedAt = null;
      return;
    }
    lastImportedAt = null;
    let cancelled = false;
    void historySourceLastImport(profile.id, paths).then((ms) => {
      if (!cancelled && lastImportVersion === $historyState.lastImportVersion) {
        lastImportedAt = ms;
      }
    });
    return () => {
      cancelled = true;
    };
  });

  onMount(() => {
    let disposed = false;
    let unlistenDevice: (() => void) | undefined;
    let unlistenDrop: (() => void) | undefined;

    void sourceState.loadDevices().then(() => {
      autoImportState.observe(get(sourceState).detectedDevices);
    });
    void listen<RemovableDevice[]>("device-changed", (event) => {
      if (Array.isArray(event.payload)) {
        void sourceState.loadDevices().then(() => {
          autoImportState.observe(get(sourceState).detectedDevices);
        });
      }
    }).then((fn) => {
      // If the component unmounted before the listener resolved, drop it now
      // instead of leaving a stale handler attached for the process lifetime.
      if (disposed) fn();
      else unlistenDevice = fn;
    });

    void getCurrentWindow()
      .onDragDropEvent((event) => {
        if (event.payload.type !== "drop") {
          return;
        }
        const paths = event.payload.paths;
        if (paths.length > 0) {
          void sourceState.selectSources(paths);
        }
      })
      .then((fn) => {
        if (disposed) fn();
        else unlistenDrop = fn;
      });

    return () => {
      disposed = true;
      unlistenDevice?.();
      unlistenDrop?.();
    };
  });

  async function chooseFolder() {
    const selected = await open({ directory: true, multiple: false });
    if (!selected || Array.isArray(selected)) {
      return;
    }
    await sourceState.selectSources([selected]);
  }

  async function chooseFiles() {
    const selected = await open({
      multiple: true,
      filters: [{
        name: "Media",
        extensions: [
          "jpg", "jpeg", "png", "heic", "heif", "avif", "tiff", "tif", "gif", "bmp",
          "webp", "raw", "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf",
          "mp4", "mov", "m4v", "avi", "mkv",
        ],
      }],
    });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    if (paths.length > 0) {
      await sourceState.selectSources(paths);
    }
  }

  async function chooseManualPath() {
    if (!manualPath.trim()) {
      return;
    }
    await sourceState.selectSources([manualPath.trim()]);
    manualPath = "";
  }

  function openPreview() {
    const files = $sourceState.scanResult?.files ?? [];
    // Default to everything selected; the grid is for de-selecting what you don't want.
    selectionState.selectOnly(files.map((f) => f.path));
    previewState.open();
  }

  function sourceLabel(paths: string[]): string {
    if (paths.length === 0) return "";
    if (paths.length === 1) return paths[0];
    const allFiles = paths.every((p) => !p.endsWith("/"));
    if (allFiles) return `${paths.length} files selected`;
    return `${paths.length} sources selected`;
  }

  function isLikelyFolderPath(path: string): boolean {
    return path.endsWith("/") || !/\.[^/\\]+$/.test(path);
  }

  function fmtGb(bytes: number): string {
    return `${Math.round(bytes / 1024 ** 3)} GB`;
  }

  function fmtSize(bytes: number): string {
    const mb = bytes / 1024 / 1024;
    if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
    return `${Math.round(mb)} MB`;
  }

  function fmtRelative(ms: number): string {
    const mins = Math.round((Date.now() - ms) / 60000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.round(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    return `${Math.round(hours / 24)}d ago`;
  }
</script>

<Card>
  <CardHeader>
    <CardTitle class="flex items-center gap-2 text-sm font-semibold">
      <span class="flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <HardDrive class="h-4 w-4" />
      </span>
      Source
    </CardTitle>
  </CardHeader>
  <CardContent class="flex flex-col gap-4">
    {#if $sourceState.selectedPaths.length > 0}
      <div class="flex items-start gap-3 rounded-lg bg-muted/50 px-3 py-3">
        {#if $sourceState.selectedPaths.length === 1}
          <FolderOpen class="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
        {:else}
          <FileImage class="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
        {/if}
        <div class="min-w-0 flex-1 space-y-2">
          <p
            class="truncate text-sm font-medium text-foreground"
            title={sourceLabel($sourceState.selectedPaths)}
          >
            {sourceLabel($sourceState.selectedPaths)}
          </p>
          {#if $sourceState.selectedPaths.length > 1}
            <div class="flex flex-col gap-1">
              {#each $sourceState.selectedPaths as path}
                <div class="flex items-center gap-2 rounded-md bg-background/60 px-2 py-1">
                  {#if isLikelyFolderPath(path)}
                    <FolderOpen class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                  {:else}
                    <FileImage class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                  {/if}
                  <span class="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground" title={path}>
                    {path}
                  </span>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onclick={() => void sourceState.removePath(path)}
                    class="h-5 w-5 shrink-0 rounded-full"
                    aria-label={`Remove ${path}`}
                  >
                    <X class="h-3.5 w-3.5" />
                  </Button>
                </div>
              {/each}
            </div>
          {/if}
          {#if $sourceState.scanning}
            <div class="flex items-center gap-2 text-xs text-muted-foreground">
              <p class="flex items-center gap-1.5">
                <Loader2 class="h-3.5 w-3.5 animate-spin" />
                Scanning… {($sourceState.scanResult?.photo_count ?? 0) + ($sourceState.scanResult?.video_count ?? 0)} found
              </p>
              <Button
                variant="ghost"
                size="sm"
                class="h-6 px-2 text-xs"
                onclick={() => void sourceState.cancelScan()}
              >
                Cancel
              </Button>
            </div>
          {:else if $sourceState.scanResult}
            <div class="flex flex-wrap items-center gap-1.5">
              <span class="rounded bg-muted px-2 py-0.5 text-xs tabular-nums text-foreground">
                {$sourceState.scanResult.photo_count} photos
              </span>
              <span class="rounded bg-muted px-2 py-0.5 text-xs tabular-nums text-foreground">
                {$sourceState.scanResult.video_count} videos
              </span>
              <span class="rounded bg-muted px-2 py-0.5 text-xs tabular-nums text-foreground">
                {fmtSize($sourceState.scanResult.total_size_bytes)} total
              </span>
            </div>
            {#if $sourceState.scanResult.skipped_unreadable > 0}
              <p class="text-xs text-muted-foreground">
                {$sourceState.scanResult.skipped_unreadable} unreadable skipped
              </p>
            {/if}
          {/if}
            {#if lastImportedAt !== null}
              <p
                class="flex items-center gap-1.5 text-xs text-muted-foreground"
                title="immich-go skips files already on the server by checksum"
              >
                <History class="h-3.5 w-3.5 shrink-0" />
                Imported from here {fmtRelative(lastImportedAt)} — already-uploaded files
                are skipped automatically
              </p>
            {/if}
        </div>
        <Button
          variant="ghost"
          size="icon-sm"
          onclick={() => sourceState.clearSource()}
          class="h-6 w-6 shrink-0 rounded-full"
          aria-label="Clear selected sources"
        >
          <X class="h-4 w-4" />
        </Button>
      </div>

    {#if $sourceState.scanResult && $sourceState.scanResult.files.length > 0}
      <section class="flex flex-col gap-1 rounded-lg border border-border/60 p-1">
        <button
          type="button"
          class="flex items-center gap-1.5 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-muted/50"
          aria-expanded={preFilterOpen}
          onclick={() => (preFilterOpen = !preFilterOpen)}
        >
          <ChevronRight class="size-3.5 text-muted-foreground transition-transform {preFilterOpen ? 'rotate-90' : ''}" />
          <span class="text-sm font-medium text-foreground">Pre-filter</span>
          {#if activeFilterCount > 0}
            <span class="rounded-full bg-primary/15 px-1.5 text-[10px] font-semibold text-primary">
              {activeFilterCount} active
            </span>
          {/if}
        </button>
        <p class="px-2 pb-1 text-xs text-muted-foreground">
          Narrow the source by type, date, or extension before you preview — trims the upload without loading thumbnails.
        </p>
        {#if preFilterOpen}
          <div class="rounded-lg p-3">
            <div class="flex min-w-0 flex-col items-start gap-1">
              <span class="text-sm font-medium text-foreground">Media type</span>
              <span class="text-xs text-muted-foreground">Import only one kind of media, or both.</span>
            </div>
            <div class="mt-2 flex gap-2" role="group" aria-label="Media type filter">
              {#each mediaTypes as { value, label } (value)}
                <Button
                  variant={$importOptionsState.mediaType === value ? "default" : "outline"}
                  size="sm"
                  aria-pressed={$importOptionsState.mediaType === value}
                  onclick={() => importOptionsState.setMediaType(value)}
                >
                  {label}
                </Button>
              {/each}
            </div>
          </div>

          <div class="rounded-lg p-3">
            <div class="flex items-start justify-between gap-3">
              <div class="flex min-w-0 flex-col items-start gap-1">
                <span class="text-sm font-medium text-foreground">Capture date range</span>
                <span class="text-xs text-muted-foreground">Only import files captured between these dates. Leave blank to import all.</span>
              </div>
              {#if filterDateFrom !== "" || filterDateTo !== ""}
                <button
                  type="button"
                  class="shrink-0 text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
                  onclick={() => importOptionsState.clearDateRange()}
                >
                  Clear
                </button>
              {/if}
            </div>
            <div class="mt-2 flex items-center gap-2">
              <Label for="prefilter-date-from" class="sr-only">From date</Label>
              <Input
                id="prefilter-date-from"
                class="w-40 shrink-0"
                type="date"
                aria-label="From date"
                aria-invalid={filterDateInvalid}
                max={filterDateTo || undefined}
                value={filterDateFrom}
                onchange={(e) => importOptionsState.setDateFrom(e.currentTarget.value)}
              />
              <span class="text-xs text-muted-foreground">to</span>
              <Label for="prefilter-date-to" class="sr-only">To date</Label>
              <Input
                id="prefilter-date-to"
                class="w-40 shrink-0"
                type="date"
                aria-label="To date"
                aria-invalid={filterDateInvalid}
                min={filterDateFrom || undefined}
                value={filterDateTo}
                onchange={(e) => importOptionsState.setDateTo(e.currentTarget.value)}
              />
            </div>
            {#if filterDateInvalid}
              <p class="mt-2 text-xs text-destructive">The start date must be on or before the end date.</p>
            {/if}
          </div>

          <div class="rounded-lg p-3">
            <div class="flex items-center justify-between gap-3">
              <Label
                for="prefilter-only-new"
                class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
              >
                <span class="text-sm font-medium text-foreground">Only media newer than last import</span>
                <span class="text-xs text-muted-foreground">Skips a re-scan of already-imported files by filtering to a capture-date floor.</span>
              </Label>
              <Switch
                id="prefilter-only-new"
                aria-label="Only media newer than last import"
                checked={$importOptionsState.onlyNewSinceLastImport}
                onCheckedChange={(v) => importOptionsState.setOnlyNewSinceLastImport(v)}
              />
            </div>
            {#if $importOptionsState.onlyNewSinceLastImport}
              <p class="mt-2 text-xs text-muted-foreground">
                Filters by EXIF capture date, not when files were added — a wrong camera clock or back-dated files may be skipped. Server-side dedupe still guards the boundary.
              </p>
            {/if}
          </div>

          <div class="rounded-lg p-3">
            <Label
              for="prefilter-include-ext"
              class="flex min-w-0 flex-col items-start gap-1 font-normal"
            >
              <span class="text-sm font-medium text-foreground">Only these extensions</span>
              <span class="text-xs text-muted-foreground">Comma-separated (e.g. jpg, heic). Leave empty for all.</span>
            </Label>
            <Input
              id="prefilter-include-ext"
              class="mt-2"
              placeholder="jpg, heic, mp4"
              aria-label="Only these extensions"
              value={includeExtText}
              onchange={(e) => importOptionsState.setIncludeExtensions(parseExtensions(e.currentTarget.value))}
            />
          </div>

          <div class="rounded-lg p-3">
            <Label
              for="prefilter-exclude-ext"
              class="flex min-w-0 flex-col items-start gap-1 font-normal"
            >
              <span class="text-sm font-medium text-foreground">Exclude extensions</span>
              <span class="text-xs text-muted-foreground">Comma-separated (e.g. gif, aae) to skip.</span>
            </Label>
            <Input
              id="prefilter-exclude-ext"
              class="mt-2"
              placeholder="gif, aae"
              aria-label="Exclude extensions"
              value={excludeExtText}
              onchange={(e) => importOptionsState.setExcludeExtensions(parseExtensions(e.currentTarget.value))}
            />
          </div>
        {/if}
      </section>

      <div class="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" class="w-fit" onclick={openPreview}>
          <LayoutGrid class="h-3.5 w-3.5" /> Preview &amp; select
        </Button>
        {#if selectedCount > 0}
          <span class="text-xs font-medium text-foreground">
            {selectedCount} of {$sourceState.scanResult.files.length} selected
          </span>
          <Button
            variant="ghost"
            size="sm"
            class="h-7 px-2 text-xs text-muted-foreground"
            onclick={() => selectionState.clear()}
          >
            Clear
          </Button>
        {/if}
      </div>
    {/if}
    {:else}
      {#if $sourceState.loadingDevices}
        <p class="text-sm text-muted-foreground">Scanning devices…</p>
      {:else if $sourceState.detectedDevices.length > 0}
        <div class="flex flex-col gap-1.5">
          <h4 class="text-xs font-medium text-muted-foreground">Removable devices</h4>
          {#each $sourceState.detectedDevices as device}
            {@const used = Math.max(0, device.total_space - device.available_space)}
            {@const pct = device.total_space > 0 ? Math.min(100, Math.round((used / device.total_space) * 100)) : 0}
            <button
              type="button"
              class="flex w-full flex-col gap-2.5 rounded-lg border border-border p-3 text-left transition-all hover:border-primary/40 hover:bg-accent"
              onclick={() => sourceState.selectSources([device.mount_path])}
            >
              <div class="flex w-full items-center gap-3">
                <HardDrive class="h-4 w-4 shrink-0 text-muted-foreground" />
                <div class="min-w-0 flex-1">
                  <div class="flex items-center gap-2">
                    <span class="truncate text-sm font-medium text-foreground">{device.name}</span>
                    {#if device.has_dcim}
                      <Badge variant="secondary">DCIM</Badge>
                    {/if}
                  </div>
                  <p class="truncate text-xs text-muted-foreground">{device.mount_path}</p>
                </div>
                <span class="shrink-0 text-xs text-muted-foreground tabular-nums">
                  {fmtGb(device.available_space)} free of {fmtGb(device.total_space)}
                </span>
              </div>
              <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                <div
                  class="h-full rounded-full transition-[width] duration-500"
                  style="width: {pct}%; background: {pct > 90
                    ? 'oklch(0.7 0.19 25)'
                    : 'linear-gradient(90deg, oklch(0.74 0.14 196), oklch(0.55 0.18 273))'};"
                ></div>
              </div>
            </button>
          {/each}
        </div>
      {/if}

      <div
        class="group flex flex-col items-center gap-3 rounded-xl border-2 border-dashed border-primary/25 bg-primary/[0.02] px-4 py-7 text-center transition-all hover:border-primary/60 hover:bg-primary/[0.06]"
      >
        <div class="brand-gradient flex h-12 w-12 items-center justify-center rounded-full shadow-lg shadow-primary/20 transition-transform group-hover:scale-105">
          <FileImage class="h-6 w-6 text-white" />
        </div>
        <div class="space-y-0.5">
          <p class="text-sm font-medium text-foreground">Drag photos or a folder here</p>
          <p class="text-xs text-muted-foreground">or choose manually</p>
        </div>
        <div class="flex items-center gap-2">
          <Button variant="outline" size="sm" onclick={chooseFiles}>
            <FileImage class="mr-2 h-4 w-4" />
            Choose files...
          </Button>
          <Button variant="outline" size="sm" onclick={chooseFolder}>
            <FolderOpen class="mr-2 h-4 w-4" />
            Choose folder...
          </Button>
        </div>
      </div>

      {#if showPathInput}
        <div class="flex items-center gap-2">
          <Input bind:value={manualPath} placeholder="/path/to/photos" class="flex-1" />
          <Button variant="outline" size="sm" onclick={chooseManualPath}>Use path</Button>
        </div>
      {:else}
        <button
          type="button"
          class="self-start text-xs text-muted-foreground underline-offset-2 transition-colors hover:text-foreground hover:underline"
          onclick={() => (showPathInput = true)}
        >
          Enter a path manually
        </button>
      {/if}
    {/if}

    {#if $sourceState.error}
      <p class="text-sm text-destructive">{$sourceState.error}</p>
    {/if}

    <div class="flex flex-col gap-1 border-t border-border/60 pt-3">
      <div class="flex items-center justify-between gap-3 rounded-lg p-2 transition-colors hover:bg-muted/50">
        <Label
          for="auto-import-toggle"
          class="flex min-w-0 flex-col items-start gap-0.5 cursor-pointer font-normal"
        >
          <span class="flex items-center gap-1.5 text-sm font-medium text-foreground">
            <Zap class="h-3.5 w-3.5 text-primary" />
            Auto-import on card insert
          </span>
          <span class="text-xs text-muted-foreground">
            Offer a one-click import when a camera card with a DCIM folder is plugged in.
          </span>
        </Label>
        <Switch
          id="auto-import-toggle"
          aria-label="Auto-import on card insert"
          checked={$autoImportState.enabled}
          onCheckedChange={(v) => autoImportState.setEnabled(v)}
        />
      </div>
      <DeviceRuleControl />
    </div>
  </CardContent>
</Card>
