# Epic scope: Pre-import media preview & selection grid (`immich-shuttle-rcv`)

Status: scoping. Two decisions block a clean breakdown (see end).

## Goal
Before importing, let the user *see* their media as a thumbnail grid and choose
exactly what to import — instead of importing an entire folder blind.

## What already exists (reuse)
- `scan_sources(paths)` → `ScanResult { files: MediaFile[], … }`. `MediaFile`
  already carries `{ path, name, extension, size_bytes, is_video }`. **The grid's
  data backbone is done** — we have the file list; we need thumbnails + selection.
- `SourcePicker` shows scan counts; `ImportOptions` has date-range/type filters.
- Import path: `queueState.startImport` → `import_start` → immich-go
  `upload from-folder <dir>` (one folder/zip arg per run).

## The two hard problems

### 1. Thumbnails (the bulk of the work)
Rust `image` crate (pure Rust) decodes JPEG/PNG/TIFF/WEBP/BMP/GIF. EXIF
orientation + capture date via `kamadak-exif` (pure Rust). **But** a camera card
(DCIM) is mostly **HEIC, RAW (CR2/CR3/NEF/ARW/RW2/RAF…), and video (MOV/MP4)** —
none decodable by `image`:
- HEIC → needs libheif (C dep).
- RAW → needs libraw/`rawler` (heavy) OR extract the embedded JPEG preview most
  RAWs carry (still needs a RAW-aware reader).
- Video → needs ffmpeg to grab a frame.

So thumbnail coverage is a cost/value fork (Decision A).

Regardless of coverage, the engine must:
- run off the UI thread (rayon/thread pool, bounded concurrency),
- **stream** results — emit a `thumbnail-ready` event per file as it finishes so
  the grid fills progressively (thousands of files),
- cache to app-data (`thumbnails/<sha1(path+mtime)>.jpg`) so re-scans are instant,
- be cancellable (user changes source / closes).

### 2. Selection → import (Decision B)
immich-go `from-folder` has **no per-file selection** — only
`--include/exclude-extensions`, `--include-type`, `--date-range`, `--ban-file`
patterns. To import an arbitrary selected subset:
- **Staging dir of symlinks** (recommended): build a temp dir mirroring selected
  files via symlinks, run `from-folder` on it, clean up. Works with immich-go
  unchanged. Caveat: Windows symlinks need privilege; fall back to hardlinks
  (same-volume only) or copy.
- **Invert to `--ban-file` patterns**: fragile for large/arbitrary selections.
- **Per-file invocations**: many immich-go runs; loses batching/dedupe view.

## Proposed v1 (recommended)
- **Thumbnails: JPEG/PNG/TIFF/WEBP via `image` + EXIF orientation/date via
  `kamadak-exif`.** HEIC/RAW/video get a typed placeholder tile (extension label +
  photo/raw/video icon). Ships real value for the common JPEG case with only two
  pure-Rust deps; heavy decoders deferred to a follow-up bead.
- **Selection → import: symlink staging dir.**
- Grid is **opt-in** (a "Preview" affordance on a scanned source), not forced, so
  large cards stay fast by default.

## Proposed child beads (after decisions)
1. `thumbnailer` service — decode+downscale+orient, cache, placeholders for
   unsupported types. Unit-tested on fixture images. *(deps: `image`, `kamadak-exif`)*
2. Streaming thumbnail command + `thumbnail-ready` events; cancellable.
3. `PreviewGrid.svelte` — virtualized grid, lazy thumbnails, per-tile checkbox,
   select all/none, live "N selected · M GB".
4. `selection` store + wiring into the source/import flow.
5. Selection → import staging (symlink dir) in `import_start`.
6. Dev harness `preview` scenario + fixtures; vitest (selection) + e2e.
7. (Deferred) HEIC/RAW/video thumbnails via native libs — separate bead.

## Decisions needed
- **A. Thumbnail coverage for v1:** JPEG/PNG-family only (placeholders for
  HEIC/RAW/video) — *recommended* — vs. full coverage now (libheif/libraw/ffmpeg,
  large native deps + cross-platform build burden).
- **B. Selection → import mechanism:** symlink staging dir (*recommended*) vs.
  ban-file inversion vs. per-file invocations.
