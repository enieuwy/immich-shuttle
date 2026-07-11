<script lang="ts">
  import { Bookmark, BookmarkCheck, X } from "@lucide/svelte";

  import { albumsState } from "$lib/state/albums";
  import { deviceKey, deviceRulesState } from "$lib/state/device-rules";
  import { importOptionsState } from "$lib/state/import-options";
  import { activeProfile, profilesState } from "$lib/state/profiles";
  import { sourceState } from "$lib/state/source";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";

  // The single selected source that is also a detected removable card. Rules
  // only make sense for a card we can identify by label/mount.
  const card = $derived(
    $sourceState.selectedPaths.length === 1
      ? ($sourceState.detectedDevices.find((d) => d.mount_path === $sourceState.selectedPaths[0]) ??
          null)
      : null,
  );

  const existing = $derived(card ? ($deviceRulesState[deviceKey(card)] ?? null) : null);

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
    deviceRulesState.saveRule(card, {
      profileId: profile.id,
      albumName: selectedAlbumName,
      keepFiles: options.keepFiles,
      stackRawJpeg: options.stackRawJpeg,
      stackBurst: options.stackBurst,
      organization: options.organization,
    });
  }
</script>

{#if card}
  <div class="rounded-lg p-3 transition-colors hover:bg-muted/50">
    <div class="flex items-start justify-between gap-3">
      <div class="flex min-w-0 flex-col items-start gap-1">
        <span class="text-sm font-medium text-foreground">Remember settings for this card</span>
        <span class="text-xs text-muted-foreground">
          Re-inserting <span class="font-medium">{card.name}</span> replays this profile, album, and
          wipe choice automatically.
        </span>
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
        <Badge variant="secondary">
          <BookmarkCheck class="mr-1 h-3 w-3" />
          {existingProfileName}{existing.albumName ? ` · ${existing.albumName}` : ""} · {existing.keepFiles
            ? "keeps files"
            : "deletes after verify"}
        </Badge>
        <button
          type="button"
          class="text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
          onclick={save}
        >
          Update to current settings
        </button>
      </div>
    {:else}
      <Button variant="secondary" size="sm" class="mt-2" onclick={save}>
        <Bookmark class="h-4 w-4" /> Remember this card
      </Button>
    {/if}
  </div>
{/if}
