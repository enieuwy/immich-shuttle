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
  /** Keep importing when a file fails (immich-go --on-errors=continue). */
  keepGoingOnErrors: boolean;
  /** Replace assets already on the server (immich-go --overwrite). */
  overwrite: boolean;
  /** Tags applied to every uploaded asset (immich-go --tag). */
  tags: string[];
  /** Tag this upload session with a timestamp (immich-go --session-tag). */
  sessionTag: boolean;
  /** Import only media captured since this source's last import (date floor). */
  onlyNewSinceLastImport: boolean;
  /** Restrict import to one media kind: "all" | "image" | "video". */
  mediaType: "all" | "image" | "video";
  /** Only import files with these extensions (immich-go --include-extensions). */
  includeExtensions: string[];
  /** Skip files with these extensions (immich-go --exclude-extensions). */
  excludeExtensions: string[];
};

const initialState: ImportOptionsState = {
  keepFiles: true,
  stackRawJpeg: true,
  stackBurst: true,
  concurrentTasks: null,
  dateFrom: null,
  dateTo: null,
  organization: "single_album",
  // Default to continue: one bad file must not abort a large migration; the app
  // surfaces per-file errors from the run log afterward.
  keepGoingOnErrors: true,
  overwrite: false,
  tags: [],
  sessionTag: false,
  onlyNewSinceLastImport: false,
  mediaType: "all",
  includeExtensions: [],
  excludeExtensions: [],
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
  setKeepGoingOnErrors(keepGoingOnErrors: boolean) {
    state.update((s) => ({ ...s, keepGoingOnErrors }));
  },
  setOverwrite(overwrite: boolean) {
    state.update((s) => ({ ...s, overwrite }));
  },
  setTags(tags: string[]) {
    state.update((s) => ({ ...s, tags }));
  },
  setSessionTag(sessionTag: boolean) {
    state.update((s) => ({ ...s, sessionTag }));
  },
  setOnlyNewSinceLastImport(onlyNewSinceLastImport: boolean) {
    state.update((s) => ({ ...s, onlyNewSinceLastImport }));
  },
  setMediaType(mediaType: "all" | "image" | "video") {
    state.update((s) => ({ ...s, mediaType }));
  },
  setIncludeExtensions(includeExtensions: string[]) {
    state.update((s) => ({ ...s, includeExtensions }));
  },
  setExcludeExtensions(excludeExtensions: string[]) {
    state.update((s) => ({ ...s, excludeExtensions }));
  },
  clearDateRange() {
    state.update((s) => ({ ...s, dateFrom: null, dateTo: null }));
  },
};

const YMD = /^\d{4}-\d{2}-\d{2}$/;

/** True when two complete date bounds are ordered backwards. */
export function isDateRangeInvalid(from: string | null, to: string | null): boolean {
  return Boolean(from && to && YMD.test(from) && YMD.test(to) && from > to);
}


/**
 * Build immich-go's `--date-range=YYYY-MM-DD,YYYY-MM-DD` value from the From/To
 * pickers. Returns null unless both bounds are present, well-formed, and
 * ordered From <= To (zero-padded ISO dates compare correctly as strings).
 */
export function toImmichDateRange(from: string | null, to: string | null): string | null {
  if (!from || !to || !YMD.test(from) || !YMD.test(to) || isDateRangeInvalid(from, to)) return null;
  return `${from},${to}`;
}
