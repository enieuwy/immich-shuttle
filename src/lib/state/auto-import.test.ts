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
import { deviceRulesState } from "./device-rules";
import { profilesState } from "./profiles";
import { sourceState } from "./source";
import type { RemovableDevice } from "$lib/types";

const card: RemovableDevice = {
  name: "CANON_EOS",
  mount_path: "/Volumes/CANON_EOS",
  total_space: 64 * 1024 ** 3,
  available_space: 12 * 1024 ** 3,
  has_dcim: true,
};

const thumbDrive: RemovableDevice = {
  name: "Untitled",
  mount_path: "/Volumes/Untitled",
  total_space: 256 * 1024 ** 3,
  available_space: 240 * 1024 ** 3,
  has_dcim: false,
};

async function withActiveProfile() {
  await profilesState.saveProfile({
    id: "p1",
    display_name: "Test",
    server_url: "https://immich.example.com",
    api_key: null,
    lan_server_url: null,
    wan_server_url: null,
  });
  profilesState.setActiveProfile("p1");
}

beforeEach(async () => {
  vi.clearAllMocks();
  localStorage.clear();
  sourceState.clearSource();
  autoImportState._reset();
  deviceRulesState._reset();
  await withActiveProfile();
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
    const card2: RemovableDevice = { ...card, name: "SONY", mount_path: "/Volumes/SONY" };
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
    deviceRulesState.saveRule(card, {
      profileId: "p2",
      albumName: "Family",
      keepFiles: false,
      stackRawJpeg: false,
      stackBurst: true,
      organization: "folder_path",
    });
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([card]);

    expect(get(autoImportState).candidateRule?.profileId).toBe("p2");
  });

  it("accept replays a saved rule's profile, album, and wipe policy", async () => {
    deviceRulesState.saveRule(card, {
      profileId: "p2",
      albumName: "Family",
      keepFiles: false,
      stackRawJpeg: false,
      stackBurst: true,
      organization: "folder_path",
    });
    // p2 must exist for the profileId override to resolve.
    await profilesState.saveProfile({
      id: "p2",
      display_name: "Family",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

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
});
