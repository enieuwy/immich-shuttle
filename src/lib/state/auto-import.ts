import { get, writable } from "svelte/store";

import { devicesListRemovable } from "$lib/api";
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
  /**
   * `candidateRule` came from a legacy label/mount key, which does not identify a physical
   * card. The banner must present it as a suggestion, and its delete-after-verify policy
   * must not be applied until the user confirms it for this card.
   */
  candidateRuleNeedsConfirmation: boolean;
};

const state = writable<AutoImportState>({
  enabled: getStoredEnabled(),
  candidate: null,
  candidateRule: null,
  candidateRuleNeedsConfirmation: false,
});

// A lifecycle entry identifies both the mount and the OS-proven volume identity. Mount paths
// are recycled, so a new card at an old mount must not inherit a seen or dismissed state.
function candidateKey(device: RemovableDevice): string | null {
  const volumeId = device.volume_id?.trim();
  return volumeId ? `${device.mount_path}\u0000${volumeId}` : null;
}


// Devices we have already accounted for. A card that stays inserted only prompts once.
const seenMounts = new Set<string>();
// Cards the user explicitly declined. They stay suppressed until they disappear.
const dismissedMounts = new Set<string>();
// Changes whenever another action can invalidate an in-flight candidate prompt.
let candidateRevision = 0;
// The first device snapshot is the startup baseline. It never prompts for cards that were
// already plugged in when the app launched.
let baselineSeeded = false;

function prune(present: Set<string>): void {
  for (const key of [...seenMounts]) {
    if (!present.has(key)) {
      seenMounts.delete(key);
    }
  }
  for (const key of [...dismissedMounts]) {
    if (!present.has(key)) {
      dismissedMounts.delete(key);
    }
  }
}

function clearCandidate(): void {
  state.update((s) => ({
    ...s,
    candidate: null,
    candidateRule: null,
    candidateRuleNeedsConfirmation: false,
  }));
}

