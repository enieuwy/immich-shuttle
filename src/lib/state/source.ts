import { writable } from "svelte/store";

import { listen } from "@tauri-apps/api/event";

import { devicesListRemovable, scanCancel, scanSourcesStream } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { RemovableDevice, ScanProgress, ScanResult } from "$lib/types";

type SourceState = {
  selectedPaths: string[];
  scanResult: ScanResult | null;
  detectedDevices: RemovableDevice[];
  loadingDevices: boolean;
  scanning: boolean;
  error: string | null;
};

const initialState: SourceState = {
  selectedPaths: [],
  scanResult: null,
  detectedDevices: [],
  loadingDevices: false,
  scanning: false,
  error: null,
};

const state = writable<SourceState>(initialState);

// Monotonic token identifying the latest user-initiated scan. Every mutation
// that changes the selection bumps it; scan progress and terminal summaries are
// applied only if their token is still current, so a slow earlier scan can never
// overwrite the state produced by a later action (lost-update race).
let scanGeneration = 0;

function emptyScanResult(): ScanResult {
  return {
    files: [],
    photo_count: 0,
    video_count: 0,
    total_size_bytes: 0,
    skipped_unreadable: 0,
  };
}

async function scanSelectedSources(paths: string[]): Promise<void> {
  const gen = ++scanGeneration;
  // Keep the potentially large file list outside reactive state until the
  // terminal summary arrives. Each progress event only updates scalar totals.
  const files: ScanResult["files"] = [];
  let unlisten: (() => void) | undefined;

  state.update((s) => ({
    ...s,
    scanResult: emptyScanResult(),
    scanning: true,
    error: null,
  }));

  try {
    unlisten = await listen<ScanProgress>("scan-progress", (event) => {
      if (gen !== scanGeneration) return;

      const progress = event.payload;
      files.push(...progress.files);
      state.update((s) => ({
        ...s,
        scanResult: {
          files: s.scanResult?.files ?? [],
          photo_count: progress.photo_count,
          video_count: progress.video_count,
          total_size_bytes: progress.total_size_bytes,
          skipped_unreadable: progress.skipped_unreadable,
        },
      }));
    });

    if (gen !== scanGeneration) return;

    const summary = await scanSourcesStream(paths);
    if (gen !== scanGeneration) return;

    state.update((s) => ({
      ...s,
      scanResult: {
        files,
        photo_count: summary.photo_count,
        video_count: summary.video_count,
        total_size_bytes: summary.total_size_bytes,
        skipped_unreadable: summary.skipped_unreadable,
      },
      scanning: false,
    }));
  } catch (error) {
    if (gen !== scanGeneration) return;
    errorsState.addError("Could not scan selected source.");
    state.update((s) => ({
      ...s,
      scanning: false,
      error: error instanceof Error ? error.message : String(error),
    }));
  } finally {
    unlisten?.();
  }
}

export const sourceState = {
  subscribe: state.subscribe,
  async loadDevices() {
    state.update((s) => ({ ...s, loadingDevices: true, error: null }));
    try {
      const detectedDevices = await devicesListRemovable();
      state.update((s) => ({ ...s, detectedDevices, loadingDevices: false }));
    } catch (error) {
      errorsState.addError("Could not load removable devices.");
      state.update((s) => ({
        ...s,
        loadingDevices: false,
        error: error instanceof Error ? error.message : String(error),
      }));
    }
  },
  async selectSources(paths: string[]) {
    if (paths.length === 0) return;
    let currentPaths: string[] = [];
    state.update((s) => {
      const selectedPaths = Array.from(new Set([...s.selectedPaths, ...paths]));
      currentPaths = selectedPaths;
      return { ...s, selectedPaths };
    });
    await scanSelectedSources(currentPaths);
  },
  async removePath(path: string) {
    let remaining: string[] = [];
    state.update((s) => {
      remaining = s.selectedPaths.filter((selectedPath) => selectedPath !== path);
      if (remaining.length === 0) {
        // Invalidate any in-flight scan so its late events/result cannot
        // repopulate the just-cleared source state.
        scanGeneration++;
        return { ...s, selectedPaths: [], scanResult: null, scanning: false, error: null };
      }
      return { ...s, selectedPaths: remaining };
    });
    if (remaining.length === 0) return;
    await scanSelectedSources(remaining);
  },
  async cancelScan() {
    try {
      await scanCancel();
    } catch {
      // Cancellation is best-effort; the in-flight stream owns terminal state.
    }
  },
  clearSource() {
    // Invalidate any in-flight scan so its late result can't repopulate state.
    scanGeneration++;
    state.update((s) => ({
      ...s,
      selectedPaths: [],
      scanResult: null,
      scanning: false,
      error: null,
    }));
  },
};
