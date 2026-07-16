import { writable } from "svelte/store";

import { scanSources } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { RemovableDevice, ScanResult } from "$lib/types";
import { devicesListRemovable } from "$lib/api";

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
// that changes the selection bumps it; an async `scanSources` result is applied
// only if its token is still current, so a slow earlier scan can never overwrite
// the state produced by a later action (lost-update race).
let scanGeneration = 0;

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
    const generation = ++scanGeneration;
    let currentPaths: string[] = [];
    state.update((s) => {
      const selectedPaths = Array.from(new Set([...s.selectedPaths, ...paths]));
      currentPaths = selectedPaths;
      return { ...s, selectedPaths, scanning: true, error: null };
    });
    try {
      const scanResult = await scanSources(currentPaths);
      if (generation !== scanGeneration) return;
      state.update((s) => ({ ...s, scanResult, scanning: false }));
    } catch (error) {
      if (generation !== scanGeneration) return;
      errorsState.addError("Could not scan selected source.");
      state.update((s) => ({
        ...s,
        scanResult: null,
        scanning: false,
        error: error instanceof Error ? error.message : String(error),
      }));
    }
  },
  async removePath(path: string) {
    const generation = ++scanGeneration;
    let remaining: string[] = [];
    state.update((s) => {
      remaining = s.selectedPaths.filter((selectedPath) => selectedPath !== path);
      if (remaining.length === 0) {
        return { ...s, selectedPaths: [], scanResult: null, scanning: false, error: null };
      }
      return { ...s, selectedPaths: remaining, scanning: true, error: null };
    });
    if (remaining.length === 0) return;
    try {
      const scanResult = await scanSources(remaining);
      if (generation !== scanGeneration) return;
      state.update((s) => ({ ...s, scanResult, scanning: false }));
    } catch (error) {
      if (generation !== scanGeneration) return;
      errorsState.addError("Could not scan selected source.");
      state.update((s) => ({
        ...s,
        scanResult: null,
        scanning: false,
        error: error instanceof Error ? error.message : String(error),
      }));
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
