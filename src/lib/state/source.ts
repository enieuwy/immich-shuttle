import { get, writable } from "svelte/store";

import { listen } from "@tauri-apps/api/event";

import { devicesListRemovable, scanCancel, scanSourcesStream } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import { createGeneration } from "$lib/state/generation";
import { selectionState } from "$lib/state/selection";
import type { RemovableDevice, ScanProgress, ScanResult, ScanSummary } from "$lib/types";

/** Terminal status of a streamed scan, mirrored from the backend summary. */
export type ScanOutcome = ScanSummary["status"];

type SourceState = {
  selectedPaths: string[];
  // While `scanning`, the live accumulator of the progress batches. Once a scan
  // settles it is non-null ONLY if that scan actually finished: the backend
  // reports the counts reached so far for a cancelled or timed-out walk, which
  // is a PREFIX of the real inventory, and committing that would let the UI
  // offer a truncated file list as the whole source.
  scanResult: ScanResult | null;
  detectedDevices: RemovableDevice[];
  loadingDevices: boolean;
  scanning: boolean;
  error: string | null;
  // Terminal status of the newest scan, or null while no scan has settled
  // (fresh state, in-flight scan, hard failure). Consumers that must tell a
  // finished scan from an abandoned one read this rather than inferring from
  // counts, which a partial scan of a big card and a full scan of a small one
  // produce identically.
  scanOutcome: ScanOutcome | null;
};

const initialState: SourceState = {
  selectedPaths: [],
  scanResult: null,
  detectedDevices: [],
  loadingDevices: false,
  scanning: false,
  error: null,
  scanOutcome: null,
};
/**
 * Gives App the exact import request for this source state.
 *
 * A selected source always starts with a scan. Only a completed scan may fall
 * back to an unfiltered whole-source request. A source with no paths has never
 * needed a scan, so queueState remains responsible for its existing empty-source
 * validation.
 */
export function importOptionsForSource(
  source: Pick<SourceState, "selectedPaths" | "scanOutcome">,
  selectedFiles: string[],
): { selectFiles?: string[] } | null {
  if (source.selectedPaths.length > 0 && source.scanOutcome !== "complete") return null;
  return selectedFiles.length > 0 ? { selectFiles: selectedFiles } : {};
}


const state = writable<SourceState>(initialState);

// Monotonic token identifying the latest user-initiated scan. Every mutation
// that changes the selection bumps it; scan progress and terminal summaries are
// applied only if their token is still current, so a slow earlier scan can never
// overwrite the state produced by a later action (lost-update race).
let scanGeneration = 0;

// Guards overlapping loadDevices() calls (e.g. rapid device-changed events)
// the same way scanGeneration guards scans: without it, a slow refresh that
// started before a newer mount/eject snapshot can commit after it and
// regress the picker -- and auto-import, which reads this list -- to stale
// device state.
const deviceLoads = createGeneration();

// Identifies the source commit produced by one selectSources call. Handed back
// so a caller that stages a source and then decides to abandon it after an
// await can clear exactly its own commit and nothing newer -- see
// clearSourceIfUnchanged.
export type SourceToken = number;

function emptyScanResult(): ScanResult {
  return {
    files: [],
    photo_count: 0,
    video_count: 0,
    total_size_bytes: 0,
    skipped_unreadable: 0,
  };
}

// Shown in the source card and as a toast: the card can be off screen when the
// scan gives up, and a partial inventory silently disappearing is worse than
// the delay that caused it.
const SCAN_TIMED_OUT =
  "Scanning this source took too long and stopped before it finished. Nothing was imported. Retry, or pick a smaller folder.";

