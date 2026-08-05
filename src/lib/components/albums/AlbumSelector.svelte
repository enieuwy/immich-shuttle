<script lang="ts">
  import { Link, Search, Plus, X, Images, KeyRound, ExternalLink } from "@lucide/svelte";
  import { userDisplayNames } from "$lib/users";
  import UserAvatar from "$lib/components/albums/UserAvatar.svelte";

  import { albumsState } from "$lib/state/albums";
  import { errorsState } from "$lib/state/errors";
  import { activeProfile } from "$lib/state/profiles";
  import { openInImmich, tagsList } from "$lib/api";
  import { Button } from "$lib/components/ui/button";
  import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "$lib/components/ui/dialog";
  import { Input } from "$lib/components/ui/input";
  import { Badge } from "$lib/components/ui/badge";
  import { Card, CardHeader, CardTitle, CardAction, CardContent } from "$lib/components/ui/card";
  import { Label } from "$lib/components/ui/label";
  import { Alert, AlertDescription } from "$lib/components/ui/alert";
  import { importOptionsState } from "$lib/state/import-options";
  import type { ImportOrganization, Tag } from "$lib/types";

  let search = $state("");
  let showCreate = $state(false);
  let newAlbumName = $state("");
  let selectedShareUserIds = $state<string[]>([]);
  let shareRole = $state<"viewer" | "editor">("viewer");
  let createPublicLink = $state(false);

  const tagsText = $derived($importOptionsState.tags.join(", "));
  function commitTags(raw: string) {
    importOptionsState.setTags(
      raw
        .split(",")
        .map((t) => t.trim())
        .filter((t) => t.length > 0),
    );
  }

  // --- Tag typeahead ---------------------------------------------------------
  // The field stays free-text (create-friendly, supports `/` hierarchy); we just
  // surface existing Immich tags as suggestions to curb typo-driven duplicates.
  let allTags = $state<Tag[]>([]);
  let tagInput = $state("");
  let tagFocused = $state(false);

  // Keep the raw input in sync when the committed tags change from elsewhere
  // (e.g. restoring a previous import), but never clobber what's being typed.
  $effect(() => {
    const next = tagsText;
    if (!tagFocused) tagInput = next;
  });

  // Load the profile's tags once per connection; suggestions are filtered
  // client-side (Immich's /tags has no query param).
  $effect(() => {
    const profile = $activeProfile;
    if (!profile) {
      allTags = [];
      return;
    }
    let cancelled = false;
    tagsList(profile.id)
      .then((tags) => {
        if (!cancelled) allTags = tags;
      })
      .catch(() => {
        // Suggestions are a nicety; a fetch failure just leaves the plain input.
        if (!cancelled) allTags = [];
      });
    return () => {
      cancelled = true;
    };
  });

  // The token being typed is the text after the last comma.
  const currentToken = $derived(tagInput.split(",").pop()?.trim() ?? "");
  const selectedTags = $derived(
    new Set($importOptionsState.tags.map((t) => t.toLowerCase())),
  );
  const tagSuggestions = $derived.by(() => {
    if (!tagFocused) return [] as Tag[];
    const token = currentToken.toLowerCase();
    return allTags
      .filter((t) => !selectedTags.has(t.value.toLowerCase()))
      .filter((t) => token === "" || t.value.toLowerCase().includes(token))
      .slice(0, 8);
  });

  function onTagInput(raw: string) {
    tagInput = raw;
    commitTags(raw);
  }

  function applySuggestion(tag: Tag) {
    const parts = tagInput.split(",");
    parts[parts.length - 1] = ` ${tag.value}`;
    // Land the caret on a fresh token so the next tag can be typed immediately.
    const next = `${parts.join(",").replace(/^\s+/, "")}, `;
    tagInput = next;
    commitTags(next);
  }

  // Selected album(s) first, then alphabetical — keeps the active choice on
  // screen without scrolling; the search box handles finding the rest.
  const sortedAlbums = $derived(
    [...$albumsState.availableAlbums].sort((a, b) => {
      const aSel = $albumsState.selectedAlbumIds.includes(a.id);
      const bSel = $albumsState.selectedAlbumIds.includes(b.id);
      if (aSel !== bSel) return aSel ? -1 : 1;
      return a.album_name.localeCompare(b.album_name);
    }),
  );

  $effect(() => {
    const _profile = $activeProfile;
    const _search = search;
    const timer = setTimeout(() => {
      void albumsState.loadAlbums(_search || undefined);
    }, 150);
    return () => {
      clearTimeout(timer);
      albumsState.cancelLoad();
    };
  });

  async function createAlbum() {
    if (!newAlbumName.trim()) {
      return;
    }
    await albumsState.createAlbum(newAlbumName.trim(), selectedShareUserIds, createPublicLink, shareRole);
    newAlbumName = "";
    selectedShareUserIds = [];
    shareRole = "viewer";
    createPublicLink = false;
    showCreate = false;
  }

  function toggleShareUser(userId: string) {
    if (selectedShareUserIds.includes(userId)) {
      selectedShareUserIds = selectedShareUserIds.filter((id) => id !== userId);
    } else {
      selectedShareUserIds = [...selectedShareUserIds, userId];
    }
  }

  async function openAlbumInImmich(albumId: string) {
    const profile = $activeProfile;
    if (!profile) return;
    try {
      await openInImmich(profile.id, albumId);
    } catch {
      errorsState.addError("Could not open Immich.");
    }
  }
