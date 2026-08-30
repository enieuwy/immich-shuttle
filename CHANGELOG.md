# Changelog

## Unreleased

### Import
- **A cancelled import can no longer let the next one start on top of it.** Cancelling publishes the outcome immediately, while the run's own bookkeeping — reading the run log, writing the final state, saving the History record — is still in progress. Starting or retrying an import inside that window admitted a second run alongside that bookkeeping. The app now refuses for those few moments and says the previous import is still finishing, rather than treating a cancelled run as gone.
- **An import cancelled or failed while staging a hand-picked selection now appears in History.** Both outcomes ended the run without saving a record, so the queue card was the only trace: nothing to replay with "Import again", and nothing in the run history.
- **A card or network share that stops responding while staging a hand-picked import no longer freezes the app.** The staging step had no time limit and could not be interrupted, so a dead mount held the import worker forever — and with it the app, which refuses to quit while a worker is live. Staging is bounded by silence rather than by a stopwatch: one minute with no progress at all, where progress is a staged file or a copied chunk. A large selection that has to be copied — on Windows without developer mode, or across volumes — can still take as long as it needs, and cancelling releases the worker within a few seconds even when the source never answers again.
- **A card that stops answering during staging can no longer stack abandoned work.** Staging now claims its source the way folder scans and "Check server" already do, so retrying against a dead mount reports the source by name instead of leaving another stuck thread behind on every attempt.
- **"Check server" now gives up on a source that stops responding.** The scan behind it had no outer time limit, so a dead mount left the check spinning for the rest of the session. It now reports the source as unresponsive.
- **A cancel can no longer relabel a run that had already finished.** The cancel read the run's state, raised the stop flag, and only then wrote `Cancelled`. A run that published its own outcome in that gap — with its History record written from it — was overwritten on the card, so the queue and History disagreed about the same import. A cancel that arrives too late now reports exactly that, and a run still working is cancelled as before. A run cancelled while staging is likewise recorded as cancelled rather than failed.
- **A refused retry says why.** "An import is still finishing" now reaches the user instead of a generic failure, so a retry that only needs a moment is not mistaken for a broken one.
- **A start that loses the race says which reason it lost to.** Two starts can both pass the admission check before either is published; the second is refused when it is inserted. That refusal always read "An import is already running", even when the truth was that the previous run was still finishing. Both refusals now come from the same place, so the sentence matches the situation.

### Safety
- **Quitting during a confirmed delete now waits for it.** Verifying a full card against the server and moving the originals to the Trash takes minutes, and the app counted that as no work at all: a quit in the middle left some originals in the Trash, the rest on the card, and no record of which was which — and the delete could not be retried, because its file list had already been consumed. Both quit paths now wait for the delete, and refuse the quit with a clear message if it is still running after thirty seconds. The delete itself keeps running, so retrying the quit is safe. A partly failed delete that is retried while the first attempt is still finishing also keeps the app from quitting through either one.

### Maintenance
- **A new advisory against a dependency now fails CI instead of scrolling past.** `cargo audit` fails only on a vulnerability; an unmaintained, unsound or yanked crate was a warning, and there were 25 of them permanently on screen. Every warning is now a failure, with the accepted ones listed one by one in `src-tauri/.cargo/audit.toml` alongside which upstream crate pulls it in and why it cannot be fixed here. Anything not on that list fails the run.
- **Took the four dependency fixes that were actually available.** `anyhow`, `event-listener` and both live `rand` lines had published fixes for the unsoundness advisories against them, and the `wasm-bindgen` family had been yanked; all are updated. The graph has no vulnerabilities and 19 accepted warnings, down from 25. The rest are the archived GTK3 bindings and Unicode tables that Tauri pulls in, which clear only when Tauri moves off them.

## v0.7.2 - 2026-08-26

### Import
- **A crashed import worker now becomes a failed job instead of a phantom running job.** Panic cleanup updates the job record as well as the worker-liveness maps, so later imports and quit are not blocked by a worker that no longer exists.
- **A failed wipe-payload save can no longer publish an unusable delete prompt.** This covers both the end of a run and a retry after a partly failed delete. The app keeps the originals and reports the failure instead of offering a confirmation action with no data behind it, which could not be answered and could not be dismissed.
- **A crashed import worker no longer leaves the run's stored API key resident.** Panic cleanup drops the pending-delete payload along with the prompt.
- **Incremental-import checkpoints cannot borrow another source set's or another profile's date floor.** Both halves of the checkpoint identity — the profile id and the collapsed source list — are now length-prefixed, so no combination of separator characters inside a profile id or a folder name can compose one key from two different inputs. **One-time consequence of the new format: every existing "last imported" association is reset, so the first import after upgrading re-scans each source in full even with "only import media new since last time" on.** Nothing is uploaded twice; the server recognises what it already holds. This supersedes the v0.7.1 note that non-overlapping selections keep their checkpoints.
- **An unresponsive card or network share can no longer stack abandoned scans.** A folder scan and a "Check server" forecast each claim their source while they walk it. A source whose previous walk has not returned within five seconds is reported by name instead of silently starting another walk, so one dead mount can no longer consume file handles and blocking threads that staging and delete-verification need.
- **"Check server" now stops the work it started.** Changing the profile, the sources, the selection or the filters cancels the running forecast's scan and its checksum pass, rather than leaving it to run for up to an hour for a result nobody will see.
- **A card whose one-click import fails to start can be retried.** The card-detected banner returns instead of treating the card as handled for the rest of the session.
- **"Import again" no longer silently uploads into the library when the recorded album has changed.** The recorded album is matched by id and then by name, because Immich assigns albums by name, so an album that was recreated still receives the replay. When neither matches, the app reports it and stages no album.
- **A sidecar that ends without a termination event is judged by its run log.** A finished upload is no longer reported as a failure, and the diagnostic now states plainly that the process was killed and its exit could not be confirmed.
- **"Delete originals after import" now deletes every format the server confirmed.** The delete step applied its own 25-entry format allowlist, so a file that immich-go uploaded and the server confirmed — an AVCHD `.mts`, a `.webm`, a `.3gp`, a vendor raw outside the list — was silently kept and counted as skipped while the app reported the delete as done. The confirmed-by-the-server set is now the only authority, alongside the unchanged checks that each path lies under a chosen source folder and still matches the file that was verified. A file that vanished before it could be deleted is now named as such instead of disappearing into an anonymous "kept" total.
- **"Open in Immich" now points at the album the upload actually populated.** Immich assigns albums by name, and the album may not exist until the run creates it, so the id is resolved from the name once the run finishes. A card-rule import into a named album previously offered no album link at all, and a link recorded before the album was recreated pointed at the deleted one.
- **An unusable import option is refused instead of silently reverting to a default.** An unrecognised error mode reverted to stopping the whole run at the first per-file error — the opposite of "keep going" — and an unrecognised media-type filter dropped the filter entirely, uploading the kinds that were excluded and, on a delete-after-import run, deleting them from the card.
- **"Open in Immich" offers no album link rather than a wrong one.** When the recorded name matches no album, or matches more than one, the card no longer falls back to the id the picker sent before the run — the id that goes stale exactly when the album is deleted or recreated. A run that spreads assets across many albums, by folder name, folder path or tags, also claims no single album.
- **"Import again" targets where the previous run actually landed.** The replay follows the album the run resolved at its end, not the id the picker held beforehand, so an album recreated during the original run no longer sends the replay to the old one.
- **An unusable import option is refused before the keychain is touched.** Previously an invalid value could raise an OS unlock prompt and then report a missing key, naming neither the offending value nor the real problem. "Check server" applies the same order.

