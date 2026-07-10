use crate::services::thumbnailer::{thumbnail, ThumbResult, MAX_PX};

/// Generate (or fetch cached) thumbnails for a batch of files. The frontend grid
/// calls this lazily for the tiles entering the viewport. Work runs on blocking
/// threads, bounded to a small pool so a large card can't saturate the runtime.
#[tauri::command]
pub async fn preview_thumbnails(paths: Vec<String>) -> Result<Vec<ThumbResult>, String> {
    let mut results = Vec::with_capacity(paths.len());

    for chunk in paths.chunks(8) {
        let handles: Vec<_> = chunk
            .iter()
            .cloned()
            .map(|path| {
                tauri::async_runtime::spawn_blocking(move || {
                    // Only read files under a folder the user selected as a
                    // source; reject arbitrary paths from the IPC boundary.
                    if crate::services::source_guard::is_within_approved(&path) {
                        thumbnail(&path, MAX_PX)
                    } else {
                        ThumbResult {
                            path,
                            data_url: None,
                            width: 0,
                            height: 0,
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            if let Ok(result) = handle.await {
                results.push(result);
            }
        }
    }

    Ok(results)
}

use serde::Serialize;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize)]
pub struct CaptureDate {
    pub path: String,
    /// Capture time as epoch seconds: EXIF DateTimeOriginal/DateTime when present,
    /// otherwise the file's modification time, or `None` if neither is readable.
    pub captured_at: Option<i64>,
}

/// Resolve a sortable capture timestamp for each file. EXIF is authoritative;
/// file mtime is the fallback (on a camera card it closely tracks capture time).
#[tauri::command]
pub async fn preview_dates(paths: Vec<String>) -> Result<Vec<CaptureDate>, String> {
    let mut results = Vec::with_capacity(paths.len());

    for chunk in paths.chunks(16) {
        let handles: Vec<_> = chunk
            .iter()
            .cloned()
            .map(|path| {
                tauri::async_runtime::spawn_blocking(move || CaptureDate {
                    captured_at: if crate::services::source_guard::is_within_approved(&path) {
                        capture_date(&path)
                    } else {
                        None
                    },
                    path,
                })
            })
            .collect();

        for handle in handles {
            if let Ok(result) = handle.await {
                results.push(result);
            }
        }
    }

    Ok(results)
}

fn capture_date(path_str: &str) -> Option<i64> {
    let path = Path::new(path_str);
    exif_capture_date(path).or_else(|| mtime_epoch(path))
}

fn mtime_epoch(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

fn exif_capture_date(path: &Path) -> Option<i64> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let field = exif
        .get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::DateTime, exif::In::PRIMARY))?;
    parse_exif_datetime(&field.display_value().to_string())
}

/// Parse an EXIF datetime (e.g. "2026:06:14 15:30:12") into epoch seconds (UTC).
/// Tolerant of separators by reading the leading 14 digits as YYYYMMDDHHMMSS.
fn parse_exif_datetime(s: &str) -> Option<i64> {
    let digits: Vec<u8> = s.bytes().filter(u8::is_ascii_digit).collect();
    if digits.len() < 14 {
        return None;
    }
    let num = |start: usize, end: usize| -> Option<i64> {
        std::str::from_utf8(&digits[start..end]).ok()?.parse().ok()
    };
    let year = num(0, 4)?;
    let month = num(4, 6)?;
    let day = num(6, 8)?;
    let hour = num(8, 10)?;
    let minute = num(10, 12)?;
    let second = num(12, 14)?;
    if !(1..=12).contains(&month) {
        return None;
    }
    // Reject impossible clock and calendar values (e.g. 24:00:00, minute/second
    // 60, Feb 31) so a corrupt EXIF stamp falls back to filesystem mtime instead
    // of sorting the file to a plausible-but-wrong date.
    if !(1..=days_in_month(year, month)).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    Some(civil_to_epoch(year, month, day, hour, minute, second))
}

/// Days-from-civil (Howard Hinnant) → epoch seconds, treating the time as UTC.
fn civil_to_epoch(year: i64, month: i64, day: i64, hour: i64, min: i64, sec: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    days * 86400 + hour * 3600 + min * 60 + sec
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exif_datetime_to_epoch() {
        // 2021-01-01 00:00:00 UTC = 1609459200.
        assert_eq!(
            parse_exif_datetime("2021:01:01 00:00:00"),
            Some(1_609_459_200)
        );
        // Tolerates dash separators.
        assert_eq!(
            parse_exif_datetime("2021-01-01 00:00:00"),
            Some(1_609_459_200)
        );
    }

    #[test]
    fn rejects_short_or_invalid() {
        assert_eq!(parse_exif_datetime("2021:01"), None);
        assert_eq!(parse_exif_datetime("2021:13:01 00:00:00"), None);
    }

    #[test]
    fn rejects_out_of_range_time_and_calendar() {
        // Hour/minute/second bounds.
        assert_eq!(parse_exif_datetime("2021:01:01 24:00:00"), None);
        assert_eq!(parse_exif_datetime("2021:01:01 00:60:00"), None);
        assert_eq!(parse_exif_datetime("2021:01:01 00:00:60"), None);
        // Impossible calendar days.
        assert_eq!(parse_exif_datetime("2021:02:31 00:00:00"), None);
        assert_eq!(parse_exif_datetime("2021:04:31 00:00:00"), None);
        assert_eq!(parse_exif_datetime("2021:02:29 00:00:00"), None); // 2021 not leap
                                                                      // Valid leap day passes.
        assert!(parse_exif_datetime("2020:02:29 12:30:45").is_some());
        // Boundary values accepted.
        assert!(parse_exif_datetime("2021:12:31 23:59:59").is_some());
    }

    #[test]
    fn epoch_known_dates() {
        assert_eq!(civil_to_epoch(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(civil_to_epoch(2000, 1, 1, 0, 0, 0), 946_684_800);
    }
}
