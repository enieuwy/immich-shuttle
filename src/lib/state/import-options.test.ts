import { describe, expect, it } from "vitest";

import { isDateRangeInvalid, toImmichDateRange } from "./import-options";

describe("import date ranges", () => {
  it("rejects a range whose start is after its end", () => {
    expect(isDateRangeInvalid("2026-02-01", "2026-01-01")).toBe(true);
    expect(toImmichDateRange("2026-02-01", "2026-01-01")).toBeNull();
  });
});
