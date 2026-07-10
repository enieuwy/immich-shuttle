//! Authorization scope for path-scoped IPC commands.
//!
//! The preview commands (`preview_thumbnails`, `preview_dates`) take raw file
//! paths from the renderer and read those files off disk. Without a guard a
//! compromised or buggy renderer could use them to read arbitrary local files
//! and exfiltrate the bytes/timestamps back through the IPC boundary.
//!
//! We authorize by the folders the user actually selected: the scan commands
//! (the point at which the user grants access to a source) record their roots
//! here, and preview requests are rejected unless they canonicalize to a path
//! nested under a recorded root.

use std::{
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

static APPROVED_ROOTS: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Record user-selected source roots as authorized for later path-scoped reads.
pub fn record_roots(paths: &[String]) {
    let Ok(mut roots) = APPROVED_ROOTS.lock() else {
        return;
    };
    for p in paths {
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
        if !roots.contains(&canon) {
            roots.push(canon);
        }
    }
}

/// Whether `path` canonicalizes to a location nested under a recorded source
/// root. Paths the user never selected as a source are rejected.
pub fn is_within_approved(path: &str) -> bool {
    let Ok(canon) = std::fs::canonicalize(path) else {
        return false;
    };
    match APPROVED_ROOTS.lock() {
        Ok(roots) => roots.iter().any(|root| canon.starts_with(root)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn rejects_paths_outside_recorded_roots() {
        let tmp = std::env::temp_dir().join(format!("guard-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let inside = tmp.join("photo.jpg");
        std::fs::write(&inside, b"x").unwrap();

        record_roots(&[tmp.to_string_lossy().to_string()]);
        assert!(is_within_approved(&inside.to_string_lossy()));

        // A real file that was never selected as a source is rejected.
        let outside = std::env::temp_dir().join(format!("outside-{}.jpg", Uuid::new_v4()));
        std::fs::write(&outside, b"y").unwrap();
        assert!(!is_within_approved(&outside.to_string_lossy()));

        std::fs::remove_dir_all(&tmp).unwrap();
        let _ = std::fs::remove_file(&outside);
    }
}
