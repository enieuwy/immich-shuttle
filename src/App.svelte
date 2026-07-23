<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { Play, FileText, KeyRound } from "@lucide/svelte";

  import AppLayout from "$lib/components/layout/AppLayout.svelte";
  import ThemeToggle from "$lib/components/layout/ThemeToggle.svelte";
  import AlbumSelector from "$lib/components/albums/AlbumSelector.svelte";
  import ErrorToast from "$lib/components/feedback/ErrorToast.svelte";
  import LogViewer from "$lib/components/feedback/LogViewer.svelte";
  import ImportOptions from "$lib/components/import/ImportOptions.svelte";
  import ImportQueue from "$lib/components/queue/ImportQueue.svelte";
  import HistoryPanel from "$lib/components/queue/HistoryPanel.svelte";
  import OnboardingOverlay from "$lib/components/onboarding/OnboardingOverlay.svelte";
  import ProfileManager from "$lib/components/profiles/ProfileManager.svelte";
  import ProfileSelector from "$lib/components/profiles/ProfileSelector.svelte";
  import SourcePicker from "$lib/components/source/SourcePicker.svelte";
  import AutoImportBanner from "$lib/components/source/AutoImportBanner.svelte";
  import PreviewDialog from "$lib/components/preview/PreviewDialog.svelte";
  import { Button } from "$lib/components/ui/button";
  import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "$lib/components/ui/dialog";
  import { activeProfile, getProfilesSnapshot, profilesState } from "$lib/state/profiles";
  import { albumsState } from "$lib/state/albums";
  import type { Profile } from "$lib/types";
  import { queueState } from "$lib/state/queue";
  import { selectionState } from "$lib/state/selection";
  import { sourceState } from "$lib/state/source";
  import { openProfileEditor, panelTab } from "$lib/state/ui";
  import { isDateRangeInvalid, importOptionsState } from "$lib/state/import-options";


  let showManager = $state(false);
  let showLogs = $state(false);
  let showOnboarding = $state(false);
  let importError = $state("");
  let editTarget = $state<Profile | null>(null);

  // When a profile has no API key, the Albums CTA requests its editor — open the
  // profile manager straight on that profile so the user lands on the key field.
  $effect(() => {
    if ($openProfileEditor) {
      editTarget = $activeProfile;
      showManager = true;
      openProfileEditor.set(false);
    }
  });

  // Files chosen in Preview & select, intersected with the current scan so a
  // stale selection from a previous source can never be staged.
  const selectedPaths = $derived.by(() => {
    const files = $sourceState.scanResult?.files ?? [];
    const valid = new Set(files.map((f) => f.path));
    const out: string[] = [];
    for (const path of $selectionState.selected) {
      if (valid.has(path)) out.push(path);
    }
    return out;
  });
  const selectedCount = $derived(selectedPaths.length);
  const dateRangeInvalid = $derived(
    isDateRangeInvalid($importOptionsState.dateFrom, $importOptionsState.dateTo),
  );


  async function startImport() {
    importError = "";
    if (dateRangeInvalid) {
      importError = "The start date must be on or before the end date.";
      return;
    }

    const selection = selectedPaths;
    try {
      await queueState.startImport(
        selection.length > 0 ? { selectFiles: selection } : {},
      );
      selectionState.clear();
    } catch (error) {
      importError = error instanceof Error ? error.message : String(error);
    }
  }

  onMount(() => {
    let disposed = false;
    let unlistenClose: (() => void) | undefined;
    let allowCloseAfterCancel = false;
    let cancellingForClose = false;


    void profilesState.loadProfiles().then(() => {
      if (getProfilesSnapshot().profiles.length === 0) {
        showOnboarding = true;
      }
    });
    void queueState.loadJobs();
    queueState.startPolling();

    void getCurrentWindow()
      .onCloseRequested((event) => {
        if (allowCloseAfterCancel) {
          return;
        }
        if (cancellingForClose) {
          event.preventDefault();
          return;
        }

        const runningJobs = $queueState.jobs.filter((job) => job.status === "running");
        if (runningJobs.length === 0) {
          return;
        }
        event.preventDefault();
        if (
          !window.confirm(
            "An import is in progress. Quit now and cancel the running import?",
          )
        ) {
          return;
        }
        cancellingForClose = true;

        void (async () => {
          let cancellationTimeout: ReturnType<typeof setTimeout> | undefined;
          try {
            await Promise.race([
              Promise.allSettled(runningJobs.map((job) => queueState.cancelImport(job.id))),
              new Promise<void>((resolve) => {
                cancellationTimeout = setTimeout(resolve, 5_000);
              }),
            ]);
            allowCloseAfterCancel = true;
            await getCurrentWindow().close();
          } catch (error) {
            importError = error instanceof Error ? error.message : String(error);
          } finally {
            if (cancellationTimeout !== undefined) {
              clearTimeout(cancellationTimeout);
            }
            cancellingForClose = false;
          }
        })();

      })
      .then((fn) => {
        // Drop the handler if the component unmounted before registration
        // resolved, so a remount can't stack duplicate close-confirm prompts.
        if (disposed) fn();
        else unlistenClose = fn;
      });

    return () => {
      disposed = true;
      queueState.stopPolling();
      unlistenClose?.();
    };
  });