// Returns the generation this scan claimed, so a caller can later ask whether
// the committed source state is still the one it produced.
async function scanSelectedSources(
  paths: string[],
  options: { reconcileSelectionAfter?: boolean } = {},
): Promise<SourceToken> {
  const gen = ++scanGeneration;
  // Unique per invocation and handed to the backend, which stamps it onto
  // every scan-progress event it emits for this scan. The backend checks
  // cancellation before `app.emit`, not before the emit lands, so a batch
  // already in flight when we cancel can still arrive after this function's
  // listener is registered; filtering on scan_id rejects it by provenance.
  // scanGeneration below is a *different* guard: it protects against a scan
  // of OURS being superseded by a later one of ours (e.g. a second
  // selectSources call) whose result should win instead. Both are required.
  const scanId = crypto.randomUUID();
  // Keep the potentially large file list outside reactive state until the
  // terminal summary arrives. Each progress event only updates scalar totals.
  const files: ScanResult["files"] = [];
  let unlisten: (() => void) | undefined;

  state.update((s) => ({
    ...s,
    scanResult: emptyScanResult(),
    scanning: true,
    error: null,
    scanOutcome: null,
  }));

  try {
    unlisten = await listen<ScanProgress>("scan-progress", (event) => {
      if (event.payload.scan_id !== scanId) return;
      if (gen !== scanGeneration) return;

      const progress = event.payload;
      files.push(...progress.files);
      state.update((s) => ({
        ...s,
        scanResult: {
          files: s.scanResult?.files ?? [],
          photo_count: progress.photo_count,
          video_count: progress.video_count,
          total_size_bytes: progress.total_size_bytes,
          skipped_unreadable: progress.skipped_unreadable,
        },
      }));
    });

    if (gen !== scanGeneration) return gen;

    const summary = await scanSourcesStream(paths, scanId);
    if (gen !== scanGeneration) return gen;

    if (summary.status !== "complete") {
      // A cancelled or timed-out walk reports only what it reached, so `files`
      // is a prefix of the source and its length is indistinguishable from a
      // finished scan of a smaller card. Refusing to commit it is what keeps an
      // incomplete preview from being representable as a complete one: every
      // consumer that offers files to import reads `scanResult.files`, so a
      // null here means no exact-selection import can be built from a partial
      // inventory. `selectedPaths` survives, so rescan() can retry the same
      // sources.
      state.update((s) => ({
        ...s,
        scanResult: null,
        scanning: false,
        scanOutcome: summary.status,
        // A user-initiated cancel is a deliberate stop, not a failure, so it
        // gets no error text and no toast; a timeout is a failure and takes the
        // same path as a scan that threw.
        error: summary.status === "timed_out" ? SCAN_TIMED_OUT : null,
      }));
      // Selections are keyed by absolute path against a file list we no longer
      // have. Any survivor would be a hidden pick from an inventory nobody can
      // vouch for, so drop them -- same invariant resetSource enforces.
      selectionState.clear();
      if (summary.status === "timed_out") errorsState.addError(SCAN_TIMED_OUT);
      return gen;
    }

    state.update((s) => ({
      ...s,
      scanResult: {
        files,
        photo_count: summary.photo_count,
        video_count: summary.video_count,
        total_size_bytes: summary.total_size_bytes,
        skipped_unreadable: summary.skipped_unreadable,
      },
      scanning: false,
      scanOutcome: "complete",
    }));

    if (options.reconcileSelectionAfter) {
      // Source removal narrowed the set of scannable files; drop any hidden
      // selection outside it or the backend's validate_selected_under_sources
      // rejects Start Import with a confusing error instead of importing the
      // remaining sources. Gated by the generation check above, so a
      // superseded rescan never reconciles against a stale file list.
      selectionState.retainPaths(files.map((file) => file.path));
    }
  } catch (error) {
    if (gen !== scanGeneration) return gen;
    // Already surfaced via addError + state.error below; selectSources/
    // removePath have no decision to make on scan failure, so don't rethrow.
    errorsState.addError("Could not scan selected source.");
    state.update((s) => ({
      ...s,
      // Same invariant as the non-complete summary above: `scanResult` is
      // non-null only for a scan that finished. The progress batches that
      // landed before the failure had already put their running totals here,
      // and leaving them would render as a successful count beside an empty
      // grid.
      scanResult: null,
      scanning: false,
      error: error instanceof Error ? error.message : String(error),
    }));
  } finally {
    unlisten?.();
  }
  return gen;
}

