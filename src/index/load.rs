//! Loading the whole index into memory for queries.
//!
//! One load reads every shard and edge file, decodes the int8 vectors into a
//! row-major `f32` matrix and builds the lookup maps the query tools need.
//! Vectors are used **as decoded** (dot product = Phase 0's scoring).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

use super::edges::EdgeLine;
use super::encode::decode_i8pv;
use super::inventory::read_inventory;
use super::layout::{
    EDGES_DIR, INDEX_DIR, IndexLayout, MANIFEST_SCHEMA_V1, SHARDS_DIR, rel_of_record,
};
use super::lsp_records::{LspTargetLine, read_lsp_edge_shard, read_lsp_targets};
use super::manifest::{IndexState, Manifest, one_line, read_index_state, read_manifest};
use super::record_io::{list_record_files, read_jsonl};
use super::shard::{ShardLine, sort_lines};
use crate::capability::languages::LspLang;
use crate::error::IndexUnavailable;
use crate::graph::containers::ContainerTable;

/// A pre-computed LSP edge as loaded into memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspIncoming {
    /// Repo-relative caller file.
    pub rel: String,
    /// Container entity id (`None` for file-level caller).
    pub src: Option<String>,
    /// Relationship label (`CALLS`, `REFERENCES`, `IMPLEMENTS`, `EXTENDS`, `OVERRIDES`).
    pub relation: String,
    /// Call/reference site count.
    pub n: u32,
}

/// One shard record plus where it lives.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedRecord {
    /// Repo-relative file the record belongs to.
    pub file: String,
    /// The record itself.
    pub line: ShardLine,
    /// Row of this record in [`Matrix`], when it has a vector.
    pub row: Option<usize>,
}

/// What identifies one version of the index (cache invalidation key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStamp {
    /// Manifest `updated_at`.
    pub updated_at: String,
    /// Manifest `source_fingerprint`.
    pub source_fingerprint: String,
}

/// Row-major matrix of every decoded vector.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Matrix {
    /// Components per row.
    pub dim: usize,
    /// `rows × dim` components.
    pub data: Vec<f32>,
    /// Row → index into [`LoadedIndex::records`].
    pub record: Vec<usize>,
}

impl Matrix {
    /// Number of rows.
    pub fn rows(&self) -> usize {
        self.record.len()
    }

    /// The vector of `row`.
    pub fn row(&self, row: usize) -> Option<&[f32]> {
        if row >= self.rows() {
            return None;
        }
        let start = row.checked_mul(self.dim)?;
        self.data.get(start..start.checked_add(self.dim)?)
    }
}

/// Target id → sorted `(caller id, relationship)`.
pub type IncomingMap = HashMap<String, Vec<(String, String)>>;

/// The whole index in memory.
#[derive(Debug)]
pub struct LoadedIndex {
    /// The manifest the load was based on.
    pub manifest: Manifest,
    /// Its cache stamp.
    pub stamp: IndexStamp,
    /// Every record, sorted by `(file, sort_key)`.
    pub records: Vec<LoadedRecord>,
    /// Record id → index into `records`.
    pub by_id: HashMap<String, usize>,
    /// File → record indices (in record order).
    pub by_file: BTreeMap<String, Vec<usize>>,
    /// Entity name → record indices (entities only, not `chunk`/`file`).
    pub by_name: HashMap<String, Vec<usize>>,
    /// Who references each target.
    pub incoming: IncomingMap,
    /// Decoded vectors.
    pub matrix: Matrix,
    /// Containers of every file, for caller granularity.
    pub containers: ContainerTable,
    /// Pre-computed LSP edges per target id.
    pub lsp_incoming: HashMap<String, Vec<LspIncoming>>,
    /// Pre-computed LSP target status per target id.
    pub lsp_targets: HashMap<String, LspTargetLine>,
    /// Inventory file sha16 per repo-relative path (for stale target detection).
    pub file_sha16: HashMap<String, String>,
    /// Milliseconds the load took.
    pub load_ms: u64,
}

/// Read only the manifest stamp (cheap): the cache calls this on every query.
pub fn read_stamp(layout: &IndexLayout) -> Result<IndexStamp, IndexUnavailable> {
    read_valid_manifest(layout).map(|manifest| stamp_of(&manifest))
}

