<script lang="ts">
  import { onMount } from "svelte";
  import { ListChecks, Inbox, X, AlertTriangle, FileWarning, RotateCcw, Trash2 } from "@lucide/svelte";

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

  function isFinished(status: string) {
    return status === "completed" || status === "failed" || status === "cancelled";
  }

  function fmtEta(s: number) {
    return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
  }

  function fileLabel(file: string): string {
    // immich-go logs the file as "<fs>:<name>"; show the last path segment.
    const afterColon = file.includes(":") ? file.slice(file.lastIndexOf(":") + 1) : file;
    const seg = afterColon.split(/[\\/]/).pop();
    return seg && seg.length > 0 ? seg : file;
  }
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <ListChecks class="h-4 w-4 text-muted-foreground" aria-hidden="true" />
    <CardTitle class="text-sm font-semibold">Import queue</CardTitle>
    <div class="ml-auto flex items-center gap-2">
      {#if $queueState.jobs.some((job) => isFinished(job.status))}
        <Button
          variant="ghost"
          size="sm"
          aria-label="Clear finished imports"
          onclick={() => {
            void queueState.clearFinished();
          }}
        >
          <Trash2 class="h-4 w-4" /> Clear finished
        </Button>
      {/if}
      {#if $queueState.jobs.length > 0}
        <Badge variant="secondary" class="tabular-nums">{$queueState.jobs.length}</Badge>
      {/if}
    </div>
  </CardHeader>

  <CardContent class="flex flex-col gap-3">
    {#if actionError}
      <p class="text-xs text-destructive">{actionError}</p>
    {/if}

    {#if $queueState.loading && $queueState.jobs.length === 0}
      <p class="py-2 text-sm text-muted-foreground">Loading jobs…</p>
    {:else if $queueState.jobs.length === 0}
      <div class="flex flex-col items-center gap-2 py-6 text-center">
        <Inbox class="h-8 w-8 text-muted-foreground/60" aria-hidden="true" />
        <p class="text-sm text-muted-foreground">No imports yet</p>
        <p class="text-xs text-muted-foreground/70">
          Pick a source and start an import to see progress here.
        </p>
      </div>
    {:else}
      <div class="flex flex-col gap-2" role="status" aria-live="polite">
        {#each $queueState.jobs as job (job.id)}
          {@const pct = job.progress.total
            ? Math.round((job.progress.uploaded / job.progress.total) * 100)
            : 0}
          {@const rate = $queueState.rates[job.id]}
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
              {#if job.status === "failed"}
                <Button
                  variant="outline"
                  size="sm"
                  class="ml-auto"
                  aria-label="Retry import"
                  onclick={() => {
                    void queueState.retry(job.id);
                  }}
                >
                  <RotateCcw class="h-4 w-4" /> Retry
                </Button>
              {/if}
              {#if isFinished(job.status)}
                <Button
                  variant="ghost"
                  size="icon-sm"
                  class={job.status === "failed" ? "" : "ml-auto"}
                  aria-label="Dismiss job"
                  onclick={() => {
                    void queueState.dismiss(job.id);
                  }}
                >
                  <X class="h-4 w-4" />
                </Button>
              {/if}
            </div>

            {#if job.status === "running" || job.status === "pending"}
              <div class="flex items-center gap-2">
                <Progress value={pct} class="h-2 flex-1" />
                {#if rate && rate.itemsPerSec > 0}
                  <span class="text-xs tabular-nums text-muted-foreground">
                    ~{Math.round(rate.itemsPerSec)}/s{#if rate.etaSeconds != null} · ETA {fmtEta(rate.etaSeconds)}{/if}
                  </span>
                {/if}
                <span class="w-9 text-right text-xs tabular-nums text-muted-foreground">{pct}%</span>
              </div>
            {/if}

            {#if job.status === "running" && $queueState.currentFiles[job.id]}
              <p
                class="truncate text-xs text-muted-foreground"
                title={$queueState.currentFiles[job.id]}
              >
                Importing {$queueState.currentFiles[job.id]}
              </p>
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
              <div
                class="flex flex-col gap-2 rounded-md border border-amber-600/40 bg-amber-600/5 p-2.5 dark:border-amber-400/40"
              >
                <div class="flex items-center gap-1.5 text-xs text-amber-600 dark:text-amber-400">
                  <AlertTriangle class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
                  <span>
                    Awaiting wipe confirmation for {job.pending_wipe_count} files. Each file is checked against the server before deletion. This cannot be undone.
                  </span>
                </div>
                <div class="flex items-center gap-2">
                  <Button
                    variant="destructive"
                    size="sm"
                    onclick={() => {
                      void queueState.confirmWipe(job.id, true);
                    }}
                  >
                    Verify &amp; delete {job.pending_wipe_count} files
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onclick={() => {
                      void queueState.confirmWipe(job.id, false);
                    }}
                  >
                    Keep files
                  </Button>
                </div>
              </div>
            {/if}

            {#if job.summary}
              <p class="text-xs text-emerald-600 dark:text-emerald-400">{job.summary}</p>
            {/if}

            {#if job.error}
              <p class="text-xs text-destructive">{job.error}</p>
            {/if}

            {#if job.file_errors.length > 0}
              <div class="flex flex-col gap-1.5 rounded-md border border-destructive/30 bg-destructive/5 p-2.5">
                <div class="flex items-center gap-1.5 text-xs font-medium text-destructive">
                  <FileWarning class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
                  <span>{job.file_errors.length} file{job.file_errors.length === 1 ? "" : "s"} failed</span>
                </div>
                <div class="flex max-h-40 flex-col gap-1 overflow-y-auto">
                  {#each job.file_errors as fe}
                    <div class="flex flex-col rounded bg-background/60 px-2 py-1">
                      <span class="truncate font-mono text-xs text-foreground" title={fe.file}>
                        {fileLabel(fe.file)}
                      </span>
                      <span class="truncate text-xs text-muted-foreground" title={fe.reason}>
                        {fe.reason}
                      </span>
                    </div>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </CardContent>
</Card>
