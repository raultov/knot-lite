//! The only place that writes index files.
//!
//! Every write is `tmp + rename`, so a reader never observes a half-written
//! file (P2-D13). JSONL files are one JSON object per line, `\n`-terminated.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Whether [`write_jsonl_if_changed`] touched the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The file was created or its content replaced.
    Written,
    /// The file already held exactly these bytes and was left alone.
    Unchanged,
}

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

/// Serialise one JSON object per line (`\n`-terminated, no trailing blank
/// line). If the target already holds identical bytes nothing is written,
/// which keeps mtimes and git churn at zero.
pub fn write_jsonl_if_changed<T: Serialize>(
    path: &Path,
    records: &[T],
) -> anyhow::Result<WriteOutcome> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record)
            .with_context(|| format!("serialising a record for {}", path.display()))?;
        bytes.push(b'\n');
    }
    if std::fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(WriteOutcome::Unchanged);
    }
    atomic_write(path, &bytes)?;
    Ok(WriteOutcome::Written)
}

/// Read a JSONL file. Blank lines are skipped; errors carry `path:line`.
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Vec<T>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str(line)
            .with_context(|| format!("{}:{}", path.display(), index + 1))?;
        records.push(record);
    }
    Ok(records)
}

/// Every `*.jsonl` file under `base`, recursively, sorted. A missing `base`
/// yields an empty list.
pub fn list_record_files(base: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if base.exists() {
        collect_record_files(base, &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn collect_record_files(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry.with_context(|| format!("listing {}", dir.display()))?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_record_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(())
}

/// Delete every `*.jsonl` under `base` whose path is not in `keep`, plus any
/// leftover `*.tmp`, then remove directories left empty (never `base` itself).
/// Returns the number of files deleted. A missing `base` is not an error.
pub fn remove_orphans(base: &Path, keep: &BTreeSet<PathBuf>) -> anyhow::Result<usize> {
    if !base.exists() {
        return Ok(0);
    }
    let mut deleted = 0;
    prune_dir(base, keep, &mut deleted)?;
    Ok(deleted)
}

/// Prune one directory; returns whether it is empty afterwards.
fn prune_dir(dir: &Path, keep: &BTreeSet<PathBuf>, deleted: &mut usize) -> anyhow::Result<bool> {
    let mut remaining = 0usize;
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry.with_context(|| format!("listing {}", dir.display()))?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if prune_dir(&path, keep, deleted)? {
                std::fs::remove_dir(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            } else {
                remaining += 1;
            }
        } else if is_orphan(&path, keep) {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            *deleted += 1;
        } else {
            remaining += 1;
        }
    }
    Ok(remaining == 0)
}

fn is_orphan(path: &Path, keep: &BTreeSet<PathBuf>) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("tmp") => true,
        Some("jsonl") => !keep.contains(path),
        _ => false,
    }
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

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Row {
        a: u32,
        b: String,
    }

    fn rows(n: u32) -> Vec<Row> {
        (0..n)
            .map(|a| Row {
                a,
                b: format!("row {a}"),
            })
            .collect()
    }

    #[test]
    fn write_if_changed_reports_unchanged_and_keeps_mtime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("x.jsonl");
        assert_eq!(
            write_jsonl_if_changed(&path, &rows(3)).expect("first"),
            WriteOutcome::Written
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "{\"a\":0,\"b\":\"row 0\"}\n{\"a\":1,\"b\":\"row 1\"}\n{\"a\":2,\"b\":\"row 2\"}\n"
        );

        // Age the file so an accidental rewrite would be visible in the mtime.
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(old)
            .expect("set mtime");
        assert_eq!(
            write_jsonl_if_changed(&path, &rows(3)).expect("same"),
            WriteOutcome::Unchanged
        );
        assert_eq!(
            std::fs::metadata(&path)
                .expect("meta")
                .modified()
                .expect("mtime"),
            old
        );

        assert_eq!(
            write_jsonl_if_changed(&path, &rows(4)).expect("changed"),
            WriteOutcome::Written
        );
        assert_ne!(
            std::fs::metadata(&path)
                .expect("meta")
                .modified()
                .expect("mtime"),
            old
        );
    }

    #[test]
    fn empty_record_list_writes_an_empty_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("empty.jsonl");
        assert_eq!(
            write_jsonl_if_changed::<Row>(&path, &[]).expect("write"),
            WriteOutcome::Written
        );
        assert_eq!(std::fs::read(&path).expect("read"), b"");
        assert!(read_jsonl::<Row>(&path).expect("read").is_empty());
    }

    #[test]
    fn read_jsonl_skips_blank_lines_and_reports_line_numbers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("x.jsonl");
        std::fs::write(
            &path,
            "{\"a\":1,\"b\":\"x\"}\n\n   \n{\"a\":2,\"b\":\"y\"}\n",
        )
        .expect("write");
        assert_eq!(
            read_jsonl::<Row>(&path).expect("read"),
            vec![
                Row {
                    a: 1,
                    b: "x".into()
                },
                Row {
                    a: 2,
                    b: "y".into()
                }
            ]
        );

        std::fs::write(&path, "{\"a\":1,\"b\":\"x\"}\n\nnot json\n").expect("write");
        let error = format!("{:#}", read_jsonl::<Row>(&path).expect_err("must fail"));
        assert!(error.contains("x.jsonl:3"), "{error}");
    }

    #[test]
    fn list_record_files_is_recursive_sorted_and_jsonl_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().join("shards");
        for rel in [
            "b.jsonl",
            "a/z.jsonl",
            "a/y.jsonl",
            "a/skip.tmp",
            "c/notes.md",
        ] {
            let path = base.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "{}\n").expect("write");
        }
        assert_eq!(
            list_record_files(&base).expect("list"),
            vec![
                base.join("a/y.jsonl"),
                base.join("a/z.jsonl"),
                base.join("b.jsonl")
            ]
        );
        assert!(
            list_record_files(&tmp.path().join("missing"))
                .expect("list")
                .is_empty()
        );
    }

    #[test]
    fn remove_orphans_deletes_unkept_and_empty_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().join("shards");
        for rel in [
            "keep.jsonl",
            "a/keep.jsonl",
            "a/drop.jsonl",
            "b/c/drop.jsonl",
            "d/notes.md",
        ] {
            let path = base.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "{}\n").expect("write");
        }
        let keep: BTreeSet<PathBuf> = [base.join("keep.jsonl"), base.join("a/keep.jsonl")]
            .into_iter()
            .collect();

        assert_eq!(remove_orphans(&base, &keep).expect("prune"), 2);
        assert!(base.join("keep.jsonl").exists());
        assert!(base.join("a/keep.jsonl").exists());
        assert!(!base.join("a/drop.jsonl").exists());
        assert!(!base.join("b").exists(), "empty dirs are removed bottom-up");
        assert!(
            base.join("d/notes.md").exists(),
            "foreign files are left alone"
        );
        assert!(base.exists());
    }

    #[test]
    fn remove_orphans_keeps_base_when_everything_goes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().join("edges");
        std::fs::create_dir_all(base.join("x")).expect("mkdir");
        std::fs::write(base.join("x/a.jsonl"), "{}\n").expect("write");
        assert_eq!(remove_orphans(&base, &BTreeSet::new()).expect("prune"), 1);
        assert!(base.exists());
        assert!(!base.join("x").exists());
        assert_eq!(
            remove_orphans(&tmp.path().join("missing"), &BTreeSet::new()).expect("absent"),
            0
        );
    }

    #[test]
    fn remove_orphans_deletes_stale_tmp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().join("shards");
        std::fs::create_dir_all(base.join("src")).expect("mkdir");
        std::fs::write(base.join("src/a.rs.jsonl"), "{}\n").expect("write");
        std::fs::write(base.join("src/a.rs.jsonl.tmp"), "{").expect("write tmp");
        let keep: BTreeSet<PathBuf> = [base.join("src/a.rs.jsonl")].into_iter().collect();
        assert_eq!(remove_orphans(&base, &keep).expect("prune"), 1);
        assert!(base.join("src/a.rs.jsonl").exists());
        assert!(!base.join("src/a.rs.jsonl.tmp").exists());
    }
}
