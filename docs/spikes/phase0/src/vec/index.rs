//! S2 indexing pipeline: parse → embed → shards (T9.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};

use knot::models::ParsedEntity;
use knot::pipeline::embed::Embedder;
use knot::pipeline::embed::model::EmbedModelChoice;

use crate::entities::EntityTable;
use crate::position;
use crate::vec::chunk::{self, Chunk, Tier1File};
use crate::vec::encode::{self, VARIANTS, Variant};
use crate::vec::shard::{self, ShardLine};

/// Inputs of one `vec_probe index` run (T9.1).
pub struct IndexConfig {
    pub repo: String,
    pub root: PathBuf,
    pub out_dir: PathBuf,
    pub batch_size: usize,
    pub per_file_batches: bool,
    /// Re-index a single file (churn experiment, T9.6).
    pub only_file: Option<String>,
}

/// Everything `measure` needs about the index run (T9.5 counts/timings).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexStats {
    pub files_scanned: usize,
    pub files_with_entities: usize,
    pub tier1_files: usize,
    pub entities: usize,
    pub chunks: usize,
    pub file_vectors: usize,
    pub skipped_no_embed_text: usize,
    pub col_total: usize,
    pub col_nulls: usize,
    pub parse_ms: u64,
    pub prepare_ms: u64,
    pub embed_ms: u64,
    pub embed_chunks_ms: u64,
    pub norm_min: f64,
    pub norm_max: f64,
    pub norm_mean: f64,
    /// Vectors were renormalised before quantisation (T9.2 step 7).
    pub normalized: bool,
    pub i8fx_clip_rate: f64,
    pub shard_raw_bytes: BTreeMap<String, u64>,
    pub shard_files: BTreeMap<String, u64>,
}

/// Parse, embed and write every variant (T9.2).
pub fn run_index(cfg: &IndexConfig) -> Result<IndexStats> {
    std::fs::create_dir_all(&cfg.out_dir)
        .with_context(|| format!("creating {}", cfg.out_dir.display()))?;

    let parse_started = Instant::now();
    let (table, mut parsed) = EntityTable::build(&cfg.repo, &cfg.root)?;
    let parse_ms = parse_started.elapsed().as_millis() as u64;

    let prepare_started = Instant::now();
    knot::pipeline::prepare::prepare_entities(&mut parsed);
    let prepare_ms = prepare_started.elapsed().as_millis() as u64;

    let mut embeddable = Vec::new();
    let mut skipped_no_embed_text = 0;
    for entity in parsed {
        if entity.embed_text.trim().is_empty() {
            skipped_no_embed_text += 1;
        } else {
            embeddable.push(entity);
        }
    }
    if let Some(only) = &cfg.only_file {
        // Churn runs re-embed one file (T9.6); the rest of the index is
        // already on disk.
        embeddable.retain(|entity| &entity.file_path == only);
    }

    let files_with_entities: BTreeSet<String> = embeddable
        .iter()
        .map(|entity| entity.file_path.clone())
        .collect();
    let tier1 = chunk::tier1_files(&cfg.root, &files_with_entities)?;
    let files_scanned = chunk::scanned_candidates(&cfg.root)?.len();

    let mut embedder = init_embedder()?;
    let mut stats = IndexStats {
        files_scanned,
        files_with_entities: files_with_entities.len(),
        tier1_files: tier1.len(),
        entities: embeddable.len(),
        chunks: 0,
        file_vectors: 0,
        skipped_no_embed_text,
        col_total: 0,
        col_nulls: 0,
        parse_ms,
        prepare_ms,
        embed_ms: 0,
        embed_chunks_ms: 0,
        norm_min: 0.0,
        norm_max: 0.0,
        norm_mean: 0.0,
        normalized: false,
        i8fx_clip_rate: 0.0,
        shard_raw_bytes: BTreeMap::new(),
        shard_files: BTreeMap::new(),
    };

    let embed_started = Instant::now();
    let mut embedded = if cfg.per_file_batches {
        embed_per_file(&mut embedder, embeddable, cfg.batch_size)?
    } else {
        embedder.embed(embeddable, cfg.batch_size)?
    };
    stats.embed_ms = embed_started.elapsed().as_millis() as u64;

    let (chunk_rows, chunk_embed_ms) = embed_chunks(&mut embedder, cfg, &tier1, &table)?;
    stats.embed_chunks_ms = chunk_embed_ms;
    stats.chunks = chunk_rows.iter().filter(|r| r.line.kind == "chunk").count();
    stats.file_vectors = chunk_rows.iter().filter(|r| r.line.kind == "file").count();

    // Entity records need the name column and text hash (T9.4).
    let mut by_file: BTreeMap<String, Vec<Record>> = BTreeMap::new();
    for item in &mut embedded {
        let col = locate_col(&cfg.root, &item.entity);
        stats.col_total += 1;
        if col.is_none() {
            stats.col_nulls += 1;
        }
        by_file
            .entry(item.entity.file_path.clone())
            .or_default()
            .push(entity_record(item, col));
    }
    for record in chunk_rows {
        by_file
            .entry(record.line.fqn.clone())
            .or_default()
            .push(record);
    }
    normalize_if_needed(&mut by_file, &mut stats);

    write_all_variants(&cfg.out_dir, cfg, &by_file, &mut stats)?;

    // A single-file re-index must not clobber the full run's stats/cache.
    if cfg.only_file.is_none() {
        std::fs::write(
            cfg.out_dir.join("index_stats.json"),
            serde_json::to_string_pretty(&stats)?,
        )?;
    }
    Ok(stats)
}

