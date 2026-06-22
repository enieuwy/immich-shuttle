<script lang="ts">
  import { AlertTriangle, CalendarRange, SlidersHorizontal, X } from "@lucide/svelte";
  import { Alert, AlertDescription, AlertTitle } from "$lib/components/ui/alert";
  import { Button } from "$lib/components/ui/button";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Separator } from "$lib/components/ui/separator";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState } from "$lib/state/import-options";

  let fromDate = $state("");
  let toDate = $state("");

  const invalidRange = $derived(fromDate !== "" && toDate !== "" && fromDate > toDate);
  const rangeActive = $derived(fromDate !== "" && toDate !== "" && fromDate <= toDate);

  $effect(() => {
    if (fromDate !== "" && toDate !== "" && fromDate <= toDate) {
      importOptionsState.setDateRange(`${fromDate},${toDate}`);
    } else {
      importOptionsState.setDateRange(null);
    }
  });

  function clearDateRange() {
    fromDate = "";
    toDate = "";
  }
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <SlidersHorizontal class="h-4 w-4 text-muted-foreground" />
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
        <span class="text-xs text-muted-foreground">Removes source files only after a confirmed upload.</span>
      </Label>
      <Switch
        id="import-option-delete-uploaded"
        aria-label="Delete uploaded files after import"
        checked={!$importOptionsState.keepFiles}
        onCheckedChange={(v) => importOptionsState.setKeepFiles(!v)}
      />
    </div>

    {#if !$importOptionsState.keepFiles}
      <Alert variant="destructive" class="mt-2">
        <AlertTriangle />
        <AlertTitle>This cannot be undone</AlertTitle>
        <AlertDescription>Files are deleted from the source once Immich confirms each upload.</AlertDescription>
      </Alert>
    {/if}

    <Separator class="my-2" />

    <div class="flex flex-col gap-3 p-3">
      <div class="flex items-start gap-3">
        <CalendarRange class="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
        <div class="flex min-w-0 flex-col gap-1">
          <span class="text-sm font-medium text-foreground">Date range</span>
          <span class="text-xs text-muted-foreground">Only import media captured between two dates.</span>
        </div>
      </div>

      <div class="grid grid-cols-2 gap-2">
        <div class="flex flex-col gap-1.5">
          <Label for="import-option-date-from" class="text-xs font-normal text-muted-foreground">From</Label>
          <Input id="import-option-date-from" type="date" bind:value={fromDate} aria-invalid={invalidRange} />
        </div>
        <div class="flex flex-col gap-1.5">
          <Label for="import-option-date-to" class="text-xs font-normal text-muted-foreground">To</Label>
          <Input id="import-option-date-to" type="date" bind:value={toDate} aria-invalid={invalidRange} />
        </div>
      </div>

      {#if invalidRange}
        <p class="text-xs text-destructive">From date must be before To date.</p>
      {:else if rangeActive}
        <div class="flex items-center justify-between gap-2">
          <span class="min-w-0 truncate text-xs text-muted-foreground">
            Importing media from {fromDate} to {toDate}
          </span>
          <Button
            variant="ghost"
            size="sm"
            aria-label="Clear date range"
            onclick={clearDateRange}
          >
            <X class="h-3.5 w-3.5" />
            Clear
          </Button>
        </div>
      {/if}
    </div>
  </CardContent>
</Card>
