/**
 * Error texts produced by the Rust backend that the frontend branches on.
 *
 * Tauri commands return `Result<T, String>`, so these strings are the only
 * channel for "which kind of failure was this". Each one is defined once in Rust
 * and pinned there by a test, and mirrored here so a reader can find both halves
 * of the contract from either side:
 *
 * | Constant below       | Rust definition                                  |
 * | -------------------- | ------------------------------------------------ |
 * | `TERMINAL_CANCEL`    | `commands::import::TERMINAL_CANCEL_ERROR`        |
 * | `JOB_NOT_FOUND`      | `commands::import::JOB_NOT_FOUND_ERROR`          |
 * | `MISSING_API_KEY`    | `services::keychain::MISSING_API_KEY_ERROR`      |
 * | `UNREACHABLE_SERVER` | `services::immich_client::UNREACHABLE_ERROR`     |
 *
 * Do not match backend prose anywhere else. In particular, never match a
 * dependency's wording (reqwest's "error sending request", "tcp connect"): a
 * version bump rewords it and the branch silently stops firing.
 */
const BACKEND_ERROR = {
  TERMINAL_CANCEL: "Cannot cancel a terminal import",
  JOB_NOT_FOUND: "Job not found:",
  MISSING_API_KEY: "No API key found for profile",
  UNREACHABLE_SERVER: "Could not reach the server",
} as const;

/** True when `reason` is an Error whose message carries the named backend contract. */
export function isBackendError(
  reason: unknown,
  kind: keyof typeof BACKEND_ERROR,
): reason is Error {
  return reason instanceof Error && reason.message.includes(BACKEND_ERROR[kind]);
}
