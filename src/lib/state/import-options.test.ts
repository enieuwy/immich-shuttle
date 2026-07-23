import { describe, expect, it } from "vitest";
import { get } from "svelte/store";

import { importOptionsState, isDateRangeInvalid, toImmichDateRange } from "./import-options";
import type { ImportInput } from "$lib/types";

describe("import date ranges", () => {
  it("rejects a range whose start is after its end", () => {
    expect(isDateRangeInvalid("2026-02-01", "2026-01-01")).toBe(true);
    expect(toImmichDateRange("2026-02-01", "2026-01-01")).toBeNull();
  });
});

describe("hydrateFromRequest", () => {
  const req = (o: Partial<ImportInput>): ImportInput => ({
    profile_id: "p",
    source_paths: ["/x"],
    album_ids: [],
    keep_files: true,
    stack_raw_jpeg: true,
    stack_burst: true,
    date_range: null,
    concurrent_tasks: null,
    ...o,
  });

  it("maps every request field into option state", () => {
    importOptionsState.hydrateFromRequest(
      req({
        keep_files: false,
        stack_raw_jpeg: false,
        stack_burst: false,
        concurrent_tasks: 6,
        date_range: "2026-01-01,2026-02-01",
        organization: "folder_name",
        on_errors: "continue",
        overwrite: true,
        tags: ["a"],
        session_tag: true,
        include_type: "VIDEO",
        include_extensions: [".mp4"],
        exclude_extensions: [".gif"],
      }),
    );
    expect(get(importOptionsState)).toMatchObject({
      keepFiles: false,
      stackRawJpeg: false,
      stackBurst: false,
      concurrentTasks: 6,
      dateFrom: "2026-01-01",
      dateTo: "2026-02-01",
      organization: "folder_name",
      keepGoingOnErrors: true,
      overwrite: true,
      tags: ["a"],
      sessionTag: true,
      mediaType: "video",
      includeExtensions: [".mp4"],
      excludeExtensions: [".gif"],
      onlyNewSinceLastImport: false,
    });
  });

  it("maps include_type IMAGE to image, drops non-continue on_errors, clears empty date", () => {
    importOptionsState.hydrateFromRequest(req({ include_type: "IMAGE", on_errors: "stop" }));
    const s = get(importOptionsState);
    expect(s.mediaType).toBe("image");
    expect(s.keepGoingOnErrors).toBe(false);
    expect(s.dateFrom).toBeNull();
    expect(s.dateTo).toBeNull();
  });
});
