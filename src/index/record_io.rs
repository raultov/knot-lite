//! The only place that writes index files.
//!
//! Every write is `tmp + rename`, so a reader never observes a half-written
//! file (P2-D13). The JSONL helpers arrive with the shard records.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// Write `bytes` to `path` atomically: the content goes to `<file name>.tmp`
/// in the same directory and is renamed over the target. Creates parents.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let tmp = tmp_path(path);
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        if let Err(cleanup) = std::fs::remove_file(&tmp) {
            tracing::debug!(path = %tmp.display(), error = %cleanup, "could not remove temporary file");
        }
        return Err(error).with_context(|| format!("renaming into {}", path.display()));
    }
    Ok(())
}

/// `<path>.tmp` next to `path` (appended to the full file name).
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_tmp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("nested/dir/file.toml");
        atomic_write(&target, b"first").expect("first write creates parents");
        atomic_write(&target, b"second").expect("second write replaces");
        assert_eq!(std::fs::read(&target).expect("read"), b"second");
        assert_eq!(names_in(&tmp.path().join("nested/dir")), vec!["file.toml"]);
    }

    #[test]
    fn tmp_path_appends_to_the_full_name() {
        assert_eq!(
            tmp_path(Path::new("/a/b/x.rs.jsonl")),
            PathBuf::from("/a/b/x.rs.jsonl.tmp")
        );
        assert_eq!(
            tmp_path(Path::new("/a/.gitignore")),
            PathBuf::from("/a/.gitignore.tmp")
        );
    }
}
