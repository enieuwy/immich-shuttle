# Changelog

## Unreleased

### Job lifecycle & queue
- Retry failed imports, dismiss individual finished jobs, and clear all finished jobs (new `import_retry`/`import_dismiss`/`import_clear_finished` commands; the original input is persisted per job for retry)
- Live throughput (items/sec) and ETA on running imports

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
