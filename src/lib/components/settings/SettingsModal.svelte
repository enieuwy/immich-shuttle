<script lang="ts">
  import {
    Dialog,
    DialogContent,
    DialogHeader,
    DialogTitle,
    DialogDescription,
  } from "$lib/components/ui/dialog";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState } from "$lib/state/import-options";

  let { open = $bindable(false) }: { open?: boolean } = $props();

  // Parallel-uploads input mirrors the number/blank coercion used elsewhere.
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
  $effect(() => {
    const stored = $importOptionsState.concurrentTasks;
    const shown = tasksValid ? tasksParsed : null;
    if (stored !== shown) {
      tasksInput = stored == null ? "" : String(stored);
    }
  });

  const excludeText = $derived($importOptionsState.excludeExtensions.join(", "));
  function parseExtensions(raw: string): string[] {
    return raw
      .split(",")
      .map((e) => e.trim().replace(/^\.+/, "").toLowerCase())
      .filter((e) => e.length > 0)
      .map((e) => `.${e}`);
  }

  // Mirror immich-go's --session-tag format: "{immich-go}/YYYY-MM-DD HH-MM-SS".
  const exampleSessionTag = $derived.by(() => {
    void $importOptionsState.sessionTag; // re-evaluate when toggled on
    const d = new Date();
    const p = (n: number) => String(n).padStart(2, "0");
    const stamp = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
    return `{immich-go}/${stamp}`;
  });
</script>

<Dialog bind:open>
  <DialogContent class="max-w-lg">
    <DialogHeader>
      <DialogTitle>Import defaults</DialogTitle>
      <DialogDescription>
        These apply to every import and persist across sessions. Per-import choices
        (dates, selection, album, tags) live on the main screen.
      </DialogDescription>
    </DialogHeader>

    <div class="flex flex-col gap-5">
      <section class="flex flex-col gap-1">
        <p class="px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">Filtering</p>
        <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
          <Label
            for="settings-exclude-ext"
            class="flex min-w-0 flex-col items-start gap-1 font-normal"
          >
            <span class="text-sm font-medium text-foreground">Always exclude extensions</span>
            <span class="text-xs text-muted-foreground">Comma-separated (e.g. aae, thm, dng). Skipped on every import.</span>
          </Label>
          <Input
            id="settings-exclude-ext"
            class="mt-2"
            placeholder="aae, thm"
            aria-label="Always exclude extensions"
            value={excludeText}
            onchange={(e) => importOptionsState.setExcludeExtensions(parseExtensions(e.currentTarget.value))}
          />
        </div>
      </section>

      <section class="flex flex-col gap-1">
        <p class="px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">Grouping</p>
        <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
          <Label
            for="settings-stack-raw-jpeg"
            class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
          >
            <span class="text-sm font-medium text-foreground">Stack RAW+JPEG pairs</span>
            <span class="text-xs text-muted-foreground">Group matching RAW and JPEG shots into one stack.</span>
          </Label>
          <Switch
            id="settings-stack-raw-jpeg"
            aria-label="Stack RAW+JPEG pairs"
            checked={$importOptionsState.stackRawJpeg}
            onCheckedChange={(v) => importOptionsState.setStackRawJpeg(v)}
          />
        </div>
        <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
          <Label
            for="settings-stack-burst"
            class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
          >
            <span class="text-sm font-medium text-foreground">Stack burst photos</span>
            <span class="text-xs text-muted-foreground">Combine rapid burst sequences into a single stack.</span>
          </Label>
          <Switch
            id="settings-stack-burst"
            aria-label="Stack burst photos"
            checked={$importOptionsState.stackBurst}
            onCheckedChange={(v) => importOptionsState.setStackBurst(v)}
          />
        </div>
      </section>

      <section class="flex flex-col gap-1">
        <p class="px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">Reliability</p>
        <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
          <div class="flex items-center justify-between gap-3">
            <Label
              for="settings-parallel-uploads"
              class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
            >
              <span class="text-sm font-medium text-foreground">Parallel uploads</span>
              <span class="text-xs text-muted-foreground">How many files to upload at once (1–20). Leave blank for the default.</span>
            </Label>
            <Input
              id="settings-parallel-uploads"
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
            for="settings-keep-going"
            class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
          >
            <span class="text-sm font-medium text-foreground">Keep going on errors</span>
            <span class="text-xs text-muted-foreground">Finish the import even if some files fail, then list the failures.</span>
          </Label>
          <Switch
            id="settings-keep-going"
            aria-label="Keep going on errors"
            checked={$importOptionsState.keepGoingOnErrors}
            onCheckedChange={(v) => importOptionsState.setKeepGoingOnErrors(v)}
          />
        </div>
      </section>

      <section class="flex flex-col gap-1">
        <p class="px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">Labeling</p>
        <div class="flex items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50">
          <Label
            for="settings-session-tag"
            class="flex min-w-0 flex-col items-start gap-1 cursor-pointer font-normal"
          >
            <span class="text-sm font-medium text-foreground">Tag each import session</span>
            <span class="text-xs text-muted-foreground">Creates an auto-generated, timestamped tag <em>in Immich</em> and applies it to every asset in the batch, so you can find the whole import later.</span>
          </Label>
          {#if $importOptionsState.sessionTag}
            <code class="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">{exampleSessionTag}</code>
          {/if}
          <Switch
            id="settings-session-tag"
            aria-label="Tag each import session"
            checked={$importOptionsState.sessionTag}
            onCheckedChange={(v) => importOptionsState.setSessionTag(v)}
          />
        </div>
      </section>
    </div>
  </DialogContent>
</Dialog>
