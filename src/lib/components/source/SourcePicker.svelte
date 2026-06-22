<script lang="ts">
  import { onMount } from "svelte";

  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open } from "@tauri-apps/plugin-dialog";
  import { FolderOpen, FileImage, HardDrive, History, Loader2, X } from "@lucide/svelte";

  import { sourceState } from "$lib/state/source";
  import { historySourceLastImport } from "$lib/api";
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

  let manualPath = $state("");

  let lastImportedAt = $state<number | null>(null);

  $effect(() => {
    const paths = $sourceState.selectedPaths;
    if (paths.length === 0) {
      lastImportedAt = null;
      return;
    }
    let cancelled = false;
    void historySourceLastImport(paths).then((ms) => {
      if (!cancelled) {
        lastImportedAt = ms;
      }
    });
    return () => {
      cancelled = true;
    };
  });

  onMount(() => {
    let unlistenDevice: (() => void) | undefined;
    let unlistenDrop: (() => void) | undefined;

    void sourceState.loadDevices();
    void listen<RemovableDevice[]>("device-changed", (event) => {
      if (Array.isArray(event.payload)) {
        void sourceState.loadDevices();
      }
    }).then((fn) => {
      unlistenDevice = fn;
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
        unlistenDrop = fn;
      });

    return () => {
      if (unlistenDevice) {
        unlistenDevice();
      }
      if (unlistenDrop) {
        unlistenDrop();
      }
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
      <HardDrive class="h-4 w-4 text-muted-foreground" />
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
            <p class="flex items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 class="h-3.5 w-3.5 animate-spin" />
              Scanning media...
            </p>
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
    {:else}
      <div
        class="flex flex-col items-center gap-3 rounded-xl border-2 border-dashed border-border px-4 py-8 text-center transition-colors hover:border-primary/50 hover:bg-muted/30"
      >
        <div class="flex h-12 w-12 items-center justify-center rounded-full bg-muted">
          <FileImage class="h-6 w-6 text-muted-foreground" />
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

      <div class="flex items-center gap-2">
        <Input bind:value={manualPath} placeholder="/path/to/photos" class="flex-1" />
        <Button variant="outline" size="sm" onclick={chooseManualPath}>Use path</Button>
      </div>

      <div class="flex flex-col gap-2">
        {#if $sourceState.loadingDevices}
          <p class="text-sm text-muted-foreground">Scanning devices...</p>
        {:else if $sourceState.detectedDevices.length > 0}
          <h4 class="text-xs font-medium text-muted-foreground">Removable devices</h4>
          <div class="flex flex-col gap-1.5">
            {#each $sourceState.detectedDevices as device}
              <button
                type="button"
                class="flex w-full items-center gap-3 rounded-lg border border-border p-3 text-left transition-colors hover:border-primary/40 hover:bg-accent"
                onclick={() => sourceState.selectSources([device.mount_path])}
              >
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
              </button>
            {/each}
          </div>
        {/if}
      </div>
    {/if}

    {#if $sourceState.error}
      <p class="text-sm text-destructive">{$sourceState.error}</p>
    {/if}
  </CardContent>
</Card>
