import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";

import { deviceKey, deviceRulesState, type DeviceRule } from "./device-rules";
import type { RemovableDevice } from "$lib/types";

const canon: RemovableDevice = {
  name: "CANON_EOS",
  mount_path: "/Volumes/CANON_EOS",
  total_space: 64 * 1024 ** 3,
  available_space: 12 * 1024 ** 3,
  has_dcim: true,
};

const untitled: RemovableDevice = {
  name: "Untitled",
  mount_path: "/Volumes/Untitled",
  total_space: 32 * 1024 ** 3,
  available_space: 8 * 1024 ** 3,
  has_dcim: true,
};

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
  it("keys labeled cards by their volume label so the path can change", () => {
    expect(deviceKey(canon)).toBe("name:CANON_EOS");
    expect(deviceKey({ ...canon, mount_path: "/Volumes/CANON_EOS-1" })).toBe("name:CANON_EOS");
  });

  it("falls back to the mount path for unlabeled/Untitled volumes", () => {
    expect(deviceKey(untitled)).toBe("mount:/Volumes/Untitled");
    expect(deviceKey({ ...canon, name: "  " })).toBe("mount:/Volumes/CANON_EOS");
  });
});

describe("deviceRulesState", () => {
  it("returns null when no rule is saved", () => {
    expect(deviceRulesState.getRule(canon)).toBeNull();
  });

  it("saves, retrieves, and forgets a rule", () => {
    deviceRulesState.saveRule(canon, rule);
    expect(deviceRulesState.getRule(canon)).toEqual(rule);

    deviceRulesState.removeRule(canon);
    expect(deviceRulesState.getRule(canon)).toBeNull();
  });

  it("persists rules across a reload via localStorage", () => {
    deviceRulesState.saveRule(canon, rule);
    // Simulate a fresh store reading the same backing storage.
    const raw = localStorage.getItem("immich-shuttle-device-rules");
    expect(raw).toBeTruthy();
    expect(JSON.parse(raw as string)[deviceKey(canon)]).toEqual(rule);
  });

  it("exposes the full map reactively for the UI", () => {
    deviceRulesState.saveRule(canon, rule);
    expect(get(deviceRulesState)[deviceKey(canon)]).toEqual(rule);
  });
});
