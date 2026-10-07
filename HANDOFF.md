# Immich Shuttle defect handoff

## Delivery

- Coordinator pane: `wAX:pQ`.
- Base: `5f034db30215331d81c86e8af95ebd5a3cd74e4e`.
- Implementation commit: `35ce52aa48a3672bccbc316c751ce6cc5fbc9981`.
- Output: `/Users/ellis/tmp/defect-fleet-20261003/immich-shuttle-HANDOFF.md`.
- Assignment: `/Users/ellis/tmp/defect-fleet-20261003/immich-shuttle-findings.json`.
- Result: 25 fixed or documented decisions, two stale findings, one partial test request, and four unresolved findings or decisions.
- No duplicate group or excluded feature record occurs in this export. No ledger closure occurred.

## Ownership, claims, and models

The main coordinator owns integration, gates, commits, and this handoff. Both workers and the reviewer report `openai-codex/gpt-6.1-sol` from their injected model context. No fallback occurred.

- `FrontendDefects` owns frontend queue behavior and assigned frontend tests.
- `RustTestDefects` owns `device_detector.rs`, `source_guard.rs`, `sidecar_runner.rs`, `keychain.rs`, `media_scanner.rs`, and `commands/profiles.rs`.
- The coordinator owns the remaining Rust changes, Cargo test features, and changelog.
- `IndependentReview` performs read-only review. It finds no concrete patch-introduced defects, including the final test corrections. It does not run checks.

The claim attempt used long-lived PID `44969` and holder `immich-shuttle-fleet`. Rumen returned `project "immich-shuttle" not found`. The coordinator notified `wAX:p1` and used only the explicit assignment. No repeated ledger query occurred.

The first date-only cutoff interpretation was wrong. The host clock showed `2026-10-02 16:46:04 UTC`. Neither stop marker existed before the worker launch. The coordinator checked the clock and markers again before the review launch at `16:56:27 UTC`. No new implementation wave started after taper. The existing reviewer completed one bounded pass over integration corrections.

## Verification

The final implementation commit contains the tested source and final test corrections.

