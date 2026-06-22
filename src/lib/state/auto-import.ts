import { get, writable } from "svelte/store";

import { errorsState } from "$lib/state/errors";
import { activeProfile } from "$lib/state/profiles";
import { queueState } from "$lib/state/queue";
import { sourceState } from "$lib/state/source";
import type { RemovableDevice } from "$lib/types";

const STORAGE_KEY = "immich-shuttle-auto-import";

function getStoredEnabled(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === "on";
  } catch {
    return false;
  }
}

type AutoImportState = {
  /** Whether inserting a card with DCIM should offer a one-click import. Off by default. */
  enabled: boolean;
  /** The freshly-inserted DCIM device awaiting the user's decision, if any. */
  candidate: RemovableDevice | null;
};

const state = writable<AutoImportState>({
  enabled: getStoredEnabled(),
  candidate: null,
});

// Mounts we've already accounted for, so a card that stays inserted (or is
// re-observed on every poll) only prompts once. Cleared per-mount when the
// device disappears, so ejecting and re-inserting prompts again.
const seenMounts = new Set<string>();
// Mounts the user explicitly declined; suppressed until the card is removed.
const dismissedMounts = new Set<string>();
// The first device snapshot is the startup baseline — never prompt for cards
// that were already plugged in when the app launched.
let baselineSeeded = false;

function prune(present: Set<string>): void {
  for (const mount of [...seenMounts]) {
    if (!present.has(mount)) {
      seenMounts.delete(mount);
    }
  }
  for (const mount of [...dismissedMounts]) {
    if (!present.has(mount)) {
      dismissedMounts.delete(mount);
    }
  }
}

export const autoImportState = {
  subscribe: state.subscribe,

  setEnabled(enabled: boolean): void {
    try {
      localStorage.setItem(STORAGE_KEY, enabled ? "on" : "off");
    } catch {
      // Best-effort persistence; behavior still applies for the session.
    }
    state.update((s) => ({
      ...s,
      enabled,
      candidate: enabled ? s.candidate : null,
    }));
  },

  /**
   * Reconcile the current removable-device list against what we've seen. When
   * the feature is enabled and a card with a DCIM folder appears that wasn't
   * present at startup (or was ejected and re-inserted), surface it as a
   * candidate for one-click import.
   */
  observe(devices: RemovableDevice[]): void {
    const present = new Set(devices.map((d) => d.mount_path));
    prune(present);

    const remember = () => {
      for (const mount of present) {
        seenMounts.add(mount);
      }
    };

    if (!baselineSeeded) {
      baselineSeeded = true;
      remember();
      return;
    }

    const { enabled } = get(state);
    if (!enabled || !get(activeProfile)) {
      remember();
      return;
    }

    const fresh = devices.find(
      (d) => d.has_dcim && !seenMounts.has(d.mount_path) && !dismissedMounts.has(d.mount_path),
    );
    remember();

    if (fresh) {
      state.update((s) => (s.candidate ? s : { ...s, candidate: fresh }));
    }
  },

  /** Start an import from the candidate card: active profile, no albums, keep files forced. */
  async accept(): Promise<void> {
    const device = get(state).candidate;
    if (!device) {
      return;
    }
    state.update((s) => ({ ...s, candidate: null }));
    try {
      // Reflect the selection in the source picker so progress is visible there.
      await sourceState.selectSources([device.mount_path]);
      await queueState.startImport({
        sourcePaths: [device.mount_path],
        keepFiles: true,
        albumIds: [],
      });
    } catch (error) {
      errorsState.addError(
        error instanceof Error ? error.message : "Could not start auto-import.",
      );
    }
  },

  /** Decline the current candidate; it won't re-prompt until the card is re-inserted. */
  dismiss(): void {
    const device = get(state).candidate;
    if (device) {
      dismissedMounts.add(device.mount_path);
    }
    state.update((s) => ({ ...s, candidate: null }));
  },

  /** Test/preview-only: reset internal detection bookkeeping. */
  _reset(): void {
    seenMounts.clear();
    dismissedMounts.clear();
    baselineSeeded = false;
    state.set({ enabled: getStoredEnabled(), candidate: null });
  },
};