### UI
- **Repeated queue-poll failures no longer create an unlimited stack of identical error messages.** One message stands for an ongoing outage, and it is retired as soon as a poll succeeds, so a later outage is reported rather than hidden behind the message from the last one.
- **Live import progress recovers from a failed event-listener registration.** One failed registration used to stop live per-file progress for the rest of the session; the two-second refresh kept the cards moving, but nothing in between. The next poll cycle now retries.
- **A dismissed card can no longer flicker back into the queue.** A refresh that began while the dismiss was still in flight could commit the list it read beforehand, briefly restoring the card. "Clear finished" had the same window.
- **The "Check server" button can no longer stay stuck on "Checking…".** Changing an import input while a forecast was in flight left the button disabled for the rest of the session; a superseded forecast now releases it.
- **Keychain failures include platform-specific recovery guidance.** macOS, Windows, and Linux messages retain the underlying backend error for diagnosis.
- **A history entry with an outcome this build does not recognise is no longer labelled "Cancelled".** It is shown as an unknown outcome instead, one unreadable entry can no longer prevent the rest of the history from loading, and the entry keeps its own outcome: a later import no longer rewrites it as "unknown", which would have made a newer build's record unrecoverable.

### Maintenance
- **Progress events no longer carry the same progress object twice.** The Rust event and TypeScript consumer now use one required `progress` field.
- **Server compatibility policy has one implementation.** Profile validation and server information now share the same minimum version and warning.
- **Pull-request builds declare read-only repository permissions.**
- **Scan and history outcomes are typed rather than free strings.** The two status fields that the frontend declares as fixed sets are now Rust enums whose wire values are pinned by tests, so a new outcome cannot type-check its way into a mislabelled row.
- **The error texts the app branches on are defined once.** App quit, album retry, and the missing-key prompt read those decisions from backend error text; each string now has a single definition, pinned by a test, and the retry decision no longer pattern-matches the HTTP client's own wording, which any dependency bump could reword. A failed request also carries the backend's own message separately from the displayed text, and each decision is anchored to the start of it, so a server's response body cannot impersonate one of these markers and choose the app's behaviour.
- **A failed thumbnail is diagnosable.** One summary line per batch records how many tiles were unsupported against how many genuinely failed, with a representative reason, so a missing thumbnail can be told apart from an unwritable cache directory or a corrupt file. A batch of merely unsupported formats stays silent.
- **The app log cannot outgrow its 1 MB budget.** Trimming required the whole log to be valid UTF-8, so one mangled byte - a garbled path from a removable card, a torn write - disabled every later trim, leaving the log to grow while each new line re-read all of it. Trimming is byte-based now, and the log viewer no longer refuses to show a log containing one bad byte.

## v0.7.1 - 2026-08-23

### Import
- **Closing the window now also waits for the run's final bookkeeping.** The quit guard waited for the import worker to leave the running set, but the worker left it *before* reading the run log, registering the wipe payload, writing the final state, and saving the history record. A cancelled import was therefore waved through while it was still parsing, so quitting in that window lost the run's History entry and its "Import again" request. The wait now covers that finalization phase too. Cancellation itself is unchanged: an import stops being cancellable at the same moment as before.
- **A very long session can no longer drop a pending delete-after-import prompt.** Once more than 500 finished imports had accumulated in one session, the oldest were evicted with their pending-wipe payloads — including an import still awaiting confirmation, whose verified-uploaded originals were then stranded with no way back to the prompt. Eviction now skips imports awaiting confirmation, the same way "Clear finished" already did.
- **A staging failure can no longer relabel a cancelled import as failed.** If the file-staging task itself died while a cancellation was being published, the card showed "Failed" instead of "Cancelled".
- **Dismissing a finished import can no longer throw away its delete prompt.** The X on a card awaiting "Verify & delete" removed the job *and* the pending-wipe payload — the only handle on the verified-uploaded originals — so the prompt could never be reached again. Dismiss now refuses that card for the same reason "Clear finished" already did, and the X is no longer shown on it.
- **A delete that partly failed can be retried.** When verification succeeded but some originals could not be moved to the Trash (a locked file, a permission error), the prompt disappeared and the run was finished; only the files that failed are now re-offered. Files the server did not confirm, and files that changed after they were verified, are still deliberately kept and not re-offered.
- **A crashed import worker no longer blocks every later import and the app's own quit.** A panic anywhere in the worker left the run registered as live forever, so new imports were refused as "already running" and quitting waited on a worker that no longer existed. Restarting the app was the only way out.
- **Hand-picked imports report what actually landed.** Because such a run uploads through a temporary folder, the app previously fell back to *what you selected* when deciding what to offer for deletion: a run that uploaded nothing still asked to delete every picked file, and per-file errors named temporary paths that had already been removed. Both now resolve back to your real files, and anything that cannot be resolved is left out of the delete list rather than guessed. Deletion was, and remains, gated on the server confirming each file by checksum.
- **An unreadable run log is no longer reported as a clean empty import.** If the log could not be read at the end of a run — a disk or permission error, or antivirus interference — the import was presented as successful with zero uploads and no diagnostic anywhere. It now surfaces as an error on the card and a named entry in the app log.
- **"Only import media new since last time" works when your selection overlaps.** Selecting a card *and* a folder inside it stored the checkpoint under one identity and looked it up under another, so the date floor was never found: every run re-scanned and re-offered the whole source, and the Source card's "imported from here" hint stayed empty. Both sides now agree on one identity. Checkpoints for non-overlapping selections are unaffected.
- **A failed upload says what immich-go reported.** When the sidecar died unexpectedly the app showed only "event channel closed unexpectedly"; the last diagnostics the uploader printed are now included.

