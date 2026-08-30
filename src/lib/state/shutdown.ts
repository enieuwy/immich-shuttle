/**
 * The quit-with-a-running-import sequence.
 *
 * Extracted from `App.svelte` so it can be tested directly: this is the path
 * that decides whether the process may exit while an import is live, and
 * getting it wrong means killing a sidecar mid-upload and leaving the run's
 * staging, log, and wipe state uncommitted. Mounting the whole app to exercise
 * it would drag in profile loading, queue polling, and device listeners, so the
 * sequence takes its collaborators as arguments instead.
 *
 * The invariant: a terminal STATUS is not a terminal WORKER. `import_cancel`
 * publishes `Cancelled` immediately while the worker is still resolving a
 * server, cleaning staging, or waiting for the sidecar to die, so only
 * `importAwaitTerminal` — which additionally requires the job to have left the
 * backend's `RUNNING_IMPORTS` map — can authorise the close.
 */

import { isBackendError } from "$lib/backendErrors";

/** Shown when shutdown could not be confirmed; the window must stay open. */
export const SHUTDOWN_INCOMPLETE_MESSAGE =
  "The import is still shutting down. Keep this window open and retry quitting.";

/** Shown when a confirmed delete is still running; the window must stay open. */
export const WIPE_INCOMPLETE_MESSAGE =
  "A verified delete is still running. Keep this window open and retry quitting.";

/** How long to wait for one worker to exit. The sidecar alone can take five
 *  seconds, and staging cleanup on a full card is slower. */
export const SHUTDOWN_TIMEOUT_MS = 30_000;

export type ShutdownOutcome =
  | { kind: "complete" }
  | { kind: "incomplete"; message: string };

export type ShutdownDeps = {
  /**
   * `startImport` calls that have not resolved yet. A start the backend has
   * already admitted may not be in the queue snapshot, so it has no job id to
   * cancel until its post-admission refresh commits.
   */
  pendingStarts: readonly Promise<unknown>[];
  /**
   * Confirmed wipes that have not settled. A wipe runs on an already terminal
   * job, so it has no id to cancel and no worker to await: the app can only
   * wait for it. It cannot be cancelled either — the payload is consumed and
   * the originals are already moving to the Trash — so shutdown waits rather
   * than trying to stop it.
   */
  pendingWipes: readonly Promise<unknown>[];
  /** Running job ids as of the moment the user confirmed the prompt. */
  runningJobIds: readonly string[];
  /** Re-read running job ids after `pendingStarts` settle, to pick up any job
   *  that was still being admitted when the prompt went up. */
  currentRunningJobIds: () => readonly string[];
  /**
   * Ids carried across attempts, mutated in place. A job whose await timed out
   * is still shutting down, so a second quit must re-await it rather than
   * treat its already-published terminal status as proof of a clean exit.
   */
  retainedJobIds: Set<string>;
  cancelImport: (jobId: string) => Promise<unknown>;
  awaitTerminal: (jobId: string, timeoutMs: number) => Promise<unknown>;
  timeoutMs?: number;
};

/**
 * A cancel rejecting because the job is ALREADY terminal is not a safety
 * failure — it means the import finished on its own while the user considered
 * the native quit confirmation, after the caller took its job snapshot.
 * `awaitTerminal` runs over a superset of the cancelled ids and re-checks the
 * real condition, so this case is safe to ignore. A missing job is also
 * terminal: the backend cannot await work that it has already removed. Any
 * OTHER rejection (lock failure, timeout, unknown job) means cancellation may
 * not have taken effect and must still block the close.
 */
function isAlreadyTerminal(reason: unknown): boolean {
  return isBackendError(reason, "TERMINAL_CANCEL");
}

/**
 * `import_await_terminal` looks up the job before it waits. A dismissed or
 * evicted job therefore rejects with this text, but no worker can still be
 * running without a backend job, so shutdown can treat the id as terminal.
 */
function isJobAlreadyGone(reason: unknown): boolean {
  return isBackendError(reason, "JOB_NOT_FOUND");
}

/**
 * Whether `work` all settled inside `timeoutMs`.
 *
 * A confirmed wipe has no cancel path, so shutdown can only wait for it — but
 * hashing and deleting a full card takes minutes, and a quit that silently
 * hangs is worse than one that says "not yet". Refusing keeps the delete
 * running untouched, so a retry is safe.
 */
async function settledWithin(
  work: readonly Promise<unknown>[],
  timeoutMs: number,
): Promise<boolean> {
  // The executor runs synchronously, so `expire` is assigned before the timer
  // below can ever fire.
  let expire = () => {};
  const expired = new Promise<boolean>((resolve) => {
    expire = () => resolve(false);
  });
  const timer = setTimeout(expire, timeoutMs);
  try {
    return await Promise.race([Promise.allSettled(work).then(() => true), expired]);
  } finally {
    clearTimeout(timer);
  }
}

export async function runImportShutdown(deps: ShutdownDeps): Promise<ShutdownOutcome> {
  const timeoutMs = deps.timeoutMs ?? SHUTDOWN_TIMEOUT_MS;

  await Promise.allSettled(deps.pendingStarts);

  // Waited on before anything is cancelled: this quit may still be refused, and
  // a refused quit must not have stopped a running import on the way.
  if (
    deps.pendingWipes.length > 0 &&
    !(await settledWithin(deps.pendingWipes, timeoutMs))
  ) {
    return { kind: "incomplete", message: WIPE_INCOMPLETE_MESSAGE };
  }

  const jobIdsToCancel = new Set(deps.runningJobIds);
  for (const jobId of deps.currentRunningJobIds()) {
    jobIdsToCancel.add(jobId);
  }
  for (const jobId of jobIdsToCancel) {
    deps.retainedJobIds.add(jobId);
  }

  const cancellations = await Promise.allSettled(
    [...jobIdsToCancel].map((jobId) => deps.cancelImport(jobId)),
  );
  const fatalCancellationFailure = cancellations.some(
    (result) => result.status === "rejected" && !isAlreadyTerminal(result.reason),
  );

  const jobIdsToAwait = [...deps.retainedJobIds];
  const terminals = await Promise.allSettled(
    jobIdsToAwait.map((jobId) => deps.awaitTerminal(jobId, timeoutMs)),
  );
  let fatalTerminalFailure = false;
  terminals.forEach((result, index) => {
    if (result.status !== "rejected") return;
    if (isJobAlreadyGone(result.reason)) {
      // Do not carry an evicted id into every later quit attempt: the backend
      // has already removed the only record that could represent its worker.
      deps.retainedJobIds.delete(jobIdsToAwait[index]);
      return;
    }
    fatalTerminalFailure = true;
  });

  if (fatalCancellationFailure || fatalTerminalFailure) {
    return { kind: "incomplete", message: SHUTDOWN_INCOMPLETE_MESSAGE };
  }

  // Every worker is confirmed gone, so nothing needs re-awaiting on a future
  // attempt. Clearing matters when the close itself then fails for an unrelated
  // reason: the next quit should not re-await jobs already proven terminal.
  deps.retainedJobIds.clear();
  return { kind: "complete" };
}

