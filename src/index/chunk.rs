//! Tier-1 files: text files knot cannot parse (or parsed to zero entities)
//! whose content is still worth a vector (Phase 0 T9.3). This module owns the
//! tier-1 policy (allow-list, size cap) and the line-window chunker.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context as _;

use super::select::Selection;
use super::shard::{KIND_CHUNK, KIND_FILE};
use crate::capability::scan::is_lock_file;

/// Windows of 60 lines with 10 lines of overlap (T9.3).
const WINDOW_LINES: usize = 60;
const OVERLAP_LINES: usize = 10;
/// Minimum non-blank lines for a standalone chunk (smaller tails merge).
const MIN_NON_BLANK: usize = 5;
/// Snap the window end to a blank line within this many trailing lines.
const BLANK_SNAP_TAIL: usize = 10;
/// Leading lines used for the file-level vector.
const FILE_HEAD_LINES: usize = 40;

/// Largest tier-1 file considered.
pub const MAX_TIER1_BYTES: u64 = 256 * 1024;

/// Lockfiles are never tier-1 (machine-generated, huge, and duplicated).
const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "poetry.lock",
];

/// Extensions of text files that are useful tier-1 content.
const TIER1_EXTENSIONS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "mk",
    "make",
    "toml",
    "yml",
    "yaml",
    "json",
    "jsonc",
    "ini",
    "cfg",
    "conf",
    "properties",
    "sql",
    "proto",
    "graphql",
    "gql",
    "txt",
];

/// Multi-part and dot-file suffixes from the allow-list (T9.3).
const TIER1_SUFFIXES: &[&str] = &[
    ".env.example",
    "env.example",
    ".dockerignore",
    "dockerignore",
    ".gitignore",
    "gitignore",
    ".editorconfig",
    "editorconfig",
];

/// Extension-less / oddly-named files that are useful tier-1 content.
const TIER1_NAMES: &[&str] = &[
    "Makefile",
    "Dockerfile",
    "Containerfile",
    "Jenkinsfile",
    "Procfile",
    "mvnw",
    "gradlew",
    "Justfile",
    "Vagrantfile",
];

/// Whether the file name passes the tier-1 text allow-list.
pub fn is_tier1_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if LOCKFILES.contains(&name) {
        return false;
    }
    if TIER1_NAMES.contains(&name) {
        return true;
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| TIER1_EXTENSIONS.contains(&ext))
    {
        return true;
    }
    TIER1_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// Whether `path` is eligible for tier 1: allow-listed name, not a lock file
/// and at most [`MAX_TIER1_BYTES`].
pub fn is_tier1_candidate(path: &Path) -> bool {
    is_tier1_name(path)
        && !is_lock_file(path)
        && std::fs::metadata(path).is_ok_and(|meta| meta.len() <= MAX_TIER1_BYTES)
}

/// One vectorised line window (or file summary) of a tier-1 file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// [`KIND_CHUNK`] or [`KIND_FILE`].
    pub kind: &'static str,
    /// File name.
    pub name: String,
    /// Repo-relative path (also the `fqn` of the record).
    pub rel: String,
    /// 1-based first line.
    pub l0: usize,
    /// 1-based last line (inclusive).
    pub l1: usize,
    /// Text handed to the embedder.
    pub text: String,
}

/// A tier-1 file: repo-relative path plus its decoded source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier1File {
    /// Repo-relative path with `/` separators.
    pub rel: String,
    /// UTF-8 source text.
    pub source: String,
}

/// Select and read the tier-1 files of a selection: the candidates (not parsed
/// by knot) plus parsed files that produced zero entities and pass the
/// tier-1 policy. Non-UTF-8 files are skipped. Sorted by `rel`.
pub fn tier1_files(
    selection: &Selection,
    files_with_entities: &BTreeSet<String>,
) -> anyhow::Result<Vec<Tier1File>> {
    let candidates = selection
        .tier1_candidates
        .iter()
        .map(|(rel, path)| (rel.as_str(), path.as_path()));
    let uncovered = selection
        .parse_rel
        .iter()
        .zip(&selection.parse)
        .filter(|(rel, path)| !files_with_entities.contains(*rel) && is_tier1_candidate(path))
        .map(|(rel, path)| (rel.as_str(), path.as_path()));

    let mut files = Vec::new();
    for (rel, path) in candidates.chain(uncovered) {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let Ok(source) = String::from_utf8(bytes) else {
            continue; // non-UTF-8: out of scope
        };
        files.push(Tier1File {
            rel: rel.to_owned(),
            source,
        });
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(files)
}

/// Split one tier-1 file into overlapping line windows plus a file summary
/// (the `file` record is always last).
pub fn chunk_file(rel: &str, source: &str) -> Vec<Chunk> {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_owned();
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let mut end = (start + WINDOW_LINES).min(lines.len());
        if end < lines.len() {
            end = snap_to_blank(&lines, start, end);
        }
        chunks.push(window_chunk(&name, rel, &lines, start, end));
        if end >= lines.len() {
            break;
        }
        start = end.saturating_sub(OVERLAP_LINES);
    }
    merge_tiny_tail(&mut chunks, rel, &lines);

    let head: Vec<&str> = lines.iter().take(FILE_HEAD_LINES).copied().collect();
    chunks.push(Chunk {
        kind: KIND_FILE,
        name,
        rel: rel.to_owned(),
        l0: 1,
        l1: lines.len(),
        text: format!("{rel}\n{}", head.join("\n")),
    });
    chunks
}

