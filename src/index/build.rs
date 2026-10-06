//! The index build pipeline: select → parse → prepare → tier-1 chunks → embed
//! → shards → inventory → manifest.
//!
//! One run re-parses the whole repository and writes only under
//! `<repo>/.knot-lite/` (P2-D6, P2-D13). The embedder is injected so the
//! pipeline is testable without a model (P2-D4).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, bail};
use knot::models::ParsedEntity;
use knot::pipeline::parser::{ParseConfig, parse_files_stream};
use serde::Serialize;
use uuid::Uuid;

use super::chunk::{chunk_file, tier1_files};
use super::edges::EdgeLine;
use super::encode::{encode_i8pv, normalize};
use super::inventory::{Inventory, collect_inventory, fingerprint, write_inventory};
use super::layout::{CommitMode, INDEX_DIR, IndexLayout, MANIFEST_SCHEMA_V1, apply_commit_mode};
use super::manifest::{
    IndexCounts, IndexTimings, KNOT_VERSION, Manifest, now_rfc3339, read_manifest,
    write_manifest_atomic,
};
use super::record_io::{WriteOutcome, remove_orphans, write_jsonl_if_changed};
use super::reuse::{ReuseMap, ReuseSource};
use super::select::{Selection, select_files};
use super::shard::{ShardLine, chunk_id, sort_lines, text_hash};
use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::config::Config;
use crate::embed::{EmbedBatch, EmbeddingSpec};
use crate::graph::heuristic::{ResolverMetrics, resolve_edges};
use crate::lsp::ops::{TargetShape, shape_of};
use crate::lsp::position::{is_nameless, locate_name_in_lines};

/// Schema id of the [`BuildReport`] document.
pub const INDEX_RUN_SCHEMA_V1: &str = "knot-lite/index-run/v1";

/// Passages handed to the embedder per progress step.
pub const EMBED_SLICE: usize = 1024;

/// Default number of passages per model call.
pub const DEFAULT_BATCH_SIZE: usize = 128;

/// Capacity of the parser → collector channel.
const PARSE_CHANNEL_CAPACITY: usize = 1024;

/// Knobs of one build.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Ignore reusable vectors and embed everything.
    pub force: bool,
    /// `None` keeps the previous manifest's mode, else `gitignore`.
    pub commit_mode: Option<CommitMode>,
    /// Maximum concurrent parser tasks (values below 1 are treated as 1).
    pub jobs: usize,
    /// Passages per embedder call (values below 1 are treated as 1).
    pub batch_size: usize,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            force: false,
            commit_mode: None,
            jobs: std::thread::available_parallelism().map_or(4, std::num::NonZero::get),
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

/// What a build reports while it runs (stderr progress in the CLI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// A pipeline stage starts: `select`, `parse`, `prepare`, `embed`, `write`.
    Stage {
        /// Stage name.
        name: &'static str,
    },
    /// Passages embedded so far, after each slice of [`EMBED_SLICE`] texts.
    Embedded {
        /// Passages embedded.
        done: usize,
        /// Passages to embed in this run.
        total: usize,
    },
    /// The run finished.
    Done {
        /// Wall-clock milliseconds of the whole run.
        total_ms: u64,
    },
}

/// `repo` object of the report.
#[derive(Debug, Clone, Serialize)]
pub struct ReportRepo {
    /// Repository name.
    pub name: String,
    /// Absolute repository path.
    pub path: String,
}

/// `embedding` object of the report.
#[derive(Debug, Clone, Serialize)]
pub struct ReportEmbedding {
    /// Canonical model name.
    pub model: String,
    /// Vector dimension.
    pub dim: u32,
    /// Wire encoding.
    pub encoding: String,
}

/// `vectors` object of the report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VectorStats {
    /// Vectors taken from the previous index.
    pub reused: usize,
    /// Vectors computed by the embedder in this run.
    pub embedded: usize,
    /// Records without embed text (no vector).
    pub without_text: usize,
    /// Why vectors were (not) reused; serialised as its label.
    pub reuse_source: ReuseSource,
}

/// `files` object of the report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileStats {
    /// Shard files created or rewritten.
    pub shards_written: usize,
    /// Shard files left untouched (identical bytes).
    pub shards_unchanged: usize,
    /// Orphan shard files deleted.
    pub shards_removed: usize,
    /// Edge shard files created or rewritten.
    pub edge_shards_written: usize,
    /// Edge shard files left untouched.
    pub edge_shards_unchanged: usize,
    /// Orphan edge shard files deleted.
    pub edge_shards_removed: usize,
}

/// The `knot-lite/index-run/v1` document (§9.1).
#[derive(Debug, Clone, Serialize)]
pub struct BuildReport {
    /// [`INDEX_RUN_SCHEMA_V1`].
    pub schema: &'static str,
    /// knot-lite version that ran the build.
    pub knot_lite_version: &'static str,
    /// Repository name and path.
    pub repo: ReportRepo,
    /// Name of the index directory inside the repository.
    pub index_dir: &'static str,
    /// Effective commit mode.
    pub commit_mode: CommitMode,
    /// Embedding model and encoding.
    pub embedding: ReportEmbedding,
    /// Record counts (the same numbers as the manifest).
    pub counts: IndexCounts,
    /// Where the vectors came from.
    pub vectors: VectorStats,
    /// What happened to the shard files.
    pub files: FileStats,
    /// Counters of knot's reference resolver.
    pub resolver: ResolverMetrics,
    /// Per-stage timings.
    pub timings_ms: IndexTimings,
    /// Non-fatal problems found during the run.
    pub warnings: Vec<String>,
}

/// A record waiting for its vector.
struct PendingRecord {
    /// Repo-relative file the record belongs to (decides its shard).
    file: String,
    line: ShardLine,
    /// Embed text; empty (after trimming) means "no vector".
    text: String,
}

#[derive(Default)]
struct RecordStats {
    records: usize,
    files_with_entities: usize,
    tier1_files: usize,
    entities: usize,
    entities_without_vector: usize,
    chunks: usize,
    file_vectors: usize,
    columns_located: usize,
    columns_missing: usize,
}

struct RecordSet {
    pending: Vec<PendingRecord>,
    stats: RecordStats,
    warnings: Vec<String>,
}

/// State prepared before the first shard is touched.
struct RunDir {
    layout: IndexLayout,
    previous: Option<Manifest>,
    commit_mode: CommitMode,
    warnings: Vec<String>,
}

