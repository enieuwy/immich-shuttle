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

#### Decoder research (2026-06, source-verified) — Decision A
No clean MIT/Apache, broad-format, pure-Rust thumbnail suite exists. Findings:

| Format | Best Rust option | Pure Rust? | License | Verdict |
| --- | --- | --- | --- | --- |
| JPEG/PNG/TIFF/WebP/BMP/GIF | `image` + `kamadak-exif` (orientation/date) | yes | MIT/Apache | ✅ clean, use it |
| RAW (Canon/Nikon/Sony/Fuji/…) | `rawler` (dnglab) — extracts embedded preview | yes | **LGPL-2.1** | ✅ viable for an OSS app; maintained (0.7.2, Feb 2026), broad incl. Canon |
| RAW (alt) | `quickraw` | yes | LGPL-2.1 | ⚠️ **no Canon**, last release 2023 — weaker than rawler |
| RAW (alt) | `raw_preview_rs` | no (bundles LibRaw+libjpeg-turbo, needs CMake) | GPL-3.0 | ❌ heavy C build + GPL |
| HEIC/HEIF | `imazen/heic` (pure-Rust HEVC) | yes | **AGPL-3.0** + patent notice | ❌ AGPL forces whole app to AGPL — blocker |
| HEIC/video | OS frameworks (macOS ImageIO/`sips`/AVFoundation, Windows WIC) | n/a (FFI/CLI) | OS-shipped | ⚠️ clean licensing + broad coverage, but per-OS work; Linux has no universal decoder |
| Video frame | ffmpeg (`video-rs`/`ffmpeg-next`) or OS | no | LGPL/GPL | ❌ heavy; no mature pure-Rust frame decode |

Notes:
- **LGPL-2.1 is acceptable here** because immich-shuttle is open source with public
  build instructions, which satisfies LGPL's relink obligation. **AGPL is not.**
- `rawler` reads the embedded preview JPEG most RAWs carry — fast, no demosaic
  needed for a thumbnail.
- macOS ships `sips`/`qlmanage -t` (Quick Look) that thumbnail HEIC/RAW/video for
  free — a pragmatic mac-only fast path, but shelling out and mac-only.

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

## Proposed v1 (revised after research)
- **Thumbnails: `image` (JPEG/PNG/TIFF/WebP) + `rawler` (RAW embedded preview) +
  `kamadak-exif` (orientation/date).** This covers the two dominant camera-card
  formats — JPEG and RAW (incl. Canon) — with maintained pure-Rust libs and no
  AGPL. **HEIC and video get a typed placeholder tile** (icon + extension + size).
- **HEIC/video real thumbnails: deferred to a phase-2 bead** via OS-native
  decoders (macOS ImageIO/`sips`, Windows WIC/AVFoundation) behind a trait, so we
  avoid AGPL and bundled C entirely. Linux keeps placeholders for those two.
- **Selection → import: symlink staging dir** (Decision B, confirmed).
- Grid is **opt-in** (a "Preview" affordance on a scanned source), not forced, so
  large cards stay fast by default.

Accepted dependency cost: `image` (MIT/Apache) + `rawler` (LGPL-2.1, acceptable
for this OSS app) + `kamadak-exif` (MIT/Apache). Document the LGPL component in
the README/licensing notes.

## Proposed child beads
1. `thumbnailer` service — for each `MediaFile`: JPEG/PNG/etc via `image`; RAW via
   `rawler` embedded preview; orientation/date via `kamadak-exif`; placeholder
   marker for HEIC/video. Downscale (~256px), cache to
   `app-data/thumbnails/<sha1(path+mtime)>.jpg`. Bounded thread pool. Unit-tested.
2. Streaming command + `thumbnail-ready {path,cachePath,w,h,capture_date}` events;
   cancellable.
3. `PreviewGrid.svelte` — virtualized grid, lazy thumbnails, per-tile checkbox,
   select all/none, live "N selected · M GB".
4. `selection` store + wiring into the source/import flow.
5. Selection → import staging (symlink dir, hardlink/copy fallback) in `import_start`.
6. Dev harness `preview` scenario + fixtures; vitest (selection) + e2e.
7. (Phase 2, separate bead) HEIC + video thumbnails via OS-native decoders.

## Decisions
- **A. Thumbnail coverage for v1:** *Recommended* — `image` + `rawler` (JPEG +
  RAW), placeholders for HEIC/video, OS-native HEIC/video deferred to phase 2.
  Alternatives: JPEG-only (drop `rawler`, simplest); or full bundled C now
  (libheif/libraw/ffmpeg — heavy, not recommended). **← awaiting confirmation**
- **B. Selection → import:** symlink staging dir. **← confirmed**