### UI
- **Dismissed and cleared import cards stay gone.** Both actions committed their result without invalidating a queue poll already in flight, so a poll that started first could resolve afterwards and briefly restore the removed cards — repeating their completion notifications. This completes the stale-response work in v0.7.0, which covered the polls but missed these two actions.
- **Error messages stay until you dismiss them.** Every notice disappeared after five seconds, including import failures; only informational notices auto-dismiss now.
- **Turning notifications off in System Settings now takes effect.** A granted permission was cached for the whole session, so the app kept trying to notify after the permission was revoked.

### Safety
- **The source-scope guard can no longer blank a preview for a folder you did select.** Starting a scan cleared the approved-source list and then repopulated it in two steps, so a thumbnail, capture-date, or "Check server" request landing in between was rejected. The scope is now swapped in one atomic step.
- **Cmd-Q on macOS now asks before killing a running import.** This closes the known issue listed under v0.7.0. The protection was registered against the window's close event, which the application menu's Quit never raises, so the most common way to quit a Mac app killed the sidecar mid-upload with no prompt and left staging, the run log, and wipe state uncommitted. Quitting now runs the same confirm, cancel, and wait-for-the-worker sequence as closing the window, and an idle app still quits immediately. The confirmation now uses the native dialog instead of the webview's synchronous `window.confirm`, which can accept the default during the Cmd-Q handoff before a user can decline it.
- **Quitting can no longer be blocked forever.** If a shutdown attempt timed out and that import was then dismissed or aged out of the queue, every later quit reported "the import is still shutting down" for a job that no longer existed. A job the backend no longer has cannot still be running, so it no longer holds the window open.
- **An auto-import gets the same quit protection as one you start.** A card-triggered import admitted in the instant before the queue noticed it was invisible to the quit sequence.
- **A failure to build the HTTP client reports instead of crashing.** The fallback path called a constructor that panics on exactly the failure that had just occurred (an unusable TLS backend or system resolver), taking the app down; it is now a normal error.
- **A removable-drive probe that crashes is recorded.** Such a probe was indistinguishable from a slow one, so the card was silently treated as having no DCIM folder.

### Network
- **Discovery searches your whole LAN, not just a /24.** "Discover servers" swept only the 254 addresses around the machine's own address regardless of the network's real size, so on a /16 or larger network — ordinary with 10.x or 172.16-31.x addressing — a reachable Immich server on a neighbouring subnet was never found. The range now follows the interface's netmask, capped and ordered nearest-first so the search still finishes within its deadline.

### Maintenance
- **Dependency updates**: `svelte` 5.56.9, `@tauri-apps/plugin-dialog` and `tauri-plugin-dialog` 2.7.2, `tauri-plugin-fs` 2.5.1, and `@tailwindcss/vite` 4.3.3. `nanoid` now resolves to 3.3.18, which fixes GHSA-2v37-7h3g-55p8 in build tooling.
- **Release builds now compile on every target.** The macOS-only quit guard no longer creates dead-code errors on Linux and Windows. CI also updates its pinned Rust toolchain action.

## v0.7.0 - 2026-08-06

### Import
- **"Check server" and Start Import now agree.** With a hand-picked preview selection, the preflight was still applying the coarse type/extension filters that the import itself drops, so it could report far fewer files — or zero — than the run would actually upload. The forecast now mirrors the import's scope exactly.
- **Durable exclude extensions no longer veto a hand-picked file.** "Always exclude extensions" is hygiene for unattended scans; the preview grid never filters by extension, so a selection really can contain an excluded file. Selecting a file now imports it.
- **A duplicate-only re-run no longer sits at 0%.** Live progress counts processed files (uploaded plus server-side duplicates), so re-importing a card the server already holds completes the bar instead of appearing stalled for the whole run.
- **"Clear finished" keeps imports awaiting wipe confirmation.** Clearing used to drop their pending-wipe payload, stranding verified-uploaded originals on the card with no way back to the prompt.
- **A capped forecast says so.** Above 5000 candidates the server check only compares the first batch; the result now reads "at least N · partial scan" instead of presenting a lower bound as an exact count.
- **Closing the window mid-import waits for the import to actually stop.** Confirming "quit and cancel" raced cancellation against a five-second timer and closed regardless of the outcome — so the app could exit while the worker was still uploading, cleaning staging, or writing the run's final state. It now waits for each job to be terminal *and* its worker gone, and keeps the window open with an actionable message if that does not happen. A quit arriving in the instant between an import being admitted and the queue noticing is no longer waved through. **This covers closing the window; see Known issues for Cmd-Q.**
- **Restored History filters are visible and clearable.** "Import again" could reinstate a date range, a photo/video restriction, or an include-extension list that the main screen has no way to show or clear since filtering moved into the preview grid — silently shrinking every later import. Active filters now appear beside Start Import with a one-click clear. Durable "always exclude" extensions and the per-source "only new" toggle are deliberately untouched by it.
- **The Source card refreshes "last imported" after a run.** It only re-read that timestamp when history was cleared, so it kept showing the previous value — the same value "only import media new since last time" derives its date floor from.
- **Delete-after-import works again for whole-card imports.** immich-go labels each logged file with the *basename* of the folder it was pointed at (`/Volumes/CARD` is logged as `CARD:DCIM/…`), but the run-log reader only matched full paths, so it resolved nothing and the verified-upload wipe prompt never appeared for a folder or card import. Hand-picked imports were unaffected. Both spellings are now accepted; two sources sharing a basename refuse to resolve rather than guess, so one card's file can never enter another's delete list.
- **A forged run-log record can no longer mark a failed run complete or move the incremental checkpoint.** immich-go writes filenames unescaped, so a file whose name contains a newline can inject whole log lines. Upload and duplicate tallies now count only records that resolve against a root immich-go was actually invoked against, and the "only new since last import" date floor additionally requires files that survived containment and still exist on disk — a spelled-but-nonexistent path no longer counts as evidence. Left unfixed, an injected line could report a run that uploaded nothing as Completed and advance the floor past media that never left the card.
- **Filenames containing `error=` or `reason=` are parsed whole.** The run-log attribute reader cut values at the first delimiter-looking substring, truncating such paths out of the wipe-candidate list and mangling them in the error report.
- **A run-log format change would now be visible.** Because resolution is required before a file counts, a future immich-go release that appends an attribute after `file=` would silently zero every count; unresolvable records are now tallied and written to the app log instead.

