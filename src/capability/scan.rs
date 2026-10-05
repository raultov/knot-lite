//! Repository file classification for the `status` capability matrix.
//!
//! Read-only (P1-D11): scanning never writes into the analysed repository.
//! The walk mirrors knot's `discover_files` ignore settings; knot's parsed set
//! decides [`FileClass::Parsed`], the remaining regular files are classified as
//! files-only or excluded.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use super::languages::label_for;

/// Mirrors knot `input.rs` `MAX_FILE_SIZE` (private there).
pub const MAX_FILE_BYTES: u64 = 500 * 1024;

/// Mirrors knot `input.rs` `EXCLUDED_NAMES`, same order.
pub const LOCK_FILE_NAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Cargo.lock",
    "composer.lock",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
];

/// Directories never entered by the scan (Phase 1: index dirs and git metadata).
pub const RESERVED_DIRS: &[&str] = &[".git", ".knot", ".knot-lite"];

/// Build-output directories never entered by any walker (P2-D9, Phase 0's
/// deny-list). They only matter outside git work trees, where `.gitignore`
/// is not honoured.
pub const BUILD_OUTPUT_DIRS: &[&str] = &["node_modules", "target", "dist"];

/// Whether a single path component names a reserved or build-output directory.
pub fn is_excluded_dir_name(name: &str) -> bool {
    RESERVED_DIRS.contains(&name) || BUILD_OUTPUT_DIRS.contains(&name)
}

/// Key used for files that have no recognisable extension at all.
pub const NO_EXTENSION_KEY: &str = "(no extension)";

/// Bytes sniffed for NUL characters when classifying binary files.
const SNIFF_BYTES: u64 = 8 * 1024;

/// Why a file is excluded from parsing (tier 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcludeReason {
    /// Contains NUL bytes in the first [`SNIFF_BYTES`] bytes.
    Binary,
    /// Larger than [`MAX_FILE_BYTES`].
    TooLarge,
    /// A lock file (see [`LOCK_FILE_NAMES`]).
    LockFile,
}

/// Classification of one regular file in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileClass {
    /// Parsed by knot's parsers; the label is the Appendix B language name.
    Parsed {
        /// Language label (Appendix B).
        label: &'static str,
    },
    /// Not parsed but counted (tier 1); `key` groups equal shapes (`.sh`, `Makefile`).
    FilesOnly {
        /// Grouping key: `.ext` (lower-cased), the file name, or [`NO_EXTENSION_KEY`].
        key: String,
    },
    /// Not counted as source (tier 0).
    Excluded(ExcludeReason),
}

/// Result of scanning one repository: repo-relative path plus classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    /// Sorted by repo-relative path.
    pub files: Vec<(String, FileClass)>,
}

/// Classify every regular file under `root` (read-only).
pub fn scan_repository(root: &Path) -> anyhow::Result<ScanResult> {
    let root_str = root
        .to_str()
        .context("repository path is not valid UTF-8")?;
    let parsed: HashSet<PathBuf> = knot::pipeline::input::discover_files(root_str, false)?
        .into_iter()
        .collect();

    let mut files = Vec::new();
    for entry in walk_repository(root_str) {
        let class = classify(entry.as_path(), &parsed);
        let relative = knot::pipeline::files::to_repo_relative(entry.as_path(), root);
        files.push((relative, class));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(ScanResult { files })
}

/// Walk `root` with the same ignore settings as knot's `discover_files`,
/// additionally skipping [`RESERVED_DIRS`], [`BUILD_OUTPUT_DIRS`] and
/// non-regular files.
pub(crate) fn walk_repository(root_str: &str) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(root_str)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            !(entry.depth() > 0
                && entry.file_type().is_some_and(|t| t.is_dir())
                && is_excluded_dir_name(entry.file_name().to_string_lossy().as_ref()))
        })
        .build()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_some_and(|t| t.is_file()) => Some(entry.into_path()),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(error = %e, "skipping unreadable directory entry");
                None
            }
        })
        .collect()
}

fn classify(path: &Path, parsed: &HashSet<PathBuf>) -> FileClass {
    if parsed.contains(path) {
        return FileClass::Parsed {
            label: label_for(path),
        };
    }
    if is_lock_file(path) {
        return FileClass::Excluded(ExcludeReason::LockFile);
    }
    let Ok(metadata) = std::fs::metadata(path) else {
        return FileClass::Excluded(ExcludeReason::Binary);
    };
    if metadata.len() > MAX_FILE_BYTES {
        return FileClass::Excluded(ExcludeReason::TooLarge);
    }
    if is_binary(path) {
        return FileClass::Excluded(ExcludeReason::Binary);
    }
    FileClass::FilesOnly {
        key: files_only_key(path),
    }
}

pub(crate) fn is_lock_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| LOCK_FILE_NAMES.contains(&name))
}

/// Whether the first [`SNIFF_BYTES`] bytes contain a NUL byte.
/// An I/O error counts as binary and is logged at `debug!`.
fn is_binary(path: &Path) -> bool {
    let mut buffer = [0u8; 1024];
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "unreadable file counts as binary");
            return true;
        }
    };
    let mut taken = (&mut file).take(SNIFF_BYTES);
    loop {
        let read = match taken.read(&mut buffer) {
            Ok(read) => read,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "unreadable file counts as binary");
                return true;
            }
        };
        if read == 0 {
            return false;
        }
        if buffer[..read].contains(&0u8) {
            return true;
        }
    }
}

