//! Building a [`LiveRequest`] for one indexed target (§6.4 step 2, P3-D10).
//!
//! The index stores the UTF-16 column of a declaration name but not its line,
//! which may sit below `l0` (attributes, annotations, multi-line signatures).
//! The line therefore comes from locating the name again in the file **as it is
//! now**, after checking the file still has the content the index was built
//! from. Pre-computation and live queries share this code, so both ask the
//! server about the same position.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use async_lsp::lsp_types::Position;

use super::ops::{ops_for, shape_of};
use super::position::locate_name;
use super::target::{DEFAULT_RETRY_PAUSE, LiveRequest};
use super::uri::join_rel;
use crate::capability::languages::LspLang;
use crate::index::hash::{sha256_hex, short16};
use crate::index::shard::ShardLine;

/// The current content of one repository file.
#[derive(Debug)]
pub struct SourceText {
    text: String,
    sha16: String,
}

impl SourceText {
    /// The file's text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The first 16 hex characters of the SHA-256 of the file (the inventory's hash).
    pub fn sha16(&self) -> &str {
        &self.sha16
    }
}

/// Repository files, each read at most once.
#[derive(Debug)]
pub struct SourceFiles<'a> {
    root: &'a Path,
    files: HashMap<String, Result<SourceText, String>>,
}

impl<'a> SourceFiles<'a> {
    /// A cache over the repository rooted at `root`.
    pub fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: HashMap::new(),
        }
    }

    /// Absolute path of `rel`.
    pub fn abs_path(&self, rel: &str) -> PathBuf {
        join_rel(self.root, rel)
    }

    /// The content of `rel`, or why it could not be read.
    pub fn get(&mut self, rel: &str) -> Result<&SourceText, &str> {
        let root = self.root;
        self.files
            .entry(rel.to_owned())
            .or_insert_with(|| read_source(&join_rel(root, rel)))
            .as_ref()
            .map_err(String::as_str)
    }
}

fn read_source(path: &Path) -> Result<SourceText, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let sha16 = short16(sha256_hex(&bytes));
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(SourceText { text, sha16 })
}

/// One indexed target to ask a language server about.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// Repo-relative file of the target.
    pub rel: &'a str,
    /// Its record.
    pub line: &'a ShardLine,
    /// The server's language.
    pub lang: LspLang,
    /// The file's hash in the index's inventory; `None` skips the stale check.
    pub indexed_sha16: Option<&'a str>,
}

/// Why a target cannot be positioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unlocated {
    /// The file cannot be read.
    Unreadable(String),
    /// The file changed since it was indexed.
    Stale,
    /// The declaration name is not in the file.
    NameNotFound,
}

impl Unlocated {
    /// A human-readable reason.
    pub fn detail(&self) -> String {
        match self {
            Self::Unreadable(reason) => reason.clone(),
            Self::Stale => "the file changed since it was indexed".to_owned(),
            Self::NameNotFound => "the declaration name is not in the file".to_owned(),
        }
    }
}

/// How a request is paced.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    /// Caller containers the heuristic found (decides retry-on-empty).
    pub heuristic_hint: usize,
    /// When the caller stops waiting.
    pub deadline: Instant,
}

/// The LSP position of the target's declaration name in the current file.
pub fn locate_target(
    files: &mut SourceFiles<'_>,
    target: &Target<'_>,
) -> Result<Position, Unlocated> {
    let source = files
        .get(target.rel)
        .map_err(|reason| Unlocated::Unreadable(reason.to_owned()))?;
    if target
        .indexed_sha16
        .is_some_and(|indexed| indexed != source.sha16())
    {
        return Err(Unlocated::Stale);
    }
    let line = target.line;
    let found = locate_name(source.text(), &line.name, line.l0, line.l1, target.lang)
        .ok_or(Unlocated::NameNotFound)?;
    Ok(Position {
        line: found.line0,
        character: found.col_utf16,
    })
}

