<script lang="ts">
  import { SlidersHorizontal, Zap } from "@lucide/svelte";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Separator } from "$lib/components/ui/separator";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState, isDateRangeInvalid } from "$lib/state/import-options";
  import { autoImportState } from "$lib/state/auto-import";
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
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <span class="flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
      <SlidersHorizontal class="h-4 w-4" />
    </span>
    <h3 class="text-sm font-semibold text-foreground">Import options</h3>
  </CardHeader>

  <CardContent class="flex flex-col gap-1">
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
