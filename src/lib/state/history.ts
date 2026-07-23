import { writable } from "svelte/store";

import { historyClear, historyList } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { ImportRecord } from "$lib/types";

type HistoryState = {
  records: ImportRecord[];
  loading: boolean;
  error: string | null;
};

const state = writable<HistoryState>({
  records: [],
  loading: false,
  error: null,
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
      state.update((s) => ({ ...s, records: [], error: null }));
    } catch {
      errorsState.addError("Could not clear import history.");
    }
  },
};
