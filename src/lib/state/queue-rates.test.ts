import { describe, expect, it } from "vitest";

import type { ImportJob, JobProgress } from "$lib/types";

import { recomputeRates } from "./queue";

type RateSample = { time: number; uploaded: number };

function job(
  over: Partial<Omit<ImportJob, "progress">> & { progress?: Partial<JobProgress> } = {},
): ImportJob {
  const progress = { total: 100, uploaded: 0, duplicates: 0, errors: 0, ...(over.progress ?? {}) };
  return {
    id: "j1",
    status: "running",
    error: null,
    summary: null,
    awaiting_wipe_confirmation: false,
    pending_wipe_count: 0,
    file_errors: [],
    profile_id: "p1",
    ...over,
    progress,
  };
}

/** A controllable clock so elapsed time is deterministic. */
function clock(startMs: number) {
  let now = startMs;
  return {
    now: () => now,
    advance: (seconds: number) => {
      now += seconds * 1000;
    },
  };
}

describe("recomputeRates", () => {
  it("computes a positive rate and ETA from the first sample", () => {
    const samples = new Map<string, RateSample>();
    const c = clock(0);
    // First observation seeds the baseline: no elapsed time yet -> no rate.
    let rates = recomputeRates([job({ progress: { total: 105, uploaded: 5 } })], c.now, samples);
    expect(rates.j1).toEqual({ itemsPerSec: 0, etaSeconds: null });

    // 10s later, 5 more uploaded (10 of 105) -> 0.5/s, 95 remaining -> 190s.
    c.advance(10);
    rates = recomputeRates([job({ progress: { total: 105, uploaded: 10 } })], c.now, samples);
    expect(rates.j1.itemsPerSec).toBeCloseTo(0.5, 5);
    expect(rates.j1.etaSeconds).toBe(190);
  });

  it("returns null ETA when no progress was made (zero delta)", () => {
    const samples = new Map<string, RateSample>();
    const c = clock(1_000);
    recomputeRates([job({ progress: { total: 100, uploaded: 20 } })], c.now, samples);
    c.advance(30);
    const rates = recomputeRates([job({ progress: { total: 100, uploaded: 20 } })], c.now, samples);
    expect(rates.j1).toEqual({ itemsPerSec: 0, etaSeconds: null });
  });

  it("returns null ETA when zero time has elapsed", () => {
    const samples = new Map<string, RateSample>();
    const c = clock(500);
    // Same timestamp for both calls: elapsed 0 even though uploaded advanced.
    recomputeRates([job({ progress: { total: 100, uploaded: 1 } })], c.now, samples);
    const rates = recomputeRates([job({ progress: { total: 100, uploaded: 9 } })], c.now, samples);
    expect(rates.j1).toEqual({ itemsPerSec: 0, etaSeconds: null });
  });

  it("clamps remaining to zero when uploaded exceeds total", () => {
    const samples = new Map<string, RateSample>();
    const c = clock(0);
    recomputeRates([job({ progress: { total: 100, uploaded: 90 } })], c.now, samples);
    c.advance(5);
    // uploaded (120) > total (100): remaining clamps to 0 -> ETA 0, not negative.
    const rates = recomputeRates([job({ progress: { total: 100, uploaded: 120 } })], c.now, samples);
    expect(rates.j1.itemsPerSec).toBeGreaterThan(0);
    expect(rates.j1.etaSeconds).toBe(0);
  });

  it("drops the sample when a job stops running", () => {
    const samples = new Map<string, RateSample>();
    const c = clock(0);
    recomputeRates([job({ progress: { uploaded: 5 } })], c.now, samples);
    expect(samples.has("j1")).toBe(true);

    const rates = recomputeRates([job({ status: "completed" })], c.now, samples);
    expect(samples.has("j1")).toBe(false);
    expect(rates.j1).toBeUndefined();
  });

  it("prunes samples for jobs removed from the queue", () => {
    const samples = new Map<string, RateSample>();
    const c = clock(0);
    recomputeRates([job({ id: "a", progress: { uploaded: 1 } })], c.now, samples);
    expect(samples.has("a")).toBe(true);

    // Next refresh no longer includes job "a".
    recomputeRates([job({ id: "b", progress: { uploaded: 1 } })], c.now, samples);
    expect(samples.has("a")).toBe(false);
    expect(samples.has("b")).toBe(true);
  });
});
