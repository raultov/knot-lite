//! On-disk index layout under `<repo>/.knot-lite/`.
//!
//! Defines every path of the index (manifest, shards, edges, inventory, the
//! commit-mode files) plus the pure helpers that map a `/`-separated
//! repo-relative path to its record file and back.

use std::path::{Component, Path, PathBuf};

use anyhow::bail;
use serde::{Deserialize, Serialize};

use super::record_io::atomic_write;
use crate::capability::scan::is_excluded_dir_name;

/// Name of the per-repository index directory.
pub const INDEX_DIR: &str = ".knot-lite";

/// Name of the manifest file inside [`INDEX_DIR`].
pub const MANIFEST_FILE: &str = "manifest.toml";

/// Manifest schema this knot-lite version understands.
pub const MANIFEST_SCHEMA_V1: &str = "knot-lite/manifest/v1";

/// Directory holding one JSONL shard per source file.
pub const SHARDS_DIR: &str = "shards";
/// Directory holding one JSONL edge shard per caller file.
pub const EDGES_DIR: &str = "edges";
/// Inventory of every file the index considered (freshness, P2-D15).
pub const INVENTORY_FILE: &str = "inventory.jsonl";
/// The previous manifest, kept while a run is in flight (P2-D13).
pub const PREVIOUS_MANIFEST_FILE: &str = "manifest.prev.toml";
/// Name of the git ignore file written in `gitignore` mode.
pub const GITIGNORE_FILE: &str = ".gitignore";
/// Name of the git attributes file written in `commit` mode.
pub const GITATTRIBUTES_FILE: &str = ".gitattributes";
/// Extension of every record file.
pub const RECORD_EXT: &str = "jsonl";

/// Body of `.knot-lite/.gitignore` in `gitignore` mode: ignores the whole index.
pub const GITIGNORE_BODY: &str = "# Written by knot-lite (commit_mode = gitignore): the index is not tracked.\n\
# Run `knot-lite index --commit` to track it instead.\n*\n";

/// Body of `.knot-lite/.gitattributes` in `commit` mode.
pub const GITATTRIBUTES_BODY: &str =
    "# Written by knot-lite (commit_mode = commit).\n* linguist-generated=true -diff\n";

/// Paths of one repository's knot-lite index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLayout {
    /// Repository root the index belongs to.
    pub root: PathBuf,
    /// The `.knot-lite/` directory.
    pub dir: PathBuf,
    /// The `.knot-lite/manifest.toml` file.
    pub manifest: PathBuf,
    /// The `.knot-lite/manifest.prev.toml` file.
    pub previous_manifest: PathBuf,
    /// The `.knot-lite/shards/` directory.
    pub shards: PathBuf,
    /// The `.knot-lite/edges/` directory.
    pub edges: PathBuf,
    /// The `.knot-lite/inventory.jsonl` file.
    pub inventory: PathBuf,
    /// The `.knot-lite/.gitignore` file.
    pub gitignore: PathBuf,
    /// The `.knot-lite/.gitattributes` file.
    pub gitattributes: PathBuf,
}

impl IndexLayout {
    /// Compute the index layout for `repo_root` (pure, no filesystem access).
    pub fn for_repo(repo_root: &Path) -> Self {
        let dir = repo_root.join(INDEX_DIR);
        Self {
            root: repo_root.to_path_buf(),
            manifest: dir.join(MANIFEST_FILE),
            previous_manifest: dir.join(PREVIOUS_MANIFEST_FILE),
            shards: dir.join(SHARDS_DIR),
            edges: dir.join(EDGES_DIR),
            inventory: dir.join(INVENTORY_FILE),
            gitignore: dir.join(GITIGNORE_FILE),
            gitattributes: dir.join(GITATTRIBUTES_FILE),
            dir,
        }
    }

    /// Shard file of the repo-relative path `rel`: `shards/<rel>.jsonl`.
    pub fn shard_for(&self, rel: &str) -> anyhow::Result<PathBuf> {
        record_path(&self.shards, rel)
    }

    /// Edge shard of the caller file `rel`: `edges/<rel>.jsonl`.
    pub fn edges_for(&self, rel: &str) -> anyhow::Result<PathBuf> {
        record_path(&self.edges, rel)
    }

