//! S2 measurements: sizes, load times, latency, quantisation, quality (T9.5),
//! plus determinism (T9.7) and Qdrant parity (T9.9).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::common::{Percentiles, S2_SCHEMA};
use crate::vec::encode::{self, VARIANTS, Variant};
use crate::vec::index::IndexStats;
use crate::vec::search::{self, Matrix};
use crate::vec::shard::{self, ShardLine};

/// Inputs of `vec_probe measure` (T9.1).
pub struct MeasureConfig {
    pub repo: String,
    pub root: PathBuf,
    pub out_dir: PathBuf,
    pub queries: PathBuf,
    pub level1: PathBuf,
    pub result: PathBuf,
    pub groundtruth_dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SizeBytes {
    pub raw: u64,
    pub gzip: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VariantSize {
    pub raw: u64,
    pub gzip: u64,
    pub shards: u64,
    pub bytes_per_entity: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub files_scanned: usize,
    pub files_with_entities: usize,
    pub tier1_files: usize,
    pub entities: usize,
    pub chunks: usize,
    pub file_vectors: usize,
    pub skipped_no_embed_text: usize,
    pub col_null_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimingsMs {
    pub parse: u64,
    pub prepare: u64,
    pub embed_entities: u64,
    pub embed_chunks: u64,
    pub entities_per_s: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Norms {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    /// Vectors were renormalised before quantisation (T9.2 step 7).
    pub normalized: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sizes {
    pub source: SizeBytes,
    pub variants: BTreeMap<String, VariantSize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryTimings {
    pub embed_ms: Percentiles,
    pub scan_ms: Percentiles,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuantLoss {
    pub overlap_at_10: f64,
    pub same_top1: f64,
    /// i8fx only: fraction of components clamped at the int8 bounds.
    #[serde(default)]
    pub clip_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Quantization {
    pub i8pv: QuantLoss,
    pub i8fx: QuantLoss,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HitSet {
    pub spike_f32: bool,
    pub spike_i8pv: bool,
    pub spike_i8fx: bool,
    pub knot_search: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QualityQuery {
    pub id: String,
    pub query: String,
    pub expected: Vec<String>,
    pub hit5: HitSet,
    pub hit10: HitSet,
    pub overlap10_vs_knot: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HitSummary {
    pub spike_f32: f64,
    pub spike_i8pv: f64,
    pub spike_i8fx: f64,
    pub knot_search: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Quality {
    pub queries: Vec<QualityQuery>,
    pub summary: HitSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tier1Query {
    pub id: String,
    pub hit1: bool,
    pub hit5: bool,
    pub hit5_mixed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tier1Summary {
    pub hit1: f64,
    pub hit5: f64,
    pub hit5_mixed: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tier1 {
    pub queries: Vec<Tier1Query>,
    pub summary: Tier1Summary,
    pub size_increase_pct: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitChurn {
    pub pack_delta_kib: i64,
    pub files_changed: usize,
    pub lines_changed: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitMetrics {
    pub pack_kib: BTreeMap<String, u64>,
    pub ratio_vs_source: BTreeMap<String, f64>,
    pub churn: BTreeMap<String, BTreeMap<String, GitChurn>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Determinism {
    pub rerun_identical: BTreeMap<String, bool>,
    pub batching_identical: BTreeMap<String, bool>,
    pub batching_differing_records: BTreeMap<String, usize>,
    pub batching_max_abs_diff: f64,
    pub threads_identical: BTreeMap<String, bool>,
    /// Records whose knot UUID differs between identical runs while the rest
    /// of the record matches (knot's html.rs uses `Uuid::new_v4()`).
    pub id_mismatches: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Parity {
    pub compared: usize,
    pub found: usize,
    pub max_abs_diff: f64,
    pub mean_cosine: f64,
    /// Records whose knot uuid collides with another entity of the shard
    /// (knot's uuid is `v5(repo:file:fqn:start_line)` and drops kind/name).
    pub ambiguous_ids: usize,
}

/// `results/s2_<repo>.json` (schema `knot-rag-spike/s2/v1`, T9.8).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct S2Result {
    pub schema: String,
    pub repo: String,
    pub root: String,
    pub snapshot_fingerprint: String,
    pub model: ModelInfo,
    pub counts: Counts,
    pub timings_ms: TimingsMs,
    pub norms: Norms,
    pub sizes: Sizes,
    pub load_ms: BTreeMap<String, f64>,
    pub query: QueryTimings,
    pub quantization: Quantization,
    pub quality: Quality,
    pub tier1: Tier1,
    pub git: GitMetrics,
    pub determinism: Determinism,
    pub parity_with_qdrant: Parity,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub dim: usize,
    pub symmetric: bool,
}

/// One parsed row of `queries/*.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryEntry {
    pub id: String,
    pub repo: String,
    pub text: String,
    pub expected: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct QueryFile {
    #[serde(rename = "query", default)]
    queries: Vec<QueryEntry>,
}

/// Load `queries/*.toml` entries for one repo.
pub fn load_queries(path: &Path, repo: &str) -> Result<Vec<QueryEntry>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: QueryFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed
        .queries
        .into_iter()
        .filter(|query| query.repo == repo)
        .collect())
}

/// True when `file` matches any expected pattern (exact path or `prefix/`).
pub fn matches_expected(file: &str, expected: &[String]) -> bool {
    expected.iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('/') {
            file.starts_with(prefix)
        } else {
            file == pattern
        }
    })
}

/// Median of the first `n` timed runs of `op` (T9.5 items 3–4).
pub fn median_ms(mut runs: Vec<f64>) -> f64 {
    if runs.is_empty() {
        return 0.0;
    }
    runs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    runs[runs.len() / 2]
}

/// Wall time of `op` in milliseconds.
pub fn timed_ms(op: impl FnOnce()) -> f64 {
    let started = Instant::now();
    op();
    started.elapsed().as_secs_f64() * 1000.0
}

/// gzip size of one file with flate2's default level (T9.5).
pub fn gzip_size(bytes: &[u8]) -> Result<u64> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes)?;
    Ok(encoder.finish()?.len() as u64)
}

/// Duration helper for async-free timing loops.
pub fn since_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Sleep-free duration type re-export for callers.
pub type Span = Duration;

/// Load one variant's shards into an in-memory f32 matrix (T9.5 item 3).
pub fn load_matrix(out_dir: &Path, variant: Variant, dim: usize) -> Result<Matrix> {
    let dir = out_dir.join(variant.id());
    let mut files: Vec<PathBuf> = Vec::new();
    shard::collect_jsonl(&dir, &mut files)?;
    files.sort();
    let mut matrix = Matrix {
        dim,
        ..Default::default()
    };
    for path in &files {
        let rel = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let file = rel.trim_end_matches(".jsonl").to_owned();
        for line in shard::read_shard(path)? {
            push_line(&mut matrix, &line, &file, dim);
        }
    }
    Ok(matrix)
}

/// Load the f32 matrix from the binary cache (T9.5 item 3).
pub fn load_cache_matrix(out_dir: &Path) -> Result<Matrix> {
    let (ids, data, dim) = shard::read_binary_cache(out_dir)?;
    Ok(Matrix {
        ids,
        files: Vec::new(),
        kinds: Vec::new(),
        names: Vec::new(),
        dim,
        data,
    })
}

fn push_line(matrix: &mut Matrix, line: &ShardLine, file: &str, dim: usize) {
    let Some(vector) = encode::decode(
        &encode::VectorFields {
            v: line.v.clone(),
            q: line.q.clone(),
            s: line.s,
        },
        dim,
    ) else {
        return;
    };
    matrix.ids.push(line.id.clone());
    matrix.files.push(file.to_owned());
    matrix.kinds.push(line.kind.clone());
    matrix.names.push(line.name.clone());
    matrix.data.extend_from_slice(&vector);
}

/// Rows of the entity-only search domain (T9.5 item 6).
pub fn entity_rows(matrix: &Matrix) -> Vec<usize> {
    (0..matrix.len())
        .filter(|&idx| {
            let kind = &matrix.kinds[idx];
            kind != "chunk" && kind != "file"
        })
        .collect()
}

/// Rows of the tier-1 domain (chunks + file vectors, T9.5 item 7).
pub fn tier1_rows(matrix: &Matrix) -> Vec<usize> {
    (0..matrix.len())
        .filter(|&idx| {
            let kind = &matrix.kinds[idx];
            kind == "chunk" || kind == "file"
        })
        .collect()
}

/// Top-k restricted to a row subset.
pub fn top_k_in(matrix: &Matrix, rows: &[usize], query: &[f32], k: usize) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = rows
        .iter()
        .map(|&idx| (idx, matrix.score(idx, query)))
        .collect();
    let split = k.min(scored.len()).saturating_sub(1);
    if scored.is_empty() {
        return Vec::new();
    }
    scored.select_nth_unstable_by(split, |a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut top: Vec<(usize, f32)> = scored.into_iter().take(k.min(rows.len())).collect();
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    top
}

/// Overlap@10 and same-top1 between two id lists (T9.5 item 5).
pub fn compare_top10(reference: &[String], other: &[String]) -> (f64, bool) {
    let n = reference.len().min(10);
    if n == 0 {
        return (0.0, false);
    }
    let ref10: std::collections::BTreeSet<&String> = reference.iter().take(10).collect();
    let overlap = other
        .iter()
        .take(10)
        .filter(|id| ref10.contains(*id))
        .count() as f64
        / n as f64;
    let same_top1 = reference.first() == other.first();
    (overlap, same_top1)
}

/// Row lookup by `(id, name, kind)` with an id-only fallback (T9.9).
fn find_row(matrix: &Matrix, id: &str, name: Option<&str>, kind: Option<&str>) -> Option<usize> {
    if let Some(idx) = (0..matrix.len()).find(|&idx| {
        matrix.ids[idx] == id
            && Some(matrix.kinds[idx].as_str()) == kind
            && Some(matrix.names[idx].as_str()) == name
    }) {
        return Some(idx);
    }
    matrix.ids.iter().position(|x| x == id)
}

/// Merge one section into an existing `results/s2_<repo>.json`.
pub fn merge_section<T: Serialize>(result: &Path, field: &str, value: &T) -> Result<()> {
    let mut doc: serde_json::Value = if result.exists() {
        let text = std::fs::read_to_string(result)?;
        serde_json::from_str(&text)?
    } else {
        serde_json::json!({})
    };
    doc[field] = serde_json::to_value(value)?;
    std::fs::write(result, format!("{}\n", serde_json::to_string_pretty(&doc)?))?;
    Ok(())
}

/// T9.7: compare two indexed output directories record by record.
pub fn compare_out_dirs(a: &Path, b: &Path, dim: usize) -> Result<CompareOutcome> {
    let mut outcome = CompareOutcome::default();
    for variant in VARIANTS {
        let left = load_matrix(a, *variant, dim)?;
        let right = match load_matrix(b, *variant, dim) {
            Ok(right) => right,
            Err(_) => {
                outcome.byte_identical = false;
                outcome.differing_shard_files += 1;
                continue;
            }
        };
        compare_variant(&left, &right, dim, &mut outcome);
        compare_shards(a, b, *variant, &mut outcome);
    }
    Ok(outcome)
}

/// Result of one comparison run (T9.7).
#[derive(Debug, Clone, Default)]
pub struct CompareOutcome {
    /// Every shard file byte-identical (the §20.2 "re-index is byte-identical"
    /// question). Knot's `Uuid::new_v4()` html ids break this — see
    /// `id_mismatches`.
    pub byte_identical: bool,
    pub differing_shard_files: usize,
    /// Records whose decoded vectors differ.
    pub differing_records: usize,
    pub max_abs_diff: f64,
    /// Records matching in content but with different ids (knot-side churn).
    pub id_mismatches: usize,
}

/// Records are matched in shard order and compared component-wise; ids are
/// checked separately because knot's html parser emits `Uuid::new_v4()` ids
/// (knot-side non-determinism, reported as `id_mismatches`).
fn compare_variant(left: &Matrix, right: &Matrix, dim: usize, outcome: &mut CompareOutcome) {
    if left.ids.len() != right.ids.len() {
        outcome.differing_records += left.ids.len().abs_diff(right.ids.len()).max(1);
        return;
    }
    for idx in 0..left.len() {
        if left.ids[idx] != right.ids[idx] {
            outcome.id_mismatches += 1;
        }
        let mut record_differs = false;
        for c in 0..dim {
            let delta = (left.data[idx * dim + c] - right.data[idx * dim + c]).abs();
            if delta > 0.0 {
                record_differs = true;
                outcome.max_abs_diff = outcome.max_abs_diff.max(f64::from(delta));
            }
        }
        if record_differs {
            outcome.differing_records += 1;
        }
    }
}

/// Byte-level shard comparison of one variant (the strict determinism test).
fn compare_shards(a: &Path, b: &Path, variant: Variant, outcome: &mut CompareOutcome) {
    let left_dir = a.join(variant.id());
    let right_dir = b.join(variant.id());
    let mut left_files = Vec::new();
    let mut right_files = Vec::new();
    if shard::collect_jsonl(&left_dir, &mut left_files).is_err()
        || shard::collect_jsonl(&right_dir, &mut right_files).is_err()
    {
        outcome.byte_identical = false;
        outcome.differing_shard_files += 1;
        return;
    }
    let rel = |path: &PathBuf, dir: &Path| {
        path.strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    let mut left_set: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for path in &left_files {
        if let Ok(bytes) = std::fs::read(path) {
            left_set.insert(rel(path, &left_dir), bytes);
        }
    }
    let mut right_set: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for path in &right_files {
        if let Ok(bytes) = std::fs::read(path) {
            right_set.insert(rel(path, &right_dir), bytes);
        }
    }
    let mut differing = 0usize;
    for key in left_set.keys().chain(right_set.keys()) {
        let same = left_set
            .get(key)
            .zip(right_set.get(key))
            .is_some_and(|(l, r)| l == r);
        if !same {
            differing += 1;
        }
    }
    outcome.differing_shard_files += differing;
    if differing > 0 {
        outcome.byte_identical = false;
    }
}
/// Bytes of one output variant (raw + gzip of every shard) and the source tree.
pub fn variant_size(out_dir: &Path, variant: Variant) -> Result<VariantSize> {
    let dir = out_dir.join(variant.id());
    let mut files: Vec<PathBuf> = Vec::new();
    shard::collect_jsonl(&dir, &mut files)?;
    files.sort();
    let mut raw = 0u64;
    let mut gzip = 0u64;
    let mut records = 0u64;
    for path in &files {
        let bytes = std::fs::read(path)?;
        gzip += crate::vec::measure::gzip_size(&bytes)?;
        raw += bytes.len() as u64;
        records += bytes.iter().filter(|b| **b == b'\n').count() as u64;
    }
    Ok(VariantSize {
        raw,
        gzip,
        shards: files.len() as u64,
        bytes_per_entity: if records > 0 {
            raw as f64 / records as f64
        } else {
            0.0
        },
    })
}

/// Source-tree size of the snapshot (raw + gzip), excluding scratch dirs.
pub fn source_size(root: &Path) -> Result<SizeBytes> {
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in crate::vec::chunk::scanned_candidates(root)? {
        let name = entry
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == "node_modules" || name == "target" {
            continue;
        }
        files.push(entry);
    }
    files.sort();
    let mut raw = 0u64;
    let mut gzip = 0u64;
    for path in &files {
        if let Ok(bytes) = std::fs::read(path) {
            raw += bytes.len() as u64;
            gzip += crate::vec::measure::gzip_size(&bytes)?;
        }
    }
    Ok(SizeBytes { raw, gzip })
}

/// One embedded retrieval query with its timing (T9.5 items 4–7).
struct EmbeddedQuery {
    id: String,
    text: String,
    expected: Vec<String>,
    vector: Vec<f32>,
    embed_ms: f64,
}

/// `vec_probe measure` — compute every S2 measurement (T9.5).
pub fn run_measure(cfg: &MeasureConfig) -> Result<()> {
    let dim = 384usize;
    let stats = load_index_stats(&cfg.out_dir)?;
    let mut embedder = crate::vec::index::init_embedder()?;
    let queries = load_queries(&cfg.queries, &cfg.repo)?;
    let embedded = embed_all_queries(&mut embedder, &queries)?;
    let level1 = load_queries(&cfg.level1, &cfg.repo)?;
    let level1 = embed_all_queries(&mut embedder, &level1)?;

    let f32_matrix = load_matrix(&cfg.out_dir, Variant::F32Dec, dim)?;
    let entities = entity_rows(&f32_matrix);

    let mut result = base_result(cfg, &stats);
    result.sizes = measure_sizes(cfg)?;
    result.load_ms = measure_load_times(cfg, dim)?;
    result.query = scan_timings(&f32_matrix, &entities, &embedded);
    result.quantization = quantization_loss(cfg, dim, &embedded)?;
    result.quality = retrieval_quality(cfg, &f32_matrix, &entities, &embedded)?;
    result.tier1 = tier1_quality(&f32_matrix, &level1, &result.sizes)?;

    crate::common::write_json_pretty(&cfg.result, &result)?;
    println!(
        "wrote {} ({} entities, {} chunks)",
        cfg.result.display(),
        result.counts.entities,
        result.counts.chunks
    );
    Ok(())
}

fn load_index_stats(out_dir: &Path) -> Result<IndexStats> {
    let text = std::fs::read_to_string(out_dir.join("index_stats.json"))
        .context("run `vec_probe index` first (index_stats.json missing)")?;
    Ok(serde_json::from_str(&text)?)
}

fn base_result(cfg: &MeasureConfig, stats: &IndexStats) -> S2Result {
    let col_total = stats.col_total.max(1) as f64;
    S2Result {
        schema: S2_SCHEMA.to_owned(),
        repo: cfg.repo.clone(),
        root: cfg.root.to_string_lossy().into_owned(),
        snapshot_fingerprint: read_fingerprint(&cfg.root),
        model: ModelInfo {
            name: "AllMiniLML6V2".to_owned(),
            dim: 384,
            symmetric: true,
        },
        counts: Counts {
            files_scanned: stats.files_scanned,
            files_with_entities: stats.files_with_entities,
            tier1_files: stats.tier1_files,
            entities: stats.entities,
            chunks: stats.chunks,
            file_vectors: stats.file_vectors,
            skipped_no_embed_text: stats.skipped_no_embed_text,
            col_null_rate: stats.col_nulls as f64 / col_total,
        },
        timings_ms: TimingsMs {
            parse: stats.parse_ms,
            prepare: stats.prepare_ms,
            embed_entities: stats.embed_ms,
            embed_chunks: stats.embed_chunks_ms,
            entities_per_s: if stats.embed_ms > 0 {
                stats.entities as f64 / (stats.embed_ms as f64 / 1000.0)
            } else {
                0.0
            },
        },
        norms: Norms {
            min: stats.norm_min,
            max: stats.norm_max,
            mean: stats.norm_mean,
            normalized: stats.normalized,
        },
        ..Default::default()
    }
}

fn read_fingerprint(root: &Path) -> String {
    let manifest = root.parent().map(|p| p.join("FINGERPRINTS.txt"));
    let Some(path) = manifest else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let repo = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    text.lines()
        .find_map(|line| {
            let (name, hash) = line.split_once(' ')?;
            (name == repo).then(|| hash.trim().to_owned())
        })
        .unwrap_or_default()
}

fn embed_all_queries(
    embedder: &mut knot::pipeline::embed::Embedder,
    queries: &[QueryEntry],
) -> Result<Vec<EmbeddedQuery>> {
    let mut out = Vec::new();
    for query in queries {
        let started = Instant::now();
        let vector = embedder.embed_query(&query.text)?;
        out.push(EmbeddedQuery {
            id: query.id.clone(),
            text: query.text.clone(),
            expected: query.expected.clone(),
            vector,
            embed_ms: since_ms(started),
        });
    }
    Ok(out)
}

/// T9.5 item 3: median load time of 5 full shard parses per variant.
fn measure_load_times(cfg: &MeasureConfig, dim: usize) -> Result<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    for variant in VARIANTS {
        let mut runs = Vec::new();
        for _ in 0..5 {
            runs.push(timed_ms(|| {
                let _ = load_matrix(&cfg.out_dir, *variant, dim);
            }));
        }
        out.insert(variant.id().to_owned(), median_ms(runs));
    }
    let mut cache_runs = Vec::new();
    for _ in 0..5 {
        cache_runs.push(timed_ms(|| {
            let _ = load_cache_matrix(&cfg.out_dir);
        }));
    }
    out.insert("binary_cache".to_owned(), median_ms(cache_runs));
    Ok(out)
}

/// T9.5 item 4: query embed + brute-force scan latency.
fn scan_timings(matrix: &Matrix, rows: &[usize], queries: &[EmbeddedQuery]) -> QueryTimings {
    let mut embeds = Vec::new();
    let mut scans = Vec::new();
    for query in queries {
        embeds.push(query.embed_ms);
        for _ in 0..20 {
            scans.push(timed_ms(|| {
                let _ = top_k_in(matrix, rows, &query.vector, 10);
            }));
        }
    }
    QueryTimings {
        embed_ms: percentiles_of(&embeds),
        scan_ms: percentiles_of(&scans),
    }
}

fn percentiles_of(samples: &[f64]) -> Percentiles {
    crate::common::percentiles(samples)
}

/// T9.5 item 2: raw + gzip sizes of every variant and of the source tree.
fn measure_sizes(cfg: &MeasureConfig) -> Result<Sizes> {
    let mut variants = BTreeMap::new();
    for variant in VARIANTS {
        variants.insert(
            variant.id().to_owned(),
            variant_size(&cfg.out_dir, *variant)?,
        );
    }
    Ok(Sizes {
        source: source_size(&cfg.root)?,
        variants,
    })
}

/// T9.5 item 5: top-10 agreement between f32 and the int8 variants.
fn quantization_loss(
    cfg: &MeasureConfig,
    dim: usize,
    queries: &[EmbeddedQuery],
) -> Result<Quantization> {
    let f32_matrix = load_matrix(&cfg.out_dir, Variant::F32Dec, dim)?;
    let i8pv_matrix = load_matrix(&cfg.out_dir, Variant::I8pvB64, dim)?;
    let i8fx_matrix = load_matrix(&cfg.out_dir, Variant::I8fxB64, dim)?;
    let rows = entity_rows(&f32_matrix);
    let mut i8pv = (0.0, 0.0, 0usize);
    let mut i8fx = (0.0, 0.0, 0usize);
    for query in queries {
        let base = ids_of(
            &f32_matrix,
            &top_k_in(&f32_matrix, &rows, &query.vector, 10),
        );
        let pv = ids_of(
            &i8pv_matrix,
            &top_k_in(&i8pv_matrix, &rows, &query.vector, 10),
        );
        let fx = ids_of(
            &i8fx_matrix,
            &top_k_in(&i8fx_matrix, &rows, &query.vector, 10),
        );
        let (ov, same) = compare_top10(&base, &pv);
        i8pv.0 += ov;
        i8pv.1 += f64::from(same);
        let (ov, same) = compare_top10(&base, &fx);
        i8fx.0 += ov;
        i8fx.1 += f64::from(same);
        i8pv.2 += 1;
        i8fx.2 += 1;
    }
    let n = i8pv.2.max(1) as f64;
    Ok(Quantization {
        i8pv: QuantLoss {
            overlap_at_10: i8pv.0 / n,
            same_top1: i8pv.1 / n,
            clip_rate: 0.0,
        },
        i8fx: QuantLoss {
            overlap_at_10: i8fx.0 / n,
            same_top1: i8fx.1 / n,
            clip_rate: read_clip_rate(cfg),
        },
    })
}

fn read_clip_rate(cfg: &MeasureConfig) -> f64 {
    load_index_stats(&cfg.out_dir)
        .map(|stats| stats.i8fx_clip_rate)
        .unwrap_or(0.0)
}

fn ids_of(matrix: &Matrix, results: &[(usize, f32)]) -> Vec<String> {
    results
        .iter()
        .map(|(idx, _)| matrix.ids[*idx].clone())
        .collect()
}

/// T9.5 item 6: hit@5/hit@10 of the three variants vs knot's own search.
fn retrieval_quality(
    cfg: &MeasureConfig,
    matrix: &Matrix,
    rows: &[usize],
    queries: &[EmbeddedQuery],
) -> Result<Quality> {
    let i8pv = load_matrix(&cfg.out_dir, Variant::I8pvB64, matrix.dim)?;
    let i8fx = load_matrix(&cfg.out_dir, Variant::I8fxB64, matrix.dim)?;
    let mut quality = Quality::default();
    for query in queries {
        let f32_files = files_of(matrix, &top_k_in(matrix, rows, &query.vector, 10));
        let pv_files = files_of(&i8pv, &top_k_in(&i8pv, rows, &query.vector, 10));
        let fx_files = files_of(&i8fx, &top_k_in(&i8fx, rows, &query.vector, 10));
        let knot_files = knot_search_files(cfg, &query.id);
        let f32_ids = ids_of(matrix, &top_k_in(matrix, rows, &query.vector, 10));
        let knot_ids = knot_search_ids(cfg, &query.id);
        let (overlap, _) = compare_top10(&f32_ids, &knot_ids);
        quality.queries.push(QualityQuery {
            id: query.id.clone(),
            query: query.text.clone(),
            expected: query.expected.clone(),
            hit5: HitSet {
                spike_f32: hit_at(&f32_files, &query.expected, 5),
                spike_i8pv: hit_at(&pv_files, &query.expected, 5),
                spike_i8fx: hit_at(&fx_files, &query.expected, 5),
                knot_search: hit_at(&knot_files, &query.expected, 5),
            },
            hit10: HitSet {
                spike_f32: hit_at(&f32_files, &query.expected, 10),
                spike_i8pv: hit_at(&pv_files, &query.expected, 10),
                spike_i8fx: hit_at(&fx_files, &query.expected, 10),
                knot_search: hit_at(&knot_files, &query.expected, 10),
            },
            overlap10_vs_knot: overlap,
        });
    }
    quality.summary = summarise_hits(&quality.queries);
    Ok(quality)
}

fn files_of(matrix: &Matrix, results: &[(usize, f32)]) -> Vec<String> {
    results
        .iter()
        .map(|(idx, _)| matrix.files[*idx].clone())
        .collect()
}

fn hit_at(files: &[String], expected: &[String], k: usize) -> bool {
    files
        .iter()
        .take(k)
        .any(|file| matches_expected(file, expected))
}

fn summarise_hits(queries: &[QualityQuery]) -> HitSummary {
    let n = queries.len().max(1) as f64;
    let mut sum = HitSummary::default();
    for query in queries {
        sum.spike_f32 += f64::from(query.hit5.spike_f32);
        sum.spike_i8pv += f64::from(query.hit5.spike_i8pv);
        sum.spike_i8fx += f64::from(query.hit5.spike_i8fx);
        sum.knot_search += f64::from(query.hit5.knot_search);
    }
    sum.spike_f32 /= n;
    sum.spike_i8pv /= n;
    sum.spike_i8fx /= n;
    sum.knot_search /= n;
    sum
}

/// knot's own `search` results for one query (ground truth parity, T9.5).
fn knot_search_files(cfg: &MeasureConfig, qid: &str) -> Vec<String> {
    knot_search_rows(cfg, qid)
        .into_iter()
        .filter_map(|row| {
            row.get("file_path")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .collect()
}

fn knot_search_ids(cfg: &MeasureConfig, qid: &str) -> Vec<String> {
    knot_search_rows(cfg, qid)
        .into_iter()
        .filter_map(|row| row.get("uuid").and_then(|v| v.as_str()).map(str::to_owned))
        .collect()
}

fn knot_search_rows(cfg: &MeasureConfig, qid: &str) -> Vec<serde_json::Value> {
    let path = cfg
        .groundtruth_dir
        .join(format!("search_{}_{}.json", cfg.repo, qid));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

/// T9.5 item 7: tier-1 file location quality (chunk/file domain + mixed).
fn tier1_quality(matrix: &Matrix, queries: &[EmbeddedQuery], sizes: &Sizes) -> Result<Tier1> {
    let domain = tier1_rows(matrix);
    let mut tier1 = Tier1::default();
    for query in queries {
        let top = top_k_in(matrix, &domain, &query.vector, 5);
        let files = search::top_files(matrix, &top, 5);
        let mixed = top_k_in(
            matrix,
            &(0..matrix.len()).collect::<Vec<_>>(),
            &query.vector,
            5,
        );
        let mixed_files = search::top_files(matrix, &mixed, 5);
        tier1.queries.push(Tier1Query {
            id: query.id.clone(),
            hit1: files
                .iter()
                .take(1)
                .any(|(file, _)| matches_expected(file, &query.expected)),
            hit5: files
                .iter()
                .any(|(file, _)| matches_expected(file, &query.expected)),
            hit5_mixed: mixed_files
                .iter()
                .any(|(file, _)| matches_expected(file, &query.expected)),
        });
    }
    let n = tier1.queries.len().max(1) as f64;
    for query in &tier1.queries {
        tier1.summary.hit1 += f64::from(query.hit1);
        tier1.summary.hit5 += f64::from(query.hit5);
        tier1.summary.hit5_mixed += f64::from(query.hit5_mixed);
    }
    tier1.summary.hit1 /= n;
    tier1.summary.hit5 /= n;
    tier1.summary.hit5_mixed /= n;
    let variant_bytes: u64 = sizes
        .variants
        .values()
        .map(|size| size.raw)
        .max()
        .unwrap_or(0);
    tier1.size_increase_pct = if sizes.source.raw > 0 {
        variant_bytes as f64 / sizes.source.raw as f64 * 100.0
    } else {
        0.0
    };
    Ok(tier1)
}

/// T9.7: determinism experiments (rerun, batching, threads) merged into the result.
pub fn run_determinism(cfg: &MeasureConfig, runs: &DeterminismRuns) -> Result<()> {
    let dim = 384usize;
    let mut det = Determinism::default();
    for variant in VARIANTS {
        let id = variant.id();
        let rerun = compare_variant_dirs(&runs.run1, &runs.run2, *variant, dim)?;
        det.rerun_identical
            .insert(id.to_owned(), rerun.byte_identical);
        // The uuid churn is variant-independent (same records everywhere);
        // `nov` has no vectors to compare, so its count comes from f32_dec.
        let id_mismatches = if *variant == Variant::Nov {
            det.id_mismatches
                .get(Variant::F32Dec.id())
                .copied()
                .unwrap_or(0)
        } else {
            rerun.id_mismatches
        };
        det.id_mismatches.insert(id.to_owned(), id_mismatches);
        let batch = compare_variant_dirs(&runs.run1, &runs.per_file, *variant, dim)?;
        det.batching_identical
            .insert(id.to_owned(), batch.byte_identical);
        det.batching_differing_records
            .insert(id.to_owned(), batch.differing_records);
        det.batching_max_abs_diff = det.batching_max_abs_diff.max(batch.max_abs_diff);
        if let Some(threads) = &runs.threads {
            let thread = compare_variant_dirs(&runs.run1, threads, *variant, dim)?;
            det.threads_identical
                .insert(id.to_owned(), thread.byte_identical);
        }
    }
    merge_section(&cfg.result, "determinism", &det)?;
    println!("merged determinism into {}", cfg.result.display());
    Ok(())
}

/// Compare one variant across two output directories (T9.7).
fn compare_variant_dirs(
    a: &Path,
    b: &Path,
    variant: Variant,
    dim: usize,
) -> Result<CompareOutcome> {
    let left = load_matrix(a, variant, dim)?;
    let right = load_matrix(b, variant, dim)?;
    let mut outcome = CompareOutcome {
        byte_identical: true,
        ..Default::default()
    };
    compare_variant(&left, &right, dim, &mut outcome);
    compare_shards(a, b, variant, &mut outcome);
    Ok(outcome)
}

/// Output directories of the determinism runs (T9.7).
pub struct DeterminismRuns {
    pub run1: PathBuf,
    pub run2: PathBuf,
    pub per_file: PathBuf,
    pub threads: Option<PathBuf>,
}

/// T9.9: compare spike f32 vectors with knot's Qdrant dump.
pub fn run_parity(cfg: &MeasureConfig, dump: &Path) -> Result<()> {
    let dim = 384usize;
    let matrix = load_matrix(&cfg.out_dir, Variant::F32Dec, dim)?;
    let text = std::fs::read_to_string(dump).context("reading qdrant dump")?;
    let dump: serde_json::Value = serde_json::from_str(&text)?;
    let points = dump
        .get("result")
        .and_then(|result| result.get("points"))
        .and_then(|points| points.as_array())
        .cloned()
        .unwrap_or_default();
    let mut compared = 0usize;
    let mut found = 0usize;
    let mut max_abs = 0.0f64;
    let mut cosine_sum = 0.0f64;
    let mut ambiguous = 0usize;
    // knot's uuid drops kind/name, so arrow-const pairs collide: correlate on
    // (uuid, name, kind) and fall back to the uuid alone.
    let mut occurrences: BTreeMap<String, usize> = BTreeMap::new();
    for id in &matrix.ids {
        *occurrences.entry(id.clone()).or_default() += 1;
    }
    for point in &points {
        compared += 1;
        // knot's collection stores numeric ids; the entity uuid lives in the
        // payload (with a fallback to a string id for other dumps).
        let payload = point.get("payload");
        let id_owned = payload
            .and_then(|payload| payload.get("uuid"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or_else(|| point.get("id").and_then(|v| v.as_str()).map(str::to_owned));
        let Some(id) = id_owned else {
            continue;
        };
        let Some(vector) = point.get("vector").and_then(|v| v.as_array()) else {
            continue;
        };
        let name = payload.and_then(|p| p.get("name")).and_then(|v| v.as_str());
        let kind = payload.and_then(|p| p.get("kind")).and_then(|v| v.as_str());
        if occurrences.get(&id).copied().unwrap_or(0) > 1 {
            ambiguous += 1;
        }
        let Some(idx) = find_row(&matrix, &id, name, kind) else {
            continue;
        };
        found += 1;
        let row = &matrix.data[idx * dim..(idx + 1) * dim];
        let mut dot = 0.0f64;
        let mut norm_a = 0.0f64;
        let mut norm_b = 0.0f64;
        for (c, value) in vector.iter().enumerate().take(dim) {
            let q = value.as_f64().unwrap_or(0.0) as f32;
            let delta = (row[c] - q).abs();
            max_abs = max_abs.max(f64::from(delta));
            dot += f64::from(row[c]) * f64::from(q);
            norm_a += f64::from(row[c]) * f64::from(row[c]);
            norm_b += f64::from(q) * f64::from(q);
        }
        let denom = (norm_a * norm_b).sqrt();
        if denom > 0.0 {
            cosine_sum += dot / denom;
        }
    }
    let parity = Parity {
        compared,
        found,
        max_abs_diff: max_abs,
        mean_cosine: if found > 0 {
            cosine_sum / found as f64
        } else {
            0.0
        },
        ambiguous_ids: ambiguous,
    };
    merge_section(&cfg.result, "parity_with_qdrant", &parity)?;
    println!(
        "merged parity (found {}/{}, mean cosine {:.6}) into {}",
        parity.found,
        parity.compared,
        parity.mean_cosine,
        cfg.result.display()
    );
    Ok(())
}