### UI
- **Queue keeps updating while the History tab is open.** Switching tabs unmounted the queue panel, which stopped the app-wide poll — freezing import progress, the footer, and the quit-time "an import is running" guard until you switched back.
- **Rotated RAW files preview the right way up.** Thumbnails built from a RAW file's embedded JPEG preview now apply EXIF orientation, matching the full-decode path.
- **Previews survive a webview reload.** Preview session tokens are seeded from the wall clock, so they can no longer restart below the backend's cancellation watermark and blank the grid.
- **A panic elsewhere no longer disables previews.** The source-scope guard recovers from a poisoned lock instead of failing every thumbnail and capture-date request for the rest of the session.
- **Stale responses can no longer overwrite newer state.** Queue polls, profile lists, history lists, and removable-device refreshes all committed in completion order, so a slow earlier response landing after a newer one could regress a completed job to running, resurrect a deleted profile, repopulate cleared history, or put an ejected card back in the picker. All of them now discard superseded results, and destructive actions invalidate reads already in flight.
- **Create album and "Import again" are single-flight.** Double-clicking Create made two albums on the server; two quick replays interleaved their profile, album, options, and source into one hybrid request. Both actions now disable while they run and show pending state.
- **Switching profiles clears the previous server's albums.** The album list and selection survived a profile switch, so starting an import immediately could target an album id that only exists on the other server. The list now clears on the switch, and the import refuses to use album state that does not belong to the active profile.
- **Failures are reported once, not twice.** Several actions showed an error toast and then re-threw, producing an unhandled rejection for something you had already been told about. History replay additionally now says which step failed — album loading previously failed silently.
- **Preview scans ignore batches from a superseded scan.** Scan progress events carry the id of the scan that produced them; changing sources quickly could otherwise mix files from a deselected source into the new grid.
- **Removing a source drops its selected files.** A hidden selection left over from a removed source made the next Start Import fail backend validation instead of importing what remained.
- **Album and user lists no longer fail on large servers.** The API response cap was 1 MiB, which a library with more than about a thousand albums exceeded outright.

### Safety
- **The server preflight is scoped to your chosen sources.** `import_forecast` opened and SHA-1'd every path the UI named without the source-root check the preview commands already enforce, so a bug in the interface could aim it at files you never selected. It now refuses anything outside the folders you scanned. (This bounds mistakes and path confusion, not a hostile renderer: the approved list is still built from paths the interface itself supplies.)
- **Dropped the unused shell-execute grant.** The webview was granted permission to launch the immich-go sidecar with an arbitrary environment and working directory, though only the Rust side ever launches it. The capability and the unused `@tauri-apps/plugin-shell` dependency are gone.
- **Profiles and history are written atomically, and owner-only on macOS and Linux.** Both stores now share one hardened write — unique temp file, `fsync`, atomic rename, then an `fsync` of the directory so the save survives an unclean shutdown — replacing a shared temp name with a non-atomic fallback. On macOS and Linux the files and their directory are also chmod'd to 0600/0700, matching the hardening the logs directory already had, and an existing world-readable store is tightened on the next save; Windows has no POSIX mode bits and continues to rely on the per-user app-data ACL. `config.json` holds your LAN/WAN endpoints and `store.json` your full import history including local paths.
- **The server no longer learns where your photos live on disk.** The duplicate check that runs before every "Check server" preflight and every verify-before-delete used each file's absolute path as its correlation id, disclosing your account name, volume labels, and folder tree (`/Users/you/Pictures/Iceland 2026/…`) to whatever endpoint the active profile resolves to, including the WAN failover target. It now sends an opaque request index and keeps the paths on your machine. Uploading a photo still tells the server its filename and size, as it must — but that is all it ever needed.
- **A hung card can no longer exhaust the app's threads.** A removable mount that stops answering filesystem calls spawned one permanently blocked probe thread every two seconds; there is now at most one outstanding probe per mount. Cancelled LAN discovery likewise aborts its probes instead of leaving up to 64 connections running.
- **Thumbnails are published atomically.** Every backend now encodes to a temporary file and renames it into the cache, so a concurrent request can never read a half-written image; an unreadable cache entry is deleted and regenerated rather than served forever; and the cache key includes file size, so a file replaced without changing its timestamp no longer shows the old thumbnail. Cancelling a preview now stops work already inside a decode, not just work still queued.

### Known issues
- **Cmd-Q on macOS quits without the cancel prompt.** The cancel-before-quit protection is registered against the window's close event, which the application menu's Quit does not raise — so quitting with Cmd-Q while an import is running kills it mid-upload with no confirmation, and can leave staging, the run log, and wipe state uncommitted. Present in earlier releases too, and not made worse by this one. **Close the window instead** (red button, or Cmd-W then quit) to get the prompt. Tracked for the next release.

## v0.6.0 - 2026-08-05

### UI
- **Cmd/Ctrl-K command palette.** A global shortcut opens a searchable palette to jump between Queue/History, start an import, open logs, manage or edit profiles, and cycle the theme — no mouse hunting for the right control.
- **Filename, type, and size filters in the preview grid.** Alongside the existing photo/video and capture-date filters, narrow the preview by filename substring (case-insensitive) and a min/max size window (MB). Filtering is purely visual — selection is keyed by path, so hidden tiles stay selected and "Select shown" scopes to the filtered set.
- **People on shared-album badges.** Shared album chips show a small badge per member: a colored initial matching their Immich avatar color by default (readable at badge size; WCAG 4.5:1 enforced), or — via the appearance menu — their actual profile picture with a colored ring, fetched through the authenticated backend and cached per session. Hovering a badge shows the person's full name; the create-album share list uses the same badges.
- **New default dark look: "Darkroom", plus a dark-palette picker.** Dark mode now defaults to a near-black chrome with the accent swapped to the teal end of the Immich lens, so photos — not the UI — carry the color. A palette picker next to the theme toggle (and a "Cycle Dark Palette" palette command) switches between Darkroom, the classic Indigo, and a warm amber "Ember"; the choice persists like the theme mode and light mode is unaffected.
- **No more light flash on dark-mode launch.** The stored theme and palette are applied before first paint via an inline bootstrap script, so opening the app in dark mode no longer flashes light chrome.
- **Durable preferences moved to an "Import defaults" settings modal.** A gear in the header (and an "Import Defaults" command) opens a modal for the set-once-and-forget options — always-exclude extensions, RAW+JPEG / burst stacking, parallel uploads, keep-going-on-errors, and session tagging — which now persist across sessions. They no longer clutter the per-import flow: the main screen keeps only this-import decisions.
- **One filtering surface: Preview & select.** Type / date / name / size filtering now lives only in the preview grid — the duplicate Source-card "pre-filter" is gone, ending the confusing two-surface split (and the case where a hand-picked file could be silently dropped by a coarse filter). Your grid selection *is* the import: when a selection exists, coarse filters never also apply. The Source card keeps a single fast-path toggle — "only import media new since last time" — which is ignored the moment you hand-pick. The old flat "Import options" card is now **Source** (input + only-new + auto-import), **Destination** (album + organize/tags), and a compact **Danger zone** (delete-after-import, replace-on-server — per-import only, never defaults); "Check server" sits beside Start Import.
- **Lists sort by what's actionable now.** Album chips show the selected album first, then alphabetical; the queue floats running jobs to the top (then queued, then failures with Retry, then finished); History is newest-first.
- **Tag suggestions from your Immich server.** The Destination "Tags" field now autocompletes against existing Immich tags as you type — pick one to reuse it instead of silently creating a near-duplicate (`Iceland` vs `iceland`). It stays free-text, so new and `/`-hierarchical tags still work; suggestions are fetched once per profile connection and filtered client-side, and already-entered tags drop out of the list.
- **Clearer "Tag each import session" copy.** The Import-defaults toggle now states the tag is created **in Immich** and shows a live example of the exact tag it would generate (`{immich-go}/YYYY-MM-DD HH-MM-SS`), removing the ambiguity about whether it was a local-only label.

