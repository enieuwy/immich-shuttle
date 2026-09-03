import { writable } from "svelte/store";

import type { ImportInput, ImportOrganization } from "$lib/types";

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

/** A whole-store value, as read with `get(importOptionsState)`. Named so a
 *  caller can hold one across an await and hand it back to `restore`. */
export type ImportOptionsSnapshot = ImportOptionsState;

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

const DEFAULTS_KEY = "immich-shuttle-import-defaults";

// Durable, cross-import preferences (edited in Settings) live in this same
// store but persist to localStorage; per-import fields (dates, selection, tags,
// media type, include-extensions) stay ephemeral. Only this subset is saved.
function loadDurable(base: ImportOptionsState): ImportOptionsState {
  try {
    const raw = localStorage.getItem(DEFAULTS_KEY);
    if (!raw) return base;
    const d = JSON.parse(raw) as Partial<ImportOptionsState>;
    return {
      ...base,
      stackRawJpeg: d.stackRawJpeg ?? base.stackRawJpeg,
      stackBurst: d.stackBurst ?? base.stackBurst,
      concurrentTasks: d.concurrentTasks ?? base.concurrentTasks,
      keepGoingOnErrors: d.keepGoingOnErrors ?? base.keepGoingOnErrors,
      sessionTag: d.sessionTag ?? base.sessionTag,
      excludeExtensions: d.excludeExtensions ?? base.excludeExtensions,
    };
  } catch {
    return base;
  }
}

/** Persist the durable subset (best-effort) and return the state unchanged so
 *  it can be used inline inside `state.update`. */
function persistDurable(s: ImportOptionsState): ImportOptionsState {
  try {
    localStorage.setItem(
      DEFAULTS_KEY,
      JSON.stringify({
        stackRawJpeg: s.stackRawJpeg,
        stackBurst: s.stackBurst,
        concurrentTasks: s.concurrentTasks,
        keepGoingOnErrors: s.keepGoingOnErrors,
        sessionTag: s.sessionTag,
        excludeExtensions: s.excludeExtensions,
      }),
    );
  } catch {
    // Persistence is best-effort (private mode, quota); defaults still work.
  }
  return s;
}

const state = writable<ImportOptionsState>(loadDurable(initialState));

export const importOptionsState = {
  subscribe: state.subscribe,
  setKeepFiles(keepFiles: boolean) {
    state.update((s) => ({ ...s, keepFiles }));
  },
  setStackRawJpeg(stackRawJpeg: boolean) {
    state.update((s) => persistDurable({ ...s, stackRawJpeg }));
  },
  setStackBurst(stackBurst: boolean) {
    state.update((s) => persistDurable({ ...s, stackBurst }));
  },

  setConcurrentTasks(concurrentTasks: number | null) {
    state.update((s) => persistDurable({ ...s, concurrentTasks }));
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
    state.update((s) => persistDurable({ ...s, keepGoingOnErrors }));
  },
  setOverwrite(overwrite: boolean) {
    state.update((s) => ({ ...s, overwrite }));
  },
  setTags(tags: string[]) {
    state.update((s) => ({ ...s, tags }));
  },
  setSessionTag(sessionTag: boolean) {
    state.update((s) => persistDurable({ ...s, sessionTag }));
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
    state.update((s) => persistDurable({ ...s, excludeExtensions }));
  },
  clearDateRange() {
    state.update((s) => ({ ...s, dateFrom: null, dateTo: null }));
  },
  /**
   * Clear the replay-restorable filters surfaced beside Start Import. Durable
   * extension exclusions and the visible per-source "only new" mode must
   * survive because neither is an invisible History leftover.
   */
  clearImportFilters() {
    state.update((s) => ({
      ...s,
      dateFrom: null,
      dateTo: null,
      mediaType: "all",
      includeExtensions: [],
    }));
  },
  /**
   * Repopulate every option from a persisted import request (History "Import
   * again"). Mirrors the request-building in queueState.startImport in reverse.
   */
  hydrateFromRequest(request: ImportInput) {
    const [from, to] = request.date_range?.split(",") ?? [];
    const mediaType =
      request.include_type === "VIDEO"
        ? "video"
        : request.include_type === "IMAGE"
          ? "image"
          : "all";
    state.set({
      keepFiles: request.keep_files,
      stackRawJpeg: request.stack_raw_jpeg,
      stackBurst: request.stack_burst,
      concurrentTasks: request.concurrent_tasks ?? null,
      dateFrom: from ?? null,
      dateTo: to ?? null,
      organization: request.organization ?? "single_album",
      keepGoingOnErrors: request.on_errors === "continue",
      overwrite: request.overwrite ?? false,
      tags: request.tags ?? [],
      sessionTag: request.session_tag ?? false,
      // Explicit date bounds are restored above; "only new since last import" is
      // a separate per-source mode a stored request cannot unambiguously encode.
      onlyNewSinceLastImport: false,
      mediaType,
      includeExtensions: request.include_extensions ?? [],
      excludeExtensions: request.exclude_extensions ?? [],
    });
  },
  /**
   * Put a value previously read from this store back, wholesale. The undo for
   * hydrateFromRequest: a History replay that gets abandoned has to drop the
   * record's options -- `keepFiles: false` above all -- and no combination of
   * the setters above expresses "exactly what was here before" atomically.
   * Deliberately does NOT persist: the durable subset in localStorage was never
   * touched by hydrateFromRequest either, so restoring must not write it back.
   */
  restore(snapshot: ImportOptionsSnapshot) {
    state.set(snapshot);
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
