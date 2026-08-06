import { get, writable } from "svelte/store";

import { historyClear, historyList } from "$lib/api";
import { albumsState } from "$lib/state/albums";
import { errorsState } from "$lib/state/errors";
import { createGeneration } from "$lib/state/generation";
import { importOptionsState } from "$lib/state/import-options";
import { profilesState } from "$lib/state/profiles";
import { selectionState } from "$lib/state/selection";
import { sourceState } from "$lib/state/source";
import { panelTab } from "$lib/state/ui";
import type { ImportRecord } from "$lib/types";

type HistoryState = {
  records: ImportRecord[];
  loading: boolean;
  error: string | null;
  lastImportVersion: number;
  /** True for the duration of replayImport. Drives the single-flight guard
   *  below and lets HistoryPanel disable every "Import again" button — without
   *  it, two rapid clicks (same row or a different one) interleave writes to
   *  the shared profile/album/source/selection stores mid-replay. */
  replaying: boolean;
  /** Record currently being replayed, for per-row pending UI. Null whenever
   *  `replaying` is false. */
  replayingRecordId: string | null;
};

const state = writable<HistoryState>({
  records: [],
  loading: false,
  error: null,
  lastImportVersion: 0,
  replaying: false,
  replayingRecordId: null,
});

// Guards a loadHistory response landing after a newer loadHistory or a
// clearHistory has already superseded it — see src/lib/state/generation.ts.
const loads = createGeneration();

export const historyState = {
  subscribe: state.subscribe,
  /**
   * Signal that a completed run was recorded and may have advanced its
   * per-source checkpoint. Source consumers key their async reads on this
   * version; without the bump, the card can keep showing the checkpoint from
   * before the just-finished run.
   */
  noteImportRecorded() {
    state.update((s) => ({ ...s, lastImportVersion: s.lastImportVersion + 1 }));
  },
  async loadHistory() {
    const isCurrent = loads.begin();
    state.update((s) => ({ ...s, loading: true, error: null }));
    try {
      const records = await historyList();
      if (!isCurrent()) {
        // Superseded by a newer loadHistory or by clearHistory: don't
        // resurrect records the user already deleted, but still drop the
        // spinner -- nothing else will if this was the last in-flight load.
        state.update((s) => ({ ...s, loading: false }));
        return;
      }
      state.update((s) => ({ ...s, records, loading: false, error: null }));
    } catch (error) {
      if (!isCurrent()) {
        state.update((s) => ({ ...s, loading: false }));
        return;
      }
      // Reported here (state.error is only for this panel's own inline
      // rendering); loadHistory has no caller that needs the rejection to make
      // a decision, so it resolves rather than throwing.
      const message = error instanceof Error ? error.message : String(error);
      errorsState.addError("Could not load import history.");
      state.update((s) => ({ ...s, loading: false, error: message }));
    }
  },
  async clearHistory() {
    // Supersede any loadHistory already in flight *before* the round trip, so
    // a load issued just before Clear can never land after it and put the
    // just-deleted records back on screen.
    loads.invalidate();
    try {
      await historyClear();
      state.update((s) => ({
        ...s,
        records: [],
        error: null,
        lastImportVersion: s.lastImportVersion + 1,
      }));
    } catch {
      // Reported here; nothing downstream needs the rejection.
      errorsState.addError("Could not clear import history.");
    }
  },
};

export type ReplayOutcome = "staged" | "no-request" | "profile-missing" | "busy";

/**
 * Stage a past import for review ("Import again"): restore the profile, album,
 * options, and source from the record's persisted request, then let the user
 * confirm and start. Does NOT auto-start — deletion/wipe safety requires a fresh
 * look. Returns "no-request" for records saved before request persistence,
 * "profile-missing" when the recorded profile has since been deleted, and
 * "busy" when a replay is already in flight (nothing is mutated in any of
 * these cases), else "staged".
 *
 * Never rejects: this is a multi-step staging sequence writing several shared
 * stores, so it can't be made transactional cheaply, and HistoryPanel awaits
 * it with no unhandledrejection handler in the webview. Failures are reported
 * through errorsState and leave the store in a defined, documented state
 * instead.
 */