/// Build (or refresh) the index of `cfg.repo_path`. Writes only under
/// `<repo>/.knot-lite/`.
pub fn build_index(
    cfg: &Config,
    embedder: &mut dyn EmbedBatch,
    opts: &BuildOptions,
    progress: &mut dyn FnMut(ProgressEvent),
) -> anyhow::Result<BuildReport> {
    let started = Instant::now();
    let spec = embedder.spec();
    let mut dir = prepare_dir(cfg, opts)?;
    let mut timings = IndexTimings::default();

    progress(ProgressEvent::Stage { name: "select" });
    let stage = Instant::now();
    let selection = select_files(&cfg.repo_path)?;
    let inventory = collect_inventory(&selection)?;
    timings.select = elapsed_ms(stage);

    progress(ProgressEvent::Stage { name: "parse" });
    let stage = Instant::now();
    let mut entities = parse_selection(cfg, &selection, opts.jobs)?;
    timings.parse = elapsed_ms(stage);

    progress(ProgressEvent::Stage { name: "prepare" });
    let stage = Instant::now();
    let mut records = collect_records(cfg, &selection, &mut entities)?;
    dir.warnings.append(&mut records.warnings);
    timings.prepare = elapsed_ms(stage);

    progress(ProgressEvent::Stage { name: "resolve" });
    let resolved = resolve_edges(&entities);
    timings.resolve = resolved.resolve_ms;

    progress(ProgressEvent::Stage { name: "embed" });
    let stage = Instant::now();
    let (reuse, reuse_warnings) =
        ReuseMap::load(&dir.layout, dir.previous.as_ref(), &spec, opts.force);
    dir.warnings.extend(reuse_warnings);
    apply_reuse(&mut records.pending, &reuse);
    let mut vectors = assign_vectors(&mut records.pending, embedder, &spec, opts, progress)?;
    vectors.reuse_source = reuse.source;
    timings.embed = elapsed_ms(stage);

    progress(ProgressEvent::Stage { name: "write" });
    let stage = Instant::now();
    let shards = write_shards(&dir.layout, records.pending)?;
    let entity_files: HashMap<Uuid, &str> = entities
        .iter()
        .map(|entity| (entity.uuid, entity.file_path.as_str()))
        .collect();
    let edges = write_edges(&dir.layout, &resolved.edges, &entity_files)?;
    write_inventory(&dir.layout.inventory, &inventory)?;
    timings.write = elapsed_ms(stage);
    timings.total = elapsed_ms(started);

    let manifest = build_manifest(
        cfg,
        &dir,
        ManifestParts {
            spec: &spec,
            source_fingerprint: fingerprint(&inventory),
            counts: index_counts(&selection, &inventory, &records.stats, &shards, &edges),
            timings,
        },
    );
    write_manifest_atomic(&dir.layout.manifest, &manifest)?;
    remove_previous_manifest(&dir.layout)?;
    progress(ProgressEvent::Done {
        total_ms: manifest.timings_ms.total,
    });

    Ok(assemble_report(
        cfg,
        dir,
        manifest,
        StageResults {
            spec,
            vectors,
            shards,
            edges,
            resolver: resolved.metrics,
        },
    ))
}

/// What the stages produced, beyond what the manifest already holds.
struct StageResults {
    spec: EmbeddingSpec,
    vectors: VectorStats,
    shards: ShardStats,
    edges: EdgeStats,
    resolver: ResolverMetrics,
}

fn assemble_report(
    cfg: &Config,
    dir: RunDir,
    manifest: Manifest,
    results: StageResults,
) -> BuildReport {
    BuildReport {
        schema: INDEX_RUN_SCHEMA_V1,
        knot_lite_version: env!("CARGO_PKG_VERSION"),
        repo: ReportRepo {
            name: cfg.repo_name.clone(),
            path: cfg.repo_path.to_string_lossy().into_owned(),
        },
        index_dir: INDEX_DIR,
        commit_mode: dir.commit_mode,
        embedding: ReportEmbedding {
            model: results.spec.model,
            dim: results.spec.dim,
            encoding: results.spec.encoding,
        },
        counts: manifest.counts,
        vectors: results.vectors,
        files: FileStats {
            shards_written: results.shards.written,
            shards_unchanged: results.shards.unchanged,
            shards_removed: results.shards.removed,
            edge_shards_written: results.edges.written,
            edge_shards_unchanged: results.edges.unchanged,
            edge_shards_removed: results.edges.removed,
        },
        resolver: results.resolver,
        timings_ms: manifest.timings_ms,
        warnings: dir.warnings,
    }
}

/// Create the index directory, settle the commit mode and move the current
/// manifest aside (commit-mode file first, P2-D13).
fn prepare_dir(cfg: &Config, opts: &BuildOptions) -> anyhow::Result<RunDir> {
    let layout = IndexLayout::for_repo(&cfg.repo_path);
    std::fs::create_dir_all(&layout.dir)
        .with_context(|| format!("creating {}", layout.dir.display()))?;
    let mut warnings = Vec::new();
    let previous = read_previous_manifest(&layout, &mut warnings);
    let commit_mode = opts
        .commit_mode
        .or(previous.as_ref().map(|manifest| manifest.commit_mode))
        .unwrap_or_default();
    apply_commit_mode(&layout, commit_mode)?;
    if layout.manifest.exists() {
        std::fs::rename(&layout.manifest, &layout.previous_manifest)
            .with_context(|| format!("moving aside {}", layout.manifest.display()))?;
    }
    Ok(RunDir {
        layout,
        previous,
        commit_mode,
        warnings,
    })
}

/// The manifest of the last completed run: `manifest.toml`, else the
/// `manifest.prev.toml` left by an interrupted one. Unreadable → warning.
fn read_previous_manifest(layout: &IndexLayout, warnings: &mut Vec<String>) -> Option<Manifest> {
    let candidates = [&layout.manifest, &layout.previous_manifest];
    for path in candidates.into_iter().filter(|path| path.exists()) {
        match read_manifest(path) {
            Ok(manifest) if manifest.schema == MANIFEST_SCHEMA_V1 => return Some(manifest),
            Ok(manifest) => warnings.push(format!(
                "replacing the index written with schema '{}'",
                manifest.schema
            )),
            Err(error) => warnings.push(format!("ignoring the previous manifest: {error:#}")),
        }
    }
    None
}

fn remove_previous_manifest(layout: &IndexLayout) -> anyhow::Result<()> {
    match std::fs::remove_file(&layout.previous_manifest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            Err(e).with_context(|| format!("removing {}", layout.previous_manifest.display()))
        }
    }
}

fn parse_selection(
    cfg: &Config,
    selection: &Selection,
    jobs: usize,
) -> anyhow::Result<Vec<ParsedEntity>> {
    let root = cfg
        .repo_path
        .to_str()
        .context("repository path is not valid UTF-8")?;
    let parse_cfg = ParseConfig {
        repo_root: cfg.repo_path.clone(),
        custom_queries_path: None,
        repo_name: cfg.repo_name.clone(),
        include_config_files: false,
        repo_path: Some(root.to_owned()),
    };
    parse_all(&selection.parse, &parse_cfg, jobs)
}

/// Run knot's streaming parser with a concurrent collector.
///
/// `parse_files_stream` blocks and only sends after every file is parsed; a
/// collector draining the channel concurrently avoids the deadlock that
/// `parse_files` hits above 1024 entities (Phase 0 T3).
fn parse_all(
    files: &[PathBuf],
    cfg: &ParseConfig,
    jobs: usize,
) -> anyhow::Result<Vec<ParsedEntity>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(PARSE_CHANNEL_CAPACITY);
    std::thread::scope(|scope| {
        let consumer = scope.spawn(move || {
            let mut entities = Vec::new();
            while let Some(entity) = rx.blocking_recv() {
                entities.push(entity);
            }
            entities
        });
        let producer = scope.spawn(move || parse_files_stream(files, cfg, tx, jobs.max(1), None));
        producer
            .join()
            .map_err(|_| anyhow::anyhow!("the parse producer thread panicked"))?;
        consumer
            .join()
            .map_err(|_| anyhow::anyhow!("the parse collector thread panicked"))
    })
}

