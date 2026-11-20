//! Changed-file detection per tool call (P6-D3).
//!
//! A file is a **candidate** when it is missing from disk, its size differs
//! from the inventory, or its mtime is at or after the index start minus 2 s
//! (coarse file-system clocks).  A candidate is **changed** when missing or
//! its content hash differs from the inventory.  Computed lazily, once per
//! call, never cached across calls.
//!
//! The coarse filters miss a same-size edit whose mtime was set back (`cp -p`,
//! `rsync -t`, `tar x`, `touch -d`), so [`EditedFiles::is_target_changed`]
//! hashes the few target files of a call whatever their size and mtime.  A
//! target file it finds changed joins the changed set for the rest of the
//! call: it is synced with the language server, its references are mapped
//! through a fresh parse, and it is counted in [`EditStats::changed`].

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::index::inventory::file_sha16;
use crate::index::load::LoadedIndex;
use crate::lsp::uri::CanonicalRoot;

/// Files whose language has a language-server mapping and whose content
/// differs from the last index.  Lazily computed, once per tool call.
pub struct EditedFiles {
    repo_root: PathBuf,
    /// Inventory entries of LSP-language files: `rel -> (sha16, bytes)`.
    universe: HashMap<String, (String, u64)>,
    /// Index start time (P6-D3 boundary); `None` for indexes built before
    /// 0.6.0 → every candidate is hashed.
    index_start: Option<SystemTime>,
    /// Lazily computed set of files the coarse pass found changed (or deleted).
    changed: OnceLock<BTreeSet<String>>,
    /// Timing statistics of the coarse pass.
    stats: OnceLock<EditStats>,
    /// Exact (always hashed) answers of [`Self::is_target_changed`], per file.
    exact: Mutex<HashMap<String, Examined>>,
}