fn entity_record(item: &knot::models::EmbeddedEntity, col: Option<u32>) -> Record {
    Record {
        line: ShardLine {
            id: item.entity.uuid.to_string(),
            kind: item.entity.kind.to_string(),
            name: item.entity.name.clone(),
            fqn: item.entity.fqn.clone(),
            lang: item.entity.language.clone(),
            l0: item.entity.start_line,
            l1: item.entity.end_line,
            col,
            sig: item.entity.signature.clone(),
            h: shard::text_hash(&item.entity.embed_text),
            v: None,
            q: None,
            s: None,
        },
        vector: item.vector.clone(),
    }
}

/// Record vector norms; renormalise before quantisation when needed (T9.2 7).
fn normalize_if_needed(by_file: &mut BTreeMap<String, Vec<Record>>, stats: &mut IndexStats) {
    let mut norms: Vec<f64> = Vec::new();
    for records in by_file.values() {
        for record in records {
            norms.push(encode::norm(&record.vector));
        }
    }
    if norms.is_empty() {
        return;
    }
    stats.norm_min = norms.iter().cloned().fold(f64::INFINITY, f64::min);
    stats.norm_max = norms.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    stats.norm_mean = norms.iter().sum::<f64>() / norms.len() as f64;
    if stats.norm_min < 0.98 || stats.norm_max > 1.02 {
        stats.normalized = true;
        for records in by_file.values_mut() {
            for record in records.iter_mut() {
                encode::normalize(&mut record.vector);
            }
        }
    }
}

/// One record: the shard line plus its f32 vector (variant encoding later).
#[derive(Clone)]
struct Record {
    line: ShardLine,
    vector: Vec<f32>,
}

pub fn init_embedder() -> Result<Embedder> {
    let choice = EmbedModelChoice::supported()
        .iter()
        .find(|(name, _)| *name == "AllMiniLML6V2")
        .map(|(_, choice)| choice.clone())
        .context("AllMiniLML6V2 not in EmbedModelChoice::supported()")?;
    assert!(
        choice.query_prefix.is_empty() && choice.passage_prefix.is_empty(),
        "MiniLM must be symmetric (T9.2 step 6)"
    );
    let cache = std::env::var("KNOT_FASTEMBED_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/fastembed")
        });
    let embedder = Embedder::init_with_model(cache, choice)?;
    assert_eq!(embedder.dim(), 384, "AllMiniLML6V2 must be 384-dim");
    Ok(embedder)
}

/// One `embed` call per source file (incremental-indexing simulation, T9.2).
fn embed_per_file(
    embedder: &mut Embedder,
    entities: Vec<ParsedEntity>,
    batch_size: usize,
) -> Result<Vec<knot::models::EmbeddedEntity>> {
    let mut by_file: BTreeMap<String, Vec<ParsedEntity>> = BTreeMap::new();
    for entity in entities {
        by_file
            .entry(entity.file_path.clone())
            .or_default()
            .push(entity);
    }
    let mut out = Vec::new();
    for (_, group) in by_file {
        out.extend(embedder.embed(group, batch_size)?);
    }
    Ok(out)
}

