import { writable } from "svelte/store";

/** Whether the pre-import preview/selection grid dialog is open. */
const state = writable<{ open: boolean }>({ open: false });

export const previewState = {
  subscribe: state.subscribe,
  open(): void {
    state.set({ open: true });
  },
  close(): void {
    state.set({ open: false });
  },
};
