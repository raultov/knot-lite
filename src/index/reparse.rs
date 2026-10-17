//! Re-parse a single file with knot's parser to get fresh records (P6-D4).
//!
//! The output is a [`FreshFile`]: the entity records of the file as the
//! parser sees them now, plus a [`ContainerTable`] built from those records.
//! Fresh records are matched back to indexed ones with [`match_record`]
//! (P6-D5) so that re-anchoring and overlay attribution can use the indexed
//! id when the declaration survived an edit.

use std::collections::HashMap;
use std::path::Path;

use anyhow::anyhow;
use knot::models::ParsedEntity;
use knot::pipeline::parser::ParseConfig;
use knot::pipeline::prepare::prepare_entities;

use crate::graph::containers::ContainerTable;
use crate::index::build::{normalise_and_sort, parse_all, records_from_entities};
use crate::index::load::LoadedIndex;
use crate::index::shard::ShardLine;
use crate::lsp::ops::shape_of;

/// Maximum files re-parsed per tool call (P6-D7).
pub const MAX_REPARSE_PER_CALL: usize = 32;

/// Maximum size of a file that may be re-parsed (256 KiB, P6-D4).
pub const MAX_REPARSE_BYTES: u64 = 256 * 1024;

/// One re-parsed file: its fresh entity records and their containers.
#[derive(Debug)]
pub struct FreshFile {
    /// Repo-relative path with `/` separators.
    pub rel: String,
    /// Fresh entity records (the vector fields are empty: no embedding at
    /// query time).
    pub records: Vec<ShardLine>,
    /// Containers built from [`Self::records`].
    pub containers: ContainerTable,
}

/// Per-call cache of re-parsed files (cap [`MAX_REPARSE_PER_CALL`]).
#[derive(Default)]
pub struct FreshFiles {
    cache: HashMap<String, Result<FreshFile, String>>,
}

impl FreshFiles {
    /// An empty per-call cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieve or re-parse `rel` under `repo_root`.
    pub fn get(
        &mut self,
        repo_root: &Path,
        rel: &str,
        cfg: &ParseConfig,
    ) -> Result<&FreshFile, &str> {
        if !self.cache.contains_key(rel) {
            if self.cache.len() >= MAX_REPARSE_PER_CALL {
                return Err("re-parse limit reached");
            }
            let res = reparse(repo_root, rel, cfg).map_err(|e| e.to_string());
            self.cache.insert(rel.to_owned(), res);
        }
        self.cache
            .get(rel)
            .expect("inserted")
            .as_ref()
            .map_err(String::as_str)
    }

    /// Pre-populate the cache with a batched re-parse of multiple files.
    pub fn populate(&mut self, repo_root: &Path, rels: &[String], cfg: &ParseConfig) {
        let uncached: Vec<String> = rels
            .iter()
            .filter(|r| !self.cache.contains_key(r.as_str()))
            .take(MAX_REPARSE_PER_CALL.saturating_sub(self.cache.len()))
            .cloned()
            .collect();
        if !uncached.is_empty() {
            let parsed = reparse_batch(repo_root, &uncached, cfg);
            self.cache.extend(parsed);
        }
    }

    /// Successfully re-parsed files in the cache.
    pub fn files(&self) -> impl Iterator<Item = (&str, &FreshFile)> {
        self.cache
            .iter()
            .filter_map(|(rel, res)| res.as_ref().ok().map(|f| (rel.as_str(), f)))
    }

    /// Number of files successfully re-parsed.
    pub fn count(&self) -> usize {
        self.cache.values().filter(|r| r.is_ok()).count()
    }
}

/// Re-parse `rels` (repo-relative) under `repo_root` in one parser call.
///
/// Batching matters: one `parse_files_stream` invocation for the whole set is
/// 3.1–3.7× cheaper than one call per file (P6.0 measurement).
pub fn reparse_batch(
    repo_root: &Path,
    rels: &[String],
    cfg: &ParseConfig,
) -> HashMap<String, Result<FreshFile, String>> {
    let mut out: HashMap<String, Result<FreshFile, String>> = HashMap::new();
    if rels.is_empty() {
        return out;
    }

    // Check size limit (P6-D4) and existence before sending to the parser.
    let mut eligible_rels = Vec::new();
    let mut eligible_paths = Vec::new();
    for rel in rels {
        let path = repo_root.join(rel);
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > MAX_REPARSE_BYTES => {
                out.insert(
                    rel.clone(),
                    Err("the changed file is too large to re-parse".to_owned()),
                );
            }
            Ok(_) => {
                eligible_rels.push(rel.clone());
                eligible_paths.push(path);
            }
            Err(err) => {
                out.insert(rel.clone(), Err(format!("cannot stat {rel}: {err}")));
            }
        }
    }

    if eligible_paths.is_empty() {
        return out;
    }

    let parsed = parse_all(&eligible_paths, cfg, 4);
    match parsed {
        Ok(entities) => {
            let mut by_file: HashMap<String, Vec<ParsedEntity>> = HashMap::new();
            for e in entities {
                let rel = e.file_path.replace('\\', "/");
                by_file.entry(rel).or_default().push(e);
            }
            for rel in eligible_rels {
                let Some(mut file_entities) = by_file.remove(&rel) else {
                    out.insert(rel.clone(), Err("no entities parsed".to_owned()));
                    continue;
                };
                normalise_and_sort(&mut file_entities);
                prepare_entities(&mut file_entities);
                let records = records_from_entities(&file_entities);
                let containers = fresh_containers(&records);
                out.insert(
                    rel.clone(),
                    Ok(FreshFile {
                        rel: rel.clone(),
                        records,
                        containers,
                    }),
                );
            }
        }
        Err(err) => {
            for rel in eligible_rels {
                out.insert(rel.clone(), Err(err.to_string()));
            }
        }
    }
    out
}

