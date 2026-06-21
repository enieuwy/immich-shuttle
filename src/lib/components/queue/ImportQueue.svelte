<script lang="ts">
  import { onMount } from "svelte";
  import { ListChecks, Inbox, X, AlertTriangle } from "@lucide/svelte";

  import { queueState } from "$lib/state/queue";
  import { Card, CardHeader, CardTitle, CardContent } from "$lib/components/ui/card";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";
  import { Progress } from "$lib/components/ui/progress";

  let actionError = $state("");

  onMount(() => {
    void queueState.loadJobs();
    queueState.startPolling();
    return () => queueState.stopPolling();
  });

  async function cancelJob(jobId: string) {
    actionError = "";
    try {
      await queueState.cancelImport(jobId);
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    }
  }

  function statusDotClass(status: string) {
    switch (status) {
      case "running": return "bg-primary animate-pulse";
      case "completed": return "bg-emerald-500";
      case "failed": return "bg-destructive";
      case "pending": return "bg-muted-foreground";
      default: return "bg-muted";
    }
  }

  function statusBadgeVariant(status: string): "default" | "outline" | "destructive" | "secondary" {
    switch (status) {
      case "running": return "default";
      case "completed": return "outline";
      case "failed": return "destructive";
      default: return "secondary";
    }
  }
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <ListChecks class="h-4 w-4 text-muted-foreground" aria-hidden="true" />
    <CardTitle class="text-sm font-semibold">Import queue</CardTitle>
    {#if $queueState.jobs.length > 0}
      <Badge variant="secondary" class="ml-auto tabular-nums">{$queueState.jobs.length}</Badge>
    {/if}
  </CardHeader>

  <CardContent class="flex flex-col gap-3">
    {#if actionError}
      <p class="text-xs text-destructive">{actionError}</p>
    {/if}

    {#if $queueState.loading && $queueState.jobs.length === 0}
      <p class="py-2 text-sm text-muted-foreground">Loading jobs…</p>
    {:else if $queueState.jobs.length === 0}
      <div class="flex flex-col items-center gap-2 py-10 text-center">
        <Inbox class="h-10 w-10 text-muted-foreground/60" aria-hidden="true" />
        <p class="text-sm text-muted-foreground">No imports yet</p>
        <p class="text-xs text-muted-foreground/70">
          Pick a source and start an import to see progress here.
        </p>
      </div>
    {:else}
      <div class="flex flex-col gap-2">
        {#each $queueState.jobs as job (job.id)}
          {@const pct = job.progress.total
            ? Math.round((job.progress.uploaded / job.progress.total) * 100)
            : 0}
          <div class="flex flex-col gap-2 rounded-lg border border-border bg-card p-3">
            <div class="flex items-center gap-2">
              <span
                class="h-2 w-2 shrink-0 rounded-full {statusDotClass(job.status)}"
                aria-hidden="true"
              ></span>
              <span class="font-mono text-xs text-foreground">{job.id.slice(0, 8)}</span>
              <Badge
                variant={statusBadgeVariant(job.status)}
                class={"capitalize" +
                  (job.status === "completed" ? " text-emerald-600 dark:text-emerald-400" : "")}
              >
                {job.status}
              </Badge>
              {#if job.status === "running"}
                <Button variant="outline" size="sm" class="ml-auto" onclick={() => cancelJob(job.id)}>
                  <X class="h-4 w-4" /> Cancel
                </Button>
              {/if}
            </div>

            {#if job.status === "running" || job.status === "pending"}
              <div class="flex items-center gap-2">
                <Progress value={pct} class="h-2 flex-1" />
                <span class="w-9 text-right text-xs tabular-nums text-muted-foreground">{pct}%</span>
              </div>
            {/if}

            <div class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
              <span class="tabular-nums">Uploaded {job.progress.uploaded}/{job.progress.total}</span>
              <span aria-hidden="true" class="text-muted-foreground/50">·</span>
              <span class="tabular-nums">Duplicates {job.progress.duplicates}</span>
              <span aria-hidden="true" class="text-muted-foreground/50">·</span>
              <span class={"tabular-nums" + (job.progress.errors > 0 ? " text-destructive" : "")}>
                Errors {job.progress.errors}
              </span>
            </div>

            {#if job.awaiting_wipe_confirmation}
              <div class="flex items-center gap-1.5 text-xs text-amber-600 dark:text-amber-400">
                <AlertTriangle class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
                <span>Awaiting wipe confirmation for {job.pending_wipe_count} files.</span>
              </div>
            {/if}

            {#if job.summary}
              <p class="text-xs text-emerald-600 dark:text-emerald-400">{job.summary}</p>
            {/if}

            {#if job.error}
              <p class="text-xs text-destructive">{job.error}</p>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </CardContent>
</Card>
