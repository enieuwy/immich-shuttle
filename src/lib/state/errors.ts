import { writable } from "svelte/store";

export type UiError = {
  id: string;
  level: "info" | "warning" | "error";
  message: string;
  dedupeKey?: string;
};

const state = writable<UiError[]>([]);

let counter = 0;

export const errorsState = {
  subscribe: state.subscribe,
  addError(message: string, level: UiError["level"] = "error", dedupeKey?: string) {
    const id = `${Date.now()}-${counter++}`;
    let added = false;
    state.update((items) => {
      if (
        dedupeKey &&
        items.some(
          (item) =>
            item.dedupeKey === dedupeKey && item.level === level && item.message === message,
        )
      ) {
        return items;
      }
      added = true;
      return [...items, { id, level, message, ...(dedupeKey ? { dedupeKey } : {}) }];
    });
    if (added && level !== "error") {
      setTimeout(() => {
        state.update((items) => items.filter((item) => item.id !== id));
      }, 5000);
    }
  },
  dismissError(id: string) {
    state.update((items) => items.filter((item) => item.id !== id));
  },
  /** Drop every active error carrying this key. Callers that dedupe by key use
   *  this on recovery: the keyed error stays suppressed while it is active, so
   *  without an explicit clear the next failure of the same kind would be
   *  swallowed by the message the user was already shown. */
  clearKeyed(dedupeKey: string) {
    state.update((items) => items.filter((item) => item.dedupeKey !== dedupeKey));
  },
};

