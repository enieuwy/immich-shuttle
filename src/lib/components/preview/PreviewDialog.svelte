<script lang="ts">
  import { Check } from "@lucide/svelte";

  import PreviewGrid from "./PreviewGrid.svelte";
  import { previewState } from "$lib/state/preview";
  import { selectionState } from "$lib/state/selection";
  import { sourceState } from "$lib/state/source";
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
</script>

<Dialog bind:open onOpenChange={handleOpenChange}>
  <DialogContent
    class="grid h-[85vh] max-h-[85vh] grid-rows-[auto_minmax(0,1fr)_auto] gap-0 p-0 sm:max-w-4xl"
  >
    <DialogHeader class="border-b border-border px-4 py-3">
      <DialogTitle>Preview &amp; select</DialogTitle>
      <DialogDescription>
        Pick the photos and videos to import. Set albums and options on the main
        screen, then Start Import.
      </DialogDescription>
    </DialogHeader>

    <div class="min-h-0 overflow-hidden">
      <PreviewGrid {files} />
    </div>

    <DialogFooter
      class="flex-row items-center justify-between border-t border-border px-4 py-3"
    >
      <span class="text-xs text-muted-foreground">{selectedCount} selected</span>
      <Button size="sm" onclick={() => previewState.close()}>
        <Check class="size-4" />
        {selectedCount > 0 ? "Use selection" : "Done"}
      </Button>
    </DialogFooter>
  </DialogContent>
</Dialog>
