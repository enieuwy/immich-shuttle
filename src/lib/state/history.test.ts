import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("$lib/api", () => ({
  historyList: vi.fn(async () => []),
  historyClear: vi.fn(async () => undefined),
  profilesList: vi.fn(async () => []),
  profileUpsert: vi.fn(async (input) => ({
    id: input.id ?? "p1",
    display_name: input.display_name ?? "Ellis",
    server_url: input.server_url,
    lan_server_url: input.lan_server_url ?? null,
    wan_server_url: input.wan_server_url ?? null,
  })),
  profileDelete: vi.fn(async () => undefined),
  profileValidate: vi.fn(async () => ({
    user_name: "Ellis",
    server_version: "1.120.0",
    is_compatible: true,
    warning: null,
  })),
  albumsList: vi.fn(async () => []),
  usersList: vi.fn(async () => []),
  albumCreate: vi.fn(async () => ({ id: "a2", album_name: "New", shared_with: [] })),
  albumShareUsers: vi.fn(async () => undefined),
  albumShareLink: vi.fn(async () => ({ url: "https://example.com/share/x" })),
  devicesListRemovable: vi.fn(async () => []),
  scanSourcesStream: vi.fn(async () => ({
    status: "complete" as const,
    total_size_bytes: 0,
    photo_count: 0,
    video_count: 0,
    skipped_unreadable: 0,
  })),
  scanCancel: vi.fn(async () => undefined),
}));

// scanSelectedSources (driven transitively by replayImport -> sourceState.selectSources)
// subscribes to the "scan-progress" event; a real listener is irrelevant here.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

import * as api from "$lib/api";
import type { Album, ImportInput, ImportRecord, ScanSummary } from "$lib/types";
import { profilesState, activeProfile } from "./profiles";
import { albumsState } from "./albums";
import { sourceState } from "./source";
import { errorsState } from "./errors";
import { importOptionsState } from "./import-options";
import { historyState, replayImport } from "./history";

function importRequest(overrides: Partial<ImportInput> = {}): ImportInput {
  return {
    profile_id: "p1",
    source_paths: [],
    album_ids: [],
    keep_files: true,
    stack_raw_jpeg: false,
    stack_burst: false,
    date_range: null,
    concurrent_tasks: null,
    ...overrides,
  };
}

function importRecord(id: string, requestOverrides: Partial<ImportInput> = {}): ImportRecord {
  const request = importRequest({ profile_id: "p1", ...requestOverrides });
  return {
    id,
    started_at: 0,
    finished_at: 0,
    profile_id: request.profile_id,
    source_paths: request.source_paths,
    album_ids: request.album_ids,
    status: "completed",
    total: 1,
    uploaded: 1,
    duplicates: 0,
    errors: 0,
    request,
  };
}

async function saveProfile(id: string, serverUrl: string) {
  await profilesState.saveProfile({
    id,
    display_name: id,
    server_url: serverUrl,
    api_key: null,
    lan_server_url: null,
    wan_server_url: null,
  });
}

describe("historyState", () => {
  it("does not let an in-flight loadHistory resurrect records a clear already deleted", async () => {
    const gate = Promise.withResolvers<ImportRecord[]>();
    vi.mocked(api.historyList).mockReturnValueOnce(gate.promise);

    const load = historyState.loadHistory();
    await historyState.clearHistory();
    expect(get(historyState).records).toEqual([]);

    // The stale load resolves only now, after Clear has already run.
    gate.resolve([importRecord("stale")]);
    await load;

    expect(get(historyState).records).toEqual([]);
    expect(get(historyState).loading).toBe(false);
  });
});

