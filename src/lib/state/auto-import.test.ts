import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("$lib/api", () => ({
  importStart: vi.fn(async () => "job-1"),
  importListJobs: vi.fn(async () => []),
  importCancel: vi.fn(async () => undefined),
  importRetry: vi.fn(async () => "job-2"),
  importDismiss: vi.fn(async () => []),
  importClearFinished: vi.fn(async () => []),
  importConfirmWipe: vi.fn(async () => ({})),
  profilesList: vi.fn(async () => []),
  profileUpsert: vi.fn(async (input) => ({
    id: input.id ?? "p1",
    display_name: input.display_name ?? "Test",
    server_url: input.server_url,
    lan_server_url: null,
    wan_server_url: null,
  })),
  profileDelete: vi.fn(async () => undefined),
  profileValidate: vi.fn(async () => ({})),
  scanSources: vi.fn(async () => ({
    files: [],
    total_size_bytes: 0,
    photo_count: 0,
    video_count: 0,
    skipped_unreadable: 0,
  })),
  devicesListRemovable: vi.fn(async () => []),
  albumsList: vi.fn(async () => []),
}));

import * as api from "$lib/api";
import { autoImportState } from "./auto-import";
import { deviceRulesState, type DeviceRule } from "./device-rules";
import { errorsState } from "./errors";
import { profilesState } from "./profiles";
import { queueState } from "./queue";
import { sourceState } from "./source";
import type { RemovableDevice } from "$lib/types";

const card: RemovableDevice = {
  name: "CANON_EOS",
  mount_path: "/Volumes/CANON_EOS",
  total_space: 64 * 1024 ** 3,
  available_space: 12 * 1024 ** 3,
  has_dcim: true,
  volume_id: "11111111-1111-1111-1111-111111111111",
};

const thumbDrive: RemovableDevice = {
  name: "Untitled",
  mount_path: "/Volumes/Untitled",
  total_space: 256 * 1024 ** 3,
  available_space: 240 * 1024 ** 3,
  has_dcim: false,
  volume_id: "99999999-9999-9999-9999-999999999999",
};

const savedRule: DeviceRule = {
  profileId: "p2",
  albumName: "Family",
  keepFiles: false,
  stackRawJpeg: false,
  stackBurst: true,
  organization: "folder_path",
};

