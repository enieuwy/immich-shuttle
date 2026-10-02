<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { open } from "@tauri-apps/plugin-dialog";
  import { activeProfile, profilesState } from "$lib/state/profiles";
  import { albumsState } from "$lib/state/albums";
  import { sourceState } from "$lib/state/source";
  import { selectionState } from "$lib/state/selection";
  import { importOptionsState, isDateRangeInvalid, toImmichDateRange } from "$lib/state/import-options";
  import { queueState } from "$lib/state/queue";
  import { panelTab } from "$lib/state/ui";
  import type { ImportExtensions, ImportInput } from "$lib/types";
  import { Button } from "$lib/components/ui/button";

  let kind = $state<NonNullable<ImportExtensions["source"]>>("folder");
  let paths = $state("");
  let sourceProfile = $state("");
  let selectedAlbums = $state<string[]>([]);
  let recipients = $state<string[]>([]);
  let publicLink = $state(false);
  let shareRole = $state<"viewer" | "editor">("viewer");
  let dateFromName = $state(true);
  let zone = $state("");
  let offset = $state(0);
  let callback = $state("");
  let useSelection = $state(false);
  let busy = $state(false);
  let message = $state("");
  let error = $state("");
  let serverPath = $state("");
  let libraryName = $state("");
  let confirmedPath = $state(false);
  let profileScope = $state<string | null>(null);
  const control = "w-full rounded-md border border-input bg-background px-2 py-1.5 text-sm";
  $effect(() => {
    const id = $activeProfile?.id ?? null;
    if (id !== profileScope) {
      profileScope = id;
      selectedAlbums = []; recipients = []; publicLink = false;
      callback = ""; confirmedPath = false; message = ""; error = "";
    }
  });

  async function pick(directory: boolean) {
    const result = await open({ directory, multiple: true, title: directory ? "Choose import folders" : "Choose exported archives" });
    if (result) paths = (Array.isArray(result) ? result : [result]).join("\n");
  }
  function request(dryRun: boolean): ImportInput {
    const profile = $activeProfile;
    if (!profile) throw new Error("Choose a destination profile first.");
    if (selectedAlbums.length && $albumsState.loadedProfileId !== profile.id) throw new Error("Wait for this profile's albums to load.");
    const options = $importOptionsState;
    const selection = useSelection && kind === "folder";
    if (selection && ($sourceState.scanOutcome !== "complete" || $selectionState.selected.size === 0)) {
      throw new Error("Complete a source scan and select at least one file first.");
    }
    if (!selection && isDateRangeInvalid(options.dateFrom, options.dateTo)) {
      throw new Error("The From date must not be after the To date.");
    }
    const sourcePaths = kind === "immich" ? [] : selection ? [...$sourceState.selectedPaths] : paths.split("\n").map(p => p.trim()).filter(Boolean);
    const album = $albumsState.availableAlbums.find(a => a.id === selectedAlbums[0]);
    return {
      profile_id: profile.id, source_paths: sourcePaths, album_ids: [...selectedAlbums], keep_files: true,
      stack_raw_jpeg: options.stackRawJpeg, stack_burst: options.stackBurst,
      concurrent_tasks: options.concurrentTasks, date_range: selection ? null : toImmichDateRange(options.dateFrom, options.dateTo),
      select_files: selection ? [...$selectionState.selected] : null,
      into_album: album?.album_name ?? null, organization: "single_album", on_errors: options.keepGoingOnErrors ? "continue" : "stop",
      tags: [...options.tags], session_tag: options.sessionTag, overwrite: options.overwrite,
      include_type: selection || options.mediaType === "all" ? null : options.mediaType === "image" ? "IMAGE" : "VIDEO",
      include_extensions: selection ? [] : [...options.includeExtensions], exclude_extensions: selection ? [] : [...options.excludeExtensions],
      extended: { source: kind, source_profile_id: kind === "immich" ? sourceProfile : null,
        date_from_name: dateFromName, time_zone: zone.trim() || null, clock_offset_minutes: offset,
        dry_run: dryRun, completion_webhook_url: callback.trim() || null,
        share_user_ids: [...recipients], share_role: shareRole, public_link: publicLink },
    };
  }
  async function start(dryRun: boolean) {
    error = ""; message = ""; busy = true;
    try {
      const input = request(dryRun);
      await queueState.startRequest(input);
      panelTab.set("queue");
      message = dryRun ? "Plan started. Read the dry-run result in Queue before starting an upload." : "Import started. All source originals stay on disk.";
    } catch (reason) { error = String(reason); }
    finally { busy = false; }
  }
  async function storage() {
    if (!$activeProfile) return;
    error = ""; busy = true;
    try {
      const info = await invoke<{available_bytes: number | null; disk_warning: string | null}>("import_storage", {profileId: $activeProfile.id});
      message = info.available_bytes == null ? "The server did not report available capacity." : `Reported headroom: ${(info.available_bytes / 1024 ** 3).toFixed(2)} GiB. Folder imports also check capacity before upload.`;
      if (info.disk_warning) message += " Disk capacity is unavailable; the server may require admin permission.";
    } catch (reason) { error = String(reason); }
    finally { busy = false; }
  }
  async function register() {
    if (!$activeProfile) return;
    error = ""; busy = true;
    try {
      const result = await invoke<{id: string}>("import_register_library", {profileId: $activeProfile.id, name: libraryName, serverPath, confirmServerPath: confirmedPath});
      message = `External library ${result.id} registered. The server scan has started; no copy upload ran.`;
      confirmedPath = false;
    } catch (reason) { error = String(reason); }
    finally { busy = false; }
  }
</script>