/// Turn parsed entities and tier-1 files into records awaiting vectors.
fn collect_records(
    cfg: &Config,
    selection: &Selection,
    entities: &mut [ParsedEntity],
) -> anyhow::Result<RecordSet> {
    normalise_and_sort(entities);
    knot::pipeline::prepare::prepare_entities(entities);

    let mut pending = entity_records(entities);
    let columns = assign_columns(&mut pending, &cfg.repo_path);
    let entity_files: BTreeSet<String> = pending.iter().map(|r| r.file.clone()).collect();
    let mut stats = RecordStats {
        files_with_entities: entity_files.len(),
        entities: pending.len(),
        entities_without_vector: pending
            .iter()
            .filter(|record| record.text.trim().is_empty())
            .count(),
        columns_located: columns.located,
        columns_missing: columns.missing,
        ..RecordStats::default()
    };

    let tier1 = tier1_records(cfg, selection, &entity_files)?;
    stats.tier1_files = tier1.files;
    stats.chunks = tier1.chunks;
    stats.file_vectors = tier1.file_vectors;
    pending.extend(tier1.records);
    stats.records = pending.len();
    Ok(RecordSet {
        pending,
        stats,
        warnings: columns.warnings,
    })
}

/// Outcome of [`assign_columns`].
#[derive(Default)]
struct ColumnPass {
    located: usize,
    missing: usize,
    warnings: Vec<String>,
}

/// Set the UTF-16 name column of the entity records a language server can be
/// asked about, in files it serves (P3-D4). Each such file is read once; a file
/// that cannot be read leaves `col` unset and adds a warning.
///
/// Only declarations count (see [`is_locatable`]): markup attributes, impl
/// blocks, modules and the synthetic `<module>` entity are neither located nor
/// missing.
///
/// `records` must be grouped by file (they are: entities are sorted by path).
fn assign_columns(records: &mut [PendingRecord], repo_root: &Path) -> ColumnPass {
    let mut pass = ColumnPass::default();
    for group in records.chunk_by_mut(|a, b| a.file == b.file) {
        let Some(file) = group.first().map(|record| record.file.clone()) else {
            continue;
        };
        let Some(lang) = lsp_lang_for_label(label_for(Path::new(&file))) else {
            continue;
        };
        match std::fs::read_to_string(repo_root.join(&file)) {
            Ok(source) => {
                let lines: Vec<&str> = source.lines().collect();
                let (located, missing) = locate_group(group, &lines, lang);
                pass.located += located;
                pass.missing += missing;
            }
            Err(error) => {
                pass.warnings
                    .push(format!("col: cannot read {file}: {error}"));
                pass.missing += group
                    .iter()
                    .filter(|record| is_locatable(&record.line))
                    .count();
            }
        }
    }
    pass
}

/// Whether a record is a named declaration a language server knows as a symbol.
fn is_locatable(line: &ShardLine) -> bool {
    !is_nameless(&line.name) && shape_of(&line.kind) != TargetShape::None
}

/// Locate the name column of every record of one file; returns `(located, missing)`.
fn locate_group(group: &mut [PendingRecord], lines: &[&str], lang: LspLang) -> (usize, usize) {
    let (mut located, mut missing) = (0, 0);
    for record in group {
        let line = &mut record.line;
        if !is_locatable(line) {
            continue;
        }
        match locate_name_in_lines(lines, &line.name, line.l0, line.l1, lang) {
            Some(position) => {
                line.col = Some(position.col_utf16);
                located += 1;
            }
            None => missing += 1,
        }
    }
    (located, missing)
}

/// Normalise paths to `/` and sort entities so the resolver input does not
/// depend on thread completion order (risk R4).
pub(crate) fn normalise_and_sort(entities: &mut [ParsedEntity]) {
    for entity in entities.iter_mut() {
        if entity.file_path.contains('\\') {
            entity.file_path = entity.file_path.replace('\\', "/");
        }
    }
    entities.sort_by_cached_key(|entity| {
        (
            entity.file_path.clone(),
            entity.start_line,
            entity.end_line,
            entity.kind.to_string(),
            entity.name.clone(),
            entity.uuid,
        )
    });
}

fn entity_records(entities: &[ParsedEntity]) -> Vec<PendingRecord> {
    entities
        .iter()
        .map(|entity| PendingRecord {
            file: entity.file_path.clone(),
            line: ShardLine {
                id: entity.uuid.to_string(),
                kind: entity.kind.to_string(),
                name: entity.name.clone(),
                fqn: entity.fqn.clone(),
                lang: entity.language.clone(),
                l0: entity.start_line,
                l1: entity.end_line,
                col: None,
                sig: entity.signature.clone(),
                h: text_hash(&entity.embed_text),
                q: None,
                s: None,
            },
            text: entity.embed_text.clone(),
        })
        .collect()
}

struct Tier1Records {
    records: Vec<PendingRecord>,
    files: usize,
    chunks: usize,
    file_vectors: usize,
}

fn tier1_records(
    cfg: &Config,
    selection: &Selection,
    files_with_entities: &BTreeSet<String>,
) -> anyhow::Result<Tier1Records> {
    let mut out = Tier1Records {
        records: Vec::new(),
        files: 0,
        chunks: 0,
        file_vectors: 0,
    };
    for file in tier1_files(selection, files_with_entities)? {
        let lang = label_for(Path::new(&file.rel));
        let chunks = chunk_file(&file.rel, &file.source);
        if chunks.is_empty() {
            continue;
        }
        out.files += 1;
        for chunk in chunks {
            if chunk.kind == super::shard::KIND_FILE {
                out.file_vectors += 1;
            } else {
                out.chunks += 1;
            }
            out.records.push(PendingRecord {
                line: ShardLine {
                    id: chunk_id(&cfg.repo_name, &chunk.rel, chunk.kind, chunk.l0, chunk.l1),
                    kind: chunk.kind.to_owned(),
                    name: chunk.name,
                    fqn: chunk.rel.clone(),
                    lang: lang.to_owned(),
                    l0: chunk.l0,
                    l1: chunk.l1,
                    col: None,
                    sig: None,
                    h: text_hash(&chunk.text),
                    q: None,
                    s: None,
                },
                file: chunk.rel,
                text: chunk.text,
            });
        }
    }
    Ok(out)
}

/// Pre-fill `q`/`s` from the previous index for every record whose text hash
/// is known (byte-identical output, P2-D6).
fn apply_reuse(records: &mut [PendingRecord], reuse: &ReuseMap) {
    for record in records.iter_mut() {
        if record.text.trim().is_empty() {
            continue;
        }
        if let Some((q, s)) = reuse.get(&record.line.h) {
            record.line.q = Some(q.clone());
            record.line.s = Some(*s);
        }
    }
}

