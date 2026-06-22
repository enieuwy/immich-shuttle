<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { Play, FileText, ListChecks, History } from "@lucide/svelte";

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
  import { Tabs, TabsList, TabsTrigger, TabsContent } from "$lib/components/ui/tabs";
  import { getProfilesSnapshot, profilesState } from "$lib/state/profiles";
  import { queueState } from "$lib/state/queue";

  let showManager = $state(false);
  let showLogs = $state(false);
  let showOnboarding = $state(false);
  let importError = $state("");

  async function startImport() {
    importError = "";
    try {
      await queueState.startImport();
    } catch (error) {
      importError = error instanceof Error ? error.message : String(error);
    }
  }

  onMount(() => {
    let unlistenClose: (() => void) | undefined;
    void profilesState.loadProfiles().then(() => {
      if (getProfilesSnapshot().profiles.length === 0) {
        showOnboarding = true;
      }
    });
    void queueState.loadJobs();
    queueState.startPolling();

    void getCurrentWindow()
      .onCloseRequested((event) => {
        const running = $queueState.jobs.some((job) => job.status === "running");
        if (!running) {
          return;
        }
        const shouldQuit = window.confirm(
          "An import is in progress. Quit now and cancel the running import?",
        );
        if (!shouldQuit) {
          event.preventDefault();
        }
      })
      .then((fn) => {
        unlistenClose = fn;
      });

    return () => {
      queueState.stopPolling();
      if (unlistenClose) {
        unlistenClose();
      }
    };
  });

</script>

<AppLayout>
  {#snippet profile()}
    <ProfileSelector onManage={() => (showManager = !showManager)} />
  {/snippet}

  {#snippet actions()}
    <ThemeToggle />
  {/snippet}

  <AutoImportBanner />

  <div class="grid grid-cols-1 items-start gap-5 lg:grid-cols-2">
    <div class="flex flex-col gap-5">
      <SourcePicker />
      <ImportOptions />
    </div>
    <div class="flex flex-col gap-5">
      <AlbumSelector />
      <Tabs value="queue">
        <TabsList>
          <TabsTrigger value="queue">
            <ListChecks class="size-4" /> Queue
          </TabsTrigger>
          <TabsTrigger value="history">
            <History class="size-4" /> History
          </TabsTrigger>
        </TabsList>
        <TabsContent value="queue">
          <ImportQueue />
        </TabsContent>
        <TabsContent value="history">
          <HistoryPanel />
        </TabsContent>
      </Tabs>
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
        <Button size="sm" onclick={startImport}>
          <Play class="size-4" /> Start Import
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
    <ProfileManager onDone={() => (showManager = false)} />
  </DialogContent>
</Dialog>

<LogViewer bind:open={showLogs} />

<PreviewDialog />

{#if showOnboarding}
  <OnboardingOverlay onDone={() => (showOnboarding = false)} />
{/if}

<ErrorToast />
