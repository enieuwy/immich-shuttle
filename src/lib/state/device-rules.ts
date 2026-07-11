import { get, writable } from "svelte/store";

import type { ImportOrganization, RemovableDevice } from "$lib/types";

const STORAGE_KEY = "immich-shuttle-device-rules";

/**
 * A saved auto-import routing for one removable card: which profile/album the
 * card's media goes to, whether originals are wiped after verify, and the
 * stacking/organization options — so re-inserting the card replays the whole
 * import setup instead of forcing the user to re-select everything.
 */
export type DeviceRule = {
  profileId: string;
  albumName: string | null;
  keepFiles: boolean;
  stackRawJpeg: boolean;
  stackBurst: boolean;
  organization: ImportOrganization;
};

type DeviceRules = Record<string, DeviceRule>;

type DeviceIdentity = Pick<RemovableDevice, "name" | "mount_path">;

/**
 * Stable per-card key. The volume label (`name`) is preferred because it
 * survives remounting at a different path; the mount path is the fallback when
 * a card has no usable label (e.g. "Untitled" volumes share no stable identity,
 * so we key those by where they mounted).
 */
export function deviceKey(device: DeviceIdentity): string {
  const name = device.name?.trim();
  return name && name.length > 0 && name.toLowerCase() !== "untitled"
    ? `name:${name}`
    : `mount:${device.mount_path}`;
}

function load(): DeviceRules {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === "object" ? (parsed as DeviceRules) : {};
  } catch {
    return {};
  }
}

function persist(rules: DeviceRules): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(rules));
  } catch {
    // Best-effort persistence; the rule still applies for the session.
  }
}

const state = writable<DeviceRules>(load());

export const deviceRulesState = {
  subscribe: state.subscribe,

  /** The saved rule for a card, or null if none. */
  getRule(device: DeviceIdentity): DeviceRule | null {
    return get(state)[deviceKey(device)] ?? null;
  },

  /** Save (or replace) the routing rule for a card. */
  saveRule(device: DeviceIdentity, rule: DeviceRule): void {
    const next = { ...get(state), [deviceKey(device)]: rule };
    persist(next);
    state.set(next);
  },

  /** Forget a card's rule so it prompts with defaults again. */
  removeRule(device: DeviceIdentity): void {
    const key = deviceKey(device);
    const current = get(state);
    if (!(key in current)) return;
    const next = { ...current };
    delete next[key];
    persist(next);
    state.set(next);
  },

  /** Test-only: clear all rules and persisted state. */
  _reset(): void {
    try {
      localStorage.removeItem(STORAGE_KEY);
    } catch {
      // ignore
    }
    state.set({});
  },
};
