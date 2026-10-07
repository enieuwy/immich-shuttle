<script lang="ts">
  import { Button } from "$lib/components/ui/button";
  import { checkForUpdates, openProjectReleases, type UpdateStatus } from "$lib/insights-api";

  let checking = $state(false);
  let result = $state<UpdateStatus | null>(null);
  let error = $state("");

  async function check() {
    if (checking) return;
    checking = true;
    error = "";
    result = null;
    try {
      result = await checkForUpdates();
    } catch (reason) {
      error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      checking = false;
    }
  }

  async function openReleases() {
    try {
      await openProjectReleases();
    } catch (reason) {
      error = reason instanceof Error ? reason.message : String(reason);
    }
  }
</script>

<section class="flex flex-col gap-2 rounded-lg border border-border p-3" aria-label="Application updates">
  <div class="flex flex-wrap items-center justify-between gap-2">
    <div>
      <h3 class="text-sm font-medium">Application updates</h3>
      <p class="text-xs text-muted-foreground">Check GitHub only when you click. This app does not install updates.</p>
    </div>
    <Button variant="outline" size="sm" disabled={checking} onclick={check}>
      {checking ? "Checking…" : "Check for updates"}
    </Button>
  </div>
  <div aria-live="polite" class="text-sm">
    {#if result}
      <div class="flex flex-wrap items-center gap-2">
        <p>
          {#if result.update_available}
            Version {result.latest_version} is available. You have {result.current_version}.
          {:else}
            No newer stable release. You have {result.current_version}; latest is {result.latest_version}.
          {/if}
        </p>
        <Button variant="outline" size="sm" onclick={openReleases}>Open project releases</Button>
        <Button variant="ghost" size="sm" aria-label="Dismiss update result" onclick={() => result = null}>Dismiss</Button>
      </div>
    {/if}
    {#if error}
      <p class="text-destructive" role="alert">{error}</p>
    {/if}
  </div>
</section>
