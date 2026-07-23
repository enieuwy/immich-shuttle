import { get, writable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  historySourceLastImport,
  importCancel,
  importClearFinished,
  importConfirmWipe,
  importDismiss,
  importListJobs,
  importRetry,
  importStart,
} from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { ImportJob, ImportOrganization } from "$lib/types";

import { importOptionsState, isDateRangeInvalid, toImmichDateRange } from "$lib/state/import-options";
import { albumsState } from "$lib/state/albums";
import { activeProfile, profilesState } from "$lib/state/profiles";
import { sourceState } from "$lib/state/source";

type QueueState = {
  jobs: ImportJob[];
  loading: boolean;
  error: string | null;
  rates: Record<string, { itemsPerSec: number; etaSeconds: number | null }>;
  currentFiles: Record<string, string>;
};

const state = writable<QueueState>({
  jobs: [],
  loading: false,
  error: null,
  rates: {},
  currentFiles: {},
});

let pollTimer: ReturnType<typeof setInterval> | null = null;
let progressUnlisten: UnlistenFn | null = null;
// In-flight `listen()` registration. Tracked so stopPolling can coordinate with
// a registration that has not resolved yet (the resolved handle would otherwise
// escape teardown and leak the listener across mount/unmount cycles).
let progressPending: Promise<UnlistenFn> | null = null;

type ImportProgressEvent = {
  job_id: string;
  progress?: ImportJob["progress"];
  parsed_progress?: ImportJob["progress"];
  current_file?: string | null;
};


const terminalStatuses: Partial<Record<ImportJob["status"], true>> = {
  completed: true,
  failed: true,
  cancelled: true,
};

const firstSamples = new Map<string, { time: number; uploaded: number }>();

type RateSample = { time: number; uploaded: number };
type RateInfo = { itemsPerSec: number; etaSeconds: number | null };

/**
 * Compute per-job upload rate and ETA from the first observed sample. `now` and
 * `samples` are injectable so the timing-dependent math can be unit-tested
 * deterministically; production callers use the module clock and shared map.
 */
export function recomputeRates(
  jobs: ImportJob[],
  now: () => number = Date.now,
  samples: Map<string, RateSample> = firstSamples,
): Record<string, RateInfo> {
  const rates: Record<string, RateInfo> = {};
  const present = new Set<string>();
  for (const job of jobs) {
    present.add(job.id);
    if (job.status !== "running") {
      // Non-running jobs must not retain a stale first sample, or a later
      // resume would compute the rate from a pre-pause baseline.
      samples.delete(job.id);
      continue;
    }
    let sample = samples.get(job.id);
    if (!sample) {
      sample = { time: now(), uploaded: job.progress.uploaded };
      samples.set(job.id, sample);
    }
    const elapsed = (now() - sample.time) / 1000;
    const delta = job.progress.uploaded - sample.uploaded;
    const itemsPerSec = elapsed > 0 && delta > 0 ? delta / elapsed : 0;
    const remaining = Math.max(0, job.progress.total - job.progress.uploaded);
    const etaSeconds = itemsPerSec > 0 ? Math.round(remaining / itemsPerSec) : null;
    rates[job.id] = { itemsPerSec, etaSeconds };
  }
  // Drop samples for jobs that disappeared from the queue.
  for (const id of samples.keys()) {
    if (!present.has(id)) {
      samples.delete(id);
    }
  }
  return rates;
}

async function refreshJobs() {
  try {
    const polled = await importListJobs();
    const runningIds = new Set(polled.filter((j) => j.status === "running").map((j) => j.id));
    state.update((s) => {
      const prevById = new Map(s.jobs.map((j) => [j.id, j]));
      // The backend's stored job progress is only refreshed at import start and
      // end; live per-file counts arrive via the "import-progress" event stream
      // between polls. For a still-running job, take the field-wise max of the
      // polled and current progress (the run log is append-only, so counts only
      // grow) so the 2s poll can't reset the bar/ETA to the stale start value.
      const jobs = polled.map((job) => {
        const prev = prevById.get(job.id);
        if (job.status !== "running" || !prev) return job;
        return {
          ...job,
          progress: {
            total: Math.max(prev.progress.total, job.progress.total),
            uploaded: Math.max(prev.progress.uploaded, job.progress.uploaded),
            duplicates: Math.max(prev.progress.duplicates, job.progress.duplicates),
            errors: Math.max(prev.progress.errors, job.progress.errors),
          },
        };
      });
      const rates = recomputeRates(jobs);
      return {
        ...s,
        jobs,
        rates,
        currentFiles: Object.fromEntries(
          Object.entries(s.currentFiles).filter(([id]) => runningIds.has(id)),
        ),
        error: null,
      };
    });
  } catch (error) {
    errorsState.addError("Could not refresh import queue.");
    state.update((s) => ({ ...s, error: error instanceof Error ? error.message : String(error) }));
  }
}