### Import
- **"Import again" from History.** Each history entry can replay its original run: the profile, album, all import options (stacking, concurrency, date range, organization, tags, error handling, type/extension filters), and source are restored and staged for review — it never auto-starts, so the wipe/delete safety gate always gets a fresh look. Older records saved before request details were recorded show no action and report why.
- **OS notifications on import completion/failure.** A desktop notification fires when an import finishes or fails (cancellations stay silent), so you can walk away from a long migration. Uses the Tauri notification plugin; permission is requested once.
- **Wiped source files go to the OS Trash instead of being hard-deleted.** The verify-before-wipe SHA-1 gate is unchanged — only server-confirmed files are removed — but a mistaken wipe is now recoverable from the Trash rather than gone.
- **A changed file is never wiped.** Verify-before-wipe now carries each file's size and modification time from the moment its contents were hashed, and re-checks them immediately before the delete. If anything rewrote the file in between — a camera sync, an editor autosave, a still-finishing copy — the server holds the *old* bytes while the card holds new ones, so the file is kept and reported instead of trashed.
- **Wipe candidates must come from the scanned source.** The list of "successfully uploaded" paths is now re-contained under the chosen source folders before it can become a delete list. immich-go's log writes filenames unescaped, so a file or folder whose name contains a newline could forge a complete log record naming an unrelated path; such a path can no longer reach the wipe prompt.
- **"Only new since last import" no longer skips media after an empty run.** The checkpoint is advanced only when a run actually processed the source: at least one asset landed and no aggregate scan error occurred. Importing an empty folder — or one where filters excluded everything — used to record "now" as the last import, permanently hiding any media added later with an older capture date. A source immich-go could not enumerate also no longer counts as fully imported (its errors were previously dropped entirely).
- **Cancelling wins.** A cancel that lands while a run is finishing can no longer be overwritten by that run's final status, so a cancelled import cannot reappear as Completed or offer its originals for deletion.

### Safety
- **One app instance per machine.** Launching the app again now focuses the running window instead of starting a second copy. Import admission, the profile/history stores, and log rotation are all per-process, so two instances could import and wipe the same card at once, overwrite each other's profiles and history, and delete a live run log.

### Maintenance
- **Dependency bumps**: svelte 5.56.8, @playwright/test 1.62.1, @internationalized/date 3.12.3, keyring 4.1.6, plus postcss 8.5.25 and undici 7.29.0 to clear two high-severity advisories in build tooling (nothing shipped in the bundle was affected).

## v0.5.0 - 2026-07-23

### UI
- **The footer action bar is always visible.** The app now pins to the viewport height with the content area scrolling internally, so the footer (import stats, Logs, Start Import) no longer slides off the bottom of a long page. Absolutely-positioned controls inside the scroll area are correctly clipped instead of overflowing the layout and scrolling the whole window.
- **"Open in Immich" deep-links.** A finished import's queue card and any selected album now offer an "Open in Immich" action that opens the album (`/albums/{id}`) — or the timeline (`/photos`) when there's no single album target — in your browser, using the reachable server URL (LAN/WAN failover, same as imports). Closes the import loop so you can jump straight to verifying uploads.

### Import options
- **Keep going on errors (default on).** Imports now pass immich-go `--on-errors=continue` so a single bad file no longer aborts a multi-thousand-file migration; failures are still listed per-file afterward. Turn the switch off to stop at the first error.
- **Replace existing on server.** Optional `--overwrite` re-uploads assets the server already holds instead of skipping them, for re-syncs.
- **Tags & session tagging.** Apply comma-separated `--tag` values (with `/` hierarchy) to every uploaded asset, and optionally add a timestamped `--session-tag` to label the whole batch.
- **Only import media newer than last import.** Opt-in toggle that derives a capture-date floor from this source's last import (via `--date-range`), turning a repeat import of a large card from a full re-scan into a fast incremental. The checkpoint is scoped per profile and advanced only by clean, complete imports (failed or partial runs never raise the floor), and is computed in the local calendar zone to avoid a timezone off-by-one. Server-side dedupe still guards the boundary; filters by EXIF capture date, so wrong camera clocks may skip files.
- **Type & extension filters.** Import only Photos or only Videos, and add comma-separated include/exclude extension lists — mapped to immich-go's `--include-type`/`--include-extensions`/`--exclude-extensions` instead of hand-selecting files.
- **Check server (pre-import forecast).** A read-only preflight that hashes the selected/scanned files and asks the server how many it already holds, showing "X to upload, Y already on server" before you start — reuses the verify-before-wipe SHA-1 + bulk-upload-check path.

### Onboarding
- **Scan network for your Immich server.** The profile editor can sweep the local `/24` (ports 2283/443/80) and list confirmed servers for one-click fill, so first-run setup no longer requires knowing the server's IP. Confirmation uses the unauthenticated ping endpoint only — the API key is never sent during discovery.

### Maintenance
- **Dependency bumps**: svelte 5.56.7, @lucide/svelte ^0.577.0, @internationalized/date 3.12.2, serde 1.0.229, serde_json 1.0.151, plus CI action-digest updates (actions/checkout, dtolnay/rust-toolchain, tauri-apps/tauri-action).

## v0.4.0 - 2026-07-23

### Import safety & data integrity
- **A local original is never deleted when the server's only copy is in the trash.** Verify-before-wipe now excludes `bulk-upload-check` results flagged `isTrashed`; previously a checksum whose sole server copy was soft-deleted counted as "safely uploaded", so the last live original was wiped and lost once the server trash was emptied.
- **Files the server already holds now join the post-import wipe candidate list** (still gated by the per-file SHA-1 existence check before any deletion); the `uploaded` counter stays a separate tally.
- **A single unstageable file no longer aborts a selected-subset import** — staging skips the failed file and continues, failing the run only if nothing could be staged. Same-named files chosen from drives with no common ancestor no longer overwrite each other (collisions nest under a numeric subfolder).
- **Staging is now cancellable.** Clicking Cancel during a large selected-subset import stops the copy-fallback staging loop instead of running it to completion before the uploader notices.