    /// Repo-relative manifest path (`.knot-lite/manifest.toml`), always with `/`.
    pub fn manifest_rel() -> String {
        format!("{INDEX_DIR}/{MANIFEST_FILE}")
    }
}

/// Join a `/`-separated repo-relative path under `base`, appending `.jsonl`
/// to the last component (never `with_extension`, which would replace `.rs`).
/// Rejects empty paths, absolute paths, drive/UNC prefixes, `.` and `..`.
fn record_path(base: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let mut components: Vec<&str> = rel.split('/').collect();
    if let Some(bad) = components.iter().find(|c| !is_plain_component(c)) {
        bail!("invalid repo-relative path {rel:?} (component {bad:?})");
    }
    let last = components.pop().unwrap_or_default();
    let mut path = base.to_path_buf();
    path.extend(components);
    path.push(format!("{last}.{RECORD_EXT}"));
    Ok(path)
}

/// A single, plain path component: not empty, not `.`/`..`, no drive prefix
/// and no separator of the host platform.
fn is_plain_component(component: &str) -> bool {
    if component.is_empty() || component == "." || component == ".." {
        return false;
    }
    if is_drive_prefix(component) {
        return false;
    }
    let mut parsed = Path::new(component).components();
    matches!(
        (parsed.next(), parsed.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// `C:`-style drive prefix (rejected on every platform so records stay portable).
fn is_drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Inverse of [`record_path`] for orphan cleanup: `base/a/b.rs.jsonl` → `a/b.rs`.
/// `None` when `path` is not under `base`, not UTF-8, or not a record file.
pub fn rel_of_record(base: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(base).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            _ => return None,
        }
    }
    let last = parts.pop()?;
    let stem = last.strip_suffix(&format!(".{RECORD_EXT}"))?;
    if stem.is_empty() {
        return None;
    }
    parts.push(stem);
    Some(parts.join("/"))
}

/// Whether any `/`-separated component names a reserved or build-output
/// directory (`.git`, `.knot`, `.knot-lite`, `node_modules`, `target`, `dist`).
pub fn is_reserved_rel(rel: &str) -> bool {
    rel.split('/').any(is_excluded_dir_name)
}

/// Whether the index is tracked by git (P2-D8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum CommitMode {
    /// `.knot-lite/.gitignore` ignores the whole index (the default).
    #[default]
    Gitignore,
    /// `.knot-lite/.gitattributes` marks the index generated; it is tracked.
    Commit,
}

