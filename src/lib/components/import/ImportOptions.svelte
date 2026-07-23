<script lang="ts">
  import { SlidersHorizontal, ChevronRight, TriangleAlert } from "@lucide/svelte";
  import { Button } from "$lib/components/ui/button";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState, isDateRangeInvalid } from "$lib/state/import-options";

  let filtersOpen = $state(false);

  // `bind:value` on a type="number" input yields a number (or undefined when
  // empty), so coerce to a string before any string ops.
  let tasksInput = $state("");
  const tasksRaw = $derived(tasksInput == null ? "" : String(tasksInput));
  const tasksParsed = $derived(Number.parseInt(tasksRaw, 10));
  const tasksValid = $derived(
    Number.isInteger(tasksParsed) && tasksParsed >= 1 && tasksParsed <= 20,
  );
  const tasksOutOfRange = $derived(tasksRaw.trim() !== "" && !tasksValid);

  $effect(() => {
    importOptionsState.setConcurrentTasks(tasksValid ? tasksParsed : null);
  });

  // Reflect external concurrency changes (e.g. History "Import again") into the
  // local input. Only fires when the stored value diverges from what the input
  // currently maps to, so it never fights user typing (out-of-range/blank input
  // already maps the store to null, which stays equal here).
  $effect(() => {
    const stored = $importOptionsState.concurrentTasks;
    const shown = tasksValid ? tasksParsed : null;
    if (stored !== shown) {
      tasksInput = stored == null ? "" : String(stored);
    }
  });

  const dateFrom = $derived($importOptionsState.dateFrom ?? "");
  const dateTo = $derived($importOptionsState.dateTo ?? "");
  const dateRangeInvalid = $derived(isDateRangeInvalid(dateFrom, dateTo));

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

  // A filter is active when it diverges from "import everything" — surfaced as a
  // count on the collapsed Filters header so a narrowed import is never hidden.
  const activeFilterCount = $derived(
    ($importOptionsState.mediaType !== "all" ? 1 : 0) +
      (dateFrom !== "" || dateTo !== "" ? 1 : 0) +
      ($importOptionsState.onlyNewSinceLastImport ? 1 : 0) +
      ($importOptionsState.includeExtensions.length > 0 ? 1 : 0) +
      ($importOptionsState.excludeExtensions.length > 0 ? 1 : 0),
  );
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <span class="flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
      <SlidersHorizontal class="h-4 w-4" />
    </span>
    <h3 class="text-sm font-semibold text-foreground">Import behavior</h3>
  </CardHeader>

  <CardContent class="flex flex-col gap-4">
    <!-- Filters — rarely touched, collapsed by default so the common controls
         below stay above the fold. -->
    <section class="flex flex-col gap-1">
      <button
        type="button"
        class="flex items-center gap-1.5 rounded-md px-1 py-1 text-left transition-colors hover:bg-muted/50"
        aria-expanded={filtersOpen}
        onclick={() => (filtersOpen = !filtersOpen)}
      >
        <ChevronRight class="size-3.5 text-muted-foreground transition-transform {filtersOpen ? 'rotate-90' : ''}" />
        <span class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Filters</span>
        <span class="text-xs font-normal normal-case text-muted-foreground">— what's included</span>
        {#if activeFilterCount > 0}
          <span class="ml-auto rounded-full bg-primary/15 px-1.5 text-[10px] font-semibold text-primary">
            {activeFilterCount} active
          </span>
        {/if}
      </button>

      {#if filtersOpen}
        <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
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

        <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
          <div class="flex items-start justify-between gap-3">
            <div class="flex min-w-0 flex-col items-start gap-1">
              <span class="text-sm font-medium text-foreground">Capture date range</span>
              <span class="text-xs text-muted-foreground">Only import files captured between these dates. Leave blank to import all.</span>
            </div>
            {#if dateFrom !== "" || dateTo !== ""}
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
            <Label for="import-option-date-from" class="sr-only">From date</Label>
            <Input
              id="import-option-date-from"
              class="w-40 shrink-0"
              type="date"
              aria-label="From date"
              aria-invalid={dateRangeInvalid}
              max={dateTo || undefined}
              value={dateFrom}
              onchange={(e) => importOptionsState.setDateFrom(e.currentTarget.value)}
            />
            <span class="text-xs text-muted-foreground">to</span>
            <Label for="import-option-date-to" class="sr-only">To date</Label>
            <Input
              id="import-option-date-to"
              class="w-40 shrink-0"
              type="date"
              aria-label="To date"
              aria-invalid={dateRangeInvalid}
              min={dateFrom || undefined}
              value={dateTo}
              onchange={(e) => importOptionsState.setDateTo(e.currentTarget.value)}
            />
          </div>
          {#if dateRangeInvalid}
            <p class="mt-2 text-xs text-destructive">The start date must be on or before the end date.</p>
          {/if}
        </div>

        <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
          <div class="flex items-center justify-between gap-3">
            <Label
              for="import-option-only-new"
              class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
            >
              <span class="text-sm font-medium text-foreground">Only import media newer than last import</span>
              <span class="text-xs text-muted-foreground">Skips a re-scan of already-imported files by filtering to a capture-date floor.</span>
            </Label>
            <Switch
              id="import-option-only-new"
              aria-label="Only import media newer than last import"
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

        <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
          <Label
            for="import-option-include-ext"
            class="flex min-w-0 flex-col items-start gap-1 font-normal"
          >
            <span class="text-sm font-medium text-foreground">Only these extensions</span>
            <span class="text-xs text-muted-foreground">Comma-separated (e.g. jpg, heic). Leave empty for all.</span>
          </Label>
          <Input
            id="import-option-include-ext"
            class="mt-2"
            placeholder="jpg, heic, mp4"
            aria-label="Only these extensions"
            value={includeExtText}
            onchange={(e) => importOptionsState.setIncludeExtensions(parseExtensions(e.currentTarget.value))}
          />
        </div>

        <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
          <Label
            for="import-option-exclude-ext"
            class="flex min-w-0 flex-col items-start gap-1 font-normal"
          >
            <span class="text-sm font-medium text-foreground">Exclude extensions</span>
            <span class="text-xs text-muted-foreground">Comma-separated (e.g. gif, aae) to skip.</span>
          </Label>
          <Input
            id="import-option-exclude-ext"
            class="mt-2"
            placeholder="gif, aae"
            aria-label="Exclude extensions"
            value={excludeExtText}
            onchange={(e) => importOptionsState.setExcludeExtensions(parseExtensions(e.currentTarget.value))}
          />
        </div>
      {/if}
    </section>

    <!-- Grouping -->
    <section class="flex flex-col gap-1">
      <p class="px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">Grouping</p>
      <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
        <Label
          for="import-option-stack-raw-jpeg"
          class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
        >
          <span class="text-sm font-medium text-foreground">Stack RAW+JPEG pairs</span>
          <span class="text-xs text-muted-foreground">Group matching RAW and JPEG shots into one stack.</span>
        </Label>
        <Switch
          id="import-option-stack-raw-jpeg"
          aria-label="Stack RAW+JPEG pairs"
          checked={$importOptionsState.stackRawJpeg}
          onCheckedChange={(v) => importOptionsState.setStackRawJpeg(v)}
        />
      </div>

      <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
        <Label
          for="import-option-stack-burst"
          class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
        >
          <span class="text-sm font-medium text-foreground">Stack burst photos</span>
          <span class="text-xs text-muted-foreground">Combine rapid burst sequences into a single stack.</span>
        </Label>
        <Switch
          id="import-option-stack-burst"
          aria-label="Stack burst photos"
          checked={$importOptionsState.stackBurst}
          onCheckedChange={(v) => importOptionsState.setStackBurst(v)}
        />
      </div>
    </section>

    <!-- Performance & reliability -->
    <section class="flex flex-col gap-1">
      <p class="px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
        Performance &amp; reliability
      </p>
      <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
        <div class="flex items-center justify-between gap-3">
          <Label
            for="import-option-parallel-uploads"
            class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
          >
            <span class="text-sm font-medium text-foreground">Parallel uploads</span>
            <span class="text-xs text-muted-foreground">How many files to upload at once (1–20). Leave blank for the default.</span>
          </Label>
          <Input
            id="import-option-parallel-uploads"
            class="w-24 shrink-0"
            type="number"
            min="1"
            max="20"
            step="1"
            inputmode="numeric"
            placeholder="Auto"
            aria-label="Parallel uploads"
            aria-invalid={tasksOutOfRange}
            bind:value={tasksInput}
          />
        </div>
        {#if tasksOutOfRange}
          <p class="mt-2 text-xs text-destructive">Enter a value between 1 and 20.</p>
        {/if}
      </div>

      <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
        <Label
          for="import-option-keep-going"
          class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
        >
          <span class="text-sm font-medium text-foreground">Keep going on errors</span>
          <span class="text-xs text-muted-foreground">Finish the import even if some files fail, then list the failures. Off stops at the first error.</span>
        </Label>
        <Switch
          id="import-option-keep-going"
          aria-label="Keep going on errors"
          checked={$importOptionsState.keepGoingOnErrors}
          onCheckedChange={(v) => importOptionsState.setKeepGoingOnErrors(v)}
        />
      </div>
    </section>

    <!-- Danger zone — irreversible actions, fenced off and last so they can't be
         flipped in a fast scan. Both still route through explicit confirms. -->
    <section class="flex flex-col gap-1 rounded-lg border border-destructive/30 bg-destructive/5 p-1">
      <p class="flex items-center gap-1.5 px-2 pt-1 text-xs font-semibold uppercase tracking-wide text-destructive">
        <TriangleAlert class="size-3.5" /> Destructive
      </p>
      <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-destructive/10">
        <Label
          for="import-option-delete-uploaded"
          class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
        >
          <span class="text-sm font-medium text-foreground">Delete uploaded files after import</span>
          <span class="text-xs text-muted-foreground">Removes source files after upload — you'll review and confirm first.</span>
        </Label>
        <Switch
          id="import-option-delete-uploaded"
          aria-label="Delete uploaded files after import"
          checked={!$importOptionsState.keepFiles}
          onCheckedChange={(v) => importOptionsState.setKeepFiles(!v)}
        />
      </div>

      <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-destructive/10">
        <Label
          for="import-option-overwrite"
          class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
        >
          <span class="text-sm font-medium text-foreground">Replace existing on server</span>
          <span class="text-xs text-muted-foreground">Overwrite assets the server already has with the local copy instead of skipping them.</span>
        </Label>
        <Switch
          id="import-option-overwrite"
          aria-label="Replace existing on server"
          checked={$importOptionsState.overwrite}
          onCheckedChange={(v) => importOptionsState.setOverwrite(v)}
        />
      </div>
    </section>
  </CardContent>
</Card>
