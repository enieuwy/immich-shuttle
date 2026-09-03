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

type DeviceIdentity = Pick<RemovableDevice, "name" | "mount_path" | "volume_id">;

/**
 * A rule found for an inserted card, plus whether it is safe to act on unattended.
 *
 * `needsConfirmation` is set when the rule was only found under a legacy label/mount key.
 * Those keys do not identify a physical medium, so the rule is a suggestion about a
 * different card until the user reconfirms it against this one.
 */
export type RuleMatch = {
  rule: DeviceRule;
  needsConfirmation: boolean;
};

/**
 * The only key a rule may be trusted under: the OS-proven volume identity.
 *
 * A rule carries a destination profile/album and a delete-after-verify policy, so keying it
 * by anything a second card can also present routes one card's photos into another card's
 * album and can wipe originals the user never agreed to wipe. Volume labels are factory
 * defaults shared across a whole batch of cards, and mount paths are recycled the moment a
 * card is swapped, so both are disqualified. Returns null when the platform could not prove
 * an identity; the card then gets no rule at all.
 */
export function deviceKey(device: DeviceIdentity): string | null {
  const id = device.volume_id?.trim();
  return id && id.length > 0 ? `vol:${id}` : null;
}

/**
 * The pre-volume-id key scheme, kept only so already-persisted rules can be recognised and
 * migrated. Never written to.
 */
function legacyKey(device: DeviceIdentity): string {
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

  /**
   * The rule for a card. A card with no stable identity is not looked up at all: there is
   * no key that would answer "is this the same card?" for it, so any hit would be a guess.
   */
  lookup(device: DeviceIdentity): RuleMatch | null {
    const key = deviceKey(device);
    if (!key) return null;

    const rules = get(state);
    const stable = rules[key];
    if (stable) return { rule: stable, needsConfirmation: false };

    const legacy = rules[legacyKey(device)];
    return legacy ? { rule: legacy, needsConfirmation: true } : null;
  },

  /**
   * Save (or replace) the routing rule for a card. Refused, and reported as refused, for a
   * card with no stable identity — writing it under the label or the mount path is what
   * made the rule apply to the wrong card in the first place.
   */
  saveRule(device: DeviceIdentity, rule: DeviceRule): boolean {
    const key = deviceKey(device);
    if (!key) return false;
    const next = { ...get(state), [key]: rule };
    persist(next);
    state.set(next);
    return true;
  },

  /**
   * Adopt a reconfirmed legacy rule for this card: write it under the stable identity and
   * drop the ambiguous entry, so the next insert of this card applies it unattended and no
   * other card ever inherits it.
   */
  migrateLegacyRule(device: DeviceIdentity, rule: DeviceRule): boolean {
    const key = deviceKey(device);
    if (!key) return false;
    const next = { ...get(state), [key]: rule };
    delete next[legacyKey(device)];
    persist(next);
    state.set(next);
    return true;
  },

  /** Forget a card's rule so it prompts with defaults again. */
  removeRule(device: DeviceIdentity): void {
    const key = deviceKey(device);
    const current = get(state);
    // The legacy entry goes too: "Forget" that left an ambiguous rule behind would let it
    // resurface as a suggestion on the very next insert.
    const doomed = [key, legacyKey(device)].filter((k): k is string => !!k && k in current);
    if (doomed.length === 0) return;
    const next = { ...current };
    for (const k of doomed) delete next[k];
    persist(next);
    state.set(next);
  },

  /**
   * Test-only: replace all rules. `seed` exists so a suite can construct the pre-volume-id
   * on-disk shape (`name:`/`mount:` keys), which no production code path writes any more.
   */
  _reset(seed: DeviceRules = {}): void {
    try {
      if (Object.keys(seed).length === 0) {
        localStorage.removeItem(STORAGE_KEY);
      } else {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(seed));
      }
    } catch {
      // ignore
    }
    state.set(seed);
  },
};
