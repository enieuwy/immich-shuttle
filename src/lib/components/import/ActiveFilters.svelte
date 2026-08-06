<script lang="ts">
  import { ListFilter, X } from "@lucide/svelte";

  import { Button } from "$lib/components/ui/button";
  import { importOptionsState } from "$lib/state/import-options";
  import { selectionState } from "$lib/state/selection";

  const summaries = $derived.by(() => {
    const items: string[] = [];
    if ($importOptionsState.mediaType === "image") items.push("Photos only");
    if ($importOptionsState.mediaType === "video") items.push("Videos only");

    const from = $importOptionsState.dateFrom;
    const to = $importOptionsState.dateTo;
    if (from && to) items.push(`${from} → ${to}`);
    else if (from) items.push(`From ${from}`);
    else if (to) items.push(`Through ${to}`);

    if ($importOptionsState.includeExtensions.length > 0) {
      items.push(`Include ${$importOptionsState.includeExtensions.join(", ")}`);
    }
    return items;
  });
  const hasSelection = $derived($selectionState.selected.size > 0);

  // A hand-picked Preview selection is the import contract: startImport drops
  // these filters rather than letting a restored filter discard a ticked file.
  // This summary therefore describes only the no-selection path.
</script>

{#if summaries.length > 0}
  <div
    class="flex min-w-0 items-center gap-1 rounded-md border border-border bg-muted/40 px-2 py-1 text-xs"
    class:text-muted-foreground={hasSelection}
    title={hasSelection ? "Ignored while a Preview selection is active." : "Active import filters"}
  >
    <ListFilter class="size-3.5 shrink-0" />
    <span class="truncate">{summaries.join(" · ")}</span>
    {#if hasSelection}
      <span class="shrink-0">· ignored by selection</span>
    {/if}
    <Button
      variant="ghost"
      size="sm"
      class="h-5 shrink-0 gap-1 px-1.5 text-[11px]"
      aria-label="Clear active import filters"
      onclick={() => importOptionsState.clearImportFilters()}
    >
      <X class="size-3" /> Clear
    </Button>
  </div>
{/if}
