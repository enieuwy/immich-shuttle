import { writable } from "svelte/store";

import { historyClear, historyList } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { ImportRecord } from "$lib/types";

type HistoryState = {
  records: ImportRecord[];
  loading: boolean;
};

const state = writable<HistoryState>({
  records: [],
  loading: false,
});

export const historyState = {
  subscribe: state.subscribe,
  async loadHistory() {
    state.update((s) => ({ ...s, loading: true }));
    try {
      const records = await historyList();
      state.update((s) => ({ ...s, records, loading: false }));
    } catch {
      errorsState.addError("Could not load import history.");
      state.update((s) => ({ ...s, loading: false }));
    }
  },
  async clearHistory() {
    try {
      await historyClear();
      state.update((s) => ({ ...s, records: [] }));
    } catch {
      errorsState.addError("Could not clear import history.");
    }
  },
};
