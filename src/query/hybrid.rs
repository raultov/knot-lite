//! The hybrid ranking of `search_hybrid_context` (P5-D7, §6.3.1): recall pool
//! → root coverage → knot's kind-aware re-rank (called, not copied) →
//! weighted reciprocal-rank fusion with the BM25 order.

use std::collections::HashMap;

use knot::cli_tools::search_hybrid_context::rank::rerank;
use serde_json::{Value, json};

use super::coverage::{Coverage, annotate};
use super::fusion::fuse;
use super::pool::{CHANNEL_PREFIX, PoolRow, Recall};
use crate::index::load::LoadedIndex;
use crate::lexicon::tokenize::query_terms;

/// Pool index carried through the re-rank's JSON rows (never serialised out).
const POOL_INDEX_FIELD: &str = "_kl";

/// One ranked record.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    /// Record position.
    pub position: usize,
    /// Fused score; `None` for a leading name-prefix slot (not re-ranked).
    pub score: Option<f64>,
    /// Cosine with the query.
    pub cosine: f32,
    /// BM25, when the record contains a query term.
    pub keyword: Option<f32>,
    /// Channels that produced it, the first one first.
    pub channels: Vec<&'static str>,
}

/// What the ranking needs.
pub struct HybridInput<'a> {
    /// The loaded index.
    pub index: &'a LoadedIndex,
    /// The query text.
    pub query: &'a str,
    /// The expanded kind filter.
    pub kinds: &'a [String],
    /// Cosine of every eligible record, by position (`None` = filtered out).
    pub cosine: &'a [Option<f32>],
    /// Results wanted.
    pub max_results: usize,
    /// Weight of the keyword order in the fusion.
    pub keyword_weight: f64,
}

/// The ranked results and what the response reports about the ranking.
#[derive(Debug)]
pub struct HybridOutcome {
    /// At most `max_results` records, best first.
    pub ranked: Vec<Ranked>,
    /// Candidates the re-rank ordered.
    pub pool: usize,
    /// Whether the keyword channel read the stored lexicon (else names only).
    pub lexicon_full: bool,
}

/// Rank the eligible records for `input.query`.
pub fn rank_hybrid(input: &HybridInput<'_>) -> HybridOutcome {
    let index = input.index;
    let aux = index.search_aux();
    let keyword = aux.lexicon.scores(&query_terms(input.query), |at| {
        input.cosine.get(at).copied().flatten().is_some()
    });
    let pool = Recall {
        index,
        aux,
        query: input.query,
        kinds: input.kinds,
        cosine: input.cosine,
        keyword: &keyword,
        max_results: input.max_results,
    }
    .collect();
    let positions: Vec<usize> = pool.rows.iter().map(|row| row.position).collect();
    let coverage = annotate(index, aux, &positions, &pool.seeds);
    let rows: Vec<Value> = pool
        .rows
        .iter()
        .enumerate()
        .map(|(at, row)| {
            let evidence = coverage.as_ref().map(|all| all[at]);
            pool_row_json(input, row, evidence, at)
        })
        .collect();
    let order: Vec<usize> = rerank(rows, input.query)
        .iter()
        .filter_map(|row| row.get(POOL_INDEX_FIELD).and_then(Value::as_u64))
        .filter_map(|at| usize::try_from(at).ok())
        .collect();
    let keyword_of_rows: Vec<Option<f32>> = positions
        .iter()
        .map(|at| keyword.get(at).copied())
        .collect();
    let mut ranked: Vec<Ranked> = pool
        .lead
        .iter()
        .map(|&at| ranked_row(input, &keyword, at, None, vec![CHANNEL_PREFIX]))
        .collect();
    ranked.extend(
        fuse(&order, &keyword_of_rows, input.keyword_weight)
            .into_iter()
            .map(|(row, score)| {
                let row = &pool.rows[row];
                ranked_row(
                    input,
                    &keyword,
                    row.position,
                    Some(score),
                    row.channels.clone(),
                )
            }),
    );
    ranked.truncate(input.max_results);
    HybridOutcome {
        ranked,
        pool: pool.rows.len(),
        lexicon_full: aux.lexicon_full,
    }
}