export async function replayImport(record: ImportRecord): Promise<ReplayOutcome> {
  const request = record.request;
  if (!request) return "no-request";

  // Single-flight: this check runs synchronously, before any await, so no
  // second call through THIS function can ever start while one is already in
  // flight -- there is no window for a bypass. (An earlier `replays`
  // generation counter existed alongside this flag to guard the same
  // second-call race and was therefore dead code -- unreachable, since the
  // flag already forecloses the only path that would supersede it -- and has
  // been removed. The real hazard this function has to defend against is
  // NOT a second replayImport call; it's the active profile changing out
  // from under an in-flight one via ProfileSelector, which is a plain UI
  // control that stays interactive for the whole replay and never goes
  // through this function at all. See abandonIfProfileChanged below.)
  if (get(state).replaying) return "busy";

  // History outlives profiles; a deleted profile would leave activeProfile null
  // and the eventual Start would fail. Bail before touching any other store.
  if (!get(profilesState).profiles.some((p) => p.id === request.profile_id)) {
    return "profile-missing";
  }

  state.update((s) => ({ ...s, replaying: true, replayingRecordId: record.id }));

  // loadAlbums below can take several seconds (up to 6 retries against an
  // unreachable server), and ProfileSelector stays interactive the whole
  // time. If the user switches profiles mid-replay, activeProfile.subscribe
  // (albums.ts) clears albumsState and the NEW profile's own AlbumSelector
  // effect repopulates it -- so by the time an await below resumes,
  // `albumsState.loadedProfileId` can match the ACTIVE (new) profile again
  // while `albums.availableAlbums`/`selectedAlbumIds` are the new profile's.
  // Resolving `request.album_ids[0]` against that and calling selectAlbum
  // would write the REPLAYED (old) profile's raw album id in as though it
  // belonged to the new profile, and queueState.startImport's own
  // `loadedProfileId === profile.id` gate would then wave it straight
  // through. Checked after every await below, not just once.
  const abandonIfProfileChanged = () => {
    if (get(profilesState).activeProfileId === request.profile_id) return false;
    errorsState.addError(
      "Import again was abandoned: the active profile changed before it finished loading.",
    );
    return true;
  };

  try {
    profilesState.setActiveProfile(request.profile_id);
    importOptionsState.hydrateFromRequest(request);
    // Whole source is staged for review; any prior preview selection is dropped.
    selectionState.clear();
    sourceState.clearSource();
    panelTab.set("queue");

    // Load the target profile's albums so the recorded album resolves, then
    // restore it. Records carry album_ids (picker path) and/or into_album (the
    // device-rule direct name, with empty album_ids); match whichever is
    // present so the run doesn't silently fall back to the library.
    await albumsState.loadAlbums();
    if (abandonIfProfileChanged()) return "staged";

    const albums = get(albumsState);
    if (albums.error || albums.missingApiKey) {
      // loadAlbums only records failure into albumsState.error/missingApiKey
      // (for the Albums panel's own inline UI) -- it never reaches the global
      // toast, so without this the replay would silently stage without an
      // album and the user would never learn why. Profile/options/selection
      // above are already applied; skip the album step and continue to source
      // restoration rather than abandon the whole replay.
      errorsState.addError(
        `Couldn't restore this import's album -- ${albums.error ?? "no API key configured for this profile"}.`,
      );
    } else {
      albumsState.clearSelection();
      const targetId =
        request.album_ids[0] ??
        (request.into_album
          ? albums.availableAlbums.find((a) => a.album_name === request.into_album)?.id
          : undefined);
      if (targetId) albumsState.selectAlbum(targetId);
    }

    if (abandonIfProfileChanged()) return "staged";
    // selectSources reports its own failures via errorsState (source.ts) and
    // never rejects, so nothing further to check here.
    await sourceState.selectSources(request.source_paths);
    return "staged";
  } catch (error) {
    // Defensive backstop: nothing above currently throws (loadAlbums and
    // selectSources both catch their own IPC failures into local store
    // state), but this function must never leave HistoryPanel with an
    // unhandled rejection regardless. Whatever ran before the failing step
    // stays applied.
    const message = error instanceof Error ? error.message : String(error);
    errorsState.addError(`Couldn't fully restore this import: ${message}`);
    return "staged";
  } finally {
    state.update((s) => ({ ...s, replaying: false, replayingRecordId: null }));
  }
}
