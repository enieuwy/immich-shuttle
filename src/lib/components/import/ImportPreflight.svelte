<script lang="ts">
  import { ServerCog } from "@lucide/svelte";
  import { Button } from "$lib/components/ui/button";
  import ActiveFilters from "./ActiveFilters.svelte";
  import { importForecast, type ImportForecast } from "$lib/api";
  import { activeProfile } from "$lib/state/profiles";
  import { sourceState } from "$lib/state/source";
  import { selectionState } from "$lib/state/selection";
  import { importOptionsState } from "$lib/state/import-options";

  // Preflight dry-run of the import: sits beside Start Import so "what would
  // this upload?" is answered right where the import is launched.
  let forecast = $state<ImportForecast | null>(null);
  let forecasting = $state(false);
  let forecastError = $state("");
  let forecastToken = 0;

  const canForecast = $derived(
    !!$activeProfile && $sourceState.selectedPaths.length > 0 && !forecasting,
  );
  // An explicit preview selection *is* the import: queueState.startImport drops
  // the coarse type/extension/date filters so they can't discard a ticked file,
  // so the forecast must drop them too or Check server and Start Import
  // disagree. Without a selection the forecast still can't cheaply replicate
  // EXIF date filtering, so flag that case as approximate.
  const hasSelection = $derived($selectionState.selected.size > 0);
  const forecastCaveat = $derived(
    !hasSelection &&
      ($importOptionsState.onlyNewSinceLastImport ||
        !!$importOptionsState.dateFrom ||
        !!$importOptionsState.dateTo),
  );

  // Invalidate any shown/in-flight forecast when its inputs change, so stale
  // counts from a previous profile/source/selection never sit under a new one.
  $effect(() => {
    void $activeProfile?.id;
    void $sourceState.selectedPaths;
    void $selectionState.selected;
    void $importOptionsState.mediaType;
    void $importOptionsState.includeExtensions;
    void $importOptionsState.excludeExtensions;
    forecastToken++;
    forecast = null;
    forecastError = "";
    // A superseded request must release the button because its own finally no longer matches the token.
    forecasting = false;
  });

  async function checkServer() {
    const profile = $activeProfile;
    const sourcePaths = $sourceState.selectedPaths;
    if (!profile || sourcePaths.length === 0) return;
    // Match the import's scope: forecast the current selection when one exists
    // (App.startImport imports only the selection), else the whole source.
    const selection = [...$selectionState.selected];
    const options = $importOptionsState;
    const token = ++forecastToken;
    forecasting = true;
    forecastError = "";
    forecast = null;
    try {
      const result = await importForecast(
        profile.id,
        sourcePaths,
        selection.length > 0 ? selection : null,
        selection.length > 0
          ? { includeType: null, includeExtensions: [], excludeExtensions: [] }
          : {
              includeType:
                options.mediaType === "image"
                  ? "IMAGE"
                  : options.mediaType === "video"
                    ? "VIDEO"
                    : null,
              includeExtensions: options.includeExtensions,
              excludeExtensions: options.excludeExtensions,
            },
      );
      if (token === forecastToken) forecast = result;
    } catch (error) {
      if (token === forecastToken)
        forecastError = error instanceof Error ? error.message : String(error);
    } finally {
      if (token === forecastToken) forecasting = false;
    }
  }
</script>

<div class="flex min-w-0 items-center gap-2">
  <ActiveFilters />
  <Button variant="outline" size="sm" disabled={!canForecast} onclick={checkServer}>
    <ServerCog class="size-4" />
    {forecasting ? "Checking…" : "Check server"}
  </Button>
  {#if forecastError}
    <span class="truncate text-xs text-destructive" title={forecastError}>{forecastError}</span>
  {:else if forecast}
    <span class="flex min-w-0 items-center gap-2 text-xs">
      <span class="text-foreground">{#if forecast.truncated}at least&nbsp;{/if}<span class="font-semibold text-primary tabular-nums">{forecast.new}</span> to upload</span>
      <span class="text-muted-foreground"><span class="font-semibold text-foreground tabular-nums">{forecast.already_present}</span> already on server</span>
      {#if forecast.unreadable > 0}
        <span class="text-muted-foreground"><span class="font-semibold text-foreground tabular-nums">{forecast.unreadable}</span> unreadable</span>
      {/if}
      {#if forecast.truncated}
        <span class="text-muted-foreground" title="Too many files to check them all — only the first batch was compared against the server. The real total is higher.">·  partial scan</span>
      {/if}
      {#if forecastCaveat}
        <span class="text-muted-foreground" title="Estimate ignores the active date filter — the import may upload fewer.">·  approx</span>
      {/if}
    </span>
  {/if}
</div>
