import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("$lib/api", () => ({
  historySourceLastImport: vi.fn(async () => null),
}));

import { historySourceLastImport } from "$lib/api";
import { importOptionsState, isDateRangeInvalid, resolveImportDateRange, toImmichDateRange } from "./import-options";
import type { ImportInput } from "$lib/types";

describe("import date ranges", () => {
  it("rejects a range whose start is after its end", () => {
    expect(isDateRangeInvalid("2026-02-01", "2026-01-01")).toBe(true);
    expect(toImmichDateRange("2026-02-01", "2026-01-01")).toBeNull();
  });
});

describe("resolveImportDateRange", () => {
  const onlyNew = { dateFrom: null, dateTo: null, onlyNewSinceLastImport: true };
  const paths = ["/migration/card-a", "/migration/card-b"];

  beforeEach(() => {
    vi.mocked(historySourceLastImport).mockReset().mockResolvedValue(null);
  });

  it("uses the folder migration source checkpoint in the local calendar zone", async () => {
    vi.mocked(historySourceLastImport).mockResolvedValue(new Date(2026, 2, 15, 12).getTime());
    await expect(resolveImportDateRange(onlyNew, "destination-profile", paths, false, "folder"))
      .resolves.toBe("2026-03-15,9999-12-31");
    expect(historySourceLastImport).toHaveBeenCalledExactlyOnceWith("destination-profile", paths);
  });

  it("gives explicit dates precedence over the only-new checkpoint", async () => {
    const options = { ...onlyNew, dateFrom: "2026-01-01", dateTo: "2026-01-31" };
    await expect(resolveImportDateRange(options, "p", paths, false, "folder"))
      .resolves.toBe("2026-01-01,2026-01-31");
    expect(historySourceLastImport).not.toHaveBeenCalled();
  });

  it("gives an explicit selection precedence over dates and only-new", async () => {
    const options = { ...onlyNew, dateFrom: "2026-02-01", dateTo: "2026-01-01" };
    await expect(resolveImportDateRange(options, "p", paths, true, "folder")).resolves.toBeNull();
    expect(historySourceLastImport).not.toHaveBeenCalled();
  });

  it("uses the checkpoint when an explicit date range is incomplete", async () => {
    vi.mocked(historySourceLastImport).mockResolvedValue(new Date(2026, 3, 2, 12).getTime());
    await expect(resolveImportDateRange({ ...onlyNew, dateFrom: "2026-01-01" }, "p", paths, false))
      .resolves.toBe("2026-04-02,9999-12-31");
  });

  it("returns no date filter when the folder has no checkpoint", async () => {
    await expect(resolveImportDateRange(onlyNew, "p", paths, false, "folder")).resolves.toBeNull();
    expect(historySourceLastImport).toHaveBeenCalledExactlyOnceWith("p", paths);
  });

  it("does not read a checkpoint when only-new is off", async () => {
    await expect(resolveImportDateRange({ ...onlyNew, onlyNewSinceLastImport: false }, "p", paths, false))
      .resolves.toBeNull();
    expect(historySourceLastImport).not.toHaveBeenCalled();
  });

  it("refuses to widen a folder migration when its checkpoint cannot be read", async () => {
    vi.mocked(historySourceLastImport).mockRejectedValue(new Error("Unreadable history store"));
    await expect(resolveImportDateRange(onlyNew, "p", paths, false, "folder")).rejects.toThrow(
      'Could not read the last-import checkpoint for this source, so "only new since last import" cannot be applied.',
    );
  });

  it.each(["google_photos", "icloud", "immich"] as const)(
    "does not adopt a folder checkpoint for %s migrations",
    async (source) => {
      await expect(resolveImportDateRange(onlyNew, "p", paths, false, source)).resolves.toBeNull();
      const explicit = { ...onlyNew, dateFrom: "2026-01-01", dateTo: "2026-01-31" };
      await expect(resolveImportDateRange(explicit, "p", paths, false, source))
        .resolves.toBe("2026-01-01,2026-01-31");
      expect(historySourceLastImport).not.toHaveBeenCalled();
    },
  );
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