### Scanning & preview
- **Source scans stream in with a live "N found" count and a Cancel button** instead of freezing behind one all-at-once result, so large libraries stay responsive and a scan of a slow/huge tree can be stopped.
- **Overlapping source folders are de-duplicated.** Selecting a parent and its child no longer scans or uploads the shared files twice (roots are collapsed and files de-duplicated by canonical path).
- **Closing or replacing the preview cancels its in-flight backend work**, so rapidly opening/closing previews on a big folder no longer keeps generating thumbnails and dates nobody will see.
- **Date-range preview filtering is timezone-correct.** Day boundaries are parsed as UTC to match the backend's UTC EXIF epochs, so photos captured near midnight aren't filtered into the wrong day outside UTC.

### Reliability
- **Crash-safe cleanup of per-run temp artifacts**, with a cross-process ownership lease: interrupted imports no longer leave staging dirs, API-key config dirs, or run logs behind, and startup cleanup uses an advisory lock so a second running copy of the app can never delete a live import's files.
- **A stalled network/USB mount no longer hides other cards.** Each removable-device DCIM probe is bounded (500 ms), so one sleeping SMB/NFS/external drive can't block detection of a freshly-inserted SD card.
- **The upload sidecar is always reaped and teardown can't hang**: cancelling, an unexpected event-channel close, or a sidecar error now kill and wait for the immich-go child within a bounded window instead of leaking a zombie or blocking the quit path.
- **Import lifecycle hardening**: only one import starts at a time; a job is published only after its fallible setup succeeds (no ghost "running" jobs); cancel/retry are guarded by job status; and in-memory job history and retry inputs are bounded.
- **Thumbnail work is memory- and time-bounded**: explicit decode dimension/allocation limits, a capped RAW embedded-JPEG scan, timed-out `sips`/`qlmanage` subprocesses (with partial-output cleanup), and cache pruning that runs during a session without evicting in-flight files.
- **Live import progress no longer flickers** — the queue poll can't reset a running job's bar/ETA to a stale start-of-run value, and a late progress event can't revive a finished job.
- **Auto-import no longer suppresses sibling cards.** Inserting two cards at once (or one while a prompt is open) now prompts each in turn instead of marking the extras "seen" forever.
- **Import history can always be reset.** A corrupt/unparseable store no longer blocks "Clear history" (it overwrites the bad file), and a panic while the store lock is held no longer disables history for the session (the lock recovers from poisoning). "Clear history" now also clears the per-source "last imported" metadata so the badge doesn't contradict a cleared history.

### Correctness
- **LAN/WAN URLs are normalized before they reach immich-go.** A LAN/WAN address with a trailing slash or `/api` suffix used to pass the connection probe (which normalizes internally) but break the sidecar; both are now normalized at save time.
- **immich-go per-file paths resolve against your source folders**, so a source directory containing a colon on macOS/Linux is parsed correctly and its files are verified/wiped instead of being silently skipped.
- **Windows verbatim UNC paths no longer break "last imported"** — a canonicalized `\\?\C:\…` path and its non-canonical fallback now produce the same store key.
- **Config and history temp files are cleaned up on failed writes** instead of leaking `config.json.*`/`store.json.tmp` in the app data directory.
- **Concurrent profile edits no longer lose data**: profile upsert/delete serialize their keychain change together with the `config.json` read-modify-write, so two simultaneous saves of the same profile can't clobber each other's key.
- **Album/user/server-info commands honor LAN/WAN failover**, resolving the reachable endpoint like imports do, instead of always hitting the primary URL.
- **Immich API calls try the `/api` path first**, abort the candidate loop on any non-404 (so an authentic 401/403 surfaces), and never replay a non-idempotent write (album/share creation) on a transport error, preventing duplicate albums/links.
- **An unreadable config surfaces an error instead of looking empty**, so a permissions/IO failure isn't mistaken for first-run and overwritten.
- **`app.log` is excluded from run-log rotation and size-capped** (trimmed to the newest lines) so it can neither be deleted as the oldest file nor grow without bound; log parsing is char-boundary-safe and error counts are per-file.

### Security & hardening
- **Path authorization tightened** on the preview/scan/staging boundary (with regression tests confirming a sibling-prefix path like `/src-evil` is not treated as inside `/src`), and the approved-source-root allowlist is bounded and reset on a fresh selection.
- **Per-run API-key config carries restrictive permissions and an ownership lease**, and stale credential-bearing temp dirs from interrupted runs are pruned at startup.

### Maintenance
- **keyring 3 → 4**: the macOS credential path now links a single `security-framework` (3.7.0), removing the dual-major (2.x + 3.x) split that shipped in the lockfile. API is unchanged; not-found handling uses the typed `Error::NoEntry` variant.
- **Frontend bundle split** into separate vendor/tauri/svelte chunks for a smaller initial parse and independent caching.
- **Aligned the `@tauri-apps/*` npm packages (api 2.11.1, CLI 2.11.4) with the Rust `tauri` 2.11 crate**, fixing the tauri-cli version-mismatch that blocked `tauri build`/`dev`.

## v0.3.0 - 2026-07-12

### Import organization
- New **folder-to-album/tag organization** for imports, so a nested library can be preserved on the server instead of collapsing into one album. Import options now offer: **Single album** (default, unchanged), **Album per folder name**, **Album per folder path**, and **Tag by folder path** — mapped to immich-go `--folder-as-album=FOLDER|PATH`, `--album-path-joiner`, and `--folder-as-tags` (previously hardcoded to `--folder-as-album=NONE`). In the folder modes the album picker is bypassed; the single-album mode keeps honoring the selected `--into-album`.

### Automation
- **Per-device auto-import rules**: teach each camera card its own destination once and re-inserting it replays the whole setup. A saved rule (kept per card, keyed by volume label with a mount-path fallback) records the target **profile, album, keep/wipe policy, stacking, and organization mode**. When a card with a rule is inserted, the auto-import banner shows its target and one click imports with those settings; a new "Remember settings for this card" control in Import options saves, updates, or forgets a card's rule. Cards without a rule keep the previous safe default (active profile, no album, originals kept). Deletion still goes through the separate verify-before-wipe step.

