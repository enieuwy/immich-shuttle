<script lang="ts">
  import { Play } from "@lucide/svelte";

  import PreviewGrid from "./PreviewGrid.svelte";
  import { previewState } from "$lib/state/preview";
  import { selectionState } from "$lib/state/selection";
  import { sourceState } from "$lib/state/source";
  import { queueState } from "$lib/state/queue";
  import { errorsState } from "$lib/state/errors";
  import { importOptionsState } from "$lib/state/import-options";
  import { Button } from "$lib/components/ui/button";
  import {
    Dialog,
    DialogContent,
    DialogHeader,
    DialogTitle,
    DialogDescription,
    DialogFooter,
  } from "$lib/components/ui/dialog";

  let open = $state(false);
  let starting = $state(false);
  let importError = $state("");

  // Mirror the store into the Dialog's bindable open state.
  $effect(() => {
    open = $previewState.open;
  });

  const files = $derived($sourceState.scanResult?.files ?? []);
  const selectedCount = $derived($selectionState.selected.size);

  function handleOpenChange(next: boolean) {
    if (!next) {
      previewState.close();
    }
  }

  async function importSelected() {
    if (selectedCount === 0) {
      return;
    }
    starting = true;
    importError = "";
    try {
      await queueState.startImport({ selectFiles: selectionState.paths() });
      selectionState.clear();
      previewState.close();
    } catch (error) {
      importError = error instanceof Error ? error.message : String(error);
      errorsState.addError("Could not start import for the selection.");
    } finally {
      starting = false;
    }
  }
</script>

<Dialog bind:open onOpenChange={handleOpenChange}>
  <DialogContent
    class="grid h-[85vh] max-h-[85vh] grid-rows-[auto_minmax(0,1fr)_auto] gap-0 p-0 sm:max-w-4xl"
  >
    <DialogHeader class="border-b border-border px-4 py-3">
      <DialogTitle>Preview &amp; select</DialogTitle>
      <DialogDescription>
        {#if $importOptionsState.keepFiles}
          Choose which photos and videos to import. Source files are kept.
        {:else}
          Choose which photos and videos to import. Uploaded files are deleted from
          the source after you confirm.
        {/if}
      </DialogDescription>
    </DialogHeader>

    <div class="min-h-0 overflow-hidden">
      <PreviewGrid {files} />
    </div>

    <DialogFooter class="flex-row items-center justify-between border-t border-border px-4 py-3">
      <span class="text-xs text-muted-foreground">
        {#if importError}
          <span class="text-destructive">{importError}</span>
        {:else}
          {selectedCount} selected
        {/if}
      </span>
      <div class="flex items-center gap-2">
        <Button variant="outline" size="sm" onclick={() => previewState.close()}>Cancel</Button>
        <Button size="sm" disabled={selectedCount === 0 || starting} onclick={importSelected}>
          <Play class="size-4" />
          {starting ? "Starting…" : `Import ${selectedCount} selected`}
        </Button>
      </div>
    </DialogFooter>
  </DialogContent>
</Dialog>