export const queueState = {
  subscribe: state.subscribe,
  async loadJobs() {
    state.update((s) => ({ ...s, loading: true }));
    await refreshJobs();
    state.update((s) => ({ ...s, loading: false }));
  },
  startPolling() {
    if (pollTimer) {
      return;
    }
    if (!progressUnlisten && !progressPending) {
      progressPending = listen<ImportProgressEvent>("import-progress", (event) => {
        const payload = event.payload;
        if (!payload?.job_id) {
          return;
        }
        const progress = payload.parsed_progress ?? payload.progress;
        if (!progress) {
          return;
        }
        state.update((s) => {
          const job = s.jobs.find((entry) => entry.id === payload.job_id);
          if (!job || terminalStatuses[job.status]) {
            return s;
          }
          const jobs: ImportJob[] = s.jobs.map((entry) =>
            entry.id === payload.job_id ? { ...entry, status: "running", progress } : entry,
          );
          const rates = recomputeRates(jobs);
          const currentFiles = payload.current_file
            ? { ...s.currentFiles, [payload.job_id]: payload.current_file }
            : s.currentFiles;
          return { ...s, jobs, rates, currentFiles };
        });
      });
      void progressPending.then((unlisten) => {
        progressPending = null;
        // If polling was stopped while this registration was in flight, tear the
        // listener down immediately rather than leaking it; otherwise retain the
        // handle so stopPolling can unlisten later.
        if (pollTimer) {
          progressUnlisten = unlisten;
        } else {
          unlisten();
        }
      });
    }
    pollTimer = setInterval(() => {
      void refreshJobs();
    }, 2000);
  },
  stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
    if (progressUnlisten) {
      progressUnlisten();
      progressUnlisten = null;
    }
  },
  async startImport(overrides?: {
    sourcePaths?: string[];
    keepFiles?: boolean;
    albumIds?: string[];
    selectFiles?: string[];
    /** Import under a specific profile instead of the active one (device rules). */
    profileId?: string;
    /** Use this album name directly, bypassing albumIds -> name resolution. */
    intoAlbum?: string | null;
    stackRawJpeg?: boolean;
    stackBurst?: boolean;
    organization?: ImportOrganization;
  }) {
    const source = get(sourceState);
    const options = get(importOptionsState);
    const albums = get(albumsState);

    const profile = overrides?.profileId
      ? (get(profilesState).profiles.find((p) => p.id === overrides.profileId) ?? null)
      : get(activeProfile);
    if (!profile) {
      throw new Error("Select a profile before starting import.");
    }
    const sourcePaths = overrides?.sourcePaths ?? source.selectedPaths;
    if (sourcePaths.length === 0) {
      throw new Error("Select a source before starting import.");
    }
    if (isDateRangeInvalid(options.dateFrom, options.dateTo)) {
      throw new Error("The start date must be on or before the end date.");
    }


    // immich-go assigns albums by name (--into-album), single album per run. A
    // device rule can supply the name directly; otherwise resolve it from the
    // first selected album id.
    const albumIds = overrides?.albumIds ?? albums.selectedAlbumIds;
    const intoAlbum =
      overrides?.intoAlbum !== undefined
        ? overrides.intoAlbum
        : albumIds.length > 0
          ? (albums.availableAlbums.find((a) => a.id === albumIds[0])?.album_name ?? null)
          : null;

    // Explicit From/To range wins. Otherwise, "only new since last import"
    // derives a capture-date floor from this source's stored last-import time.
    // immich-go's --date-range needs both bounds, so pair the floor with a
    // far-future upper bound (open-ended "floor," is rejected).
    let dateRange = toImmichDateRange(options.dateFrom, options.dateTo);
    if (!dateRange && options.onlyNewSinceLastImport) {
      const lastMs = await historySourceLastImport(profile.id, sourcePaths);
      if (lastMs != null) {
        // Format in the local calendar zone: immich-go parses --date-range in
        // local time, so a UTC date could land a day off and skip newer files.
        const d = new Date(lastMs);
        const floor = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(
          d.getDate(),
        ).padStart(2, "0")}`;
        dateRange = `${floor},9999-12-31`;
      }
    }

    await importStart({
      profile_id: profile.id,
      source_paths: sourcePaths,
      album_ids: albumIds,
      keep_files: overrides?.keepFiles ?? options.keepFiles,
      stack_raw_jpeg: overrides?.stackRawJpeg ?? options.stackRawJpeg,
      stack_burst: overrides?.stackBurst ?? options.stackBurst,
      date_range: dateRange,
      concurrent_tasks: options.concurrentTasks,
      select_files: overrides?.selectFiles ?? null,
      into_album: intoAlbum,
      organization: overrides?.organization ?? options.organization,
      on_errors: options.keepGoingOnErrors ? "continue" : null,
      overwrite: options.overwrite,
      tags: options.tags,
      session_tag: options.sessionTag,
      include_type:
        options.mediaType === "image" ? "IMAGE" : options.mediaType === "video" ? "VIDEO" : null,
      include_extensions: options.includeExtensions,
      exclude_extensions: options.excludeExtensions,
    });
    await refreshJobs();
  },
  async cancelImport(jobId: string) {
    try {
      await importCancel(jobId);
      await refreshJobs();
    } catch (error) {
      errorsState.addError("Could not cancel import.");
      throw error;
    }
  },
  async retry(jobId: string) {
    try {
      await importRetry(jobId);
      await refreshJobs();
    } catch (error) {
      errorsState.addError("Could not retry import.");
      throw error;
    }
  },
  async dismiss(jobId: string) {
    try {
      const jobs = await importDismiss(jobId);
      state.update((s) => ({ ...s, jobs }));
    } catch (error) {
      errorsState.addError("Could not dismiss job.");
      throw error;
    }
  },
  async clearFinished() {
    try {
      const jobs = await importClearFinished();
      state.update((s) => ({ ...s, jobs }));
    } catch (error) {
      errorsState.addError("Could not clear finished jobs.");
      throw error;
    }
  },
  async confirmWipe(jobId: string, proceed: boolean) {
    try {
      await importConfirmWipe(jobId, proceed);
      await refreshJobs();
    } catch (error) {
      errorsState.addError("Could not complete wipe confirmation.");
      state.update((s) => ({ ...s, error: error instanceof Error ? error.message : String(error) }));
      throw error;
    }
  },
};
