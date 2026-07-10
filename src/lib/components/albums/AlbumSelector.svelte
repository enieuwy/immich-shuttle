<script lang="ts">
  import { Link, Search, Plus, X, Images, KeyRound } from "@lucide/svelte";
  import { userDisplayNames } from "$lib/users";

  import { albumsState } from "$lib/state/albums";
  import { activeProfile } from "$lib/state/profiles";
  import { Button } from "$lib/components/ui/button";
  import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "$lib/components/ui/dialog";
  import { Input } from "$lib/components/ui/input";
  import { Badge } from "$lib/components/ui/badge";
  import { Card, CardHeader, CardTitle, CardAction, CardContent } from "$lib/components/ui/card";
  import { Label } from "$lib/components/ui/label";
  import { Alert, AlertDescription } from "$lib/components/ui/alert";

  let search = $state("");
  let showCreate = $state(false);
  let newAlbumName = $state("");
  let selectedShareUserIds = $state<string[]>([]);
  let shareRole = $state<"viewer" | "editor">("viewer");
  let createPublicLink = $state(false);

  $effect(() => {
    const _profile = $activeProfile;
    const _search = search;
    const timer = setTimeout(() => {
      void albumsState.loadAlbums(_search || undefined);
    }, 150);
    return () => clearTimeout(timer);
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
</script>

<Card class="flex flex-col gap-4 py-4">
  <CardHeader class="px-4">
    <div class="flex items-center gap-2">
      <span class="flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <Images class="h-4 w-4" />
      </span>
      <CardTitle class="text-sm font-semibold text-foreground">Albums</CardTitle>
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
          {#each $albumsState.availableAlbums as album}
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
                  {#each album.shared_with.slice(0, 3) as user}
                    <span
                      class="grid size-4 place-items-center rounded-full bg-primary/70 text-[8px] font-semibold text-primary-foreground ring-1 ring-card"
                    >
                      {user.name.charAt(0).toUpperCase()}
                    </span>
                  {/each}
                  {#if album.shared_with.length > 3}
                    <span
                      class="grid size-4 place-items-center rounded-full bg-muted text-[8px] font-semibold text-muted-foreground ring-1 ring-card"
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
            {#each $albumsState.availableUsers as user}
              <label class="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  class="h-4 w-4 rounded border-border text-primary accent-primary focus:ring-primary"
                  checked={selectedShareUserIds.includes(user.id)}
                  onchange={() => toggleShareUser(user.id)}
                />
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
