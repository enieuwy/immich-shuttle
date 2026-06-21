import { writable } from "svelte/store";

type ImportOptionsState = {
  keepFiles: boolean;
  stackRawJpeg: boolean;
  stackBurst: boolean;
};

const initialState: ImportOptionsState = {
  keepFiles: true,
  stackRawJpeg: true,
  stackBurst: true,
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
};
