//! One place decides what the index looks at: the build, the inventory and
//! `status` all go through [`select_files`] (P2-D9: the index never indexes
//! itself and never reads build output).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use super::chunk::is_tier1_candidate;
use super::layout::is_reserved_rel;
use crate::capability::scan::{walk_reaches, walk_repository};
use crate::lsp::uri::CanonicalRoot;

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
    // A file symlink that resolves outside the repository, or to a file the
    // walk never selects by its own path (gitignored, under `.git/` or
    // `.knot-lite/`), is never read: its content would end up in
    // `.knot-lite/` (directory symlinks are already not followed by the
    // walkers).
    let canonical_root = CanonicalRoot::new(root);
    let mut parse: Vec<(String, PathBuf)> = knot::pipeline::input::discover_files(root_str, false)?
        .into_iter()
        .filter(|path| canonical_root.contains(path))
        .filter(|path| link_target_selectable(root, &canonical_root, path))
        .map(|path| (relative(&path, root), path))
        .filter(|(rel, _)| !is_reserved_rel(rel))
        .collect();
    parse.sort_by(|a, b| a.0.cmp(&b.0));

    let parsed: HashSet<&Path> = parse.iter().map(|(_, path)| path.as_path()).collect();
    let mut tier1_candidates: Vec<(String, PathBuf)> = walk_repository(root_str)
        .into_iter()
        .filter(|path| {
            !parsed.contains(path.as_path())
                && is_tier1_candidate(path)
                && canonical_root.contains(path)
        })
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

/// Whether the file at `path` may be read for `root`'s index as far as
/// symlinks go: it is no symlink, or it resolves to a regular file inside the
/// repository that the walk reaches by its own path (not under a reserved or
/// build-output directory such as `.git/`, not gitignored).
pub(crate) fn link_target_selectable(
    root: &Path,
    canonical_root: &CanonicalRoot,
    path: &Path,
) -> bool {
    let is_link = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    if !is_link {
        return true;
    }
    let Ok(target) = path.canonicalize() else {
        return false;
    };
    let Ok(target_rel) = target.strip_prefix(canonical_root.canonical()) else {
        return false;
    };
    target.is_file()
        && root
            .to_str()
            .is_some_and(|root_str| walk_reaches(root_str, &root.join(target_rel)))
}

/// Repo-relative path with `/` separators on every platform.
fn relative(path: &Path, root: &Path) -> String {
    knot::pipeline::files::to_repo_relative(path, root).replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::chunk::MAX_TIER1_BYTES;
    use crate::index::fixture::git_init;
    use std::fs;

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
        if !git_init(root) {
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

    #[cfg(unix)]
    #[test]
    fn select_skips_file_symlinks_leading_outside_the_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        write(&outside, "secret.rs", b"pub fn leaked() {}\n");
        write(&outside, "secret.sh", b"echo leaked\n");
        write(&root, "src/lib.rs", b"pub fn f() {}\n");
        write(&root, "run.sh", b"echo\n");
        let link = |target: &str, rel: &str| {
            std::os::unix::fs::symlink(target, root.join(rel)).expect("symlink");
        };
        link("../../outside/secret.rs", "src/leak.rs");
        link("../outside/secret.sh", "leak.sh");
        link("lib.rs", "src/alias.rs");

        let selection = select_files(&root).expect("select");
        assert_eq!(selection.parse_rel, vec!["src/alias.rs", "src/lib.rs"]);
        assert_eq!(tier1_rels(&selection), vec!["run.sh"]);
    }

    /// An in-repository file symlink is read only when its target is a file
    /// the walk selects by its own path: never a gitignored file nor one
    /// under `.git/` (#30).
    #[cfg(unix)]
    #[test]
    fn select_skips_file_symlinks_to_ignored_or_reserved_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        if !git_init(root) {
            eprintln!(
                "git unavailable: skipped select_skips_file_symlinks_to_ignored_or_reserved_files"
            );
            return;
        }
        write(root, ".gitignore", b".env\n");
        write(root, ".env", b"API_TOKEN=envsecretzq\n");
        write(root, "README.md", b"# Readme\n");
        write(root, "src/lib.rs", b"pub fn f() {}\n");
        let link = |target: &str, rel: &str| {
            std::os::unix::fs::symlink(target, root.join(rel)).expect("symlink");
        };
        link(".env", "notes.md");
        link(".git/config", "gitcfg.md");
        link("README.md", "alias.md");

        let selection = select_files(root).expect("select");
        assert_eq!(
            selection.parse_rel,
            vec!["README.md", "alias.md", "src/lib.rs"]
        );
        assert_eq!(tier1_rels(&selection), vec![".gitignore"]);
        let canonical = CanonicalRoot::new(root);
        assert!(!link_target_selectable(
            root,
            &canonical,
            &root.join("notes.md")
        ));
        assert!(link_target_selectable(
            root,
            &canonical,
            &root.join("README.md")
        ));
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
