//! Path <-> LSP file-URI conversions.
//!
//! Paths are built with `std::path` and converted through the `url` crate
//! (never string concatenation). Percent-encoding (e.g. spaces) is handled by
//! the `Url` type; symlink escapes (knot-site's `node_modules` symlink) are
//! rejected so LSP locations outside the repository classify as external.
//!
//! [`CanonicalRoot::contains_file`] is **the** rule for "may this repository
//! file be read?". The index selection and every reader at query time (the
//! changed-file walk, the re-parse, the documents opened in a language
//! server, the outlines) ask it, so a file the index would not read is never
//! read through another door.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use async_lsp::lsp_types::Url;

use crate::capability::scan::walk_reaches;

/// A repository root with its canonical form computed once.
///
/// Canonicalising the root for every location a server returns would hit the
/// filesystem thousands of times per answer; a session builds one of these and
/// reuses it.
#[derive(Debug, Clone)]
pub struct CanonicalRoot {
    /// The root as configured (the form used to build the URIs sent to servers).
    raw: PathBuf,
    /// The root with symlinks resolved; equals `raw` when canonicalisation fails.
    canonical: PathBuf,
}

impl CanonicalRoot {
    /// Canonicalise `root` (falling back to the path as given when it cannot be).
    pub fn new(root: &Path) -> Self {
        Self {
            raw: root.to_path_buf(),
            canonical: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        }
    }

    /// The root as configured.
    pub fn raw(&self) -> &Path {
        &self.raw
    }

    /// The root with symlinks resolved (the root as configured when that fails).
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }

    /// Whether `path` exists and its canonical form (symlinks resolved) lies
    /// inside the repository: a symlink that leads out of the root is not.
    pub fn contains(&self, path: &Path) -> bool {
        path.canonicalize()
            .is_ok_and(|canonical| canonical.starts_with(&self.canonical))
    }

    /// Whether `path` is a regular file of the repository that may be read.
    ///
    /// It is not when it is missing or not a regular file (FIFO, socket,
    /// directory), nor when a symlink — the file itself or a directory on the
    /// way — takes it out of the root or to a file the index selection does
    /// not reach by its own path (gitignored, under `.git/`, `.knot-lite/`
    /// or a build-output directory): reading it would hand out content the
    /// index never describes.
    pub fn contains_file(&self, path: &Path) -> bool {
        path.canonicalize()
            .is_ok_and(|resolved| resolved.is_file() && self.admits(path, &resolved))
    }

    /// Whether `resolved`, the canonical form of `path`, is a place of the
    /// repository `path` may stand for: `path` itself (no symlink below the
    /// root), or a path the index selection reaches on its own.
    fn admits(&self, path: &Path, resolved: &Path) -> bool {
        let Ok(resolved_rel) = resolved.strip_prefix(&self.canonical) else {
            return false;
        };
        let lexical_rel = path
            .strip_prefix(&self.raw)
            .or_else(|_| path.strip_prefix(&self.canonical));
        if lexical_rel.is_ok_and(|rel| rel == resolved_rel) {
            return true;
        }
        self.raw
            .to_str()
            .is_some_and(|root| walk_reaches(root, &self.raw.join(resolved_rel)))
    }
}

/// `root` + a `/`-separated repo-relative path, component by component so
/// Windows never sees mixed separators.
pub fn join_rel(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// Convert an absolute filesystem path into a `file://` URI.
pub fn path_to_uri(abs: &Path) -> Result<Url> {
    Url::from_file_path(abs).map_err(|()| anyhow!("not an absolute path: {}", abs.display()))
}

/// Map a `file://` URI back to a repo-relative path with `/` separators.
///
/// Returns `None` for non-`file` URIs, for paths outside `root`, and for paths
/// whose canonical form escapes `root` (symlinked directories such as
/// knot-site's `node_modules` point outside the repository and are external)
/// or is a file the index selection does not reach (a symlink to a gitignored
/// file): see [`CanonicalRoot::contains_file`].
pub fn uri_to_repo_relative(uri: &Url, root: &Path) -> Option<String> {
    uri_to_repo_relative_with(uri, &CanonicalRoot::new(root))
}

/// [`uri_to_repo_relative`] against a precomputed [`CanonicalRoot`] (hot path).
///
/// A server may report the repository through its canonical path even when the
/// root was configured through a symlink, so the relative path is taken against
/// the raw root first and the canonical root second.
pub fn uri_to_repo_relative_with(uri: &Url, root: &CanonicalRoot) -> Option<String> {
    let path = uri.to_file_path().ok()?;
    if let Ok(canonical) = path.canonicalize()
        && !root.admits(&path, &canonical)
    {
        return None;
    }
    let rel = strip_root(&path, &root.raw).or_else(|| strip_root(&path, &root.canonical))?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    if rel.is_empty() { None } else { Some(rel) }
}

/// `path` relative to `root`, ignoring the case of a Windows drive letter
/// (`c:\x` and `C:\x` are the same place; URIs and `canonicalize` disagree).
fn strip_root(path: &Path, root: &Path) -> Option<PathBuf> {
    path.strip_prefix(root)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| strip_root_ignoring_drive_case(path, root))
}

