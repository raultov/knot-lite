//! `search_hybrid_context`: hybrid ranking over the loaded index (P5-D7) —
//! a recall pool (cosine, definitions, names, BM25 keywords, callers of the
//! best roots), knot's kind-aware re-rank and a reciprocal-rank fusion with the
//! keyword order — or the 0.4.x cosine top-k with `KNOT_LITE_SEARCH=vector`.

use serde::Serialize;
use serde_json::Value;

use super::files::path_line;
use super::filters::{PathFilter, PathMode};
use super::hybrid::{HybridInput, Ranked, rank_hybrid};
use super::pool::CHANNEL_COSINE;
use super::render::{Column, RenderDoc, RenderSection, str_at, u64_at};
use crate::config::{OutputFormat, SearchMode};
use crate::index::load::{LoadedIndex, Matrix};
use crate::index::search_aux::NAMES_ONLY_NOTE;
use crate::utils::top_scored;
use knot::cli_tools::kinds::kinds_allow;

/// Schema id of the search document (P5-D3: `score` is the fused score).
pub const SEARCH_SCHEMA_V2: &str = "knot-lite/search/v2";
/// Default `max_results`.
pub const DEFAULT_MAX_RESULTS: usize = 5;
/// Largest accepted `max_results`.
pub const MAX_RESULTS_CAP: usize = 100;

const PURE_VECTOR_NOTE: &str =
    "Pure vector ranking (cosine over int8 vectors); no keyword channel or re-ranking.";
const HYBRID_NOTE: &str = "Hybrid ranking: cosine + BM25 keyword channel, knot's kind-aware re-rank and call-graph entry-point signal (weighted reciprocal-rank fusion).";

/// A validated search request.
#[derive(Debug, Clone)]
pub struct SearchRequest<'a> {
    /// Query text (echoed in the response).
    pub query: &'a str,
    /// Results to return (already clamped).
    pub max_results: usize,
    /// Expanded kind filter; empty = every record, tier-1 included.
    pub kinds: &'a [String],
    /// Optional path filter (prefix or glob).
    pub path: Option<&'a PathFilter>,
    /// Hybrid or vector ranking.
    pub mode: SearchMode,
    /// Weight of the keyword order in the fusion (hybrid only).
    pub keyword_weight: f64,
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
    /// The normalised path filter.
    pub path: Option<String>,
    /// How `path` matched (`prefix` or `glob`), when one was given.
    pub path_mode: Option<PathMode>,
}

/// `ranking` object of the response.
#[derive(Debug, Clone, Serialize)]
pub struct SearchRanking {
    /// `hybrid` or `vector`.
    pub mode: SearchMode,
    /// Weight of the keyword order (`null` in vector mode).
    pub keyword_weight: Option<f64>,
    /// Candidates the re-rank ordered (`null` in vector mode).
    pub pool: Option<usize>,
    /// `full` (stored lexicon) or `names_only` (`null` in vector mode).
    pub lexicon: Option<&'static str>,
}

/// One ranked result.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// 1-based rank.
    pub rank: usize,
    /// Fused score (the cosine in vector mode), 4 decimals; `null` for a
    /// leading name-prefix slot.
    pub score: Option<f64>,
    /// Cosine similarity, 4 decimals.
    pub cosine: f64,
    /// BM25 score, 4 decimals; `null` when the record has no query term.
    pub keyword: Option<f64>,
    /// Recall channels that produced the result.
    pub channels: Vec<&'static str>,
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

/// The `knot-lite/search/v2` document (§10.1).
#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    /// [`SEARCH_SCHEMA_V2`].
    pub schema: &'static str,
    /// knot-lite version.
    pub knot_lite_version: &'static str,
    /// Repository name.
    pub repo: String,
    /// The query text.
    pub query: String,
    /// `hybrid` or `vector`.
    pub provenance: &'static str,
    /// How the results were ranked.
    pub ranking: SearchRanking,
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
    pub notes: Vec<String>,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Top-`k` of `candidates` (matrix rows) by descending dot product with
/// `query`, as `(row, score)`. Ties go to the lower row (record order), so the
/// ranking is deterministic.
pub fn top_k(matrix: &Matrix, candidates: &[usize], query: &[f32], k: usize) -> Vec<(usize, f32)> {
    let scored: Vec<(usize, f32)> = candidates
        .iter()
        .filter_map(|&row| matrix.row(row).map(|vector| (row, dot(vector, query))))
        .collect();
    top_scored(scored, k)
}

