use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::models::history::ImportRecord;

static STORE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Lock the store mutex, recovering the guard if a previous holder panicked.
/// Poisoning only signals that some earlier operation aborted mid-flight; the
/// store data itself lives on disk, so a single panic must not permanently
/// brick every future history/metadata operation for the session.
fn lock_store() -> std::sync::MutexGuard<'static, ()> {
    STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default, Serialize, Deserialize)]
struct StoreData {
    #[serde(default)]
    history: Vec<ImportRecord>,
    #[serde(default)]
    sources: HashMap<String, SourceMeta>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SourceMeta {
    last_imported_at: i64,
    last_total: u32,
}

fn store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Could not resolve app data directory: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create app data directory: {e}"))?;
    Ok(dir.join("store.json"))
}

fn load(app: &tauri::AppHandle) -> Result<StoreData, String> {
    read_store_at(&store_path(app)?)
}

/// Read the store, distinguishing "never written" from "cannot be read".
///
/// Split out from `load` so the distinction can be tested without an
/// `AppHandle`: every caller that treats `Ok(StoreData::default())` as a first
/// run depends on a read failure NOT arriving as an empty store.
fn read_store_at(path: &Path) -> Result<StoreData, String> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<StoreData>(&raw)
            .map_err(|e| format!("Could not parse store at {}: {e}", path.display())),
        // A missing file is the first-run case, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreData::default()),
        // File exists but is locked/unreadable — refuse to fall back to an empty
        // store, or the next save would overwrite the user's real history.
        Err(e) => Err(format!("Could not read store at {}: {e}", path.display())),
    }
}

fn save(app: &tauri::AppHandle, data: &StoreData) -> Result<(), String> {
    let path = store_path(app)?;
    let content = serde_json::to_string_pretty(data)
        .map_err(|e| format!("Could not serialize store: {e}"))?;
    crate::services::private_file::write_atomic_private(&path, &content)
}

/// Append a run to history, advancing the incremental checkpoint only when the
/// caller certifies this run earned it.
///
/// `checkpoint_eligible` is decided by `classify_completed_run`, which sees the
/// run's full tallies (landed assets, per-file errors, aggregate scan errors).
/// It is passed in rather than re-derived from the record because status +
/// `errors` alone cannot express it: a run that read nothing at all — an empty
/// card, filters that excluded every file, or a source immich-go could not
/// enumerate — is legitimately `completed` with `errors == 0`, yet must NOT raise
/// the date floor, or the next "only new" import silently skips media whose
/// capture date predates this run.
pub fn append_history(
    app: &tauri::AppHandle,
    record: ImportRecord,
    checkpoint_eligible: bool,
) -> Result<(), String> {
    let _guard = lock_store();

    let mut data = load(app)?;
    if checkpoint_eligible {
        data.sources.insert(
            checkpoint_key(&record.profile_id, &record.source_paths),
            SourceMeta {
                last_imported_at: record.finished_at,
                last_total: record.total,
            },
        );
    }
    data.history.insert(0, record);
    data.history.truncate(100);

    save(app, &data)
}

pub fn list_history(app: &tauri::AppHandle) -> Result<Vec<ImportRecord>, String> {
    let _guard = lock_store();

    let mut history = load(app)?.history;
    history.sort_by_key(|record| std::cmp::Reverse(record.finished_at));
    Ok(history)
}

pub fn clear_history(app: &tauri::AppHandle) -> Result<(), String> {
    let _guard = lock_store();
    // A corrupt/unparseable store must still be resettable — fall back to an
    // empty store so "Clear history" can overwrite and repair it instead of
    // propagating the parse error and leaving the user permanently stuck.
    let mut data = load(app).unwrap_or_default();
    clear_store_data(&mut data);
    save(app, &data)
}

fn clear_store_data(data: &mut StoreData) {
    data.history.clear();
    data.sources.clear();
}

/// The incremental date floor for one (profile, source set), or `None` when this
/// pair has never earned a checkpoint.
///
/// A store that cannot be read or parsed returns `Err`, never `Ok(None)`: the
/// renderer turns `None` into "no floor, import everything", so collapsing a
/// read failure into the first-run answer would silently widen a requested
/// "only new since last import" run into a full re-upload of the whole card.
/// A genuinely MISSING store is still `Ok(None)` — that is a real first run, and
/// `load` already distinguishes the two.
pub fn last_import_for(
    app: &AppHandle,
    profile_id: &str,
    source_paths: &[String],
) -> Result<Option<i64>, String> {
    let _guard = lock_store();

    last_import_at(&store_path(app)?, profile_id, source_paths)
}

/// The `AppHandle`-free core of `last_import_for`, so the missing-versus-
/// unreadable distinction is directly testable against a real store file.
fn last_import_at(
    path: &Path,
    profile_id: &str,
    source_paths: &[String],
) -> Result<Option<i64>, String> {
    Ok(read_store_at(path)?
        .sources
        .get(&checkpoint_key(profile_id, source_paths))
        .map(|source| source.last_imported_at))
}

