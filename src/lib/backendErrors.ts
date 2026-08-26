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

/** Prefix `invokeCommand` adds so a raw message can be recovered from the text. */
const WRAPPER = /^[a-z_]+ failed: /;

/**
 * A rejected Tauri command, keeping the backend's own message intact.
 *
 * The displayed message names the command, but the marker test below must run
 * against the unwrapped text: the markers are prefixes on the Rust side, and
 * anchoring is what stops a server-supplied string from impersonating one.
 */
export class BackendError extends Error {
  readonly backendMessage: string;

  constructor(command: string, backendMessage: string, cause?: unknown) {
    super(`${command} failed: ${backendMessage}`, { cause });
    this.name = "BackendError";
    this.backendMessage = backendMessage;
  }
}

/**
 * True when `reason` is an Error whose message carries the named backend
 * contract.
 *
 * Matching is anchored at the start of the backend's own message, not a
 * substring of the whole text. Every one of these markers is a prefix in Rust,
 * while a failed HTTP request embeds the server's response body AFTER its own
 * `API {method} {path} failed at {url} ({status}): ` prefix. An unanchored test
 * therefore lets any server -- or anything that can influence a response body --
 * choose the frontend's branch: a 500 whose body read "Could not reach the
 * server" would drive the album loader's retry loop, and one reading "No API key
 * found for profile" would raise the add-your-key prompt.
 */
export function isBackendError(
  reason: unknown,
  kind: keyof typeof BACKEND_ERROR,
): reason is Error {
  if (!(reason instanceof Error)) return false;
  const message =
    reason instanceof BackendError
      ? reason.backendMessage
      : reason.message.replace(WRAPPER, "");
  return message.startsWith(BACKEND_ERROR[kind]);
}
