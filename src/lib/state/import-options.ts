import { writable } from "svelte/store";

import type { ImportOrganization } from "$lib/types";

type ImportOptionsState = {
  keepFiles: boolean;
  stackRawJpeg: boolean;
  stackBurst: boolean;
  concurrentTasks: number | null;
  /** Inclusive capture-date lower bound as "YYYY-MM-DD", or null. */
  dateFrom: string | null;
  /** Inclusive capture-date upper bound as "YYYY-MM-DD", or null. */
  dateTo: string | null;
  /** How to map the source folder tree onto Immich albums/tags. */
  organization: ImportOrganization;
};

const initialState: ImportOptionsState = {
  keepFiles: true,
  stackRawJpeg: true,
  stackBurst: true,
  concurrentTasks: null,
  dateFrom: null,
  dateTo: null,
  organization: "single_album",
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

  setConcurrentTasks(concurrentTasks: number | null) {
    state.update((s) => ({ ...s, concurrentTasks }));
  },
  setDateFrom(dateFrom: string | null) {
    state.update((s) => ({ ...s, dateFrom: dateFrom || null }));
  },
  setDateTo(dateTo: string | null) {
    state.update((s) => ({ ...s, dateTo: dateTo || null }));
  },
  setOrganization(organization: ImportOrganization) {
    state.update((s) => ({ ...s, organization }));
  },
  clearDateRange() {
    state.update((s) => ({ ...s, dateFrom: null, dateTo: null }));
  },
};

const YMD = /^\d{4}-\d{2}-\d{2}$/;

/**
 * Build immich-go's `--date-range=YYYY-MM-DD,YYYY-MM-DD` value from the From/To
 * pickers. Returns null unless both bounds are present, well-formed, and
 * ordered From <= To (zero-padded ISO dates compare correctly as strings).
 */
export function toImmichDateRange(from: string | null, to: string | null): string | null {
  if (!from || !to || !YMD.test(from) || !YMD.test(to) || from > to) return null;
  return `${from},${to}`;
}
