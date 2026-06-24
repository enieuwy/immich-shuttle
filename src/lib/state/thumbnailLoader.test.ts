import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createThumbnailLoader } from "./thumbnailLoader";
import type { ThumbResult } from "$lib/types";

function thumb(path: string): ThumbResult {
  return { path, data_url: `data:${path}`, width: 1, height: 1 };
}

/** A fetch whose every call is resolved manually, in arbitrary order. */
function gatedFetch() {
  const calls: string[][] = [];
  const gates: Array<() => void> = [];
  let inFlight = 0;
  let maxInFlight = 0;
  const fetch = (paths: string[]): Promise<ThumbResult[]> => {
    calls.push(paths);
    inFlight++;
    maxInFlight = Math.max(maxInFlight, inFlight);
    return new Promise<ThumbResult[]>((resolve, reject) => {
      gates.push(() => {
        inFlight--;
        resolve(paths.map(thumb));
      });
      // Stash a rejecter alongside the resolver via the same index.
      rejecters.push(() => {
        inFlight--;
        reject(new Error("boom"));
      });
    });
  };
  const rejecters: Array<() => void> = [];
  return {
    fetch,
    calls,
    resolve: (i: number) => gates[i](),
    reject: (i: number) => rejecters[i](),
    get maxInFlight() {
      return maxInFlight;
    },
  };
}

describe("createThumbnailLoader", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("coalesces burst requests and drains them in request order, one fetch per chunk", async () => {
    const calls: string[][] = [];
    const painted: string[][] = [];
    const loader = createThumbnailLoader({
      fetch: async (paths) => {
        calls.push(paths);
        return paths.map(thumb);
      },
      onResults: (rs) => painted.push(rs.map((r) => r.path)),
      chunkSize: 2,
      debounceMs: 10,
    });

    for (const p of ["a", "b", "c", "d", "e"]) loader.request(p);
    await vi.advanceTimersByTimeAsync(10);
    for (let i = 0; i < 20; i++) await Promise.resolve();

    // One fetch per chunk, in order — not one all-or-nothing batch.
    expect(calls).toEqual([["a", "b"], ["c", "d"], ["e"]]);
    // Each chunk painted incrementally as it resolved.
    expect(painted).toEqual([["a", "b"], ["c", "d"], ["e"]]);
  });

  it("never requests the same path twice", async () => {
    const calls: string[][] = [];
    const loader = createThumbnailLoader({
      fetch: async (paths) => {
        calls.push(paths);
        return paths.map(thumb);
      },
      onResults: () => {},
      chunkSize: 10,
      debounceMs: 10,
    });

    loader.request("a");
    loader.request("a");
    loader.request("b");
    await vi.advanceTimersByTimeAsync(10);
    for (let i = 0; i < 10; i++) await Promise.resolve();

    expect(calls).toEqual([["a", "b"]]);
  });

  it("runs a single fetch at a time and picks up requests that arrive mid-drain", async () => {
    const g = gatedFetch();
    const loader = createThumbnailLoader({
      fetch: g.fetch,
      onResults: () => {},
      chunkSize: 1,
      debounceMs: 5,
    });

    loader.request("a");
    loader.request("b");
    await vi.advanceTimersByTimeAsync(5); // debounce fires → fetch("a") in flight
    expect(g.calls).toEqual([["a"]]);

    // A request arriving while a drain is in flight must not start a 2nd fetch.
    loader.request("c");
    await vi.advanceTimersByTimeAsync(5);
    expect(g.calls).toEqual([["a"]]);

    g.resolve(0); // "a" done → drain continues to "b"
    await vi.advanceTimersByTimeAsync(0);
    expect(g.calls).toEqual([["a"], ["b"]]);

    g.resolve(1); // "b" done → "c"
    await vi.advanceTimersByTimeAsync(0);
    expect(g.calls).toEqual([["a"], ["b"], ["c"]]);

    g.resolve(2);
    await vi.advanceTimersByTimeAsync(0);
    expect(g.maxInFlight).toBe(1);
  });

  it("a slow chunk does not block an earlier chunk from painting", async () => {
    const g = gatedFetch();
    const painted: string[] = [];
    const loader = createThumbnailLoader({
      fetch: g.fetch,
      onResults: (rs) => painted.push(...rs.map((r) => r.path)),
      chunkSize: 1,
      debounceMs: 5,
    });

    loader.request("fast");
    loader.request("slow");
    await vi.advanceTimersByTimeAsync(5);

    g.resolve(0); // "fast" resolves first
    await vi.advanceTimersByTimeAsync(0);
    // "fast" painted even though "slow" is still in flight.
    expect(painted).toEqual(["fast"]);
    expect(g.calls).toEqual([["fast"], ["slow"]]);

    g.resolve(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(painted).toEqual(["fast", "slow"]);
  });

  it("releases a failed chunk for retry and still drains the rest", async () => {
    const g = gatedFetch();
    const painted: string[] = [];
    const loader = createThumbnailLoader({
      fetch: g.fetch,
      onResults: (rs) => painted.push(...rs.map((r) => r.path)),
      chunkSize: 1,
      debounceMs: 5,
    });

    loader.request("a");
    loader.request("b");
    await vi.advanceTimersByTimeAsync(5);

    g.reject(0); // "a" fails
    await vi.advanceTimersByTimeAsync(0);
    g.resolve(1); // "b" still succeeds
    await vi.advanceTimersByTimeAsync(0);
    expect(painted).toEqual(["b"]);

    // "a" was released, so a later intersection re-requests it.
    loader.request("a");
    await vi.advanceTimersByTimeAsync(5);
    expect(g.calls).toEqual([["a"], ["b"], ["a"]]);
  });

  it("dispose halts draining and ignores later requests", async () => {
    const g = gatedFetch();
    const painted: string[] = [];
    const loader = createThumbnailLoader({
      fetch: g.fetch,
      onResults: (rs) => painted.push(...rs.map((r) => r.path)),
      chunkSize: 1,
      debounceMs: 5,
    });

    loader.request("a");
    loader.request("b");
    await vi.advanceTimersByTimeAsync(5); // fetch("a") in flight
    loader.dispose();

    g.resolve(0); // "a" resolves after dispose → must not paint or advance
    await vi.advanceTimersByTimeAsync(5);
    expect(painted).toEqual([]);
    expect(g.calls).toEqual([["a"]]);

    loader.request("c"); // ignored after dispose
    await vi.advanceTimersByTimeAsync(5);
    expect(g.calls).toEqual([["a"]]);
  });
});