/// Write the mode's file and remove the other one (creates `.knot-lite/`).
pub fn apply_commit_mode(layout: &IndexLayout, mode: CommitMode) -> anyhow::Result<()> {
    let (keep, body, drop) = match mode {
        CommitMode::Gitignore => (&layout.gitignore, GITIGNORE_BODY, &layout.gitattributes),
        CommitMode::Commit => (&layout.gitattributes, GITATTRIBUTES_BODY, &layout.gitignore),
    };
    atomic_write(keep, body.as_bytes())?;
    match std::fs::remove_file(drop) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_repo_joins_index_dir_and_manifest() {
        let layout = IndexLayout::for_repo(Path::new("/tmp/repo"));
        let dir = PathBuf::from("/tmp/repo").join(INDEX_DIR);
        assert_eq!(layout.root, PathBuf::from("/tmp/repo"));
        assert_eq!(layout.dir, dir);
        assert_eq!(layout.manifest, dir.join(MANIFEST_FILE));
        assert_eq!(layout.previous_manifest, dir.join("manifest.prev.toml"));
        assert_eq!(layout.shards, dir.join("shards"));
        assert_eq!(layout.edges, dir.join("edges"));
        assert_eq!(layout.inventory, dir.join("inventory.jsonl"));
        assert_eq!(layout.gitignore, dir.join(".gitignore"));
        assert_eq!(layout.gitattributes, dir.join(".gitattributes"));
        assert_eq!(INDEX_DIR, ".knot-lite");
        assert_eq!(MANIFEST_FILE, "manifest.toml");
        assert_eq!(MANIFEST_SCHEMA_V1, "knot-lite/manifest/v1");
        assert_eq!(IndexLayout::manifest_rel(), ".knot-lite/manifest.toml");
    }

    #[test]
    fn shard_for_appends_jsonl_to_full_name() {
        let layout = IndexLayout::for_repo(Path::new("/r"));
        assert_eq!(
            layout.shard_for("src/config.rs").expect("shard"),
            Path::new("/r/.knot-lite/shards/src/config.rs.jsonl")
        );
    }

    #[test]
    fn edges_for_mirrors_shard_layout() {
        let layout = IndexLayout::for_repo(Path::new("/r"));
        assert_eq!(
            layout.edges_for("src/config.rs").expect("edges"),
            Path::new("/r/.knot-lite/edges/src/config.rs.jsonl")
        );
        assert_eq!(
            layout.edges_for("Makefile").expect("edges"),
            Path::new("/r/.knot-lite/edges/Makefile.jsonl")
        );
    }

    #[test]
    fn record_path_rejects_traversal_and_absolute() {
        for bad in ["../x", "/etc/x", "a/../b", "./a", "", "C:/x", "a//b", "a/"] {
            assert!(
                record_path(Path::new("/b"), bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        assert!(record_path(Path::new("/b"), "a/b.c/d").is_ok());
        assert!(record_path(Path::new("/b"), ".github/ci.yml").is_ok());
    }

    #[test]
    fn rel_of_record_inverts_record_path() {
        let base = Path::new("/b");
        for rel in [
            "Makefile",
            "src/config.rs",
            "a/b/c/d.tar.gz",
            "x.jsonl",
            ".github/workflows/ci.yml",
        ] {
            let path = record_path(base, rel).expect("record path");
            assert_eq!(rel_of_record(base, &path).as_deref(), Some(rel), "{rel}");
        }
        assert_eq!(
            record_path(base, "x.jsonl").expect("path"),
            Path::new("/b/x.jsonl.jsonl")
        );
    }

    #[test]
    fn rel_of_record_rejects_foreign_paths() {
        let base = Path::new("/b");
        assert_eq!(rel_of_record(base, Path::new("/other/a.rs.jsonl")), None);
        assert_eq!(rel_of_record(base, Path::new("/b/a.rs.tmp")), None);
        assert_eq!(rel_of_record(base, Path::new("/b/.jsonl")), None);
        assert_eq!(rel_of_record(base, base), None);
    }

    #[test]
    fn is_reserved_rel_cases() {
        for reserved in [
            ".knot-lite/shards/a",
            "node_modules/x",
            "a/target/b",
            ".git/config",
        ] {
            assert!(is_reserved_rel(reserved), "{reserved}");
        }
        for allowed in ["targets/x", "src/dist.rs", "src/lib.rs", ".github/ci.yml"] {
            assert!(!is_reserved_rel(allowed), "{allowed}");
        }
    }

    #[test]
    fn apply_commit_mode_is_mutually_exclusive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        let read = |path: &Path| std::fs::read_to_string(path).expect("read");

        apply_commit_mode(&layout, CommitMode::Gitignore).expect("gitignore");
        assert_eq!(read(&layout.gitignore), GITIGNORE_BODY);
        assert!(!layout.gitattributes.exists());

        apply_commit_mode(&layout, CommitMode::Commit).expect("commit");
        assert_eq!(read(&layout.gitattributes), GITATTRIBUTES_BODY);
        assert!(!layout.gitignore.exists());

        apply_commit_mode(&layout, CommitMode::Gitignore).expect("gitignore again");
        assert_eq!(read(&layout.gitignore), GITIGNORE_BODY);
        assert!(!layout.gitattributes.exists());
    }

    #[test]
    fn gitignore_body_ignores_everything() {
        assert!(GITIGNORE_BODY.lines().any(|line| line == "*"));
        assert!(GITATTRIBUTES_BODY.contains("linguist-generated=true -diff"));
    }

    #[test]
    fn commit_mode_serialises_snake_case_and_defaults_to_gitignore() {
        assert_eq!(CommitMode::default(), CommitMode::Gitignore);
        assert_eq!(
            serde_json::to_string(&CommitMode::Commit).expect("json"),
            "\"commit\""
        );
        assert_eq!(
            serde_json::from_str::<CommitMode>("\"gitignore\"").expect("parse"),
            CommitMode::Gitignore
        );
    }
}
