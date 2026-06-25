<script lang="ts">
  import { SlidersHorizontal } from "@lucide/svelte";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Separator } from "$lib/components/ui/separator";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState } from "$lib/state/import-options";

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
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <SlidersHorizontal class="h-4 w-4 text-primary" />
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
  </CardContent>
</Card>
