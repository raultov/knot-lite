//! Phase 5 search gates H1, H2, M4r and M5r (PLAN_PHASE5_SEARCH_LSP_CI.md §11).
//! H3 (index cost) runs the release CLI like its P5.0 baseline
//! (`~/.local/share/knot-lite/measure/p5-h3.py`).
//!
//! The indexes must be built by this version (they carry the lexicon). The
//! gates call the library with an explicit mode and weight; `KNOT_LITE_SEARCH`
//! is never read here (P5-D12).

use super::*;
use crate::config::SearchMode;
use crate::query::fusion::KEYWORD_WEIGHT;
use crate::query::search::{SearchRequest, SearchResponse, search_index};

const REPOS: [&str; 3] = ["knot", "knot-site", "spring-ai"];
/// The keyword weights H1 tunes over (P5-D7).
const WEIGHTS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
/// Results asked per query (MRR@10 needs ten).
const DEPTH: usize = 10;
/// Query classes of Appendix A.
const CLASSES: [&str; 4] = ["behaviour", "identifier", "docs_config", "mixed"];

#[derive(Deserialize)]
struct HybridQuery {
    id: String,
    repo: String,
    class: String,
    text: String,
    expected: Vec<String>,
}

#[derive(Deserialize)]
struct HybridQueries {
    query: Vec<HybridQuery>,
}

impl HybridQuery {
    fn tuning(&self) -> bool {
        self.id[1..].parse::<u32>().expect("hNN id") % 2 == 1
    }
}

fn hybrid_queries() -> Vec<HybridQuery> {
    let text = std::fs::read_to_string("docs/measure/phase5_queries.toml").expect("query set");
    toml::from_str::<HybridQueries>(&text)
        .expect("parse query set")
        .query
}

/// Whether a result matches an expectation: `file#name`, a prefix ending in
/// `/`, or a file (Appendix A).
fn matches_expected(expected: &[String], file: &str, name: &str) -> bool {
    expected.iter().any(|entry| match entry.split_once('#') {
        Some((path, entity)) => path == file && entity == name,
        None if entry.ends_with('/') => file.starts_with(entry.as_str()),
        None => entry == file,
    })
}

/// 1-based rank of the first result that matches, among the first [`DEPTH`].
fn first_hit(response: &SearchResponse, expected: &[String]) -> Option<usize> {
    response
        .results
        .iter()
        .take(DEPTH)
        .position(|hit| matches_expected(expected, &hit.file, &hit.name))
        .map(|at| at + 1)
}

/// The sandbox indexes and one embedder per model.
struct Sandbox {
    indexes: HashMap<String, LoadedIndex>,
    embedders: HashMap<String, KnotEmbedder>,
}

impl Sandbox {
    fn load() -> Self {
        let mut sandbox = Self {
            indexes: HashMap::new(),
            embedders: HashMap::new(),
        };
        for repo in REPOS {
            sandbox.reload(repo);
        }
        sandbox
    }

    /// (Re)load the index of `repo` (a fresh `LoadedIndex`: no search structures yet).
    fn reload(&mut self, repo: &str) -> &LoadedIndex {
        let layout = IndexLayout::for_repo(&sandbox_path(repo));
        let index = load_index(&layout).expect("load (run knot-lite index first)");
        assert!(
            index.manifest.lexicon.is_some(),
            "{repo}: the index has no lexicon; re-index with this version"
        );
        let model = index.manifest.embedding.model.clone();
        self.embedders
            .entry(model.clone())
            .or_insert_with(|| KnotEmbedder::init(&model, &fastembed_cache_dir()).expect("model"));
        self.indexes.insert(repo.to_owned(), index);
        &self.indexes[repo]
    }

    fn embed(&mut self, repo: &str, text: &str) -> Vec<f32> {
        let model = self.indexes[repo].manifest.embedding.model.clone();
        self.embedders
            .get_mut(&model)
            .expect("embedder")
            .embed_query(text)
            .expect("embed query")
    }
}

fn search(
    index: &LoadedIndex,
    query: (&str, &[f32]),
    kinds: &[String],
    mode: SearchMode,
    weight: f64,
) -> SearchResponse {
    let request = SearchRequest {
        query: query.0,
        max_results: DEPTH,
        kinds,
        path: None,
        mode,
        keyword_weight: weight,
    };
    search_index(index, "measure", &request, query.1)
}