</script>

<Card class="flex flex-col gap-4 py-4">
  <CardHeader class="px-4">
    <div class="flex items-center gap-2">
      <span class="flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <Images class="h-4 w-4" />
      </span>
      <CardTitle class="text-sm font-semibold text-foreground">Destination</CardTitle>
    </div>
    <CardAction>
      <Button variant="outline" size="sm" onclick={() => (showCreate = true)}>
        <Plus class="mr-2 h-4 w-4" /> Create album
      </Button>
    </CardAction>
  </CardHeader>

  <CardContent class="flex flex-col gap-4 px-4">
    <div class="flex flex-wrap gap-2">
      {#if $albumsState.selectedAlbumIds.length > 0}
        {#each $albumsState.selectedAlbumIds as albumId}
          {@const album = $albumsState.availableAlbums.find((entry) => entry.id === albumId)}
          {#if album}
            <Badge variant="secondary" class="gap-1 pr-1 bg-primary/10 text-primary border-primary/20">
              {album.album_name}
              <Button
                variant="ghost"
                size="icon-sm"
                class="h-4 w-4 rounded-full p-0 text-primary hover:bg-primary/20"
                onclick={() => albumsState.deselectAlbum(album.id)}
                aria-label={`Remove ${album.album_name} from selection`}
              >
                <X class="h-3 w-3" />
              </Button>
            </Badge>
          {/if}
        {/each}
        {@const openableAlbum = $albumsState.availableAlbums.find((a) => a.id === $albumsState.selectedAlbumIds[0])}
        {#if openableAlbum && $albumsState.loadedProfileId === $activeProfile?.id}
          <Button
            variant="ghost"
            size="sm"
            class="text-primary hover:bg-primary/10"
            onclick={() => openAlbumInImmich(openableAlbum.id)}
          >
            <ExternalLink class="mr-1 h-3.5 w-3.5" /> Open in Immich
          </Button>
        {/if}
      {:else}
        <Badge variant="outline" class="text-muted-foreground">No album selected</Badge>
      {/if}
    </div>

    {#if $albumsState.shareLinkUrl}
      {@const shareLinkUrl = $albumsState.shareLinkUrl ?? ""}
      <Alert class="border-primary/20 bg-primary/10 text-primary">
        <Link class="shrink-0" />
        <AlertDescription class="flex min-w-0 flex-col gap-2 text-primary sm:flex-row sm:items-center">
          <span class="min-w-0 flex-1 truncate font-mono text-xs" title={shareLinkUrl}>{shareLinkUrl}</span>
          <div class="flex shrink-0 items-center gap-1">
            <Button size="sm" onclick={() => $albumsState.shareLinkUrl && navigator.clipboard.writeText($albumsState.shareLinkUrl)}>Copy</Button>
            <Button
              variant="ghost"
              size="icon-sm"
              class="text-primary hover:bg-primary/20"
              aria-label="Dismiss share link"
              onclick={() => albumsState.clearShareLink()}
            >
              <X class="h-3.5 w-3.5" />
            </Button>
          </div>
        </AlertDescription>
      </Alert>
    {/if}

    <div class="flex items-center gap-2 rounded-md border border-border bg-card px-2 py-1.5">
      <Search class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
      <input
        bind:value={search}
        aria-label="Search albums"
        placeholder="Search albums..."
        class="w-full bg-transparent text-sm text-foreground placeholder:text-muted-foreground focus:outline-none"
      />
      {#if search}
        <button
          type="button"
          class="text-muted-foreground transition-colors hover:text-foreground"
          aria-label="Clear search"
          onclick={() => (search = "")}
        >
          <X class="h-3.5 w-3.5" />
        </button>
      {/if}
    </div>

    <div class="album-scroll h-[160px] overflow-y-auto rounded-md border border-border bg-card p-2">
      {#if $albumsState.missingApiKey}
        <div class="flex h-full flex-col items-center justify-center gap-1.5 py-4 text-center">
          <KeyRound class="size-5 text-muted-foreground/60" aria-hidden="true" />
          <p class="text-sm text-muted-foreground">Add an API key to load albums.</p>
        </div>
      {:else if $albumsState.loading}
        <p class="px-1 py-1 text-sm text-muted-foreground">Loading albums…</p>
      {:else if $albumsState.error}
        <div class="flex h-full flex-col items-center justify-center gap-2 py-4 text-center">
          <p class="text-sm text-muted-foreground">{$albumsState.error}</p>
          <Button size="sm" variant="outline" onclick={() => albumsState.loadAlbums(search || undefined)}>Retry</Button>
        </div>
      {:else if $albumsState.availableAlbums.length === 0}
        <p class="px-1 py-1 text-sm text-muted-foreground">No albums match.</p>
      {:else}
        <div class="flex flex-wrap gap-1.5">
          {#each sortedAlbums as album (album.id)}
            {@const selected = $albumsState.selectedAlbumIds.includes(album.id)}
            <button
              type="button"
              title={album.shared_with.length > 0
                ? `${album.album_name} — shared with ${userDisplayNames(album.shared_with).join(", ")}`
                : album.album_name}
              class="inline-flex max-w-[14rem] items-center gap-1.5 rounded-full border px-3 py-1 text-xs font-medium transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-ring {selected
                ? 'border-primary bg-primary/15 text-primary'
                : 'border-border bg-muted/40 text-foreground hover:bg-accent'}"
              onclick={() => (selected ? albumsState.deselectAlbum(album.id) : albumsState.selectAlbum(album.id))}
            >
              <span class="truncate">{album.album_name}</span>
              {#if album.shared_with.length > 0}
                <span class="flex shrink-0 -space-x-1" aria-hidden="true">
                  {#each album.shared_with.slice(0, 3) as user (user.id)}
                    <UserAvatar {user} profileId={$activeProfile?.id} class="size-4.5 text-[9px]" />
                  {/each}
                  {#if album.shared_with.length > 3}
                    <span
                      class="grid size-4.5 place-items-center rounded-full bg-muted text-[9px] font-semibold text-muted-foreground ring-1 ring-card"
                    >
                      +{album.shared_with.length - 3}
                    </span>
                  {/if}
                </span>
              {/if}
            </button>
          {/each}
        </div>
      {/if}
    </div>

    <div class="flex flex-col gap-3 border-t border-border/60 pt-3">
      <div>
        <div class="flex items-start justify-between gap-3">
          <Label
            for="album-option-organization"
            class="flex min-w-0 flex-col items-start gap-1 font-normal"
          >
            <span class="text-sm font-medium text-foreground">Organize into albums</span>
            <span class="text-xs text-muted-foreground">
              Group uploads by the source folder structure instead of one album.
            </span>
          </Label>
          <select
            id="album-option-organization"
            class="h-9 w-52 shrink-0 rounded-md border border-input bg-transparent px-2 text-sm shadow-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-label="Organize into albums"
            value={$importOptionsState.organization}
            onchange={(e) =>
              importOptionsState.setOrganization(e.currentTarget.value as ImportOrganization)}
          >
            <option value="single_album">Single album (selected)</option>
            <option value="folder_name">Album per folder name</option>
            <option value="folder_path">Album per folder path</option>
            <option value="folder_tags">Tag by folder path</option>
          </select>
        </div>
        {#if $importOptionsState.organization !== "single_album"}
          <p class="mt-2 text-xs text-muted-foreground">
            Albums or tags are derived from the source folders; the album picker above is ignored for this mode.
          </p>
        {/if}
      </div>

      <div>
        <Label
          for="album-option-tags"
          class="flex min-w-0 flex-col items-start gap-1 font-normal"
        >
          <span class="text-sm font-medium text-foreground">Tags</span>
          <span class="text-xs text-muted-foreground">Comma-separated tags applied to every uploaded asset. Use / for hierarchy (e.g. Trip/Iceland). Start typing to reuse an existing Immich tag.</span>
        </Label>
        <div class="relative mt-2">
          <Input
            id="album-option-tags"
            placeholder="Trip/Iceland, client-a"
            aria-label="Tags"
            autocomplete="off"
            role="combobox"
            aria-expanded={tagFocused && tagSuggestions.length > 0}
            aria-controls="album-option-tags-suggestions"
            value={tagInput}
            oninput={(e) => onTagInput(e.currentTarget.value)}
            onfocus={() => (tagFocused = true)}
            onblur={() => (tagFocused = false)}
          />
          {#if tagFocused && tagSuggestions.length > 0}
            <ul
              id="album-option-tags-suggestions"
              role="listbox"
              class="absolute z-20 mt-1 max-h-56 w-full overflow-auto rounded-md border border-border bg-popover p-1 shadow-md"
            >
              {#each tagSuggestions as tag (tag.id)}
                <li role="option" aria-selected="false">
                  <button
                    type="button"
                    class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm text-popover-foreground hover:bg-accent hover:text-accent-foreground"
                    onmousedown={(e) => {
                      e.preventDefault();
                      applySuggestion(tag);
                    }}
                  >
                    <Plus class="size-3.5 shrink-0 text-muted-foreground" />
                    <span class="truncate">{tag.value}</span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>
    </div>
  </CardContent>

  <Dialog bind:open={showCreate}>
    <DialogContent class="max-w-md">
      <DialogHeader>
        <DialogTitle>Create album</DialogTitle>
        <DialogDescription>Create a new album on your Immich server.</DialogDescription>
      </DialogHeader>
      <div class="flex flex-col gap-4">
        <div class="flex flex-col gap-2">
          <Label for="newAlbumName">Album name</Label>
          <Input id="newAlbumName" bind:value={newAlbumName} placeholder="Summer Vacation 2024" />
        </div>

        <div class="flex flex-col gap-2">
          <Label>Share with users (optional)</Label>
          <div class="flex flex-col gap-2 rounded-md border border-border bg-background p-3">
            {#each $albumsState.availableUsers as user (user.id)}
              <label class="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  class="h-4 w-4 rounded border-border text-primary accent-primary focus:ring-primary"
                  checked={selectedShareUserIds.includes(user.id)}
                  onchange={() => toggleShareUser(user.id)}
                />
                <UserAvatar {user} profileId={$activeProfile?.id} class="size-5 text-[9px]" />
                <span class="text-sm font-medium leading-none text-foreground">{user.name}</span>
              </label>
            {/each}
          </div>
          {#if selectedShareUserIds.length > 0}
            <div class="flex flex-col gap-1.5 pt-1">
              <span class="text-xs font-medium text-muted-foreground">Access level</span>
              <div class="flex gap-2">
                <label class="flex flex-1 items-center gap-2 rounded-md border border-border bg-background p-2 cursor-pointer">
                  <input
                    type="radio"
                    name="shareRole"
                    value="viewer"
                    class="h-4 w-4 accent-primary"
                    checked={shareRole === "viewer"}
                    onchange={() => (shareRole = "viewer")}
                  />
                  <span class="text-sm leading-tight text-foreground">Viewer<br /><span class="text-xs text-muted-foreground">Can view only</span></span>
                </label>
                <label class="flex flex-1 items-center gap-2 rounded-md border border-border bg-background p-2 cursor-pointer">
                  <input
                    type="radio"
                    name="shareRole"
                    value="editor"
                    class="h-4 w-4 accent-primary"
                    checked={shareRole === "editor"}
                    onchange={() => (shareRole = "editor")}
                  />
                  <span class="text-sm leading-tight text-foreground">Editor<br /><span class="text-xs text-muted-foreground">Can add &amp; delete</span></span>
                </label>
              </div>
            </div>
          {/if}
        </div>

        <label class="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            class="h-4 w-4 rounded border-border text-primary accent-primary focus:ring-primary"
            bind:checked={createPublicLink}
          />
          <span class="text-sm font-medium leading-none text-foreground">Create public link</span>
        </label>

        <Button onclick={createAlbum} class="w-full">Create album</Button>
      </div>
    </DialogContent>
  </Dialog>
</Card>

<style>
  /* Keep the album list's scrollbar visible (WebKit defaults to an auto-hiding
     overlay scrollbar) so it reads clearly as a scroll box. */
  .album-scroll::-webkit-scrollbar {
    width: 10px;
  }
  .album-scroll::-webkit-scrollbar-thumb {
    background-color: var(--border);
    border-radius: 9999px;
    border: 2px solid transparent;
    background-clip: content-box;
  }
  .album-scroll::-webkit-scrollbar-track {
    background: transparent;
  }
</style>
