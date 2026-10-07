//! Bounded, optional EXIF extraction for inspection and camera/GPS filtering.
use crate::models::media::PreviewMetadata;
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};

const MAX_EXIF_BYTES: u64 = 16 * 1024 * 1024;

pub(super) fn metadata(path: &Path) -> Option<PreviewMetadata> {
    if matches!(
        path.extension()?.to_str()?.to_ascii_lowercase().as_str(),
        "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm"
    ) {
        return None;
    }
    // TIFF/RAW pointers outside the bounded prefix remain unavailable. Preview
    // metadata must not read an entire large video or untrusted container.
    let source = File::open(path).ok()?;
    let len = source.metadata().ok()?.len().min(MAX_EXIF_BYTES);
    let mut bytes = Vec::with_capacity(len as usize);
    source.take(MAX_EXIF_BYTES).read_to_end(&mut bytes).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()?;
    Some(from_exif(&exif))
}

fn text(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let value = &exif.get_field(tag, exif::In::PRIMARY)?.value;
    let exif::Value::Ascii(parts) = value else {
        return None;
    };
    let bytes = parts.first()?;
    let value = String::from_utf8_lossy(&bytes[..bytes.len().min(512)])
        .trim_matches('\0')
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn uint(exif: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    exif.get_field(tag, exif::In::PRIMARY)?.value.get_uint(0)
}

fn gps(exif: &exif::Exif, coord: exif::Tag, reference: exif::Tag, max: f64) -> Option<f64> {
    let exif::Value::Rational(parts) = &exif.get_field(coord, exif::In::PRIMARY)?.value else {
        return None;
    };
    if parts.len() != 3 || parts.iter().any(|r| r.denom == 0) {
        return None;
    }
    let degrees = parts[0].to_f64();
    let minutes = parts[1].to_f64();
    let seconds = parts[2].to_f64();
    let sign = text(exif, reference)?;
    let negative = match (reference, sign.as_str()) {
        (exif::Tag::GPSLatitudeRef, "N") | (exif::Tag::GPSLongitudeRef, "E") => false,
        (exif::Tag::GPSLatitudeRef, "S") | (exif::Tag::GPSLongitudeRef, "W") => true,
        _ => return None,
    };
    coordinate(degrees, minutes, seconds, negative, max)
}

fn coordinate(degrees: f64, minutes: f64, seconds: f64, negative: bool, max: f64) -> Option<f64> {
    if !degrees.is_finite()
        || !minutes.is_finite()
        || !seconds.is_finite()
        || degrees < 0.0
        || !(0.0..60.0).contains(&minutes)
        || !(0.0..60.0).contains(&seconds)
    {
        return None;
    }
    let value = degrees + minutes / 60.0 + seconds / 3600.0;
    if value > max {
        return None;
    }
    Some(if negative { -value } else { value })
}

fn from_exif(exif: &exif::Exif) -> PreviewMetadata {
    let model = text(exif, exif::Tag::Model);
    let make = text(exif, exif::Tag::Make);
    let camera = match (make, model) {
        (Some(make), Some(model)) if !model.to_lowercase().starts_with(&make.to_lowercase()) => {
            Some(format!("{make} {model}"))
        }
        (_, Some(model)) => Some(model),
        (make, None) => make,
    };
    PreviewMetadata {
        camera,
        lens: text(exif, exif::Tag::LensModel),
        width: uint(exif, exif::Tag::PixelXDimension)
            .or_else(|| uint(exif, exif::Tag::ImageWidth))
            .filter(|v| *v > 0),
        height: uint(exif, exif::Tag::PixelYDimension)
            .or_else(|| uint(exif, exif::Tag::ImageLength))
            .filter(|v| *v > 0),
        gps_lat: gps(
            exif,
            exif::Tag::GPSLatitude,
            exif::Tag::GPSLatitudeRef,
            90.0,
        ),
        gps_lon: gps(
            exif,
            exif::Tag::GPSLongitude,
            exif::Tag::GPSLongitudeRef,
            180.0,
        ),
        iso: uint(exif, exif::Tag::PhotographicSensitivity),
        exposure: exif
            .get_field(exif::Tag::ExposureTime, exif::In::PRIMARY)
            .and_then(|field| {
                let exif::Value::Rational(values) = &field.value else {
                    return None;
                };
                let value = values.first()?;
                (value.denom != 0).then(|| format!("{}/{} s", value.num, value.denom))
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exif_fixture(with_gps: bool) -> Vec<u8> {
        // Little-endian TIFF: Model + optional GPS IFD. This exercises the real
        // reader, not a mock metadata struct.
        let mut bytes = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        let count: u16 = if with_gps { 2 } else { 1 };
        bytes.extend_from_slice(&count.to_le_bytes());
        let model_offset = 8 + 2 + usize::from(count) * 12 + 4;
        let entry = |tag: u16, kind: u16, count: u32, value: u32| {
            [
                tag.to_le_bytes().as_slice(),
                kind.to_le_bytes().as_slice(),
                count.to_le_bytes().as_slice(),
                value.to_le_bytes().as_slice(),
            ]
            .concat()
        };
        bytes.extend(entry(0x0110, 2, 5, model_offset as u32));
        if with_gps {
            bytes.extend(entry(0x8825, 4, 1, model_offset as u32 + 5));
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"Test\0");
        if with_gps {
            let rationals = bytes.len() + 2 + 4 * 12 + 4;
            bytes.extend_from_slice(&4u16.to_le_bytes());
            bytes.extend(entry(1, 2, 2, u32::from_le_bytes([b'S', 0, 0, 0])));
            bytes.extend(entry(2, 5, 3, rationals as u32));
            bytes.extend(entry(3, 2, 2, u32::from_le_bytes([b'E', 0, 0, 0])));
            bytes.extend(entry(4, 5, 3, rationals as u32 + 24));
            bytes.extend_from_slice(&0u32.to_le_bytes());
            for n in [31u32, 30, 0, 115, 45, 0] {
                bytes.extend_from_slice(&n.to_le_bytes());
                bytes.extend_from_slice(&1u32.to_le_bytes());
            }
        }
        // Wrap the EXIF TIFF in a JPEG APP1 segment.
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend_from_slice(&((bytes.len() + 8) as u16).to_be_bytes());
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend(bytes);
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        jpeg
    }

    #[test]
    fn reads_camera_and_signed_gps_from_jpeg() {
        let parsed = exif::Reader::new()
            .read_from_container(&mut Cursor::new(exif_fixture(true)))
            .unwrap();
        let metadata = from_exif(&parsed);
        assert_eq!(metadata.camera.as_deref(), Some("Test"));
        assert_eq!(metadata.gps_lat, Some(-31.5));
        assert_eq!(metadata.gps_lon, Some(115.75));
    }

    #[test]
    fn missing_gps_remains_absent() {
        let parsed = exif::Reader::new()
            .read_from_container(&mut Cursor::new(exif_fixture(false)))
            .unwrap();
        let metadata = from_exif(&parsed);
        assert_eq!(metadata.camera.as_deref(), Some("Test"));
        assert_eq!(metadata.gps_lat, None);
        assert_eq!(metadata.gps_lon, None);
    }

    #[test]
    fn malformed_gps_is_not_a_location() {
        assert_eq!(coordinate(90.0, 1.0, 0.0, false, 90.0), None);
        assert_eq!(coordinate(10.0, 60.0, 0.0, false, 180.0), None);
        assert_eq!(coordinate(f64::NAN, 0.0, 0.0, false, 90.0), None);
    }

    #[test]
    fn files_without_exif_and_video_return_none() {
        let dir = std::env::temp_dir().join(format!("preview-metadata-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("plain.png");
        image::RgbImage::new(4, 4).save(&png).unwrap();
        assert!(metadata(&png).is_none());
        let video = dir.join("clip.mp4");
        std::fs::write(&video, exif_fixture(true)).unwrap();
        assert!(metadata(&video).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
