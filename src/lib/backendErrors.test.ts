import { describe, expect, it } from "vitest";

import { BackendError, isBackendError } from "./backendErrors";

const contracts = [
  ["TERMINAL_CANCEL", "Cannot cancel a terminal import"],
  ["JOB_NOT_FOUND", "Job not found:"],
  ["IMPORT_NOT_RUNNING", "Import is no longer running:"],
  ["MISSING_API_KEY", "No API key found for profile"],
  ["UNREACHABLE_SERVER", "Could not reach the server"],
] as const;

describe("isBackendError", () => {
  it.each(contracts)("recognizes %s only at the backend message prefix", (kind, marker) => {
    const message = `${marker} detail`;
    expect(isBackendError(new BackendError("test_command", message), kind)).toBe(true);
    expect(isBackendError(new Error(message), kind)).toBe(true);
    expect(isBackendError(new Error(`test_command failed: ${message}`), kind)).toBe(true);

    const remoteMessage = `API GET /albums failed at https://example.com (500): ${message}`;
    expect(isBackendError(new BackendError("test_command", remoteMessage), kind)).toBe(false);
    expect(isBackendError(new Error(remoteMessage), kind)).toBe(false);
    expect(isBackendError(new Error(`test_command failed: ${remoteMessage}`), kind)).toBe(false);
    expect(isBackendError(new Error(` ${message}`), kind)).toBe(false);
  });

  it.each(contracts)("rejects non-Errors and other error kinds for %s", (kind, marker) => {
    for (const reason of [undefined, null, false, 1, marker, { message: marker }]) {
      expect(isBackendError(reason, kind)).toBe(false);
    }
    for (const [otherKind, otherMarker] of contracts) {
      if (otherKind !== kind) {
        expect(isBackendError(new BackendError("test_command", otherMarker), kind)).toBe(false);
        expect(isBackendError(new Error(otherMarker), kind)).toBe(false);
      }
    }
  });

  it("uses BackendError.backendMessage rather than the displayed message", () => {
    const backendError = new BackendError("test_command", "unrelated failure");
    backendError.message = "Could not reach the server";
    expect(isBackendError(backendError, "UNREACHABLE_SERVER")).toBe(false);
  });
});
