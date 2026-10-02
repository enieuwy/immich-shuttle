import { describe, expect, it } from "vitest";
import { historyAnalytics } from "./history-analytics";
import type { ImportRecord, Profile } from "./types";

const now = Date.UTC(2026, 9, 3, 12);
const profiles: Profile[] = [{ id: "p1", display_name: "Camera", server_url: "http://127.0.0.1:2283" }];
function record(id: string, overrides: Partial<ImportRecord> = {}): ImportRecord {
  return {
    id, profile_id: "p1", source_paths: [], album_ids: [], started_at: now - 60_000,
    finished_at: now, status: "completed", total: 10, uploaded: 7, duplicates: 2, errors: 1,
    ...overrides,
  };
}

describe("retained history analytics", () => {
  it("counts partial runs and removed profiles without hiding unknown outcomes", () => {
    const result = historyAnalytics([
      record("one"), record("two", { status: "failed", incomplete: true }),
      record("three", { profile_id: "removed", status: "future-status", total: 4, uploaded: 0, duplicates: 1, errors: 3 }),
    ], profiles, null, "day", now);
    expect(result.totals).toEqual({ runs: 3, completed: 1, failed: 1, cancelled: 0, unknown: 1, incomplete: 1, total: 24, uploaded: 14, duplicates: 5, errors: 5 });
    expect(result.perProfile.find((p) => p.profileId === "removed")).toMatchObject({ name: "Removed profile (removed)", runs: 1, errors: 3 });
    expect(historyAnalytics([record("one"), record("other", { profile_id: "removed" })], profiles, "p1", "day", now).totals.runs).toBe(1);
  });

  it("places dates into UTC buckets and keeps older records in lifetime totals", () => {
    const boundary = Date.UTC(2026, 9, 3);
    const result = historyAnalytics([
      record("yesterday", { finished_at: boundary - 1, uploaded: 2 }),
      record("today", { finished_at: boundary, uploaded: 3 }),
      record("old", { finished_at: Date.UTC(2025, 0, 1), uploaded: 8 }),
    ], profiles, null, "day", now);
    expect(result.buckets[result.buckets.length - 2]?.uploaded).toBe(2);
    expect(result.buckets[result.buckets.length - 1]?.uploaded).toBe(3);
    expect(result.totals.uploaded).toBe(13);
    const week = historyAnalytics([record("monday", { finished_at: Date.UTC(2026, 8, 28) })], profiles, null, "week", now);
    expect(week.buckets[week.buckets.length - 1]).toMatchObject({ start: Date.UTC(2026, 8, 28), runs: 1, uploaded: 7 });
  });

  it("clearing all records clears totals and does not invent storage bytes", () => {
    const result = historyAnalytics([], profiles, null, "day", now);
    expect(result.totals).toEqual({ runs: 0, completed: 0, failed: 0, cancelled: 0, unknown: 0, incomplete: 0, total: 0, uploaded: 0, duplicates: 0, errors: 0 });
    expect(result.buckets.every((b) => b.runs === 0 && b.uploaded === 0 && b.errors === 0)).toBe(true);
  });

  it("excludes corrupt counters and undated runs from chart values", () => {
    const result = historyAnalytics([record("bad", { uploaded: NaN, errors: -1, finished_at: NaN })], profiles, null, "day", now);
    expect(result.totals).toMatchObject({ runs: 1, uploaded: 0, errors: 0 });
    expect(result.skippedCounters).toBe(2);
    expect(result.undatedRuns).toBe(1);
    expect(result.buckets.every((b) => b.runs === 0)).toBe(true);
  });
});
