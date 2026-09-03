import { describe, expect, it, vi } from "vitest";

const { listenMock } = vi.hoisted(() => ({ listenMock: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(async () => true),
  requestPermission: vi.fn(async () => "granted"),
  sendNotification: vi.fn(),
}));

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
  historySourceLastImport: vi.fn(async () => null),
  historyList: vi.fn(async () => []),
  historyClear: vi.fn(async () => undefined),
}));

import * as api from "$lib/api";
import { handleTerminalTransitions, queueState, selectNewlyTerminal } from "./queue";
import { sendNotification } from "@tauri-apps/plugin-notification";
import { profilesState } from "./profiles";
import { sourceState } from "./source";
import { albumsState } from "./albums";
import { historyState } from "./history";
import { errorsState } from "./errors";
import { importOptionsState } from "./import-options";
import { get } from "svelte/store";
import { BackendError } from "$lib/backendErrors";
import type { ImportJob } from "$lib/types";

type TestProfile = Parameters<typeof profilesState.saveProfile>[0];

const saveTestProfile = (id: string, overrides: Partial<TestProfile> = {}) =>
  profilesState.saveProfile({
    id,
    display_name: "Ellis",
    server_url: "https://immich.example.com",
    api_key: null,
    lan_server_url: null,
    wan_server_url: null,
    ...overrides,
  });

// Most cases start from the same ground: one saved profile, made active, with
// one selected source. This stays an explicit per-test call rather than a
// beforeEach hook because these cases share module state deliberately — later
// cases lean on profiles that earlier ones saved, and on the absence of a
// profile in the very first case.
const activateProfileWithSource = async (id = "p1") => {
  await saveTestProfile(id);
  profilesState.setActiveProfile(id);
  await sourceState.selectSources(["/Volumes/SD/DCIM"]);
};