/// Rank the records that pass the filters for `query_vector` and return the
/// response.
pub fn search_index(
    index: &LoadedIndex,
    repo: &str,
    request: &SearchRequest<'_>,
    query_vector: &[f32],
) -> SearchResponse {
    let candidates: Vec<usize> = (0..index.matrix.rows())
        .filter(|&row| passes(index, row, request))
        .collect();
    let (ranked, ranking, ranking_note) = match request.mode {
        SearchMode::Vector => vector_ranking(index, request, &candidates, query_vector),
        SearchMode::Hybrid => hybrid_ranking(index, request, &candidates, query_vector),
    };
    let results = ranked
        .into_iter()
        .enumerate()
        .map(|(at, ranked)| hit(index, at + 1, ranked))
        .collect();
    SearchResponse {
        schema: SEARCH_SCHEMA_V2,
        knot_lite_version: env!("CARGO_PKG_VERSION"),
        repo: repo.to_owned(),
        query: request.query.to_owned(),
        provenance: match request.mode {
            SearchMode::Vector => "vector",
            SearchMode::Hybrid => "hybrid",
        },
        ranking,
        index: SearchIndexInfo {
            model: index.manifest.embedding.model.clone(),
            updated_at: index.manifest.updated_at.clone(),
        },
        filters: SearchFilters {
            kinds: request.kinds.to_vec(),
            path: request.path.map(|filter| filter.as_str().to_owned()),
            path_mode: request.path.map(PathFilter::mode),
        },
        max_results: request.max_results,
        candidates: candidates.len(),
        results,
        notes: search_notes(index, request.path, ranking_note),
    }
}

/// The 0.4.x ranking: cosine top-k.
fn vector_ranking(
    index: &LoadedIndex,
    request: &SearchRequest<'_>,
    candidates: &[usize],
    query_vector: &[f32],
) -> (Vec<Ranked>, SearchRanking, Vec<&'static str>) {
    let ranked = top_k(&index.matrix, candidates, query_vector, request.max_results)
        .into_iter()
        .map(|(row, cosine)| Ranked {
            position: index.matrix.record[row],
            score: Some(f64::from(cosine)),
            cosine,
            keyword: None,
            channels: vec![CHANNEL_COSINE],
        })
        .collect();
    let ranking = SearchRanking {
        mode: SearchMode::Vector,
        keyword_weight: None,
        pool: None,
        lexicon: None,
    };
    (ranked, ranking, vec![PURE_VECTOR_NOTE])
}

/// The hybrid ranking over the cosine of every candidate.
fn hybrid_ranking(
    index: &LoadedIndex,
    request: &SearchRequest<'_>,
    candidates: &[usize],
    query_vector: &[f32],
) -> (Vec<Ranked>, SearchRanking, Vec<&'static str>) {
    let mut cosine: Vec<Option<f32>> = vec![None; index.records.len()];
    for &row in candidates {
        if let Some(vector) = index.matrix.row(row) {
            cosine[index.matrix.record[row]] = Some(dot(vector, query_vector));
        }
    }
    let outcome = rank_hybrid(&HybridInput {
        index,
        query: request.query,
        kinds: request.kinds,
        cosine: &cosine,
        max_results: request.max_results,
        keyword_weight: request.keyword_weight,
    });
    let ranking = SearchRanking {
        mode: SearchMode::Hybrid,
        keyword_weight: Some(request.keyword_weight),
        pool: Some(outcome.pool),
        lexicon: Some(if outcome.lexicon_full {
            "full"
        } else {
            "names_only"
        }),
    };
    let mut notes = vec![HYBRID_NOTE];
    if !outcome.lexicon_full {
        notes.push(NAMES_ONLY_NOTE);
    }
    (outcome.ranked, ranking, notes)
}

/// The ranking notes, plus the no-match note when no indexed file passes the path filter.
fn search_notes(
    index: &LoadedIndex,
    path: Option<&PathFilter>,
    ranking_notes: Vec<&'static str>,
) -> Vec<String> {
    let mut notes: Vec<String> = ranking_notes.into_iter().map(str::to_owned).collect();
    if let Some(filter) = path
        && !index.by_file.keys().any(|file| filter.matches(file))
    {
        notes.push(filter.no_match_note());
    }
    notes
}

fn passes(index: &LoadedIndex, row: usize, request: &SearchRequest<'_>) -> bool {
    let record = &index.records[index.matrix.record[row]];
    kinds_allow(request.kinds, &record.line.kind)
        && request
            .path
            .is_none_or(|prefix| prefix.matches(&record.file))
}

fn round4(value: f64) -> f64 {
    (value * 1e4).round() / 1e4
}

fn hit(index: &LoadedIndex, rank: usize, ranked: Ranked) -> SearchHit {
    let record = &index.records[ranked.position];
    let line = &record.line;
    SearchHit {
        rank,
        score: ranked.score.map(round4),
        cosine: round4(f64::from(ranked.cosine)),
        keyword: ranked.keyword.map(|bm25| round4(f64::from(bm25))),
        channels: ranked.channels,
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

/// Build the format-agnostic document for a `knot-lite/search/v2` value.
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
        ranking_line(value),
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
            caption: None,
            heading: None,
            columns: search_columns(markdown),
            rows,
        }],
        notes,
    }
}

