//! Generates small preview thumbnails for the pre-import grid.
//!
//! Strategy (a `Thumbnailer` with platform backends, sequenced so each dep earns
//! its keep):
//! - **macOS**: shell the OS built-ins — `sips` for still images (JPEG/PNG/TIFF/
//!   HEIC/RAW, all via ImageIO) and `qlmanage -t` (Quick Look) for video. Full
//!   coverage, zero extra crates.
//! - **All platforms (and the macOS fallback)**: the pure-Rust `image` crate for
//!   JPEG/PNG/TIFF/WebP/GIF/BMP, with EXIF orientation applied via `kamadak-exif`.
//! - **All platforms**: for camera RAW (CR2/CR3/NEF/ARW/RAF/RW2/ORF/DNG…), the
//!   largest JPEG preview embedded in the file is extracted and decoded — pure
//!   Rust, no RAW decoder, so RAW gets real thumbnails off macOS too.
//! - **Windows**: whatever the pure-Rust paths can't render (HEIC, video) falls
//!   back to the Shell thumbnail API (`IShellItemImageFactory`), which delegates
//!   to the registered OS thumbnail handlers — video via Media Foundation, HEIC
//!   when the HEIF Image Extensions are installed. Same thumbnails Explorer shows.
//! - Anything still unrendered (HEIC/video off macOS without the above, or a RAW
//!   with no embedded preview) → a placeholder result (`data_url: None`) that the
//!   UI renders as a typed tile.
//!
//! Results are cached on disk keyed by path+mtime+size so re-scans are instant.

use std::{
    fs,
    io::{Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use sha1::{Digest, Sha1};

/// Max thumbnail edge in pixels (aspect-preserving fit).
pub const MAX_PX: u32 = 256;

#[derive(Debug, Clone, Serialize)]
pub struct ThumbResult {
    /// Source file path, echoed back so the UI can map results.
    pub path: String,
    /// `data:<mime>;base64,...` thumbnail, or `None` when no backend rendered it.
    pub data_url: Option<String>,
    /// Thumbnail pixel dimensions; 0 for a placeholder.
    pub width: u32,
    pub height: u32,
}

fn cache_dir() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| "Could not resolve local data directory".to_string())?;
    let dir = base.join("immich-shuttle").join("thumbnails");
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create thumbnail cache dir: {e}"))?;
    Ok(dir)
}

fn cache_key(path: &Path, max: u32) -> String {
    let mtime = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha1::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(mtime.to_le_bytes());
    hasher.update(b":");
    hasher.update(max.to_le_bytes());
    format!("{:x}", hasher.finalize())
}

