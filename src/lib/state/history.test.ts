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
import type { Album, ImportInput, ImportRecord } from "$lib/types";
import { profilesState, activeProfile } from "./profiles";
import { sourceState } from "./source";
import { errorsState } from "./errors";
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
});