<details class="rounded-xl border bg-card p-4">
  <summary class="cursor-pointer text-sm font-semibold">Migration and import planning</summary>
  <div class="mt-4 space-y-4">
    <p class="text-xs text-muted-foreground">Destination: {$activeProfile?.display_name ?? "Choose a profile"}. This workflow keeps all originals. The standard import controls still support verified Trash deletion.</p>
    <label class="block text-xs">Import source
      <select class={control} bind:value={kind} disabled={busy}>
        <option value="folder">Folder or current preview selection</option>
        <option value="google_photos">Google Photos Takeout</option>
        <option value="icloud">Apple iCloud export</option>
        <option value="immich">Another Immich server</option>
      </select>
    </label>
    {#if kind === "immich"}
      <label class="block text-xs">Source profile
        <select class={control} bind:value={sourceProfile}><option value="">Choose source server</option>
          {#each $profilesState.profiles.filter(p => p.id !== $activeProfile?.id) as p}<option value={p.id}>{p.display_name}</option>{/each}
        </select>
      </label>
      <p class="text-xs text-muted-foreground">Copies accessible assets. It never deletes assets or pauses jobs on the source server.</p>
    {:else}
      <label class="block text-xs">Source paths, one per line<textarea class={control} rows="3" bind:value={paths} placeholder="/path/to/export"></textarea></label>
      <div class="flex gap-2"><Button variant="outline" size="sm" onclick={() => pick(true)}>Choose folder</Button>
        {#if kind !== "folder"}<Button variant="outline" size="sm" onclick={() => pick(false)}>Choose archive</Button>{/if}
      </div>
      {#if kind === "folder"}<label class="flex items-center gap-2 text-xs"><input type="checkbox" bind:checked={useSelection} />Use current preview selection ({$selectionState.selected.size} files)</label>{/if}
      {#if kind === "icloud"}<p class="text-xs text-muted-foreground">Choose an iCloud data export folder or ZIP. Direct access to an Apple Photos library is not supported by the bundled uploader.</p>{/if}
    {/if}
    <label class="block text-xs">Destination albums (hold Command or Control to select several)
      <select class={control} multiple size="4" bind:value={selectedAlbums} disabled={$albumsState.loadedProfileId !== $activeProfile?.id}>
        {#each $albumsState.availableAlbums as album}<option value={album.id}>{album.album_name}</option>{/each}
      </select>
    </label>
    <fieldset class="space-y-2 rounded-md border p-3"><legend class="px-1 text-xs">Capture dates</legend>
      {#if kind === "folder" || kind === "icloud"}<label class="flex items-center gap-2 text-xs"><input type="checkbox" bind:checked={dateFromName} />Use filename dates when metadata has no date</label>{/if}
      <label class="block text-xs">Time zone (for example, Australia/Perth)<input class={control} bind:value={zone} /></label>
      <label class="block text-xs">Clock correction in minutes<input class={control} type="number" min="-525600" max="525600" step="1" bind:value={offset} /></label>
      <p class="text-xs text-muted-foreground">Clock correction changes only newly uploaded assets on the server. Original files and existing duplicates stay unchanged.</p>
    </fieldset>
    <fieldset class="space-y-2 rounded-md border p-3"><legend class="px-1 text-xs">Optional completion actions</legend>
      <label class="block text-xs">Share destination albums with users<select class={control} multiple size="3" bind:value={recipients}>
        {#each $albumsState.availableUsers as user}<option value={user.id}>{user.name}</option>{/each}
      </select></label>
      <label class="block text-xs">Recipient role<select class={control} bind:value={shareRole}><option value="viewer">Viewer</option><option value="editor">Editor</option></select></label>
      <label class="flex items-center gap-2 text-xs"><input type="checkbox" bind:checked={publicLink} />Create public links for these albums after a successful import</label>
      <p class="text-xs text-muted-foreground">Sharing exposes the entire destination album, including existing assets. Public links allow anyone with the link to view it.</p>
      <label class="block text-xs">HTTP completion callback<input class={control} type="url" bind:value={callback} placeholder="https://automation.example/callback" /></label>
      <p class="text-xs text-muted-foreground">The callback receives the result, profile ID, album IDs, and local source paths. Leave it empty to send nothing.</p>
    </fieldset>
    <div class="flex flex-wrap gap-2">
      <Button variant="outline" size="sm" disabled={busy || !$activeProfile} onclick={storage}>Check capacity</Button>
      <Button variant="outline" size="sm" disabled={busy || !$activeProfile} onclick={() => start(true)}>Preview import plan</Button>
      <Button size="sm" disabled={busy || !$activeProfile} onclick={() => start(false)}>Start migration import</Button>
    </div>
    <details class="rounded-md border p-3"><summary class="cursor-pointer text-xs font-medium">Index a server-visible external library</summary>
      <div class="mt-3 space-y-2">
        <p class="text-xs text-muted-foreground">This registers a server path for read-in-place indexing. It does not upload local files. The server requires library permissions.</p>
        <label class="block text-xs">Library name<input class={control} bind:value={libraryName} /></label>
        <label class="block text-xs">Absolute path inside the Immich server<input class={control} bind:value={serverPath} placeholder="/external/photos" /></label>
        <label class="flex items-center gap-2 text-xs"><input type="checkbox" bind:checked={confirmedPath} />I confirm the server can read this mounted path.</label>
        <Button variant="outline" size="sm" disabled={busy || !confirmedPath || !$activeProfile} onclick={register}>Register and scan library</Button>
      </div>
    </details>
    {#if message}<p role="status" class="text-xs">{message}</p>{/if}
    {#if error}<p role="alert" class="text-xs text-destructive">{error}</p>{/if}
  </div>
</details>