#[cfg(windows)]
fn strip_root_ignoring_drive_case(path: &Path, root: &Path) -> Option<PathBuf> {
    lower_drive_letter(path)
        .strip_prefix(lower_drive_letter(root))
        .ok()
        .map(Path::to_path_buf)
}

/// Lower-case a leading `X:` so two spellings of the same drive compare equal.
#[cfg(windows)]
fn lower_drive_letter(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => {
            PathBuf::from(format!("{}{}", letter.to_ascii_lowercase(), &text[1..]))
        }
        _ => path.to_path_buf(),
    }
}

#[cfg(not(windows))]
fn strip_root_ignoring_drive_case(_path: &Path, _root: &Path) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn round_trip_preserves_spaces() {
        let abs = Path::new("/tmp/knot-rag spike/src/a file.rs");
        let uri = path_to_uri(abs).expect("absolute path converts");
        assert_eq!(uri.as_str(), "file:///tmp/knot-rag%20spike/src/a%20file.rs");
        assert_eq!(
            uri_to_repo_relative(&uri, Path::new("/tmp/knot-rag spike")),
            Some("src/a file.rs".to_owned())
        );
    }

    #[test]
    fn relative_path_is_not_a_uri() {
        assert!(path_to_uri(Path::new("src/lib.rs")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn windows_style_uri_is_external_on_unix_root() {
        let uri = Url::parse("file:///C:/x/y.rs").expect("valid URI");
        assert_eq!(uri_to_repo_relative(&uri, Path::new("/tmp/root")), None);
    }

    #[test]
    fn join_rel_adds_one_component_per_segment() {
        let root = std::env::temp_dir();
        assert_eq!(
            join_rel(&root, "src/a/b.rs"),
            root.join("src").join("a").join("b.rs")
        );
        assert_eq!(join_rel(&root, "b.rs"), root.join("b.rs"));
    }

    #[test]
    fn non_file_uris_are_rejected() {
        let uri = Url::parse("https://example.com/a.rs").expect("valid URI");
        assert_eq!(uri_to_repo_relative(&uri, Path::new("/tmp/root")), None);
    }

    #[test]
    fn the_root_itself_is_not_a_repo_relative_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uri = path_to_uri(dir.path()).expect("URI");
        assert_eq!(uri_to_repo_relative(&uri, dir.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directory_target_is_external() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("src")).expect("create root");
        std::fs::create_dir_all(&outside).expect("create outside");
        let file = outside.join("leaked.rs");
        std::fs::write(&file, "fn leaked() {}\n").expect("write file");
        std::os::unix::fs::symlink(&outside, root.join("node_modules")).expect("symlink");

        // URI that literally lies under the root but resolves outside it.
        let via_symlink = root.join("node_modules").join("leaked.rs");
        let uri = path_to_uri(&via_symlink).expect("URI");
        assert_eq!(uri_to_repo_relative(&uri, &root), None);

        // A real file under the root maps back to its relative path.
        let real = root.join("src").join("real.rs");
        std::fs::write(&real, "fn real() {}\n").expect("write real");
        let uri = path_to_uri(&real).expect("URI");
        assert_eq!(
            uri_to_repo_relative(&uri, &root),
            Some("src/real.rs".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_uri_maps_back_when_the_root_is_a_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real_root = dir.path().join("real");
        std::fs::create_dir_all(real_root.join("src")).expect("create root");
        std::fs::write(real_root.join("src").join("a.rs"), "fn a() {}\n").expect("write");
        let link_root = dir.path().join("link");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("symlink");

        // The server answers with canonical URIs, the root was configured through the link.
        let canonical_file = real_root
            .canonicalize()
            .expect("canonical")
            .join("src")
            .join("a.rs");
        let uri = path_to_uri(&canonical_file).expect("URI");
        assert_eq!(
            uri_to_repo_relative(&uri, &link_root),
            Some("src/a.rs".to_owned())
        );
    }

    /// A git repository with a gitignored file, a gitignored directory, and
    /// links into them, into `.git/`, to an ordinary file and out of the
    /// repository. `None` when git is unavailable.
    #[cfg(unix)]
    fn repo_with_links() -> Option<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        let write = |rel: &str, body: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, body).expect("write");
        };
        write("repo/src/lib.rs", "pub fn f() {}\n");
        write("repo/secret.rs", "pub fn secret_token_fn() {}\n");
        write("repo/private/inner.rs", "pub fn private_fn() {}\n");
        write("repo/.gitignore", "secret.rs\nprivate/\n");
        write("outside/out.rs", "pub fn outside_fn() {}\n");
        if !crate::index::fixture::git_init(&root) {
            return None;
        }
        let link = |target: &str, rel: &str| {
            std::os::unix::fs::symlink(target, root.join(rel)).expect("symlink");
        };
        link("lib.rs", "src/alias.rs");
        link("../secret.rs", "src/leak.rs");
        link("../.git/config", "src/gitcfg.rs");
        link("../../outside/out.rs", "src/out.rs");
        link("private", "linked");
        link("src", "mirror");
        Some((dir, root))
    }

    /// The reading rule (#4, #10, #30, #34): a path is read only when the
    /// file it resolves to is one the index selection reaches by itself,
    /// whether the symlink is the file or a directory on the way.
    #[cfg(unix)]
    #[test]
    fn contains_file_follows_links_only_to_files_the_index_selects() {
        let Some((_dir, root)) = repo_with_links() else {
            eprintln!(
                "git unavailable: skipped contains_file_follows_links_only_to_files_the_index_selects"
            );
            return;
        };
        let canonical = CanonicalRoot::new(&root);
        let cases = [
            ("src/lib.rs", true),
            ("src/alias.rs", true),
            ("mirror/lib.rs", true),
            ("src/leak.rs", false),
            ("src/gitcfg.rs", false),
            ("src/out.rs", false),
            ("linked/inner.rs", false),
            ("src/missing.rs", false),
            ("src", false),
        ];
        for (rel, expected) in cases {
            assert_eq!(
                canonical.contains_file(&join_rel(&root, rel)),
                expected,
                "{rel}"
            );
        }
        // A file that is itself ignored but reached without a link is not
        // this rule's business: the selection decides it by its own path.
        assert!(canonical.contains_file(&root.join("secret.rs")));
    }

    /// Every reader of repository files at query time refuses what the rule
    /// refuses: a new reader belongs in this list (#34).
    #[cfg(unix)]
    #[test]
    fn every_query_time_reader_applies_the_reading_rule() {
        let Some((_dir, root)) = repo_with_links() else {
            eprintln!("git unavailable: skipped every_query_time_reader_applies_the_reading_rule");
            return;
        };
        let canonical = CanonicalRoot::new(&root);
        let parse_cfg = knot::pipeline::parser::ParseConfig {
            repo_root: root.clone(),
            custom_queries_path: None,
            repo_name: "demo".to_owned(),
            include_config_files: false,
            repo_path: root.to_str().map(str::to_owned),
        };
        for (rel, allowed) in [
            ("src/alias.rs", true),
            ("src/leak.rs", false),
            ("src/gitcfg.rs", false),
            ("src/out.rs", false),
            ("linked/inner.rs", false),
        ] {
            let path = join_rel(&root, rel);
            // Locations a language server reports.
            let uri = path_to_uri(&path).expect("URI");
            assert_eq!(
                uri_to_repo_relative_with(&uri, &canonical).is_some(),
                allowed,
                "location: {rel}"
            );
            // The source cache of `find_callers` (positions, freshness rule).
            let mut sources = crate::lsp::request::SourceFiles::new(&root);
            assert_eq!(sources.get(rel).is_ok(), allowed, "source: {rel}");
            // The re-parse of a changed file (re-anchoring, overlay).
            let fresh = crate::index::reparse::reparse(&root, rel, &parse_cfg);
            assert_eq!(fresh.is_ok(), allowed, "re-parse: {rel}");
            // The outline of a file the index does not hold.
            assert_eq!(
                crate::query::explore_refs::on_disk_candidate(&root, rel),
                allowed,
                "explore: {rel}"
            );
        }
    }

    #[test]
    fn canonical_root_matches_free_function() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("src")).expect("create root");
        let file = root.join("src").join("lib.rs");
        std::fs::write(&file, "pub fn f() {}\n").expect("write");
        let canonical_root = CanonicalRoot::new(&root);
        assert_eq!(canonical_root.raw(), root);
        for target in [
            file,
            root.join("src").join("missing.rs"),
            dir.path().join("x.rs"),
        ] {
            let uri = path_to_uri(&target).expect("URI");
            assert_eq!(
                uri_to_repo_relative_with(&uri, &canonical_root),
                uri_to_repo_relative(&uri, &root),
                "{}",
                target.display()
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_letter_case_is_ignored() {
        let upper = Url::parse("file:///C:/repo/src/a.rs").expect("valid URI");
        assert_eq!(
            uri_to_repo_relative(&upper, Path::new("c:\\repo")),
            Some("src/a.rs".to_owned())
        );
        let lower = Url::parse("file:///c:/repo/src/a.rs").expect("valid URI");
        assert_eq!(
            uri_to_repo_relative(&lower, Path::new("C:\\repo")),
            Some("src/a.rs".to_owned())
        );
    }
}
