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
const MAX_APPROVED_ROOTS: usize = 256;

/// Record user-selected source roots as authorized for later path-scoped reads.
///
/// Bounded to `MAX_APPROVED_ROOTS` to prevent unbounded growth across a long
/// session. When at capacity we evict the OLDEST root to admit the newest,
/// rather than dropping the new one — silently ignoring a just-selected root
/// would make its scanned media fail `is_within_approved`, rejecting files the
/// user legitimately selected.
pub fn record_roots(paths: &[String]) {
    let Ok(mut roots) = APPROVED_ROOTS.lock() else {
        return;
    };
    for p in paths {
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
        if roots.contains(&canon) {
            continue;
        }
        while roots.len() >= MAX_APPROVED_ROOTS {
            roots.remove(0);
        }
        roots.push(canon);
    }
}

/// Clear the authorization scope when the user changes their selected sources.
///
/// Callers must record the newly selected roots after resetting the prior scope.
pub fn reset_roots() {
    if let Ok(mut roots) = APPROVED_ROOTS.lock() {
        roots.clear();
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
    use std::sync::Mutex;

    use super::*;
    use uuid::Uuid;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn rejects_paths_outside_recorded_roots() {
        let _test_lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_roots();
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

        reset_roots();
        assert!(!is_within_approved(&inside.to_string_lossy()));

        std::fs::remove_dir_all(&tmp).unwrap();
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn rejects_sibling_with_the_same_string_prefix() {
        let _test_lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_roots();

        let source = std::env::temp_dir().join(format!("source-{}", Uuid::new_v4()));
        let sibling = std::env::temp_dir().join(format!(
            "{}-evil",
            source.file_name().unwrap().to_string_lossy()
        ));
        let nested_file = source.join("nested/photo.jpg");
        let sibling_file = sibling.join("x.jpg");
        std::fs::create_dir_all(nested_file.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(&nested_file, b"inside").unwrap();
        std::fs::write(&sibling_file, b"outside").unwrap();

        record_roots(&[source.to_string_lossy().to_string()]);
        assert!(is_within_approved(&nested_file.to_string_lossy()));
        assert!(!is_within_approved(&sibling_file.to_string_lossy()));

        reset_roots();
        std::fs::remove_dir_all(&source).unwrap();
        std::fs::remove_dir_all(&sibling).unwrap();
    }
}
