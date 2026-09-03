import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

import { deviceKey, deviceRulesState, type DeviceRule } from "./device-rules";
import type { RemovableDevice } from "$lib/types";

const canon: RemovableDevice = {
  name: "CANON_EOS",
  mount_path: "/Volumes/CANON_EOS",
  total_space: 64 * 1024 ** 3,
  available_space: 12 * 1024 ** 3,
  has_dcim: true,
  volume_id: "11111111-1111-1111-1111-111111111111",
};

// Same factory label as `canon`, different physical card — the collision that used to
// route one photographer's card into another card's album.
const canonTwin: RemovableDevice = {
  ...canon,
  mount_path: "/Volumes/CANON_EOS 1",
  volume_id: "22222222-2222-2222-2222-222222222222",
};

const untitled: RemovableDevice = {
  name: "Untitled",
  mount_path: "/Volumes/Untitled",
  total_space: 32 * 1024 ** 3,
  available_space: 8 * 1024 ** 3,
  has_dcim: true,
  volume_id: "33333333-3333-3333-3333-333333333333",
};

// A volume the platform could not identify (e.g. a FAT card with no volume UUID).
const anonymous: RemovableDevice = { ...canon, volume_id: null };

const rule: DeviceRule = {
  profileId: "p1",
  albumName: "2026 Weddings",
  keepFiles: false,
  stackRawJpeg: true,
  stackBurst: true,
  organization: "folder_name",
};

beforeEach(() => {
  localStorage.clear();
  deviceRulesState._reset();
});

describe("deviceKey", () => {
  it("keys cards by the OS volume id, so the label and the path can both change", () => {
    expect(deviceKey(canon)).toBe(`vol:${canon.volume_id}`);
    expect(deviceKey({ ...canon, name: "RENAMED", mount_path: "/Volumes/elsewhere" })).toBe(
      `vol:${canon.volume_id}`,
    );
  });

  it("gives two cards with the same label two different keys", () => {
    expect(deviceKey(canon)).not.toBe(deviceKey(canonTwin));
  });

  it("has no key at all for a volume the platform could not identify", () => {
    expect(deviceKey(anonymous)).toBeNull();
    expect(deviceKey({ ...canon, volume_id: undefined })).toBeNull();
    expect(deviceKey({ ...canon, volume_id: "   " })).toBeNull();
  });
});

describe("deviceRulesState", () => {
  it("returns null when no rule is saved", () => {
    expect(deviceRulesState.lookup(canon)).toBeNull();
  });

  it("saves, retrieves, and forgets a rule", () => {
    expect(deviceRulesState.saveRule(canon, rule)).toBe(true);
    expect(deviceRulesState.lookup(canon)).toEqual({ rule, needsConfirmation: false });

    deviceRulesState.removeRule(canon);
    expect(deviceRulesState.lookup(canon)).toBeNull();
  });

  it("does not hand one card's rule to a different card with the same label", () => {
    deviceRulesState.saveRule(canon, rule);
    expect(deviceRulesState.lookup(canonTwin)).toBeNull();
  });

  it("does not hand a rule to the next card that reuses the same mount path", () => {
    deviceRulesState.saveRule(untitled, rule);
    const swapped: RemovableDevice = {
      ...untitled,
      volume_id: "44444444-4444-4444-4444-444444444444",
    };
    expect(deviceRulesState.lookup(swapped)).toBeNull();
  });

  it("still finds a card's rule when it remounts at a different path", () => {
    deviceRulesState.saveRule(untitled, rule);
    const remounted: RemovableDevice = { ...untitled, mount_path: "/Volumes/Untitled 1" };
    expect(deviceRulesState.lookup(remounted)?.rule).toEqual(rule);
  });

  it("refuses to save a rule for a card with no stable identity", () => {
    expect(deviceRulesState.saveRule(anonymous, rule)).toBe(false);
    expect(get(deviceRulesState)).toEqual({});
    expect(deviceRulesState.lookup(anonymous)).toBeNull();
  });

  it("persists rules across a reload via localStorage", () => {
    deviceRulesState.saveRule(canon, rule);
    const raw = localStorage.getItem("immich-shuttle-device-rules");
    expect(raw).toBeTruthy();
    expect(JSON.parse(raw as string)[deviceKey(canon) as string]).toEqual(rule);
  });

  it("exposes the full map reactively for the UI", () => {
    deviceRulesState.saveRule(canon, rule);
    expect(get(deviceRulesState)[deviceKey(canon) as string]).toEqual(rule);
  });
});

describe("legacy label/mount rules", () => {
  it("reads a rule persisted under the old label key as needing confirmation", async () => {
    // The real upgrade path: rules written by the previous version are already on disk when
    // the store module first loads. The statically imported store read storage at suite
    // load, so only a module reload can exercise that boundary.
    localStorage.setItem(
      "immich-shuttle-device-rules",
      JSON.stringify({ "name:CANON_EOS": rule }),
    );
    vi.resetModules();
    const reloaded = await import("./device-rules");

    expect(reloaded.deviceRulesState.lookup(canon)).toEqual({ rule, needsConfirmation: true });
  });

  it("marks a legacy label rule as needing confirmation", () => {
    deviceRulesState._reset({ "name:CANON_EOS": rule });
    expect(deviceRulesState.lookup(canon)).toEqual({ rule, needsConfirmation: true });
  });

  it("marks a legacy mount rule as needing confirmation", () => {
    deviceRulesState._reset({ "mount:/Volumes/Untitled": rule });
    expect(deviceRulesState.lookup(untitled)).toEqual({ rule, needsConfirmation: true });
  });

  it("never offers a legacy rule to a card with no stable identity", () => {
    deviceRulesState._reset({ "name:CANON_EOS": rule });
    expect(deviceRulesState.lookup(anonymous)).toBeNull();
  });

  it("prefers the confirmed rule over a stale legacy entry for the same card", () => {
    const confirmed: DeviceRule = { ...rule, keepFiles: true, albumName: "Confirmed" };
    deviceRulesState._reset({ "name:CANON_EOS": rule, [deviceKey(canon) as string]: confirmed });
    expect(deviceRulesState.lookup(canon)).toEqual({ rule: confirmed, needsConfirmation: false });
  });

  it("migrating a confirmed legacy rule re-keys it and drops the ambiguous entry", () => {
    deviceRulesState._reset({ "name:CANON_EOS": rule });
    const confirmed: DeviceRule = { ...rule, keepFiles: true };

    expect(deviceRulesState.migrateLegacyRule(canon, confirmed)).toBe(true);

    expect(deviceRulesState.lookup(canon)).toEqual({ rule: confirmed, needsConfirmation: false });
    expect(get(deviceRulesState)["name:CANON_EOS"]).toBeUndefined();
    // The twin shared the label; once the entry is migrated it inherits nothing.
    expect(deviceRulesState.lookup(canonTwin)).toBeNull();
  });

  it("refuses to migrate a legacy rule onto a card with no stable identity", () => {
    deviceRulesState._reset({ "name:CANON_EOS": rule });
    expect(deviceRulesState.migrateLegacyRule(anonymous, rule)).toBe(false);
    expect(get(deviceRulesState)).toEqual({ "name:CANON_EOS": rule });
  });

  it("forgetting a card also drops its ambiguous legacy entry", () => {
    deviceRulesState._reset({ "name:CANON_EOS": rule });
    deviceRulesState.removeRule(canon);
    expect(deviceRulesState.lookup(canon)).toBeNull();
    expect(get(deviceRulesState)).toEqual({});
  });
});
