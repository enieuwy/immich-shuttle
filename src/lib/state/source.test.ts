import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";

import * as api from "$lib/api";
import type { MediaFile, RemovableDevice, ScanProgress, ScanSummary } from "$lib/types";

type ProgressListener = (event: { payload: ScanProgress }) => void;

// The streamed scan subscribes to `scan-progress`; capture the callback so
// tests can invoke it directly to simulate late or misrouted batches.

type ForecastCancelApi = {
  forecastCancel(generation: number): Promise<void>;
};
let progressListener: ProgressListener | undefined;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, callback: ProgressListener) => {
    progressListener = callback;
    return () => {};
  }),
}));


vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("$lib/api", () => ({
  devicesListRemovable: vi.fn(async () => []),
  scanSourcesStream: vi.fn(async () => ({
    status: "complete",
    photo_count: 1,
    video_count: 0,
    total_size_bytes: 1024,
    skipped_unreadable: 0,
  })),
  scanCancel: vi.fn(async () => {}),
}));

import { importOptionsForSource, sourceState } from "./source";
import { selectionState } from "./selection";
import { errorsState } from "./errors";

function mediaFile(path: string): MediaFile {
  return { path, name: path, extension: "jpg", size_bytes: 1, is_video: false };
}

function device(name: string): RemovableDevice {
  return { name, mount_path: `/Volumes/${name}`, total_space: 1, available_space: 1, has_dcim: true };
}

beforeEach(() => {
  vi.clearAllMocks();
  progressListener = undefined;
  sourceState.clearSource();
  selectionState.clear();
});

