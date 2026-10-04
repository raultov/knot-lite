//! Path <-> LSP file-URI conversions.
//!
//! Paths are built with `std::path` and converted through the `url` crate
//! (never string concatenation). Percent-encoding (e.g. spaces) is handled by
//! the `Url` type; symlink escapes (knot-site's `node_modules` symlink) are
//! rejected so LSP locations outside the snapshot classify as external.

use std::path::Path;

use anyhow::{Result, anyhow};

use async_lsp::lsp_types::Url;

/// Convert an absolute filesystem path into a `file://` URI.
pub fn path_to_uri(abs: &Path) -> Result<Url> {
    Url::from_file_path(abs).map_err(|()| anyhow!("not an absolute path: {}", abs.display()))
}

/// Map a `file://` URI back to a repo-relative path with `/` separators.
///
/// Returns `None` for non-`file` URIs, for paths outside `root`, and for paths
/// whose canonical form escapes `root` (symlinked directories such as
/// knot-site's `node_modules` point outside the snapshot and are external).
pub fn uri_to_repo_relative(uri: &Url, root: &Path) -> Option<String> {
    let path = uri.to_file_path().ok()?;
    if let (Ok(canonical), Ok(root_canonical)) = (path.canonicalize(), root.canonicalize())
        && !canonical.starts_with(&root_canonical)
    {
        return None;
    }
    let rel = path.strip_prefix(root).ok()?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    if rel.is_empty() { None } else { Some(rel) }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn windows_style_uri_is_external_on_unix_root() {
        let uri = Url::parse("file:///C:/x/y.rs").expect("valid URI");
        assert_eq!(uri_to_repo_relative(&uri, Path::new("/tmp/root")), None);
    }

    #[test]
    fn relative_and_non_file_uris_rejected() {
        assert_eq!(
            uri_to_repo_relative(
                &Url::parse("https://example.com/a.rs").expect("valid URI"),
                Path::new("/tmp/root")
            ),
            None
        );
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
}
