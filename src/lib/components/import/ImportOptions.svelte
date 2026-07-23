<script lang="ts">
  import { SlidersHorizontal, Zap, ServerCog } from "@lucide/svelte";
  import { Button } from "$lib/components/ui/button";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Separator } from "$lib/components/ui/separator";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState, isDateRangeInvalid } from "$lib/state/import-options";
  import { autoImportState } from "$lib/state/auto-import";
  import { sourceState } from "$lib/state/source";
  import { activeProfile } from "$lib/state/profiles";
  import { importForecast, type ImportForecast } from "$lib/api";
  import DeviceRuleControl from "$lib/components/source/DeviceRuleControl.svelte";
  import type { ImportOrganization } from "$lib/types";

  let tasksInput = $state("");

  // `bind:value` on a type="number" input yields a number (or undefined when
  // empty), so coerce to a string before any string ops.
  const tasksRaw = $derived(tasksInput == null ? "" : String(tasksInput));
  const tasksParsed = $derived(Number.parseInt(tasksRaw, 10));
  const tasksValid = $derived(
    Number.isInteger(tasksParsed) && tasksParsed >= 1 && tasksParsed <= 20,
  );
  const tasksOutOfRange = $derived(tasksRaw.trim() !== "" && !tasksValid);

  $effect(() => {
    importOptionsState.setConcurrentTasks(tasksValid ? tasksParsed : null);
  });

  const dateFrom = $derived($importOptionsState.dateFrom ?? "");
  const dateTo = $derived($importOptionsState.dateTo ?? "");
  const dateRangeInvalid = $derived(isDateRangeInvalid(dateFrom, dateTo));
  const tagsText = $derived($importOptionsState.tags.join(", "));
  function commitTags(raw: string) {
    importOptionsState.setTags(
      raw
        .split(",")
        .map((t) => t.trim())
        .filter((t) => t.length > 0),
    );
  }
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

  let forecast = $state<ImportForecast | null>(null);
  let forecasting = $state(false);
  let forecastError = $state("");
  const canForecast = $derived(
    !!$activeProfile && $sourceState.selectedPaths.length > 0 && !forecasting,
  );

  async function checkServer() {
    const profile = $activeProfile;
    if (!profile || $sourceState.selectedPaths.length === 0) return;
    forecasting = true;
    forecastError = "";
    forecast = null;
    try {
      forecast = await importForecast(profile.id, $sourceState.selectedPaths);
    } catch (error) {
      forecastError = error instanceof Error ? error.message : String(error);
    } finally {
      forecasting = false;
    }
  }
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <span class="flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
      <SlidersHorizontal class="h-4 w-4" />
    </span>
    <h3 class="text-sm font-semibold text-foreground">Import options</h3>
  </CardHeader>

  <CardContent class="flex flex-col gap-1">
    <div class="rounded-lg border border-border/60 bg-muted/30 p-3">
      <div class="flex items-center justify-between gap-3">
        <div class="flex min-w-0 flex-col items-start gap-0.5">
          <span class="text-sm font-medium text-foreground">Check server</span>
          <span class="text-xs text-muted-foreground">Preview how much would upload vs. is already on the server.</span>
        </div>
        <Button variant="outline" size="sm" disabled={!canForecast} onclick={checkServer}>
          <ServerCog class="mr-1 h-4 w-4" />
          {forecasting ? "Checking…" : "Check"}
        </Button>
      </div>
      {#if forecastError}
        <p class="mt-2 text-xs text-destructive">{forecastError}</p>
      {:else if forecast}
        <div class="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs">
          <span class="text-foreground"><span class="font-semibold text-primary">{forecast.new}</span> to upload</span>
          <span class="text-muted-foreground"><span class="font-semibold text-foreground">{forecast.already_present}</span> already on server</span>
          {#if forecast.unreadable > 0}
            <span class="text-muted-foreground"><span class="font-semibold text-foreground">{forecast.unreadable}</span> unreadable</span>
          {/if}
          {#if forecast.truncated}
            <span class="text-muted-foreground">(sampled first {5000} files)</span>
          {/if}
        </div>
      {/if}
    </div>

    <Separator class="my-2" />
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

    <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
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

    <Separator class="my-2" />

    <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
      <div class="flex items-start justify-between gap-3">
        <Label
          for="import-option-organization"
          class="flex min-w-0 flex-col items-start gap-1 font-normal"
        >
          <span class="text-sm font-medium text-foreground">Organize into albums</span>
          <span class="text-xs text-muted-foreground">
            Group uploads by the source folder structure instead of one album.
          </span>
        </Label>
        <select
          id="import-option-organization"
          class="h-9 w-52 shrink-0 rounded-md border border-input bg-transparent px-2 text-sm shadow-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          aria-label="Organize into albums"
          value={$importOptionsState.organization}
          onchange={(e) =>
            importOptionsState.setOrganization(e.currentTarget.value as ImportOrganization)}
        >
          <option value="single_album">Single album (selected)</option>
          <option value="folder_name">Album per folder name</option>
          <option value="folder_path">Album per folder path</option>
          <option value="folder_tags">Tag by folder path</option>
        </select>
      </div>

      {#if $importOptionsState.organization !== "single_album"}
        <p class="mt-2 text-xs text-muted-foreground">
          Albums or tags are derived from the source folders; the album picker is ignored for this mode.
        </p>
      {/if}
    </div>

    <Separator class="my-2" />

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

    <Separator class="my-2" />

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

    <Separator class="my-2" />

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

    <Separator class="my-2" />

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

    <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
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

    <Separator class="my-2" />

    <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
      <Label
        for="import-option-tags"
        class="flex min-w-0 flex-col items-start gap-1 font-normal"
      >
        <span class="text-sm font-medium text-foreground">Tags</span>
        <span class="text-xs text-muted-foreground">Comma-separated tags applied to every uploaded asset. Use / for hierarchy (e.g. Trip/Iceland).</span>
      </Label>
      <Input
        id="import-option-tags"
        class="mt-2"
        placeholder="Trip/Iceland, client-a"
        aria-label="Tags"
        value={tagsText}
        onchange={(e) => commitTags(e.currentTarget.value)}
      />
      <div class="mt-2 flex items-center justify-between gap-3">
        <Label
          for="import-option-session-tag"
          class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
        >
          <span class="text-sm font-medium text-foreground">Tag this import session</span>
          <span class="text-xs text-muted-foreground">Add a timestamped tag so this batch is easy to find later.</span>
        </Label>
        <Switch
          id="import-option-session-tag"
          aria-label="Tag this import session"
          checked={$importOptionsState.sessionTag}
          onCheckedChange={(v) => importOptionsState.setSessionTag(v)}
        />
      </div>
    </div>

    <Separator class="my-2" />

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

    <Separator class="my-2" />

    <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
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
  </CardContent>
</Card>
