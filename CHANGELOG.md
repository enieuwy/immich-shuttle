# Changelog

## Unreleased

### Layout
- Reworked the main window for wide displays: content is now capped to a comfortable width and centered (no more edge-to-edge sprawl), and the right column carries Albums **plus** the Queue/History so it fills the space instead of leaving a tall empty gap next to the import options. Reflows cleanly to a single column on narrow windows.
- macOS now uses a frameless **overlay title bar** — just the traffic-light controls, no title strip — with the app header doubling as the drag region and reserving space so the brand clears the lights. Other platforms are unaffected.

### Preview & selection
- New pre-import **preview grid**: click "Preview & select" on a scanned source to see your media as a thumbnail grid and pick exactly what to import. Thumbnails are generated on demand and cached — on macOS via the OS (`sips` for photos incl. HEIC/RAW, Quick Look for video), elsewhere via a built-in decoder for JPEG/PNG (RAW/HEIC/video show a typed placeholder tile). Selecting a subset stages just those files (via symlinks) for upload and always keeps the originals.
- The preview grid can sort by **capture date** (EXIF `DateTimeOriginal`, falling back to file modification time) as well as by name, so you can review a shoot newest-first.

### Automation
- Optional "Auto-import on card insert": when enabled (off by default), inserting a removable card that contains a DCIM folder surfaces a "card detected — import now?" banner with a one-click Start. Accepting imports to the active profile with no albums and source files always kept (deletion stays a separate, explicit, verified step); nothing uploads or deletes without your action. Toggle lives in the Source panel.

### Error reporting
- Failed imports now list *which* files failed and *why*, not just an aggregate count: immich-go's per-file errors are parsed from its run log and shown as a scrollable list (filename + reason) under the failed job in the import queue, and mirrored into the in-app log viewer (`import_error` lines). Capped at 100 entries per run.

### Tooling
- CI now runs the full test suite in a dedicated job — svelte-check, Vitest, `cargo test`, and Playwright e2e — not just fmt/clippy/build
- Added `npm run verify` (full CI mirror) and `npm run verify:fast`, plus version-controlled git hooks (`.githooks`, wired via `core.hooksPath` on install): a fast **pre-commit** (svelte-check + Vitest + rustfmt) and a full **pre-push** (everything CI runs) to keep CI green

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
