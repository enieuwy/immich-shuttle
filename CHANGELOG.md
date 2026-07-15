# Changelog

## Unreleased

### Fixes
- **Duplicate files are now wiped from the source drive.** Files the server already had ("server has duplicate") were counted but never added to the post-import wipe candidate list, so they were left behind on the SD card even when the user chose to delete originals. They now join the candidate list (still gated by the per-file SHA-1 existence check before any deletion); the `uploaded` counter stays a separate tally.
- **Import history can always be reset.** A corrupt/unparseable history store no longer blocks "Clear history" (it falls back to an empty store and overwrites the bad file), and a panic while the store lock is held no longer permanently disables all history/metadata operations for the session (the lock recovers from poisoning).
- **A single unstageable file no longer aborts a selected-subset import** — staging skips the failed file and continues, failing the run only if nothing could be staged. Same-named files chosen from drives with no common ancestor no longer overwrite each other in the staging dir (collisions are nested under a numeric subfolder).
- **Album creation survives a failed share.** If sharing or public-link creation fails after the album is created on the server, the album is still registered and selected in the UI (no orphaned/duplicate albums) with a specific warning, instead of throwing a generic error and desyncing state.
- **Date-range preview filtering is timezone-correct.** Day boundaries are parsed as UTC to match the backend's UTC EXIF capture epochs, so photos captured near midnight are no longer filtered into the wrong day in browsers outside UTC.
- **Album/user/server-info commands honor LAN/WAN failover**, resolving the reachable endpoint like imports do, instead of always hitting the primary URL and failing when only a failover server is up.
- **Concurrent profile edits no longer lose data**: profile upsert/delete now serialize their read-modify-write of `config.json`.
- **Auto-import no longer suppresses sibling cards.** Inserting two cards at once, or a card while another prompt is open, previously marked the extra cards "seen" forever; now only the surfaced card is marked seen and the others prompt in turn.
- **Live import progress no longer flickers.** The 2-second queue poll took the field-wise maximum of polled vs. event-driven progress for running jobs, so it can't reset the bar/ETA to the stale start-of-run value.
- Immich API calls try the `/api` path first, eliminating a wasted request (a 404 or HTML 200) that previously preceded every metadata/validation call.
- The persistent `app.log` is excluded from run-log rotation, so it can't be deleted as the oldest file.

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