// Full source reset: drops the selected paths, the scan result, and -- because
// selections are keyed by absolute path and outlive the scan that produced
// them -- the media selection too. A surviving hidden selection would point at
// nothing importable, and would silently re-activate the moment a later scan
// exposed the same absolute path again: removable cards reuse both mount names
// and DCIM filenames, so a different card can resurrect a selection from the
// previous one and stage only the path-colliding files. Same invariant
// removePath enforces when the last source goes away.
function resetSource(): void {
  // Invalidate any in-flight scan so its late result can't repopulate state.
  scanGeneration++;
  state.update((s) => ({
    ...s,
    selectedPaths: [],
    scanResult: null,
    scanning: false,
    error: null,
    scanOutcome: null,
  }));
  selectionState.clear();
}

export const sourceState = {
  subscribe: state.subscribe,
  async loadDevices() {
    const isCurrent = deviceLoads.begin();
    state.update((s) => ({ ...s, loadingDevices: true, error: null }));
    try {
      const detectedDevices = await devicesListRemovable();
      // Overlapping device-changed events can start refreshes out of order;
      // only the newest one may commit, or a slow refresh regresses the
      // picker (and auto-import) to a stale device list.
      if (!isCurrent()) return;
      state.update((s) => ({ ...s, detectedDevices, loadingDevices: false }));
    } catch (error) {
      if (!isCurrent()) return;
      // Already surfaced via addError + state.error below; the device-changed
      // listener has no decision to make on a failed refresh.
      errorsState.addError("Could not load removable devices.");
      state.update((s) => ({
        ...s,
        loadingDevices: false,
        error: error instanceof Error ? error.message : String(error),
      }));
    }
  },
  /** Add `paths` to the selected sources and rescan. Returns a token owning the
   *  resulting source commit, or null when there was nothing to select -- no
   *  scan ran, so the caller owns no commit. */
  async selectSources(paths: string[]): Promise<SourceToken | null> {
    if (paths.length === 0) return null;
    let currentPaths: string[] = [];
    state.update((s) => {
      const selectedPaths = Array.from(new Set([...s.selectedPaths, ...paths]));
      currentPaths = selectedPaths;
      return { ...s, selectedPaths };
    });
    return await scanSelectedSources(currentPaths);
  },
  async removePath(path: string) {
    let remaining: string[] = [];
    state.update((s) => {
      remaining = s.selectedPaths.filter((selectedPath) => selectedPath !== path);
      if (remaining.length === 0) {
        // Invalidate any in-flight scan so its late events/result cannot
        // repopulate the just-cleared source state.
        scanGeneration++;
        return {
          ...s,
          selectedPaths: [],
          scanResult: null,
          scanning: false,
          error: null,
          scanOutcome: null,
        };
      }
      return { ...s, selectedPaths: remaining };
    });
    if (remaining.length === 0) {
      // No sources left means no scannable files at all -- any hidden
      // selection would point at nothing importable.
      selectionState.clear();
      return;
    }
    // Reconcile once the rescan resolves and the new file list is known
    // (guarded internally against a superseded rescan), not here against the
    // old list -- reconciling now against the pre-rescan list could drop a
    // path that's still valid.
    await scanSelectedSources(remaining, { reconcileSelectionAfter: true });
  },
  /** Re-run the scan for the sources already selected. The retry path for an
   *  incomplete scan (`scanOutcome` "cancelled"/"timed_out"), which drops the
   *  partial result but keeps the selection: there is nothing new to pick, so
   *  this must not go through selectSources and re-derive one. */
  async rescan(): Promise<SourceToken | null> {
    const paths = get(state).selectedPaths;
    if (paths.length === 0) return null;
    return await scanSelectedSources(paths);
  },
  async cancelScan() {
    try {
      await scanCancel();
    } catch {
      // Cancellation is best-effort; the in-flight stream owns terminal state.
    }
  },
  clearSource() {
    resetSource();
  },
  /** Reset the source only if `token` -- returned by selectSources -- still
   *  identifies the newest source commit. A caller that stages a source and
   *  then abandons it after an await uses this so its cleanup cannot clobber a
   *  newer source the user established while that await was pending. */
  clearSourceIfUnchanged(token: SourceToken): boolean {
    if (token !== scanGeneration) return false;
    resetSource();
    return true;
  },
};
