import { writable } from "svelte/store";

export type PanelTab = "queue" | "history";

/** Which panel (import queue vs history) is shown in the right-column card. */
export const panelTab = writable<PanelTab>("queue");

/** Set true to request opening the profile editor for the active profile
 * (e.g. when it has no API key). App.svelte consumes and resets it. */
export const openProfileEditor = writable(false);