fn ext_lower(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn is_video_ext(ext: &str) -> bool {
    matches!(ext, "mp4" | "mov" | "m4v" | "avi" | "mkv")
}

/// Camera RAW extensions whose embedded JPEG preview we extract off macOS
/// (macOS renders these natively via `sips`).
fn is_raw_ext(ext: &str) -> bool {
    matches!(
        ext,
        "cr2"
            | "cr3"
            | "nef"
            | "nrw"
            | "arw"
            | "srf"
            | "sr2"
            | "raf"
            | "rw2"
            | "orf"
            | "dng"
            | "pef"
            | "rwl"
            | "raw"
            | "3fr"
            | "erf"
            | "kdc"
            | "mrw"
            | "iiq"
            | "cap"
            | "mef"
    )
}

/// Generate (or fetch from cache) a thumbnail for a single file. Never errors:
/// any failure yields a placeholder so one bad file can't break a batch.
pub fn thumbnail(path_str: &str, max: u32) -> ThumbResult {
    let placeholder = || ThumbResult {
        path: path_str.to_string(),
        data_url: None,
        width: 0,
        height: 0,
    };

    let path = Path::new(path_str);
    if !path.is_file() {
        return placeholder();
    }

    let cache = match cache_dir() {
        Ok(d) => d,
        Err(_) => return placeholder(),
    };
    let key = cache_key(path, max);
    let jpg = cache.join(format!("{key}.jpg"));
    let png = cache.join(format!("{key}.png"));

    let file = if jpg.is_file() {
        Some(jpg.clone())
    } else if png.is_file() {
        Some(png.clone())
    } else {
        generate(path, max, &jpg, &png)
    };

    match file {
        Some(f) => to_result(path_str, &f).unwrap_or_else(|_| placeholder()),
        None => placeholder(),
    }
}

fn to_result(path_str: &str, file: &Path) -> Result<ThumbResult, String> {
    let (width, height) = image::image_dimensions(file).map_err(|e| e.to_string())?;
    let bytes = fs::read(file).map_err(|e| e.to_string())?;
    let mime = if file.extension().map(|e| e == "png").unwrap_or(false) {
        "image/png"
    } else {
        "image/jpeg"
    };
    let data_url = format!("data:{};base64,{}", mime, STANDARD.encode(&bytes));
    Ok(ThumbResult {
        path: path_str.to_string(),
        data_url: Some(data_url),
        width,
        height,
    })
}

/// Try the available backends in order; return the produced cache file, if any.
// `png` is only consumed by the macOS Quick Look path; off macOS the portable
// `image` backend writes JPEG only, leaving it intentionally unused.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn generate(path: &Path, max: u32, jpg: &Path, png: &Path) -> Option<PathBuf> {
    let ext = ext_lower(path);

    #[cfg(target_os = "macos")]
    {
        if is_video_ext(&ext) {
            if generate_qlmanage(path, max, png) {
                return Some(png.to_path_buf());
            }
        } else if generate_sips(path, max, jpg) {
            return Some(jpg.to_path_buf());
        }
    }

    // Camera RAW: pull the largest embedded JPEG preview (CR2/CR3/NEF/ARW/RAF/…),
    // before the portable decoder so a TIFF-based RAW (e.g. DNG) isn't misread.
    if is_raw_ext(&ext) && generate_with_raw_preview(path, max, jpg) {
        return Some(jpg.to_path_buf());
    }

    // Portable backend (primary off macOS, fallback on macOS): still images only.
    if !is_video_ext(&ext) && generate_with_image(path, max, jpg) {
        return Some(jpg.to_path_buf());
    }

    // Windows-native fallback: the Shell thumbnail provider handles HEIC, video,
    // and anything else Explorer can thumbnail (no bundled codec required).
    #[cfg(windows)]
    {
        if generate_with_shell(path, max, jpg) {
            return Some(jpg.to_path_buf());
        }
    }

    None
}

/// Windows-native fallback: ask the Shell thumbnail provider for a bitmap and
/// re-encode it to JPEG. `IShellItemImageFactory` delegates to whatever thumbnail
/// handler is registered for the file type, so this covers video (Media
/// Foundation) and HEIC (with the HEIF Image Extensions installed) — the same
/// thumbnails Explorer shows — without bundling any codec.
#[cfg(windows)]
fn generate_with_shell(src: &Path, max: u32, out: &Path) -> bool {
    use std::ffi::c_void;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_THUMBNAILONLY,
    };

    unsafe {
        // COM must be live on this thread; balance only when we actually init it
        // (S_FALSE = already init here still adds a ref; RPC_E_CHANGED_MODE does not).
        let init = CoInitializeEx(None, COINIT_MULTITHREADED);
        let need_uninit = init.is_ok();

        let render = || -> windows::core::Result<bool> {
            let wide = HSTRING::from(src.as_os_str().to_string_lossy().as_ref());
            let factory: IShellItemImageFactory =
                SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)?;
            // THUMBNAILONLY: never accept a generic file-type icon as a "thumbnail".
            let hbitmap = factory.GetImage(
                SIZE {
                    cx: max as i32,
                    cy: max as i32,
                },
                SIIGBF_THUMBNAILONLY,
            )?;

            let mut info = BITMAP::default();
            let got = GetObjectW(
                HGDIOBJ(hbitmap.0),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut info as *mut _ as *mut c_void),
            );
            if got == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
                let _ = DeleteObject(HGDIOBJ(hbitmap.0));
                return Ok(false);
            }
            let (w, h) = (info.bmWidth, info.bmHeight);

            // Pull the pixels as a 32-bit top-down DIB (negative height).
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; w as usize * h as usize * 4];
            let hdc = GetDC(None);
            let scanned = GetDIBits(
                hdc,
                hbitmap,
                0,
                h as u32,
                Some(buf.as_mut_ptr() as *mut c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, hdc);
            let _ = DeleteObject(HGDIOBJ(hbitmap.0));
            if scanned == 0 {
                return Ok(false);
            }

            // GDI hands back BGRA rows; flatten to RGB (thumbnails are opaque).
            let mut rgb = image::RgbImage::new(w as u32, h as u32);
            for y in 0..h as u32 {
                for x in 0..w as u32 {
                    let i = ((y * w as u32 + x) * 4) as usize;
                    rgb.put_pixel(x, y, image::Rgb([buf[i + 2], buf[i + 1], buf[i]]));
                }
            }
            let thumb = image::DynamicImage::ImageRgb8(rgb).thumbnail(max, max);
            Ok(thumb
                .save_with_format(out, image::ImageFormat::Jpeg)
                .is_ok())
        };

        let result = render().unwrap_or(false);
        if need_uninit {
            CoUninitialize();
        }
        result
    }
}

