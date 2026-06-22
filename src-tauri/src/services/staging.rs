//! Stages a user-selected subset of files for upload.
//!
//! immich-go's `from-folder` has no per-file selection (only extension/type/date
//! filters), so to import exactly the files a user picked in the preview grid we
//! build a temporary directory that links to just those files, preserving each
//! file's name and relative structure (so original filenames reach the server and
//! same-named files in different folders don't collide), then point the uploader
//! at that directory. Links are symlinks where possible, falling back to hard
//! links then a copy. Cleanup removes only the links, never the originals.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use uuid::Uuid;

/// Build a staging directory linking to `selected`. Returns the directory path.
pub fn create_staging_dir(selected: &[String]) -> Result<PathBuf, String> {
    if selected.is_empty() {
        return Err("No files selected to stage".to_string());
    }

    let root = std::env::temp_dir().join(format!("immich-shuttle-stage-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).map_err(|e| format!("Could not create staging dir: {e}"))?;

    let base = common_ancestor(selected);
    let mut linked = 0_usize;
    for entry in selected {
        let src = Path::new(entry);
        if !src.is_file() {
            continue;
        }
        let rel = base
            .as_ref()
            .and_then(|b| src.strip_prefix(b).ok())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(src.file_name().unwrap_or_default()));
        let dest = root.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create staging subdir: {e}"))?;
        }
        link_file(src, &dest)?;
        linked += 1;
    }

    if linked == 0 {
        let _ = fs::remove_dir_all(&root);
        return Err("None of the selected files could be staged".to_string());
    }
    Ok(root)
}

/// Remove a staging directory. Only the links are removed; targets are untouched.
pub fn cleanup_staging_dir(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

fn link_file(src: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(src, dest).is_ok() {
            return Ok(());
        }
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_file(src, dest).is_ok() {
            return Ok(());
        }
    }
    if fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    fs::copy(src, dest)
        .map(|_| ())
        .map_err(|e| format!("Could not stage {}: {e}", src.display()))
}

/// Longest common directory prefix of the parents of all paths.
fn common_ancestor(paths: &[String]) -> Option<PathBuf> {
    let mut parents = paths.iter().map(|p| {
        Path::new(p)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    });
    let first = parents.next()?;
    let mut common: Vec<Component> = first.components().collect();
    for parent in parents {
        let comps: Vec<Component> = parent.components().collect();
        let mut i = 0;
        while i < common.len() && i < comps.len() && common[i] == comps[i] {
            i += 1;
        }
        common.truncate(i);
        if common.is_empty() {
            return None;
        }
    }
    let mut out = PathBuf::new();
    for c in &common {
        out.push(c.as_os_str());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_selected_files_preserving_names() {
        let tmp = std::env::temp_dir().join(format!("stage-src-{}", Uuid::new_v4()));
        fs::create_dir_all(tmp.join("100")).unwrap();
        fs::create_dir_all(tmp.join("101")).unwrap();
        let a = tmp.join("100/IMG_1.JPG");
        let b = tmp.join("101/IMG_2.JPG");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();

        let staged = create_staging_dir(&[
            a.to_string_lossy().to_string(),
            b.to_string_lossy().to_string(),
        ])
        .unwrap();

        assert!(staged.join("100/IMG_1.JPG").exists());
        assert!(staged.join("101/IMG_2.JPG").exists());

        cleanup_staging_dir(&staged);
        assert!(!staged.exists());
        // Originals survive cleanup.
        assert!(a.exists() && b.exists());
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn empty_selection_errors() {
        assert!(create_staging_dir(&[]).is_err());
    }
}
