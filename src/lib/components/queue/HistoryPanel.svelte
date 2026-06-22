<script lang="ts">
  import { onMount } from "svelte";
  import { History, Clock, Trash2 } from "@lucide/svelte";

  import { historyState } from "$lib/state/history";
  import type { ImportRecord } from "$lib/types";
  import { Card, CardHeader, CardTitle, CardContent } from "$lib/components/ui/card";
  import { Badge } from "$lib/components/ui/badge";
  import { Button } from "$lib/components/ui/button";
  import { ScrollArea } from "$lib/components/ui/scroll-area";

  onMount(() => {
    void historyState.loadHistory();
  });

  function statusDotClass(status: ImportRecord["status"]) {
    switch (status) {
      case "completed": return "bg-emerald-500";
      case "failed": return "bg-destructive";
      case "cancelled": return "bg-muted-foreground";
      default: return "bg-muted";
    }
  }

  function statusBadgeVariant(
    status: ImportRecord["status"],
  ): "default" | "outline" | "destructive" | "secondary" {
    switch (status) {
      case "completed": return "outline";
      case "failed": return "destructive";
      default: return "secondary";
    }
  }

  function statusBadgeClass(status: ImportRecord["status"]) {
    return status === "completed" ? " text-emerald-600 dark:text-emerald-400" : "";
  }

  function basename(path: string) {
    const parts = path.split(/[\\/]/).filter(Boolean);
    return parts[parts.length - 1] ?? path;
  }

  function sourceLabel(paths: string[]) {
    if (paths.length === 0) return "—";
    const base = basename(paths[0]);
    return paths.length > 1 ? `${base} +${paths.length - 1} more` : base;
  }

  function relativeTime(ms: number) {
    const sec = Math.round((Date.now() - ms) / 1000);
    if (sec < 45) return "just now";
    const min = Math.round(sec / 60);
    if (min < 60) return `${min}m ago`;
    const hr = Math.round(min / 60);
    if (hr < 24) return `${hr}h ago`;
    const day = Math.round(hr / 24);
    if (day < 7) return `${day}d ago`;
    const wk = Math.round(day / 7);
    if (wk < 5) return `${wk}w ago`;
    return new Date(ms).toLocaleDateString();
  }
</script>

<Card>
  <CardHeader class="flex flex-row items-center gap-2">
    <History class="h-4 w-4 text-muted-foreground" aria-hidden="true" />
    <CardTitle class="text-sm font-semibold">Import history</CardTitle>
    <div class="ml-auto flex items-center gap-2">
      {#if $historyState.records.length > 0}
        <Button
          variant="ghost"
          size="sm"
          aria-label="Clear import history"
          onclick={() => {
            void historyState.clearHistory();
          }}
        >
          <Trash2 class="h-4 w-4" /> Clear history
        </Button>
        <Badge variant="secondary" class="tabular-nums">{$historyState.records.length}</Badge>
      {/if}
    </div>
  </CardHeader>

  <CardContent class="flex flex-col gap-3">
    {#if $historyState.loading && $historyState.records.length === 0}
      <p class="py-2 text-sm text-muted-foreground">Loading history…</p>
    {:else if $historyState.records.length === 0}
      <div class="flex flex-col items-center gap-2 py-10 text-center">
        <Clock class="h-10 w-10 text-muted-foreground/60" aria-hidden="true" />
        <p class="text-sm text-muted-foreground">No past imports</p>
        <p class="text-xs text-muted-foreground/70">
          Completed imports will be recorded here so you can look back later.
        </p>
      </div>
    {:else}
      <ScrollArea class="max-h-[26rem]">
        <ul class="flex flex-col gap-2 pr-3">
          {#each $historyState.records as record (record.id)}
            <li class="flex flex-col gap-2 rounded-lg border border-border bg-card p-3">
              <div class="flex items-center gap-2">
                <span
                  class="h-2 w-2 shrink-0 rounded-full {statusDotClass(record.status)}"
                  aria-hidden="true"
                ></span>
                <Badge
                  variant={statusBadgeVariant(record.status)}
                  class={"capitalize" + statusBadgeClass(record.status)}
                >
                  {record.status}
                </Badge>
                <time
                  class="ml-auto flex items-center gap-1.5 text-xs text-muted-foreground"
                  datetime={new Date(record.finished_at).toISOString()}
                  title={new Date(record.finished_at).toLocaleString()}
                >
                  <span class="tabular-nums">{new Date(record.finished_at).toLocaleString()}</span>
                  <span class="text-muted-foreground/60">· {relativeTime(record.finished_at)}</span>
                </time>
              </div>

              <p
                class="truncate font-mono text-xs text-muted-foreground"
                title={record.source_paths.join(", ")}
              >
                {sourceLabel(record.source_paths)}
              </p>

              <div class="flex flex-wrap items-center gap-1.5">
                <span class="rounded-md bg-muted px-1.5 py-0.5 text-xs tabular-nums text-muted-foreground">
                  {record.uploaded}/{record.total} uploaded
                </span>
                <span class="rounded-md bg-muted px-1.5 py-0.5 text-xs tabular-nums text-muted-foreground">
                  {record.duplicates} dup
                </span>
                <span
                  class={"rounded-md bg-muted px-1.5 py-0.5 text-xs tabular-nums " +
                    (record.errors > 0 ? "text-destructive" : "text-muted-foreground")}
                >
                  {record.errors} err
                </span>
                {#if record.album_ids.length > 0}
                  <span class="rounded-md bg-muted px-1.5 py-0.5 text-xs tabular-nums text-muted-foreground">
                    {record.album_ids.length} albums
                  </span>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
      </ScrollArea>
    {/if}
  </CardContent>
</Card>
