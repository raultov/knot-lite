//! On-disk index layout under `<repo>/.knot-lite/`.
//!
//! Phase 1 only defines the index root and the manifest path. The other
//! Appendix F.3 paths (`shards/`, `edges/`, `cache/`, `.gitattributes`) are
//! defined by Phase 2 together with their writers.

use std::path::{Path, PathBuf};

/// Name of the per-repository index directory.
pub const INDEX_DIR: &str = ".knot-lite";

/// Name of the manifest file inside [`INDEX_DIR`].
pub const MANIFEST_FILE: &str = "manifest.toml";

/// Manifest schema this knot-lite version understands (written from Phase 2 on).
pub const MANIFEST_SCHEMA_V1: &str = "knot-lite/manifest/v1";

/// Paths of one repository's knot-lite index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLayout {
    /// Repository root the index belongs to.
    pub root: PathBuf,
    /// The `.knot-lite/` directory.
    pub dir: PathBuf,
    /// The `.knot-lite/manifest.toml` file.
    pub manifest: PathBuf,
}

impl IndexLayout {
    /// Compute the index layout for `repo_root` (pure, no filesystem access).
    pub fn for_repo(repo_root: &Path) -> Self {
        let dir = repo_root.join(INDEX_DIR);
        let manifest = dir.join(MANIFEST_FILE);
        Self {
            root: repo_root.to_path_buf(),
            dir,
            manifest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_repo_joins_index_dir_and_manifest() {
        let layout = IndexLayout::for_repo(Path::new("/tmp/repo"));
        assert_eq!(layout.root, PathBuf::from("/tmp/repo"));
        assert_eq!(layout.dir, PathBuf::from("/tmp/repo").join(INDEX_DIR));
        assert_eq!(
            layout.manifest,
            PathBuf::from("/tmp/repo")
                .join(INDEX_DIR)
                .join(MANIFEST_FILE)
        );
        assert_eq!(INDEX_DIR, ".knot-lite");
        assert_eq!(MANIFEST_FILE, "manifest.toml");
        assert_eq!(MANIFEST_SCHEMA_V1, "knot-lite/manifest/v1");
    }
}
