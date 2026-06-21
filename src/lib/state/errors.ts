import { writable } from "svelte/store";

export type UiError = {
  id: string;
  level: "info" | "warning" | "error";
  message: string;
};

const state = writable<UiError[]>([]);

let counter = 0;

export const errorsState = {
  subscribe: state.subscribe,
  addError(message: string, level: UiError["level"] = "error") {
    const id = `${Date.now()}-${counter++}`;
    state.update((items) => [...items, { id, level, message }]);
    setTimeout(() => {
      state.update((items) => items.filter((item) => item.id !== id));
    }, 5000);
  },
  dismissError(id: string) {
    state.update((items) => items.filter((item) => item.id !== id));
  },
};