describe("replayImport", () => {
  it("refuses a second replay while the first is still staging, and only the first commits", async () => {
    await saveProfile("p1", "https://one.example.com");
    await saveProfile("p2", "https://two.example.com");

    const gate = Promise.withResolvers<Album[]>();
    vi.mocked(api.albumsList).mockReturnValueOnce(gate.promise);

    const record1 = importRecord("r1", { profile_id: "p1", source_paths: ["/a"] });
    const record2 = importRecord("r2", { profile_id: "p2", source_paths: ["/b"] });

    const first = replayImport(record1);
    const second = replayImport(record2);

    // The second call must short-circuit synchronously on the `replaying`
    // flag -- it never even reaches loadAlbums for record2's profile.
    expect(await second).toBe("busy");
    expect(get(historyState).replaying).toBe(true);
    expect(get(historyState).replayingRecordId).toBe("r1");

    gate.resolve([]);
    expect(await first).toBe("staged");

    expect(api.albumsList).toHaveBeenCalledTimes(1);
    expect(api.scanSourcesStream).toHaveBeenCalledTimes(1);
    expect(get(activeProfile)?.id).toBe("p1");
    expect(get(sourceState).selectedPaths).toEqual(["/a"]);
    expect(get(historyState).replaying).toBe(false);
    expect(get(historyState).replayingRecordId).toBeNull();
  });

  it("reports a failed album restore through errorsState, resolves, and clears replaying", async () => {
    await saveProfile("p1", "https://one.example.com");

    vi.mocked(api.albumsList).mockRejectedValueOnce(new Error("boom"));

    const record = importRecord("r1", {
      profile_id: "p1",
      source_paths: ["/a"],
      album_ids: ["missing-album"],
    });

    const outcome = await replayImport(record);

    expect(outcome).toBe("staged");
    expect(get(historyState).replaying).toBe(false);
    expect(get(historyState).replayingRecordId).toBeNull();
    // Source restoration still ran despite the album failure -- documented
    // partial-application behavior, not a full abort.
    expect(get(sourceState).selectedPaths).toEqual(["/a"]);

    const errors = get(errorsState);
    expect(errors.some((e) => /album/i.test(e.message))).toBe(true);
  });

  it("falls back to the recorded album name when the recorded id no longer resolves", async () => {
    await saveProfile("p1", "https://one.example.com");

    // The album was deleted and recreated, so its id changed. immich-go targets
    // albums by name, so the destination the user chose still exists.
    vi.mocked(api.albumsList).mockResolvedValueOnce([
      { id: "recreated-album", album_name: "Current", shared_with: [] },
    ]);

    const record = importRecord("stale-id-record", {
      album_ids: ["deleted-album"],
      into_album: "Current",
    });
    const errorCountBefore = get(errorsState).length;

    expect(await replayImport(record)).toBe("staged");
    expect(get(albumsState).selectedAlbumIds).toEqual(["recreated-album"]);
    expect(get(errorsState).slice(errorCountBefore)).toHaveLength(0);
  });

  it("reports an unresolvable recorded album instead of staging into the library", async () => {
    await saveProfile("p1", "https://one.example.com");

    vi.mocked(api.albumsList).mockResolvedValueOnce([
      { id: "unrelated-album", album_name: "Unrelated", shared_with: [] },
    ]);

    const record = importRecord("gone-album-record", {
      album_ids: ["deleted-album"],
      into_album: "Holiday",
    });
    const errorCountBefore = get(errorsState).length;

    expect(await replayImport(record)).toBe("staged");
    // No selection: startImport would turn the stale id into `into_album: null`
    // and upload into the library without telling the user.
    expect(get(albumsState).selectedAlbumIds).toEqual([]);
    const addedErrors = get(errorsState).slice(errorCountBefore);
    expect(addedErrors).toHaveLength(1);
    expect(addedErrors[0]?.message).toMatch(/couldn't restore.*"Holiday".*no longer exists/i);
  });

  it("restores a resolvable album id and an album matched by name", async () => {
    await saveProfile("p1", "https://one.example.com");

    vi.mocked(api.albumsList)
      .mockResolvedValueOnce([{ id: "recorded-album", album_name: "Recorded", shared_with: [] }])
      .mockResolvedValueOnce([{ id: "named-album", album_name: "Named", shared_with: [] }]);

    expect(
      await replayImport(
        importRecord("resolvable-album-record", {
          album_ids: ["recorded-album"],
          into_album: "Named",
        }),
      ),
    ).toBe("staged");
    expect(get(albumsState).selectedAlbumIds).toEqual(["recorded-album"]);

    expect(
      await replayImport(
        importRecord("named-album-record", {
          album_ids: [],
          into_album: "Named",
        }),
      ),
    ).toBe("staged");
    expect(get(albumsState).selectedAlbumIds).toEqual(["named-album"]);
  });

  it("replays where the run landed, not where the picker pointed", async () => {
    await saveProfile("p1", "https://one.example.com");

    // The record's own album_ids hold the id resolved from the album NAME after
    // the run finished; the persisted request holds only what the picker sent
    // beforehand. When they disagree -- the album was recreated mid-run, so the
    // name now names a different id -- the replay must follow the destination.
    const record = importRecord("landed-elsewhere", {
      album_ids: ["picker-album"],
      into_album: "Holiday",
    });
    record.album_ids = ["landed-album"];

    vi.mocked(api.albumsList).mockResolvedValueOnce([
      { id: "picker-album", album_name: "Stale Holiday", shared_with: [] },
      { id: "landed-album", album_name: "Holiday", shared_with: [] },
    ]);

    expect(await replayImport(record)).toBe("staged");
    expect(get(albumsState).selectedAlbumIds).toEqual(["landed-album"]);
  });

  it("abandons the album selection if the active profile changes while albums are loading", async () => {
    await saveProfile("p1", "https://one.example.com");
    await saveProfile("p2", "https://two.example.com");

    const gate = Promise.withResolvers<Album[]>();
    vi.mocked(api.albumsList).mockReturnValueOnce(gate.promise);

    const record = importRecord("r1", {
      profile_id: "p1",
      source_paths: ["/a"],
      album_ids: ["stale-album"],
    });

    const replay = replayImport(record);

    // Simulate the user switching profiles via ProfileSelector while
    // replayImport is suspended awaiting p1's albums -- the real component
    // stays interactive for the whole replay.
    profilesState.setActiveProfile("p2");

    // Resolves with an album that only makes sense for p1; if the profile
    // check were missing (or only re-checked a dead generation counter, as
    // before this fix), this id would land in p2's selectedAlbumIds.
    gate.resolve([{ id: "stale-album", album_name: "Stale", shared_with: [] }]);

    const outcome = await replay;

    expect(outcome).toBe("staged");
    expect(get(activeProfile)?.id).toBe("p2");
    expect(get(albumsState).selectedAlbumIds).not.toContain("stale-album");
    expect(get(albumsState).selectedAlbumIds).toEqual([]);
    expect(get(historyState).replaying).toBe(false);
    expect(get(historyState).replayingRecordId).toBeNull();

    const errors = get(errorsState);
    expect(errors.some((e) => /profile changed/i.test(e.message))).toBe(true);
  });

  it("abandons the staged source if the active profile changes while it is scanning", async () => {
    await saveProfile("p1", "https://one.example.com");
    await saveProfile("p2", "https://two.example.com");

    // Block the source scan so the profile can be switched while it is in
    // flight -- the scan is the last await of the replay and ProfileSelector
    // stays interactive for all of it.
    const gate = Promise.withResolvers<ScanSummary>();
    let scanStarted = false;
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(() => {
      scanStarted = true;
      return gate.promise;
    });

    const record = importRecord("r1", { profile_id: "p1", source_paths: ["/old-card"] });
    const errorCountBefore = get(errorsState).length;

    const replay = replayImport(record);
    await vi.waitFor(() => expect(scanStarted).toBe(true));

    profilesState.setActiveProfile("p2");
    gate.resolve({
      status: "complete",
      photo_count: 1,
      video_count: 0,
      total_size_bytes: 10,
      skipped_unreadable: 0,
    });

    expect(await replay).toBe("staged");
    expect(get(activeProfile)?.id).toBe("p2");
    // The record's source must not stay committed under the new profile: the
    // album subscription clears album state on a switch but never the source,
    // so a surviving commit would offer the old run's source and options as
    // though they had been reviewed for p2.
    expect(get(sourceState).selectedPaths).toEqual([]);
    expect(get(sourceState).scanResult).toBeNull();
    expect(
      get(errorsState)
        .slice(errorCountBefore)
        .some((e) => /profile changed/i.test(e.message)),
    ).toBe(true);
  });

  it("leaves a source the user established after the switch alone when it abandons", async () => {
    await saveProfile("p1", "https://one.example.com");
    await saveProfile("p2", "https://two.example.com");

    const gate = Promise.withResolvers<ScanSummary>();
    let scanStarted = false;
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(() => {
      scanStarted = true;
      return gate.promise;
    });

    const record = importRecord("r1", { profile_id: "p1", source_paths: ["/old-card"] });
    const errorCountBefore = get(errorsState).length;

    const replay = replayImport(record);
    await vi.waitFor(() => expect(scanStarted).toBe(true));

    // The user switches profiles and then picks a fresh card for it. That
    // selection supersedes the replay's scan and owns the source from here on.
    profilesState.setActiveProfile("p2");
    sourceState.clearSource();
    await sourceState.selectSources(["/new-card"]);

    gate.resolve({
      status: "complete",
      photo_count: 1,
      video_count: 0,
      total_size_bytes: 10,
      skipped_unreadable: 0,
    });

    expect(await replay).toBe("staged");
    expect(
      get(errorsState)
        .slice(errorCountBefore)
        .some((e) => /profile changed/i.test(e.message)),
    ).toBe(true);
    // The abandonment cleanup is scoped to the replay's own source commit, so
    // it must not wipe the newer one the user made while the scan was pending.
    expect(get(sourceState).selectedPaths).toEqual(["/new-card"]);
    expect(get(sourceState).scanResult).not.toBeNull();
  });

  it("disarms the options it hydrated when a replay is abandoned", async () => {
    await saveProfile("p1", "https://one.example.com");
    await saveProfile("p2", "https://two.example.com");

    // What the user has set up for themselves before pressing "Import again".
    importOptionsState.setKeepFiles(true);
    importOptionsState.setTags(["mine"]);
    const optionsBefore = get(importOptionsState);

    const gate = Promise.withResolvers<Album[]>();
    vi.mocked(api.albumsList).mockReturnValueOnce(gate.promise);

    // The record ran with delete-after-import armed.
    const record = importRecord("r1", {
      profile_id: "p1",
      source_paths: ["/old-card"],
      keep_files: false,
      tags: ["from-the-record"],
    });

    const replay = replayImport(record);
    // hydrateFromRequest runs synchronously before the first await, so the
    // record's options are already in force here -- this is the state the
    // abandonment has to undo.
    expect(get(importOptionsState).keepFiles).toBe(false);

    profilesState.setActiveProfile("p2");
    gate.resolve([]);
    expect(await replay).toBe("staged");

    // keepFiles: false is delete-after-import. Left armed under p2, the next
    // Start Import the user reviews for p2 would wipe their card.
    expect(get(importOptionsState).keepFiles).toBe(true);
    expect(get(importOptionsState).tags).toEqual(["mine"]);
    expect(get(importOptionsState)).toEqual(optionsBefore);
  });

  it("leaves options the user changed after the switch alone when it abandons", async () => {
    await saveProfile("p1", "https://one.example.com");
    await saveProfile("p2", "https://two.example.com");

    importOptionsState.setKeepFiles(true);
    importOptionsState.setOverwrite(false);

    const gate = Promise.withResolvers<ScanSummary>();
    let scanStarted = false;
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(() => {
      scanStarted = true;
      return gate.promise;
    });

    const record = importRecord("r1", {
      profile_id: "p1",
      source_paths: ["/old-card"],
      keep_files: false,
    });

    const replay = replayImport(record);
    await vi.waitFor(() => expect(scanStarted).toBe(true));

    // The user switches profiles and then edits the options themselves. From
    // here the options are theirs, so the replay's cleanup owns nothing --
    // exactly the discipline clearSourceIfUnchanged applies to the source.
    profilesState.setActiveProfile("p2");
    importOptionsState.setOverwrite(true);

    gate.resolve({
      status: "complete",
      photo_count: 1,
      video_count: 0,
      total_size_bytes: 10,
      skipped_unreadable: 0,
    });
    expect(await replay).toBe("staged");

    expect(get(importOptionsState).overwrite).toBe(true);
  });
});
