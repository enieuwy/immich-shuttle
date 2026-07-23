import { writable } from "svelte/store";

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

/**
 * Stage a past import for review ("Import again"): restore the profile, album,
 * options, and source from the record's persisted request, then let the user
 * confirm and start. Does NOT auto-start — deletion/wipe safety requires a fresh
 * look. Returns false when the record predates request persistence (no request
 * stored), so callers can surface that the run can't be replayed.
 */
export async function replayImport(record: ImportRecord): Promise<boolean> {
  const request = record.request;
  if (!request) return false;

  profilesState.setActiveProfile(request.profile_id);
  albumsState.clearSelection();
  if (request.album_ids.length > 0) {
    albumsState.selectAlbum(request.album_ids[0]);
  }
  importOptionsState.hydrateFromRequest(request);
  // Whole source is staged for review; any prior preview selection is dropped.
  selectionState.clear();
  sourceState.clearSource();
  panelTab.set("queue");
  await sourceState.selectSources(request.source_paths);
  return true;
}