/// Merge a tiny, nearly blank last window into the previous one (T9.3 minimum
/// size).
fn merge_tiny_tail(chunks: &mut Vec<Chunk>, rel: &str, lines: &[&str]) {
    if chunks.len() < 2 {
        return;
    }
    let Some(tail) = chunks.pop() else {
        return;
    };
    let tail_lines = tail.l1 - tail.l0 + 1;
    let tail_non_blank = lines[tail.l0 - 1..tail.l1]
        .iter()
        .filter(|line| !line.trim().is_empty())
        .count();
    let is_tiny = tail_non_blank < MIN_NON_BLANK && tail_lines <= OVERLAP_LINES + MIN_NON_BLANK;
    if is_tiny && let Some(head) = chunks.last_mut() {
        head.l1 = tail.l1;
        head.text = chunk_text(rel, head.l0, head.l1, lines);
    } else {
        chunks.push(tail);
    }
}

fn snap_to_blank(lines: &[&str], start: usize, mut end: usize) -> usize {
    let floor = end.saturating_sub(BLANK_SNAP_TAIL).max(start + 1);
    while end > floor && !lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    end
}

fn window_chunk(name: &str, rel: &str, lines: &[&str], start: usize, end: usize) -> Chunk {
    Chunk {
        kind: KIND_CHUNK,
        name: name.to_owned(),
        rel: rel.to_owned(),
        l0: start + 1,
        l1: end,
        text: chunk_text(rel, start + 1, end, lines),
    }
}

