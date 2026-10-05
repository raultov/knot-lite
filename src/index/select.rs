//! One place decides what the index looks at: the build, the inventory and
//! `status` all go through [`select_files`] (P2-D9: the index never indexes
//! itself and never reads build output).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use super::chunk::is_tier1_candidate;
use super::layout::is_reserved_rel;
use crate::capability::scan::walk_repository;

/// The files one index run considers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Absolute paths knot will parse (`discover_files(root, false)` minus
    /// reserved dirs), sorted by repo-relative path.
    pub parse: Vec<PathBuf>,
    /// Repo-relative (`/`-separated) paths of `parse`, same order.
    pub parse_rel: Vec<String>,
    /// `(rel, absolute)` of tier-1 candidates: not in `parse`, accepted by
    /// [`is_tier1_candidate`].
    /// Sorted by `rel`.
    pub tier1_candidates: Vec<(String, PathBuf)>,
}

/// Decide the parse set and the tier-1 candidates of the repository at `root`.
pub fn select_files(root: &Path) -> anyhow::Result<Selection> {
    let root_str = root
        .to_str()
        .context("repository path is not valid UTF-8")?;
    let mut parse: Vec<(String, PathBuf)> = knot::pipeline::input::discover_files(root_str, false)?
        .into_iter()
        .map(|path| (relative(&path, root), path))
        .filter(|(rel, _)| !is_reserved_rel(rel))
        .collect();
    parse.sort_by(|a, b| a.0.cmp(&b.0));

    let parsed: HashSet<&Path> = parse.iter().map(|(_, path)| path.as_path()).collect();
    let mut tier1_candidates: Vec<(String, PathBuf)> = walk_repository(root_str)
        .into_iter()
        .filter(|path| !parsed.contains(path.as_path()) && is_tier1_candidate(path))
        .map(|path| (relative(&path, root), path))
        .collect();
    tier1_candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let (parse_rel, parse) = parse.into_iter().unzip();
    Ok(Selection {
        parse,
        parse_rel,
        tier1_candidates,
    })
}

/// Repo-relative path with `/` separators on every platform.
fn relative(path: &Path, root: &Path) -> String {
    knot::pipeline::files::to_repo_relative(path, root).replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::chunk::MAX_TIER1_BYTES;
    use std::fs;
    use std::process::Command;

    fn write(root: &Path, rel: &str, body: &[u8]) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, body).expect("write");
    }

    fn tier1_rels(selection: &Selection) -> Vec<&str> {
        selection
            .tier1_candidates
            .iter()
            .map(|(rel, _)| rel.as_str())
            .collect()
    }

    #[test]
    fn select_excludes_index_and_build_output_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write(root, ".knot-lite/shards/x.rs.jsonl", b"{}\n");
        write(root, ".knot-lite/manifest.toml", b"schema = \"x\"\n");
        write(root, "target/a.rs", b"pub fn a() {}\n");
        write(root, "node_modules/b/run.sh", b"echo\n");
        write(root, "src/lib.rs", b"pub fn f() {}\n");
        write(root, "Makefile", b"all:\n");

        let selection = select_files(root).expect("select");
        assert_eq!(selection.parse_rel, vec!["src/lib.rs"]);
        assert_eq!(selection.parse, vec![root.join("src/lib.rs")]);
        assert_eq!(tier1_rels(&selection), vec!["Makefile"]);
    }

    #[test]
    fn select_honours_gitignore_inside_git() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let git_ok = Command::new("git")
            .arg("init")
            .current_dir(root)
            .output()
            .is_ok_and(|out| out.status.success());
        if !git_ok {
            eprintln!("git unavailable: skipped select_honours_gitignore_inside_git");
            return;
        }
        write(root, ".gitignore", b"ignored.rs\nignored.sh\n");
        write(root, "src/lib.rs", b"pub fn f() {}\n");
        write(root, "ignored.rs", b"pub fn i() {}\n");
        write(root, "ignored.sh", b"echo\n");
        write(root, "run.sh", b"echo\n");

        let selection = select_files(root).expect("select");
        assert_eq!(selection.parse_rel, vec!["src/lib.rs"]);
        assert_eq!(tier1_rels(&selection), vec![".gitignore", "run.sh"]);
    }

    #[test]
    fn select_skips_large_and_lock_tier1_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write(root, "ok.txt", b"hello\n");
        write(root, "exact.txt", &vec![b'a'; MAX_TIER1_BYTES as usize]);
        write(root, "big.txt", &vec![b'a'; MAX_TIER1_BYTES as usize + 1]);
        write(root, "Cargo.lock", b"lock\n");
        write(root, "package-lock.json", b"{}\n");

        let selection = select_files(root).expect("select");
        assert_eq!(tier1_rels(&selection), vec!["exact.txt", "ok.txt"]);
    }

    #[test]
    fn select_is_sorted_and_relative_with_slashes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for rel in ["b/z.rs", "a/y.rs", "a/b/x.rs", "a.rs"] {
            write(root, rel, b"pub fn f() {}\n");
        }
        for rel in ["c.sh", "a.sh", "d/e.sh"] {
            write(root, rel, b"echo\n");
        }

        let selection = select_files(root).expect("select");
        assert_eq!(
            selection.parse_rel,
            vec!["a.rs", "a/b/x.rs", "a/y.rs", "b/z.rs"]
        );
        assert_eq!(selection.parse.len(), selection.parse_rel.len());
        for (rel, path) in selection.parse_rel.iter().zip(&selection.parse) {
            assert_eq!(path, &root.join(rel));
        }
        assert_eq!(tier1_rels(&selection), vec!["a.sh", "c.sh", "d/e.sh"]);
    }
}