/// Embed every record that has text but no vector yet, in slices of
/// [`EMBED_SLICE`] texts, then normalise and quantise (P2-D5).
fn assign_vectors(
    records: &mut [PendingRecord],
    embedder: &mut dyn EmbedBatch,
    spec: &EmbeddingSpec,
    opts: &BuildOptions,
    progress: &mut dyn FnMut(ProgressEvent),
) -> anyhow::Result<VectorStats> {
    let dim = usize::try_from(spec.dim).unwrap_or(usize::MAX);
    let misses: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| !record.text.trim().is_empty() && !record.line.has_vector())
        .map(|(index, _)| index)
        .collect();
    let without_text = records
        .iter()
        .filter(|record| record.text.trim().is_empty())
        .count();
    let reused = records
        .iter()
        .filter(|record| record.line.has_vector())
        .count();

    let total = misses.len();
    let mut done = 0;
    for slice in misses.chunks(EMBED_SLICE) {
        let texts: Vec<String> = slice.iter().map(|&i| records[i].text.clone()).collect();
        let vectors = embedder.embed_passages(&texts, opts.batch_size.max(1))?;
        if vectors.len() != texts.len() {
            bail!(
                "the embedder returned {} vectors for {} texts",
                vectors.len(),
                texts.len()
            );
        }
        for (&index, mut vector) in slice.iter().zip(vectors) {
            if vector.len() != dim {
                bail!(
                    "the embedder returned a {}-component vector, expected {dim}",
                    vector.len()
                );
            }
            normalize(&mut vector);
            let quantized = encode_i8pv(&vector);
            records[index].line.q = Some(quantized.q);
            records[index].line.s = Some(quantized.s);
        }
        done += slice.len();
        progress(ProgressEvent::Embedded { done, total });
    }
    Ok(VectorStats {
        reused,
        embedded: total,
        without_text,
        reuse_source: ReuseSource::default(),
    })
}

struct ShardStats {
    written: usize,
    unchanged: usize,
    removed: usize,
    /// Distinct shard files that exist after the run.
    files: usize,
}

/// Group records by file, sort them and write one shard per file; delete the
/// shards of files that no longer have records.
fn write_shards(layout: &IndexLayout, records: Vec<PendingRecord>) -> anyhow::Result<ShardStats> {
    let mut by_file: BTreeMap<String, Vec<ShardLine>> = BTreeMap::new();
    for record in records {
        by_file.entry(record.file).or_default().push(record.line);
    }
    let mut stats = ShardStats {
        written: 0,
        unchanged: 0,
        removed: 0,
        files: by_file.len(),
    };
    let mut kept = BTreeSet::new();
    for (file, mut lines) in by_file {
        sort_lines(&mut lines);
        let path = layout.shard_for(&file)?;
        match write_jsonl_if_changed(&path, &lines)? {
            WriteOutcome::Written => stats.written += 1,
            WriteOutcome::Unchanged => stats.unchanged += 1,
        }
        kept.insert(path);
    }
    stats.removed = remove_orphans(&layout.shards, &kept)?;
    Ok(stats)
}

struct EdgeStats {
    written: usize,
    unchanged: usize,
    removed: usize,
    /// Distinct edge shard files that exist after the run.
    files: usize,
    /// Edges kept.
    edges: usize,
    /// Edges dropped because an endpoint is not an indexed entity.
    dropped: usize,
}

/// Group edges by the caller's file and drop those with an endpoint that is
/// not an entity of the index. Edges inside each group are sorted.
fn partition_edges(
    edges: &BTreeSet<(Uuid, Uuid, String)>,
    entity_files: &HashMap<Uuid, &str>,
) -> (BTreeMap<String, Vec<EdgeLine>>, usize) {
    let mut by_file: BTreeMap<String, Vec<EdgeLine>> = BTreeMap::new();
    let mut dropped = 0;
    for (src, dst, rel) in edges {
        match (entity_files.get(src), entity_files.contains_key(dst)) {
            (Some(file), true) => by_file
                .entry((*file).to_owned())
                .or_default()
                .push(EdgeLine {
                    src: src.to_string(),
                    dst: dst.to_string(),
                    rel: rel.clone(),
                }),
            _ => dropped += 1,
        }
    }
    for group in by_file.values_mut() {
        group.sort();
    }
    (by_file, dropped)
}

/// Write one edge shard per caller file; delete the shards of files that no
/// longer have edges.
fn write_edges(
    layout: &IndexLayout,
    edges: &BTreeSet<(Uuid, Uuid, String)>,
    entity_files: &HashMap<Uuid, &str>,
) -> anyhow::Result<EdgeStats> {
    let (by_file, dropped) = partition_edges(edges, entity_files);
    let mut stats = EdgeStats {
        written: 0,
        unchanged: 0,
        removed: 0,
        files: by_file.len(),
        edges: edges.len() - dropped,
        dropped,
    };
    let mut kept = BTreeSet::new();
    for (file, lines) in &by_file {
        let path = layout.edges_for(file)?;
        match write_jsonl_if_changed(&path, lines)? {
            WriteOutcome::Written => stats.written += 1,
            WriteOutcome::Unchanged => stats.unchanged += 1,
        }
        kept.insert(path);
    }
    stats.removed = remove_orphans(&layout.edges, &kept)?;
    Ok(stats)
}

fn index_counts(
    selection: &Selection,
    inventory: &Inventory,
    stats: &RecordStats,
    shards: &ShardStats,
    edges: &EdgeStats,
) -> IndexCounts {
    IndexCounts {
        files_inventoried: count(inventory.entries.len()),
        files_parsed: count(selection.parse.len()),
        files_with_entities: count(stats.files_with_entities),
        tier1_files: count(stats.tier1_files),
        entities: count(stats.entities),
        entities_without_vector: count(stats.entities_without_vector),
        chunks: count(stats.chunks),
        file_vectors: count(stats.file_vectors),
        records: count(stats.records),
        edges: count(edges.edges),
        dropped_edges: count(edges.dropped),
        shards: count(shards.files),
        edge_shards: count(edges.files),
        columns_located: count(stats.columns_located),
        columns_missing: count(stats.columns_missing),
    }
}

/// The run-specific values of a manifest.
struct ManifestParts<'a> {
    spec: &'a EmbeddingSpec,
    source_fingerprint: String,
    counts: IndexCounts,
    timings: IndexTimings,
}

