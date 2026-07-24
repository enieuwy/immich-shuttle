<script lang="ts">
  import { TriangleAlert } from "@lucide/svelte";
  import { Card, CardContent, CardHeader } from "$lib/components/ui/card";
  import { Label } from "$lib/components/ui/label";
  import { Switch } from "$lib/components/ui/switch";
  import { importOptionsState } from "$lib/state/import-options";
</script>

<Card class="border-destructive/30">
  <CardHeader class="flex flex-row items-center gap-2">
    <span class="flex size-7 items-center justify-center rounded-lg bg-destructive/10 text-destructive">
      <TriangleAlert class="h-4 w-4" />
    </span>
    <h3 class="text-sm font-semibold text-foreground">Danger zone</h3>
  </CardHeader>

  <CardContent class="flex flex-col gap-1">
    <!-- Irreversible, per-import decisions — deliberately not saved as defaults.
         Both still route through explicit confirms before anything is deleted. -->
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
  </CardContent>
</Card>
