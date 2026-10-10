//! On-disk serialization for pre-computed LSP edges (`lsp-edges/<lang>/<caller rel>.jsonl`)
//! and targets (`lsp-targets/<lang>.jsonl`) — Appendix B of the Phase 3 plan.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Context as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::layout::IndexLayout;
use super::record_io::atomic_write;
use crate::capability::languages::LspLang;
use crate::lsp::target::OpCounts;

/// One line in an `lsp-edges/<lang>/<caller rel>.jsonl` file (§B.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspEdgeLine {
    /// Container entity id, or `None` for file-level callers.
    pub src: Option<String>,
    /// Target entity id.
    pub dst: String,
    /// Relationship word (`"CALLS"`, `"REFERENCES"`, `"IMPLEMENTS"`, `"EXTENDS"`, `"OVERRIDES"`).
    pub rel: String,
    /// Distinct call/reference site count.
    pub n: u32,
}

/// One line in an `lsp-targets/<lang>.jsonl` file (§B.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspTargetLine {
    /// Target entity id.
    pub id: String,
    /// Wire status word (§10.1).
    pub status: String,
    /// Pre-computation wall-clock milliseconds.
    pub ms: u64,
    /// In-repository location counts per operation.
    pub ops: OpCounts,
    /// Language server description from `serverInfo`, or `""`.
    pub server: String,
    /// Distinct caller containers the `references` probe found (`lsp-refs`
    /// selection; absent when not probed or with the `heuristic` selection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<usize>,
    /// Heuristic caller containers (`lsp-refs` selection only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<usize>,
}

/// Sort key for `LspEdgeLine`: `src` null-first, then `dst`, then `rel`.
pub fn edge_sort_key(edge: &LspEdgeLine) -> (Option<&str>, &str, &str) {
    (edge.src.as_deref(), edge.dst.as_str(), edge.rel.as_str())
}

/// Write edge shards for `lang` under `.knot-lite/lsp-edges/<lang>/`.
/// `shards` maps caller repo-relative path to its edge lines.
/// Returns `(edges_count, shards_count)`.
pub fn write_lsp_edge_shards(
    layout: &IndexLayout,
    lang: LspLang,
    shards: &BTreeMap<String, Vec<LspEdgeLine>>,
) -> anyhow::Result<(usize, usize)> {
    let lang_dir = layout.lsp_edges_lang_dir(lang);
    std::fs::create_dir_all(&lang_dir)
        .with_context(|| format!("creating directory {}", lang_dir.display()))?;

    let mut total_edges = 0;
    let total_shards = shards.len();

    for (rel, lines) in shards {
        let path = layout.lsp_edges_for(lang, rel)?;
        let mut sorted = lines.clone();
        sorted.sort_by(|a, b| edge_sort_key(a).cmp(&edge_sort_key(b)));

        let mut buf = Vec::new();
        for edge in &sorted {
            serde_json::to_writer(&mut buf, edge)?;
            buf.push(b'\n');
        }
        atomic_write(&path, &buf)?;
        total_edges += sorted.len();
    }

    Ok((total_edges, total_shards))
}

fn read_jsonl_records<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Vec<T>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    for (idx, line_res) in reader.lines().enumerate() {
        let line =
            line_res.with_context(|| format!("reading line {} of {}", idx + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let item: T = serde_json::from_str(&line)
            .with_context(|| format!("parsing line {} of {}", idx + 1, path.display()))?;
        lines.push(item);
    }
    Ok(lines)
}

/// Read an LSP edge shard file.
pub fn read_lsp_edge_shard(path: &Path) -> anyhow::Result<Vec<LspEdgeLine>> {
    read_jsonl_records(path)
}

/// Write target status file under `.knot-lite/lsp-targets/<lang>.jsonl`.
/// `targets` are sorted by `id`. Returns `targets_count`.
pub fn write_lsp_targets(
    layout: &IndexLayout,
    lang: LspLang,
    targets: &[LspTargetLine],
) -> anyhow::Result<usize> {
    let path = layout.lsp_targets_for(lang);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    let mut sorted = targets.to_vec();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let mut buf = Vec::new();
    for target in &sorted {
        serde_json::to_writer(&mut buf, target)?;
        buf.push(b'\n');
    }
    atomic_write(&path, &buf)?;
    Ok(sorted.len())
}

/// Read an LSP targets file.
pub fn read_lsp_targets(path: &Path) -> anyhow::Result<Vec<LspTargetLine>> {
    read_jsonl_records(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_edge_line_wire_order_is_pinned() {
        let line = LspEdgeLine {
            src: Some("c1".to_owned()),
            dst: "t1".to_owned(),
            rel: "CALLS".to_owned(),
            n: 3,
        };
        let json = serde_json::to_string(&line).expect("json");
        assert_eq!(json, r#"{"src":"c1","dst":"t1","rel":"CALLS","n":3}"#);

        let parsed: LspEdgeLine = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed, line);
    }

    #[test]
    fn lsp_target_line_wire_order_is_pinned() {
        let line = LspTargetLine {
            id: "t1".to_owned(),
            status: "ok".to_owned(),
            ms: 120,
            ops: OpCounts {
                incoming: Some(2),
                references: Some(5),
                implementation: None,
            },
            server: "rust-analyzer".to_owned(),
            refs: None,
            h: None,
        };
        let json = serde_json::to_string(&line).expect("json");
        assert_eq!(
            json,
            r#"{"id":"t1","status":"ok","ms":120,"ops":{"incoming":2,"references":5,"implementation":null},"server":"rust-analyzer"}"#
        );

        let parsed: LspTargetLine = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed, line);
    }
}
