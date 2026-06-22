import { writable } from "svelte/store";

type ImportOptionsState = {
  keepFiles: boolean;
  stackRawJpeg: boolean;
  stackBurst: boolean;
  dateRange: string | null;
  concurrentTasks: number | null;
};

const initialState: ImportOptionsState = {
  keepFiles: true,
  stackRawJpeg: true,
  stackBurst: true,
  dateRange: null,
  concurrentTasks: null,
};

const state = writable<ImportOptionsState>(initialState);

export const importOptionsState = {
  subscribe: state.subscribe,
  setKeepFiles(keepFiles: boolean) {
    state.update((s) => ({ ...s, keepFiles }));
  },
  setStackRawJpeg(stackRawJpeg: boolean) {
    state.update((s) => ({ ...s, stackRawJpeg }));
  },
  setStackBurst(stackBurst: boolean) {
    state.update((s) => ({ ...s, stackBurst }));
  },
  setDateRange(dateRange: string | null) {
    state.update((s) => ({ ...s, dateRange }));
  },
  setConcurrentTasks(concurrentTasks: number | null) {
    state.update((s) => ({ ...s, concurrentTasks }));
  },
};