#[cfg(target_os = "macos")]
fn generate_sips(src: &Path, max: u32, out: &Path) -> bool {
    std::process::Command::new("/usr/bin/sips")
        .args(["-Z", &max.to_string(), "-s", "format", "jpeg"])
        .arg(src)
        .arg("--out")
        .arg(out)
        .output()
        .map(|o| o.status.success() && out.is_file())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn generate_qlmanage(src: &Path, max: u32, out: &Path) -> bool {
    // qlmanage writes "<input name>.png" into an output directory; render into a
    // private temp dir, then move the single produced file to the cache path.
    let tmp = match out.parent().and_then(|p| {
        out.file_stem()
            .map(|s| p.join(format!(".ql-{}", s.to_string_lossy())))
    }) {
        Some(d) => d,
        None => return false,
    };
    if fs::create_dir_all(&tmp).is_err() {
        return false;
    }

    let ran = std::process::Command::new("/usr/bin/qlmanage")
        .args(["-t", "-s", &max.to_string(), "-o"])
        .arg(&tmp)
        .arg(src)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let produced = if ran {
        fs::read_dir(&tmp).ok().and_then(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.extension().map(|x| x == "png").unwrap_or(false))
        })
    } else {
        None
    };

    let moved = match produced {
        Some(p) => fs::rename(&p, out)
            .or_else(|_| fs::copy(&p, out).map(|_| ()))
            .is_ok(),
        None => false,
    };

    let _ = fs::remove_dir_all(&tmp);
    moved && out.is_file()
}

fn generate_with_image(src: &Path, max: u32, out: &Path) -> bool {
    let decoded = match image::open(src) {
        Ok(img) => img,
        Err(_) => return false,
    };
    let oriented = apply_orientation(src, decoded);
    let thumb = oriented.thumbnail(max, max);
    // JPEG has no alpha channel; flatten to RGB before encoding.
    let rgb = image::DynamicImage::ImageRgb8(thumb.to_rgb8());
    rgb.save_with_format(out, image::ImageFormat::Jpeg).is_ok()
}

/// Scan `src` for embedded JPEG SOI markers (`FF D8 FF`) using a small rolling
/// buffer, so we never hold the whole (often 20-100MB) RAW file in memory.
/// Returns the absolute byte offset of each embedded JPEG stream. Byte stuffing
/// means a real `FF D8 FF` never appears inside JPEG entropy data, so these
/// offsets reliably mark the start of each embedded stream (false positives from
/// raw sensor data are rejected later when dimension parsing fails).
fn find_embedded_jpeg_offsets(src: &Path) -> Vec<u64> {
    let mut file = match fs::File::open(src) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    const CHUNK: usize = 1 << 16; // 64 KiB
    let mut buf = vec![0u8; CHUNK + 2]; // +2 so a boundary-straddling marker survives
    let mut offsets = Vec::new();
    let mut base: u64 = 0; // absolute offset of buf[0]
    let mut carry = 0usize; // bytes retained from the previous chunk at buf[0..carry]
    loop {
        let n = match file.read(&mut buf[carry..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let filled = carry + n;
        let mut i = 0usize;
        while i + 3 <= filled {
            if buf[i] == 0xFF && buf[i + 1] == 0xD8 && buf[i + 2] == 0xFF {
                offsets.push(base + i as u64);
            }
            i += 1;
        }
        // Retain the last 2 bytes so a marker split across the chunk boundary is
        // still matched on the next pass.
        let keep = filled.min(2);
        let drop = filled - keep;
        buf.copy_within(drop..filled, 0);
        base += drop as u64;
        carry = keep;
    }
    offsets
}

/// Read up to `len` bytes from `src` starting at `offset`, capping the request so
/// a corrupt length can't blow up memory. Real camera previews are a few MB.
fn read_file_range(src: &Path, offset: u64, len: usize) -> Option<Vec<u8>> {
    const MAX_PREVIEW_BYTES: usize = 32 * 1024 * 1024;
    let len = len.min(MAX_PREVIEW_BYTES);
    let mut file = fs::File::open(src).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len];
    let mut read = 0usize;
    while read < len {
        match file.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(_) => return None,
        }
    }
    buf.truncate(read);
    Some(buf)
}

