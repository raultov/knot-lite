//! Changed-file detection per tool call (P6-D3).
//!
//! A file is a **candidate** when it is missing from disk, its size differs
//! from the inventory, or its mtime is at or after the index start minus 2 s
//! (coarse file-system clocks).  A candidate is **changed** when missing or
//! its content hash differs from the inventory.  Computed lazily, once per
//! call, never cached across calls.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::index::inventory::file_sha16;
use crate::index::load::LoadedIndex;

/// Files whose language has a language-server mapping and whose content
/// differs from the last index.  Lazily computed, once per tool call.
pub struct EditedFiles {
    repo_root: PathBuf,
    /// Inventory entries of LSP-language files: `(rel, sha16, bytes)`.
    universe: Vec<(String, String, u64)>,
    /// Index start time (P6-D3 boundary); `None` for indexes built before
    /// 0.6.0 → every candidate is hashed.
    index_start: Option<SystemTime>,
    /// Lazily computed set of changed (or deleted) files.
    changed: OnceLock<BTreeSet<String>>,
    /// Lazily computed per-language sorted lists.
    by_lang: OnceLock<HashMap<LspLang, Vec<String>>>,
    /// Timing statistics.
    stats: OnceLock<EditStats>,
}

/// Cost of one changed-set computation.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditStats {
    /// Files examined (the LSP-language universe).
    pub universe: usize,
    /// Files whose hash was computed (the candidates).
    pub hashed: usize,
    /// Files that turned out to be changed (or deleted).
    pub changed: usize,
    /// Milliseconds the computation took.
    pub ms: u64,
}

impl EditedFiles {
    /// Build the lazy detector for the index at `repo_root`.
    pub fn new(index: &LoadedIndex, repo_root: &Path) -> Self {
        let universe = index
            .file_sha16
            .iter()
            .filter(|(rel, _)| lsp_lang_for_label(label_for(Path::new(rel))).is_some())
            .map(|(rel, sha)| {
                let bytes = index.file_bytes.get(rel).copied().unwrap_or(0);
                (rel.clone(), sha.clone(), bytes)
            })
            .collect();
        Self {
            repo_root: repo_root.to_path_buf(),
            universe,
            index_start: parse_rfc3339(&index.manifest.started_at),
            changed: OnceLock::new(),
            by_lang: OnceLock::new(),
            stats: OnceLock::new(),
        }
    }

    /// Whether `rel` changed (or was deleted) since the index.
    pub fn is_changed(&self, rel: &str) -> bool {
        self.changed_set().contains(rel)
    }

    /// Changed files of `lang`, sorted.
    pub fn changed(&self, lang: LspLang) -> &[String] {
        self.by_lang_map().get(&lang).map_or(&[], |v| v.as_slice())
    }

    /// All changed files, sorted.
    pub fn all_changed(&self) -> &BTreeSet<String> {
        self.changed_set()
    }

    /// Cost of the changed-set pass (computed on first use).
    pub fn stats(&self) -> EditStats {
        let _ = self.changed_set();
        self.stats.get().copied().unwrap_or_default()
    }

    // ---- internals ----

    fn changed_set(&self) -> &BTreeSet<String> {
        self.changed.get_or_init(|| self.compute_changed())
    }

    fn by_lang_map(&self) -> &HashMap<LspLang, Vec<String>> {
        self.by_lang.get_or_init(|| {
            let mut map: HashMap<LspLang, Vec<String>> = HashMap::new();
            for rel in self.changed_set() {
                if let Some(lang) = lsp_lang_for_label(label_for(Path::new(rel))) {
                    map.entry(lang).or_default().push(rel.clone());
                }
            }
            for v in map.values_mut() {
                v.sort();
            }
            map
        })
    }