fn build_manifest(cfg: &Config, dir: &RunDir, parts: ManifestParts<'_>) -> Manifest {
    let now = now_rfc3339();
    Manifest {
        schema: MANIFEST_SCHEMA_V1.to_owned(),
        knot_lite_version: env!("CARGO_PKG_VERSION").to_owned(),
        knot_version: KNOT_VERSION.to_owned(),
        repo_name: cfg.repo_name.clone(),
        created_at: dir
            .previous
            .as_ref()
            .map_or_else(|| now.clone(), |manifest| manifest.created_at.clone()),
        updated_at: now,
        source_fingerprint: parts.source_fingerprint,
        commit_mode: dir.commit_mode,
        embedding: parts.spec.clone(),
        counts: parts.counts,
        timings_ms: parts.timings,
    }
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Fixtures shared by the build tests and the incremental re-index tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::config::test_support::config_for;
    use crate::embed::fake::FakeEmbedder;
    use crate::index::fixture::write_fixture_repo;
    use crate::index::record_io::read_jsonl;

    pub(crate) const DIM: usize = 16;

    pub(crate) fn fixture() -> (tempfile::TempDir, Config) {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture_repo(tmp.path()).expect("fixture");
        let cfg = config_for(tmp.path().to_path_buf());
        (tmp, cfg)
    }

    pub(crate) fn opts() -> BuildOptions {
        BuildOptions {
            jobs: 2,
            batch_size: 8,
            ..BuildOptions::default()
        }
    }

    pub(crate) fn run_with(cfg: &Config, opts: &BuildOptions) -> BuildReport {
        let mut fake = FakeEmbedder::new(DIM);
        build_index(cfg, &mut fake, opts, &mut |_| {}).expect("build")
    }

    pub(crate) fn run(cfg: &Config) -> BuildReport {
        run_with(cfg, &opts())
    }

    /// Every file under `dir` as `/`-separated relative path → bytes.
    pub(crate) fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).expect("read_dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    let rel = path.strip_prefix(base).expect("under base");
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    out.insert(rel, std::fs::read(&path).expect("read"));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    /// Every shard as repo-relative file → lines.
    pub(crate) fn shards(layout: &IndexLayout) -> BTreeMap<String, Vec<ShardLine>> {
        snapshot(&layout.shards)
            .into_keys()
            .map(|key| {
                let rel = key.strip_suffix(".jsonl").expect("record file").to_owned();
                let lines = read_jsonl(&layout.shard_for(&rel).expect("path")).expect("read");
                (rel, lines)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{DIM, fixture, opts, run, run_with, shards, snapshot};
    use super::*;
    use crate::config::test_support::config_for;
    use crate::embed::fake::FakeEmbedder;
    use crate::index::encode::decode_i8pv;
    use crate::index::manifest::{IndexState, read_index_state};
    use crate::index::record_io::read_jsonl;

    #[test]
    fn build_writes_expected_layout() {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let layout = IndexLayout::for_repo(tmp.path());
        for rel in [
            "src/lib.rs",
            "src/util.rs",
            "web/app.ts",
            "docs/guide.md",
            "scripts/run.sh",
            "Makefile",
        ] {
            assert!(layout.shard_for(rel).expect("path").exists(), "{rel}");
        }
        assert!(
            !layout.shards.join("target").exists(),
            "build output is never indexed"
        );
        assert!(layout.manifest.exists());
        assert!(layout.inventory.exists());
        assert!(layout.gitignore.exists());
        assert!(!layout.previous_manifest.exists());
        let files = snapshot(&layout.dir);
        assert!(
            files.keys().all(|name| !name.ends_with(".tmp")),
            "{files:?}"
        );
    }

    #[test]
    fn build_indexes_the_expected_entities() {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let by_file = shards(&IndexLayout::for_repo(tmp.path()));
        let names = |file: &str| -> Vec<String> {
            by_file[file].iter().map(|line| line.name.clone()).collect()
        };
        assert!(names("src/lib.rs").contains(&"target".to_owned()));
        assert!(names("src/lib.rs").contains(&"caller".to_owned()));
        assert!(names("src/util.rs").contains(&"helper".to_owned()));
        assert!(names("web/app.ts").contains(&"greet".to_owned()));
        let kinds: Vec<&str> = by_file["scripts/run.sh"]
            .iter()
            .map(|l| l.kind.as_str())
            .collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == "chunk").count(),
            2,
            "{kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|k| **k == "file").count(),
            1,
            "{kinds:?}"
        );
        assert_eq!(
            by_file["scripts/run.sh"][0].lang, "other",
            "no `.sh` row in the label table"
        );
        // knot parses build manifests even with config files off: Cargo.toml
        // yields entities (language `toml`) and is therefore not tier 1.
        let cargo_kinds: Vec<&str> = by_file["Cargo.toml"]
            .iter()
            .map(|l| l.kind.as_str())
            .collect();
        assert_eq!(cargo_kinds, vec!["cargo_package", "project_identity"]);
        assert!(by_file["Cargo.toml"].iter().all(|line| line.lang == "toml"));
        assert_eq!(
            by_file.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "Cargo.toml",
                "Makefile",
                "docs/guide.md",
                "scripts/run.sh",
                "src/lib.rs",
                "src/util.rs",
                "web/app.ts"
            ],
            "exactly the fixture files except build output"
        );
    }

    #[test]
    fn build_counts_match_records() {
        let (tmp, cfg) = fixture();
        let report = run(&cfg);
        let layout = IndexLayout::for_repo(tmp.path());
        let by_file = shards(&layout);
        let records: usize = by_file.values().map(Vec::len).sum();
        let counts = &report.counts;
        assert_eq!(counts.records, count(records));
        assert_eq!(
            counts.records,
            counts.entities + counts.chunks + counts.file_vectors
        );
        assert_eq!(counts.shards, count(by_file.len()));
        assert_eq!(
            report.vectors.embedded + report.vectors.without_text,
            records,
            "every record is embedded or has no text"
        );
        assert_eq!(report.vectors.reused, 0);
        assert_eq!(
            counts.files_inventoried,
            count(snapshot_inventory_len(&layout))
        );
        assert_eq!(
            read_manifest(&layout.manifest).expect("manifest").counts,
            *counts
        );
    }

    fn snapshot_inventory_len(layout: &IndexLayout) -> usize {
        crate::index::inventory::read_inventory(&layout.inventory)
            .expect("inventory")
            .entries
            .len()
    }

    #[test]
    fn build_records_are_sorted_and_parse_as_shard_lines() {
        let (tmp, cfg) = fixture();
        run(&cfg);
        for (file, lines) in shards(&IndexLayout::for_repo(tmp.path())) {
            assert!(!lines.is_empty(), "{file}");
            let mut sorted = lines.clone();
            sort_lines(&mut sorted);
            assert_eq!(lines, sorted, "{file} must be stored sorted");
            let served = lsp_lang_for_label(label_for(Path::new(&file))).is_some();
            for line in &lines {
                assert!(line.l0 >= 1 && line.l1 >= line.l0, "{file}: {line:?}");
                if !served {
                    assert_eq!(line.col, None, "{file}: no language server, no column");
                }
            }
        }
    }

    #[test]
    fn build_vectors_decode_to_unit_norm() {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let mut decoded = 0;
        for lines in shards(&IndexLayout::for_repo(tmp.path())).values() {
            for line in lines.iter().filter(|line| line.has_vector()) {
                let vector = decode_i8pv(line.q.as_deref().expect("q"), line.s.expect("s"), DIM)
                    .expect("decode");
                let norm: f64 = vector
                    .iter()
                    .map(|x| f64::from(*x).powi(2))
                    .sum::<f64>()
                    .sqrt();
                assert!((norm - 1.0).abs() < 0.02, "{norm} for {line:?}");
                decoded += 1;
            }
        }
        assert!(decoded > 10, "decoded {decoded} vectors");
    }

    #[test]
    fn build_writes_nothing_outside_index_dir() {
        let (tmp, cfg) = fixture();
        let before = snapshot(tmp.path());
        run(&cfg);
        let after = snapshot(tmp.path());
        let outside = |files: &BTreeMap<String, Vec<u8>>| -> BTreeMap<String, Vec<u8>> {
            files
                .iter()
                .filter(|(rel, _)| !rel.starts_with(".knot-lite/"))
                .map(|(rel, bytes)| (rel.clone(), bytes.clone()))
                .collect()
        };
        assert_eq!(outside(&before), outside(&after));
        assert!(after.keys().any(|rel| rel.starts_with(".knot-lite/")));
    }

    #[test]
    fn build_is_byte_identical_across_runs() {
        let (tmp, cfg) = fixture();
        let layout = IndexLayout::for_repo(tmp.path());
        run(&cfg);
        let first = snapshot(&layout.dir);
        std::fs::remove_dir_all(&layout.dir).expect("remove index");
        run(&cfg);
        let second = snapshot(&layout.dir);

        assert_eq!(
            first.keys().collect::<Vec<_>>(),
            second.keys().collect::<Vec<_>>()
        );
        for (rel, bytes) in &first {
            if rel != "manifest.toml" {
                assert_eq!(bytes, &second[rel], "{rel} differs between runs");
            }
        }
        let parse = |bytes: &[u8]| {
            let mut manifest: Manifest =
                toml::from_str(std::str::from_utf8(bytes).expect("utf8")).expect("manifest");
            manifest.created_at.clear();
            manifest.updated_at.clear();
            manifest.timings_ms = IndexTimings::default();
            manifest
        };
        assert_eq!(
            parse(&first["manifest.toml"]),
            parse(&second["manifest.toml"])
        );
    }

    #[test]
    fn second_run_leaves_shards_unchanged_and_keeps_created_at() {
        let (tmp, cfg) = fixture();
        let layout = IndexLayout::for_repo(tmp.path());
        let first = run(&cfg);
        let created = read_manifest(&layout.manifest)
            .expect("manifest")
            .created_at;
        let second = run(&cfg);
        assert_eq!(second.files.shards_written, 0);
        assert_eq!(second.files.shards_unchanged, first.files.shards_written);
        assert_eq!(
            read_manifest(&layout.manifest)
                .expect("manifest")
                .created_at,
            created
        );
    }

    #[test]
    fn build_survives_more_than_1024_entities() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source: String = (0..1100).map(|i| format!("pub fn f{i}() {{}}\n")).collect();
        std::fs::write(tmp.path().join("many.rs"), source).expect("write");
        let cfg = config_for(tmp.path().to_path_buf());

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut fake = FakeEmbedder::new(DIM);
            let outcome = build_index(&cfg, &mut fake, &opts(), &mut |_| {});
            tx.send(outcome.map(|report| report.counts.entities)).ok();
        });
        let entities = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the build must not deadlock above 1024 entities")
            .expect("build");
        assert!(entities >= 1100, "{entities}");
    }

    #[test]
    fn build_commit_mode_persists() {
        let (tmp, cfg) = fixture();
        let layout = IndexLayout::for_repo(tmp.path());

        let report = run_with(
            &cfg,
            &BuildOptions {
                commit_mode: Some(CommitMode::Commit),
                ..opts()
            },
        );
        assert_eq!(report.commit_mode, CommitMode::Commit);
        assert!(layout.gitattributes.exists() && !layout.gitignore.exists());

        run_with(
            &cfg,
            &BuildOptions {
                commit_mode: None,
                ..opts()
            },
        );
        assert!(layout.gitattributes.exists() && !layout.gitignore.exists());
        assert_eq!(
            read_manifest(&layout.manifest)
                .expect("manifest")
                .commit_mode,
            CommitMode::Commit
        );

        run_with(
            &cfg,
            &BuildOptions {
                commit_mode: Some(CommitMode::Gitignore),
                ..opts()
            },
        );
        assert!(layout.gitignore.exists() && !layout.gitattributes.exists());
    }

    #[test]
    fn build_progress_reports_stages_in_order() {
        let (_tmp, cfg) = fixture();
        let mut events = Vec::new();
        let mut fake = FakeEmbedder::new(DIM);
        build_index(&cfg, &mut fake, &opts(), &mut |event| events.push(event)).expect("build");

        let stages: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                ProgressEvent::Stage { name } => Some(*name),
                _ => None,
            })
            .collect();
        assert_eq!(
            stages,
            vec!["select", "parse", "prepare", "resolve", "embed", "write"]
        );
        assert!(matches!(events.last(), Some(ProgressEvent::Done { .. })));
        let embedded: Vec<(usize, usize)> = events
            .iter()
            .filter_map(|event| match event {
                ProgressEvent::Embedded { done, total } => Some((*done, *total)),
                _ => None,
            })
            .collect();
        let (done, total) = *embedded.last().expect("at least one embed step");
        assert_eq!(done, total);
        assert!(embedded.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn build_marks_incomplete_when_interrupted() {
        let (tmp, cfg) = fixture();
        let layout = IndexLayout::for_repo(tmp.path());
        std::fs::create_dir_all(&layout.dir).expect("mkdir");
        std::fs::write(
            &layout.previous_manifest,
            "schema = \"knot-lite/manifest/v1\"\n",
        )
        .expect("prev");
        assert_eq!(read_index_state(&layout), IndexState::Incomplete);

        let report = run(&cfg);
        assert!(matches!(
            read_index_state(&layout),
            IndexState::Present { .. }
        ));
        assert!(!layout.previous_manifest.exists());
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("previous manifest"),
            "{:?}",
            report.warnings
        );
    }

    #[test]
    fn build_report_serialises_the_index_run_schema() {
        let (_tmp, cfg) = fixture();
        let report = run(&cfg);
        let json = serde_json::to_string(&report).expect("json");
        let mut last = 0;
        for key in [
            "\"schema\":\"knot-lite/index-run/v1\"",
            "\"knot_lite_version\"",
            "\"repo\"",
            "\"index_dir\":\".knot-lite\"",
            "\"commit_mode\":\"gitignore\"",
            "\"embedding\"",
            "\"counts\"",
            "\"vectors\"",
            "\"files\"",
            "\"timings_ms\"",
            "\"warnings\":[]",
        ] {
            let at = json
                .find(key)
                .unwrap_or_else(|| panic!("{key} missing in {json}"));
            assert!(at >= last, "{key} out of order in {json}");
            last = at;
        }
        assert!(json.contains("\"model\":\"knot-lite-fake-sha256\""));
    }

    /// Embedder whose vectors have the wrong number of components.
    struct ShortEmbedder;

    impl EmbedBatch for ShortEmbedder {
        fn spec(&self) -> EmbeddingSpec {
            FakeEmbedder::new(DIM).spec()
        }

        fn embed_passages(
            &mut self,
            texts: &[String],
            _batch: usize,
        ) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0; DIM - 1]).collect())
        }

        fn embed_query(&mut self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![1.0; DIM - 1])
        }
    }

    #[test]
    fn build_rejects_vectors_of_the_wrong_dimension() {
        let (_tmp, cfg) = fixture();
        let error =
            build_index(&cfg, &mut ShortEmbedder, &opts(), &mut |_| {}).expect_err("must fail");
        assert!(error.to_string().contains("expected 16"), "{error}");
    }

    /// Every edge shard as caller file → edges.
    fn edge_shards(layout: &IndexLayout) -> BTreeMap<String, Vec<EdgeLine>> {
        snapshot(&layout.edges)
            .into_keys()
            .map(|key| {
                let rel = key.strip_suffix(".jsonl").expect("record file").to_owned();
                let lines = read_jsonl(&layout.edges_for(&rel).expect("path")).expect("read");
                (rel, lines)
            })
            .collect()
    }

    /// `file::name` of every entity id in the index.
    fn names_by_id(layout: &IndexLayout) -> HashMap<String, String> {
        shards(layout)
            .into_iter()
            .flat_map(|(file, lines)| {
                lines
                    .into_iter()
                    .map(move |line| (line.id, format!("{file}::{}", line.name)))
            })
            .collect()
    }

    #[test]
    fn real_parse_resolves_fixture_calls() {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let layout = IndexLayout::for_repo(tmp.path());
        let names = names_by_id(&layout);
        let triples: BTreeSet<(String, String, String)> = edge_shards(&layout)
            .into_values()
            .flatten()
            .map(|edge| (names[&edge.src].clone(), edge.rel, names[&edge.dst].clone()))
            .collect();
        let calls = |src: &str, dst: &str| (src.to_owned(), "CALLS".to_owned(), dst.to_owned());
        for expected in [
            calls("src/lib.rs::caller", "src/lib.rs::target"),
            calls("src/lib.rs::caller", "src/util.rs::helper"),
            calls("src/util.rs::helper", "src/lib.rs::target"),
            calls("src/util.rs::run", "src/lib.rs::target"),
            calls("web/app.ts::main", "web/app.ts::greet"),
        ] {
            assert!(
                triples.contains(&expected),
                "{expected:?} missing from {triples:?}"
            );
        }
    }

    #[test]
    fn edges_are_keyed_by_caller_file() {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let layout = IndexLayout::for_repo(tmp.path());
        let entities = shards(&layout);
        let all_ids: BTreeSet<&str> = entities.values().flatten().map(|l| l.id.as_str()).collect();
        let edges = edge_shards(&layout);
        assert_eq!(
            edges.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["src/lib.rs", "src/util.rs", "web/app.ts"]
        );
        for (file, lines) in &edges {
            let own: BTreeSet<&str> = entities[file].iter().map(|l| l.id.as_str()).collect();
            let mut sorted = lines.clone();
            sorted.sort();
            assert_eq!(lines, &sorted, "{file} edges are stored sorted");
            for edge in lines {
                assert!(
                    own.contains(edge.src.as_str()),
                    "{file}: src {} is not its entity",
                    edge.src
                );
                assert!(
                    all_ids.contains(edge.dst.as_str()),
                    "{file}: dangling dst {}",
                    edge.dst
                );
                assert!(crate::graph::heuristic::EDGE_REL_LABELS.contains(&edge.rel.as_str()));
            }
        }
    }

    #[test]
    fn build_edge_counts_match_the_edge_shards() {
        let (tmp, cfg) = fixture();
        let report = run(&cfg);
        let layout = IndexLayout::for_repo(tmp.path());
        let edges = edge_shards(&layout);
        let total: usize = edges.values().map(Vec::len).sum();
        assert_eq!(report.counts.edges, count(total));
        assert_eq!(report.counts.edge_shards, count(edges.len()));
        assert_eq!(report.counts.dropped_edges, 0);
        assert_eq!(report.files.edge_shards_written, edges.len());
        assert_eq!(
            report.resolver.entities_indexed,
            usize::try_from(report.counts.entities).expect("fits")
        );
        assert_eq!(
            read_manifest(&layout.manifest).expect("manifest").counts,
            report.counts
        );
    }

    #[test]
    fn dangling_edges_are_dropped_and_counted() {
        let (a, b, ghost) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let files: HashMap<Uuid, &str> = HashMap::from([(a, "f1.rs"), (b, "f2.rs")]);
        let calls = |src, dst| (src, dst, "CALLS".to_owned());
        let edges: BTreeSet<_> =
            [calls(a, b), calls(a, ghost), calls(ghost, b), calls(a, a)].into();

        let (by_file, dropped) = partition_edges(&edges, &files);
        assert_eq!(dropped, 2, "an unknown dst and an unknown src");
        assert_eq!(by_file.keys().collect::<Vec<_>>(), vec!["f1.rs"]);
        let kept: Vec<(&str, &str)> = by_file["f1.rs"]
            .iter()
            .map(|e| (e.src.as_str(), e.dst.as_str()))
            .collect();
        assert_eq!(kept.len(), 2, "self edges are kept: {kept:?}");
        assert!(kept.contains(&(a.to_string().as_str(), a.to_string().as_str())));
    }

    #[test]
    fn edge_shard_removed_with_caller_file() {
        let (tmp, cfg) = fixture();
        let layout = IndexLayout::for_repo(tmp.path());
        run(&cfg);
        assert!(layout.edges_for("src/util.rs").expect("path").exists());

        std::fs::remove_file(tmp.path().join("src/util.rs")).expect("remove");
        let report = run(&cfg);
        assert!(!layout.edges_for("src/util.rs").expect("path").exists());
        assert_eq!(report.files.edge_shards_removed, 1);
        let remaining: Vec<String> = edge_shards(&layout).into_keys().collect();
        assert_eq!(remaining, vec!["src/lib.rs", "web/app.ts"]);
    }

    #[test]
    fn edges_are_byte_identical_across_runs() {
        let (tmp, cfg) = fixture();
        let layout = IndexLayout::for_repo(tmp.path());
        run(&cfg);
        let first = snapshot(&layout.edges);
        assert!(!first.is_empty());
        std::fs::remove_dir_all(&layout.dir).expect("remove index");
        run(&cfg);
        assert_eq!(snapshot(&layout.edges), first);
    }
}