// Checkpoints are per (profile, source set): the same card imported under a
// different profile must not inherit another profile's date floor. The source
// key collapses nested roots and exact duplicates, giving one identity to one
// conceptual source set whether callers provide raw or collapsed selections.
// Changing this key format resets existing `last_import` associations. The key
// still collapses nested roots and exact duplicates, so equivalent source sets
// retain the same checkpoint identity.
//
// Both halves are length-prefixed. Prefixing only the source list would leave
// the separator ambiguous: a profile id ending in `\u{1f}10:/a` and a source
// path starting `/a\u{1f}5:` compose the same bytes as the reverse split, so
// two unrelated (profile, source set) pairs would share one date floor.
// `profile_id` is renderer-supplied (`profile_upsert` only defaults it to a
// UUID when absent), so it cannot be assumed to be hex-and-dashes.
fn checkpoint_key(profile_id: &str, paths: &[String]) -> String {
    format!(
        "{}:{profile_id}\u{1f}{}",
        profile_id.len(),
        source_key(paths)
    )
}

// Changing this normalization or encoding changes persisted keys and resets existing `last_import` associations.
fn source_key(paths: &[String]) -> String {
    let mut normalized: Vec<String> = paths
        .iter()
        .map(|path| normalize_source_path(path))
        .collect();
    normalized.sort();
    normalized.dedup();

    let collapsed: Vec<String> = normalized
        .iter()
        .filter(|candidate| {
            !normalized.iter().any(|parent| {
                parent != *candidate && Path::new(candidate).starts_with(Path::new(parent))
            })
        })
        .cloned()
        .collect();

    // Each entry writes its decimal byte length, a colon, then the path.
    let capacity = collapsed.iter().map(|path| path.len() + 8).sum();
    let mut key = String::with_capacity(capacity);
    for path in collapsed {
        write!(&mut key, "{}:", path.len()).expect("writing a checkpoint key cannot fail");
        key.push_str(&path);
    }
    key
}