/// Re-parse a single file.
pub fn reparse(repo_root: &Path, rel: &str, cfg: &ParseConfig) -> anyhow::Result<FreshFile> {
    let map = reparse_batch(repo_root, &[rel.to_owned()], cfg);
    match map.into_values().next() {
        Some(Ok(fresh)) => Ok(fresh),
        Some(Err(msg)) => Err(anyhow!(msg)),
        None => Err(anyhow!("re-parse produced no result")),
    }
}

/// Match a fresh record to its indexed counterpart (P6-D5).
///
/// 1. Same `fqn` and `kind`, unique among unconsumed → match.
/// 2. Else same `name` and kind family, nearest `l0` (ties: earlier) → match.
/// 3. Else no match.
///
/// `consumed` marks indexed records already matched so that a record is never
/// matched twice (risk R8).
pub fn match_record(
    index: &LoadedIndex,
    rel: &str,
    fresh: &ShardLine,
    consumed: &mut [bool],
) -> Option<usize> {
    let file_indices = index.by_file.get(rel)?;

    // Pass 1: fqn + kind, unique among unconsumed records.
    let fqn_kind: Vec<usize> = file_indices
        .iter()
        .copied()
        .filter(|&i| {
            !consumed[i]
                && index.records[i].line.fqn == fresh.fqn
                && index.records[i].line.kind == fresh.kind
        })
        .collect();
    if fqn_kind.len() == 1 {
        let i = fqn_kind[0];
        consumed[i] = true;
        return Some(i);
    }

    // Pass 2: name + kind family, nearest l0.
    let fresh_family = shape_of(&fresh.kind);
    let mut best: Option<(usize, usize)> = None; // (abs_line_diff, record_idx)
    for &i in file_indices {
        if consumed[i] {
            continue;
        }
        let rec = &index.records[i];
        if rec.line.name != fresh.name {
            continue;
        }
        if shape_of(&rec.line.kind) != fresh_family {
            continue;
        }
        let diff = rec.line.l0.abs_diff(fresh.l0);
        match best {
            Some((best_diff, _)) if diff > best_diff => {}
            Some((best_diff, best_idx)) if diff == best_diff && i > best_idx => {}
            _ => best = Some((diff, i)),
        }
    }
    if let Some((_, i)) = best {
        consumed[i] = true;
        Some(i)
    } else {
        None
    }
}

/// Match every fresh record of one file, consuming indexed records as they
/// are matched.  Returns `fresh_idx → Some(indexed_record_idx)`.
pub fn match_all(index: &LoadedIndex, rel: &str, fresh: &[ShardLine]) -> Vec<Option<usize>> {
    let n = index.records.len();
    let mut consumed = vec![false; n];
    fresh
        .iter()
        .map(|f| match_record(index, rel, f, &mut consumed))
        .collect()
}

