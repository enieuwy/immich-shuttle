import { get, writable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

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
import { historyState } from "$lib/state/history";
import type { ImportJob, ImportOrganization } from "$lib/types";

import { importOptionsState, isDateRangeInvalid, toImmichDateRange } from "$lib/state/import-options";
import { albumsState } from "$lib/state/albums";
import { createGeneration } from "$lib/state/generation";
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
// Starts remain visible to shutdown until their admission and the following
// queue refresh settle, including starts that reject validation or IPC.
const pendingImportStarts = new Set<Promise<void>>();

type ImportProgressEvent = {
  job_id: string;
  progress: ImportJob["progress"];
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

// A denial may remain cached because the user must change it in system settings;
// a grant must be checked again so revoking notification permission takes effect
// without restarting the app.
let notifyPermission: boolean | null = null;

async function ensureNotifyPermission(): Promise<boolean> {
  if (notifyPermission === false) return false;
  let granted = await isPermissionGranted();
  if (!granted) {
    granted = (await requestPermission()) === "granted";
  }
  notifyPermission = granted ? null : false;
  return granted;
}

function notificationForJob(job: ImportJob): { title: string; body: string } | null {
  if (job.status === "completed") {
    return {
      title: "Import complete",
      body: `Uploaded ${job.progress.uploaded} of ${job.progress.total} file(s).`,
    };
  }
  if (job.status === "failed") {
    return {
      title: "Import failed",
      body: job.error ?? "The import did not finish. Check the logs for details.",
    };
  }
  return null;
}

// Jobs observed transitioning from a non-terminal state into completed/failed.
// Unseen jobs (initial hydration or app restart with old finished jobs) and
// cancellations are excluded. Pure so the transition logic is unit-testable.
export function selectNewlyTerminal(prev: ImportJob[], next: ImportJob[]): ImportJob[] {
  const prevStatus = new Map(prev.map((j) => [j.id, j.status]));
  return next.filter((job) => {
    const before = prevStatus.get(job.id);
    if (before === undefined || terminalStatuses[before]) return false;
    return job.status === "completed" || job.status === "failed";
  });
}

async function handleTerminalTransitions(prev: ImportJob[], next: ImportJob[]) {
  const newlyTerminal = selectNewlyTerminal(prev, next);
  if (newlyTerminal.length === 0) return;
  // The backend's stricter checkpoint eligibility inputs are not present on
  // ImportJob, so completed is the closest visible gate. A completed run that
  // did not earn a checkpoint only causes a harmless unchanged re-read.
  if (newlyTerminal.some((job) => job.status === "completed")) {
    historyState.noteImportRecorded();
  }
  try {
    if (!(await ensureNotifyPermission())) return;
    for (const job of newlyTerminal) {
      const notification = notificationForJob(job);
      if (notification) sendNotification(notification);
    }
  } catch {
    // Notifications are best-effort: a missing/denied permission backend or a
    // throwing send must not surface as an unhandled rejection or disturb the
    // already-refreshed queue state.
  }
}

// Guards refreshJobs commits against out-of-order IPC completion: the 2s poll
// interval fires `void refreshJobs()` without waiting for the previous call to
// settle, so a slow earlier poll can resolve after a faster later one. Without
// this, the earlier snapshot would win on completion order and regress the
// store — a completed/cancelled job flips back to running, live counts and
// current-file entries reappear, and handleTerminalTransitions repeats the
// already-applied completion side effects on the stale "correction".
const refreshes = createGeneration();

async function refreshJobs() {
  const isCurrent = refreshes.begin();
  try {
    const polled = await importListJobs();
    if (!isCurrent()) return;
    const runningIds = new Set(polled.filter((j) => j.status === "running").map((j) => j.id));
    // Capture the pre-update jobs synchronously (no await before state.update, so
    // no listener can interleave) to diff progress and detect terminal transitions.
    const prev = get(state).jobs;
    const prevById = new Map(prev.map((j) => [j.id, j]));
    // The backend's stored job progress is only refreshed at import start and
    // end; live per-file counts arrive via the "import-progress" event stream
    // between polls. For a still-running job, take the field-wise max of the
    // polled and current progress (the run log is append-only, so counts only
    // grow) so the 2s poll can't reset the bar/ETA to the stale start value.
    const jobs = polled.map((job) => {
      const previous = prevById.get(job.id);
      if (job.status !== "running" || !previous) return job;
      return {
        ...job,
        progress: {
          total: Math.max(previous.progress.total, job.progress.total),
          uploaded: Math.max(previous.progress.uploaded, job.progress.uploaded),
          duplicates: Math.max(previous.progress.duplicates, job.progress.duplicates),
          errors: Math.max(previous.progress.errors, job.progress.errors),
        },
      };
    });
    state.update((s) => {
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
    void handleTerminalTransitions(prev, jobs);
  } catch (error) {
    if (!isCurrent()) return;
    errorsState.addError("Could not refresh import queue.", "error", "queue-refresh");
    state.update((s) => ({ ...s, error: error instanceof Error ? error.message : String(error) }));
  }
}

export const queueState = {
  subscribe: state.subscribe,
  // Shutdown snapshots this set before its confirmation prompt; return a copy
  // so later starts cannot mutate the sequence's fixed pending-start list.
  pendingStarts() {
    return [...pendingImportStarts];
  },
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
        const progress = payload.progress;
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
  startImport(overrides?: {
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
    const pendingStart = (async () => {
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

      // Album state is scoped to a profile (loadedProfileId). Switching the
      // active profile and hitting Start before the albums store reloads must
      // not carry the old profile's selection across: an album id/name that
      // only exists on the previous server would otherwise silently create a
      // stray album there, or worse, upload into someone else's album. Gate on
      // loadedProfileId rather than depending on a sibling clear-on-switch to
      // have already run.
      const albumsUsable = albums.loadedProfileId === profile.id;
      // immich-go assigns albums by name (--into-album), single album per run. A
      // device rule can supply the name directly; otherwise resolve it from the
      // first selected album id.
      const albumIds = overrides?.albumIds ?? (albumsUsable ? albums.selectedAlbumIds : []);
      const intoAlbum =
        overrides?.intoAlbum !== undefined
          ? overrides.intoAlbum
          : albumIds.length > 0
            ? (albums.availableAlbums.find((a) => a.id === albumIds[0])?.album_name ?? null)
            : null;

      // An explicit preview selection IS the import: the user hand-picked exact
      // files, so no coarse filter may silently drop one. Type, date, include-
      // and exclude-extension filters therefore all apply only on the no-preview
      // (fast) path and to History replays, which clear the selection. Durable
      // excludes are hygiene for unattended scans, not a veto over a ticked file
      // — the preview grid never filters by extension, so a selection genuinely
      // can contain one.
      const selectFiles = overrides?.selectFiles ?? null;
      const hasSelection = !!selectFiles && selectFiles.length > 0;

      // Explicit From/To range wins. Otherwise, "only new since last import"
      // derives a capture-date floor from this source's stored last-import time.
      // immich-go's --date-range needs both bounds, so pair the floor with a
      // far-future upper bound (open-ended "floor," is rejected).
      let dateRange: string | null = null;
      if (!hasSelection) {
        dateRange = toImmichDateRange(options.dateFrom, options.dateTo);
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
        select_files: selectFiles,
        into_album: intoAlbum,
        organization: overrides?.organization ?? options.organization,
        on_errors: options.keepGoingOnErrors ? "continue" : null,
        overwrite: options.overwrite,
        tags: options.tags,
        session_tag: options.sessionTag,
        include_type: hasSelection
          ? null
          : options.mediaType === "image"
            ? "IMAGE"
            : options.mediaType === "video"
              ? "VIDEO"
              : null,
        include_extensions: hasSelection ? [] : options.includeExtensions,
        exclude_extensions: hasSelection ? [] : options.excludeExtensions,
      });
      await refreshJobs();
    })();
    pendingImportStarts.add(pendingStart);
    void pendingStart
      .finally(() => {
        pendingImportStarts.delete(pendingStart);
      })
      .catch(() => {
        // The caller still receives the original rejection; consume only the
        // cleanup branch's mirrored rejection to keep shutdown best-effort.
      });
    return pendingStart;
  },
  // cancelImport/retry/dismiss/clearFinished/confirmWipe below already report
  // their failure once via errorsState.addError before reaching here — the
  // user has been told. Re-throwing on top would leave every fire-and-forget
  // caller (`void queueState.retry(...)`, etc. — there is no global
  // unhandledrejection handler in the webview) with an unhandled rejection for
  // a failure already surfaced, which tells the user nothing new and only
  // pollutes the console. startImport is the exception: its throws are
  // validation errors ("Select a profile...") the caller must react to and are
  // never routed through errorsState, so it keeps throwing.
  async cancelImport(jobId: string) {
    try {
      await importCancel(jobId);
      await refreshJobs();
    } catch {
      errorsState.addError("Could not cancel import.");
    }
  },
  async retry(jobId: string) {
    try {
      await importRetry(jobId);
      await refreshJobs();
    } catch {
      errorsState.addError("Could not retry import.");
    }
  },
  async dismiss(jobId: string) {
    try {
      // Invalidate a poll that began before dismiss: if it resolves afterwards, it
      // resurrects the removed cards and re-fires terminal notifications. See
      // history.ts clearHistory for the same pattern.
      refreshes.invalidate();
      const jobs = await importDismiss(jobId);
      state.update((s) => ({ ...s, jobs }));
    } catch {
      errorsState.addError("Could not dismiss job.");
    }
  },
  async clearFinished() {
    try {
      refreshes.invalidate();
      const jobs = await importClearFinished();
      state.update((s) => ({ ...s, jobs }));
    } catch {
      errorsState.addError("Could not clear finished jobs.");
    }
  },
  async confirmWipe(jobId: string, proceed: boolean) {
    try {
      await importConfirmWipe(jobId, proceed);
      await refreshJobs();
    } catch (error) {
      errorsState.addError("Could not complete wipe confirmation.");
      state.update((s) => ({ ...s, error: error instanceof Error ? error.message : String(error) }));
    }
  },
};