/// P3.3: the UTF-16 name column of entities served by a language server.
#[cfg(test)]
mod column_tests {
    use super::test_support::{fixture, run, shards};
    use super::*;

    /// A pending entity record with nothing but the fields the column pass reads.
    fn pending(file: &str, kind: &str, name: &str, l0: usize, l1: usize) -> PendingRecord {
        PendingRecord {
            file: file.to_owned(),
            line: ShardLine {
                id: format!("{file}:{name}"),
                kind: kind.to_owned(),
                name: name.to_owned(),
                fqn: name.to_owned(),
                lang: "x".to_owned(),
                l0,
                l1,
                col: None,
                sig: None,
                h: String::new(),
                q: None,
                s: None,
            },
            text: String::new(),
        }
    }

    /// Shards of the fixture repository after one build.
    fn built() -> (
        tempfile::TempDir,
        BTreeMap<String, Vec<ShardLine>>,
        BuildReport,
    ) {
        let (tmp, cfg) = fixture();
        let report = run(&cfg);
        let by_file = shards(&IndexLayout::for_repo(tmp.path()));
        (tmp, by_file, report)
    }

    fn col_of(by_file: &BTreeMap<String, Vec<ShardLine>>, file: &str, name: &str) -> Option<u32> {
        by_file[file]
            .iter()
            .find(|line| line.name == name)
            .unwrap_or_else(|| panic!("{name} missing from {file}"))
            .col
    }

