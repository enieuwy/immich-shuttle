<script lang="ts">
  import { tick } from "svelte";
  import { Copy, FolderOpen, RotateCcw } from "@lucide/svelte";

  import { getRecentLogs, openLogsDir } from "$lib/api";
  import { Button } from "$lib/components/ui/button";
  import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
  } from "$lib/components/ui/dialog";
  import { ScrollArea } from "$lib/components/ui/scroll-area";
  import { errorsState } from "$lib/state/errors";

  let { open = $bindable(false) }: { open?: boolean } = $props();

  let logs = $state("");
  let loading = $state(false);
  let viewport = $state<HTMLElement | null>(null);

  async function load() {
    loading = true;
    try {
      logs = await getRecentLogs();
      await tick();
      if (viewport) {
        viewport.scrollTop = viewport.scrollHeight;
      }
    } catch (e) {
      errorsState.addError("Could not read logs.");
    } finally {
      loading = false;
    }
  }

  async function copyLogs() {
    try {
      await navigator.clipboard.writeText(logs);
    } catch (e) {
      errorsState.addError("Could not copy logs to clipboard.");
    }
  }

  async function openFolder() {
    try {
      await openLogsDir();
    } catch (e) {
      errorsState.addError("Could not open logs folder.");
    }
  }

  $effect(() => {
    if (open) {
      void load();
    }
  });
</script>

<Dialog bind:open>
  <DialogContent class="max-w-2xl">
    <DialogHeader>
      <DialogTitle>Application logs</DialogTitle>
      <DialogDescription>Recent activity from the app log file.</DialogDescription>
    </DialogHeader>

    <div class="overflow-hidden rounded-md border border-border bg-muted/30">
      <ScrollArea class="h-[420px]" bind:viewportRef={viewport}>
        {#if loading}
          <div class="flex h-[420px] items-center justify-center px-4">
            <p class="text-sm text-muted-foreground">Loading logs…</p>
          </div>
        {:else if !logs}
          <div class="flex h-[420px] items-center justify-center px-4">
            <p class="text-sm text-muted-foreground">No logs yet.</p>
          </div>
        {:else}
          <pre class="whitespace-pre-wrap break-words p-4 font-mono text-xs text-muted-foreground">{logs}</pre>
        {/if}
      </ScrollArea>
    </div>

    <DialogFooter class="sm:justify-start">
      <Button
        variant="ghost"
        size="sm"
        onclick={load}
        disabled={loading}
        aria-label="Refresh logs"
      >
        <RotateCcw class="size-4" /> Refresh
      </Button>
      <Button
        variant="ghost"
        size="sm"
        onclick={copyLogs}
        disabled={!logs}
        aria-label="Copy logs to clipboard"
      >
        <Copy class="size-4" /> Copy
      </Button>
      <Button variant="ghost" size="sm" onclick={openFolder} aria-label="Open logs folder">
        <FolderOpen class="size-4" /> Open folder
      </Button>
    </DialogFooter>
  </DialogContent>
</Dialog>
