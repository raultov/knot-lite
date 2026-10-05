//! `search_hybrid_context`: cosine top-k over the loaded vectors (P2-D10).
//!
//! The name is kept for surface parity with knot; there is no keyword channel,
//! no kind-aware re-ranking and no graph boost.

use serde::Serialize;
use serde_json::Value;

use super::filters::PathPrefix;
use super::render::{Column, RenderDoc, RenderSection, str_at, u64_at};
use crate::config::OutputFormat;
use crate::index::load::{LoadedIndex, Matrix};
use knot::cli_tools::kinds::kinds_allow;

/// Schema id of the search document.
pub const SEARCH_SCHEMA_V1: &str = "knot-lite/search/v1";
/// Default `max_results`.
pub const DEFAULT_MAX_RESULTS: usize = 5;
/// Largest accepted `max_results`.
pub const MAX_RESULTS_CAP: usize = 100;

const PURE_VECTOR_NOTE: &str =
    "Pure vector ranking (cosine over int8 vectors); no keyword channel or re-ranking.";

/// A validated search request.
#[derive(Debug, Clone)]
pub struct SearchRequest<'a> {
    /// Query text (echoed in the response).
    pub query: &'a str,
    /// Results to return (already clamped).
    pub max_results: usize,
    /// Expanded kind filter; empty = every record, tier-1 included.
    pub kinds: &'a [String],
    /// Optional path prefix.
    pub path: Option<&'a PathPrefix>,
}

/// `index` object of the response.
#[derive(Debug, Clone, Serialize)]
pub struct SearchIndexInfo {
    /// Embedding model of the index.
    pub model: String,
    /// When the index was last built.
    pub updated_at: String,
}

/// `filters` object of the response.
#[derive(Debug, Clone, Serialize)]
pub struct SearchFilters {
    /// The expanded kinds (empty = no kind filter).
    pub kinds: Vec<String>,
    /// The normalised path prefix.
    pub path: Option<String>,
}

/// One ranked result.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// 1-based rank.
    pub rank: usize,
    /// Cosine similarity, rounded to 4 decimals.
    pub score: f64,
    /// Record id.
    pub id: String,
    /// Entity kind, or `chunk` / `file`.
    pub kind: String,
    /// Entity name (file name for tier-1 records).
    pub name: String,
    /// Fully qualified name (the path for tier-1 records).
    pub fqn: String,
    /// Repo-relative file.
    pub file: String,
    /// First line.
    pub l0: usize,
    /// Last line.
    pub l1: usize,
    /// Signature, when the entity has one.
    pub signature: Option<String>,
    /// Language label.
    pub lang: String,
    /// Incoming reference edges (heuristic).
    pub callers: usize,
}

/// The `knot-lite/search/v1` document (§9.2).
#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    /// [`SEARCH_SCHEMA_V1`].
    pub schema: &'static str,
    /// knot-lite version.
    pub knot_lite_version: &'static str,
    /// Repository name.
    pub repo: String,
    /// The query text.
    pub query: String,
    /// Always `vector` in 0.2.0.
    pub provenance: &'static str,
    /// The index the answer came from.
    pub index: SearchIndexInfo,
    /// Filters applied.
    pub filters: SearchFilters,
    /// Requested number of results.
    pub max_results: usize,
    /// Records that passed the filters (scored).
    pub candidates: usize,
    /// Ranked results.
    pub results: Vec<SearchHit>,
    /// Caveats for the reader.
    pub notes: Vec<&'static str>,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Top-`k` of `candidates` (matrix rows) by descending dot product with
/// `query`, as `(row, score)`. Ties go to the lower row (record order), so the
/// ranking is deterministic.
pub fn top_k(matrix: &Matrix, candidates: &[usize], query: &[f32], k: usize) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = candidates
        .iter()
        .filter_map(|&row| matrix.row(row).map(|vector| (row, dot(vector, query))))
        .collect();
    let keep = k.min(scored.len());
    if keep == 0 {
        return Vec::new();
    }
    let by_rank = |a: &(usize, f32), b: &(usize, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
    scored.select_nth_unstable_by(keep - 1, by_rank);
    scored.truncate(keep);
    scored.sort_by(by_rank);
    scored
}

