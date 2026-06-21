<script lang="ts">
  import { AlertTriangle, SlidersHorizontal } from "@lucide/svelte";
  import { Alert, AlertDescription, AlertTitle } from "$lib/components/ui/alert";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { Label } from "$lib/components/ui/label";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState } from "$lib/state/import-options";
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
  </CardContent>
</Card>