// `p1` is the active profile; `p2` exists only so a saved rule's `profileId` override has
// something to resolve to.
async function withActiveProfile() {
  for (const id of ["p1", "p2"]) {
    await profilesState.saveProfile({
      id,
      display_name: id === "p1" ? "Test" : "Family",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
  }
  profilesState.setActiveProfile("p1");
}

beforeEach(async () => {
  vi.clearAllMocks();
  localStorage.clear();
  sourceState.clearSource();
  autoImportState._reset();
  deviceRulesState._reset();
  await withActiveProfile();
  vi.mocked(api.devicesListRemovable).mockResolvedValue([card]);
});

describe("autoImportState", () => {
  it("defaults to disabled and persists the toggle", () => {
    expect(get(autoImportState).enabled).toBe(false);
    autoImportState.setEnabled(true);
    expect(get(autoImportState).enabled).toBe(true);
    expect(localStorage.getItem("immich-shuttle-auto-import")).toBe("on");
  });

  it("does not prompt while disabled", () => {
    autoImportState.observe([]); // baseline
    autoImportState.observe([card]);
    expect(get(autoImportState).candidate).toBeNull();
  });

  it("does not prompt for cards already present at startup", () => {
    autoImportState.setEnabled(true);
    autoImportState.observe([card]); // baseline includes the card
    expect(get(autoImportState).candidate).toBeNull();
  });

  it("prompts when a DCIM card is inserted after baseline", () => {
    autoImportState.setEnabled(true);
    autoImportState.observe([]); // baseline empty
    autoImportState.observe([card]);
    expect(get(autoImportState).candidate?.mount_path).toBe(card.mount_path);
  });

  it("surfaces a second card inserted alongside the first once resolved", async () => {
    const card2: RemovableDevice = {
      ...card,
      name: "SONY",
      mount_path: "/Volumes/SONY",
      volume_id: "22222222-2222-2222-2222-222222222222",
    };
    autoImportState.setEnabled(true);
    autoImportState.observe([]); // baseline empty

    // Both cards appear in the same poll: only one can prompt at a time.
    autoImportState.observe([card, card2]);
    const first = get(autoImportState).candidate;
    expect(first).not.toBeNull();

    // Re-polling while the prompt is open must not silently consume the sibling.
    autoImportState.observe([card, card2]);

    // Resolve the first; the second must now surface, not stay suppressed.
    autoImportState.dismiss();
    autoImportState.observe([card, card2]);
    const second = get(autoImportState).candidate;
    expect(second).not.toBeNull();
    expect(second?.mount_path).not.toBe(first?.mount_path);
  });

  it("ignores inserted drives without a DCIM folder", () => {
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([thumbDrive]);
    expect(get(autoImportState).candidate).toBeNull();
  });

  it("does not prompt without an active profile", () => {
    profilesState.setActiveProfile("");
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);
    expect(get(autoImportState).candidate).toBeNull();
  });

  it("accept starts an import with keep-files forced and no albums", async () => {
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);

    await autoImportState.accept();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload).toMatchObject({
      profile_id: "p1",
      source_paths: [card.mount_path],
      album_ids: [],
      keep_files: true,
    });
    expect(get(autoImportState).candidate).toBeNull();
  });

  it("accept restores the candidate and raises a toast when starting fails", async () => {
    const rule = {
      profileId: "p1",
      albumName: null,
      keepFiles: true,
      stackRawJpeg: true,
      stackBurst: false,
      organization: "single_album" as const,
    };
    deviceRulesState.saveRule(card, rule);
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);
    vi.spyOn(queueState, "startImport").mockRejectedValueOnce(new Error("start failed"));

    await autoImportState.accept();

    expect(get(autoImportState).candidate).toEqual(card);
    expect(get(autoImportState).candidateRule).toEqual(rule);
    expect(get(errorsState)).toEqual(
      expect.arrayContaining([expect.objectContaining({ message: "start failed" })]),
    );
  });

  it("dismiss suppresses re-prompt until the card is re-inserted", () => {
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);
    autoImportState.dismiss();
    expect(get(autoImportState).candidate).toBeNull();

    // Still inserted: must not re-prompt.
    autoImportState.observe([card]);
    expect(get(autoImportState).candidate).toBeNull();

    // Ejected, then re-inserted: prompt again.
    autoImportState.observe([]);
    autoImportState.observe([card]);
    expect(get(autoImportState).candidate?.mount_path).toBe(card.mount_path);
  });

  it("disabling clears any pending candidate", () => {
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);
    expect(get(autoImportState).candidate).not.toBeNull();

    autoImportState.setEnabled(false);
    expect(get(autoImportState).candidate).toBeNull();
  });

  it("pre-fills the candidate rule when the inserted card has a saved rule", () => {
    deviceRulesState.saveRule(card, savedRule);
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);

    expect(get(autoImportState).candidateRule?.profileId).toBe("p2");
    expect(get(autoImportState).candidateRuleNeedsConfirmation).toBe(false);
  });

  it("accept replays a saved rule's profile, album, and wipe policy", async () => {
    deviceRulesState.saveRule(card, savedRule);
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);
    await autoImportState.accept();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload).toMatchObject({
      profile_id: "p2",
      source_paths: [card.mount_path],
      into_album: "Family",
      keep_files: false,
      stack_raw_jpeg: false,
      stack_burst: true,
      organization: "folder_path",
    });
  });

  it("does not offer one card's rule to a different card with the same label", () => {
    const twin: RemovableDevice = {
      ...card,
      mount_path: "/Volumes/CANON_EOS 1",
      volume_id: "22222222-2222-2222-2222-222222222222",
    };
    deviceRulesState.saveRule(card, savedRule);
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([twin]);

    expect(get(autoImportState).candidate?.mount_path).toBe(twin.mount_path);
    expect(get(autoImportState).candidateRule).toBeNull();
  });

  it("does not offer a rule to the next Untitled card that reuses the mount path", () => {
    const first: RemovableDevice = {
      ...card,
      name: "Untitled",
      mount_path: "/Volumes/Untitled",
      volume_id: "aaaaaaaa-0000-0000-0000-000000000001",
    };
    const second: RemovableDevice = {
      ...first,
      volume_id: "aaaaaaaa-0000-0000-0000-000000000002",
    };
    deviceRulesState.saveRule(first, savedRule);
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([first]);
    expect(get(autoImportState).candidateRule).toEqual(savedRule);

    // Card pulled, a different card inserted into the same slot.
    autoImportState.dismiss();
    autoImportState.observe([]);
    autoImportState.observe([second]);

    expect(get(autoImportState).candidate?.mount_path).toBe(second.mount_path);
    expect(get(autoImportState).candidateRule).toBeNull();
  });

  it("still finds a card's rule when it remounts at a different path", () => {
    deviceRulesState.saveRule(card, savedRule);
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([{ ...card, mount_path: "/Volumes/CANON_EOS 2" }]);

    expect(get(autoImportState).candidateRule).toEqual(savedRule);
  });

  it("does not auto-import a card when the platform could not identify it", () => {
    const anonymous: RemovableDevice = { ...card, volume_id: null };
    // A legacy entry that matches this card's label is present and must stay unused.
    deviceRulesState._reset({ "name:CANON_EOS": savedRule });
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([anonymous]);

    expect(get(autoImportState).candidate).toBeNull();
    expect(deviceRulesState.saveRule(anonymous, savedRule)).toBe(false);
  });

  it("re-prompts a replacement card at the same mount without the prior card's rule", async () => {
    const replacement: RemovableDevice = {
      ...card,
      volume_id: "22222222-2222-2222-2222-222222222222",
    };
    deviceRulesState.saveRule(card, savedRule);
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);
    expect(get(autoImportState).candidateRule).toEqual(savedRule);

    // The detector now observes a different card at the identical mount and capacity.
    autoImportState.observe([replacement]);
    expect(get(autoImportState).candidate).toEqual(replacement);
    expect(get(autoImportState).candidateRule).toBeNull();

    vi.mocked(api.devicesListRemovable).mockResolvedValue([replacement]);
    await autoImportState.accept();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]).toMatchObject({
      source_paths: [replacement.mount_path],
      profile_id: "p1",
      keep_files: true,
    });
  });

  it("rejects acceptance when a fresh device check finds a replacement card", async () => {
    const replacement: RemovableDevice = {
      ...card,
      volume_id: "22222222-2222-2222-2222-222222222222",
    };
    deviceRulesState._reset({ "name:CANON_EOS": savedRule });
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);

    // No new observation has arrived, so accept itself must still verify the mounted card.
    vi.mocked(api.devicesListRemovable).mockResolvedValue([replacement]);
    await autoImportState.accept();

    expect(vi.mocked(api.importStart)).not.toHaveBeenCalled();
    expect(get(autoImportState).candidate).toBeNull();
    expect(deviceRulesState.lookup(card)).toEqual({ rule: savedRule, needsConfirmation: true });
  });

  it("offers a legacy rule as unconfirmed and keeps originals without a fresh confirmation", async () => {
    deviceRulesState._reset({ "name:CANON_EOS": savedRule });
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);

    expect(get(autoImportState).candidateRule).toEqual(savedRule);
    expect(get(autoImportState).candidateRuleNeedsConfirmation).toBe(true);

    await autoImportState.accept();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    // The destination the user read on the banner is honoured; the delete policy is not.
    expect(payload).toMatchObject({ profile_id: "p2", into_album: "Family", keep_files: true });
    // The migrated rule records what was actually confirmed, so the next insert of this
    // card cannot resurrect the unconfirmed delete policy.
    expect(deviceRulesState.lookup(card)).toEqual({
      rule: { ...savedRule, keepFiles: true },
      needsConfirmation: false,
    });
  });

  it("applies a legacy rule's delete policy only when the user confirms it", async () => {
    deviceRulesState._reset({ "name:CANON_EOS": savedRule });
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);

    await autoImportState.accept(true);

    expect(vi.mocked(api.importStart).mock.lastCall?.[0]).toMatchObject({ keep_files: false });
    expect(deviceRulesState.lookup(card)).toEqual({ rule: savedRule, needsConfirmation: false });
  });

  it("does not migrate a legacy rule when the import fails to start", async () => {
    deviceRulesState._reset({ "name:CANON_EOS": savedRule });
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);
    vi.spyOn(queueState, "startImport").mockRejectedValueOnce(new Error("start failed"));

    await autoImportState.accept(true);

    expect(get(autoImportState).candidateRuleNeedsConfirmation).toBe(true);
    expect(deviceRulesState.lookup(card)).toEqual({ rule: savedRule, needsConfirmation: true });
  });
});
