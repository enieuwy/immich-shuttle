<script lang="ts">
  import { Bookmark, BookmarkCheck, X } from "@lucide/svelte";

  import { albumsState } from "$lib/state/albums";
  import { deviceKey, deviceRulesState } from "$lib/state/device-rules";
  import { importOptionsState } from "$lib/state/import-options";
  import { activeProfile, profilesState } from "$lib/state/profiles";
  import { sourceState } from "$lib/state/source";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";

  // The single selected source that is also a detected removable card. Rules only make
  // sense for a card the OS can identify by volume id; label and mount path are shared
  // between cards, so a rule keyed by those would route the wrong card's photos.
  const card = $derived(
    $sourceState.selectedPaths.length === 1
      ? ($sourceState.detectedDevices.find((d) => d.mount_path === $sourceState.selectedPaths[0]) ??
          null)
      : null,
  );

  const canRemember = $derived(card ? deviceKey(card) !== null : false);

  // Reading the store map keeps this derived reactive; `lookup` adds the legacy
  // classification the plain map cannot express.
  const match = $derived.by(() => {
    void $deviceRulesState;
    return card ? deviceRulesState.lookup(card) : null;
  });
  const existing = $derived(match?.rule ?? null);
  const existingUnverified = $derived(match?.needsConfirmation ?? false);

  const selectedAlbumName = $derived(
    $albumsState.selectedAlbumIds.length > 0
      ? ($albumsState.availableAlbums.find((a) => a.id === $albumsState.selectedAlbumIds[0])
          ?.album_name ?? null)
      : null,
  );

  const existingProfileName = $derived(
    existing
      ? ($profilesState.profiles.find((p) => p.id === existing.profileId)?.display_name ?? "a profile")
      : null,
  );

  function save() {
    const profile = $activeProfile;
    if (!card || !profile) return;
    const options = $importOptionsState;
    const rule = {
      profileId: profile.id,
      albumName: selectedAlbumName,
      keepFiles: options.keepFiles,
      stackRawJpeg: options.stackRawJpeg,
      stackBurst: options.stackBurst,
      organization: options.organization,
    };
    // An explicit save supersedes the ambiguous legacy entry for this card; leaving it
    // behind would keep offering it to every other card sharing that label or mount point.
    if (existingUnverified) {
      deviceRulesState.migrateLegacyRule(card, rule);
      return;
    }
    deviceRulesState.saveRule(card, rule);
  }
</script>

{#if card}
  <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
    <div class="flex items-start justify-between gap-3">
      <div class="flex min-w-0 flex-col items-start gap-1">
        <span class="text-sm font-medium text-foreground">Remember settings for this card</span>
        {#if canRemember}
          <span class="text-xs text-muted-foreground">
            Re-inserting <span class="font-medium">{card.name}</span> replays this profile, album,
            and wipe choice automatically.
          </span>
        {:else}
          <span class="text-xs text-muted-foreground">
            This system cannot read a volume ID for <span class="font-medium">{card.name}</span>, so
            its settings cannot be remembered. Another card with the same label or mount point
            would inherit them.
          </span>
        {/if}
      </div>
      {#if existing}
        <Button
          variant="ghost"
          size="sm"
          class="shrink-0"
          onclick={() => deviceRulesState.removeRule(card)}
        >
          <X class="h-4 w-4" /> Forget
        </Button>
      {/if}
    </div>

    {#if existing}
      <div class="mt-2 flex flex-wrap items-center gap-2">
        <Badge variant={existingUnverified ? "outline" : "secondary"}>
          <BookmarkCheck class="mr-1 h-3 w-3" />
          {existingProfileName}{existing.albumName ? ` · ${existing.albumName}` : ""} · {existing.keepFiles
            ? "keeps files"
            : "deletes after verify"}
        </Badge>
        {#if existingUnverified}
          <span class="text-xs text-muted-foreground">
            Saved before cards could be told apart — confirm on the next insert, or update it now.
          </span>
        {/if}
        {#if canRemember}
          <button
            type="button"
            class="text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
            onclick={save}
          >
            Update to current settings
          </button>
        {/if}
      </div>
    {:else if canRemember}
      <Button variant="secondary" size="sm" class="mt-2" onclick={save}>
        <Bookmark class="h-4 w-4" /> Remember this card
      </Button>
    {/if}
  </div>
{/if}