fn normalize_source_path(path: &str) -> String {
    #[cfg(windows)]
    let path = PathBuf::from(path.replace('\\', "/"));
    #[cfg(not(windows))]
    let path = PathBuf::from(path);

    let normalized = fs::canonicalize(&path).unwrap_or(path);
    let normalized = normalized.to_string_lossy();
    #[cfg(windows)]
    let normalized = {
        let normalized = normalized.replace('\\', "/");
        normalized
            .strip_prefix("//?/UNC/")
            .map(|path| format!("//{path}"))
            .or_else(|| normalized.strip_prefix("//?/").map(str::to_owned))
            .unwrap_or(normalized)
    };
    #[cfg(not(windows))]
    let normalized = normalized.as_ref();
    let trimmed = normalized.trim_end_matches('/');

    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
    };

    use uuid::Uuid;

    use super::{
        checkpoint_key, clear_store_data, last_import_at, normalize_source_path, source_key,
        SourceMeta, StoreData,
    };

    fn scratch_dir(stem: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("immich-shuttle-store-{stem}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Written through `serde_json` rather than a JSON literal: `checkpoint_key`
    /// embeds a U+001F separator, which is not legal raw inside a JSON string.
    fn store_with_checkpoint(path: &Path, profile_id: &str, source: &str, at: i64) {
        let data = StoreData {
            history: Vec::new(),
            sources: HashMap::from([(
                checkpoint_key(profile_id, &[source.to_string()]),
                SourceMeta {
                    last_imported_at: at,
                    last_total: 3,
                },
            )]),
        };
        fs::write(path, serde_json::to_string(&data).unwrap()).unwrap();
    }

    #[test]
    fn clear_history_resets_source_metadata() {
        let mut data = StoreData {
            history: Vec::new(),
            sources: HashMap::from([(
                "source".to_string(),
                SourceMeta {
                    last_imported_at: 1,
                    last_total: 1,
                },
            )]),
        };

        clear_store_data(&mut data);

        assert!(data.history.is_empty());
        assert!(data.sources.is_empty());
    }

    #[test]
    fn a_stored_checkpoint_is_returned_for_its_own_key_only() {
        let dir = scratch_dir("checkpoint-hit");
        let path = dir.join("store.json");
        store_with_checkpoint(&path, "p1", "/Volumes/SD/DCIM", 1_700_000_000);

        let source = ["/Volumes/SD/DCIM".to_string()];
        assert_eq!(
            last_import_at(&path, "p1", &source),
            Ok(Some(1_700_000_000))
        );
        // A different profile must not inherit this card's date floor.
        assert_eq!(last_import_at(&path, "p2", &source), Ok(None));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_store_is_a_first_run_not_a_failure() {
        let dir = scratch_dir("checkpoint-missing");
        let path = dir.join("store.json");

        // Never written: there genuinely is no date floor, so "only new since
        // last import" correctly falls back to importing everything.
        assert_eq!(
            last_import_at(&path, "p1", &["/Volumes/SD/DCIM".to_string()]),
            Ok(None)
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_malformed_store_fails_instead_of_reporting_a_first_run() {
        let dir = scratch_dir("checkpoint-malformed");
        let path = dir.join("store.json");
        fs::write(&path, b"{ not json").unwrap();

        // `Ok(None)` here would be indistinguishable from a first run, and the
        // renderer would drop the requested only-new floor and re-upload the
        // entire card. The failure has to reach the caller.
        let error = last_import_at(&path, "p1", &["/Volumes/SD/DCIM".to_string()]).unwrap_err();
        assert!(error.contains("Could not parse store"), "got: {error}");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unreadable_store_fails_instead_of_reporting_a_first_run() {
        let dir = scratch_dir("checkpoint-unreadable");
        // A directory where the store file belongs: the entry exists, so this is
        // an I/O failure rather than the NotFound first-run case.
        let path = dir.join("store.json");
        fs::create_dir(&path).unwrap();

        let error = last_import_at(&path, "p1", &["/Volumes/SD/DCIM".to_string()]).unwrap_err();
        assert!(error.contains("Could not read store"), "got: {error}");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_key_normalizes_trailing_slashes_and_order() {
        let canonical_form = vec![
            "__store_key_test__/second".to_string(),
            "__store_key_test__/first".to_string(),
        ];
        let alternate_form = vec![
            "__store_key_test__/first/".to_string(),
            "__store_key_test__/second/".to_string(),
        ];

        assert_eq!(source_key(&canonical_form), source_key(&alternate_form));
    }

    #[test]
    fn source_key_collapses_overlapping_existing_roots() {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let child = parent.join("src");
        let raw_selection = vec![
            parent.to_string_lossy().into_owned(),
            child.to_string_lossy().into_owned(),
        ];
        let collapsed_selection = vec![parent.to_string_lossy().into_owned()];

        assert_eq!(source_key(&raw_selection), source_key(&collapsed_selection));
    }

    #[test]
    fn source_key_keeps_disjoint_roots_in_the_key() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let disjoint = vec![
            manifest_dir.join("src").to_string_lossy().into_owned(),
            manifest_dir
                .join("Cargo.toml")
                .to_string_lossy()
                .into_owned(),
        ];
        let mut expected = disjoint
            .iter()
            .map(|path| normalize_source_path(path))
            .collect::<Vec<_>>();
        expected.sort();

        let key = source_key(&disjoint);
        let expected = expected
            .iter()
            .map(|path| format!("{}:{path}", path.len()))
            .collect::<String>();

        assert_eq!(key, expected);
    }

    #[test]
    fn source_key_ignores_order_and_exact_duplicates() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let first = manifest_dir.join("src").to_string_lossy().into_owned();
        let second = manifest_dir
            .join("Cargo.toml")
            .to_string_lossy()
            .into_owned();

        assert_eq!(
            source_key(&[first.clone(), second.clone(), first.clone(), second.clone(),]),
            source_key(&[second, first])
        );
    }

    #[test]
    fn source_key_distinguishes_newline_path_from_multiple_paths() {
        let single = vec!["left\nright".to_string()];
        let multiple = vec!["left".to_string(), "right".to_string()];

        assert_ne!(source_key(&single), source_key(&multiple));
    }

    #[test]
    fn checkpoint_key_distinguishes_profile_and_source_splits() {
        // Both pairs compose the same bytes when only the source half carries a
        // length prefix: "P" + US + "10:/a<US>5:/b/cd" versus
        // "P<US>10:/a" + US + "5:/b/cd". Sharing one key would hand one card's
        // only-new date floor to an unrelated profile.
        assert_ne!(
            checkpoint_key("P", &["/a\u{1f}5:/b/cd".to_string()]),
            checkpoint_key("P\u{1f}10:/a", &["/b/cd".to_string()])
        );
    }

    #[test]
    fn source_key_keeps_reordered_and_redundant_sets_equal() {
        let newline_path = "left\nright".to_string();
        let other_path = "other".to_string();

        assert_eq!(
            source_key(&[newline_path.clone(), other_path.clone()]),
            source_key(&[other_path.clone(), newline_path.clone(), other_path])
        );
    }

    #[test]
    fn source_key_does_not_collapse_siblings_with_shared_name_prefixes() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("__store_key_test__");
        let foo = base.join("foo").to_string_lossy().into_owned();
        let foobar = base.join("foobar").to_string_lossy().into_owned();
        let mut expected = [normalize_source_path(&foo), normalize_source_path(&foobar)];
        expected.sort();

        let expected = expected
            .iter()
            .map(|path| format!("{}:{path}", path.len()))
            .collect::<String>();

        assert_eq!(source_key(&[foo, foobar]), expected);
    }

    #[cfg(windows)]
    #[test]
    fn source_key_normalizes_windows_separators() {
        assert_eq!(
            source_key(&["__store_key_test__/first".to_string()]),
            source_key(&["__store_key_test__\\first\\".to_string()])
        );
    }

    #[cfg(windows)]
    #[test]
    fn source_key_normalizes_windows_verbatim_prefix() {
        assert_eq!(
            source_key(&["//?/C:/Path".to_string()]),
            source_key(&["C:/Path".to_string()])
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn source_key_preserves_backslashes_in_unix_filenames() {
        assert_ne!(
            source_key(&["__store_key_test__/first".to_string()]),
            source_key(&["__store_key_test__\\first".to_string()])
        );
    }
}
