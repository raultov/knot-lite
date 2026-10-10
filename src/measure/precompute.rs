//! Phase 5 pre-computation gates L13 and L7s (PLAN_PHASE5_SEARCH_LSP_CI.md §11),
//! scoped by the human to the sandbox `knot` (Rust, full) and a small sample of
//! `spring-ai` (Java, 20 targets).

use super::lsp::{l7, pool_for_root};
use super::*;
use crate::graph::fanin::caller_fanin;
use crate::index::inventory::read_inventory;
use crate::index::shard::ShardLine;
use crate::lsp::precompute::measure_support::{OracleRow, heuristic_ids, lsp_refs_ids, oracle};
use crate::lsp::precompute::{PrecomputeInput, PrecomputeOptions, PrecomputePolicy, Selection};

/// Selection quality of one set of ids against the oracle.
#[derive(Serialize)]
struct SetQuality {
    selected: usize,
    overlap_pct: f64,
    sum_refs: usize,
}

fn quality(ids: &[String], oracle_top: &BTreeSet<&str>, refs: &HashMap<&str, usize>) -> SetQuality {
    let hits = ids
        .iter()
        .filter(|id| oracle_top.contains(id.as_str()))
        .count();
    SetQuality {
        selected: ids.len(),
        overlap_pct: if oracle_top.is_empty() {
            100.0
        } else {
            100.0 * hits as f64 / oracle_top.len() as f64
        },
        sum_refs: ids.iter().filter_map(|id| refs.get(id.as_str())).sum(),
    }
}

/// The probe reaches the candidates whose cumulative probe time stays within
/// 40 % of the budget (pool order, the same sequence the real probe runs).
fn reached_within_budget(rows: &[OracleRow], opts: &PrecomputeOptions) -> HashMap<String, usize> {
    let share = u64::try_from(opts.budget.mul_f64(0.4).as_millis()).unwrap_or(u64::MAX);
    let mut spent = 0u64;
    let mut reached = HashMap::new();
    for row in rows {
        if spent >= share {
            break;
        }
        spent += row.ms;
        if let Some(refs) = row.refs {
            reached.insert(row.id.clone(), refs);
        }
    }
    reached
}

/// The L13 pass rule: ≥ 85 % overlap with the oracle, ≥ heuristic + 10
/// points, and ≥ 1.10 × the callers the heuristic selection covers.
fn l13_failures(heuristic: &SetQuality, lsp_refs: &SetQuality) -> Vec<String> {
    let mut failures = Vec::new();
    if lsp_refs.overlap_pct < 85.0 {
        failures.push(format!(
            "lsp-refs overlap {:.1} % < 85 %",
            lsp_refs.overlap_pct
        ));
    }
    if lsp_refs.overlap_pct < heuristic.overlap_pct + 10.0 {
        failures.push(format!(
            "lsp-refs overlap {:.1} % < heuristic {:.1} % + 10",
            lsp_refs.overlap_pct, heuristic.overlap_pct
        ));
    }
    if (lsp_refs.sum_refs as f64) < heuristic.sum_refs as f64 * 1.10 {
        failures.push(format!(
            "Σrefs {} < heuristic {} × 1.10",
            lsp_refs.sum_refs, heuristic.sum_refs
        ));
    }
    failures
}

