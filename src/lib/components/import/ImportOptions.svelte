<script lang="ts">
  import { AlertTriangle, SlidersHorizontal } from "@lucide/svelte";
  import { Alert, AlertDescription, AlertTitle } from "$lib/components/ui/alert";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { importOptionsState } from "$lib/state/import-options";
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <SlidersHorizontal class="h-4 w-4 text-muted-foreground" />
    <h3 class="text-sm font-semibold text-foreground">Import options</h3>
  </CardHeader>

  <CardContent class="flex flex-col gap-1">
    <label
      class="flex cursor-pointer items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50"
    >
      <span class="flex min-w-0 flex-col gap-1">
        <span class="text-sm font-medium text-foreground">Stack RAW+JPEG pairs</span>
        <span class="text-xs text-muted-foreground">Group matching RAW and JPEG shots into one stack.</span>
      </span>
      <input
        type="checkbox"
        class="h-4 w-4 rounded border-border accent-primary"
        checked={$importOptionsState.stackRawJpeg}
        onchange={(event) => importOptionsState.setStackRawJpeg((event.target as HTMLInputElement).checked)}
      />
    </label>

    <label
      class="flex cursor-pointer items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50"
    >
      <span class="flex min-w-0 flex-col gap-1">
        <span class="text-sm font-medium text-foreground">Stack burst photos</span>
        <span class="text-xs text-muted-foreground">Combine rapid burst sequences into a single stack.</span>
      </span>
      <input
        type="checkbox"
        class="h-4 w-4 rounded border-border accent-primary"
        checked={$importOptionsState.stackBurst}
        onchange={(event) => importOptionsState.setStackBurst((event.target as HTMLInputElement).checked)}
      />
    </label>

    <label
      class="flex cursor-pointer items-center justify-between gap-3 rounded-lg p-3 transition-colors hover:bg-muted/50"
    >
      <span class="flex min-w-0 flex-col gap-1">
        <span class="text-sm font-medium text-foreground">Delete uploaded files after import</span>
        <span class="text-xs text-muted-foreground">Removes source files only after a confirmed upload.</span>
      </span>
      <input
        type="checkbox"
        class="h-4 w-4 rounded border-border accent-destructive"
        checked={!$importOptionsState.keepFiles}
        onchange={(event) => importOptionsState.setKeepFiles(!(event.target as HTMLInputElement).checked)}
      />
    </label>

    {#if !$importOptionsState.keepFiles}
      <Alert variant="destructive" class="mt-2">
        <AlertTriangle />
        <AlertTitle>This cannot be undone</AlertTitle>
        <AlertDescription>Files are deleted from the source once Immich confirms each upload.</AlertDescription>
      </Alert>
    {/if}
  </CardContent>
</Card>
