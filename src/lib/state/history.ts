import { get, writable } from "svelte/store";

import { historyClear, historyList } from "$lib/api";
import { albumsState } from "$lib/state/albums";
import { errorsState } from "$lib/state/errors";
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
};

const state = writable<HistoryState>({
  records: [],
  loading: false,
  error: null,
  lastImportVersion: 0,
});


export const historyState = {
  subscribe: state.subscribe,
  async loadHistory() {
    state.update((s) => ({ ...s, loading: true, error: null }));
    try {
      const records = await historyList();
      state.update((s) => ({ ...s, records, loading: false, error: null }));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      errorsState.addError("Could not load import history.");
      state.update((s) => ({ ...s, loading: false, error: message }));
    }
  },
  async clearHistory() {
    try {
      await historyClear();
      state.update((s) => ({
        ...s,
        records: [],
        error: null,
        lastImportVersion: s.lastImportVersion + 1,
      }));
    } catch {
      errorsState.addError("Could not clear import history.");
    }
  },
};

export type ReplayOutcome = "staged" | "no-request" | "profile-missing";

/**
 * Stage a past import for review ("Import again"): restore the profile, album,
 * options, and source from the record's persisted request, then let the user
 * confirm and start. Does NOT auto-start — deletion/wipe safety requires a fresh
 * look. Returns "no-request" for records saved before request persistence and
 * "profile-missing" when the recorded profile has since been deleted (nothing is
 * mutated in either case), else "staged".
 */
export async function replayImport(record: ImportRecord): Promise<ReplayOutcome> {
  const request = record.request;
  if (!request) return "no-request";

  // History outlives profiles; a deleted profile would leave activeProfile null
  // and the eventual Start would fail. Bail before touching any other store.
  if (!get(profilesState).profiles.some((p) => p.id === request.profile_id)) {
    return "profile-missing";
  }

  profilesState.setActiveProfile(request.profile_id);
  importOptionsState.hydrateFromRequest(request);
  // Whole source is staged for review; any prior preview selection is dropped.
  selectionState.clear();
  sourceState.clearSource();
  panelTab.set("queue");

  // Load the target profile's albums so the recorded album resolves, then
  // restore it. Records carry album_ids (picker path) and/or into_album (the
  // device-rule direct name, with empty album_ids); match whichever is present
  // so the run doesn't silently fall back to the library.
  await albumsState.loadAlbums();
  albumsState.clearSelection();
  const albums = get(albumsState).availableAlbums;
  const targetId =
    request.album_ids[0] ??
    (request.into_album
      ? albums.find((a) => a.album_name === request.into_album)?.id
      : undefined);
  if (targetId) albumsState.selectAlbum(targetId);

  await sourceState.selectSources(request.source_paths);
  return "staged";
}
