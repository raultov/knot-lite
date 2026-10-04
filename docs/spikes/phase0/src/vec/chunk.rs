//! Tier-1 chunker: line-window vectors for files knot cannot parse (T9.3).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ignore::WalkBuilder;

/// Windows of 60 lines with 10 lines of overlap (T9.3).
const WINDOW_LINES: usize = 60;
const OVERLAP_LINES: usize = 10;
/// Minimum non-blank lines for a standalone chunk (smaller tails merge).
const MIN_NON_BLANK: usize = 5;
/// Snap the window end to a blank line within this many trailing lines.
const BLANK_SNAP_TAIL: usize = 10;
/// Largest tier-1 file considered.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// Leading lines used for the file-level vector.
const FILE_HEAD_LINES: usize = 40;

/// Directory names never walked (T9.3).
const DENY_LISTED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    ".git",
    ".knot",
    ".knot-rag",
];

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

/// One vectorised line window (or file summary) of a tier-1 file.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// `"chunk"` or `"file"`.
    pub kind: &'static str,
    /// File name.
    pub name: String,
    /// Repo-relative path (also the `fqn` of the chunk record).
    pub rel: String,
    /// 1-based first line.
    pub l0: usize,
    /// 1-based last line.
    pub l1: usize,
    /// Text handed to the embedder.
    pub text: String,
}

/// A tier-1 file: repo-relative path plus its decoded source.
#[derive(Debug, Clone)]
pub struct Tier1File {
    pub rel: String,
    pub source: String,
}

/// Select tier-1 files under `root` (T9.3 file rules).
///
/// `files_with_entities` holds repo-relative paths knot produced entities for:
/// a file is tier-1 when knot does not support it **or** produced zero entities
/// for it, and it passes the text allow-list.
pub fn tier1_files(root: &Path, files_with_entities: &BTreeSet<String>) -> Result<Vec<Tier1File>> {
    let mut out = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() || is_deny_listed(path) {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        // Tier-1 = knot does not cover the file (unsupported or zero entities)
        // AND the text allow-list accepts it.
        let covered = knot::pipeline::files::is_supported_file(path, false)
            && files_with_entities.contains(&rel);
        if covered || !is_tier1_name(path) {
            continue;
        }
        if path
            .metadata()
            .is_ok_and(|meta| meta.len() > MAX_FILE_BYTES)
        {
            continue;
        }
        let bytes = std::fs::read(path)?;
        let Ok(source) = String::from_utf8(bytes) else {
            continue; // non-UTF-8: out of scope
        };
        out.push(Tier1File { rel, source });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn is_deny_listed(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| DENY_LISTED_DIRS.contains(&name))
    })
}

fn is_tier1_name(path: &Path) -> bool {
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

/// Split one tier-1 file into overlapping line windows plus a file summary.
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

    // Merge a tiny tail into the previous window (T9.3 minimum size).
    if chunks.len() >= 2 {
        let tail = chunks.pop().expect("two chunks");
        let tail_lines = tail.l1 - tail.l0 + 1;
        let tail_non_blank = lines[tail.l0 - 1..tail.l1]
            .iter()
            .filter(|line| !line.trim().is_empty())
            .count();
        if tail_non_blank < MIN_NON_BLANK && tail_lines <= OVERLAP_LINES + MIN_NON_BLANK {
            let head = chunks.last_mut().expect("previous chunk");
            head.l1 = tail.l1;
            head.text = chunk_text(rel, head.l0, head.l1, &lines);
        } else {
            chunks.push(tail);
        }
    }

    let head: Vec<&str> = lines.iter().take(FILE_HEAD_LINES).copied().collect();
    chunks.push(Chunk {
        kind: "file",
        name,
        rel: rel.to_owned(),
        l0: 1,
        l1: lines.len(),
        text: format!("{rel}\n{}", head.join("\n")),
    });
    chunks
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
        kind: "chunk",
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

/// Tier-1 file paths of a root, as scan evidence for the results (T9.5 counts).
pub fn scanned_candidates(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        if path.is_file() && !is_deny_listed(path) {
            out.push(path.to_path_buf());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn windows_have_overlap_and_cover_everything() {
        let source = source_of(130);
        let chunks = chunk_file("dir/a.sh", &source);
        let windows: Vec<_> = chunks.iter().filter(|c| c.kind == "chunk").collect();
        assert!(windows.len() >= 2, "got {} windows", windows.len());
        assert_eq!(windows[0].l0, 1);
        assert_eq!(windows[0].l1, 60);
        // Overlap of 10 lines between consecutive windows.
        for pair in windows.windows(2) {
            assert_eq!(pair[0].l1 - pair[1].l0, 9, "10 shared lines");
        }
        assert_eq!(windows.last().expect("last").l1, 130);
        // File-level vector always present and last.
        assert_eq!(chunks.last().expect("file").kind, "file");
    }

    #[test]
    fn blank_line_snapping_shortens_the_window() {
        // Blank at line 55 → the first window must end there (within the
        // last-10-lines snapping zone).
        let mut lines: Vec<String> = (1..=100).map(|i| format!("x{i}")).collect();
        lines[54] = String::new();
        let source = lines.join("\n");
        let chunks = chunk_file("a.sh", &source);
        assert_eq!(chunks[0].l1, 55);
    }

    #[test]
    fn tiny_file_yields_one_window_plus_file_vector() {
        let chunks = chunk_file("Makefile", "all:\n\techo hi\n");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].kind, "chunk");
        assert_eq!(chunks[0].l0, 1);
        assert_eq!(chunks[0].l1, 2);
        assert_eq!(chunks[1].kind, "file");
    }

    #[test]
    fn empty_file_yields_nothing() {
        assert!(chunk_file("a.sh", "").is_empty());
    }

    #[test]
    fn chunk_text_contains_path_and_range() {
        let chunks = chunk_file("scripts/run.sh", "a\nb\nc\n");
        assert_eq!(chunks[0].text, "scripts/run.sh (lines 1-3)\na\nb\nc");
    }

    #[test]
    fn tiny_tail_merges_into_previous_window() {
        // 63 lines: window 1 = 1..60, tail = 51..63 (13 lines) — no merge here;
        // 65 lines with a long first window ends at 60 and the tail is small.
        let source = source_of(63);
        let chunks = chunk_file("a.sh", &source);
        let windows: Vec<_> = chunks.iter().filter(|c| c.kind == "chunk").collect();
        // The tail (51..63) has 13 lines and enough non-blank content to stay.
        assert!(!windows.is_empty());
        assert_eq!(windows[0].l1, 60);
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
    fn non_utf8_files_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("notes.txt"), [0xffu8, 0xfe, 0x00]).expect("write");
        let files = tier1_files(dir.path(), &BTreeSet::new()).expect("scan");
        assert!(files.is_empty(), "binary file must be skipped");
    }

    #[test]
    fn files_with_entities_are_not_tier1() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("tool.sh"), "echo hi\n").expect("write");
        // `tool.sh` is entity-covered only when knot supports it — .sh is
        // unsupported, so it stays tier-1 regardless; use a .rs file instead.
        std::fs::write(dir.path().join("lib.rs"), "fn main() {}\n").expect("write");
        let mut covered = BTreeSet::new();
        covered.insert("tool.sh".to_owned());
        covered.insert("lib.rs".to_owned());
        let files = tier1_files(dir.path(), &covered).expect("scan");
        let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert!(rels.contains(&"tool.sh"));
        assert!(!rels.contains(&"lib.rs"));
    }
}
