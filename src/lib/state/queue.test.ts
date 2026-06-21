import { describe, expect, it, vi } from "vitest";

vi.mock("$lib/api", () => ({
  importListJobs: vi.fn(async () => []),
  importStart: vi.fn(async () => "job-1"),
  importCancel: vi.fn(async () => undefined),
  importConfirmWipe: vi.fn(async () => ({
    id: "job-1",
    status: "completed",
    progress: { total: 0, uploaded: 0, duplicates: 0, errors: 0 },
    awaiting_wipe_confirmation: false,
    pending_wipe_count: 0,
  })),
}));

import { queueState } from "./queue";

describe("queueState", () => {
  it("rejects startImport when profile/source not set", async () => {
    await expect(queueState.startImport()).rejects.toThrow("Select a profile before starting import");
  });
});
