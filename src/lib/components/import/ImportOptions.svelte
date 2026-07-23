<script lang="ts">
  import { SlidersHorizontal, TriangleAlert } from "@lucide/svelte";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState } from "$lib/state/import-options";

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
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <span class="flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
      <SlidersHorizontal class="h-4 w-4" />
    </span>
    <h3 class="text-sm font-semibold text-foreground">Import behavior</h3>
  </CardHeader>

  <CardContent class="flex flex-col gap-4">
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
