import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

import * as api from "$lib/api";
import type { MediaFile, RemovableDevice, ScanProgress, ScanSummary } from "$lib/types";

type ProgressListener = (event: { payload: ScanProgress }) => void;

// The streamed scan subscribes to `scan-progress`; capture the callback so
// tests can invoke it directly to simulate late or misrouted batches.
let progressListener: ProgressListener | undefined;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, callback: ProgressListener) => {
    progressListener = callback;
    return () => {};
  }),
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

import { sourceState } from "./source";
import { selectionState } from "./selection";

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
});