describe("sourceState", () => {
  it("selects sources and stores scan result", async () => {
    await sourceState.selectSources(["/tmp/photos"]);
    const state = get(sourceState);
    expect(state.selectedPaths).toContain("/tmp/photos");
    expect(state.scanResult?.photo_count).toBe(1);
  });

  it("does not duplicate a source path selected twice", async () => {
    await sourceState.selectSources(["/tmp/photos"]);
    await sourceState.selectSources(["/tmp/photos", "/tmp/videos"]);
    const state = get(sourceState);
    expect(state.selectedPaths).toEqual(["/tmp/photos", "/tmp/videos"]);
    expect(vi.mocked(api.scanSourcesStream)).toHaveBeenLastCalledWith(
      ["/tmp/photos", "/tmp/videos"],
      expect.any(String),
    );
  });

  it("loads removable devices", async () => {
    await sourceState.loadDevices();
    expect(vi.mocked(api.devicesListRemovable)).toHaveBeenCalled();
  });

  it("clears selected source", async () => {
    await sourceState.selectSources(["/tmp/photos"]);
    sourceState.clearSource();
    const state = get(sourceState);
    expect(state.selectedPaths).toEqual([]);
    expect(state.scanResult).toBeNull();
  });

  it("removes one selected source and clears when removing the last", async () => {
    await sourceState.selectSources(["/a", "/b"]);
    await sourceState.removePath("/a");
    let state = get(sourceState);
    expect(state.selectedPaths).toEqual(["/b"]);
    expect(vi.mocked(api.scanSourcesStream)).toHaveBeenLastCalledWith(["/b"], expect.any(String));
    await sourceState.removePath("/b");
    state = get(sourceState);
    expect(state.selectedPaths).toEqual([]);
    expect(state.scanResult).toBeNull();
    expect(vi.mocked(api.scanSourcesStream)).toHaveBeenCalledTimes(2);
  });

  it("ignores a scan-progress event carrying a different scan_id", async () => {
    // Block the terminal summary so a batch can be injected mid-scan, the
    // way a late emission from a just-cancelled scan would arrive.
    const gate = Promise.withResolvers<ScanSummary>();
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(() => gate.promise);

    const selecting = sourceState.selectSources(["/tmp/photos"]);
    // `listen` is awaited before `scanSourcesStream` is called, so by now the
    // listener is registered synchronously (no macrotask needed).
    expect(progressListener).toBeDefined();

    progressListener?.({
      payload: {
        scan_id: "not-our-scan",
        files: [mediaFile("/tmp/photos/intruder.jpg")],
        photo_count: 999,
        video_count: 999,
        total_size_bytes: 999999,
        skipped_unreadable: 999,
      },
    });

    // The foreign batch must not have touched the accumulator or the totals.
    expect(get(sourceState).scanResult?.files).toEqual([]);
    expect(get(sourceState).scanResult?.photo_count).toBe(0);

    gate.resolve({
      status: "complete",
      photo_count: 1,
      video_count: 0,
      total_size_bytes: 1024,
      skipped_unreadable: 0,
    });
    await selecting;

    const state = get(sourceState);
    expect(state.scanResult?.files).toEqual([]);
    expect(state.scanResult?.photo_count).toBe(1);
  });

  it("keeps the newer device snapshot when an older refresh resolves later", async () => {
    const older = Promise.withResolvers<RemovableDevice[]>();
    const newer = Promise.withResolvers<RemovableDevice[]>();
    vi.mocked(api.devicesListRemovable)
      .mockImplementationOnce(() => older.promise)
      .mockImplementationOnce(() => newer.promise);

    const firstLoad = sourceState.loadDevices();
    const secondLoad = sourceState.loadDevices();

    const newerDevices = [device("newer-device")];
    newer.resolve(newerDevices);
    await secondLoad;

    const olderDevices = [device("older-device")];
    older.resolve(olderDevices);
    await firstLoad;

    expect(get(sourceState).detectedDevices).toEqual(newerDevices);
  });

  it("clears the media selection when the last source is removed", async () => {
    await sourceState.selectSources(["/a"]);
    selectionState.selectOnly(["/a/1.jpg"]);
    await sourceState.removePath("/a");
    expect(selectionState.paths()).toEqual([]);
  });

  it("clears the media selection when the whole source is cleared", async () => {
    await sourceState.selectSources(["/Volumes/Untitled/DCIM"]);
    selectionState.selectOnly(["/Volumes/Untitled/DCIM/IMG_0001.JPG"]);

    sourceState.clearSource();

    expect(get(sourceState).selectedPaths).toEqual([]);
    expect(selectionState.paths()).toEqual([]);

    // A different card reusing the same mount name and DCIM filenames exposes
    // the identical absolute path. Selections are keyed by that path, so one
    // surviving the clear would come back active here and Start Import would
    // silently stage only the colliding files.
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(async (_paths, scanId) => {
      progressListener?.({
        payload: {
          scan_id: scanId,
          files: [mediaFile("/Volumes/Untitled/DCIM/IMG_0001.JPG")],
          photo_count: 1,
          video_count: 0,
          total_size_bytes: 10,
          skipped_unreadable: 0,
        },
      });
      return {
        status: "complete",
        photo_count: 1,
        video_count: 0,
        total_size_bytes: 10,
        skipped_unreadable: 0,
      };
    });

    await sourceState.selectSources(["/Volumes/Untitled/DCIM"]);

    expect(get(sourceState).scanResult?.files.map((file) => file.path)).toEqual([
      "/Volumes/Untitled/DCIM/IMG_0001.JPG",
    ]);
    expect(selectionState.paths()).toEqual([]);
  });

  it("retains only the surviving source's paths after removing one of two sources", async () => {
    await sourceState.selectSources(["/a", "/b"]);
    selectionState.selectOnly(["/a/1.jpg", "/b/1.jpg"]);

    vi.mocked(api.scanSourcesStream).mockImplementationOnce(async (_paths, scanId) => {
      // Use the real scanId the caller generated so the listener's
      // provenance check accepts this batch as belonging to the rescan.
      progressListener?.({
        payload: {
          scan_id: scanId,
          files: [mediaFile("/b/1.jpg")],
          photo_count: 1,
          video_count: 0,
          total_size_bytes: 10,
          skipped_unreadable: 0,
        },
      });
      return {
        status: "complete",
        photo_count: 1,
        video_count: 0,
        total_size_bytes: 10,
        skipped_unreadable: 0,
      };
    });

    await sourceState.removePath("/a");

    expect(selectionState.paths()).toEqual(["/b/1.jpg"]);
  });

  it("drops a stale scan result when a newer selection supersedes it", async () => {
    const stale = {
      status: "complete" as const,
      total_size_bytes: 1,
      photo_count: 99,
      video_count: 0,
      skipped_unreadable: 0,
    };
    const fresh = {
      status: "complete" as const,
      total_size_bytes: 2,
      photo_count: 1,
      video_count: 0,
      skipped_unreadable: 0,
    };
    let call = 0;
    const gate = Promise.withResolvers<void>();
    vi.mocked(api.scanSourcesStream).mockImplementation(async () => {
      call += 1;
      // The first (superseded) scan blocks in its stream call until released, so
      // it resolves LAST — after a newer selection has advanced the generation.
      if (call === 1) {
        await gate.promise;
        return stale;
      }
      return fresh;
    });

    const first = sourceState.selectSources(["/a"]);
    // Let the first scan pass its generation guard and reach the stream call
    // (blocked on the gate) before the superseding selection bumps generation.
    await vi.waitFor(() => expect(call).toBe(1));
    const second = sourceState.selectSources(["/b"]);
    await second;
    gate.resolve();
    await first;

    const state = get(sourceState);
    expect(state.scanResult?.photo_count).toBe(1);
    expect(state.scanning).toBe(false);
  });

  // A streamed scan that gives up reports the counts it reached, which is a
  // PREFIX of the source (media.rs). Committing that as a scan result let the
  // user tick "everything" in the preview grid and start an exact-selection
  // import that silently omitted every file past the point the walk stalled.
  it("refuses to commit the partial inventory of a timed-out scan", async () => {
    const gate = Promise.withResolvers<ScanSummary>();
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(async (_paths, scanId) => {
      progressListener?.({
        payload: {
          scan_id: scanId,
          files: [mediaFile("/card/DCIM/IMG_0001.JPG")],
          photo_count: 1,
          video_count: 0,
          total_size_bytes: 10,
          skipped_unreadable: 0,
        },
      });
      return gate.promise;
    });
    const errorCountBefore = get(errorsState).length;

    const selecting = sourceState.selectSources(["/card/DCIM"]);
    await vi.waitFor(() => expect(progressListener).toBeDefined());
    gate.resolve({
      status: "timed_out",
      photo_count: 1,
      video_count: 0,
      total_size_bytes: 10,
      skipped_unreadable: 0,
    });
    await selecting;

    const state = get(sourceState);
    expect(state.scanOutcome).toBe("timed_out");
    expect(state.scanning).toBe(false);
    // SourcePicker renders its "N photos / N videos" counts and the
    // "Preview & select" button from `scanResult` alone, so a null here is what
    // makes it impossible for the UI to present this as a finished scan.
    expect(state.scanResult).toBeNull();
    // A timeout is a failure, so it takes the same visible path as a scan that
    // threw: state.error plus a toast, because the card can be off screen.
    expect(state.error).toMatch(/took too long/i);
    expect(get(errorsState).slice(errorCountBefore)).toHaveLength(1);
    // Retry needs the sources it was scanning, so they stay selected.
    expect(state.selectedPaths).toEqual(["/card/DCIM"]);
  });

  it("rejects the whole-source Start Import fallback after a timed-out scan", async () => {
    vi.mocked(api.scanSourcesStream).mockResolvedValueOnce({
      status: "timed_out",
      photo_count: 0,
      video_count: 0,
      total_size_bytes: 0,
      skipped_unreadable: 0,
    });

    await sourceState.selectSources(["/card/DCIM"]);

    // App passes this result directly to queueState.startImport. Before the
    // admission check it converted an empty selection to `{}`, which imported
    // the entire incomplete source.
    expect(importOptionsForSource(get(sourceState), [])).toBeNull();
  });

  it("blocks an exact-selection import built from a timed-out scan", async () => {
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(async (_paths, scanId) => {
      progressListener?.({
        payload: {
          scan_id: scanId,
          files: [mediaFile("/card/DCIM/IMG_0001.JPG")],
          photo_count: 1,
          video_count: 0,
          total_size_bytes: 10,
          skipped_unreadable: 0,
        },
      });
      return {
        status: "timed_out",
        photo_count: 1,
        video_count: 0,
        total_size_bytes: 10,
        skipped_unreadable: 0,
      };
    });

    await sourceState.selectSources(["/card/DCIM"]);
    // The grid selects whatever the scan exposed; here it exposed nothing, and
    // a selection made before the scan cannot survive an inventory nobody can
    // vouch for either.
    selectionState.selectOnly(["/card/DCIM/IMG_0001.JPG"]);

    // App.svelte derives the exact list it hands to startImport this way.
    const files = get(sourceState).scanResult?.files ?? [];
    const valid = new Set(files.map((file) => file.path));
    expect(selectionState.paths().filter((path) => valid.has(path))).toEqual([]);
  });

  it("drops the partial inventory of a cancelled scan without reporting an error", async () => {
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(async (_paths, scanId) => {
      progressListener?.({
        payload: {
          scan_id: scanId,
          files: [mediaFile("/card/DCIM/IMG_0001.JPG")],
          photo_count: 1,
          video_count: 0,
          total_size_bytes: 10,
          skipped_unreadable: 0,
        },
      });
      return {
        status: "cancelled",
        photo_count: 1,
        video_count: 0,
        total_size_bytes: 10,
        skipped_unreadable: 0,
      };
    });
    selectionState.selectOnly(["/card/DCIM/IMG_0001.JPG"]);
    const errorCountBefore = get(errorsState).length;

    await sourceState.selectSources(["/card/DCIM"]);

    const state = get(sourceState);
    expect(state.scanOutcome).toBe("cancelled");
    expect(state.scanResult).toBeNull();
    // Pressing Cancel is a deliberate stop, not a failure: no error text, no
    // toast. The card says so instead.
    expect(state.error).toBeNull();
    expect(get(errorsState).slice(errorCountBefore)).toHaveLength(0);
    expect(selectionState.paths()).toEqual([]);
    expect(importOptionsForSource(state, [])).toBeNull();
  });

  it("rescans the same sources and commits once the scan completes", async () => {
    vi.mocked(api.scanSourcesStream).mockImplementationOnce(async () => ({
      status: "timed_out" as const,
      photo_count: 0,
      video_count: 0,
      total_size_bytes: 0,
      skipped_unreadable: 0,
    }));
    await sourceState.selectSources(["/card/DCIM"]);
    expect(get(sourceState).scanOutcome).toBe("timed_out");

    vi.mocked(api.scanSourcesStream).mockImplementationOnce(async (_paths, scanId) => {
      progressListener?.({
        payload: {
          scan_id: scanId,
          files: [mediaFile("/card/DCIM/IMG_0001.JPG")],
          photo_count: 1,
          video_count: 0,
          total_size_bytes: 10,
          skipped_unreadable: 0,
        },
      });
      return {
        status: "complete",
        photo_count: 1,
        video_count: 0,
        total_size_bytes: 10,
        skipped_unreadable: 0,
      };
    });

    await sourceState.rescan();

    // The retry re-scans exactly what was already selected; it must not go
    // through selectSources and re-derive a selection.
    expect(vi.mocked(api.scanSourcesStream)).toHaveBeenLastCalledWith(
      ["/card/DCIM"],
      expect.any(String),
    );
    const state = get(sourceState);
    expect(state.scanOutcome).toBe("complete");
    expect(state.scanResult?.files.map((file) => file.path)).toEqual([
      "/card/DCIM/IMG_0001.JPG",
    ]);
  });
});

describe("forecast cancellation generation", () => {
  it("keeps F2 active when delayed F1 cancellation reaches the backend", async () => {
    const releaseF1Cancel = Promise.withResolvers<void>();
    let activeGeneration = 1;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      expect(command).toBe("forecast_cancel");
      await releaseF1Cancel.promise;
      const generation = (args as { generation?: unknown } | undefined)?.generation;
      expect(generation).toBeTypeOf("number");
      if (typeof generation === "number" && activeGeneration === generation) activeGeneration = 0;
    });
    const actualApi = await vi.importActual<ForecastCancelApi>("$lib/api");

    // ImportPreflight gives F1 this value, then starts F2 before the delayed
    // F1 IPC request reaches the backend.
    const cancelF1 = actualApi.forecastCancel(1);
    activeGeneration = 2;
    releaseF1Cancel.resolve();
    await cancelF1;

    expect(activeGeneration).toBe(2);
    expect(invoke).toHaveBeenCalledWith("forecast_cancel", { generation: 1 });
  });
});