    fn compute_changed(&self) -> BTreeSet<String> {
        let start = std::time::Instant::now();
        let margin = Duration::from_secs(2);
        let mut changed = BTreeSet::new();
        let mut hashed = 0usize;

        for (rel, inv_sha, inv_bytes) in &self.universe {
            let path = self.repo_root.join(rel);
            let meta = std::fs::metadata(&path);

            // Deleted files are always changed.
            let Ok(meta) = meta else {
                changed.insert(rel.clone());
                continue;
            };

            let is_candidate = self.is_candidate(&meta, *inv_bytes, margin);
            if !is_candidate {
                continue;
            }

            hashed += 1;
            match file_sha16(&path) {
                Ok((sha, _)) => {
                    if &sha != inv_sha {
                        changed.insert(rel.clone());
                    }
                }
                Err(_) => {
                    changed.insert(rel.clone());
                }
            }
        }

        let _ = self.stats.set(EditStats {
            universe: self.universe.len(),
            hashed,
            changed: changed.len(),
            ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        changed
    }

    fn is_candidate(&self, meta: &std::fs::Metadata, inv_bytes: u64, margin: Duration) -> bool {
        // Size check: if inventory recorded a non-zero size and it differs on disk.
        if inv_bytes > 0 && meta.len() != inv_bytes {
            return true;
        }
        // Mtime check: mtime >= index_start - 2 s.
        if let Some(start) = self.index_start {
            let boundary = start.checked_sub(margin).unwrap_or(start);
            if let Ok(mtime) = meta.modified()
                && mtime >= boundary
            {
                return true;
            }
        } else {
            // No `started_at` in the manifest (index built before 0.6.0):
            // every file is a candidate so hashing is the only filter.
            return true;
        }
        false
    }
}

/// Parse an RFC 3339 timestamp with `Z` suffix and seconds precision.
fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    if s.is_empty() {
        return None;
    }
    let s = s.strip_suffix('Z').unwrap_or(s);
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;
    // Days from civil epoch (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = u64::try_from(days * 86_400 + hour * 3600 + min * 60 + sec).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Build a `LoadedIndex` stub with actual files hashed on disk.
    fn stub_index(root: &Path, started_at: &str) -> LoadedIndex {
        use crate::index::load::test_support::synthetic_index;
        use crate::index::shard::ShardLine;

        let rec = |name: &str, fqn: &str| ShardLine {
            id: format!("id-{name}"),
            kind: "Function".into(),
            name: name.into(),
            fqn: fqn.into(),
            lang: "rust".into(),
            l0: 1,
            l1: 2,
            col: Some(0),
            sig: None,
            h: "h".into(),
            q: None,
            s: None,
            tc: false,
        };
        for rel in ["src/lib.rs", "src/main.rs"] {
            if !root.join(rel).exists() {
                write_file(root, rel, "fn x() {}");
            }
        }
        let mut index = synthetic_index(
            vec![
                ("src/lib.rs", rec("a", "a")),
                ("src/main.rs", rec("b", "b")),
            ],
            &[],
        );
        index.manifest.started_at = started_at.to_owned();
        for rel in index.by_file.keys() {
            let (sha, bytes) = file_sha16(&root.join(rel)).expect("hash");
            index.file_sha16.insert(rel.clone(), sha);
            index.file_bytes.insert(rel.clone(), bytes);
        }
        index
    }

    #[test]
    fn unchanged_tree_has_no_changes() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        write_file(tmp.path(), "src/main.rs", "fn b() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        let edits = EditedFiles::new(&index, tmp.path());
        assert!(!edits.is_changed("src/lib.rs"));
        assert!(!edits.is_changed("src/main.rs"));
        assert!(edits.all_changed().is_empty());
    }

    #[test]
    fn size_change_is_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        // Start far in the future so mtime is well before start - 2s:
        let index = stub_index(tmp.path(), "2099-01-01T00:00:00Z");
        // Append characters to change file size:
        write_file(tmp.path(), "src/lib.rs", "fn a() {} // bigger now");
        // Force file mtime to the distant past so only size triggers candidate:
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path().join("src/lib.rs"))
            .unwrap();
        f.set_modified(past).unwrap();
        drop(f);

        let edits = EditedFiles::new(&index, tmp.path());
        assert!(
            edits.is_changed("src/lib.rs"),
            "size change must trigger changed"
        );
        assert_eq!(edits.stats().hashed, 1, "size candidate was hashed");
    }

    #[test]
    fn mtime_after_start_is_hashed() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        write_file(tmp.path(), "src/main.rs", "fn b() {}");
        // Index started long ago:
        let index = stub_index(tmp.path(), "2020-01-01T00:00:00Z");
        let edits = EditedFiles::new(&index, tmp.path());
        assert!(!edits.is_changed("src/lib.rs"));
        // Both files have recent mtime >= 2020 start, so both are hashed candidates:
        assert_eq!(edits.stats().hashed, 2);
        assert_eq!(edits.stats().changed, 0);
    }

    #[test]
    fn same_bytes_new_mtime_is_not_changed() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        // Touch the file: same content, new mtime.
        let path = tmp.path().join("src/lib.rs");
        let now = SystemTime::now();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(now).unwrap();
        drop(f);
        let edits = EditedFiles::new(&index, tmp.path());
        assert!(
            !edits.is_changed("src/lib.rs"),
            "same bytes must not count as changed"
        );
    }

    #[test]
    fn deleted_file_is_changed() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        std::fs::remove_file(tmp.path().join("src/lib.rs")).unwrap();
        let edits = EditedFiles::new(&index, tmp.path());
        assert!(edits.is_changed("src/lib.rs"));
        assert!(!edits.is_changed("src/main.rs"));
    }

    #[test]
    fn only_lsp_languages_in_universe() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        write_file(tmp.path(), "README.md", "# docs");
        let mut index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        index.file_sha16.insert("README.md".into(), "abcd".into());
        index.file_bytes.insert("README.md".into(), 6);
        let edits = EditedFiles::new(&index, tmp.path());
        // README.md is not in universe (label_for gives markdown, not an LSP language).
        assert_eq!(edits.stats().universe, 2, "only rust files in universe");
        assert!(edits.changed(LspLang::Rust).is_empty());
    }

    #[test]
    fn computed_once_per_call() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        let edits = EditedFiles::new(&index, tmp.path());
        // First access computes.
        assert!(!edits.is_changed("src/lib.rs"));
        // Edit after computation: cached set is returned.
        write_file(tmp.path(), "src/lib.rs", "fn a() { /* late */ }");
        assert!(
            !edits.is_changed("src/lib.rs"),
            "must use the cached result"
        );
        assert_eq!(edits.stats().universe, 2);
    }

    #[test]
    fn parse_rfc3339_roundtrip() {
        let t = parse_rfc3339("2026-10-05T10:00:00Z").unwrap();
        assert_eq!(
            t.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_791_194_400
        );
        assert!(parse_rfc3339("").is_none());
        assert!(parse_rfc3339("garbage").is_none());
    }
}