/// The request that asks the server about `target`.
pub fn live_request(
    files: &mut SourceFiles<'_>,
    target: &Target<'_>,
    pacing: Pacing,
) -> Result<LiveRequest, Unlocated> {
    let position = locate_target(files, target)?;
    Ok(LiveRequest {
        lang: target.lang,
        target_id: target.line.id.clone(),
        abs_file: files.abs_path(target.rel),
        rel_file: target.rel.to_owned(),
        position,
        ops: ops_for(shape_of(&target.line.kind)),
        heuristic_hint: pacing.heuristic_hint,
        deadline: pacing.deadline,
        retry_pause: DEFAULT_RETRY_PAUSE,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn record(name: &str, l0: usize, l1: usize) -> ShardLine {
        ShardLine {
            id: format!("id-{name}"),
            kind: "rust_function".to_owned(),
            name: name.to_owned(),
            fqn: name.to_owned(),
            lang: "rust".to_owned(),
            l0,
            l1,
            col: Some(0),
            sig: None,
            h: "h".to_owned(),
            q: None,
            s: None,
        }
    }

    fn pacing() -> Pacing {
        Pacing {
            heuristic_hint: 3,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }

    fn repo_with(rel: &str, text: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = join_rel(tmp.path(), rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
        tmp
    }

    #[test]
    fn name_below_the_start_line_is_located_on_its_own_line() {
        // An attribute line comes first: `l0` is the attribute, the name sits one line below.
        let tmp = repo_with("src/lib.rs", "#[inline]\npub fn target() {}\n");
        let line = record("target", 1, 2);
        let mut files = SourceFiles::new(tmp.path());
        let target = Target {
            rel: "src/lib.rs",
            line: &line,
            lang: LspLang::Rust,
            indexed_sha16: None,
        };
        let position = locate_target(&mut files, &target).expect("located");
        assert_eq!(
            position,
            Position {
                line: 1,
                character: 7
            }
        );
    }

    #[test]
    fn live_request_carries_position_ops_and_pacing() {
        let tmp = repo_with("src/lib.rs", "pub fn target() {}\n");
        let line = record("target", 1, 1);
        let mut files = SourceFiles::new(tmp.path());
        let target = Target {
            rel: "src/lib.rs",
            line: &line,
            lang: LspLang::Rust,
            indexed_sha16: None,
        };
        let request = live_request(&mut files, &target, pacing()).expect("request");
        assert_eq!(request.target_id, "id-target");
        assert_eq!(request.rel_file, "src/lib.rs");
        assert_eq!(request.abs_file, join_rel(tmp.path(), "src/lib.rs"));
        assert_eq!(request.heuristic_hint, 3);
        assert!(request.ops.incoming && request.ops.references && !request.ops.implementation);
    }

    #[test]
    fn matching_hash_passes_and_a_changed_file_is_stale() {
        let tmp = repo_with("src/lib.rs", "pub fn target() {}\n");
        let line = record("target", 1, 1);
        let indexed = short16(sha256_hex(b"pub fn target() {}\n"));
        let mut files = SourceFiles::new(tmp.path());
        let fresh = Target {
            rel: "src/lib.rs",
            line: &line,
            lang: LspLang::Rust,
            indexed_sha16: Some(&indexed),
        };
        assert!(locate_target(&mut files, &fresh).is_ok());

        let stale = Target {
            indexed_sha16: Some("0000000000000000"),
            ..fresh
        };
        assert_eq!(locate_target(&mut files, &stale), Err(Unlocated::Stale));
    }

    #[test]
    fn unreadable_file_and_missing_name_are_reported() {
        let tmp = repo_with("src/lib.rs", "pub fn other() {}\n");
        let line = record("target", 1, 1);
        let mut files = SourceFiles::new(tmp.path());
        let missing_file = Target {
            rel: "src/gone.rs",
            line: &line,
            lang: LspLang::Rust,
            indexed_sha16: None,
        };
        assert!(matches!(
            locate_target(&mut files, &missing_file),
            Err(Unlocated::Unreadable(_))
        ));

        let missing_name = Target {
            rel: "src/lib.rs",
            ..missing_file
        };
        assert_eq!(
            locate_target(&mut files, &missing_name),
            Err(Unlocated::NameNotFound)
        );
    }

    #[test]
    fn each_file_is_read_once() {
        let tmp = repo_with("src/lib.rs", "pub fn target() {}\n");
        let mut files = SourceFiles::new(tmp.path());
        let first = files.get("src/lib.rs").expect("read").sha16().to_owned();
        // Rewriting the file does not change what the cache already holds.
        std::fs::write(join_rel(tmp.path(), "src/lib.rs"), "changed\n").expect("write");
        assert_eq!(files.get("src/lib.rs").expect("cached").sha16(), first);
    }

    #[test]
    fn unlocated_details_are_non_empty() {
        for why in [
            Unlocated::Unreadable("boom".to_owned()),
            Unlocated::Stale,
            Unlocated::NameNotFound,
        ] {
            assert!(!why.detail().is_empty());
        }
    }
}
