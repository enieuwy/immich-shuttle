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
//! - Anything no backend can render (e.g. HEIC/video off macOS, or a RAW with no
//!   embedded preview) → a placeholder result (`data_url: None`) that the UI
//!   renders as a typed tile.
//!
//! Results are cached on disk keyed by path+mtime+size so re-scans are instant.

use std::{
    fs,
    io::Cursor,
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

    None
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

/// Find the byte offset of the largest (by pixel area) JPEG embedded in a RAW
/// container. Cameras embed a full-size preview and often a small thumbnail;
/// byte stuffing means a real `FF D8 FF` SOI never appears inside JPEG entropy
/// data, so scanning for it reliably locates the embedded streams.
fn best_embedded_jpeg_offset(data: &[u8]) -> Option<usize> {
    let mut best: Option<(u64, usize)> = None;
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0xFF && data[i + 1] == 0xD8 && data[i + 2] == 0xFF {
            if let Ok((w, h)) =
                image::ImageReader::with_format(Cursor::new(&data[i..]), image::ImageFormat::Jpeg)
                    .into_dimensions()
            {
                let area = u64::from(w) * u64::from(h);
                let better = match best {
                    Some((a, _)) => area > a,
                    None => true,
                };
                if better {
                    best = Some((area, i));
                }
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    best.map(|(_, off)| off)
}

/// Decode the largest embedded JPEG preview from a camera RAW file and write a
/// JPEG thumbnail. Pure Rust (no RAW decoder); works on every platform.
fn generate_with_raw_preview(src: &Path, max: u32, out: &Path) -> bool {
    let data = match fs::read(src) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let offset = match best_embedded_jpeg_offset(&data) {
        Some(o) => o,
        None => return false,
    };
    let decoded =
        match image::load_from_memory_with_format(&data[offset..], image::ImageFormat::Jpeg) {
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
    fn picks_largest_embedded_jpeg() {
        // Simulate a RAW container: junk, a small thumbnail JPEG, more junk, the
        // full-size preview JPEG, trailing bytes. The picker must choose the preview.
        let small = jpeg_of(16, 16);
        let big = jpeg_of(200, 120);
        let mut data = vec![0x00, 0x11, 0x22, 0x33];
        data.extend_from_slice(&small);
        data.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let big_off = data.len();
        data.extend_from_slice(&big);
        data.extend_from_slice(&[0x99, 0x88]);

        let off = best_embedded_jpeg_offset(&data).expect("an embedded jpeg");
        assert_eq!(
            off, big_off,
            "should pick the larger preview, not the thumbnail"
        );
        let (w, h) =
            image::ImageReader::with_format(Cursor::new(&data[off..]), image::ImageFormat::Jpeg)
                .into_dimensions()
                .unwrap();
        assert_eq!((w, h), (200, 120));
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