### Security
- Public album **share links now default to `showMetadata: false`**, so a public link no longer exposes capture/location metadata; the payload is built by a tested helper.
- **Album sharing defaults to the Viewer role**: the create-album dialog gained a Viewer/Editor access selector (defaulting to least-privilege Viewer) threaded through to the `album_share_users` command, which validates the role server-side — previously every shared user was silently granted Editor. The `album_id` is percent-encoded as a single path segment so a renderer-supplied id can't smuggle `/` or `../` into the authenticated request path.
- **LAN/WAN failover now verifies server identity**: the resolver only switches to an alternate endpoint after an unauthenticated `/server/ping` confirms it is a real Immich server, instead of switching on bare TCP port reachability — so the API key and uploads are never routed to an unrelated service merely listening on the configured host:port. Plaintext HTTP endpoints remain fully supported.
- The immich-go **API-key config** is now written into a fresh random per-run directory (0700 on unix) with exclusive `create_new` + 0600, instead of a predictable shared-temp path, so a local user can't pre-create or symlink-hijack it.
- The immich-go **run log** dropped from `DEBUG` to `INFO` (DEBUG can echo an `x-api-key` header), the log file is pre-created 0600, and the logs directory is 0700 on unix.
- Removed the unused `opener:default` renderer capability (the only opener use is a fixed-path Rust command), narrowing the renderer's OS-opener surface.
- **Supply chain**: every third-party GitHub Action in `build.yml`/`release.yml` is pinned to a full commit SHA (matching `ci.yml`), and the `immich-go` sidecar download verifies a SHA-256 pinned in the repo rather than a checksum fetched from the same mutable release.

### Performance
- Blocking I/O moved off the async executor via `spawn_blocking`: recursive source scans (`WalkDir`), removable-device polling (disk refresh + directory probes), and LAN/WAN URL resolution no longer stall the runtime or the IPC path — `import_start` returns the job id immediately instead of blocking on endpoint probing.
- RAW **preview extraction is memory-bounded**: instead of loading a whole 20–100 MB RAW to find its embedded JPEG, the file is streamed with a 64 KB rolling buffer, cutting per-file memory from ~100 MB to a few MB (concurrent 8-file scans no longer spike ~800 MB).

### Fixes
- The Immich API client aborts its URL-candidate loop on any non-404 status, so an authentic 401/403 (e.g. an expired API key) is surfaced instead of being masked by the next candidate's 404.
- A just-stored keychain credential is rolled back when a new-profile save fails (no orphaned keys under unreferenced UUIDs), and a profile is removed before its key so a failed delete can't leave a broken keyless profile.
- Removed dead persisted `recent_album_ids` config state (round-tripped to disk but never read or written).

### Maintenance
- Replaced `once_cell::sync::Lazy` with `std::sync::LazyLock` throughout and dropped the direct `once_cell` dependency.
- Extracted pure, unit-tested helpers on the sidecar argument builder, import-run classification, media scanner, and upload-rate math; added coverage for share roles, path-segment encoding, folder-organization flag mapping, the device-rules store, rule pre-fill/replay, and `startImport` overrides.

## v0.2.0 - 2026-07-10

