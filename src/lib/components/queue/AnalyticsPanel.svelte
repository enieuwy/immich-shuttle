<script lang="ts">
  import { historyState } from "$lib/state/history";
  import { profilesState } from "$lib/state/profiles";
  import { historyAnalytics } from "$lib/history-analytics";
  import { Button } from "$lib/components/ui/button";

  let selectedProfile = $state("");
  let interval = $state<"day" | "week">("day");
  const allProfiles = $derived(historyAnalytics($historyState.records, $profilesState.profiles).perProfile);
  const analytics = $derived(historyAnalytics($historyState.records, $profilesState.profiles, selectedProfile || null, interval));
  const largest = $derived(Math.max(1, ...analytics.buckets.map((b) => b.uploaded)));
  const number = new Intl.NumberFormat();
  const date = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", timeZone: "UTC" });
</script>

<section class="flex flex-col gap-4" aria-label="Import history analytics">
  <div class="flex flex-wrap items-end gap-3">
    <label class="flex flex-col gap-1 text-sm">
      <span>Profile filter</span>
      <select class="rounded-md border border-border bg-background px-2 py-1" bind:value={selectedProfile}>
        <option value="">All profiles</option>
        {#each allProfiles as profile (profile.profileId)}
          <option value={profile.profileId}>{profile.name}</option>
        {/each}
      </select>
    </label>
    <label class="flex flex-col gap-1 text-sm">
      <span>Trend interval</span>
      <select class="rounded-md border border-border bg-background px-2 py-1" bind:value={interval}>
        <option value="day">Daily · last 30 days</option>
        <option value="week">Weekly · last 12 weeks</option>
      </select>
    </label>
    <Button variant="ghost" size="sm" disabled={$historyState.loading} onclick={() => historyState.loadHistory()}>Refresh history</Button>
  </div>
  <p class="text-xs text-muted-foreground">
    Lifetime totals cover retained history only. Clearing history also clears these totals.
    Stored upload counts can include partial runs. History does not record bytes; storage totals are unavailable.
  </p>
  {#if $historyState.error}
    <p class="text-sm text-destructive" role="alert">Could not load history: {$historyState.error}</p>
  {:else if $historyState.loading && $historyState.records.length === 0}
    <p class="text-sm text-muted-foreground">Loading history…</p>
  {:else}
    <dl class="grid grid-cols-2 gap-2 sm:grid-cols-4" aria-label="Retained lifetime totals">
      {#each [
        ["Runs", analytics.totals.runs], ["Uploaded", analytics.totals.uploaded],
        ["Duplicates", analytics.totals.duplicates], ["Errors", analytics.totals.errors],
      ] as [label, value]}
        <div class="rounded-lg border border-border p-3">
          <dt class="text-xs text-muted-foreground">{label}</dt>
          <dd class="text-xl font-semibold tabular-nums">{number.format(Number(value))}</dd>
        </div>
      {/each}
    </dl>
    <p class="text-xs text-muted-foreground">
      {analytics.totals.completed} completed · {analytics.totals.failed} failed · {analytics.totals.cancelled} cancelled
      · {analytics.totals.unknown} unknown · {analytics.totals.incomplete} incomplete.
      {number.format(analytics.totals.total)} files reported across all runs.
    </p>
    {#if analytics.totals.runs === 0}
      <p class="text-sm text-muted-foreground">No stored imports match this filter.</p>
    {/if}
    {#if analytics.skippedCounters > 0 || analytics.undatedRuns > 0}
      <p class="text-xs text-destructive">Excluded {analytics.skippedCounters} invalid counters. {analytics.undatedRuns} runs have no valid trend date.</p>
    {/if}
    <section aria-label="Upload trend" class="flex flex-col gap-2">
      <h3 class="text-sm font-medium">Uploads over time</h3>
      <p class="text-xs text-muted-foreground">UTC dates. Weekly buckets start on Monday. Totals include older records outside this chart.</p>
      <div class="flex h-28 items-end gap-1 rounded-md bg-muted/40 px-2 pt-2" aria-hidden="true">
        {#each analytics.buckets as bucket (bucket.start)}
          <div
            class="min-w-0 flex-1 rounded-t bg-primary"
            style:height={`${bucket.uploaded / largest * 100}%`}
            title={`${date.format(bucket.start)}: ${bucket.uploaded} uploaded, ${bucket.errors} errors`}
          ></div>
        {/each}
      </div>
      <div class="flex justify-between text-xs text-muted-foreground">
        <span>{date.format(analytics.buckets[0].start)}</span>
        <span>{date.format(analytics.buckets[analytics.buckets.length - 1].start)}</span>
      </div>
      <details class="text-xs">
        <summary class="cursor-pointer rounded-sm py-1 focus-visible:ring-2 focus-visible:ring-ring">View trend values</summary>
        <div class="max-h-40 overflow-auto">
          <table class="w-full text-left tabular-nums">
            <caption class="sr-only">Upload trend values in UTC</caption>
            <thead><tr><th scope="col">Starting</th><th scope="col">Runs</th><th scope="col">Uploaded</th><th scope="col">Duplicates</th><th scope="col">Errors</th></tr></thead>
            <tbody>
              {#each analytics.buckets as bucket (bucket.start)}
                <tr><th scope="row" class="font-normal">{new Date(bucket.start).toISOString().slice(0, 10)}</th><td>{bucket.runs}</td><td>{bucket.uploaded}</td><td>{bucket.duplicates}</td><td>{bucket.errors}</td></tr>
              {/each}
            </tbody>
          </table>
        </div>
      </details>
    </section>
    <div class="overflow-x-auto">
      <table class="w-full text-left text-xs tabular-nums">
        <caption class="mb-2 text-left text-sm font-medium">Per-profile totals</caption>
        <thead><tr class="border-b border-border"><th scope="col" class="py-2">Profile</th><th scope="col">Runs</th><th scope="col">Uploaded</th><th scope="col">Duplicates</th><th scope="col">Errors</th></tr></thead>
        <tbody>
          {#each analytics.perProfile as profile (profile.profileId)}
            <tr class="border-b border-border/50"><th scope="row" class="py-2 pr-2 font-normal" title={profile.profileId}>{profile.name}</th><td>{number.format(profile.runs)}</td><td>{number.format(profile.uploaded)}</td><td>{number.format(profile.duplicates)}</td><td>{number.format(profile.errors)}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>