export const autoImportState = {
  subscribe: state.subscribe,

  setEnabled(enabled: boolean): void {
    try {
      localStorage.setItem(STORAGE_KEY, enabled ? "on" : "off");
    } catch {
      // Best-effort persistence; behavior still applies for the session.
    }
    candidateRevision += 1;
    state.update((s) => ({
      ...s,
      enabled,
      candidate: enabled ? s.candidate : null,
      candidateRule: enabled ? s.candidateRule : null,
      candidateRuleNeedsConfirmation: enabled ? s.candidateRuleNeedsConfirmation : false,
    }));
  },

  /**
   * Reconcile the current removable-device list against what we've seen. When
   * the feature is enabled and a card with a DCIM folder appears that wasn't
   * present at startup (or was ejected and re-inserted), surface it as a
   * candidate for one-click import.
   */
  observe(devices: RemovableDevice[]): void {
    const present = new Set(
      devices.map(candidateKey).filter((key): key is string => key !== null),
    );
    prune(present);

    const rememberAll = () => {
      for (const key of present) {
        seenMounts.add(key);
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

    let { enabled, candidate } = get(state);
    if (candidate) {
      const key = candidateKey(candidate);
      const stillMounted =
        key !== null &&
        devices.some(
          (device) =>
            device.mount_path === candidate!.mount_path && candidateKey(device) === key,
        );
      if (stillMounted) {
        // A prompt is already showing. Leave every other card unseen so it gets a turn later.
        return;
      }
      // The mounted card changed or disappeared. Its prompt and all settings derived from it
      // are invalid. Pruning above also removes its seen and dismissed lifecycle entries.
      candidateRevision += 1;
      clearCandidate();
      candidate = null;
      ({ enabled } = get(state));
    }

    if (!enabled || !get(activeProfile)) {
      rememberAll();
      return;
    }

    const fresh = devices.find((device) => {
      const key = candidateKey(device);
      return (
        device.has_dcim &&
        key !== null &&
        !seenMounts.has(key) &&
        !dismissedMounts.has(key)
      );
    });
    if (fresh) {
      // Only the card we actually surface is marked seen. Sibling cards stay unseen.
      seenMounts.add(candidateKey(fresh)!);
      candidateRevision += 1;
      const match = deviceRulesState.lookup(fresh);
      state.update((s) => ({
        ...s,
        candidate: fresh,
        candidateRule: match?.rule ?? null,
        candidateRuleNeedsConfirmation: match?.needsConfirmation ?? false,
      }));
    }
  },
  /**
   * Start an import from the candidate card. A rule keyed by the card's own volume identity
   * replays in full (profile / album / wipe policy / options). A rule recognised only under
   * a legacy label/mount key describes some earlier card, so its destination applies only
   * because the user read it on the banner and clicked Import, and its delete-after-verify
   * policy applies only when `confirmDeleteOriginals` reports the extra explicit tick. With
   * no rule the safe default stands: active profile, no album, keep originals.
   */
  async accept(confirmDeleteOriginals = false): Promise<void> {
    const {
      candidate: device,
      candidateRule: rule,
      candidateRuleNeedsConfirmation: needsConfirmation,
    } = get(state);
    const key = device && candidateKey(device);
    if (!device || !key) {
      return;
    }

    const revision = candidateRevision;
    let liveDevice: RemovableDevice | undefined;
    try {
      liveDevice = (await devicesListRemovable()).find(
        (current) =>
          current.mount_path === device.mount_path &&
          current.has_dcim &&
          candidateKey(current) === key,
      );
    } catch {
      // No fresh detector evidence means we cannot safely start work on source media.
      candidateRevision += 1;
      clearCandidate();
      errorsState.addError("Could not verify the removable device before auto-import.");
      return;
    }

    const currentCandidate = get(state).candidate;
    if (
      candidateRevision !== revision ||
      !currentCandidate ||
      candidateKey(currentCandidate) !== key ||
      currentCandidate.mount_path !== device.mount_path
    ) {
      return;
    }
    if (!liveDevice) {
      // A different card may now occupy this mount. Drop the old settings and wait for a
      // fresh observation rather than passing its destination or delete policy to startImport.
      candidateRevision += 1;
      clearCandidate();
      errorsState.addError("The removable device changed. Review it before auto-import.");
      return;
    }

    // Nothing about an unconfirmed legacy rule proves it belongs to this card, so the
    // destructive half of it degrades to the safe answer unless the user ticked it now.
    const effective: DeviceRule | null = rule
      ? { ...rule, keepFiles: needsConfirmation ? !confirmDeleteOriginals : rule.keepFiles }
      : null;
    clearCandidate();
    try {
      // Reflect the selection in the source picker so progress is visible there.
      await sourceState.selectSources([liveDevice.mount_path]);
      await queueState.startImport(
        effective
          ? {
              sourcePaths: [liveDevice.mount_path],
              profileId: effective.profileId,
              albumIds: [],
              intoAlbum: effective.albumName,
              keepFiles: effective.keepFiles,
              stackRawJpeg: effective.stackRawJpeg,
              stackBurst: effective.stackBurst,
              organization: effective.organization,
            }
          : { sourcePaths: [liveDevice.mount_path], keepFiles: true, albumIds: [] },
      );
      // The fresh probe above proves this exact current volume identity before migration.
      if (effective && needsConfirmation) {
        deviceRulesState.migrateLegacyRule(liveDevice, effective);
      }
    } catch (error) {
      // Restore only the exact card whose start failed. A later observation can replace the
      // card at this mount, and must never receive this candidate's rule.
      if (candidateRevision === revision && seenMounts.has(key)) {
        state.update((s) => ({
          ...s,
          candidate: device,
          candidateRule: rule,
          candidateRuleNeedsConfirmation: needsConfirmation,
        }));
      }
      errorsState.addError(
        error instanceof Error ? error.message : "Could not start auto-import.",
      );
    }
  },

  /** Decline the current candidate; it won't re-prompt until the card is re-inserted. */
  dismiss(): void {
    candidateRevision += 1;
    const device = get(state).candidate;
    const key = device && candidateKey(device);
    if (key) {
      dismissedMounts.add(key);
    }
    clearCandidate();
  },

  _reset(): void {
    candidateRevision += 1;
    seenMounts.clear();
    dismissedMounts.clear();
    baselineSeeded = false;
    state.set({
      enabled: getStoredEnabled(),
      candidate: null,
      candidateRule: null,
      candidateRuleNeedsConfirmation: false,
    });
  },
};