fn stamp_of(manifest: &Manifest) -> IndexStamp {
    IndexStamp {
        updated_at: manifest.updated_at.clone(),
        source_fingerprint: manifest.source_fingerprint.clone(),
    }
}

/// Map the index state to a diagnostic, then read the full manifest.
fn read_valid_manifest(layout: &IndexLayout) -> Result<Manifest, IndexUnavailable> {
    let manifest_rel = IndexLayout::manifest_rel();
    match read_index_state(layout) {
        IndexState::Absent => Err(IndexUnavailable::Absent {
            manifest: manifest_rel,
        }),
        IndexState::Incomplete => Err(IndexUnavailable::Incomplete),
        IndexState::Unsupported { schema } => Err(IndexUnavailable::Unsupported {
            schema,
            expected: MANIFEST_SCHEMA_V1.to_owned(),
        }),
        IndexState::Invalid { error } => Err(IndexUnavailable::Invalid {
            manifest: manifest_rel,
            error,
        }),
        IndexState::Present { .. } => {
            read_manifest(&layout.manifest).map_err(|error| IndexUnavailable::Invalid {
                manifest: manifest_rel,
                error: one_line(&format!("{error:#}")),
            })
        }
    }
}

/// Load the index at `layout` into memory.
pub fn load_index(layout: &IndexLayout) -> Result<LoadedIndex, IndexUnavailable> {
    let started = Instant::now();
    let manifest = read_valid_manifest(layout)?;
    let dim = usize::try_from(manifest.embedding.dim).unwrap_or(usize::MAX);
    let (records, matrix) = load_records(layout, dim)?;
    let loaded = Loaded {
        records,
        matrix,
        edges: load_edges(layout)?,
        lsp_incoming: load_lsp_incoming(layout)?,
        lsp_targets: load_lsp_targets(layout)?,
        file_sha16: load_file_sha16(layout),
    };
    Ok(assemble(manifest, loaded, started))
}

fn load_lsp_incoming(
    layout: &IndexLayout,
) -> Result<HashMap<String, Vec<LspIncoming>>, IndexUnavailable> {
    let mut map: HashMap<String, Vec<LspIncoming>> = HashMap::new();
    let lsp_edges_dir = layout.lsp_edges();
    if !lsp_edges_dir.exists() {
        return Ok(map);
    }
    for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
        let lang_dir = layout.lsp_edges_lang_dir(lang);
        if !lang_dir.exists() {
            continue;
        }
        let record_files = match list_record_files(&lang_dir) {
            Ok(files) => files,
            Err(error) => {
                return Err(IndexUnavailable::Corrupt {
                    path: format!("{INDEX_DIR}/lsp-edges/{}", lang.dir_name()),
                    error: one_line(&format!("{error:#}")),
                });
            }
        };
        for path in record_files {
            let Some(caller_rel) = rel_of_record(&lang_dir, &path) else {
                return Err(corrupt(layout, &path, "not an lsp edge record file name"));
            };
            let shard = read_lsp_edge_shard(&path)
                .map_err(|error| corrupt(layout, &path, &format!("{error:#}")))?;
            for edge in shard {
                map.entry(edge.dst).or_default().push(LspIncoming {
                    rel: caller_rel.clone(),
                    src: edge.src,
                    relation: edge.rel,
                    n: edge.n,
                });
            }
        }
    }
    Ok(map)
}

fn load_lsp_targets(
    layout: &IndexLayout,
) -> Result<HashMap<String, LspTargetLine>, IndexUnavailable> {
    let mut map = HashMap::new();
    let lsp_targets_dir = layout.lsp_targets();
    if !lsp_targets_dir.exists() {
        return Ok(map);
    }
    for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
        let path = layout.lsp_targets_for(lang);
        if !path.exists() {
            continue;
        }
        let lines = read_lsp_targets(&path)
            .map_err(|error| corrupt(layout, &path, &format!("{error:#}")))?;
        for line in lines {
            map.insert(line.id.clone(), line);
        }
    }
    Ok(map)
}

fn load_file_sha16(layout: &IndexLayout) -> HashMap<String, String> {
    let Ok(inv) = read_inventory(&layout.inventory) else {
        return HashMap::new();
    };
    inv.entries.into_iter().map(|e| (e.rel, e.sha)).collect()
}