/// Tier-1 chunks + file vectors, embedded with `embed_query` (symmetric, T9.2).
fn embed_chunks(
    embedder: &mut Embedder,
    cfg: &IndexConfig,
    tier1: &[Tier1File],
    table: &EntityTable,
) -> Result<(Vec<Record>, u64)> {
    let started = Instant::now();
    let mut out = Vec::new();
    for file in tier1 {
        if let Some(only) = &cfg.only_file
            && only != &file.rel
        {
            continue;
        }
        for chunk in chunk::chunk_file(&file.rel, &file.source) {
            let vector = embedder.embed_query(&chunk.text)?;
            out.push(chunk_record(&cfg.repo, &file.rel, chunk, vector, table));
        }
    }
    Ok((out, started.elapsed().as_millis() as u64))
}

fn chunk_record(
    repo: &str,
    rel: &str,
    chunk: Chunk,
    vector: Vec<f32>,
    _table: &EntityTable,
) -> Record {
    Record {
        line: ShardLine {
            id: shard::chunk_uuid(repo, rel, chunk.kind, chunk.l0, chunk.l1).to_string(),
            kind: chunk.kind.to_owned(),
            name: chunk.name.clone(),
            fqn: rel.to_owned(),
            lang: String::new(),
            l0: chunk.l0,
            l1: chunk.l1,
            col: None,
            sig: None,
            h: shard::text_hash(&chunk.text),
            v: None,
            q: None,
            s: None,
        },
        vector,
    }
}

fn locate_col(root: &Path, entity: &ParsedEntity) -> Option<u32> {
    let source = std::fs::read_to_string(root.join(&entity.file_path)).ok()?;
    position::locate_name(
        &source,
        &entity.name,
        entity.start_line,
        entity.end_line,
        crate::common::Lang::Rust, // the scoring keywords only affect ranking
    )
    .map(|pos| pos.col_utf16)
}

fn write_all_variants(
    out_dir: &Path,
    cfg: &IndexConfig,
    by_file: &BTreeMap<String, Vec<Record>>,
    stats: &mut IndexStats,
) -> Result<()> {
    let mut i8fx_clip = 0.0;
    let mut i8fx_n = 0;
    for variant in VARIANTS {
        let mut lines_by_file: BTreeMap<String, Vec<ShardLine>> = BTreeMap::new();
        for (file, records) in by_file {
            if let Some(only) = &cfg.only_file
                && only != file
            {
                continue;
            }
            let mut lines = Vec::new();
            for record in records {
                let (fields, enc_stats) = encode::encode(*variant, &record.vector);
                if *variant == Variant::I8fxB64 {
                    i8fx_clip += enc_stats.clip_rate;
                    i8fx_n += 1;
                }
                lines.push(record.line.clone().with_fields(&fields));
            }
            lines_by_file.insert(file.clone(), lines);
        }
        let totals = if cfg.only_file.is_some() {
            for (file, lines) in &mut lines_by_file {
                shard::write_shard(
                    &out_dir.join(variant.id()).join(format!("{file}.jsonl")),
                    lines,
                )?;
            }
            shard::rebuild_monolith(out_dir, *variant)?;
            shard::ShardBytes::default()
        } else {
            shard::write_variant(out_dir, *variant, &lines_by_file)?
        };
        stats
            .shard_raw_bytes
            .insert(variant.id().to_owned(), totals.raw);
        stats
            .shard_files
            .insert(variant.id().to_owned(), totals.shards);
    }
    if i8fx_n > 0 {
        stats.i8fx_clip_rate = i8fx_clip / i8fx_n as f64;
    }

    // Binary cache over the f32 records in shard order (T9.4).
    if cfg.only_file.is_some() {
        return Ok(());
    }
    let mut ids = Vec::new();
    let mut matrix = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for (file, records) in by_file {
        let mut records = records.clone();
        records.sort_by(|a, b| a.line.sort_key().cmp(&b.line.sort_key()));
        for record in records {
            ids.push(record.line.id.clone());
            matrix.extend_from_slice(&record.vector);
            files.push(file.clone());
        }
    }
    shard::write_binary_cache(out_dir, &ids, &matrix, 384)?;
    Ok(())
}