/// Cost of one changed-set computation.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditStats {
    /// Files examined (the LSP-language universe).
    pub universe: usize,
    /// Files whose hash was computed (the candidates and the target files).
    pub hashed: usize,
    /// Files that turned out to be changed (or deleted), by either pass.
    pub changed: usize,
    /// Milliseconds the coarse pass took.
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
                (rel.clone(), (sha.clone(), bytes))
            })
            .collect();
        Self {
            repo_root: repo_root.to_path_buf(),
            universe,
            index_start: parse_rfc3339(&index.manifest.started_at),
            changed: OnceLock::new(),
            stats: OnceLock::new(),
            exact: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `rel` changed (or was deleted) since the index: found by the
    /// coarse size and mtime filters, or by an earlier
    /// [`Self::is_target_changed`] of this call.
    pub fn is_changed(&self, rel: &str) -> bool {
        self.changed_set().contains(rel)
            || self.exact_lock().get(rel).is_some_and(Examined::is_changed)
    }

    /// Whether the target file `rel` changed (or was deleted) since the
    /// index, hashing it even when the size and mtime filters skip it, as
    /// `status` does.  Cached per file for the call; a changed file joins
    /// the changed set; `false` for files outside the language-server
    /// universe.
    pub fn is_target_changed(&self, rel: &str) -> bool {
        let Some((inv_sha, _)) = self.universe.get(rel) else {
            return false;
        };
        if self.changed.get().is_some_and(|set| set.contains(rel)) {
            return true;
        }
        let mut exact = self.exact_lock();
        if let Some(examined) = exact.get(rel) {
            return examined.is_changed();
        }
        let root = CanonicalRoot::new(&self.repo_root);
        let examined = self.examine(&root, rel, inv_sha, None);
        let changed = examined.is_changed();
        exact.insert(rel.to_owned(), examined);
        changed
    }

    /// Changed files of `lang`, sorted.
    pub fn changed(&self, lang: LspLang) -> Vec<String> {
        self.all_changed()
            .into_iter()
            .filter(|rel| lsp_lang_for_label(label_for(Path::new(rel))) == Some(lang))
            .collect()
    }

    /// All changed files, sorted.
    pub fn all_changed(&self) -> BTreeSet<String> {
        let mut all = self.changed_set().clone();
        all.extend(
            self.exact_lock()
                .iter()
                .filter(|(_, examined)| examined.is_changed())
                .map(|(rel, _)| rel.clone()),
        );
        all
    }

    /// Whether any file changed (cheaper than `!all_changed().is_empty()`).
    pub fn has_changes(&self) -> bool {
        !self.changed_set().is_empty() || self.exact_lock().values().any(Examined::is_changed)
    }

    /// Cost of the changed-set pass (computed on first use), counting the
    /// target files [`Self::is_target_changed`] hashed or found changed so far.
    pub fn stats(&self) -> EditStats {
        let coarse = self.changed_set();
        let mut stats = self.stats.get().copied().unwrap_or_default();
        for (rel, examined) in self.exact_lock().iter() {
            if matches!(examined, Examined::Hashed { .. }) {
                stats.hashed += 1;
            }
            if examined.is_changed() && !coarse.contains(rel) {
                stats.changed += 1;
            }
        }
        stats
    }

    // ---- internals ----

    fn changed_set(&self) -> &BTreeSet<String> {
        self.changed.get_or_init(|| self.compute_changed())
    }

    fn exact_lock(&self) -> MutexGuard<'_, HashMap<String, Examined>> {
        self.exact
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn compute_changed(&self) -> BTreeSet<String> {
        let start = std::time::Instant::now();
        let margin = Duration::from_secs(2);
        let root = CanonicalRoot::new(&self.repo_root);
        let mut changed = BTreeSet::new();
        let mut hashed = 0usize;

        for (rel, (inv_sha, inv_bytes)) in &self.universe {
            let examined = self.examine(&root, rel, inv_sha, Some((*inv_bytes, margin)));
            if matches!(examined, Examined::Hashed { .. }) {
                hashed += 1;
            }
            if examined.is_changed() {
                changed.insert(rel.clone());
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

    /// Compare one indexed file with the inventory.  With `coarse =
    /// Some((inv_bytes, margin))` a file the size and mtime filters rule out
    /// is skipped without hashing; with `None` it is always hashed.
    fn examine(
        &self,
        root: &CanonicalRoot,
        rel: &str,
        inv_sha: &str,
        coarse: Option<(u64, Duration)>,
    ) -> Examined {
        let path = self.repo_root.join(rel);
        // Deleted files, non-regular files (e.g. a FIFO) or symlinks
        // resolving outside the repository are always changed.
        let Ok(meta) = std::fs::metadata(&path) else {
            return Examined::Gone;
        };
        if !meta.is_file() || !root.contains(&path) {
            return Examined::Gone;
        }
        if let Some((inv_bytes, margin)) = coarse
            && !self.is_candidate(&meta, inv_bytes, margin)
        {
            return Examined::Skipped;
        }
        let changed = !file_sha16(&path).is_ok_and(|(sha, _)| sha == inv_sha);
        Examined::Hashed { changed }
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

/// Outcome of comparing one indexed file with the inventory.
enum Examined {
    /// Missing, not a regular file, or resolving outside the repository.
    Gone,
    /// Ruled out by the size and mtime filters; not hashed.
    Skipped,
    /// Hashed (an unreadable file counts as changed).
    Hashed { changed: bool },
}

impl Examined {
    fn is_changed(&self) -> bool {
        match self {
            Self::Gone => true,
            Self::Skipped => false,
            Self::Hashed { changed } => *changed,
        }
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

    /// Same-size edit with an mtime set back (`cp -p`, `touch -d`): the
    /// coarse pass skips it, the target check hashes it.
    #[test]
    fn same_size_edit_with_old_mtime_is_a_changed_target() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        write_file(tmp.path(), "src/main.rs", "fn b() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        write_file(tmp.path(), "src/lib.rs", "fn z() {}");
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000);
        for rel in ["src/lib.rs", "src/main.rs"] {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(tmp.path().join(rel))
                .unwrap();
            f.set_modified(past).unwrap();
        }

        let edits = EditedFiles::new(&index, tmp.path());
        assert!(!edits.is_changed("src/lib.rs"), "coarse filters skip it");
        assert_eq!(edits.stats().hashed, 0);
        assert!(edits.is_target_changed("src/lib.rs"));
        assert!(!edits.is_target_changed("src/main.rs"));
        assert!(!edits.is_target_changed("README.md"), "not in the universe");
    }

    /// A target file found changed by the exact check joins the changed set:
    /// the request syncs it, `build_lookup` re-parses it, and `.edits`
    /// counts it.
    #[test]
    fn changed_target_joins_the_changed_set() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        write_file(tmp.path(), "src/main.rs", "fn b() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        write_file(tmp.path(), "src/lib.rs", "fn z() {}");
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000);
        for rel in ["src/lib.rs", "src/main.rs"] {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(tmp.path().join(rel))
                .unwrap();
            f.set_modified(past).unwrap();
        }

        let edits = EditedFiles::new(&index, tmp.path());
        assert!(!edits.has_changes(), "coarse filters skip it");
        assert_eq!(edits.stats().changed, 0);
        assert!(edits.is_target_changed("src/lib.rs"));
        assert!(!edits.is_target_changed("src/main.rs"));

        assert!(edits.is_changed("src/lib.rs"));
        assert!(!edits.is_changed("src/main.rs"));
        assert!(edits.has_changes());
        assert_eq!(edits.changed(LspLang::Rust), vec!["src/lib.rs".to_owned()]);
        assert!(edits.changed(LspLang::Java).is_empty());
        assert_eq!(
            edits.all_changed(),
            BTreeSet::from(["src/lib.rs".to_owned()])
        );
        let stats = edits.stats();
        assert_eq!(stats.changed, 1);
        assert_eq!(stats.hashed, 2, "both target files were hashed");
    }

    /// A file both passes found changed is counted once.
    #[test]
    fn changed_file_found_by_both_passes_is_counted_once() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        let edits = EditedFiles::new(&index, tmp.path());
        write_file(tmp.path(), "src/lib.rs", "fn a() { /* longer */ }");
        assert!(edits.is_target_changed("src/lib.rs"), "exact pass first");
        assert!(edits.is_changed("src/lib.rs"), "coarse pass: size differs");
        assert_eq!(edits.stats().changed, 1);
        assert_eq!(edits.changed(LspLang::Rust).len(), 1);
    }

    #[test]
    fn target_check_is_cached_per_call() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        let edits = EditedFiles::new(&index, tmp.path());
        assert!(!edits.is_target_changed("src/lib.rs"));
        write_file(tmp.path(), "src/lib.rs", "fn a() { /* late */ }");
        assert!(
            !edits.is_target_changed("src/lib.rs"),
            "must use the cached result"
        );
    }

    #[test]
    fn deleted_file_is_a_changed_target() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        let index = stub_index(tmp.path(), "2026-01-01T00:00:00Z");
        std::fs::remove_file(tmp.path().join("src/lib.rs")).unwrap();
        let edits = EditedFiles::new(&index, tmp.path());
        assert!(edits.is_target_changed("src/lib.rs"));
        assert!(edits.is_changed("src/lib.rs"));
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
    fn non_regular_file_is_changed_without_hashing() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "src/lib.rs", "fn a() {}");
        let index = stub_index(tmp.path(), "2099-01-01T00:00:00Z");
        std::fs::remove_file(tmp.path().join("src/lib.rs")).unwrap();
        std::fs::create_dir(tmp.path().join("src/lib.rs")).unwrap();
        let edits = EditedFiles::new(&index, tmp.path());
        assert!(edits.is_changed("src/lib.rs"));
        assert_eq!(edits.stats().hashed, 0);
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
