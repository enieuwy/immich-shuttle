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

Download artifacts from GitHub Releases:

- macOS (Apple Silicon): `.dmg`
- macOS (Intel): `.dmg`
- Windows (x64): `.exe`
- Linux (x64): `.AppImage`

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

### macOS says the app is from an unidentified developer

Right-click the app and choose Open.

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

```bash
npm run check
npm run build
cargo build --manifest-path src-tauri/Cargo.toml
```

## Contributing

PRs are welcome. Keep changes focused, include verification output, and update docs when behavior changes.

## License

MIT