</script>

<AppLayout>
  {#snippet profile()}
    <ProfileSelector onManage={() => { editTarget = null; showManager = !showManager; }} />
  {/snippet}

  {#snippet actions()}
    <ThemeToggle />
  {/snippet}

  {#if $albumsState.missingApiKey}
    <div class="flex flex-wrap items-center gap-3 rounded-lg border border-amber-500/40 bg-amber-500/10 px-4 py-3">
      <KeyRound class="size-5 shrink-0 text-amber-600 dark:text-amber-400" />
      <div class="min-w-0 flex-1">
        <p class="text-sm font-medium text-foreground">This profile has no API key</p>
        <p class="text-xs text-muted-foreground">Albums, users, and imports won't work until you add one.</p>
      </div>
      <Button size="sm" class="shrink-0" onclick={() => openProfileEditor.set(true)}>Add API key</Button>
    </div>
  {/if}

  <AutoImportBanner />

  <div class="grid grid-cols-1 items-start gap-5 lg:grid-cols-2">
    <div class="flex flex-col gap-5">
      <SourcePicker />
      <ImportOptions />
    </div>
    <div class="flex flex-col gap-5">
      <AlbumSelector />
      {#if $panelTab === "queue"}
        <ImportQueue />
      {:else}
        <HistoryPanel />
      {/if}
    </div>
  </div>

  {#snippet footer()}
    {@const jobs = $queueState.jobs}
    {@const running = jobs.filter((j) => j.status === "running").length}
    {@const completed = jobs.filter((j) => j.status === "completed").length}
    {@const failed = jobs.filter((j) => j.status === "failed").length}
    <div class="flex w-full items-center justify-between gap-4">
      <div class="flex min-w-0 items-center gap-3">
        {#if jobs.length === 0 || (running === 0 && completed === 0 && failed === 0)}
          <span class="text-muted-foreground">Ready to import</span>
        {:else}
          {#if running > 0}
            <span class="font-medium text-primary tabular-nums">{running} running</span>
          {/if}
          {#if completed > 0}
            <span class="text-emerald-600 tabular-nums dark:text-emerald-400">{completed} completed</span>
          {/if}
          {#if failed > 0}
            <span class="text-destructive tabular-nums">{failed} failed</span>
          {/if}
        {/if}
        {#if importError}
          <span class="truncate text-xs text-destructive" title={importError}>{importError}</span>
        {/if}
      </div>
      <div class="flex shrink-0 items-center gap-2">
        <Button variant="ghost" size="sm" onclick={() => (showLogs = true)}>
          <FileText class="size-4" /> Logs
        </Button>
        <Button size="sm" class="btn-brand" onclick={startImport} disabled={dateRangeInvalid}>
          <Play class="size-4" />
          {selectedCount > 0 ? `Import ${selectedCount} selected` : "Start Import"}
        </Button>
      </div>
    </div>
  {/snippet}
</AppLayout>

<Dialog bind:open={showManager}>
  <DialogContent class="max-w-md">
    <DialogHeader>
      <DialogTitle>Manage Users</DialogTitle>
      <DialogDescription>Add or edit Immich user profiles.</DialogDescription>
    </DialogHeader>
    <ProfileManager
      initialEdit={editTarget}
      onDone={() => {
        showManager = false;
        editTarget = null;
        void albumsState.loadAlbums();
      }}
    />
  </DialogContent>
</Dialog>

<LogViewer bind:open={showLogs} />

<PreviewDialog />

{#if showOnboarding}
  <OnboardingOverlay onDone={() => (showOnboarding = false)} />
{/if}

<ErrorToast />
