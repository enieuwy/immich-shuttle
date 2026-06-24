import { writable } from "svelte/store";

export type PanelTab = "queue" | "history";

/** Which panel (import queue vs history) is shown in the right-column card. */
export const panelTab = writable<PanelTab>("queue");
