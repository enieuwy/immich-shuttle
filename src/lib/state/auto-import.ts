import { get, writable } from "svelte/store";

import { deviceRulesState, type DeviceRule } from "$lib/state/device-rules";
import { activeProfile } from "$lib/state/profiles";
import { errorsState } from "$lib/state/errors";
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
  /** The saved routing for `candidate`, when one exists — pre-fills the prompt. */
  candidateRule: DeviceRule | null;
};

const state = writable<AutoImportState>({
  enabled: getStoredEnabled(),
  candidate: null,
  candidateRule: null,
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

    const rememberAll = () => {
      for (const mount of present) {
        seenMounts.add(mount);
      }
    };

    // Startup baseline and the not-prompting states (disabled / no active
    // profile) account for everything currently plugged in, so re-enabling
    // later doesn't surface a backlog of cards that were present all along.
    if (!baselineSeeded) {
      baselineSeeded = true;
      rememberAll();
      return;
    }

    const { enabled, candidate } = get(state);
    if (!enabled || !get(activeProfile)) {
      rememberAll();
      return;
    }

    // A prompt is already showing: leave every other freshly-inserted card
    // unseen so it gets its own turn once this one is resolved, instead of being
    // marked seen on this poll and suppressed forever.
    if (candidate) {
      return;
    }

    const fresh = devices.find(
      (d) => d.has_dcim && !seenMounts.has(d.mount_path) && !dismissedMounts.has(d.mount_path),
    );
    if (fresh) {
      // Only the card we actually surface is marked seen; sibling cards inserted
      // in the same batch stay unseen and prompt on a later poll.
      seenMounts.add(fresh.mount_path);
      const rule = deviceRulesState.getRule(fresh);
      state.update((s) => ({ ...s, candidate: fresh, candidateRule: rule }));
    }
  },

  /**
   * Start an import from the candidate card. When the card has a saved rule the
   * import replays it (profile / album / wipe policy / options); otherwise it
   * falls back to the safe default: active profile, no album, keep originals.
   */
  async accept(): Promise<void> {
    const { candidate: device, candidateRule: rule } = get(state);
    if (!device) {
      return;
    }
    state.update((s) => ({ ...s, candidate: null, candidateRule: null }));
    try {
      // Reflect the selection in the source picker so progress is visible there.
      await sourceState.selectSources([device.mount_path]);
      await queueState.startImport(
        rule
          ? {
              sourcePaths: [device.mount_path],
              profileId: rule.profileId,
              albumIds: [],
              intoAlbum: rule.albumName,
              keepFiles: rule.keepFiles,
              stackRawJpeg: rule.stackRawJpeg,
              stackBurst: rule.stackBurst,
              organization: rule.organization,
            }
          : { sourcePaths: [device.mount_path], keepFiles: true, albumIds: [] },
      );
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
    state.update((s) => ({ ...s, candidate: null, candidateRule: null }));
  },

  /** Test/preview-only: reset internal detection bookkeeping. */
  _reset(): void {
    seenMounts.clear();
    dismissedMounts.clear();
    baselineSeeded = false;
    state.set({ enabled: getStoredEnabled(), candidate: null, candidateRule: null });
  },
};