fn chunk_text(rel: &str, l0: usize, l1: usize, lines: &[&str]) -> String {
    let body = lines[l0 - 1..l1].join("\n");
    format!("{rel} (lines {l0}-{l1})\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::scan::LOCK_FILE_NAMES;
    use crate::index::select::select_files;

    /// `n` lines; every 20th is blank so windows end on a blank line.
    fn source_of(n: usize) -> String {
        (1..=n)
            .map(|i| {
                if i % 20 == 0 {
                    String::new()
                } else {
                    format!("line {i} content")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn windows(chunks: &[Chunk]) -> Vec<&Chunk> {
        chunks.iter().filter(|c| c.kind == KIND_CHUNK).collect()
    }

    fn write(root: &Path, rel: &str, body: &[u8]) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    }

    fn rels(files: &[Tier1File]) -> Vec<&str> {
        files.iter().map(|f| f.rel.as_str()).collect()
    }

    #[test]
    fn windows_have_overlap_and_cover_everything() {
        let chunks = chunk_file("dir/a.sh", &source_of(130));
        let windows = windows(&chunks);
        assert!(windows.len() >= 2, "got {} windows", windows.len());
        assert_eq!((windows[0].l0, windows[0].l1), (1, 60));
        for pair in windows.windows(2) {
            assert_eq!(pair[0].l1 - pair[1].l0, 9, "10 shared lines");
        }
        assert_eq!(windows.last().expect("last window").l1, 130);
        assert_eq!(chunks.last().expect("file record").kind, KIND_FILE);
    }

    #[test]
    fn blank_line_snapping_shortens_the_window() {
        let mut lines: Vec<String> = (1..=100).map(|i| format!("x{i}")).collect();
        lines[54] = String::new();
        let chunks = chunk_file("a.sh", &lines.join("\n"));
        assert_eq!(chunks[0].l1, 55);
    }

    #[test]
    fn tiny_file_yields_one_window_plus_file_vector() {
        let chunks = chunk_file("Makefile", "all:\n\techo hi\n");
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            (chunks[0].kind, chunks[0].l0, chunks[0].l1),
            (KIND_CHUNK, 1, 2)
        );
        assert_eq!(chunks[1].kind, KIND_FILE);
        assert_eq!(chunks[1].text, "Makefile\nall:\n\techo hi");
    }

    #[test]
    fn empty_file_yields_nothing() {
        assert!(chunk_file("a.sh", "").is_empty());
    }

    #[test]
    fn chunk_text_contains_path_and_range() {
        let chunks = chunk_file("scripts/run.sh", "a\nb\nc\n");
        assert_eq!(chunks[0].text, "scripts/run.sh (lines 1-3)\na\nb\nc");
        assert_eq!(chunks[0].name, "run.sh");
        assert_eq!(chunks[0].rel, "scripts/run.sh");
    }

    #[test]
    fn file_record_uses_only_the_head_of_the_file() {
        let source = (1..=100)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_file("a.sh", &source);
        let file = chunks.last().expect("file record");
        assert_eq!((file.l0, file.l1), (1, 100));
        assert!(file.text.ends_with("l40"), "{}", file.text);
        assert!(!file.text.contains("l41"));
    }

    #[test]
    fn tiny_tail_merges_into_previous_window() {
        // 50 content lines, then 12 blank ones: the second window (51..62) has
        // no content and is merged back into the first, which now ends at 62.
        let mut lines: Vec<String> = (1..=50).map(|i| format!("c{i}")).collect();
        lines.extend(std::iter::repeat_n(" ".to_owned(), 12));
        let chunks = chunk_file("a.sh", &lines.join("\n"));
        let windows = windows(&chunks);
        assert_eq!(windows.len(), 1, "{windows:?}");
        assert_eq!((windows[0].l0, windows[0].l1), (1, 62));
        assert!(windows[0].text.starts_with("a.sh (lines 1-62)\n"));
    }

    #[test]
    fn substantial_tail_stays_a_separate_window() {
        let mut lines: Vec<String> = (1..=100).map(|i| format!("c{i}")).collect();
        lines[49] = String::new(); // window 1 ends on this blank line (50)
        let chunks = chunk_file("a.sh", &lines.join("\n"));
        let windows = windows(&chunks);
        assert!(windows.len() >= 2, "{windows:?}");
        assert_eq!(windows.last().expect("tail").l1, 100);
    }

    #[test]
    fn tier1_name_rules() {
        assert!(is_tier1_name(Path::new("Makefile")));
        assert!(is_tier1_name(Path::new("mvnw")));
        assert!(is_tier1_name(Path::new("a/b/config.yml")));
        assert!(is_tier1_name(Path::new("x.env.example")));
        assert!(!is_tier1_name(Path::new("Cargo.lock")));
        assert!(!is_tier1_name(Path::new("src/lib.rs")));
        assert!(!is_tier1_name(Path::new("image.png")));
    }

    #[test]
    fn tier1_lockfiles_subset_of_scan_lockfiles() {
        for lock in LOCKFILES {
            assert!(
                LOCK_FILE_NAMES.contains(lock),
                "{lock} must also be a scan lock file"
            );
        }
    }

    #[test]
    fn non_utf8_files_are_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "notes.txt", &[0xff, 0xfe, 0x00]);
        write(tmp.path(), "ok.txt", b"fine\n");
        let selection = select_files(tmp.path()).expect("select");
        let files = tier1_files(&selection, &BTreeSet::new()).expect("tier1");
        assert_eq!(rels(&files), vec!["ok.txt"]);
    }

    #[test]
    fn files_with_entities_are_not_tier1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "src/lib.rs", b"pub fn f() {}\n");
        write(tmp.path(), "tool.sh", b"echo hi\n");
        let selection = select_files(tmp.path()).expect("select");
        let covered: BTreeSet<String> = ["src/lib.rs".to_owned()].into();
        let files = tier1_files(&selection, &covered).expect("tier1");
        assert_eq!(
            rels(&files),
            vec!["tool.sh"],
            ".rs is never tier-1; .sh always is"
        );
    }

    #[test]
    fn parsed_file_without_entities_becomes_tier1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "empty.yml", b"key: value\n");
        let selection = Selection {
            parse: vec![tmp.path().join("empty.yml")],
            parse_rel: vec!["empty.yml".to_owned()],
            tier1_candidates: Vec::new(),
        };
        let files = tier1_files(&selection, &BTreeSet::new()).expect("tier1");
        assert_eq!(rels(&files), vec!["empty.yml"]);
        assert_eq!(files[0].source, "key: value\n");

        let covered: BTreeSet<String> = ["empty.yml".to_owned()].into();
        assert!(tier1_files(&selection, &covered).expect("tier1").is_empty());
    }

    #[test]
    fn parsed_source_without_entities_is_not_tier1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "src/empty.rs", b"// nothing here\n");
        let selection = select_files(tmp.path()).expect("select");
        assert_eq!(selection.parse_rel, vec!["src/empty.rs"]);
        assert!(
            tier1_files(&selection, &BTreeSet::new())
                .expect("tier1")
                .is_empty()
        );
    }

    #[test]
    fn index_dir_is_never_chunked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), ".knot-lite/x.json", b"{}\n");
        write(tmp.path(), "config.json", b"{}\n");
        let selection = select_files(tmp.path()).expect("select");
        let files = tier1_files(&selection, &BTreeSet::new()).expect("tier1");
        assert_eq!(rels(&files), vec!["config.json"]);
    }

    #[test]
    fn tier1_files_are_sorted_by_rel() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for rel in ["z.sh", "a/b.sh", "a.sh", "m/Makefile"] {
            write(tmp.path(), rel, b"echo\n");
        }
        let selection = select_files(tmp.path()).expect("select");
        let files = tier1_files(&selection, &BTreeSet::new()).expect("tier1");
        assert_eq!(rels(&files), vec!["a.sh", "a/b.sh", "m/Makefile", "z.sh"]);
    }
}