/// Score every record that passes the filters against `query_vector` and
/// return the ranked response.
pub fn search_index(
    index: &LoadedIndex,
    repo: &str,
    request: &SearchRequest<'_>,
    query_vector: &[f32],
) -> SearchResponse {
    let candidates: Vec<usize> = (0..index.matrix.rows())
        .filter(|&row| passes(index, row, request))
        .collect();
    let results = top_k(
        &index.matrix,
        &candidates,
        query_vector,
        request.max_results,
    )
    .into_iter()
    .enumerate()
    .map(|(position, (row, score))| hit(index, position + 1, row, score))
    .collect();
    SearchResponse {
        schema: SEARCH_SCHEMA_V1,
        knot_lite_version: env!("CARGO_PKG_VERSION"),
        repo: repo.to_owned(),
        query: request.query.to_owned(),
        provenance: "vector",
        index: SearchIndexInfo {
            model: index.manifest.embedding.model.clone(),
            updated_at: index.manifest.updated_at.clone(),
        },
        filters: SearchFilters {
            kinds: request.kinds.to_vec(),
            path: request.path.map(|prefix| prefix.as_str().to_owned()),
        },
        max_results: request.max_results,
        candidates: candidates.len(),
        results,
        notes: vec![PURE_VECTOR_NOTE],
    }
}

fn passes(index: &LoadedIndex, row: usize, request: &SearchRequest<'_>) -> bool {
    let record = &index.records[index.matrix.record[row]];
    kinds_allow(request.kinds, &record.line.kind)
        && request
            .path
            .is_none_or(|prefix| prefix.matches(&record.file))
}

fn hit(index: &LoadedIndex, rank: usize, row: usize, score: f32) -> SearchHit {
    let record = &index.records[index.matrix.record[row]];
    let line = &record.line;
    SearchHit {
        rank,
        score: (f64::from(score) * 1e4).round() / 1e4,
        id: line.id.clone(),
        kind: line.kind.clone(),
        name: line.name.clone(),
        fqn: line.fqn.clone(),
        file: record.file.clone(),
        l0: line.l0,
        l1: line.l1,
        signature: line.sig.clone(),
        lang: line.lang.clone(),
        callers: index.incoming.get(&line.id).map_or(0, Vec::len),
    }
}

/// Build the format-agnostic document for a `knot-lite/search/v1` value.
pub fn search_doc(value: &Value, format: OutputFormat) -> RenderDoc {
    let repo = str_at(value, &["repo"]).unwrap_or("-");
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let markdown = format == OutputFormat::Markdown;

    let mut summary = vec![
        format!("query: {}", str_at(value, &["query"]).unwrap_or("")),
        format!(
            "index: {} (updated {})",
            str_at(value, &["index", "model"]).unwrap_or("-"),
            str_at(value, &["index", "updated_at"]).unwrap_or("-")
        ),
        format!(
            "showing {} of {} candidates (max_results {})",
            results.len(),
            u64_at(value, &["candidates"]).unwrap_or(0),
            u64_at(value, &["max_results"]).unwrap_or(0)
        ),
    ];
    summary.extend(filter_lines(value));

    let rows = results
        .iter()
        .map(|hit| search_row(hit, markdown))
        .collect();
    let mut notes: Vec<String> = Vec::new();
    if markdown {
        notes.extend(results.iter().filter_map(signature_note));
    }
    notes.extend(string_array(value, "notes"));

    RenderDoc {
        title: format!("knot-lite search — repository '{repo}'"),
        summary,
        sections: vec![RenderSection {
            heading: None,
            columns: vec![
                Column::right("#"),
                Column::right("SCORE"),
                Column::left("KIND"),
                Column::left("NAME"),
                Column::left("LOCATION"),
                Column::right("CALLERS"),
            ],
            rows,
        }],
        notes,
    }
}

fn filter_lines(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let kinds = string_array(value, "filters.kinds");
    if !kinds.is_empty() {
        lines.push(format!("kinds: {}", kinds.join(", ")));
    }
    if let Some(path) = str_at(value, &["filters", "path"]) {
        lines.push(format!("path: {path}"));
    }
    lines
}