### Compatibility
- Bumped the bundled **immich-go** upload engine from 0.31.0 to **0.32.0**, which adds full **Immich v3.0.0** compatibility (server-version detection; drops the `deviceId`/`deviceAssetId` upload fields removed from v3's `AssetMediaCreateDto`; V2/V3-aware error parsing). immich-go 0.31.0 sent the old upload payload and would fail against a v3 server. immich-go 0.32.0 remains backward-compatible with Immich v2.

### Security
- Bumped transitive dependencies to clear three RustSec advisories flagged by `cargo audit`: `quick-xml` 0.38.4 → 0.41.0 (via `plist` 1.8.0 → 1.10.0) fixing RUSTSEC-2026-0194/0195 (two high-severity XML-parser DoS issues), and `crossbeam-epoch` 0.9.18 → 0.9.20 fixing RUSTSEC-2026-0204. Lockfile-only; no manifest changes.
- Hardened file-system boundaries from a security audit: preview and scan commands now honour a source allowlist so a compromised renderer can't read arbitrary local files over IPC; staged relative paths are stripped of `..`/root components and containment-checked to block writes escaping the temp staging dir; renderer-supplied `select_files` are re-validated against approved source roots before staging; and symlinks are skipped during scans so links pointing outside the selected source can't be staged or uploaded.
- Fixed data-loss and correctness bugs: history is no longer wiped when the store file is locked or corrupt (aborts instead of overwriting with empty state); a failed post-import wipe verification now retains the pending payload so the delete can be retried instead of being silently dropped; wipe existence checks target the resolved upload URL after failover; `concurrent_tasks` is clamped to 1–20; the true (uncapped) error count is reported on mass-failure runs; and impossible EXIF timestamps fall back to file mtime.

### Branding
- New original app icon and in-app logo — the "Send-lens" mark (an open lens ring with an upward arrow, reading as *sending photos into Immich*) in the indigo→teal brand gradient — replacing the default Tauri scaffold logo. The full macOS/Windows/Linux icon set is regenerated from it; editable SVG masters live at `src/lib/assets/logo.svg` and `src-tauri/icons/icon.svg`.

### Design
- Depth pass: cards now lift off the dark canvas (layered shadow + top highlight), a subtle brand glow sits behind the workspace, and a gradient hairline underlines the header.
- The empty source dropzone gets a brand-gradient icon and a brand-tinted dashed border; section headers (Source, Import options, Albums) carry tinted icon chips; the profile avatar gains a brand-gradient ring.
- Removable devices now show a storage **capacity bar** (teal→indigo, turning red past 90% full).
- "Start Import" is now the gradient primary call-to-action.
- Moved "Auto-import on card insert" from the Source panel into Import options, styled consistently with the other toggles.

### Layout
- Reworked the main window for wide displays: content is now capped to a comfortable width and centered (no more edge-to-edge sprawl), and the right column carries Albums **plus** the Queue/History so it fills the space instead of leaving a tall empty gap next to the import options. Reflows cleanly to a single column on narrow windows.
- macOS now uses a frameless **overlay title bar** — just the traffic-light controls, no title strip — with the app header doubling as the drag region and reserving space so the brand clears the lights. Other platforms are unaffected.

### Preview & selection
- New pre-import **preview grid**: click "Preview & select" on a scanned source to see your media as a thumbnail grid and pick exactly what to import. Thumbnails are generated on demand and cached — on macOS via the OS (`sips` for photos incl. HEIC/RAW, Quick Look for video); on Windows/Linux via a built-in decoder for JPEG/PNG/TIFF/WebP/GIF/BMP **plus camera RAW** (CR2/CR3/NEF/ARW/RAF/RW2/ORF/DNG…), where the largest embedded JPEG preview is extracted — pure Rust, no RAW decoder. On **Windows**, HEIC and video are additionally thumbnailed natively via the Shell thumbnail API (`IShellItemImageFactory`) — the same previews Explorer shows (video via Media Foundation; HEIC when the HEIF Image Extensions are installed), falling back to a typed placeholder when no OS thumbnail handler is present. HEIC/video still fall back to a placeholder tile on Linux. Selecting a subset stages just those files (via symlinks) for upload and always keeps the originals.
- The preview grid can sort by **capture date** (EXIF `DateTimeOriginal`, falling back to file modification time) as well as by name, so you can review a shoot newest-first.
- New **date-range import filter**: From/To pickers (with Clear) in Import options, validated so From ≤ To and forwarded to immich-go as `--date-range=YYYY-MM-DD,YYYY-MM-DD`, so you can import only media captured within a chosen window.

### Automation
- Optional "Auto-import on card insert": when enabled (off by default), inserting a removable card that contains a DCIM folder surfaces a "card detected — import now?" banner with a one-click Start. Accepting imports to the active profile with no albums and source files always kept (deletion stays a separate, explicit, verified step); nothing uploads or deletes without your action. Toggle lives in the Source panel.

### Error reporting
- Failed imports now list *which* files failed and *why*, not just an aggregate count: immich-go's per-file errors are parsed from its run log and shown as a scrollable list (filename + reason) under the failed job in the import queue, and mirrored into the in-app log viewer (`import_error` lines). Capped at 100 entries per run.

### Tooling
- CI now runs the full test suite in a dedicated job — svelte-check, Vitest, `cargo test`, and Playwright e2e — not just fmt/clippy/build
- Added `npm run verify` (full CI mirror) and `npm run verify:fast`, plus version-controlled git hooks (`.githooks`, wired via `core.hooksPath` on install): a fast **pre-commit** (svelte-check + Vitest + rustfmt) and a full **pre-push** (everything CI runs) to keep CI green
- The `immich-go` sidecar download now verifies each archive's SHA-256 against the upstream release `checksums.txt` before extracting, failing the build on any mismatch
- Per-push CI builds Linux + Windows and runs the full test suite (svelte-check, Vitest, `cargo test`, Playwright) on Linux; macOS bundles build on `v*` release tags via the release workflow, to conserve Actions minutes. Bumped CI to Node 22 and `actions/*@v5`.
- Bumped the `tauri` crate 2.10.2 → 2.11.5 so the Rust runtime tracks the same 2.11 minor as `@tauri-apps/api`, resolving the tauri-cli version-mismatch that was failing the Windows/Linux release builds.
- Moved `renovate.json` into `.github/` and pruned a stale internal scoping doc from `docs/`.

### Distribution
- Release workflow now publishes prebuilt installers (macOS `.dmg`, Linux `.AppImage`/`.deb`, Windows `.exe`) to GitHub Releases on each `v*` tag
- macOS bundles are ad-hoc signed (`signingIdentity: "-"`) so they run on Apple Silicon after a one-time Gatekeeper "Open Anyway"; added a documented (disabled) Apple notarization hook in the release workflow and updated the install/Gatekeeper docs in the README

### Performance
- Optional "Parallel uploads" control in Import options (1–20) that sets immich-go's `--concurrent-tasks`; leave blank to use the default (CPU-core count)

### Diagnostics
- In-app log viewer: the footer "Logs" button now opens a dialog showing recent application-log activity (new `get_recent_logs` command) with Refresh, Copy, and Open-folder actions, instead of only opening the logs folder

### Filtering
- Optional date-range import filter: pick a From/To date in Import options to import only media captured in that range (passed to immich-go as `--date-range=YYYY-MM-DD,YYYY-MM-DD`); leave it empty to import everything

### Safety
- Verify before wipe: when deleting source files after an import, each file's SHA-1 is checked against the Immich server (`POST /api/assets/bulk-upload-check`) and only files the server confirms it holds are deleted; unverified files are kept. If verification can't run (server unreachable), all files are kept.

### Import history & persistence
- Persist import history across app restarts in a JSON store under the app data dir (was in-memory only); new `history_list`/`history_clear` commands
- New History tab beside the queue listing past imports with status, timestamp, source, and per-import stats
- Per-source "last imported" indicator in the source picker; relies on immich-go's server-side checksum dedupe to skip already-uploaded files on repeat imports (verified: immich-go v0.31.0 has no timestamp-since filter, so no misleading "only new" toggle was added)

### Job lifecycle & queue
- Retry failed imports, dismiss individual finished jobs, and clear all finished jobs (new `import_retry`/`import_dismiss`/`import_clear_finished` commands; the original input is persisted per job for retry)
- Live throughput (items/sec), ETA, and the current/last file being imported on running jobs

### Source & options
- Remove individual selected source paths (not just clear-all), with a re-scan of the remainder
- Import options now use proper Switch toggles via a new `ui/switch` primitive

### Onboarding & window
- Onboarding is now a real two-step wizard (connect → "you're connected" → get started) instead of force-closing on first save
- Set a minimum window size (720×560) so the layout stays usable when resized small

### Accessibility
- Added descriptive `aria-label`s to icon-only controls and `aria-live` status regions for the import queue and toasts

- Redesigned the entire UI around an Immich-indigo brand identity (light/dark/system themes)
- Surfaced the import queue as a dedicated panel with per-job progress bars and duplicate/error stats
- Reworked the app shell: header brand mark, sticky footer action bar (live status + Start Import), and a clearer two-column layout
- Rebuilt the source picker with a drag-and-drop dropzone, removable-device cards (free space + DCIM), and a media scan summary
- Polished onboarding into a branded first-run flow with connection testing and validation states
- Replaced the native profile `<select>` with a profile-switcher dropdown menu
- Restyled import options as descriptive toggle rows with a destructive-action warning, and toasts with per-level icons and animations
- Added a browser design-preview harness (mocked Tauri backend + scenarios) for visual UI inspection; dev-only and excluded from production builds
- Removed stale compiled `.js`/`.js.map` artifacts from `src/` that were shadowing TypeScript sources and breaking production builds
- Fixed: the "Stack RAW+JPEG" and "Stack burst" toggles are now sent to the backend (threaded through `ImportInput` → sidecar `--manage-raw-jpeg`/`--manage-burst`); previously they had no effect
- Fixed: the public album share link is now shown with a copy action instead of being discarded after creation
- Replaced the blocking native wipe confirmation with an in-app Delete/Keep confirmation in the queue panel
- Added Playwright end-to-end tests covering every design-preview scenario

## v0.1.0

- Scaffolded Tauri v2 + Svelte 5 desktop app
- Added profile, source, album, options, queue, and onboarding UI shells
- Added Rust services for config persistence, key storage, Immich API access, sidecar execution, scanning, and URL resolution
- Added CI workflows for build and release matrix targets
