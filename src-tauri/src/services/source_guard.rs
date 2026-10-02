//! Consistency scope for path-scoped IPC commands.
//!
//! The scan commands record source roots supplied by the renderer. Preview and
//! forecast commands use this scope to reject paths outside the current source
//! selection, including sibling paths with a matching text prefix.
//!
//! This is not an authorization boundary against a compromised renderer: the
//! renderer supplies both the scan roots and the later read paths. The guard
//! prevents path confusion, but does not independently prove user consent.

use std::{
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

static APPROVED_ROOTS: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Replace the recorded source roots for later path-scoped consistency checks.
///
/// The approved scope is always exactly the current selection: callers pass the
/// complete current selection, so `roots` holds nothing else by the time this
/// returns. There is no cross-session accumulation to bound, so unlike some
/// caches here this list is never evicted -- just replaced whole by the next
/// call. Canonicalization runs before the lock so a slow filesystem cannot stall
/// concurrent `is_within_approved` checks. The prepared list replaces the prior
/// list under one lock hold. This swap is atomic because a path-scoped IPC read
/// that lands mid-update must never be denied for a source still in the current
/// selection. Clearing the scope uses `replace_roots(&[])`. A panic elsewhere must
/// not wedge the guard: the root list is a plain `Vec<PathBuf>` with no
/// invariant a panic could break mid-update, so a poisoned lock is recovered
/// rather than treated as permanent denial (which would blank previews for the
/// session).
pub fn replace_roots(paths: &[String]) {
    replace_roots_with(paths, |path| {
        std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
    });
}

fn replace_roots_with(paths: &[String], mut prepare_root: impl FnMut(&str) -> PathBuf) {
    let batch: Vec<PathBuf> = paths.iter().map(|path| prepare_root(path)).collect();
    let mut roots = APPROVED_ROOTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *roots = batch;
}

/// Whether `path` canonicalizes to a location nested under a recorded source
/// root. This checks scan consistency, not independent filesystem authorization.
pub fn is_within_approved(path: &str) -> bool {
    let Ok(canon) = std::fs::canonicalize(path) else {
        return false;
    };
    let roots = APPROVED_ROOTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    roots.iter().any(|root| canon.starts_with(root))
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
        replace_roots(&[]);
        let tmp = std::env::temp_dir().join(format!("guard-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let inside = tmp.join("photo.jpg");
        std::fs::write(&inside, b"x").unwrap();

        replace_roots(&[tmp.to_string_lossy().to_string()]);
        assert!(is_within_approved(&inside.to_string_lossy()));

        // A real file that was never selected as a source is rejected.
        let outside = std::env::temp_dir().join(format!("outside-{}.jpg", Uuid::new_v4()));
        std::fs::write(&outside, b"y").unwrap();
        assert!(!is_within_approved(&outside.to_string_lossy()));

        replace_roots(&[]);
        assert!(!is_within_approved(&inside.to_string_lossy()));

        std::fs::remove_dir_all(&tmp).unwrap();
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn rejects_sibling_with_the_same_string_prefix() {
        let _test_lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        replace_roots(&[]);

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

        replace_roots(&[source.to_string_lossy().to_string()]);
        assert!(is_within_approved(&nested_file.to_string_lossy()));
        assert!(!is_within_approved(&sibling_file.to_string_lossy()));

        replace_roots(&[]);
        std::fs::remove_dir_all(&source).unwrap();
        std::fs::remove_dir_all(&sibling).unwrap();
    }

    /// A replacement keeps the old scope visible while it prepares the new one.
    /// Block preparation explicitly so the concurrent observation cannot miss
    /// a clear-before-refill regression.
    #[test]
    fn replace_roots_swaps_the_scope_without_an_empty_window() {
        let _test_lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        replace_roots(&[]);

        let root_a = std::env::temp_dir().join(format!("guard-a-{}", Uuid::new_v4()));
        let root_b = std::env::temp_dir().join(format!("guard-b-{}", Uuid::new_v4()));
        let file_a = root_a.join("photo-a.jpg");
        let file_b = root_b.join("photo-b.jpg");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        std::fs::write(&file_a, b"a").unwrap();
        std::fs::write(&file_b, b"b").unwrap();

        replace_roots(&[root_a.to_string_lossy().to_string()]);
        let paths = [root_b.to_string_lossy().to_string()];
        let (preparing_tx, preparing_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let (observed_tx, observed_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let updater = scope.spawn(move || {
                replace_roots_with(&paths, |path| {
                    preparing_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    std::fs::canonicalize(path).unwrap()
                });
            });
            preparing_rx.recv().unwrap();
            let observer = scope.spawn(|| {
                observed_tx
                    .send(is_within_approved(&file_a.to_string_lossy()))
                    .unwrap();
            });
            let observed = observed_rx.recv_timeout(std::time::Duration::from_secs(5));
            release_tx.send(()).unwrap();
            updater.join().unwrap();
            observer.join().unwrap();
            assert!(observed.unwrap());
        });
        assert!(is_within_approved(&file_b.to_string_lossy()));
        assert!(!is_within_approved(&file_a.to_string_lossy()));

        replace_roots(&[]);
        std::fs::remove_dir_all(&root_a).unwrap();
        std::fs::remove_dir_all(&root_b).unwrap();
    }
}
