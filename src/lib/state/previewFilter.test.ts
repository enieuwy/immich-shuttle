import { afterEach, describe, expect, it } from "vitest";

import type { MediaFile } from "$lib/types";
import type { PreviewMetadata } from "$lib/previewApi";
import { selectionState } from "./selection";
import {
  dayEndEpoch,
  dayStartEpoch,
  filterFiles,
  presetRange,
  toYmd,
  type PreviewFilter,
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
  const base: PreviewFilter = {
    type: "all",
    fromEpoch: null,
    toEpoch: null,
    nameQuery: "",
    minBytes: null,
    maxBytes: null,
  };
  afterEach(() => selectionState.clear());
  const run = (overrides: Partial<PreviewFilter>, input = files) =>
    filterFiles(input, dates, { ...base, ...overrides });

  it("passes everything with no filter", () => {
    expect(run({})).toHaveLength(3);
  });

  it("filters by media type", () => {
    expect(run({ type: "photo" })).toEqual([photoA, photoB]);
    expect(run({ type: "video" })).toEqual([video]);
  });

  it("filters by inclusive date window and drops unknown-date files", () => {
    const from = dayStartEpoch("2026-06-01");
    const to = dayEndEpoch("2026-06-30");
    // photoB (Jun 15) in range; photoA (Jan) out; video (unknown) excluded.
    expect(run({ fromEpoch: from, toEpoch: to })).toEqual([photoB]);
  });

  it("combines type and date filters", () => {
    const from = dayStartEpoch("2026-01-01");
    expect(run({ type: "photo", fromEpoch: from })).toEqual([photoA, photoB]);
  });

  it("filters by case-insensitive filename substring", () => {
    expect(run({ nameQuery: "A.JPG" })).toEqual([photoA]);
    expect(run({ nameQuery: ".mp4" })).toEqual([video]);
    // Blank/whitespace query is a no-op.
    expect(run({ nameQuery: "   " })).toHaveLength(3);
  });

  it("filters by inclusive byte-size window", () => {
    const small = { ...file("small.jpg"), size_bytes: 500 };
    const mid = { ...file("mid.jpg"), size_bytes: 1500 };
    const big = { ...file("big.jpg"), size_bytes: 3000 };
    const sized = [small, mid, big];
    expect(run({ minBytes: 1000 }, sized)).toEqual([mid, big]);
    expect(run({ maxBytes: 2000 }, sized)).toEqual([small, mid]);
    expect(run({ minBytes: 1000, maxBytes: 2000 }, sized)).toEqual([mid]);
  });

  it("matches normalized extension membership and combines all existing predicates", () => {
    const uppercase = { ...photoB, extension: ".JPG", size_bytes: 1500 };
    const raw = file("b.CR3");
    const input = [photoA, uppercase, raw, video];
    expect(run({ extensions: new Set(["jpeg", ".jpg"]) }, input)).toEqual([photoA, uppercase]);
    expect(run({ extensions: new Set([".CR3"]) }, input)).toEqual([raw]);
    expect(run({
      type: "photo", nameQuery: "B.", extensions: new Set(["jpg"]),
      minBytes: 1500, maxBytes: 1500, fromEpoch: JUN_15, toEpoch: JUN_15,
    }, input)).toEqual([uppercase]);
    expect(run({ extensions: new Set(["mp4"]), type: "photo" }, input)).toEqual([]);
  });

  it("distinguishes missing metadata, missing GPS, and valid zero coordinates", () => {
    const details: PreviewMetadata = {
      camera: "Drone Test Camera", lens: null, width: 4000, height: 3000,
      gps_lat: 0, gps_lon: 0, iso: 100, exposure: "1/100 s",
    };
    const metadata = new Map<string, PreviewMetadata | null>([
      [photoA.path, details], [photoB.path, { ...details, camera: "Other Camera", gps_lat: null }],
      [video.path, null],
    ]);
    expect(filterFiles(files, dates, { ...base, gps: "with" }, metadata)).toEqual([photoA]);
    expect(filterFiles(files, dates, { ...base, gps: "without" }, metadata)).toEqual([photoB, video]);
    expect(filterFiles(files, dates, { ...base, gps: "without" }, new Map())).toEqual([]);
    expect(filterFiles(files, dates, { ...base, cameraQuery: "  DRONE ", gps: "with" }, metadata)).toEqual([photoA]);
    expect(filterFiles(files, dates, { ...base, cameraQuery: "drone", gps: "without" }, metadata)).toEqual([]);
  });

  it("keeps hidden picks when selecting and inverting an extension-filtered subset", () => {
    selectionState.selectOnly([photoA.path]);
    const shown = run({ extensions: new Set(["mp4"]) });
    selectionState.add(shown.map((file) => file.path));
    expect(selectionState.paths()).toEqual([photoA.path, video.path]);
    selectionState.invert(shown.map((file) => file.path));
    expect(selectionState.paths()).toEqual([photoA.path]);
    expect(run({})).toEqual(files);
  });
});