/// Decode the largest embedded JPEG preview from a camera RAW file and write a
/// JPEG thumbnail. Pure Rust (no RAW decoder); works on every platform. Memory
/// is bounded to a small header probe per candidate plus one preview read for
/// the chosen stream — never the whole RAW file.
fn generate_with_raw_preview(src: &Path, max: u32, out: &Path) -> bool {
    let offsets = find_embedded_jpeg_offsets(src);
    if offsets.is_empty() {
        return false;
    }
    let file_len = fs::metadata(src).map(|m| m.len()).unwrap_or(0);

    // Selection: read only a small header per candidate to get its dimensions
    // (the JPEG SOF marker sits near the start), never the full stream.
    const PROBE_BYTES: usize = 256 * 1024;
    let mut best: Option<(u64, u64)> = None; // (pixel area, offset)
    for &off in &offsets {
        let header = match read_file_range(src, off, PROBE_BYTES) {
            Some(b) if !b.is_empty() => b,
            _ => continue,
        };
        if let Ok((w, h)) =
            image::ImageReader::with_format(Cursor::new(&header), image::ImageFormat::Jpeg)
                .into_dimensions()
        {
            let area = u64::from(w) * u64::from(h);
            if best.is_none_or(|(a, _)| area > a) {
                best = Some((area, off));
            }
        }
    }

    let Some((_, off)) = best else {
        return false;
    };
    // Read the chosen stream up to the next embedded marker (or EOF). Trailing
    // raw sensor bytes are harmless: the JPEG decoder stops at the EOI marker.
    let end = offsets
        .iter()
        .copied()
        .find(|&o| o > off)
        .unwrap_or(file_len);
    let span = end.saturating_sub(off).max(1) as usize;
    let bytes = match read_file_range(src, off, span) {
        Some(b) => b,
        None => return false,
    };
    let decoded = match image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg) {
        Ok(img) => img,
        Err(_) => return false,
    };
    let thumb = decoded.thumbnail(max, max);
    // JPEG has no alpha channel; flatten to RGB before encoding.
    let rgb = image::DynamicImage::ImageRgb8(thumb.to_rgb8());
    rgb.save_with_format(out, image::ImageFormat::Jpeg).is_ok()
}

fn read_orientation(src: &Path) -> Option<u32> {
    let file = fs::File::open(src).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    field.value.get_uint(0)
}