fn files_only_key(path: &Path) -> String {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        return format!(".{}", ext.to_ascii_lowercase());
    }
    path.file_name().map_or_else(
        || NO_EXTENSION_KEY.to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// Build the shared fixture. `.gitignore` is only honoured inside a git
    /// repository (knot does not set `require_git(false)`), so the fixture runs
    /// `git init` inside its own tempdir. Returns `false` when `git` is missing.
    fn build_fixture(root: &Path) -> bool {
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::create_dir_all(root.join("web")).expect("mkdir web");
        fs::create_dir_all(root.join(".git")).expect("mkdir .git");
        fs::create_dir_all(root.join(".knot-lite")).expect("mkdir .knot-lite");
        fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").expect("write lib.rs");
        fs::write(root.join("web/app.tsx"), "export const x = 1;\n").expect("write app.tsx");
        fs::write(root.join("Makefile"), "all:\n").expect("write Makefile");
        fs::write(root.join("run.sh"), "echo hi\n").expect("write run.sh");
        fs::write(root.join("img.png"), [0x89, 0x50, 0x4e, 0x47, 0x00, 0x01]).expect("write png");
        fs::write(
            root.join("big.txt"),
            vec![b'a'; (MAX_FILE_BYTES as usize) + 1],
        )
        .expect("big");
        fs::write(root.join("Cargo.lock"), "lock\n").expect("write lock");
        fs::write(root.join("ignored.rs"), "pub fn ignored() {}\n").expect("write ignored.rs");
        fs::write(root.join(".git/inner.rs"), "pub fn git() {}\n").expect("write .git file");
        fs::write(root.join(".knot-lite/manifest.toml"), "schema = \"x\"\n").expect("manifest");

        let git_ok = Command::new("git")
            .arg("init")
            .current_dir(root)
            .output()
            .is_ok_and(|out| out.status.success());
        if git_ok {
            fs::write(root.join(".gitignore"), "ignored.rs\n").expect("write .gitignore");
        }
        git_ok
    }

    fn class_of<'a>(files: &'a [(String, FileClass)], name: &str) -> Option<&'a FileClass> {
        files.iter().find(|(p, _)| p == name).map(|(_, c)| c)
    }

    #[test]
    fn fixture_classifies_every_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let git_ok = build_fixture(tmp.path());
        let scan = scan_repository(tmp.path()).expect("scan");
        let files = &scan.files;

        assert_eq!(
            class_of(files, "src/lib.rs"),
            Some(&FileClass::Parsed { label: "rust" })
        );
        assert_eq!(
            class_of(files, "web/app.tsx"),
            Some(&FileClass::Parsed {
                label: "typescript"
            })
        );
        assert_eq!(
            class_of(files, "Makefile"),
            Some(&FileClass::FilesOnly {
                key: "Makefile".to_string()
            })
        );
        assert_eq!(
            class_of(files, "run.sh"),
            Some(&FileClass::FilesOnly {
                key: ".sh".to_string()
            })
        );
        assert_eq!(
            class_of(files, "img.png"),
            Some(&FileClass::Excluded(ExcludeReason::Binary))
        );
        assert_eq!(
            class_of(files, "big.txt"),
            Some(&FileClass::Excluded(ExcludeReason::TooLarge))
        );
        assert_eq!(
            class_of(files, "Cargo.lock"),
            Some(&FileClass::Excluded(ExcludeReason::LockFile))
        );
        if git_ok {
            assert_eq!(
                class_of(files, "ignored.rs"),
                None,
                "gitignored files never appear"
            );
        } else {
            eprintln!("git unavailable: skipped the gitignore assertion");
        }
        assert_eq!(class_of(files, ".git/inner.rs"), None);
        assert_eq!(class_of(files, ".knot-lite/manifest.toml"), None);
        assert!(
            class_of(files, ".gitignore").is_some(),
            ".gitignore is walked"
        );
    }

    #[test]
    fn build_output_dirs_are_never_walked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for rel in [
            "node_modules/a.js",
            "target/b.rs",
            "dist/c.js",
            "src/d.rs",
            "src/nested/target/e.rs",
        ] {
            let path = tmp.path().join(rel);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(&path, "x\n").expect("write");
        }
        let scan = scan_repository(tmp.path()).expect("scan");
        let names: Vec<&str> = scan.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, vec!["src/d.rs"]);
    }

    #[test]
    fn excluded_dir_names_cover_reserved_and_build_output() {
        for name in [
            ".git",
            ".knot",
            ".knot-lite",
            "node_modules",
            "target",
            "dist",
        ] {
            assert!(is_excluded_dir_name(name), "{name}");
        }
        for name in ["src", "targets", "dist.rs", ".github"] {
            assert!(!is_excluded_dir_name(name), "{name}");
        }
    }

    #[test]
    fn scan_is_side_effect_free() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_fixture(tmp.path());
        let mut before: Vec<String> = fs::read_dir(tmp.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        before.sort();
        let _ = scan_repository(tmp.path()).expect("scan");
        let mut after: Vec<String> = fs::read_dir(tmp.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn sniff_reads_at_most_8k() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // NUL only after the sniff window, well under the size limit.
        let mut body = vec![b'a'; (SNIFF_BYTES as usize) + 16];
        body.push(0u8);
        fs::write(tmp.path().join("late.txt"), body).expect("write");
        let scan = scan_repository(tmp.path()).expect("scan");
        assert_eq!(
            class_of(&scan.files, "late.txt"),
            Some(&FileClass::FilesOnly {
                key: ".txt".to_string()
            })
        );
    }
}