- `npm run check`: zero errors and zero warnings before the two final auto-import fixture corrections.
- Final `npm run test`: **236 passed**, 20 files.
- Final `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check`: passed.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings`: passed before the final test-only corrections. No production code changed after this pass. The resource stop prevented a new full Clippy build after cleanup.
- Final `CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --manifest-path src-tauri/Cargo.toml --lib --quiet`: **347 passed, one ignored**, 1.08 seconds of test execution.
- `npm run build`: passed. Vite reports the existing `svelte -> vendor -> svelte` circular-chunk warning.
- `npm run e2e`: **13 passed** using Chromium. This runs the repository's safe fixture scenarios.
- No Windows runtime or signing/distribution gate ran. The Windows probe change therefore lacks platform runtime proof.

### Changed-path smoke

A real browser loaded the development fixture scenario. The operator path selected the Family album, chose each folder organization mode, and pressed Start Import. Captured IPC inputs showed:

| Organization | album_ids | into_album | keep_files |
| --- | --- | --- | --- |
| folder_name | [] | null | true |
| folder_path | [] | null | true |
| folder_tags | [] | null | true |

The screenshot showed the selected Family album still present and the folder-mode explanation visible. The fixture adapter handled all requests; no server upload or live data operation occurred. This proves the frontend path, not a real Immich upload.

A throwaway executable compiled the production `services/logs.rs` module. It called both log APIs with six unsupported names. All calls returned `Invalid log file name` before directory access. The executable printed its successful result. It did not read or write live logs.

### Failures and recovery

1. The first combined gate found two auto-import tests that expected an album target with folder organization. Their intended album-target scenarios now use `single_album`. The final frontend suite passes.
2. The first Clippy attempt could not find the sidecar binary. The repository download script fetched version 0.32.0 and verified its pinned SHA-256. Clippy then passed.
3. The first Rust test build ran out of disk space. The coordinator removed only this worktree's generated Cargo target: 1.9 GiB. A retry disabled debug information and incremental output.
4. That retry found an old empty-source forecast assertion with obsolete error precedence. The coordinator removed that duplicate assertion. The dedicated empty-source forecast table remains. The final Rust suite passes.
5. The browser helper could not connect to its relay, then failed two isolated browser launches. Playwright's installed Chromium completed the safe browser smoke. One earlier Eval attempt timed out; its browser close could not be confirmed after the kernel reset. The successful smoke browser closed explicitly. The browser helper reports zero managed tabs.
6. Node reports experimental localStorage warnings. Playwright reports NO_COLOR/FORCE_COLOR warnings. These do not fail the gates.

## Resource and artifact state

After all Rust proof completed, `cargo clean` removed another 1.3 GiB from this worktree. No Cargo target remains for immediate reuse. The final clean did not change source or test results.

The coordinator removed the throwaway Rust source, executable, and smoke screenshot after inspection. The smoke server stopped. Generated `node_modules`, `dist`, E2E output, and the checksum-verified sidecar remain local. They support reruns but are not part of the source commit. No shared cache, peer container, sibling worktree, or user data was removed. No Docker restart occurred. The possible browser process from the timed-out Eval remains a cleanup uncertainty; no broad process kill occurred.

## Remaining IDs and decisions

- `immich-shuttle-0fb5fbf4a7454fa4` — **P1 unresolved**: final hash-to-Trash pathname race. An atomic capture and recovery policy remains necessary.
- `immich-shuttle-670e5cc81795374d` — **P3 unresolved**: synchronous filesystem calls remain in async admission/finalization. A bounded lifetime policy remains necessary.
- `immich-shuttle-6b3` — Apple notarization/Homebrew cost and credential decision remains with the operator.
- `immich-shuttle-iy3` — Windows signing cost and credential decision remains with the operator.
- `immich-shuttle-a107d6406c7cf671` — date/concurrency tests are complete; raw/burst mapping-only tests remain intentionally absent under the brief's test rule.

No release, deployment, push, secret rotation, or OMP configuration change occurred. The URL fix does not prove that any previously exposed credential was rotated.

Landing evidence correction: the existing Rust thumbnail tests use the application thumbnail cache under `~/Library/Application Support/immich-shuttle/thumbnails`. The captured Rust output shows those paths. Tests create UUID-named temporary image fixtures and attempt to remove their matching cache files. They ignore cleanup errors, so removal is not guaranteed. This run therefore cannot claim that every test write stayed outside application data directories. No additional cache cleanup occurs during landing.

## Per-ID dispositions

Paths below reproduce the export's primary metadata path. The evidence describes the current code and the changes in the implementation commit.

### immich-shuttle-259290ca0595edf2 — P1 — fixed

append_log and read_recent join an unsanitized file_name onto the logs directory, so absolute paths and .. segments escape it

- Export path: `src-tauri/src/services/logs.rs`.
- Evidence: services/logs.rs now permits app.log or a canonical run-log basename. Both APIs reject unsupported names before directory access. Existing symlinks also fail. Rust tests and a standalone production-API smoke pass. This does not claim protection against a same-user concurrent symlink replacement.

### immich-shuttle-0fb5fbf4a7454fa4 — P1 — unresolved

Wipe verification has a path-based race between the final hash and Trash move

- Export path: `src-tauri/src/services/wipe.rs`.
- Evidence: services/wipe.rs still hashes through a file handle, then calls the path-based Trash API after that handle closes. The final pathname race remains. The code comment now states the limit. A safe fix needs atomic capture plus recovery rules for changed files, collisions, and crashes; this slice does not invent that transaction.

### immich-shuttle-e8a7b75ee7c936be — P1 — fixed

Folder organization rejects imports when an album is selected

- Export path: `src/lib/state/queue.ts`.
- Evidence: state/queue.ts computes the effective organization before album resolution. All three folder modes send album_ids=[] and into_album=null without clearing the picker. Queue tests and browser smoke cover each mode.

### immich-shuttle-61234de4829b826f — P2 — fixed

normalize_server_url keeps raw userinfo when URL parse fails, and api_endpoint_urls then echoes that string into errors

- Export path: `src-tauri/src/services/immich_client.rs`.
- Evidence: services/immich_client.rs returns an empty normalized value for an invalid URL. api_endpoint_urls returns fixed credential-free error text. The malformed-URL regression passes.

### immich-shuttle-c9a61efc1aa35cb2 — P2 — fixed

import_forecast accepts an empty source_paths list and fabricates a zero-file forecast where its sibling commands refuse

- Export path: `src-tauri/src/commands/import.rs`.
- Evidence: commands/import.rs rejects empty source_paths before replacing ACTIVE_FORECAST or reading credentials. The table covers absent, empty, and nonempty explicit selections with no source roots.

### immich-shuttle-eac686f51a2f198a — P2 — fixed

Windows mount_root_for_path calls mountvol with no timeout while walking ancestors, so a wedged volume can hang identity resolution forever

- Export path: `src-tauri/src/services/device_detector.rs`.
- Evidence: services/device_detector.rs uses volume_identity_for_path for Windows mount-root acceptance. That helper has the existing timeout and in-flight guard. macOS checks pass; no Windows runtime proof is available.

### immich-shuttle-112d2ba4340a2886 — P2 — fixed

Mount-root file identity test executes three real diskutil subprocesses costing 390ms

- Export path: `src-tauri/src/services/device_detector.rs`.
- Evidence: The mount-root identity test uses an injected probe and checks the root it receives. It no longer invokes diskutil. The Rust suite passes.

### immich-shuttle-ae941c37b7a2e12c — P2 — fixed

Source-guard atomic-swap test is purely sequential and cannot see the empty window it is named for

- Export path: `src-tauri/src/services/source_guard.rs`.
- Evidence: The source-guard test blocks replacement preparation and lets an observer inspect the old scope before the update proceeds. This removes the sequential-only atomicity claim. The Rust suite passes.

### immich-shuttle-6d508ee080f37372 — P2 — fixed

Missing unit tests for wipe_files unprovable identity and re-read failure branches

- Export path: `src-tauri/src/services/wipe.rs`.
- Evidence: Existing tests already cover absent mtime, absent record, and remount identity. A new deterministic read-error test checks unprovable classification, the message, zero deletion, and unchanged file bytes.

### immich-shuttle-dc456d17b6c34f3f — P2 — fixed

Missing unit tests for createAlbum public link failure and compound warnings formatting

- Export path: `src/lib/state/albums.ts`.
- Evidence: state/albums.test.ts now covers public-link failure and combined sharing failures. Tests check registration, selection, creation state, and warning delivery.

### immich-shuttle-5be6f6c533d5b32d — P2 — fixed

Missing unit tests for isBackendError prefix anchoring and type discrimination

- Export path: `src/lib/backendErrors.ts`.
- Evidence: backendErrors.test.ts covers all markers, wrapped errors, non-Errors, wrong kinds, and embedded response markers. The frontend suite passes these cases.

### immich-shuttle-a107d6406c7cf671 — P2 — partial; remaining mapping-only tests declined

Missing unit tests for sidecar upload arguments (date_range, concurrent_tasks, raw/burst stacking)

- Export path: `src-tauri/src/services/sidecar_runner.rs`.
- Evidence: sidecar_runner.rs tests now cover date trimming, blank ranges, zero concurrency, and positive concurrency. Raw/burst flag-copy assertions remain absent because the brief forbids wiring-only tests. No production defect was established in those direct mappings.

### immich-shuttle-7d3556d24ad519e3 — P2 — fixed

Missing unit tests for keychain store read failure and readback verification error branches

- Export path: `src-tauri/src/services/keychain.rs`.
- Evidence: keychain.rs adds counted failure injection for initial reads, post-write reads, and writes. Tests verify unchanged credentials and rollback with either prior or absent keys.

### immich-shuttle-fd4b63ee971781cb — P2 — fixed

Missing unit tests for get_profile_image redirect rejection and MIME validation branches

- Export path: `src-tauri/src/services/immich_client.rs`.
- Evidence: immich_client.rs HTTP tests cover redirect refusal, non-image MIME, invalid image bytes, 404 fallback, and signature-derived MIME. The Rust suite passes.

### immich-shuttle-18598378014a6fe6 — P2 — fixed

Missing unit tests for share_album_users role validation and payload construction

- Export path: `src-tauri/src/services/immich_client.rs`.
- Evidence: The HTTP test rejects unsupported roles before any request. It also checks the viewer/editor request path and parsed JSON body.

### immich-shuttle-9dc6ca2dab669a94 — P2 — fixed

Missing unit tests for resolve_album_id_by_name ambiguity defense and fallback branches

- Export path: `src-tauri/src/commands/import.rs`.
- Evidence: The album-resolution test covers blank names without network access, empty server URL, duplicate names, no match, case mismatch, one exact match, and invalid server data.

### immich-shuttle-47f545c5f93b5ca6 — P2 — fixed

Missing unit tests for profile_upsert credential rollback on profile store failure

- Export path: `src-tauri/src/commands/profiles.rs`.
- Evidence: profiles.rs tests force real profile-store parse failures after key changes. They check restoration, removal of a new key, compound rollback failures, and unchanged damaged configuration.

### immich-shuttle-bc0c69863f641dd7 — P2 — fixed

Missing unit tests for profile_delete rollback and failure handling branches

- Export path: `src-tauri/src/commands/profiles.rs`.
- Evidence: profiles.rs tests cover normal deletion, restoration after store failure, failed restoration, and early keychain deletion failure.

### immich-shuttle-ba9994b34e246027 — P2 — stale

Deleting a profile can strand an existing pending wipe

- Export path: `src-tauri/src/commands/profiles.rs`.
- Evidence: The original claim says the user cannot dismiss the pending wipe. Current import_confirm_wipe(false) skips credential lookup and consumes the offer. Its existing test passes with an empty fake keychain. confirm=true still needs the profile credential; the supported keep-files action resolves the offer.

### immich-shuttle-61b4be4361817527 — P2 — fixed

Host-side Immich opener interpolates an untrusted album ID into URL syntax

- Export path: `src-tauri/src/commands/settings.rs`.
- Evidence: commands/settings.rs rejects album IDs containing URL syntax before the OS opener. Allowed IDs contain ASCII letters, digits, hyphens, or underscores. Existing empty-ID timeline behavior remains.

### immich-shuttle-560467213de8a2b0 — P2 — fixed by documented design decision

The source-path guard is renderer-self-authorizing, so it cannot defend against a compromised renderer

- Export path: `src-tauri/src/services/source_guard.rs`.
- Evidence: source_guard.rs now calls this a consistency check. Renderer-supplied roots do not independently prove user consent. The slice adds no native-dialog authorization feature.

### immich-shuttle-b8e0803fb56342f3 — P2 — stale

Unhandled promise rejection at application startup if profile loading fails

- Export path: `src/App.svelte`.
- Evidence: profiles.ts catches load failure, records the error, and resolves instead of rethrowing. profiles.test.ts already checks this behavior. Worker history inspection identifies existing fix 8c7d592. App.svelte therefore does not receive the claimed rejected promise.

### immich-shuttle-6b3 — P2 — unresolved operator decision

Decide on Apple notarization and Homebrew distribution

- Export path: `No primary metadata path in the export`.
- Evidence: No Apple account purchase, signing identity, notarization, release, or Homebrew change occurs. Keep the current distribution setup until the operator approves its cost and credentials.

### immich-shuttle-670e5cc81795374d — P3 — unresolved

Import admission and finalization run unbounded filesystem canonicalization inline on the async runtime

- Export path: `src-tauri/src/commands/import.rs`.
- Evidence: Import admission and finalization still call synchronous canonicalize/stat on runtime workers. A spawn_blocking wrapper alone does not bound a stuck kernel call. A bounded lifetime and abandonment policy remains necessary; this slice does not change that policy.

### immich-shuttle-9dcd3a631cccd11b — P3 — fixed

Safety-lease admission refuses every import but its error says only 'this source' — message and check disagree

- Export path: `src-tauri/src/commands/import.rs`.
- Evidence: The safety-lease error now says all imports are paused until restart. The process-global admission behavior stays unchanged.

### immich-shuttle-a0fbc9592053eeb1 — P3 — fixed

History replay tests spend 50ms per test waiting out Vitest default waitFor polling interval

- Export path: `src/lib/state/history.test.ts`.
- Evidence: Three history replay tests use explicit scan-start promise gates instead of default waitFor polling. The frontend suite passes.

### immich-shuttle-d6ab0ebf27ed54c9 — P3 — fixed

Cross-origin redirect test wastes 100ms awaiting negative assertion channel timeout

- Export path: `src-tauri/src/services/immich_client.rs`.
- Evidence: The cross-origin redirect test uses try_recv after the completed request, instead of waiting 100 ms for an empty channel. The Rust suite passes.

### immich-shuttle-7e3621f8b45a0d18 — P3 — fixed

join_bounded timeout and cancellation tests incur 210ms wall-clock latency waiting out real poll intervals

- Export path: `src-tauri/src/commands/import.rs`.
- Evidence: join_bounded uses Tokio time for its async deadline. Both timeout/cancel tests use a paused Tokio clock. The Rust suite passes without real deadline sleeps.

### immich-shuttle-d276502cad2bc613 — P3 — fixed

await_terminal timeout tests incur 219ms wall-clock latency per test due to unpaused 100ms polling sleep

- Export path: `src-tauri/src/commands/import.rs`.
- Evidence: import_await_terminal uses Tokio time for its async deadline. Both worker/finalization timeout tests use a paused clock. The Rust suite passes.

### immich-shuttle-6bfc8b4951782fd5 — P3 — fixed

Media scanner guard release test sleeps 150ms on a hardcoded thread delay

- Export path: `src-tauri/src/services/media_scanner.rs`.
- Evidence: The scanner guard test replaces its arbitrary 150 ms sleep with explicit synchronization. The Rust suite passes.

### immich-shuttle-9a9600f52814e1a6 — P3 — fixed

'waits for a confirmed wipe before closing' proves nothing: one microtask is below the function's own baseline latency

- Export path: `src/lib/state/shutdown.test.ts`.
- Evidence: The shutdown test now drains the normal async path with virtual time. It asserts no cancellation, terminal wait, or close while the wipe is pending, then checks progress after resolution.

### immich-shuttle-iy3 — P3 — unresolved operator decision

Decide on Windows code signing for SmartScreen

- Export path: `No primary metadata path in the export`.
- Evidence: No certificate purchase, signing credential, release, or Windows packaging change occurs. The operator must approve the certificate cost and signing policy.