/// hit@1, hit@5 and MRR@10 of a set of first-hit ranks.
#[derive(Debug, Clone, Default, Serialize)]
struct Metrics {
    queries: usize,
    hit1: usize,
    hit5: usize,
    mrr10: f64,
}

impl Metrics {
    fn of<'a>(ranks: impl IntoIterator<Item = &'a Option<usize>>) -> Self {
        let mut metrics = Self::default();
        for rank in ranks {
            metrics.queries += 1;
            if let Some(rank) = *rank {
                metrics.hit1 += usize::from(rank == 1);
                metrics.hit5 += usize::from(rank <= 5);
                metrics.mrr10 += 1.0 / rank as f64;
            }
        }
        if metrics.queries > 0 {
            metrics.mrr10 /= metrics.queries as f64;
        }
        metrics
    }

    fn hit5_pct(&self) -> f64 {
        if self.queries == 0 {
            0.0
        } else {
            100.0 * self.hit5 as f64 / self.queries as f64
        }
    }
}

/// First-hit rank per query id.
type Ranks = BTreeMap<String, Option<usize>>;

fn evaluate(
    sandbox: &Sandbox,
    queries: &[&HybridQuery],
    vectors: &HashMap<String, Vec<f32>>,
    mode: SearchMode,
    weight: f64,
) -> Ranks {
    queries
        .iter()
        .map(|query| {
            let index = &sandbox.indexes[&query.repo];
            let response = search(index, (&query.text, &vectors[&query.id]), &[], mode, weight);
            (query.id.clone(), first_hit(&response, &query.expected))
        })
        .collect()
}

/// Metrics overall, per repository and per class.
fn breakdown(queries: &[&HybridQuery], ranks: &Ranks) -> serde_json::Value {
    let of = |keep: &dyn Fn(&HybridQuery) -> bool| {
        Metrics::of(queries.iter().filter(|q| keep(q)).map(|q| &ranks[&q.id]))
    };
    serde_json::json!({
        "all": of(&|_| true),
        "repo": REPOS.iter().map(|repo| (*repo, of(&|q| q.repo == *repo))).collect::<BTreeMap<_, _>>(),
        "class": CLASSES.iter().map(|class| (*class, of(&|q| q.class == *class))).collect::<BTreeMap<_, _>>(),
    })
}

/// The weight with the best tuning hit@5 (ties: MRR@10, then the smaller weight).
fn choose_weight(tuning: &[(f64, Metrics)]) -> f64 {
    let mut best = &tuning[0];
    for candidate in &tuning[1..] {
        let better = candidate.1.hit5 > best.1.hit5
            || (candidate.1.hit5 == best.1.hit5 && candidate.1.mrr10 > best.1.mrr10 + 1e-12);
        if better {
            best = candidate;
        }
    }
    best.0
}

