import { describe, expect, it } from "vitest";

import type { MediaFile } from "$lib/types";
import {
  dayEndEpoch,
  dayStartEpoch,
  filterFiles,
  presetRange,
  toYmd,
} from "./previewFilter";

function file(name: string, is_video = false): MediaFile {
  return {
    path: `/card/${name}`,
    name,
    extension: name.slice(name.lastIndexOf(".")),
    size_bytes: 1000,
    is_video,
  };
}

const photoA = file("a.jpg");
const photoB = file("b.jpg");
const video = file("c.mp4", true);
const files = [photoA, photoB, video];

// captured_at epoch seconds. Parsed as UTC to mirror the backend, which builds
// capture epochs from EXIF wall-clock time treated as UTC.
const JAN_1 = Math.floor(new Date("2026-01-01T12:00:00Z").getTime() / 1000);
const JUN_15 = Math.floor(new Date("2026-06-15T12:00:00Z").getTime() / 1000);

const dates = new Map<string, number | null>([
  [photoA.path, JAN_1],
  [photoB.path, JUN_15],
  [video.path, null], // unknown date
]);

describe("date helpers", () => {
  it("toYmd formats local date", () => {
    expect(toYmd(new Date("2026-06-24T08:00:00"))).toBe("2026-06-24");
  });

  it("dayStartEpoch / dayEndEpoch bracket the UTC day", () => {
    const start = dayStartEpoch("2026-06-15")!;
    const end = dayEndEpoch("2026-06-15")!;
    expect(start).toBeLessThan(JUN_15);
    expect(end).toBeGreaterThan(JUN_15);
    expect(end - start).toBe(86399); // 23:59:59 - 00:00:00
  });

  it("rejects malformed dates", () => {
    expect(dayStartEpoch("")).toBeNull();
    expect(dayStartEpoch("2026/06/15")).toBeNull();
  });

  it("presetRange returns inclusive windows or null", () => {
    const now = new Date("2026-06-24T10:00:00");
    expect(presetRange("all", now)).toBeNull();
    expect(presetRange("custom", now)).toBeNull();
    expect(presetRange("7d", now)).toEqual({ from: "2026-06-18", to: "2026-06-24" });
    expect(presetRange("30d", now)).toEqual({ from: "2026-05-26", to: "2026-06-24" });
    expect(presetRange("year", now)).toEqual({ from: "2026-01-01", to: "2026-06-24" });
  });
});

describe("filterFiles", () => {
  it("passes everything with no filter", () => {
    const out = filterFiles(files, dates, { type: "all", fromEpoch: null, toEpoch: null });
    expect(out).toHaveLength(3);
  });

  it("filters by media type", () => {
    expect(
      filterFiles(files, dates, { type: "photo", fromEpoch: null, toEpoch: null }),
    ).toEqual([photoA, photoB]);
    expect(
      filterFiles(files, dates, { type: "video", fromEpoch: null, toEpoch: null }),
    ).toEqual([video]);
  });

  it("filters by inclusive date window and drops unknown-date files", () => {
    const from = dayStartEpoch("2026-06-01");
    const to = dayEndEpoch("2026-06-30");
    const out = filterFiles(files, dates, { type: "all", fromEpoch: from, toEpoch: to });
    // photoB (Jun 15) in range; photoA (Jan) out; video (unknown) excluded.
    expect(out).toEqual([photoB]);
  });

  it("combines type and date filters", () => {
    const from = dayStartEpoch("2026-01-01");
    const out = filterFiles(files, dates, { type: "photo", fromEpoch: from, toEpoch: null });
    expect(out).toEqual([photoA, photoB]);
  });
});
