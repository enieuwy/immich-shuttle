import type { ThumbResult } from "$lib/types";

/** Backend call that produces thumbnails for a batch, echoing each `path` back. */
export type ThumbnailFetch = (paths: string[]) => Promise<ThumbResult[]>;

export interface ThumbnailLoaderOptions {
  /** Backend call that renders thumbnails for a batch of paths. */
  fetch: ThumbnailFetch;
  /** Invoked with each chunk's results the moment that chunk resolves. */
  onResults: (results: ThumbResult[]) => void;
  /**
   * Files per backend request. Small chunks paint incrementally and top-down so
   * one slow file (RAW/video) only delays its own chunk, not the whole viewport;
   * larger chunks trade that for fewer round-trips. Default 4.
   */
  chunkSize?: number;
  /** Window (ms) over which burst requests are coalesced. Default 80. */
  debounceMs?: number;
}

/** Lazy, ordered, incremental thumbnail loader handle. */
export interface ThumbnailLoader {
  /** Request a thumbnail for `path`; a no-op if already requested or loaded. */
  request(path: string): void;
  /** Cancel timers and stop emitting; subsequent requests are ignored. */
  dispose(): void;
}

/**
 * Drives lazy thumbnail loading for the preview grid.
 *
 * Requests are coalesced over `debounceMs`, then drained in `chunkSize`-sized
 * chunks in request order by a single in-flight loop — `onResults` fires per
 * chunk as it resolves. Sequencing the chunks keeps backend concurrency bounded
 * to one request at a time and makes tiles paint incrementally, top-down,
 * instead of the whole viewport waiting on the slowest file.
 */
export function createThumbnailLoader(options: ThumbnailLoaderOptions): ThumbnailLoader {
  const chunkSize = Math.max(1, options.chunkSize ?? 4);
  const debounceMs = options.debounceMs ?? 80;
  // Paths already claimed (in-flight or loaded); never fetched twice.
  const requested = new Set<string>();
  // Pending paths in request order (top-down as tiles intersect).
  let queue: string[] = [];
  let debounce: number | undefined;
  let draining = false;
  let disposed = false;

  function request(path: string): void {
    if (disposed || requested.has(path)) return;
    requested.add(path);
    queue.push(path);
    clearTimeout(debounce);
    debounce = setTimeout(drain, debounceMs);
  }

  async function drain(): Promise<void> {
    debounce = undefined;
    if (draining) return;
    draining = true;
    try {
      while (queue.length > 0 && !disposed) {
        const chunk = queue.splice(0, chunkSize);
        let results: ThumbResult[];
        try {
          results = await options.fetch(chunk);
        } catch {
          // Release the chunk so a later intersection can retry it.
          for (const path of chunk) requested.delete(path);
          continue;
        }
        const returnedPaths = new Set(results.map((result) => result.path));
        // The backend can omit failed/unsupported paths without rejecting the
        // batch. Release those claims so their next intersection can retry.
        for (const path of chunk) {
          if (!returnedPaths.has(path)) requested.delete(path);
        }
        if (disposed) return;
        options.onResults(results);
      }
    } finally {
      draining = false;
    }
  }

  function dispose(): void {
    disposed = true;
    clearTimeout(debounce);
    debounce = undefined;
    queue = [];
  }

  return { request, dispose };
}