/// Everything read from disk, before the lookup maps are derived.
#[derive(Default)]
struct Loaded {
    records: Vec<LoadedRecord>,
    matrix: Matrix,
    edges: Vec<EdgeLine>,
    lsp_incoming: HashMap<String, Vec<LspIncoming>>,
    lsp_targets: HashMap<String, LspTargetLine>,
    file_sha16: HashMap<String, String>,
}

/// Derive the lookup maps, the incoming-edge map and the containers from what
/// was read (shared by the loader and the test builder).
fn assemble(manifest: Manifest, loaded: Loaded, started: Instant) -> LoadedIndex {
    let Loaded {
        records,
        matrix,
        edges,
        lsp_incoming,
        lsp_targets,
        file_sha16,
    } = loaded;
    let mut by_id = HashMap::with_capacity(records.len());
    let mut by_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, record) in records.iter().enumerate() {
        by_id.entry(record.line.id.clone()).or_insert(index);
        by_file.entry(record.file.clone()).or_default().push(index);
        if !record.line.is_tier1() {
            by_name
                .entry(record.line.name.clone())
                .or_default()
                .push(index);
        }
    }
    let incoming = incoming_map(edges, &by_id);
    let containers = ContainerTable::from_records(
        records
            .iter()
            .enumerate()
            .map(|(index, record)| (index, record.file.as_str(), &record.line)),
    );
    LoadedIndex {
        stamp: stamp_of(&manifest),
        manifest,
        records,
        by_id,
        by_file,
        by_name,
        incoming,
        matrix,
        containers,
        lsp_incoming,
        lsp_targets,
        file_sha16,
        load_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

/// Read every shard (sorted by file) and decode its vectors into the matrix.
fn load_records(
    layout: &IndexLayout,
    dim: usize,
) -> Result<(Vec<LoadedRecord>, Matrix), IndexUnavailable> {
    let mut shards: Vec<(String, Vec<ShardLine>)> = Vec::new();
    for path in list_files(&layout.shards, SHARDS_DIR)? {
        let Some(file) = rel_of_record(&layout.shards, &path) else {
            return Err(corrupt(layout, &path, "not a record file name"));
        };
        let mut lines: Vec<ShardLine> =
            read_jsonl(&path).map_err(|error| corrupt(layout, &path, &format!("{error:#}")))?;
        sort_lines(&mut lines);
        shards.push((file, lines));
    }
    shards.sort_by(|a, b| a.0.cmp(&b.0));

    let mut records = Vec::new();
    let mut matrix = Matrix {
        dim,
        ..Matrix::default()
    };
    for (file, lines) in shards {
        for line in lines {
            let row = decode_into(&mut matrix, &line, records.len(), dim)
                .map_err(|error| corrupt_file(SHARDS_DIR, &file, &error))?;
            records.push(LoadedRecord {
                file: file.clone(),
                line,
                row,
            });
        }
    }
    Ok((records, matrix))
}

/// Decode the vector of `line` (if any) as the next matrix row.
fn decode_into(
    matrix: &mut Matrix,
    line: &ShardLine,
    record_index: usize,
    dim: usize,
) -> Result<Option<usize>, String> {
    let (Some(q), Some(s)) = (line.q.as_deref(), line.s) else {
        return Ok(None);
    };
    let vector = decode_i8pv(q, s, dim).map_err(|error| format!("record {}: {error}", line.id))?;
    let row = matrix.record.len();
    matrix.data.extend(vector);
    matrix.record.push(record_index);
    Ok(Some(row))
}

/// Read every edge shard.
fn load_edges(layout: &IndexLayout) -> Result<Vec<EdgeLine>, IndexUnavailable> {
    let mut edges = Vec::new();
    for path in list_files(&layout.edges, EDGES_DIR)? {
        let shard: Vec<EdgeLine> =
            read_jsonl(&path).map_err(|error| corrupt(layout, &path, &format!("{error:#}")))?;
        edges.extend(shard);
    }
    Ok(edges)
}

/// `dst → sorted (src, rel)`. Edges with an unknown endpoint are ignored (and
/// counted in a debug log).
fn incoming_map(edges: Vec<EdgeLine>, by_id: &HashMap<String, usize>) -> IncomingMap {
    let mut incoming = IncomingMap::new();
    let mut ignored = 0usize;
    for edge in edges {
        if by_id.contains_key(&edge.src) && by_id.contains_key(&edge.dst) {
            incoming
                .entry(edge.dst)
                .or_default()
                .push((edge.src, edge.rel));
        } else {
            ignored += 1;
        }
    }
    for callers in incoming.values_mut() {
        callers.sort();
        callers.dedup();
    }
    if ignored > 0 {
        tracing::debug!(ignored, "ignored edges with an unknown endpoint");
    }
    incoming
}

fn list_files(base: &Path, dir_name: &str) -> Result<Vec<std::path::PathBuf>, IndexUnavailable> {
    list_record_files(base).map_err(|error| IndexUnavailable::Corrupt {
        path: format!("{INDEX_DIR}/{dir_name}"),
        error: one_line(&format!("{error:#}")),
    })
}

/// `Corrupt` for `path`, reported repo-relative with `/` separators.
fn corrupt(layout: &IndexLayout, path: &Path, error: &str) -> IndexUnavailable {
    let relative = path.strip_prefix(&layout.root).unwrap_or(path);
    IndexUnavailable::Corrupt {
        path: relative.to_string_lossy().replace('\\', "/"),
        error: one_line(error),
    }
}

/// `Corrupt` for the shard/edge file of repo-relative `file`.
fn corrupt_file(dir_name: &str, file: &str, error: &str) -> IndexUnavailable {
    IndexUnavailable::Corrupt {
        path: format!("{INDEX_DIR}/{dir_name}/{file}.jsonl"),
        error: one_line(error),
    }
}

/// Builders for hand-made indexes in tests of the query tools.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::index::manifest::test_support::sample_manifest;

    /// An entity record with the given identity and a distinct hash.
    pub(crate) fn entity(
        id: &str,
        kind: &str,
        name: &str,
        fqn: &str,
        (l0, l1): (usize, usize),
    ) -> ShardLine {
        ShardLine {
            id: id.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            fqn: fqn.to_owned(),
            lang: "rust".to_owned(),
            l0,
            l1,
            col: None,
            sig: Some("()".to_owned()),
            h: id.to_owned(),
            q: None,
            s: None,
        }
    }

    /// An index assembled from `(file, record)` pairs and `(src, dst, rel)`
    /// edges, without touching the disk (no vectors).
    pub(crate) fn synthetic_index(
        records: Vec<(&str, ShardLine)>,
        edges: &[(&str, &str, &str)],
    ) -> LoadedIndex {
        let mut records: Vec<LoadedRecord> = records
            .into_iter()
            .map(|(file, line)| LoadedRecord {
                file: file.to_owned(),
                line,
                row: None,
            })
            .collect();
        records.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then_with(|| a.line.sort_key().cmp(&b.line.sort_key()))
        });
        let edges = edges
            .iter()
            .map(|(src, dst, rel)| EdgeLine {
                src: (*src).to_owned(),
                dst: (*dst).to_owned(),
                rel: (*rel).to_owned(),
            })
            .collect();
        let loaded = Loaded {
            records,
            edges,
            ..Loaded::default()
        };
        assemble(sample_manifest(), loaded, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build::test_support::{DIM, fixture, run};
    use crate::index::record_io::write_jsonl_if_changed;

    fn loaded() -> (tempfile::TempDir, IndexLayout, LoadedIndex) {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let layout = IndexLayout::for_repo(tmp.path());
        let index = load_index(&layout).expect("load");
        (tmp, layout, index)
    }

    fn id_of(index: &LoadedIndex, file: &str, name: &str) -> String {
        index
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&i| &index.records[i])
            .find(|record| record.file == file)
            .unwrap_or_else(|| panic!("{file}::{name} not indexed"))
            .line
            .id
            .clone()
    }

    #[test]
    fn load_fixture_records_are_sorted_and_indexed() {
        let (_tmp, _layout, index) = loaded();
        let counts = &index.manifest.counts;
        assert_eq!(index.records.len() as u64, counts.records);
        assert_eq!(index.by_id.len(), index.records.len());

        // Sorted by (file, sort_key).
        let keys: Vec<_> = index
            .records
            .iter()
            .map(|record| (record.file.as_str(), record.line.sort_key()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert_eq!(index.by_file.len() as u64, counts.shards);
        assert_eq!(
            index.by_file.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "Cargo.toml",
                "Makefile",
                "docs/guide.md",
                "scripts/run.sh",
                "src/lib.rs",
                "src/util.rs",
                "web/app.ts"
            ]
        );
        assert_eq!(
            index.stamp.source_fingerprint,
            index.manifest.source_fingerprint
        );
    }

    #[test]
    fn load_fixture_names_exclude_tier1_records() {
        let (_tmp, _layout, index) = loaded();
        assert!(index.by_name.contains_key("target"));
        assert!(!index.by_name.contains_key("run.sh"));
        assert!(!index.by_name.contains_key("Makefile"));
    }

    #[test]
    fn load_fixture_incoming_edges_and_containers() {
        let (_tmp, _layout, index) = loaded();
        let target = id_of(&index, "src/lib.rs", "target");
        let callers = &index.incoming[&target];
        let caller_ids: Vec<&str> = callers.iter().map(|(src, _)| src.as_str()).collect();
        for (file, name) in [
            ("src/lib.rs", "caller"),
            ("src/util.rs", "helper"),
            ("src/util.rs", "run"),
        ] {
            assert!(
                caller_ids.contains(&id_of(&index, file, name).as_str()),
                "{name}"
            );
        }
        assert!(callers.iter().all(|(_, rel)| rel == "CALLS"));
        let mut sorted_callers = callers.clone();
        sorted_callers.sort();
        assert_eq!(callers, &sorted_callers);

        let target_record = &index.records[index.by_id[&target]];
        let (key, idx) = index
            .containers
            .container_at("src/lib.rs", target_record.line.l0);
        assert_eq!(key.name, "target");
        assert_eq!(idx, Some(index.by_id[&target]));
    }

    #[test]
    fn load_fixture_matrix_has_one_row_per_vector() {
        let (_tmp, _layout, index) = loaded();
        let with_vector = index.records.iter().filter(|r| r.line.has_vector()).count();
        assert_eq!(index.matrix.rows(), with_vector);
        assert_eq!(index.matrix.dim, DIM);
        assert_eq!(index.matrix.data.len(), with_vector * DIM);
        for (i, record) in index.records.iter().enumerate() {
            match record.row {
                Some(row) => {
                    assert_eq!(index.matrix.record[row], i);
                    assert_eq!(index.matrix.row(row).map(<[f32]>::len), Some(DIM));
                }
                None => assert!(!record.line.has_vector()),
            }
        }
        assert!(index.matrix.row(index.matrix.rows()).is_none());
    }

    #[test]
    fn load_absent_incomplete_unsupported_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        let unavailable = |layout: &IndexLayout| load_index(layout).expect_err("must fail");

        assert_eq!(
            unavailable(&layout),
            IndexUnavailable::Absent {
                manifest: ".knot-lite/manifest.toml".to_owned()
            }
        );

        std::fs::create_dir_all(&layout.dir).expect("mkdir");
        std::fs::write(&layout.previous_manifest, "x = 1\n").expect("prev");
        assert_eq!(unavailable(&layout), IndexUnavailable::Incomplete);
        std::fs::remove_file(&layout.previous_manifest).expect("remove prev");

        std::fs::write(
            &layout.manifest,
            "schema = \"knot-lite/manifest/v2\"\nknot_lite_version = \"9\"\n",
        )
        .expect("v2");
        assert_eq!(
            unavailable(&layout),
            IndexUnavailable::Unsupported {
                schema: "knot-lite/manifest/v2".to_owned(),
                expected: "knot-lite/manifest/v1".to_owned(),
            }
        );

        std::fs::write(&layout.manifest, "not = = toml\n").expect("garbage");
        assert!(matches!(
            unavailable(&layout),
            IndexUnavailable::Invalid { ref manifest, ref error }
                if manifest == ".knot-lite/manifest.toml" && !error.contains('\n')
        ));
    }

    #[test]
    fn header_only_manifest_is_invalid_not_a_panic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        std::fs::create_dir_all(&layout.dir).expect("mkdir");
        std::fs::write(
            &layout.manifest,
            "schema = \"knot-lite/manifest/v1\"\nknot_lite_version = \"0.1.0\"\n",
        )
        .expect("write");
        match load_index(&layout) {
            Err(IndexUnavailable::Invalid { error, .. }) => assert!(!error.is_empty()),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn load_corrupt_shard_is_reported_with_path() {
        let (_tmp, layout, _) = loaded();
        std::fs::write(layout.shard_for("src/util.rs").expect("path"), "garbage\n")
            .expect("corrupt");
        match load_index(&layout) {
            Err(IndexUnavailable::Corrupt { path, error }) => {
                assert_eq!(path, ".knot-lite/shards/src/util.rs.jsonl");
                assert!(error.contains("util.rs.jsonl:1"), "{error}");
                assert!(!error.contains('\n'));
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn load_corrupt_edge_shard_is_reported_with_path() {
        let (_tmp, layout, _) = loaded();
        std::fs::write(
            layout.edges_for("src/lib.rs").expect("path"),
            "{\"src\":1}\n",
        )
        .expect("corrupt");
        match load_index(&layout) {
            Err(IndexUnavailable::Corrupt { path, .. }) => {
                assert_eq!(path, ".knot-lite/edges/src/lib.rs.jsonl");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn undecodable_vector_is_corrupt() {
        let (_tmp, layout, index) = loaded();
        let path = layout.shard_for("src/lib.rs").expect("path");
        let mut lines: Vec<ShardLine> = read_jsonl(&path).expect("read");
        let victim = lines.iter_mut().find(|l| l.has_vector()).expect("a vector");
        victim.q = Some("not base64!!".to_owned());
        let victim_id = victim.id.clone();
        write_jsonl_if_changed(&path, &lines).expect("write");

        match load_index(&layout) {
            Err(IndexUnavailable::Corrupt { path, error }) => {
                assert_eq!(path, ".knot-lite/shards/src/lib.rs.jsonl");
                assert!(error.contains(&victim_id), "{error}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
        drop(index);
    }

    #[test]
    fn edges_with_unknown_endpoints_are_ignored() {
        let (_tmp, layout, index) = loaded();
        let before: usize = index.incoming.values().map(Vec::len).sum();
        let target = id_of(&index, "src/lib.rs", "target");
        let path = layout.edges_for("src/lib.rs").expect("path");
        let mut edges: Vec<EdgeLine> = read_jsonl(&path).expect("read");
        edges.push(EdgeLine {
            src: "ghost".to_owned(),
            dst: target.clone(),
            rel: "CALLS".to_owned(),
        });
        edges.push(EdgeLine {
            src: target,
            dst: "ghost".to_owned(),
            rel: "CALLS".to_owned(),
        });
        write_jsonl_if_changed(&path, &edges).expect("write");

        let reloaded = load_index(&layout).expect("load");
        assert_eq!(
            reloaded.incoming.values().map(Vec::len).sum::<usize>(),
            before
        );
        assert!(!reloaded.incoming.contains_key("ghost"));
    }

    #[test]
    fn read_stamp_matches_the_loaded_stamp_and_maps_states() {
        let (tmp, layout, index) = loaded();
        assert_eq!(read_stamp(&layout).expect("stamp"), index.stamp);
        let empty = IndexLayout::for_repo(&tmp.path().join("nowhere"));
        assert!(matches!(
            read_stamp(&empty),
            Err(IndexUnavailable::Absent { .. })
        ));
    }

    #[test]
    fn matrix_row_is_bounds_checked() {
        let matrix = Matrix {
            dim: 2,
            data: vec![1.0, 2.0, 3.0, 4.0],
            record: vec![0, 1],
        };
        assert_eq!(matrix.row(1), Some(&[3.0, 4.0][..]));
        assert_eq!(matrix.row(2), None);
        assert_eq!(matrix.row(usize::MAX), None);
        assert_eq!(matrix.rows(), 2);
        // A zero-dimension matrix has no rows either (`0..0` is a valid range).
        assert_eq!(Matrix::default().row(0), None);
    }
    /// Writes the pre-computed LSP data of the fixture index: the target
    /// `target` answered with a call from `caller` and a file-level reference.
    fn write_lsp_data(layout: &IndexLayout, index: &LoadedIndex) -> (String, String) {
        use crate::capability::languages::LspLang;
        use crate::index::lsp_records::{
            LspEdgeLine, LspTargetLine, write_lsp_edge_shards, write_lsp_targets,
        };

        let target = id_of(index, "src/lib.rs", "target");
        let caller = id_of(index, "src/lib.rs", "caller");
        let edge = |src: Option<&str>, rel: &str| LspEdgeLine {
            src: src.map(str::to_owned),
            dst: target.clone(),
            rel: rel.to_owned(),
            n: 2,
        };
        let shards = BTreeMap::from([
            (
                "src/lib.rs".to_owned(),
                vec![edge(Some(&caller), "CALLS"), edge(None, "REFERENCES")],
            ),
            ("src/util.rs".to_owned(), vec![edge(None, "REFERENCES")]),
        ]);
        write_lsp_edge_shards(layout, LspLang::Rust, &shards).expect("edges");
        let line = LspTargetLine {
            id: target.clone(),
            status: "ok".to_owned(),
            ms: 812,
            ops: crate::lsp::target::OpCounts {
                incoming: Some(1),
                references: Some(2),
                implementation: None,
            },
            server: "rust-analyzer".to_owned(),
        };
        write_lsp_targets(layout, LspLang::Rust, &[line]).expect("targets");
        (target, caller)
    }

    #[test]
    fn lsp_maps_are_empty_for_phase2_index() {
        let (_tmp, layout, index) = loaded();
        assert!(index.lsp_incoming.is_empty());
        assert!(index.lsp_targets.is_empty());
        assert!(!layout.lsp_edges().exists() && !layout.lsp_targets().exists());
    }

    #[test]
    fn lsp_maps_load_fixture() {
        let (_tmp, layout, index) = loaded();
        let (target, caller) = write_lsp_data(&layout, &index);
        let reloaded = load_index(&layout).expect("reload");

        let incoming = &reloaded.lsp_incoming[&target];
        assert_eq!(incoming.len(), 3);
        let calls = incoming
            .iter()
            .find(|edge| edge.relation == "CALLS")
            .expect("CALLS edge");
        assert_eq!(
            (calls.rel.as_str(), calls.src.as_deref(), calls.n),
            ("src/lib.rs", Some(caller.as_str()), 2)
        );
        let file_level: Vec<&str> = incoming
            .iter()
            .filter(|edge| edge.src.is_none())
            .map(|edge| edge.rel.as_str())
            .collect();
        assert_eq!(file_level, vec!["src/lib.rs", "src/util.rs"]);

        let line = &reloaded.lsp_targets[&target];
        assert_eq!((line.status.as_str(), line.ms), ("ok", 812));
        assert_eq!(line.ops.incoming, Some(1));
        assert_eq!(reloaded.lsp_targets.len(), 1);
    }

    #[test]
    fn corrupt_lsp_shard_is_reported_with_path() {
        let (_tmp, layout, index) = loaded();
        write_lsp_data(&layout, &index);
        std::fs::write(
            layout
                .lsp_edges_for(crate::capability::languages::LspLang::Rust, "src/lib.rs")
                .expect("path"),
            "{\"src\":1}\n",
        )
        .expect("corrupt");
        match load_index(&layout) {
            Err(IndexUnavailable::Corrupt { path, error }) => {
                assert_eq!(path, ".knot-lite/lsp-edges/rust/src/lib.rs.jsonl");
                assert!(!error.contains('\n'), "{error}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_lsp_targets_file_is_reported_with_path() {
        let (_tmp, layout, index) = loaded();
        write_lsp_data(&layout, &index);
        std::fs::write(
            layout.lsp_targets_for(crate::capability::languages::LspLang::Rust),
            "not json\n",
        )
        .expect("corrupt");
        match load_index(&layout) {
            Err(IndexUnavailable::Corrupt { path, .. }) => {
                assert_eq!(path, ".knot-lite/lsp-targets/rust.jsonl");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn file_sha16_is_loaded() {
        let (tmp, _layout, index) = loaded();
        let (sha, _) =
            crate::index::inventory::file_sha16(&tmp.path().join("src/lib.rs")).expect("hash");
        assert_eq!(index.file_sha16["src/lib.rs"], sha);
        assert!(index.file_sha16.contains_key("src/util.rs"));
        // The build output the index never reads is not in the inventory.
        assert!(!index.file_sha16.contains_key("target/debug/junk.rs"));
    }
}
