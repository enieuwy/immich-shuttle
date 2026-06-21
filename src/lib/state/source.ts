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
    state.update((s) => ({
      ...s,
      selectedPaths: [...s.selectedPaths, ...paths],
      scanning: true,
      error: null,
    }));
    try {
      let currentPaths: string[] = [];
      state.subscribe((s) => { currentPaths = s.selectedPaths; })();
      const scanResult = await scanSources(currentPaths);
      state.update((s) => ({ ...s, scanResult, scanning: false }));
    } catch (error) {
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
    state.update((s) => ({ ...s, selectedPaths: [], scanResult: null, error: null }));
  },
};
