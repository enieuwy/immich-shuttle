import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

import * as api from "$lib/api";

vi.mock("$lib/api", () => ({
  devicesListRemovable: vi.fn(async () => []),
  scanSources: vi.fn(async () => ({
    files: [],
    total_size_bytes: 1024,
    photo_count: 1,
    video_count: 0,
    skipped_unreadable: 0,
  })),
}));

import { sourceState } from "./source";

beforeEach(() => {
  vi.clearAllMocks();
  sourceState.clearSource();
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
    expect(vi.mocked(api.scanSources)).toHaveBeenLastCalledWith(["/tmp/photos", "/tmp/videos"]);
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
    expect(vi.mocked(api.scanSources)).toHaveBeenLastCalledWith(["/b"]);
    await sourceState.removePath("/b");
    state = get(sourceState);
    expect(state.selectedPaths).toEqual([]);
    expect(state.scanResult).toBeNull();
    expect(vi.mocked(api.scanSources)).toHaveBeenCalledTimes(2);
  });

  it("drops a stale scan result when a newer selection supersedes it", async () => {
    const stale = {
      files: [],
      total_size_bytes: 1,
      photo_count: 99,
      video_count: 0,
      skipped_unreadable: 0,
    };
    const fresh = {
      files: [],
      total_size_bytes: 2,
      photo_count: 1,
      video_count: 0,
      skipped_unreadable: 0,
    };
    let call = 0;
    const gate = Promise.withResolvers<void>();
    vi.mocked(api.scanSources).mockImplementation(async () => {
      call += 1;
      // The first (superseded) scan blocks until released, so it resolves LAST.
      if (call === 1) {
        await gate.promise;
        return stale;
      }
      return fresh;
    });

    const first = sourceState.selectSources(["/a"]);
    const second = sourceState.selectSources(["/b"]);
    await second;
    gate.resolve();
    await first;

    const state = get(sourceState);
    expect(state.scanResult?.photo_count).toBe(1);
    expect(state.scanning).toBe(false);
  });
});
