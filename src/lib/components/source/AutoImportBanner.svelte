<script lang="ts">
  import { HardDrive, Play, X } from "@lucide/svelte";

  import { autoImportState } from "$lib/state/auto-import";
  import { profilesState } from "$lib/state/profiles";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";

  let starting = $state(false);

  const device = $derived($autoImportState.candidate);
  const rule = $derived($autoImportState.candidateRule);
  const ruleProfileName = $derived(
    rule ? ($profilesState.profiles.find((p) => p.id === rule.profileId)?.display_name ?? null) : null,
  );

  function fmtGb(bytes: number): string {
    return `${Math.round(bytes / 1024 ** 3)} GB`;
  }

  async function accept() {
    starting = true;
    try {
      await autoImportState.accept();
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
          {rule ? "Card detected — import with saved settings?" : "Card detected — import now?"}
        </span>
        <Badge variant="secondary">DCIM</Badge>
        {#if rule}
          <Badge variant="secondary">Saved rule</Badge>
        {/if}
      </div>
      <p class="truncate text-xs text-muted-foreground" title={device.mount_path}>
        {#if rule}
          {device.name} → {ruleProfileName ?? "saved profile"}{rule.albumName
            ? ` · album “${rule.albumName}”`
            : ""} · {rule.keepFiles ? "keeps source files" : "deletes after verify"}
        {:else}
          {device.name} · {fmtGb(device.available_space)} free · keeps source files
        {/if}
      </p>
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