/// Gate L13 on one repository and language; writes `l13_<repo>.json`.
fn l13(repo: &str, lang: LspLang, opts: &PrecomputeOptions) {
    let root = sandbox_path(repo).canonicalize().expect("sandbox repo");
    let layout = IndexLayout::for_repo(&root);
    let (index, _settings, root, pool) = pool_for_root(&root);
    let records: Vec<(String, ShardLine)> = index
        .records
        .iter()
        .filter(|record| !record.line.is_tier1())
        .map(|record| (record.file.clone(), record.line.clone()))
        .collect();
    let pairs: Vec<(&str, &str)> = index
        .incoming
        .iter()
        .flat_map(|(target, callers)| {
            callers
                .iter()
                .map(move |(caller, _)| (caller.as_str(), target.as_str()))
        })
        .collect();
    let fanin = caller_fanin(pairs, &records);
    let inventory = read_inventory(&layout.inventory).expect("inventory");
    let input = PrecomputeInput {
        records: &records,
        heuristic_fanin: &fanin,
        inventory: &inventory,
        previous: None,
        repo_root: &root,
    };

    let started = Instant::now();
    let rows = oracle(&input, opts, &pool, &layout, lang);
    let oracle_ms = started.elapsed().as_millis();
    let refs: HashMap<&str, usize> = rows
        .iter()
        .filter_map(|row| row.refs.map(|refs| (row.id.as_str(), refs)))
        .collect();
    let mut ranked: Vec<&OracleRow> = rows
        .iter()
        .filter(|row| row.refs.is_some_and(|refs| refs >= opts.min_fanin))
        .collect();
    ranked.sort_by(|a, b| {
        b.refs
            .cmp(&a.refs)
            .then(b.fanin.cmp(&a.fanin))
            .then(a.id.cmp(&b.id))
    });
    let oracle_top: BTreeSet<&str> = ranked
        .iter()
        .take(opts.max_targets)
        .map(|row| row.id.as_str())
        .collect();

    let reached = reached_within_budget(&rows, opts);
    let heuristic = quality(&heuristic_ids(&input, lang, opts), &oracle_top, &refs);
    let lsp_refs = quality(
        &lsp_refs_ids(&input, lang, opts, &reached),
        &oracle_top,
        &refs,
    );
    println!(
        "L13 {repo}: pool {} (answered {}), oracle top {} in {oracle_ms} ms; reached within 40 %: {}",
        rows.len(),
        refs.len(),
        oracle_top.len(),
        reached.len()
    );
    println!(
        "L13 {repo}: heuristic overlap {:.1} % Σrefs {} ({} selected) | lsp-refs overlap {:.1} % Σrefs {} ({} selected)",
        heuristic.overlap_pct,
        heuristic.sum_refs,
        heuristic.selected,
        lsp_refs.overlap_pct,
        lsp_refs.sum_refs,
        lsp_refs.selected
    );
    write_gate(
        &format!("l13_{repo}"),
        &serde_json::json!({
            "language": lang.dir_name(),
            "options": {"min_fanin": opts.min_fanin, "max_targets": opts.max_targets,
                        "budget_s": opts.budget.as_secs(), "probe_min_fanin": opts.probe_min_fanin},
            "pool": rows.len(), "answered": refs.len(), "oracle_ms": oracle_ms,
            "oracle_top": oracle_top.len(), "reached_within_probe_share": reached.len(),
            "heuristic": heuristic, "lsp_refs": lsp_refs,
        }),
    );
    let failures = l13_failures(&heuristic, &lsp_refs);
    assert!(failures.is_empty(), "L13 {repo} failed: {failures:?}");
}

#[test]
#[ignore = "L13: probes every candidate of the sandbox `knot` with a real rust-analyzer (KNOT_LITE_LSP_CACHE_DIR; minutes)"]
fn l13_selection_knot_rust() {
    l13(
        "knot",
        LspLang::Rust,
        &PrecomputeOptions {
            policy: PrecomputePolicy::All,
            selection: Selection::LspRefs,
            ..PrecomputeOptions::default()
        },
    );
}

#[test]
#[ignore = "L13: probes a 20-target sample of the sandbox `spring-ai` with a real jdtls (KNOT_LITE_LSP_CACHE_DIR; minutes)"]
fn l13_selection_spring_ai_sample() {
    l13(
        "spring-ai",
        LspLang::Java,
        &PrecomputeOptions {
            max_targets: 20,
            budget: Duration::from_secs(120),
            selection: Selection::LspRefs,
            ..PrecomputeOptions::default()
        },
    );
}

#[test]
#[ignore = "L7s: needs the sandbox `knot` indexed with `--lsp-precompute all` (lsp-refs) and starts a real rust-analyzer (KNOT_LITE_LSP_CACHE_DIR)"]
fn l7s_precompute_knot_rust() {
    l7("knot", "rust", "l7s_knot_rust");
}