/// `ranking: hybrid (pool 61, keyword weight 0.5, lexicon full)` or `ranking: vector`.
fn ranking_line(value: &Value) -> String {
    let mode = str_at(value, &["ranking", "mode"]).unwrap_or("-");
    match (
        u64_at(value, &["ranking", "pool"]),
        value
            .pointer("/ranking/keyword_weight")
            .and_then(Value::as_f64),
        str_at(value, &["ranking", "lexicon"]),
    ) {
        (Some(pool), Some(weight), Some(lexicon)) => {
            format!("ranking: {mode} (pool {pool}, keyword weight {weight}, lexicon {lexicon})")
        }
        _ => format!("ranking: {mode}"),
    }
}

/// `#`, `SCORE`, `COS`, `KW`, `KIND`, `NAME`, `LOCATION`, `CALLERS`; markdown
/// adds `CHANNELS`.
fn search_columns(markdown: bool) -> Vec<Column> {
    let mut columns = vec![
        Column::right("#"),
        Column::right("SCORE"),
        Column::right("COS"),
        Column::right("KW"),
        Column::left("KIND"),
        Column::left("NAME"),
        Column::left("LOCATION"),
        Column::right("CALLERS"),
    ];
    if markdown {
        columns.push(Column::left("CHANNELS"));
    }
    columns
}

fn filter_lines(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let kinds = string_array(value, "filters.kinds");
    if !kinds.is_empty() {
        lines.push(format!("kinds: {}", kinds.join(", ")));
    }
    if let Some(path) = str_at(value, &["filters", "path"]) {
        lines.push(path_line(path, str_at(value, &["filters", "path_mode"])));
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
    let number = |key: &str| {
        hit.get(key)
            .and_then(Value::as_f64)
            .map_or_else(|| "-".to_owned(), |n| format!("{n:.4}"))
    };
    let mut row = vec![
        u64_at(hit, &["rank"]).unwrap_or(0).to_string(),
        number("score"),
        number("cosine"),
        number("keyword"),
        str_at(hit, &["kind"]).unwrap_or("-").to_owned(),
        name_cell,
        format!(
            "{}:{}-{}",
            str_at(hit, &["file"]).unwrap_or("-"),
            u64_at(hit, &["l0"]).unwrap_or(0),
            u64_at(hit, &["l1"]).unwrap_or(0)
        ),
        u64_at(hit, &["callers"]).unwrap_or(0).to_string(),
    ];
    if markdown {
        row.push(string_array(hit, "channels").join(", "));
    }
    row
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
            "schema": SEARCH_SCHEMA_V2, "repo": "demo", "query": "find me",
            "provenance": "hybrid",
            "ranking": {"mode": "hybrid", "keyword_weight": 0.5, "pool": 61, "lexicon": "full"},
            "index": {"model": "M", "updated_at": "2026-10-05T10:00:00Z"},
            "filters": {"kinds": ["rust_function"], "path": "src"},
            "max_results": 5, "candidates": 40,
            "results": [{
                "rank": 1, "score": 0.0312, "cosine": 0.5, "keyword": null,
                "channels": ["cosine", "bridge"], "id": "i", "kind": "rust_function", "name": "run",
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
        assert!(
            doc.summary.contains(
                &"ranking: hybrid (pool 61, keyword weight 0.5, lexicon full)".to_owned()
            )
        );
        assert_eq!(
            doc.sections[0].rows[0],
            vec![
                "1",
                "0.0312",
                "0.5000",
                "-",
                "rust_function",
                "run",
                "src/a.rs:3-9",
                "2"
            ]
        );
        assert_eq!(doc.notes, vec!["a note"]);
    }

    #[test]
    fn doc_for_markdown_has_fqn_names_and_signature_bullets() {
        let doc = search_doc(&sample(), OutputFormat::Markdown);
        assert_eq!(doc.sections[0].rows[0][5], "run — m::run");
        assert_eq!(doc.sections[0].rows[0][8], "cosine, bridge");
        assert_eq!(doc.notes, vec!["#1 `(x: u8)`", "a note"]);
    }

    #[test]
    fn doc_without_results_has_an_empty_section() {
        let mut value = sample();
        value["results"] = serde_json::json!([]);
        let doc = search_doc(&value, OutputFormat::Table);
        assert!(doc.sections[0].rows.is_empty());
        value["ranking"] = serde_json::json!({"mode": "vector", "keyword_weight": null, "pool": null, "lexicon": null});
        assert!(
            search_doc(&value, OutputFormat::Table)
                .summary
                .contains(&"ranking: vector".to_owned())
        );
    }
}