#[test]
#[ignore = "H1: hybrid vs vector quality on the Phase 5 query set (indexed sandbox, fastembed model)"]
fn h1_hybrid_quality() {
    let all = hybrid_queries();
    let mut sandbox = Sandbox::load();
    let vectors: HashMap<String, Vec<f32>> = all
        .iter()
        .map(|q| (q.id.clone(), sandbox.embed(&q.repo, &q.text)))
        .collect();
    let tuning: Vec<&HybridQuery> = all.iter().filter(|q| q.tuning()).collect();
    let held_out: Vec<&HybridQuery> = all.iter().filter(|q| !q.tuning()).collect();

    let tuning_vector = evaluate(&sandbox, &tuning, &vectors, SearchMode::Vector, 0.0);
    let per_weight: Vec<(f64, Metrics, Ranks)> = WEIGHTS
        .iter()
        .map(|&weight| {
            let ranks = evaluate(&sandbox, &tuning, &vectors, SearchMode::Hybrid, weight);
            (weight, Metrics::of(ranks.values()), ranks)
        })
        .collect();
    let chosen = choose_weight(
        &per_weight
            .iter()
            .map(|(w, m, _)| (*w, m.clone()))
            .collect::<Vec<_>>(),
    );
    println!(
        "H1 tuning: vector hit@5 {}/{}",
        Metrics::of(tuning_vector.values()).hit5,
        tuning.len()
    );
    for (weight, metrics, _) in &per_weight {
        println!(
            "H1 tuning: w={weight} hit@5 {}/{} MRR@10 {:.3}",
            metrics.hit5, metrics.queries, metrics.mrr10
        );
    }
    println!("H1 chosen keyword weight: {chosen} (shipped KEYWORD_WEIGHT = {KEYWORD_WEIGHT})");

    // The held-out split is evaluated once, with the chosen weight only.
    let held_vector = evaluate(&sandbox, &held_out, &vectors, SearchMode::Vector, 0.0);
    let held_hybrid = evaluate(&sandbox, &held_out, &vectors, SearchMode::Hybrid, chosen);
    let report = serde_json::json!({
        "tuning": {
            "vector": breakdown(&tuning, &tuning_vector),
            "hybrid": per_weight.iter().map(|(w, _, ranks)| (w.to_string(), breakdown(&tuning, ranks))).collect::<BTreeMap<_, _>>(),
        },
        "chosen_weight": chosen,
        "held_out": {
            "vector": breakdown(&held_out, &held_vector),
            "hybrid": breakdown(&held_out, &held_hybrid),
            "ranks": held_out.iter().map(|q| (q.id.clone(), (held_vector[&q.id], held_hybrid[&q.id]))).collect::<BTreeMap<_, _>>(),
        },
    });
    write_gate("h1_hybrid_quality", &report);
    assert_h1(&held_out, &held_vector, &held_hybrid);
}

/// H1 pass rule on the held-out split: +10 points of hit@5 overall, no class
/// losing more than one query, no repository below vector.
fn assert_h1(held_out: &[&HybridQuery], vector: &Ranks, hybrid: &Ranks) {
    let hit5 = |ranks: &Ranks, keep: &dyn Fn(&HybridQuery) -> bool| {
        Metrics::of(held_out.iter().filter(|q| keep(q)).map(|q| &ranks[&q.id]))
    };
    let (v, h) = (hit5(vector, &|_| true), hit5(hybrid, &|_| true));
    println!(
        "H1 held-out hit@5: vector {:.1} %, hybrid {:.1} %",
        v.hit5_pct(),
        h.hit5_pct()
    );
    let mut failures = Vec::new();
    if h.hit5_pct() < v.hit5_pct() + 10.0 {
        failures.push(format!(
            "hit@5 {:.1} % < vector {:.1} % + 10",
            h.hit5_pct(),
            v.hit5_pct()
        ));
    }
    for class in CLASSES {
        let (v, h) = (
            hit5(vector, &|q| q.class == class),
            hit5(hybrid, &|q| q.class == class),
        );
        if h.hit5 + 1 < v.hit5 {
            failures.push(format!(
                "class {class}: hybrid {} < vector {} - 1",
                h.hit5, v.hit5
            ));
        }
    }
    for repo in REPOS {
        let (v, h) = (
            hit5(vector, &|q| q.repo == repo),
            hit5(hybrid, &|q| q.repo == repo),
        );
        if h.hit5 < v.hit5 {
            failures.push(format!(
                "repo {repo}: hybrid {} < vector {}",
                h.hit5, v.hit5
            ));
        }
    }
    assert!(failures.is_empty(), "H1 failed: {failures:?}");
}