/// Strings of the array at `key` (`a.b` descends into objects).
fn string_array(value: &Value, key: &str) -> Vec<String> {
    key.split('.')
        .try_fold(value, |acc, part| acc.get(part))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn search_row(hit: &Value, markdown: bool) -> Vec<String> {
    let name = str_at(hit, &["name"]).unwrap_or("-");
    let name_cell = match (markdown, str_at(hit, &["fqn"])) {
        (true, Some(fqn)) if fqn != name => format!("{name} — {fqn}"),
        _ => name.to_owned(),
    };
    vec![
        u64_at(hit, &["rank"]).unwrap_or(0).to_string(),
        format!(
            "{:.4}",
            hit.get("score").and_then(Value::as_f64).unwrap_or(0.0)
        ),
        str_at(hit, &["kind"]).unwrap_or("-").to_owned(),
        name_cell,
        format!(
            "{}:{}-{}",
            str_at(hit, &["file"]).unwrap_or("-"),
            u64_at(hit, &["l0"]).unwrap_or(0),
            u64_at(hit, &["l1"]).unwrap_or(0)
        ),
        u64_at(hit, &["callers"]).unwrap_or(0).to_string(),
    ]
}

/// `#1 \`signature\`` for a result that has one (markdown notes).
fn signature_note(hit: &Value) -> Option<String> {
    let signature = str_at(hit, &["signature"])?;
    Some(format!(
        "#{} `{}`",
        u64_at(hit, &["rank"]).unwrap_or(0),
        signature.replace('`', "'")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix() -> Matrix {
        // Three unit-ish rows: x-axis, y-axis, and a mix.
        Matrix {
            dim: 2,
            data: vec![1.0, 0.0, 0.0, 1.0, 0.6, 0.8],
            record: vec![0, 1, 2],
        }
    }

    #[test]
    fn top_k_orders_by_score() {
        let top = top_k(&matrix(), &[0, 1, 2], &[1.0, 0.0], 3);
        let rows: Vec<usize> = top.iter().map(|(row, _)| *row).collect();
        assert_eq!(rows, vec![0, 2, 1]);
        assert!(top.windows(2).all(|pair| pair[0].1 >= pair[1].1));
    }

    #[test]
    fn top_k_on_subset_and_oversized_k() {
        let m = matrix();
        let subset = top_k(&m, &[1, 2], &[1.0, 0.0], 99);
        assert_eq!(
            subset.iter().map(|(row, _)| *row).collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert!(top_k(&m, &[0, 1, 2], &[1.0, 0.0], 0).is_empty());
        assert!(top_k(&m, &[], &[1.0, 0.0], 3).is_empty());
        assert!(top_k(&Matrix::default(), &[0], &[1.0], 3).is_empty());
        assert!(
            top_k(&m, &[7], &[1.0, 0.0], 3).is_empty(),
            "out-of-range rows are skipped"
        );
        assert_eq!(top_k(&m, &[0, 1, 2], &[1.0, 0.0], 1).len(), 1);
    }

    #[test]
    fn top_k_ties_go_to_the_lower_row() {
        let m = Matrix {
            dim: 1,
            data: vec![1.0; 6],
            record: (0..6).collect(),
        };
        let top = top_k(&m, &[5, 4, 3, 2, 1, 0], &[1.0], 3);
        assert_eq!(
            top.iter().map(|(row, _)| *row).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn score_rounding_has_four_decimals() {
        let rounded = (f64::from(0.123_456_f32) * 1e4).round() / 1e4;
        assert!((rounded - 0.1235).abs() < 1e-12, "{rounded}");
    }

    fn sample() -> Value {
        serde_json::json!({
            "schema": SEARCH_SCHEMA_V1, "repo": "demo", "query": "find me",
            "index": {"model": "M", "updated_at": "2026-10-05T10:00:00Z"},
            "filters": {"kinds": ["rust_function"], "path": "src"},
            "max_results": 5, "candidates": 40,
            "results": [{
                "rank": 1, "score": 0.5, "id": "i", "kind": "rust_function", "name": "run",
                "fqn": "m::run", "file": "src/a.rs", "l0": 3, "l1": 9,
                "signature": "(x: u8)", "lang": "rust", "callers": 2
            }],
            "notes": ["a note"]
        })
    }

    #[test]
    fn doc_for_table_has_plain_name_and_no_signature_notes() {
        let doc = search_doc(&sample(), OutputFormat::Table);
        assert_eq!(doc.title, "knot-lite search — repository 'demo'");
        assert!(
            doc.summary
                .contains(&"showing 1 of 40 candidates (max_results 5)".to_owned())
        );
        assert!(doc.summary.contains(&"kinds: rust_function".to_owned()));
        assert!(doc.summary.contains(&"path: src".to_owned()));
        assert_eq!(
            doc.sections[0].rows[0],
            vec!["1", "0.5000", "rust_function", "run", "src/a.rs:3-9", "2"]
        );
        assert_eq!(doc.notes, vec!["a note"]);
    }

    #[test]
    fn doc_for_markdown_has_fqn_names_and_signature_bullets() {
        let doc = search_doc(&sample(), OutputFormat::Markdown);
        assert_eq!(doc.sections[0].rows[0][3], "run — m::run");
        assert_eq!(doc.notes, vec!["#1 `(x: u8)`", "a note"]);
    }

    #[test]
    fn doc_without_results_has_an_empty_section() {
        let mut value = sample();
        value["results"] = serde_json::json!([]);
        let doc = search_doc(&value, OutputFormat::Table);
        assert!(doc.sections[0].rows.is_empty());
    }
}
