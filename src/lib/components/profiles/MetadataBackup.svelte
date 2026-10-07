<script lang="ts">
  import { get } from "svelte/store";
  import { open, save } from "@tauri-apps/plugin-dialog";
  import { Button } from "$lib/components/ui/button";
  import { profilesState } from "$lib/state/profiles";
  import { importOptionsState } from "$lib/state/import-options";
  import { avatarDisplayState, paletteState, themeState } from "$lib/state/theme";
  import { exportMetadata, importMetadata, readMetadata, type BackupUiSettings, type MetadataBackup } from "$lib/insights-api";
  import type { Profile } from "$lib/types";

  let { onNeedsKey }: { onNeedsKey: (profile: Profile) => void } = $props();
  let busy = $state(false);
  let error = $state("");
  let message = $state("");
  let preview = $state<MetadataBackup | null>(null);
  let restoreSettings = $state(false);
  let missingKeys = $state<string[]>([]);

  function currentSettings(): BackupUiSettings {
    const options = get(importOptionsState);
    return {
      stackRawJpeg: options.stackRawJpeg, stackBurst: options.stackBurst,
      concurrentTasks: options.concurrentTasks, keepGoingOnErrors: options.keepGoingOnErrors,
      sessionTag: options.sessionTag, excludeExtensions: [...options.excludeExtensions],
      theme: themeState.mode, palette: paletteState.palette, avatarDisplay: avatarDisplayState.display,
    };
  }

  function restoreUiSettings(settings: BackupUiSettings) {
    // Unlike the stores' normal best-effort saves, backup restoration must
    // report persistence failures instead of claiming a complete round-trip.
    localStorage.setItem("immich-shuttle-import-defaults", JSON.stringify({
      stackRawJpeg: settings.stackRawJpeg, stackBurst: settings.stackBurst,
      concurrentTasks: settings.concurrentTasks, keepGoingOnErrors: settings.keepGoingOnErrors,
      sessionTag: settings.sessionTag, excludeExtensions: settings.excludeExtensions,
    }));
    localStorage.setItem("immich-shuttle-theme", settings.theme);
    localStorage.setItem("immich-shuttle-palette", settings.palette);
    localStorage.setItem("immich-shuttle-avatar-display", settings.avatarDisplay);
    importOptionsState.setStackRawJpeg(settings.stackRawJpeg);
    importOptionsState.setStackBurst(settings.stackBurst);
    importOptionsState.setConcurrentTasks(settings.concurrentTasks);
    importOptionsState.setKeepGoingOnErrors(settings.keepGoingOnErrors);
    importOptionsState.setSessionTag(settings.sessionTag);
    importOptionsState.setExcludeExtensions(settings.excludeExtensions);
    themeState.setMode(settings.theme);
    paletteState.setPalette(settings.palette);
    avatarDisplayState.setDisplay(settings.avatarDisplay);
  }

  async function exportBackup() {
    if (busy) return;
    busy = true;
    error = "";
    message = "";
    try {
      const path = await save({ defaultPath: "immich-shuttle-metadata.json", filters: [{ name: "JSON backup", extensions: ["json"] }] });
      if (!path) return;
      await exportMetadata(path, currentSettings());
      message = "Metadata backup saved. It contains no API keys. Existing files never change.";
    } catch (reason) {
      error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      busy = false;
    }
  }

  async function chooseBackup() {
    if (busy) return;
    busy = true;
    error = "";
    message = "";
    preview = null;
    restoreSettings = false;
    try {
      const path = await open({ multiple: false, directory: false, filters: [{ name: "JSON backup", extensions: ["json"] }] });
      if (typeof path !== "string") return;
      preview = await readMetadata(path);
    } catch (reason) {
      error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      busy = false;
    }
  }

  async function restoreBackup() {
    if (busy || !preview) return;
    busy = true;
    error = "";
    message = "";
    try {
      const result = await importMetadata(preview, restoreSettings);
      missingKeys = result.needs_api_key;
      preview = null;
      await profilesState.loadProfiles();
      message = `Imported ${result.added} new profiles and merged ${result.merged} existing profiles. Existing API keys stay unchanged.`;
      if (result.ui_settings) {
        try {
          restoreUiSettings(result.ui_settings);
          message += " Import defaults and display settings restored.";
        } catch (reason) {
          error = `Profiles saved, but display or import settings did not fully persist: ${String(reason)}`;
        }
      }
    } catch (reason) {
      error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      busy = false;
    }
  }
</script>

<section class="flex flex-col gap-3 rounded-lg border border-border p-3" aria-label="Metadata backup">
  <h3 class="text-sm font-medium">Profiles and settings backup</h3>
  <p class="text-xs text-muted-foreground">
    Metadata only: server endpoints, profile names and IDs, import defaults, and display settings.
    Re-enter API keys on another machine. Existing matching profiles keep their keys.
    Backups omit history, media, automation rules, and secrets.
    Export removes URL credentials, queries, and fragments.
  </p>
  <div class="flex flex-wrap gap-2">
    <Button variant="outline" size="sm" disabled={busy} onclick={exportBackup}>Export metadata</Button>
    <Button variant="outline" size="sm" disabled={busy} onclick={chooseBackup}>Import metadata</Button>
  </div>
  {#if preview}
    <div class="flex flex-col gap-2 rounded-md bg-muted p-3">
      <p class="text-sm">Import {preview.profiles.length} profiles. This merge never deletes existing profiles or replaces API keys.</p>
      <ul class="max-h-36 overflow-auto text-xs">
        {#each preview.profiles as profile (profile.id)}
          <li class="py-1">{profile.display_name} — {profile.server_url}</li>
        {/each}
      </ul>
      <label class="flex items-start gap-2 text-sm">
        <input type="checkbox" bind:checked={restoreSettings} disabled={busy} class="mt-1" />
        <span>Also replace saved import defaults and display settings. Keep originals default: {preview.defaults.keep_files_on_disk ? "yes" : "no"}.</span>
      </label>
      <p class="text-xs text-muted-foreground">An existing profile ID must have matching server endpoints. Conflicts stop the entire import.</p>
      <div class="flex gap-2">
        <Button size="sm" disabled={busy} onclick={restoreBackup}>Confirm metadata import</Button>
        <Button variant="ghost" size="sm" disabled={busy} onclick={() => preview = null}>Cancel</Button>
      </div>
    </div>
  {/if}
  <div aria-live="polite">
    {#if busy}<p class="text-xs text-muted-foreground">Working…</p>{/if}
    {#if message}<p class="text-xs">{message}</p>{/if}
    {#if error}<p class="text-xs text-destructive" role="alert">{error}</p>{/if}
  </div>
  {#if missingKeys.length > 0}
    <div class="flex flex-col gap-1">
      <p class="text-sm font-medium">API key re-entry required</p>
      {#each missingKeys as id (id)}
        {@const profile = $profilesState.profiles.find((p) => p.id === id)}
        {#if profile}
          <Button variant="outline" size="sm" disabled={busy} onclick={() => onNeedsKey(profile)}>Add API key for {profile.display_name}</Button>
        {/if}
      {/each}
    </div>
  {/if}
</section>