/// Build a [`ContainerTable`] from fresh records (record index = position
/// in the slice).
pub fn fresh_containers(records: &[ShardLine]) -> ContainerTable {
    ContainerTable::from_records(
        records
            .iter()
            .enumerate()
            .map(|(idx, line)| (idx, "", line)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn parse_cfg(root: &Path) -> ParseConfig {
        ParseConfig {
            repo_root: root.to_path_buf(),
            custom_queries_path: None,
            repo_name: "test".into(),
            include_config_files: false,
            repo_path: root.to_str().map(str::to_owned),
        }
    }

    #[test]
    fn one_file_matches_index_records() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "lib.rs",
            "fn alpha() {}\nstruct Beta;\nfn gamma() {}",
        );
        let cfg = parse_cfg(tmp.path());
        let fresh = reparse(tmp.path(), "lib.rs", &cfg).expect("reparse");
        assert_eq!(fresh.records.len(), 3, "alpha, Beta, gamma");
        let names: Vec<&str> = fresh.records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "Beta", "gamma"]);
    }

    #[test]
    fn fqn_kind_match_wins() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "lib.rs", "fn target() {}");
        let cfg = parse_cfg(tmp.path());
        let fresh = reparse(tmp.path(), "lib.rs", &cfg).expect("reparse");
        let rec = &fresh.records[0];

        use crate::index::load::test_support::synthetic_index;
        let idx = synthetic_index(
            vec![(
                "lib.rs",
                ShardLine {
                    id: "id-1".into(),
                    kind: rec.kind.clone(),
                    name: rec.name.clone(),
                    fqn: rec.fqn.clone(),
                    lang: "rust".into(),
                    l0: 1,
                    l1: 1,
                    col: Some(0),
                    sig: None,
                    h: "h".into(),
                    q: None,
                    s: None,
                    tc: false,
                },
            )],
            &[],
        );
        let mut consumed = vec![false; idx.records.len()];
        let m = match_record(&idx, "lib.rs", rec, &mut consumed);
        assert_eq!(m, Some(0));
    }

    #[test]
    fn name_family_nearest_line_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "lib.rs", "fn helper() {}");
        let cfg = parse_cfg(tmp.path());
        let fresh = reparse(tmp.path(), "lib.rs", &cfg).expect("reparse");
        let rec = &fresh.records[0];

        use crate::index::load::test_support::synthetic_index;
        let mk = |id: &str, fqn: &str, l0: usize| ShardLine {
            id: id.into(),
            kind: "function".into(),
            name: "helper".into(),
            fqn: fqn.into(),
            lang: "rust".into(),
            l0,
            l1: l0 + 2,
            col: Some(0),
            sig: None,
            h: "h".into(),
            q: None,
            s: None,
            tc: false,
        };
        let idx = synthetic_index(
            vec![
                ("lib.rs", mk("a", "old::helper", 50)),
                ("lib.rs", mk("b", "other::helper", 1)),
            ],
            &[],
        );
        let mut consumed = vec![false; idx.records.len()];
        let m = match_record(&idx, "lib.rs", rec, &mut consumed);
        // synthetic_index sorts by (file, sort_key) so l0=1 is index 0.
        assert_eq!(m, Some(0), "nearest l0 wins (record at line 1)");
    }

    #[test]
    fn matched_records_are_consumed() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "lib.rs", "fn dup() {}\nfn dup() {}");
        let cfg = parse_cfg(tmp.path());
        let fresh = reparse(tmp.path(), "lib.rs", &cfg).expect("reparse");
        assert_eq!(fresh.records.len(), 2);

        use crate::index::load::test_support::synthetic_index;
        let mk = |id: &str| ShardLine {
            id: id.into(),
            kind: "function".into(),
            name: "dup".into(),
            fqn: "dup".into(),
            lang: "rust".into(),
            l0: 1,
            l1: 1,
            col: Some(0),
            sig: None,
            h: "h".into(),
            q: None,
            s: None,
            tc: false,
        };
        let idx = synthetic_index(vec![("lib.rs", mk("a")), ("lib.rs", mk("b"))], &[]);
        let mapping = match_all(&idx, "lib.rs", &fresh.records);
        assert_eq!(mapping.len(), 2);
        assert!(mapping[0].is_some());
        assert!(mapping[1].is_some());
        assert_ne!(
            mapping[0], mapping[1],
            "a consumed record must not be matched again"
        );
    }

    #[test]
    fn oversized_file_is_not_reparsed() {
        let tmp = tempfile::tempdir().unwrap();
        // File larger than 256 KiB:
        let big = "fn x() {}\n".repeat(30_000);
        write_file(tmp.path(), "big.rs", &big);
        let cfg = parse_cfg(tmp.path());
        let res = reparse(tmp.path(), "big.rs", &cfg);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("too large"));
    }

    #[test]
    fn parse_error_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = parse_cfg(tmp.path());
        let result = reparse(tmp.path(), "missing.rs", &cfg);
        assert!(result.is_err());
    }

    #[test]
    fn batch_parses_all_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a.rs", "fn a() {}");
        write_file(tmp.path(), "b.rs", "fn b() {}");
        let cfg = parse_cfg(tmp.path());
        let map = reparse_batch(tmp.path(), &["a.rs".into(), "b.rs".into()], &cfg);
        assert_eq!(map.len(), 2);
        assert!(map["a.rs"].is_ok());
        assert!(map["b.rs"].is_ok());
    }

    #[test]
    fn fresh_files_cache_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = parse_cfg(tmp.path());
        let mut fresh = FreshFiles::new();
        for i in 0..MAX_REPARSE_PER_CALL {
            let rel = format!("f{i}.rs");
            write_file(tmp.path(), &rel, "fn x() {}");
            assert!(fresh.get(tmp.path(), &rel, &cfg).is_ok());
        }
        assert_eq!(fresh.count(), MAX_REPARSE_PER_CALL);
        // Exceeding the limit:
        let overflow = "overflow.rs";
        write_file(tmp.path(), overflow, "fn x() {}");
        let err = fresh.get(tmp.path(), overflow, &cfg);
        assert!(err.is_err());
        assert_eq!(err.unwrap_err(), "re-parse limit reached");
    }
}