fn ranked_row(
    input: &HybridInput<'_>,
    keyword: &HashMap<usize, f32>,
    position: usize,
    score: Option<f64>,
    channels: Vec<&'static str>,
) -> Ranked {
    Ranked {
        position,
        score,
        cosine: input.cosine[position].unwrap_or(0.0),
        keyword: keyword.get(&position).copied(),
        channels,
    }
}

/// The row knot's `rerank` reads (`rank.rs:617-659`): identity, kind, path,
/// cosine as `score`, the coverage annotation and the test-context flag.
fn pool_row_json(
    input: &HybridInput<'_>,
    row: &PoolRow,
    coverage: Option<Coverage>,
    at: usize,
) -> Value {
    let record = &input.index.records[row.position];
    let line = &record.line;
    let mut value = json!({
        "uuid": line.id,
        "name": line.name,
        "kind": line.kind,
        "file_path": record.file,
        "fqn": line.fqn,
        "start_line": line.l0,
        "score": f64::from(input.cosine[row.position].unwrap_or(0.0)),
        "is_test_context": line.tc,
        "_channel": row.channels[0],
        POOL_INDEX_FIELD: at,
    });
    if let (Some(coverage), Some(object)) = (coverage, value.as_object_mut()) {
        object.insert("caller_roots".to_owned(), json!(coverage.direct));
        object.insert(
            "caller_roots_transitive".to_owned(),
            json!(coverage.transitive),
        );
        object.insert("caller_root_total".to_owned(), json!(coverage.total));
        object.insert("caller_out_degree".to_owned(), json!(coverage.out_degree));
        object.insert("caller_superseded".to_owned(), json!(coverage.superseded));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twelve hand-made pool rows; their order through knot 1.11.3's `rerank`
    /// is pinned, so a knot bump that changes the ranking breaks this test on
    /// purpose (P5-D1, P2-D1 spirit).
    #[test]
    fn rerank_parity_pinned_order() {
        let row = |uuid: &str, name: &str, kind: &str, file: &str, score: f64| {
            json!({"uuid": uuid, "name": name, "kind": kind, "file_path": file,
                   "fqn": format!("m::{name}"), "start_line": 1, "score": score,
                   "is_test_context": false, "_channel": "cosine"})
        };
        let mut rows = vec![
            row("01", "Overview", "markdown_section", "docs/guide.md", 0.62),
            row("02", "resolve_edges", "rust_function", "src/graph.rs", 0.55),
            row(
                "03",
                "test_resolve_edges",
                "rust_function",
                "tests/graph.rs",
                0.60,
            ),
            row("04", "EdgeResolver", "rust_struct", "src/graph.rs", 0.50),
            row("05", "edges", "config_property", "config.yaml", 0.58),
            row("06", "run.sh", "chunk", "scripts/run.sh", 0.57),
            row("07", "load", "rust_function", "src/load.rs", 0.40),
            row("08", "resolve", "rust_method", "src/graph.rs", 0.45),
            row("09", "Graph", "rust_struct", "src/graph.rs", 0.35),
            row("10", "Install", "markdown_section", "README.md", 0.30),
            row("11", "helper", "rust_function", "src/util.rs", 0.20),
            row("12", "edge_count", "rust_function", "src/graph.rs", 0.52),
        ];
        rows[6]["caller_roots"] = json!(3);
        rows[6]["caller_root_total"] = json!(4);
        rows[6]["caller_out_degree"] = json!(5);
        let order: Vec<String> = rerank(rows, "resolve edges")
            .iter()
            .map(|row| row["uuid"].as_str().expect("uuid").to_owned())
            .collect();
        assert_eq!(
            order,
            [
                "07", "08", "02", "05", "12", "03", "04", "06", "09", "01", "11", "10"
            ]
        );
    }
}
