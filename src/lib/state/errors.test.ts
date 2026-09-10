import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

import { errorsState } from "./errors";

beforeEach(() => {
  vi.useFakeTimers();
  for (const error of get(errorsState)) {
    errorsState.dismissError(error.id);
  }
});

afterEach(() => {
  for (const error of get(errorsState)) {
    errorsState.dismissError(error.id);
  }
  vi.useRealTimers();
});

describe("errorsState", () => {
  it("keeps error toasts after the auto-dismiss window while info toasts expire", () => {
    errorsState.addError("A persistent error");
    errorsState.addError("A temporary message", "info");

    expect(get(errorsState)).toHaveLength(2);
    expect(vi.getTimerCount()).toBe(1);

    vi.advanceTimersByTime(5000);

    expect(get(errorsState)).toEqual([
      expect.objectContaining({ level: "error", message: "A persistent error" }),
    ]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("deduplicates an active key across messages and levels until recovery clears it", () => {
    errorsState.addError("Queue refresh failed.", "error", "queue-refresh");
    errorsState.addError("The queue is unavailable.", "warning", "queue-refresh");

    expect(get(errorsState)).toEqual([
      expect.objectContaining({
        dedupeKey: "queue-refresh",
        level: "error",
        message: "Queue refresh failed.",
      }),
    ]);

    errorsState.clearKeyed("queue-refresh");
    errorsState.addError("The queue is still unavailable.", "warning", "queue-refresh");

    expect(get(errorsState)).toEqual([
      expect.objectContaining({
        dedupeKey: "queue-refresh",
        level: "warning",
        message: "The queue is still unavailable.",
      }),
    ]);
  });
});