    #[test]
    fn col_is_set_for_rust_and_ts_entities() {
        let (_tmp, by_file, _) = built();
        // `pub fn target() -> u32 {`
        assert_eq!(col_of(&by_file, "src/lib.rs", "target"), Some(7));
        assert_eq!(col_of(&by_file, "src/lib.rs", "caller"), Some(7));
        assert_eq!(col_of(&by_file, "src/util.rs", "helper"), Some(7));
        // `    pub fn run(&self) -> u32 {`
        assert_eq!(col_of(&by_file, "src/util.rs", "run"), Some(11));
        // `export function greet(n: string): string {`
        assert_eq!(col_of(&by_file, "web/app.ts", "greet"), Some(16));
        assert_eq!(col_of(&by_file, "web/app.ts", "main"), Some(16));
    }

    #[test]
    fn col_is_null_for_tier1_and_markdown() {
        let (_tmp, by_file, _) = built();
        for file in ["docs/guide.md", "scripts/run.sh", "Makefile", "Cargo.toml"] {
            assert!(
                by_file[file].iter().all(|line| line.col.is_none()),
                "{file}: {:?}",
                by_file[file]
            );
        }
    }

    #[test]
    fn col_is_utf16_not_bytes() {
        let (tmp, cfg) = fixture();
        // `ñ` is 2 bytes / 1 UTF-16 unit and the emoji 4 bytes / 2 units, so the
        // byte column (29) and the UTF-16 column (26) of `wide` differ.
        std::fs::write(
            tmp.path().join("web/wide.ts"),
            "export function /* ñ😀 */ wide() {}\n",
        )
        .expect("write");
        run(&cfg);
        let by_file = shards(&IndexLayout::for_repo(tmp.path()));
        assert_eq!(col_of(&by_file, "web/wide.ts", "wide"), Some(26));
    }

