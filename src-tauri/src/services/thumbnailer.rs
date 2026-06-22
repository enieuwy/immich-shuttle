//! Generates small preview thumbnails for the pre-import grid.
//!
//! Strategy (a `Thumbnailer` with platform backends, sequenced so each dep earns
//! its keep):
//! - **macOS**: shell the OS built-ins — `sips` for still images (JPEG/PNG/TIFF/
//!   HEIC/RAW, all via ImageIO) and `qlmanage -t` (Quick Look) for video. Full
//!   coverage, zero extra crates.
//! - **All platforms (and the macOS fallback)**: the pure-Rust `image` crate for
//!   JPEG/PNG/TIFF/WebP/GIF/BMP, with EXIF orientation applied via `kamadak-exif`.
//! - Anything no backend can render (e.g. HEIC/RAW/video off macOS) → a
//!   placeholder result (`data_url: None`) that the UI renders as a typed tile.
//!
//! Results are cached on disk keyed by path+mtime+size so re-scans are instant.

use std::{
    fs,
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
}
