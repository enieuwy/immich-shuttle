import { describe, expect, it, vi } from "vitest";

const { listenMock } = vi.hoisted(() => ({ listenMock: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

vi.mock("$lib/api", () => ({
  importListJobs: vi.fn(async () => []),
  importStart: vi.fn(async () => "job-1"),
  importCancel: vi.fn(async () => undefined),
  importRetry: vi.fn(async () => "job-2"),
  importDismiss: vi.fn(async () => []),
  importClearFinished: vi.fn(async () => []),
  importConfirmWipe: vi.fn(async () => ({
    id: "job-1",
    status: "completed",
    progress: { total: 0, uploaded: 0, duplicates: 0, errors: 0 },
    awaiting_wipe_confirmation: false,
    pending_wipe_count: 0,
  })),
  profileUpsert: vi.fn(async (input) => ({
    id: input.id ?? "p1",
    display_name: input.display_name ?? "Ellis",
    server_url: input.server_url,
    lan_server_url: input.lan_server_url ?? null,
    wan_server_url: input.wan_server_url ?? null,
  })),
  scanSources: vi.fn(async () => ({
    files: [],
    total_size_bytes: 0,
    photo_count: 0,
    video_count: 0,
    skipped_unreadable: 0,
  })),
  devicesListRemovable: vi.fn(async () => []),
  albumsList: vi.fn(async () => [{ id: "a1", album_name: "Trip", shared_with: [] }]),
  usersList: vi.fn(async () => []),
}));

import * as api from "$lib/api";
import { queueState } from "./queue";
import { profilesState } from "./profiles";
import { sourceState } from "./source";
import { albumsState } from "./albums";
import { importOptionsState } from "./import-options";
import { get } from "svelte/store";
import type { ImportJob } from "$lib/types";

describe("queueState", () => {
  it("rejects startImport when profile/source not set", async () => {
    await expect(queueState.startImport()).rejects.toThrow("Select a profile before starting import");
  });

  it("forwards stack flags to importStart", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");
    await sourceState.selectSources(["/Volumes/SD/DCIM"]);

    await queueState.startImport();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload).toBeDefined();
    expect(typeof payload?.stack_raw_jpeg).toBe("boolean");
    expect(typeof payload?.stack_burst).toBe("boolean");
    expect(payload).toHaveProperty("date_range");
    expect(payload?.date_range).toBeNull();
    expect(payload).toHaveProperty("concurrent_tasks");
    expect(payload?.concurrent_tasks).toBeNull();
    expect(payload).toMatchObject({
      profile_id: "p1",
      stack_raw_jpeg: true,
      stack_burst: true,
      date_range: null,
      concurrent_tasks: null,
    });
  });

  it("forwards a valid date range and rejects an invalid one", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");
    await sourceState.selectSources(["/Volumes/SD/DCIM"]);

    importOptionsState.setDateFrom("2026-01-01");
    importOptionsState.setDateTo("2026-01-31");
    await queueState.startImport();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]?.date_range).toBe("2026-01-01,2026-01-31");

    // An inverted range must block the import instead of being treated as no filter.
    importOptionsState.setDateFrom("2026-02-01");
    importOptionsState.setDateTo("2026-01-01");
    await expect(queueState.startImport()).rejects.toThrow(
      "The start date must be on or before the end date.",
    );

    importOptionsState.clearDateRange();
  });

  it("confirmWipe forwards args to importConfirmWipe", async () => {
    await queueState.confirmWipe("job-1", true);
    expect(vi.mocked(api.importConfirmWipe)).toHaveBeenCalledWith("job-1", true);
  });

  it("dismiss removes a job and replaces state.jobs with the returned list", async () => {
    const remaining: ImportJob[] = [
      {
        id: "job-2",
        status: "running",
        progress: { total: 4, uploaded: 1, duplicates: 0, errors: 0 },
        awaiting_wipe_confirmation: false,
        pending_wipe_count: 0,
        file_errors: [],
        profile_id: "p1",
      },
    ];
    vi.mocked(api.importDismiss).mockResolvedValueOnce(remaining);

    await queueState.dismiss("job-1");

    expect(vi.mocked(api.importDismiss)).toHaveBeenCalledWith("job-1");
    expect(get(queueState).jobs).toEqual(remaining);
  });

  it("clearFinished drops finished jobs and updates state.jobs", async () => {
    vi.mocked(api.importClearFinished).mockResolvedValueOnce([]);

    await queueState.clearFinished();

    expect(vi.mocked(api.importClearFinished)).toHaveBeenCalled();
    expect(get(queueState).jobs).toEqual([]);
  });

  it("retry re-runs the import for the given job id", async () => {
    await queueState.retry("job-1");

    expect(vi.mocked(api.importRetry)).toHaveBeenCalledWith("job-1");
  });

  it("forwards selectFiles override as select_files to importStart", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");
    await sourceState.selectSources(["/Volumes/SD/DCIM"]);

    await queueState.startImport({ selectFiles: ["/Volumes/SD/DCIM/IMG_1.JPG"] });

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload?.select_files).toEqual(["/Volumes/SD/DCIM/IMG_1.JPG"]);
  });

  it("resolves the selected album id to a name in into_album", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");
    await sourceState.selectSources(["/Volumes/SD/DCIM"]);
    await albumsState.loadAlbums();
    albumsState.selectAlbum("a1");

    await queueState.startImport();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload?.album_ids).toEqual(["a1"]);
    expect(payload?.into_album).toBe("Trip");
  });

  it("defaults organization to single_album", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");
    await sourceState.selectSources(["/Volumes/SD/DCIM"]);

    await queueState.startImport();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload?.organization).toBe("single_album");
  });

  it("forwards the selected folder organization mode to importStart", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");
    await sourceState.selectSources(["/Volumes/SD/DCIM"]);

    importOptionsState.setOrganization("folder_path");
    await queueState.startImport();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]?.organization).toBe("folder_path");

    importOptionsState.setOrganization("folder_tags");
    await queueState.startImport();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]?.organization).toBe("folder_tags");

    importOptionsState.setOrganization("single_album");
  });

  it("honors profileId, intoAlbum, and stack overrides (device rules)", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    await profilesState.saveProfile({
      id: "p2",
      display_name: "Family",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    await queueState.startImport({
      sourcePaths: ["/Volumes/SD/DCIM"],
      profileId: "p2",
      intoAlbum: "Family",
      albumIds: [],
      keepFiles: false,
      stackRawJpeg: false,
      stackBurst: true,
      organization: "folder_name",
    });

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload).toMatchObject({
      profile_id: "p2",
      into_album: "Family",
      keep_files: false,
      stack_raw_jpeg: false,
      stack_burst: true,
      organization: "folder_name",
    });
  });

  it("does not revive a terminal job when a late progress event arrives", async () => {
    const completed: ImportJob = {
      id: "job-completed",
      status: "completed",
      progress: { total: 4, uploaded: 4, duplicates: 0, errors: 0 },
      error: null,
      summary: "Imported 4 items.",
      awaiting_wipe_confirmation: false,
      pending_wipe_count: 0,
      file_errors: [],
      profile_id: "p1",
    };
    vi.mocked(api.importListJobs).mockResolvedValueOnce([completed]);
    await queueState.loadJobs();

    let onProgress:
      | ((event: { payload: { job_id: string; progress: ImportJob["progress"] } }) => void)
      | undefined;
    const unlisten = vi.fn();
    listenMock.mockImplementationOnce(
      async (
        _event: string,
        handler: (event: { payload: { job_id: string; progress: ImportJob["progress"] } }) => void,
      ) => {
        onProgress = handler;
        return unlisten;
      },
    );
    queueState.startPolling();

    onProgress?.({
      payload: {
        job_id: completed.id,
        progress: { total: 4, uploaded: 3, duplicates: 0, errors: 0 },
      },
    });
    expect(get(queueState).jobs).toEqual([completed]);

    queueState.stopPolling();
    await Promise.resolve();
  });

  it("tears down the progress listener when stopped mid-registration", async () => {
    const unlisten = vi.fn();
    const { promise, resolve } = Promise.withResolvers<() => void>();
    listenMock.mockReturnValueOnce(promise);

    queueState.startPolling();
    queueState.stopPolling();
    // Registration resolves only after polling was already stopped.
    resolve(unlisten);
    await promise;
    await Promise.resolve();

    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});
