# Immich Shuttle

Immich Shuttle is a cross-platform desktop importer for Immich built with Tauri v2 + Svelte 5.

## Features

- Multi-profile management with live API key validation and connection testing
- Secure key storage using system keychain backends
- Source picker with a drag-and-drop dropzone, removable-media detection (with free-space and DCIM hints), folder/file selection, and a live media scan summary
- Album multi-select with inline album creation and sharing options
- Import queue panel with per-job progress bars, live throughput/ETA, duplicate/error stats, and cancel, retry, dismiss, and clear-finished actions
- Optional wipe mode that deletes only confirmed uploaded files, with an in-app safety warning
- Immich-branded UI with light, dark, and system themes
- LAN/WAN URL resolution using TCP probe

## Installation

Prebuilt binaries are published on [GitHub Releases](../../releases) for each tagged version:

- **macOS (Apple Silicon)** — `.dmg`
- **macOS (Intel)** — `.dmg`
- **Linux (x64)** — `.AppImage` / `.deb`
- **Windows (x64)** — `.exe`

> These builds are **ad-hoc signed but not notarized** (no paid Apple/Windows code-signing certificate). They run normally, but the OS shows a **one-time** "unverified developer" prompt on first launch — see the [FAQ](#faq) for the bypass. On Linux, make the AppImage executable first: `chmod +x Immich\ Shuttle_*.AppImage`.

## Quick Start

1. Launch Immich Shuttle.
2. Add your Immich server URL and API key.
3. Choose a source folder or removable device.
4. Select target albums (or create one inline).
5. Start import and monitor queue progress.

## Requirements

- Immich server v1.106+
- Valid Immich API key

## FAQ

### macOS says the app "can't be opened" or is from an unidentified developer

The builds are ad-hoc signed but not notarized, so macOS blocks them on first launch. This is a one-time step per install:

- **macOS 15 (Sequoia) and later:** open **System Settings → Privacy & Security**, scroll to the message naming "Immich Shuttle", click **Open Anyway**, then authenticate. (The old right-click → Open shortcut no longer works for unsigned apps.)
- **Older macOS:** right-click (Control-click) the app in Finder, choose **Open**, then confirm.

Notarized builds with no prompt require a paid Apple Developer account.

### Windows SmartScreen warning appears

Choose More info, then Run anyway.

### Linux keychain error

Install and unlock a Secret Service provider like `gnome-keyring` or `kwallet`.

## Development

### Prerequisites

- Node.js 20+
- Rust stable toolchain

### Run locally

```bash
npm install
./scripts/download-sidecar.sh
npm run tauri dev
```

### Design preview (visual UI inspection)

The frontend can run in a normal browser against a mocked Tauri backend, so the
full UI can be inspected and tuned without a real Immich server:

```bash
npm run dev
# open http://localhost:1420 in a browser
```

When no Tauri runtime is present, the app loads fixture data and renders every
screen. Switch states with the `scenario` query param:

- `?scenario=default` — populated source, albums, and a finished/failed job
- `?scenario=onboarding` — first-run server-connection flow
- `?scenario=importing` — an in-progress import with live progress bars
- `?scenario=wipe` — a job awaiting wipe confirmation
- `?scenario=empty` — empty source dropzone and queue

The preview code (`src/lib/dev/*`) is dev-only and is excluded from production
Tauri builds.

### Verify

Run the full suite that CI runs (svelte-check, Vitest, rustfmt, Clippy, `cargo test`, build, Playwright e2e):

```bash
npm run verify        # full local mirror of CI
npm run verify:fast   # quick subset: svelte-check + Vitest + rustfmt
```

Git hooks (installed automatically on `npm install` via `core.hooksPath`) run these for you: a fast **pre-commit** (`verify:fast`) and a full **pre-push** (`verify`), so a clean local run means green CI. Bypass once with `git commit --no-verify` / `git push --no-verify`. The pre-push e2e step needs the Playwright browser once: `npx playwright install chromium`.

## Contributing

PRs are welcome. Keep changes focused, include verification output, and update docs when behavior changes.

## License

MIT
