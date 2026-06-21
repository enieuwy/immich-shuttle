<script lang="ts">
  import { Link, Search, Plus, X, Images } from "@lucide/svelte";

  import { albumsState } from "$lib/state/albums";
  import { activeProfile } from "$lib/state/profiles";
  import { Button } from "$lib/components/ui/button";
  import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "$lib/components/ui/dialog";
  import { Input } from "$lib/components/ui/input";
  import { Badge } from "$lib/components/ui/badge";
  import { ScrollArea } from "$lib/components/ui/scroll-area";
  import { Card, CardHeader, CardTitle, CardAction, CardContent } from "$lib/components/ui/card";
  import { Label } from "$lib/components/ui/label";
  import { Alert, AlertDescription } from "$lib/components/ui/alert";

  let search = $state("");
  let showCreate = $state(false);
  let newAlbumName = $state("");
  let selectedShareUserIds = $state<string[]>([]);
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
    await albumsState.createAlbum(newAlbumName.trim(), selectedShareUserIds, createPublicLink);
    newAlbumName = "";
    selectedShareUserIds = [];
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
      <Images class="h-4 w-4 text-muted-foreground" />
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
              >
                <X class="h-3 w-3" />
              </Button>
            </Badge>
          {/if}
        {/each}
      {:else}
        <Badge variant="outline" class="text-muted-foreground">No album</Badge>
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

    <ScrollArea class="h-[200px] rounded-md border border-border bg-card">
      <div class="sticky top-0 z-10 bg-card px-2 pt-2 pb-1">
        <div class="flex items-center gap-2 border-b border-border pb-1.5">
          <Search class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <input
            bind:value={search}
            placeholder="Search albums..."
            class="w-full bg-transparent text-sm text-foreground placeholder:text-muted-foreground focus:outline-none"
          />
          {#if search}
            <button type="button" class="text-muted-foreground transition-colors hover:text-foreground" onclick={() => (search = "")}>
              <X class="h-3.5 w-3.5" />
            </button>
          {/if}
        </div>
      </div>
      <div class="flex flex-col gap-1 p-2">
        {#if $albumsState.loading}
          <p class="px-2 py-1.5 text-sm text-muted-foreground">Loading albums…</p>
        {:else if $albumsState.availableAlbums.length === 0}
          <p class="px-2 py-1.5 text-sm text-muted-foreground">No albums match.</p>
        {:else}
          {#each $albumsState.availableAlbums as album}
            {@const selected = $albumsState.selectedAlbumIds.includes(album.id)}
            <button
              type="button"
              class="flex w-full flex-col items-start gap-0.5 rounded-md px-2 py-1.5 text-left text-sm transition-colors focus:outline-none {selected ? 'border-l-2 border-primary bg-primary/10 text-primary' : 'hover:bg-accent focus:bg-accent'}"
              onclick={() => selected ? albumsState.deselectAlbum(album.id) : albumsState.selectAlbum(album.id)}
            >
              <span class="font-medium {selected ? 'text-primary' : 'text-foreground'}">{album.album_name}</span>
              {#if album.shared_with.length > 0}
                <span class="text-xs text-muted-foreground">
                  shared with: {album.shared_with.map((user) => user.name).join(", ")}
                </span>
              {/if}
            </button>
          {/each}
        {/if}
      </div>
    </ScrollArea>

    {#if $albumsState.error}
      <p class="text-sm text-destructive">{$albumsState.error}</p>
    {/if}
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