fn apply_orientation(src: &Path, img: image::DynamicImage) -> image::DynamicImage {
    match read_orientation(src).unwrap_or(1) {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn renders_png_via_image_backend() {
        // A repo PNG icon exercises the portable `image` path on every platform.
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/icons/128x128.png");
        let out = std::env::temp_dir().join("immich_shuttle_thumb_test.jpg");
        let _ = fs::remove_file(&out);
        assert!(generate_with_image(Path::new(src), 64, &out));
        let (w, h) = image::image_dimensions(&out).unwrap();
        assert!(w <= 64 && h <= 64 && w > 0 && h > 0);
        let _ = fs::remove_file(&out);
    }

    #[test]
    fn missing_file_is_placeholder() {
        let r = thumbnail("/no/such/file.jpg", 64);
        assert!(r.data_url.is_none());
        assert_eq!(r.width, 0);
    }

    #[test]
    fn video_extension_detected() {
        assert!(is_video_ext("mov"));
        assert!(!is_video_ext("jpg"));
    }

    #[test]
    fn thumbnail_produces_data_url_for_png() {
        // Full path: macOS uses sips, other platforms use the image backend.
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/icons/128x128.png");
        let r = thumbnail(src, 64);
        assert!(r.data_url.is_some(), "expected a thumbnail data url");
        assert!(r.width > 0 && r.height > 0);
        assert!(r.data_url.unwrap().starts_with("data:image/"));
    }

    fn jpeg_of(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([20, 40, 80]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Jpeg)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn raw_extension_detected() {
        assert!(is_raw_ext("cr3"));
        assert!(is_raw_ext("dng"));
        assert!(!is_raw_ext("jpg"));
        assert!(!is_raw_ext("mov"));
    }

    #[test]
    fn finds_all_embedded_jpeg_offsets() {
        // junk, small thumbnail JPEG, junk, full-size preview JPEG, trailing bytes.
        let small = jpeg_of(16, 16);
        let big = jpeg_of(200, 120);
        let mut data = vec![0x00, 0x11, 0x22, 0x33];
        let small_off = data.len() as u64;
        data.extend_from_slice(&small);
        data.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let big_off = data.len() as u64;
        data.extend_from_slice(&big);
        data.extend_from_slice(&[0x99, 0x88]);

        let src =
            std::env::temp_dir().join(format!("immich_shuttle_offsets_{}.cr2", Uuid::new_v4()));
        fs::write(&src, &data).unwrap();
        let offsets = find_embedded_jpeg_offsets(&src);
        let _ = fs::remove_file(&src);
        assert!(
            offsets.contains(&small_off),
            "should find the thumbnail SOI"
        );
        assert!(offsets.contains(&big_off), "should find the preview SOI");
    }

    #[test]
    fn generate_picks_largest_preview() {
        // The 200x120 preview must be chosen over the 16x16 thumbnail; the written
        // thumbnail preserves the preview's landscape aspect (a thumbnail would be
        // square), proving the picker read the larger stream.
        let small = jpeg_of(16, 16);
        let big = jpeg_of(200, 120);
        let mut data = vec![0x00, 0x11, 0x22, 0x33];
        data.extend_from_slice(&small);
        data.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        data.extend_from_slice(&big);
        let src = std::env::temp_dir().join(format!("immich_shuttle_pick_{}.cr2", Uuid::new_v4()));
        let out = std::env::temp_dir().join(format!("immich_shuttle_pick_{}.jpg", Uuid::new_v4()));
        fs::write(&src, &data).unwrap();
        let _ = fs::remove_file(&out);
        assert!(generate_with_raw_preview(&src, 64, &out));
        let (w, h) = image::image_dimensions(&out).unwrap();
        assert!(
            w > h,
            "aspect should match the 200x120 preview, got {w}x{h}"
        );
        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&out);
    }

    #[test]
    fn finds_marker_across_chunk_boundary() {
        // Place the SOI at byte 65536 so it straddles the 64 KiB read window and
        // is only matched via the carried bytes on the next pass.
        let big = jpeg_of(80, 60);
        let mut data = vec![0u8; 65536];
        let off = data.len() as u64;
        data.extend_from_slice(&big);
        let src =
            std::env::temp_dir().join(format!("immich_shuttle_boundary_{}.cr2", Uuid::new_v4()));
        fs::write(&src, &data).unwrap();
        let offsets = find_embedded_jpeg_offsets(&src);
        let _ = fs::remove_file(&src);
        assert!(
            offsets.contains(&off),
            "SOI straddling the chunk boundary must be found; got {offsets:?}"
        );
    }

    #[test]
    fn raw_preview_backend_writes_thumbnail() {
        let big = jpeg_of(300, 200);
        let mut data = vec![0x49, 0x49, 0x2A, 0x00]; // TIFF-ish header bytes
        data.extend_from_slice(&big);
        let src = std::env::temp_dir().join("immich_shuttle_raw_test.cr2");
        let out = std::env::temp_dir().join("immich_shuttle_raw_test_thumb.jpg");
        fs::write(&src, &data).unwrap();
        let _ = fs::remove_file(&out);
        assert!(generate_with_raw_preview(&src, 64, &out));
        let (w, h) = image::image_dimensions(&out).unwrap();
        assert!(w <= 64 && h <= 64 && w > 0 && h > 0);
        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&out);
    }

    #[test]
    fn no_embedded_jpeg_returns_false() {
        let src = std::env::temp_dir().join("immich_shuttle_raw_none.cr2");
        let out = std::env::temp_dir().join("immich_shuttle_raw_none_thumb.jpg");
        fs::write(&src, [0u8; 256]).unwrap();
        let _ = fs::remove_file(&out);
        assert!(!generate_with_raw_preview(&src, 64, &out));
        let _ = fs::remove_file(&src);
    }
}
