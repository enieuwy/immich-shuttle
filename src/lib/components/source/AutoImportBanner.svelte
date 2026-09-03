<script lang="ts">
  import { HardDrive, Play, X } from "@lucide/svelte";

  import { autoImportState } from "$lib/state/auto-import";
  import { profilesState } from "$lib/state/profiles";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";
  import { Label } from "$lib/components/ui/label";
  import { Switch } from "$lib/components/ui/switch";

  let starting = $state(false);
  let confirmDelete = $state(false);

  const device = $derived($autoImportState.candidate);
  const rule = $derived($autoImportState.candidateRule);
  // A rule found only under the old label/mount key describes some earlier card, not
  // necessarily this one, so it is shown as a proposal rather than replayed silently.
  const unverified = $derived($autoImportState.candidateRuleNeedsConfirmation);
  const ruleProfileName = $derived(
    rule ? ($profilesState.profiles.find((p) => p.id === rule.profileId)?.display_name ?? null) : null,
  );
  // Deleting originals is the one part of an unverified rule that cannot be undone, so it
  // is the one part the user must re-arm for this specific card.
  const askDeleteConfirmation = $derived(!!rule && unverified && !rule.keepFiles);
  const willDelete = $derived(!!rule && (unverified ? confirmDelete : !rule.keepFiles));

  // Every candidate is a different physical card; a confirmation given for the previous
  // one must never carry over to the next.
  $effect(() => {
    void device?.mount_path;
    confirmDelete = false;
  });

  function fmtGb(bytes: number): string {
    return `${Math.round(bytes / 1024 ** 3)} GB`;
  }

  async function accept() {
    starting = true;
    try {
      await autoImportState.accept(confirmDelete);
    } finally {
      starting = false;
    }
  }
</script>

{#if device}
  <div
    class="mb-5 flex flex-wrap items-center gap-3 rounded-xl border border-primary/40 bg-primary/5 px-4 py-3 shadow-sm"
    role="status"
  >
    <div class="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-primary/15 text-primary">
      <HardDrive class="h-5 w-5" />
    </div>
    <div class="min-w-0 flex-1">
      <div class="flex items-center gap-2">
        <span class="text-sm font-semibold text-foreground">
          {#if unverified}
            Card detected — are these the right settings?
          {:else if rule}
            Card detected — import with saved settings?
          {:else}
            Card detected — import now?
          {/if}
        </span>
        <Badge variant="secondary">DCIM</Badge>
        {#if rule}
          <Badge variant={unverified ? "outline" : "secondary"}>
            {unverified ? "Unconfirmed settings" : "Saved rule"}
          </Badge>
        {/if}
      </div>
      <p class="truncate text-xs text-muted-foreground" title={device.mount_path}>
        {#if rule}
          {device.name} → {ruleProfileName ?? "saved profile"}{rule.albumName
            ? ` · album “${rule.albumName}”`
            : ""} · {willDelete ? "deletes after verify" : "keeps source files"}
        {:else}
          {device.name} · {fmtGb(device.available_space)} free · keeps source files
        {/if}
      </p>
      {#if unverified}
        <p class="mt-1 text-xs text-muted-foreground">
          These settings were saved before this app could tell cards apart, so they may
          belong to a different card. Check them, then import to attach them to this card.
        </p>
      {/if}
      {#if askDeleteConfirmation}
        <div class="mt-2 flex items-center gap-2">
          <Switch
            id="auto-import-confirm-delete"
            aria-label="Delete originals from this card after verify"
            checked={confirmDelete}
            onCheckedChange={(v) => (confirmDelete = v)}
          />
          <Label for="auto-import-confirm-delete" class="cursor-pointer text-xs font-normal">
            Also delete originals from this card after verify
          </Label>
        </div>
      {/if}
    </div>
    <div class="flex shrink-0 items-center gap-2">
      <Button variant="ghost" size="sm" onclick={() => autoImportState.dismiss()} disabled={starting}>
        <X class="h-4 w-4" /> Not now
      </Button>
      <Button size="sm" onclick={accept} disabled={starting}>
        <Play class="h-4 w-4" /> {starting ? "Starting…" : "Import"}
      </Button>
    </div>
  </div>
{/if}