describe("queueState", () => {
  it("rejects startImport when profile/source not set", async () => {
    await expect(queueState.startImport()).rejects.toThrow("Select a profile before starting import");
  });

  it("reports an in-flight start and removes it after settling", async () => {
    await activateProfileWithSource();

    const start = Promise.withResolvers<string>();
    vi.mocked(api.importStart).mockReturnValueOnce(start.promise);
    const importPromise = queueState.startImport();

    expect(queueState.pendingStarts()).toHaveLength(1);
    start.resolve("job-1");
    await importPromise;
    expect(queueState.pendingStarts()).toEqual([]);
  });

  it("removes a rejected start from shutdown tracking", async () => {
    await activateProfileWithSource();

    const start = Promise.withResolvers<string>();
    vi.mocked(api.importStart).mockReturnValueOnce(start.promise);
    const importPromise = queueState.startImport();

    expect(queueState.pendingStarts()).toHaveLength(1);
    start.reject(new Error("start failed"));
    await expect(importPromise).rejects.toThrow("start failed");
    expect(queueState.pendingStarts()).toEqual([]);
  });

  it("forwards stack flags to importStart", async () => {
    await activateProfileWithSource();

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
    await activateProfileWithSource();

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

  it("forwards error-resilience and tag options to importStart", async () => {
    await activateProfileWithSource();

    importOptionsState.setKeepGoingOnErrors(true);
    importOptionsState.setOverwrite(true);
    importOptionsState.setTags(["Trip/Iceland", "client-a"]);
    importOptionsState.setSessionTag(true);
    await queueState.startImport();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload).toMatchObject({
      on_errors: "continue",
      overwrite: true,
      tags: ["Trip/Iceland", "client-a"],
      session_tag: true,
    });

    // Off -> immich-go default (stop): send null, not "continue".
    importOptionsState.setKeepGoingOnErrors(false);
    importOptionsState.setOverwrite(false);
    importOptionsState.setTags([]);
    importOptionsState.setSessionTag(false);
    await queueState.startImport();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]?.on_errors).toBeNull();
  });

  it("derives a date-range floor from last import when only-new is on", async () => {
    await activateProfileWithSource();
    importOptionsState.clearDateRange();
    importOptionsState.setOnlyNewSinceLastImport(true);

    // Local midday so the local-calendar-zone floor is 2026-03-15 in any TZ,
    // paired with a far-future bound (immich-go rejects an open-ended "floor,").
    vi.mocked(api.historySourceLastImport).mockResolvedValueOnce(
      new Date(2026, 2, 15, 12, 0, 0).getTime(),
    );
    await queueState.startImport();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]?.date_range).toBe("2026-03-15,9999-12-31");
    // Checkpoint lookup is scoped to the active profile.
    expect(vi.mocked(api.historySourceLastImport)).toHaveBeenLastCalledWith("p1", [
      "/Volumes/SD/DCIM",
    ]);

    // No stored last-import -> no floor (import everything).
    vi.mocked(api.historySourceLastImport).mockResolvedValueOnce(null);
    await queueState.startImport();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]?.date_range).toBeNull();

    importOptionsState.setOnlyNewSinceLastImport(false);
  });

  it("refuses the import when the only-new checkpoint cannot be read", async () => {
    await activateProfileWithSource();
    importOptionsState.clearDateRange();
    importOptionsState.setOnlyNewSinceLastImport(true);

    // An unreadable store used to arrive as `null`, which is also the first-run
    // answer: the floor silently vanished and the whole card was re-uploaded.
    // The requested incremental window must never be widened without saying so.
    const callsBefore = vi.mocked(api.importStart).mock.calls.length;
    vi.mocked(api.historySourceLastImport).mockRejectedValueOnce(
      new Error("history_source_last_import failed: Could not read store at /tmp/store.json"),
    );

    await expect(queueState.startImport()).rejects.toThrow(
      'Could not read the last-import checkpoint for this source, so "only new since last import" cannot be applied.',
    );
    expect(vi.mocked(api.importStart).mock.calls.length).toBe(callsBefore);

    importOptionsState.setOnlyNewSinceLastImport(false);
  });

  it("treats an explicit selection as the import: coarse filters do not compose", async () => {
    await activateProfileWithSource();

    // Coarse filters set (as History replay would), but a hand-picked selection
    // is provided: the selection must win, and type/date/include/exclude must
    // NOT also filter it, or picked files would be silently dropped. The preview
    // grid never filters by extension, so a selection really can hold a file the
    // durable excludes would otherwise veto.
    importOptionsState.setMediaType("image");
    importOptionsState.setDateFrom("2026-01-01");
    importOptionsState.setDateTo("2026-01-31");
    importOptionsState.setIncludeExtensions([".mp4"]);
    importOptionsState.setExcludeExtensions([".mov"]);
    importOptionsState.setOnlyNewSinceLastImport(true);

    await queueState.startImport({ selectFiles: ["/Volumes/SD/DCIM/a.mov"] });
    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload?.select_files).toEqual(["/Volumes/SD/DCIM/a.mov"]);
    expect(payload?.include_type).toBeNull();
    expect(payload?.date_range).toBeNull();
    expect(payload?.include_extensions).toEqual([]);
    expect(payload?.exclude_extensions).toEqual([]);

    importOptionsState.setMediaType("all");
    importOptionsState.setIncludeExtensions([]);
    importOptionsState.setExcludeExtensions([]);
    importOptionsState.setOnlyNewSinceLastImport(false);
    importOptionsState.clearDateRange();
  });

  it("confirmWipe forwards args to importConfirmWipe", async () => {
    await queueState.confirmWipe("job-1", true);
    expect(vi.mocked(api.importConfirmWipe)).toHaveBeenCalledWith("job-1", true);
  });

  it("shows the backend's own reason when a retry is refused", async () => {
    // Admission refuses for the moment a cancelled run finishes writing history,
    // and that sentence tells the user to try again; a generic message would not.
    const reason = "An import is still finishing; try again in a moment.";
    vi.mocked(api.importRetry).mockRejectedValueOnce(new BackendError("import_retry", reason));

    await queueState.retry("job-1");

    const shown = get(errorsState).find((error) => error.message === reason);
    expect(shown, "the refusal reason must reach the user").toBeDefined();
    errorsState.dismissError(shown!.id);
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
    await activateProfileWithSource();

    await queueState.startImport({ selectFiles: ["/Volumes/SD/DCIM/IMG_1.JPG"] });

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload?.select_files).toEqual(["/Volumes/SD/DCIM/IMG_1.JPG"]);
  });

  it("resolves the selected album id to a name in into_album", async () => {
    await activateProfileWithSource();
    await albumsState.loadAlbums();
    albumsState.selectAlbum("a1");

    await queueState.startImport();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload?.album_ids).toEqual(["a1"]);
    expect(payload?.into_album).toBe("Trip");
  });

  it("defaults organization to single_album", async () => {
    await activateProfileWithSource();

    await queueState.startImport();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload?.organization).toBe("single_album");
  });

  it("forwards the selected folder organization mode to importStart", async () => {
    await activateProfileWithSource();

    importOptionsState.setOrganization("folder_path");
    await queueState.startImport();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]?.organization).toBe("folder_path");

    importOptionsState.setOrganization("folder_tags");
    await queueState.startImport();
    expect(vi.mocked(api.importStart).mock.lastCall?.[0]?.organization).toBe("folder_tags");

    importOptionsState.setOrganization("single_album");
  });

  it("honors profileId, intoAlbum, and stack overrides (device rules)", async () => {
    await saveTestProfile("p1");
    await saveTestProfile("p2", { display_name: "Family" });
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

  it("does not import into another profile's album selection", async () => {
    await activateProfileWithSource();
    await albumsState.loadAlbums();
    albumsState.selectAlbum("a1");

    // Switch profiles: albumsState may synchronously clear on the switch (a
    // sibling concern) or may not have reloaded yet — either way
    // loadedProfileId no longer equals the new profile's id, so this is the
    // hard gate startImport must enforce regardless of that timing.
    await saveTestProfile("p2", { display_name: "Family" });
    profilesState.setActiveProfile("p2");

    await queueState.startImport();

    const payload = vi.mocked(api.importStart).mock.lastCall?.[0];
    expect(payload?.album_ids).toEqual([]);
    expect(payload?.into_album).toBeNull();

    profilesState.setActiveProfile("p1");
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

  it("retries the progress listener after a failed registration", async () => {
    // A rejected registration used to stay in the pending slot forever, so every
    // later attempt was refused and live progress was dead for the session.
    listenMock
      .mockRejectedValueOnce(new Error("event channel unavailable"))
      .mockResolvedValueOnce(vi.fn());

    queueState.startPolling();
    await Promise.resolve();
    await Promise.resolve();
    queueState.stopPolling();

    const attemptsBefore = listenMock.mock.calls.length;
    queueState.startPolling();
    await Promise.resolve();
    queueState.stopPolling();
    await Promise.resolve();

    expect(listenMock.mock.calls.length).toBe(attemptsBefore + 1);
  });

  it("out-of-order refresh: a newer poll snapshot wins over a stale, later-resolving older poll", async () => {
    const older = Promise.withResolvers<ImportJob[]>();
    const newer = Promise.withResolvers<ImportJob[]>();
    vi.mocked(api.importListJobs).mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);

    const runningStale: ImportJob = {
      id: "job-race",
      status: "running",
      progress: { total: 4, uploaded: 1, duplicates: 0, errors: 0 },
      awaiting_wipe_confirmation: false,
      pending_wipe_count: 0,
      file_errors: [],
      profile_id: "p1",
    };
    const completedFresh: ImportJob = {
      id: "job-race",
      status: "completed",
      progress: { total: 4, uploaded: 4, duplicates: 0, errors: 0 },
      error: null,
      summary: "Imported 4 items.",
      awaiting_wipe_confirmation: false,
      pending_wipe_count: 0,
      file_errors: [],
      profile_id: "p1",
    };

    // The older poll (issued first) stalls; the newer poll (issued second)
    // resolves first, as a slow IPC round-trip can cause in practice.
    const stale = queueState.loadJobs();
    const fresh = queueState.loadJobs();

    newer.resolve([completedFresh]);
    await fresh;
    expect(get(queueState).jobs).toEqual([completedFresh]);

    // The stale response finally arrives — without the generation guard this
    // would regress the completed job back to running.
    older.resolve([runningStale]);
    await stale;

    expect(get(queueState).jobs).toEqual([completedFresh]);
  });
  it("a poll that resolves after a dismiss cannot resurrect the dismissed job", async () => {
    // This pins dismissed cards flickering back into the queue and repeat
    // completion notifications after a stale poll resolves.
    const dismissed: ImportJob = {
      id: "job-dismiss-race",
      status: "completed",
      progress: { total: 4, uploaded: 4, duplicates: 0, errors: 0 },
      error: null,
      summary: "Imported 4 items.",
      awaiting_wipe_confirmation: false,
      pending_wipe_count: 0,
      file_errors: [],
      profile_id: "p1",
    };
    const poll = Promise.withResolvers<ImportJob[]>();
    vi.mocked(api.importListJobs).mockReturnValueOnce(poll.promise);
    vi.mocked(api.importDismiss).mockResolvedValueOnce([]);

    const refresh = queueState.loadJobs();
    await queueState.dismiss(dismissed.id);
    poll.resolve([dismissed]);
    await refresh;

    expect(get(queueState).jobs.some((job) => job.id === dismissed.id)).toBe(false);
  });

  it("a poll that resolves after clearFinished cannot resurrect cleared jobs", async () => {
    // This pins cleared cards flickering back into the queue and repeat
    // completion notifications after a stale poll resolves.
    const cleared: ImportJob = {
      id: "job-clear-race",
      status: "completed",
      progress: { total: 4, uploaded: 4, duplicates: 0, errors: 0 },
      error: null,
      summary: "Imported 4 items.",
      awaiting_wipe_confirmation: false,
      pending_wipe_count: 0,
      file_errors: [],
      profile_id: "p1",
    };
    const poll = Promise.withResolvers<ImportJob[]>();
    vi.mocked(api.importListJobs).mockReturnValueOnce(poll.promise);
    vi.mocked(api.importClearFinished).mockResolvedValueOnce([]);

    const refresh = queueState.loadJobs();
    await queueState.clearFinished();
    poll.resolve([cleared]);
    await refresh;

    expect(get(queueState).jobs.some((job) => job.id === cleared.id)).toBe(false);
  });

  it("a poll begun during a dismiss cannot resurrect the dismissed job", async () => {
    // The invalidate before the request cannot reach a poll that starts while it
    // is in flight: that poll is current, and it can read the backend list before
    // the dismiss lands and commit it afterwards.
    const dismissed: ImportJob = {
      id: "job-dismiss-midflight",
      status: "completed",
      progress: { total: 4, uploaded: 4, duplicates: 0, errors: 0 },
      error: null,
      summary: "Imported 4 items.",
      awaiting_wipe_confirmation: false,
      pending_wipe_count: 0,
      file_errors: [],
      profile_id: "p1",
    };
    const poll = Promise.withResolvers<ImportJob[]>();
    const dismiss = Promise.withResolvers<ImportJob[]>();
    vi.mocked(api.importListJobs).mockReturnValueOnce(poll.promise);
    vi.mocked(api.importDismiss).mockReturnValueOnce(dismiss.promise);

    const pendingDismiss = queueState.dismiss(dismissed.id);
    // Begins after dismiss's invalidate, while importDismiss is still in flight,
    // so it holds a backend list from before the dismiss landed.
    const refresh = queueState.loadJobs();
    dismiss.resolve([]);
    await pendingDismiss;
    // The stale snapshot arrives only now, after the queue was emptied.
    poll.resolve([dismissed]);
    await refresh;

    expect(get(queueState).jobs.some((job) => job.id === dismissed.id)).toBe(false);
  });

  it("bumps the source checkpoint version only on a completed transition", async () => {
    const running: ImportJob = {
      id: "job-checkpoint",
      status: "running",
      progress: { total: 4, uploaded: 1, duplicates: 0, errors: 0 },
      awaiting_wipe_confirmation: false,
      pending_wipe_count: 0,
      file_errors: [],
      profile_id: "p1",
    };
    const completed: ImportJob = {
      ...running,
      status: "completed",
      progress: { total: 4, uploaded: 4, duplicates: 0, errors: 0 },
      error: null,
      summary: "Imported 4 items.",
    };

    vi.mocked(api.importListJobs).mockResolvedValueOnce([running]);
    await queueState.loadJobs();
    const before = get(historyState).lastImportVersion;

    vi.mocked(api.importListJobs).mockResolvedValueOnce([completed]);
    await queueState.loadJobs();
    expect(get(historyState).lastImportVersion).toBe(before + 1);

    vi.mocked(api.importListJobs).mockResolvedValueOnce([completed]);
    await queueState.loadJobs();
    expect(get(historyState).lastImportVersion).toBe(before + 1);
  });

  it("keeps repeated queue refresh failures to one active error", async () => {
    for (const error of get(errorsState)) {
      errorsState.dismissError(error.id);
    }

    vi.mocked(api.importListJobs)
      .mockRejectedValueOnce(new Error("network down"))
      .mockRejectedValueOnce(new Error("network still down"))
      .mockResolvedValueOnce([])
      .mockRejectedValueOnce(new Error("network down again"))
      .mockRejectedValueOnce(new Error("network down after dismissal"));

    await queueState.loadJobs();
    await queueState.loadJobs();
    expect(
      get(errorsState).filter((error) => error.message === "Could not refresh import queue."),
    ).toHaveLength(1);

    // A successful poll retires the message: it no longer describes reality,
    // and leaving it active would suppress the next outage's error.
    await queueState.loadJobs();
    expect(
      get(errorsState).filter((error) => error.message === "Could not refresh import queue."),
    ).toHaveLength(0);

    await queueState.loadJobs();
    expect(
      get(errorsState).filter((error) => error.message === "Could not refresh import queue."),
    ).toHaveLength(1);

    const queueError = get(errorsState).find(
      (error) => error.message === "Could not refresh import queue.",
    );
    expect(queueError).toBeDefined();
    errorsState.dismissError(queueError!.id);
    await queueState.loadJobs();
    expect(
      get(errorsState).filter((error) => error.message === "Could not refresh import queue."),
    ).toHaveLength(1);
  });
});

const job = (id: string, status: ImportJob["status"]): ImportJob =>
  ({
    id,
    status,
    progress: { total: 1, uploaded: 1, duplicates: 0, errors: 0 },
    awaiting_wipe_confirmation: false,
    pending_wipe_count: 0,
  }) as ImportJob;

describe("selectNewlyTerminal", () => {
  it("detects running -> completed and running -> failed", () => {
    const prev = [job("a", "running"), job("b", "running")];
    const next = [job("a", "completed"), job("b", "failed")];
    expect(selectNewlyTerminal(prev, next).map((j) => j.id)).toEqual(["a", "b"]);
  });

  it("ignores cancellations", () => {
    const prev = [job("a", "running")];
    const next = [job("a", "cancelled")];
    expect(selectNewlyTerminal(prev, next)).toEqual([]);
  });

  it("ignores unseen jobs already terminal (initial hydration / restart)", () => {
    const next = [job("a", "completed"), job("b", "failed")];
    expect(selectNewlyTerminal([], next)).toEqual([]);
  });

  it("does not re-notify a job already terminal", () => {
    const prev = [job("a", "completed")];
    const next = [job("a", "completed")];
    expect(selectNewlyTerminal(prev, next)).toEqual([]);
  });
});

describe("handleTerminalTransitions", () => {
  it("routes a rejecting notification backend into the catch", async () => {
    // The send used to be fired without `await`, so the try/catch around it
    // could only ever see a synchronous throw. A rejecting notification backend
    // escaped it entirely, and the call site is `void handleTerminalTransitions`
    // — so the rejection became the unhandled rejection the catch promises to
    // prevent. Awaiting the send makes an async rejection behave exactly like a
    // synchronous throw: it reaches the catch, which abandons the rest of the
    // best-effort batch. The second job's send NOT firing is what proves the
    // rejection was handled here rather than leaked.
    // `failed` jobs are used so no history checkpoint version is bumped.
    vi.mocked(sendNotification).mockReset();
    vi.mocked(sendNotification).mockImplementationOnce(
      () => Promise.reject(new Error("notification backend unavailable")) as never,
    );

    await expect(
      handleTerminalTransitions(
        [job("notify-a", "running"), job("notify-b", "running")],
        [job("notify-a", "failed"), job("notify-b", "failed")],
      ),
    ).resolves.toBeUndefined();

    expect(vi.mocked(sendNotification).mock.calls).toEqual([
      [{ title: "Import failed", body: "The import did not finish. Check the logs for details." }],
    ]);
  });
});
