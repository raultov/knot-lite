//! Path <-> LSP file-URI conversions.
//!
//! Paths are built with `std::path` and converted through the `url` crate
//! (never string concatenation). Percent-encoding (e.g. spaces) is handled by
//! the `Url` type; symlink escapes (knot-site's `node_modules` symlink) are
//! rejected so LSP locations outside the repository classify as external.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use async_lsp::lsp_types::Url;

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
}

/// Convert an absolute filesystem path into a `file://` URI.
pub fn path_to_uri(abs: &Path) -> Result<Url> {
    Url::from_file_path(abs).map_err(|()| anyhow!("not an absolute path: {}", abs.display()))
}

/// Map a `file://` URI back to a repo-relative path with `/` separators.
///
/// Returns `None` for non-`file` URIs, for paths outside `root`, and for paths
/// whose canonical form escapes `root` (symlinked directories such as
/// knot-site's `node_modules` point outside the repository and are external).
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
        && !canonical.starts_with(&root.canonical)
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