#[test]
#[ignore = "M4r: the 19 Phase 0 entity queries through the hybrid ranking (indexed sandbox, fastembed model)"]
fn m4r_entity_retrieval_hybrid() {
    let text = std::fs::read_to_string("docs/spikes/phase0/queries/entities.toml").expect("read");
    let queries: QueriesFile = toml::from_str(&text).expect("parse");
    let mut sandbox = Sandbox::load();
    let mut per_repo: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut rows = Vec::new();
    for query in &queries.query {
        let vector = sandbox.embed(&query.repo, &query.text);
        let index = &sandbox.indexes[&query.repo];
        // The entity domain of M4: every kind but the tier-1 records.
        let kinds: Vec<String> = index
            .records
            .iter()
            .filter(|record| !record.line.is_tier1())
            .map(|record| record.line.kind.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let response = search(
            index,
            (&query.text, &vector),
            &kinds,
            SearchMode::Hybrid,
            KEYWORD_WEIGHT,
        );
        let hit = response
            .results
            .iter()
            .take(5)
            .any(|hit| is_expected_match(&hit.file, &query.expected));
        let entry = per_repo.entry(query.repo.clone()).or_default();
        entry.0 += usize::from(hit);
        entry.1 += 1;
        rows.push(serde_json::json!({"id": query.id, "repo": query.repo, "hit5": hit}));
    }
    write_gate(
        "m4r_entity_retrieval_hybrid",
        &serde_json::json!({"queries": rows, "per_repo": per_repo}),
    );
    for (repo, floor) in [("knot", 0.75), ("knot-site", 0.80), ("spring-ai", 0.833)] {
        let (hits, total) = per_repo[repo];
        println!("M4r {repo}: {hits}/{total}");
        assert!(
            hits as f64 / total as f64 >= floor - 1e-9,
            "M4r {repo}: {hits}/{total} < {floor}"
        );
    }
}

#[test]
#[ignore = "H2: hybrid search latency (indexed sandbox, fastembed model)"]
fn h2_hybrid_latency() {
    let all = hybrid_queries();
    let mut sandbox = Sandbox::load();
    let mut report = BTreeMap::new();
    for (repo, p95_ms, aux_ms) in [
        ("knot", 30, 1500),
        ("knot-site", 10, 1500),
        ("spring-ai", 150, 1500),
    ] {
        let queries: Vec<(String, Vec<f32>)> = all
            .iter()
            .filter(|q| q.repo == repo)
            .map(|q| (q.text.clone(), sandbox.embed(repo, &q.text)))
            .collect();
        let index = sandbox.reload(repo);
        let aux_build = index.search_aux().build_ms;
        let mut micros = Vec::new();
        for _ in 0..20 {
            for (text, vector) in &queries {
                let started = Instant::now();
                let response = search(
                    index,
                    (text, vector),
                    &[],
                    SearchMode::Hybrid,
                    KEYWORD_WEIGHT,
                );
                micros.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
                assert!(!response.results.is_empty());
            }
        }
        let (p50, p95) = (percentile(&micros, 50.0), percentile(&micros, 95.0));
        println!("H2 {repo}: aux build {aux_build} ms, search p50 {p50} µs, p95 {p95} µs");
        report.insert(repo, serde_json::json!({"aux_build_ms": aux_build, "p50_us": p50, "p95_us": p95, "runs": micros.len()}));
        assert!(
            p95 <= p95_ms * 1000,
            "H2 {repo}: p95 {p95} µs > {p95_ms} ms"
        );
        assert!(
            aux_build <= aux_ms,
            "H2 {repo}: aux build {aux_build} ms > {aux_ms} ms"
        );
    }
    write_gate("h2_hybrid_latency", &report);
}

#[test]
#[ignore = "M5r: index load latency with the lexicon present (indexed sandbox)"]
fn m5r_load_regression() {
    // P5.0 baselines (Appendix D): 20 / 4 / 222 ms. Integer milliseconds:
    // the bound is ×1.10 or +1 ms, whichever is larger.
    let mut report = BTreeMap::new();
    for (repo, baseline) in [("knot", 20u64), ("knot-site", 4), ("spring-ai", 222)] {
        let layout = IndexLayout::for_repo(&sandbox_path(repo));
        let mut runs: Vec<u64> = (0..5)
            .map(|_| load_index(&layout).expect("load").load_ms)
            .collect();
        runs.sort_unstable();
        let median = runs[2];
        let bound = (baseline * 11).div_ceil(10).max(baseline + 1);
        println!("M5r {repo}: median {median} ms (runs {runs:?}), bound {bound} ms");
        report.insert(
            repo,
            serde_json::json!({"median_ms": median, "runs": runs, "bound_ms": bound}),
        );
        assert!(median <= bound, "M5r {repo}: {median} ms > {bound} ms");
    }
    write_gate("m5r_load_regression", &report);
}