    #[test]
    fn unreadable_file_leaves_col_null_with_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A directory where a source file should be: reading it fails on every
        // platform and, unlike `chmod 000`, also for root.
        std::fs::create_dir(tmp.path().join("broken.rs")).expect("mkdir");
        let mut records = vec![pending("broken.rs", "function", "broken", 1, 3)];
        let pass = assign_columns(&mut records, tmp.path());
        assert_eq!(records[0].line.col, None);
        assert_eq!((pass.located, pass.missing), (0, 1));
        assert_eq!(pass.warnings.len(), 1);
        assert!(
            pass.warnings[0].starts_with("col: cannot read broken.rs:"),
            "{:?}",
            pass.warnings
        );
    }

    #[test]
    fn module_entity_counts_as_neither_located_nor_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}\n").expect("write");
        let mut records = vec![
            pending("a.rs", "rust_function", "<module>", 1, 1),
            pending("a.rs", "rust_function", "a", 1, 1),
            pending("a.rs", "rust_function", "ghost", 1, 1),
        ];
        let pass = assign_columns(&mut records, tmp.path());
        assert_eq!((pass.located, pass.missing), (1, 1));
        assert_eq!(records[0].line.col, None);
        assert_eq!(records[1].line.col, Some(7));
        assert_eq!(records[2].line.col, None);
        assert!(pass.warnings.is_empty());
    }

    #[test]
    fn non_symbol_kinds_are_neither_located_nor_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("view.tsx"),
            "const v = <p className=\"hero\" id=\"hero\" />;\nmod hero {}\n",
        )
        .expect("write");
        let mut records = vec![
            pending("view.tsx", "html_class", "hero", 1, 1),
            pending("view.tsx", "html_id", "hero", 1, 1),
            pending("view.tsx", "rust_module", "hero", 2, 2),
            pending("view.tsx", "rust_impl", "hero", 2, 2),
        ];
        let pass = assign_columns(&mut records, tmp.path());
        assert_eq!((pass.located, pass.missing), (0, 0));
        assert!(records.iter().all(|record| record.line.col.is_none()));
    }

    #[test]
    fn unlocatable_symbol_is_counted_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // knot places import bindings on line 1 even when the import is on line 2.
        std::fs::write(
            tmp.path().join("vite.config.ts"),
            "import a from 'a'\nimport pkg from './package.json'\n",
        )
        .expect("write");
        let mut records = vec![pending("vite.config.ts", "constant", "pkg", 1, 1)];
        let pass = assign_columns(&mut records, tmp.path());
        assert_eq!((pass.located, pass.missing), (0, 1));
        assert_eq!(records[0].line.col, None);
    }

    #[test]
    fn files_of_other_languages_are_not_even_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Neither file exists: a read attempt would produce a warning.
        let mut records = vec![
            pending("docs/guide.md", "function", "Guide", 1, 2),
            pending("scripts/run.sh", "function", "run", 1, 2),
        ];
        let pass = assign_columns(&mut records, tmp.path());
        assert_eq!((pass.located, pass.missing), (0, 0));
        assert!(pass.warnings.is_empty());
        assert!(records.iter().all(|record| record.line.col.is_none()));
    }

    #[test]
    fn columns_counters_in_manifest() {
        let (tmp, by_file, report) = built();
        let served = |file: &str| lsp_lang_for_label(label_for(Path::new(file))).is_some();
        let (mut located, mut missing) = (0, 0);
        for (file, lines) in &by_file {
            for line in lines
                .iter()
                .filter(|line| served(file) && is_locatable(line))
            {
                if line.col.is_some() {
                    located += 1;
                } else {
                    missing += 1;
                }
            }
        }
        assert!(located >= 6, "the fixture has six located entities");
        assert_eq!(report.counts.columns_located, count(located));
        assert_eq!(report.counts.columns_missing, count(missing));
        let layout = IndexLayout::for_repo(tmp.path());
        assert_eq!(
            read_manifest(&layout.manifest).expect("manifest").counts,
            report.counts
        );
    }

    #[test]
    fn reuse_is_unaffected_by_col() {
        let (tmp, cfg) = fixture();
        let first = run(&cfg);
        assert!(first.vectors.embedded > 0);
        let second = run(&cfg);
        assert_eq!(second.vectors.embedded, 0, "no vector is recomputed");
        assert_eq!(second.vectors.reused, first.vectors.embedded);
        let by_file = shards(&IndexLayout::for_repo(tmp.path()));
        assert_eq!(col_of(&by_file, "src/lib.rs", "target"), Some(7));
    }
}
