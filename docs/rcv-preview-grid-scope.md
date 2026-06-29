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

## Recommended architecture (after "native vs crates" review)
A `Thumbnailer` trait with pluggable, per-platform backends — sequenced by
value/effort so each dependency earns its keep.

- **macOS backend (v1): OS built-ins, zero deps.** `sips` thumbnails
  JPEG/PNG/TIFF/**HEIC/RAW**; `qlmanage -t` (Quick Look) thumbnails **video**.
  Batched invocations + lazy (visible tiles only) + on-disk cache amortise the
  subprocess cost. Full coverage on the primary platform, no crates, no copyleft.
- **Portable Rust backend (v1): `image`** (MIT) for JPEG/PNG/TIFF/WebP on Windows
  & Linux; `kamadak-exif` for orientation/date; placeholder tiles for the rest.
- **Phase 2 (only on real demand):** `rawler` (LGPL-2.1) for RAW on Windows/Linux;
  a Windows-native backend (WIC / `IShellItemImageFactory`) for HEIC/RAW/video.
  Linux HEIC/video stay placeholders unless a freedesktop thumbnailer is present.

Why this split: on macOS the OS tools are broader and cheaper than any Rust crate,
so the crates do **not** earn their keep there; off macOS, `image` is a cheap
portable JPEG floor that does. `rawler` / Windows-native are deferred until there
is evidence non-macOS users need RAW/HEIC — earning their keep on demand.

## v1 scope
- Thumbnails via the trait: macOS backend (full) + `image` backend (JPEG) + typed
  placeholders. New deps: `image` + `kamadak-exif` (both MIT/Apache).
- Selection → import: symlink staging dir (Decision B, confirmed).
- Opt-in "Preview" affordance on a scanned source; lazy + cached; cancellable.

## Child beads (v1)
1. `Thumbnailer` trait + macOS backend (`sips`/`qlmanage`, batched, cached) +
   `image` backend + placeholder markers. Cache to
   `app-data/thumbnails/<sha1(path+mtime)>.jpg`. Unit-tested where feasible.
2. Streaming command + `thumbnail-ready {path,cachePath,w,h,capture_date}` events;
   cancellable.
3. `PreviewGrid.svelte` — virtualized grid, lazy thumbnails, per-tile checkbox,
   select all/none, live "N selected · M GB".
4. `selection` store + source/import wiring.
5. Selection → import staging (symlink dir, hardlink/copy fallback) in `import_start`.
6. Dev harness `preview` scenario + fixtures; vitest (selection) + e2e.

## Phase 2 beads
7. ✅ **Done** (as embedded-JPEG extraction, not `rawler`): RAW thumbnails on
   Windows/Linux by extracting the largest embedded JPEG preview — pure Rust, no
   RAW decoder. See `thumbnailer::generate_with_raw_preview`.
8. ✅ **Done**: Windows-native backend via the Shell thumbnail API
   (`IShellItemImageFactory::GetImage`) for HEIC + video — delegates to the OS
   thumbnail handlers (video via Media Foundation; HEIC with the HEIF Image
   Extensions installed), re-encoding the returned HBITMAP to JPEG. Verified on a
   real Windows 11 box: HEIC and MP4 produce real thumbnails. See
   `thumbnailer::generate_with_shell`. Linux HEIC/video remain placeholders.

## Decisions
- **A. Coverage/strategy:** trait-based hybrid — macOS-native (full) + portable
  `image` (JPEG) in v1; `rawler` + Windows-native in phase 2. ← recommended
- **B. Selection → import:** symlink staging dir. ← confirmed
