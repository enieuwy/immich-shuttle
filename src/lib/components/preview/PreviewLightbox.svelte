<script lang="ts">
  import { ChevronLeft, ChevronRight, Loader2, Search, X } from "@lucide/svelte";
  import type { MediaFile } from "$lib/types";
  import { previewFullImage, previewVideo, previewVideoRelease, type PreviewMetadata } from "$lib/previewApi";
  import { selectionState } from "$lib/state/selection";
  import { Button } from "$lib/components/ui/button";
  import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "$lib/components/ui/dialog";

  let { files, path, token, metadata, metadataLoaded, metadataError, onNavigate, onClose }: {
    files: MediaFile[];
    path: string;
    token: number;
    metadata: PreviewMetadata | null | undefined;
    metadataLoaded: boolean;
    metadataError: string;
    onNavigate: (path: string) => void;
    onClose: () => void;
  } = $props();

  let open = $state(true);
  let loading = $state(false);
  let error = $state("");
  let imageUrl = $state("");
  let videoUrl = $state("");
  let renderedSize = $state("");
  let zoom = $state(false);
  const index = $derived(files.findIndex((file) => file.path === path));
  const file = $derived(files[index]);

  $effect(() => {
    const current = file;
    const currentToken = token;
    if (!current) { onClose(); return; }
    loading = true;
    error = "";
    imageUrl = "";
    videoUrl = "";
    renderedSize = "";
    zoom = false;
    let disposed = false;
    let ticket: string | undefined;
    if (current.is_video) {
      void previewVideo(current.path, currentToken).then((video) => {
        if (disposed) { void previewVideoRelease(video.ticket).catch(() => {}); return; }
        ticket = video.ticket;
        videoUrl = video.url;
      }).catch((reason: unknown) => {
        if (!disposed) error = reason instanceof Error ? reason.message : String(reason);
      }).finally(() => { if (!disposed) loading = false; });
    } else {
      void previewFullImage(current.path, currentToken).then((image) => {
        if (disposed) return;
        if (!image.data_url) { error = "This image has no inspection preview."; return; }
        imageUrl = image.data_url;
        renderedSize = `${image.width} × ${image.height}`;
      }).catch((reason: unknown) => {
        if (!disposed) error = reason instanceof Error ? reason.message : String(reason);
      }).finally(() => { if (!disposed) loading = false; });
    }
    return () => {
      disposed = true;
      if (ticket) void previewVideoRelease(ticket).catch(() => {});
    };
  });

  function navigate(offset: number) {
    const next = files[index + offset];
    if (next) onNavigate(next.path);
  }

  function onKey(event: KeyboardEvent) {
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    const target = event.target;
    if (target instanceof HTMLElement && ["INPUT", "TEXTAREA", "SELECT", "VIDEO"].includes(target.tagName)) return;
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      navigate(event.key === "ArrowLeft" ? -1 : 1);
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<Dialog bind:open onOpenChange={(next) => { if (!next) onClose(); }}>
  <DialogContent class="flex h-[90vh] max-h-[90vh] flex-col gap-3 sm:max-w-[95vw]" showCloseButton={false}>
    <DialogHeader class="pr-10">
      <DialogTitle>{file?.name ?? "Inspect media"}</DialogTitle>
      <DialogDescription>
        {index + 1} of {files.length} shown · Inspect without changing the selection.
      </DialogDescription>
    </DialogHeader>
    <Button class="absolute right-3 top-3" variant="ghost" size="icon" aria-label="Close inspection" onclick={onClose}>
      <X class="size-4" />
    </Button>
    {#if file}
      <div class="flex min-h-0 flex-1 flex-col gap-3 md:flex-row">
        <div class={`flex min-h-[12rem] min-w-0 flex-1 overflow-auto rounded-md bg-black/90 ${zoom ? "items-start justify-start" : "items-center justify-center"}`}>
          {#if loading}
            <Loader2 class="size-8 animate-spin text-white" aria-label="Loading inspection preview" />
          {:else if error}
            <p class="max-w-md p-6 text-center text-sm text-white" role="alert">{error}</p>
          {:else if file.is_video && videoUrl}
            {#key videoUrl}
              <video src={videoUrl} controls preload="metadata" class="max-h-full max-w-full"
                onerror={() => { error = "The system video decoder cannot play this file. Its selection remains unchanged."; }}>
                <track kind="captions" />
              </video>
            {/key}
          {:else if imageUrl}
            <img src={imageUrl} alt={file.name} draggable="false"
              class={zoom ? "max-w-none shrink-0" : "max-h-full max-w-full object-contain"} />
          {/if}
        </div>
        <aside class="max-h-[30vh] shrink-0 overflow-auto text-sm md:max-h-none md:w-60" aria-label="Media details">
          <h3 class="mb-2 font-semibold">Details</h3>
          <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-2 break-words">
            <dt class="text-muted-foreground">Type</dt><dd>{file.extension.replace(/^\./, "").toUpperCase()}</dd>
            <dt class="text-muted-foreground">Size</dt><dd>{(file.size_bytes / 1024 / 1024).toFixed(2)} MB</dd>
            {#if renderedSize}<dt class="text-muted-foreground">Preview</dt><dd>{renderedSize}</dd>{/if}
            {#if metadata?.camera}<dt class="text-muted-foreground">Camera</dt><dd>{metadata.camera}</dd>{/if}
            {#if metadata?.lens}<dt class="text-muted-foreground">Lens</dt><dd>{metadata.lens}</dd>{/if}
            {#if metadata?.width && metadata?.height}<dt class="text-muted-foreground">Resolution</dt><dd>{metadata.width} × {metadata.height}</dd>{/if}
            {#if metadata?.iso != null}<dt class="text-muted-foreground">ISO</dt><dd>{metadata.iso}</dd>{/if}
            {#if metadata?.exposure}<dt class="text-muted-foreground">Exposure</dt><dd>{metadata.exposure}</dd>{/if}
            {#if metadata?.gps_lat != null && metadata?.gps_lon != null}
              <dt class="text-muted-foreground">GPS</dt><dd>{metadata.gps_lat.toFixed(6)}, {metadata.gps_lon.toFixed(6)}</dd>
            {:else if metadataLoaded}
              <dt class="text-muted-foreground">GPS</dt><dd>No GPS coordinates</dd>
            {/if}
          </dl>
          {#if metadataError && !metadataLoaded}
            <p class="mt-3 text-xs text-destructive" role="alert">{metadataError}</p>
          {:else if !metadataLoaded}
            <p class="mt-3 text-xs text-muted-foreground">Reading EXIF metadata…</p>
          {:else if !metadata}
            <p class="mt-3 text-xs text-muted-foreground">No readable EXIF metadata.</p>
          {/if}
          {#if !file.is_video}<p class="mt-3 text-xs text-muted-foreground">Preview at up to 2560 px. The original file remains unchanged.</p>{/if}
        </aside>
      </div>
      <div class="flex flex-wrap items-center justify-between gap-2">
        <div class="flex items-center gap-2">
          <Button variant="outline" size="sm" disabled={index <= 0} onclick={() => navigate(-1)} aria-label="Previous shown file"><ChevronLeft class="size-4" />Previous</Button>
          <Button variant="outline" size="sm" disabled={index >= files.length - 1} onclick={() => navigate(1)} aria-label="Next shown file">Next<ChevronRight class="size-4" /></Button>
          {#if imageUrl}<Button variant="ghost" size="sm" onclick={() => (zoom = !zoom)}><Search class="size-4" />{zoom ? "Fit image" : "Zoom image"}</Button>{/if}
        </div>
        <Button variant={$selectionState.selected.has(path) ? "secondary" : "outline"} size="sm"
          aria-pressed={$selectionState.selected.has(path)} onclick={() => selectionState.toggle(path)}>
          {$selectionState.selected.has(path) ? "Selected · remove" : "Select this file"}
        </Button>
      </div>
    {/if}
  </DialogContent>
</Dialog>
