import { get, writable } from "svelte/store";

/**
 * Tracks which media files the user has selected in the pre-import preview grid.
 * Paths are the absolute file paths from the scan result. The Set is replaced
 * on every mutation so Svelte reactivity fires on `$selectionState.selected`.
 */
type SelectionState = {
  selected: Set<string>;
};

const state = writable<SelectionState>({ selected: new Set() });

export const selectionState = {
  subscribe: state.subscribe,

  toggle(path: string): void {
    state.update((s) => {
      const next = new Set(s.selected);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return { selected: next };
    });
  },

  /** Replace the selection with exactly these paths. */
  selectOnly(paths: string[]): void {
    state.set({ selected: new Set(paths) });
  },

  clear(): void {
    state.set({ selected: new Set() });
  },

  /** Non-reactive read; for reactive checks use `$selectionState.selected.has(path)`. */
  has(path: string): boolean {
    return get(state).selected.has(path);
  },

  /** Current selection as an array (non-reactive). */
  paths(): string[] {
    return [...get(state).selected];
  },
};
