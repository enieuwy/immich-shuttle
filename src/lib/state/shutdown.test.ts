import { describe, expect, it, vi } from "vitest";

import {
  runImportShutdown,
  SHUTDOWN_INCOMPLETE_MESSAGE,
  type ShutdownDeps,
} from "./shutdown";

/** The rejection Tauri surfaces when a job went terminal before the cancel
 *  landed: `invokeCommand` wraps the Rust `Err(String)` as
 *  `<command> failed: <message>`. Hard-coded here so a change to either side
 *  breaks this test rather than silently making every cancel look fatal. */
const alreadyTerminal = () =>
  Promise.reject(
    new Error("import_cancel failed: Cannot cancel a terminal import: job-1"),
  );

function deps(overrides: Partial<ShutdownDeps> = {}): ShutdownDeps {
  return {
    pendingStarts: [],
    runningJobIds: [],
    currentRunningJobIds: () => [],
    retainedJobIds: new Set<string>(),
    cancelImport: vi.fn(() => Promise.resolve()),
    awaitTerminal: vi.fn(() => Promise.resolve({})),
    timeoutMs: 1_000,
    ...overrides,
  };
}

describe("runImportShutdown", () => {
  it("closes when every worker is confirmed gone", async () => {
    const awaitTerminal = vi.fn(() => Promise.resolve({}));
    const retainedJobIds = new Set<string>();

    const outcome = await runImportShutdown(
      deps({ runningJobIds: ["job-1"], awaitTerminal, retainedJobIds }),
    );

    expect(outcome).toEqual({ kind: "complete" });
    expect(awaitTerminal).toHaveBeenCalledWith("job-1", 1_000);
    // Nothing left to re-await: a later quit must not block on proven-dead jobs.
    expect([...retainedJobIds]).toEqual([]);
  });

  /**
   * The regression that shipped: `window.confirm` blocks the JS thread, so a job
   * finishing while the prompt is up is still listed as running and gets
   * cancelled — which rejects. Treating that as a shutdown failure told the user
   * an already-finished import was "still shutting down" and made them quit twice.
   */
  it("closes when a job went terminal while the confirm prompt was open", async () => {
    const outcome = await runImportShutdown(
      deps({ runningJobIds: ["job-1"], cancelImport: vi.fn(alreadyTerminal) }),
    );

    expect(outcome).toEqual({ kind: "complete" });
  });

  it("keeps the window open when a cancel fails for any other reason", async () => {
    const outcome = await runImportShutdown(
      deps({
        runningJobIds: ["job-1"],
        cancelImport: vi.fn(() =>
          Promise.reject(new Error("import_cancel failed: Could not lock import job state")),
        ),
      }),
    );

    expect(outcome).toEqual({
      kind: "incomplete",
      message: SHUTDOWN_INCOMPLETE_MESSAGE,
    });
  });

  /**
   * A worker that has not exited is the one case that must never close the
   * window, and its id has to survive so the retry re-awaits it instead of
   * trusting the terminal status `import_cancel` already published.
   */
  it("keeps the window open and retains the job when the worker never exits", async () => {
    const retainedJobIds = new Set<string>();

    const outcome = await runImportShutdown(
      deps({
        runningJobIds: ["job-1"],
        retainedJobIds,
        awaitTerminal: vi.fn(() =>
          Promise.reject(new Error("import_await_terminal failed: still shutting down")),
        ),
      }),
    );

    expect(outcome.kind).toBe("incomplete");
    expect([...retainedJobIds]).toEqual(["job-1"]);
  });

  it("re-awaits a retained job on the next attempt even when nothing is running", async () => {
    const retainedJobIds = new Set<string>(["job-1"]);
    const awaitTerminal = vi.fn(() => Promise.resolve({}));
    const cancelImport = vi.fn(() => Promise.resolve());

    const outcome = await runImportShutdown(
      deps({ runningJobIds: [], retainedJobIds, awaitTerminal, cancelImport }),
    );

    expect(outcome).toEqual({ kind: "complete" });
    // The job is terminal by now, so there is nothing to cancel a second time —
    // but its worker still has to be confirmed gone.
    expect(cancelImport).not.toHaveBeenCalled();
    expect(awaitTerminal).toHaveBeenCalledWith("job-1", 1_000);
  });

  it("closes and drops a retained job that the backend has removed", async () => {
    const retainedJobIds = new Set<string>(["job-evicted"]);
    const awaitTerminal = vi.fn(() =>
      Promise.reject(new Error("import_await_terminal failed: Job not found: job-evicted")),
    );

    const outcome = await runImportShutdown(
      deps({ retainedJobIds, awaitTerminal }),
    );

    expect(outcome).toEqual({ kind: "complete" });
    expect([...retainedJobIds]).toEqual([]);
    expect(awaitTerminal).toHaveBeenCalledWith("job-evicted", 1_000);
  });

  it("keeps the window open for a genuine await timeout", async () => {
    const retainedJobIds = new Set<string>(["job-timeout"]);
    const awaitTerminal = vi.fn(() =>
      Promise.reject(new Error("import_await_terminal failed: timed out")),
    );

    const outcome = await runImportShutdown(
      deps({ retainedJobIds, awaitTerminal }),
    );

    expect(outcome).toEqual({
      kind: "incomplete",
      message: SHUTDOWN_INCOMPLETE_MESSAGE,
    });
    expect([...retainedJobIds]).toEqual(["job-timeout"]);
  });

  /**
   * A start the backend has admitted is not yet in the polled queue snapshot, so
   * it has no job id to cancel. Quitting in that window must still wait for the
   * start to land and then cancel the job it produced.
   */
  it("waits for an in-flight start and cancels the job it produces", async () => {
    const start = Promise.withResolvers<void>();
    let running: string[] = [];
    const cancelImport = vi.fn(() => Promise.resolve());
    const awaitTerminal = vi.fn(() => Promise.resolve({}));

    const shutdown = runImportShutdown(
      deps({
        pendingStarts: [start.promise],
        runningJobIds: [],
        currentRunningJobIds: () => running,
        cancelImport,
        awaitTerminal,
      }),
    );

    // The start resolves after the quit was requested, only then publishing its id.
    running = ["job-late"];
    start.resolve();
    const outcome = await shutdown;

    expect(outcome).toEqual({ kind: "complete" });
    expect(cancelImport).toHaveBeenCalledWith("job-late");
    expect(awaitTerminal).toHaveBeenCalledWith("job-late", 1_000);
  });

  it("does not cancel the same job twice when it appears in both snapshots", async () => {
    const cancelImport = vi.fn(() => Promise.resolve());

    await runImportShutdown(
      deps({
        runningJobIds: ["job-1"],
        currentRunningJobIds: () => ["job-1"],
        cancelImport,
      }),
    );

    expect(cancelImport).toHaveBeenCalledTimes(1);
  });
});
