//! Deterministic JSONL shards, monoliths and the binary cache (T9.4).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::vec::encode::{self, Variant, VectorFields};

/// One shard record — serialised with a struct so key order is fixed (T9.4):
/// `id, kind, name, fqn, lang, l0, l1, col, sig, h, v, q, s`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShardLine {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub fqn: String,
    pub lang: String,
    pub l0: usize,
    pub l1: usize,
    pub col: Option<u32>,
    pub sig: Option<String>,
    /// First 16 hex chars of `sha256(embed_text)`.
    pub h: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<encode::VectorValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s: Option<f64>,
}

impl ShardLine {
    /// Deterministic sort key `(l0, col, kind, name, fqn)` (T9.4).
    pub fn sort_key(&self) -> (usize, u32, &str, &str, &str) {
        (
            self.l0,
            self.col.unwrap_or(u32::MAX),
            self.kind.as_str(),
            self.name.as_str(),
            self.fqn.as_str(),
        )
    }

    /// Attach the encoded vector fields of one variant.
    pub fn with_fields(mut self, fields: &VectorFields) -> Self {
        self.v = fields.v.clone();
        self.q = fields.q.clone();
        self.s = fields.s;
        self
    }
}

/// Deterministic id for a tier-1 chunk/file record (T9.4).
pub fn chunk_uuid(repo: &str, rel: &str, kind: &str, l0: usize, l1: usize) -> Uuid {
    let identity = format!("{repo}:{rel}:{kind}:{l0}-{l1}");
    Uuid::new_v5(&knot::models::NAMESPACE_KNOT, identity.as_bytes())
}

/// `sha256(text)` truncated to 16 hex chars (T9.4 `h` field).
pub fn text_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Write one shard atomically (`*.tmp` + rename), lines sorted (T9.4).
pub fn write_shard(path: &Path, lines: &mut [ShardLine]) -> Result<usize> {
    lines.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let mut out = String::new();
    for line in lines.iter() {
        out.push_str(&serde_json::to_string(line)?);
        out.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(out.as_bytes())?;
        file.flush()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(out.len())
}

/// Read one shard back.
pub fn read_shard(path: &Path) -> Result<Vec<ShardLine>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        out.push(
            serde_json::from_str(line).with_context(|| format!("parsing {}", path.display()))?,
        );
    }
    Ok(out)
}

/// Per-shard byte accounting for the size measurements (T9.5).
#[derive(Debug, Clone, Copy, Default)]
pub struct ShardBytes {
    pub raw: u64,
    pub shards: u64,
}

/// Write every shard of one variant plus its monolith (T9.4).
///
/// `by_file` maps repo-relative source path → its records (already tagged).
pub fn write_variant(
    out_root: &Path,
    variant: Variant,
    by_file: &BTreeMap<String, Vec<ShardLine>>,
) -> Result<ShardBytes> {
    let variant_dir = out_root.join(variant.id());
    let mut totals = ShardBytes::default();
    let mut monolith: Vec<ShardLine> = Vec::new();
    for (rel, lines) in by_file {
        let mut lines = lines.clone();
        let bytes = write_shard(&variant_dir.join(format!("{rel}.jsonl")), &mut lines)?;
        totals.raw += bytes as u64;
        totals.shards += 1;
        monolith.extend(lines);
    }
    // Monolith: one file, records grouped by source file order (git-measure only).
    monolith.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let monolith_dir = out_root.join(format!("{}-monolith", variant.id()));
    std::fs::create_dir_all(&monolith_dir)
        .with_context(|| format!("creating {}", monolith_dir.display()))?;
    let mut out = String::new();
    for line in &monolith {
        out.push_str(&serde_json::to_string(line)?);
        out.push('\n');
    }
    let tmp = monolith_dir.join("index.jsonl.tmp");
    std::fs::write(&tmp, &out)?;
    std::fs::rename(tmp, monolith_dir.join("index.jsonl"))?;
    Ok(totals)
}

/// Binary cache: `KRAG` header + f32 LE matrix + `ids.jsonl` (T9.4).
pub fn write_binary_cache(
    out_root: &Path,
    ids: &[String],
    matrix: &[f32],
    dim: usize,
) -> Result<()> {
    let cache = out_root.join("cache");
    std::fs::create_dir_all(&cache)?;
    let mut header: Vec<u8> = Vec::new();
    header.extend_from_slice(b"KRAG");
    header.extend_from_slice(&1u32.to_le_bytes()); // version
    header.extend_from_slice(&(dim as u32).to_le_bytes());
    header.extend_from_slice(&(ids.len() as u32).to_le_bytes());
    let mut body = header;
    for value in matrix {
        body.extend_from_slice(&value.to_le_bytes());
    }
    let tmp = cache.join("vectors.bin.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(tmp, cache.join("vectors.bin"))?;
    std::fs::write(cache.join("ids.jsonl"), {
        let mut out = String::new();
        for id in ids {
            out.push_str(&serde_json::to_string(id)?);
            out.push('\n');
        }
        out
    })?;
    Ok(())
}

/// Load the binary cache back into a matrix.
pub fn read_binary_cache(out_root: &Path) -> Result<(Vec<String>, Vec<f32>, usize)> {
    let cache = out_root.join("cache");
    let body = std::fs::read(cache.join("vectors.bin"))?;
    if body.len() < 16 || &body[..4] != b"KRAG" {
        anyhow::bail!("bad vectors.bin header");
    }
    let dim = u32::from_le_bytes([body[8], body[9], body[10], body[11]]) as usize;
    let count = u32::from_le_bytes([body[12], body[13], body[14], body[15]]) as usize;
    let payload = &body[16..];
    if payload.len() != count * dim * 4 {
        anyhow::bail!("vectors.bin size mismatch");
    }
    let (chunks, _) = payload.as_chunks::<4>();
    let matrix: Vec<f32> = chunks.iter().map(|c| f32::from_le_bytes(*c)).collect();
    let ids_text = std::fs::read_to_string(cache.join("ids.jsonl"))?;
    let ids: Vec<String> = ids_text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|_| line.trim_matches('"').to_owned())
        })
        .collect();
    Ok((ids, matrix, dim))
}

/// Rebuild one variant's monolith from its existing per-file shards (T9.6).
pub fn rebuild_monolith(out_root: &Path, variant: Variant) -> Result<()> {
    let dir = out_root.join(variant.id());
    let mut all = Vec::new();
    let mut shard_files: Vec<PathBuf> = Vec::new();
    collect_jsonl(&dir, &mut shard_files)?;
    shard_files.sort();
    for path in &shard_files {
        all.extend(read_shard(path)?);
    }
    all.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let monolith_dir = out_root.join(format!("{}-monolith", variant.id()));
    std::fs::create_dir_all(&monolith_dir)?;
    let mut out = String::new();
    for line in &all {
        out.push_str(&serde_json::to_string(line)?);
        out.push('\n');
    }
    let tmp = monolith_dir.join("index.jsonl.tmp");
    std::fs::write(&tmp, &out)?;
    std::fs::rename(tmp, monolith_dir.join("index.jsonl"))?;
    Ok(())
}

pub fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}
