//! Root coverage of the hybrid search's pool (§6.3.3): how many bridge seeds
//! each candidate calls, directly or through one helper, its out-degree, and
//! whether a pool caller covering more roots supersedes it.
//!
//! The in-memory equivalent of knot 1.11.3's `annotate_root_coverage`
//! (`search_hybrid_context/coverage.rs:264-377`) and its Cypher
//! `root_coverage_query` (`db/graph/query.rs:476-498`), over the index's
//! heuristic `CALLS` edges (P5-D9).

use std::collections::HashSet;

use knot::cli_tools::search_hybrid_context::rank::is_test_row;

use crate::index::load::LoadedIndex;
use crate::index::search_aux::SearchAux;

/// The graph evidence of one pool row, as knot's re-rank reads it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Seeds the row calls directly.
    pub direct: usize,
    /// Seeds it reaches through exactly one helper, excluding direct ones.
    pub transitive: usize,
    /// Number of seeds.
    pub total: usize,
    /// Distinct callees.
    pub out_degree: usize,
    /// Whether a production pool caller covers strictly more roots (≥ 2).
    pub superseded: bool,
}

/// The coverage of every row (record positions) against `seeds`; `None`
/// without seeds (knot then leaves the pool unannotated).
pub fn annotate(
    index: &LoadedIndex,
    aux: &SearchAux,
    rows: &[usize],
    seeds: &[usize],
) -> Option<Vec<Coverage>> {
    if seeds.is_empty() {
        return None;
    }
    let seeds: HashSet<u32> = seeds
        .iter()
        .filter_map(|&at| u32::try_from(at).ok())
        .collect();
    let mut coverage: Vec<Coverage> = rows
        .iter()
        .map(|&at| row_coverage(aux, at, &seeds))
        .collect();
    let superseded = superseded_rows(index, aux, rows, &coverage);
    for (row, flag) in coverage.iter_mut().zip(superseded) {
        row.superseded = flag;
    }
    Some(coverage)
}

fn row_coverage(aux: &SearchAux, at: usize, seeds: &HashSet<u32>) -> Coverage {
    let callees = &aux.calls_out[at];
    let direct: HashSet<u32> = callees
        .iter()
        .copied()
        .filter(|callee| seeds.contains(callee))
        .collect();
    let transitive: HashSet<u32> = callees
        .iter()
        .flat_map(|&helper| aux.calls_out[helper as usize].iter().copied())
        .filter(|root| seeds.contains(root) && !direct.contains(root))
        .collect();
    Coverage {
        direct: direct.len(),
        transitive: transitive.len(),
        total: seeds.len(),
        out_degree: callees.len(),
        superseded: false,
    }
}

/// Whether each row has a production pool caller (other than itself) that
/// covers at least two roots directly and strictly more than the row
/// (knot's `supersedes_caller`, `coverage.rs:121-127`).
fn superseded_rows(
    index: &LoadedIndex,
    aux: &SearchAux,
    rows: &[usize],
    coverage: &[Coverage],
) -> Vec<bool> {
    let pool_row: std::collections::HashMap<usize, usize> = rows
        .iter()
        .enumerate()
        .map(|(row, &at)| (at, row))
        .collect();
    rows.iter()
        .enumerate()
        .map(|(row, &at)| {
            aux.calls_in[at].iter().any(|&caller| {
                let caller = caller as usize;
                let Some(&caller_row) = pool_row.get(&caller) else {
                    return false;
                };
                let record = &index.records[caller];
                let production = !is_test_row(&record.line.kind, &record.file, record.line.tc);
                let (theirs, ours) = (coverage[caller_row].direct, coverage[row].direct);
                caller != at && production && theirs >= 2 && theirs > ours
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::load::test_support::{entity, synthetic_index};
    use crate::query::pool::test_support::{at, aux_for};

    #[test]
    fn coverage_direct_transitive_superseded() {
        // Seeds r1, r2, r3. `entry` calls r1, r2 and `step`; `step` calls r1 and
        // r3; `tester` (a test file) calls r1, r2, r3; `leaf` calls nothing.
        let names = ["r1", "r2", "r3", "entry", "step", "leaf"];
        let mut records: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                (
                    "src/a.rs",
                    entity(
                        name,
                        "rust_function",
                        name,
                        &format!("a::{name}"),
                        (i * 3 + 1, i * 3 + 2),
                    ),
                )
            })
            .collect();
        records.push((
            "tests/t.rs",
            entity("tester", "rust_function", "tester", "t::tester", (1, 2)),
        ));
        let index = synthetic_index(records, &[]);
        let aux = aux_for(
            &index,
            &[
                ("entry", "r1"),
                ("entry", "r2"),
                ("entry", "step"),
                ("step", "r1"),
                ("step", "r3"),
                ("tester", "r1"),
                ("tester", "r2"),
                ("tester", "r3"),
            ],
        );
        let seeds = [at(&index, "r1"), at(&index, "r2"), at(&index, "r3")];
        let rows: Vec<usize> = ["entry", "step", "leaf", "tester", "r1"]
            .iter()
            .map(|name| at(&index, name))
            .collect();
        let coverage = annotate(&index, &aux, &rows, &seeds).expect("seeds");
        let entry = coverage[0];
        assert_eq!((entry.direct, entry.transitive, entry.total), (2, 1, 3));
        assert_eq!(entry.out_degree, 3);
        assert!(!entry.superseded);
        // `step` covers 2 directly; its pool caller `entry` covers 2 as well:
        // equal is not strictly more, so no supersession.
        let step = coverage[1];
        assert_eq!((step.direct, step.transitive), (2, 0));
        assert!(!step.superseded);
        assert_eq!(
            coverage[2],
            Coverage {
                total: 3,
                ..Coverage::default()
            }
        );
        // `r1` is called by `entry` (2 roots) and `step` (2), both production
        // and strictly above r1's 0: superseded. The test caller does not count
        // (it calls r1 too, but is not a production row).
        assert!(coverage[4].superseded);
        assert!(annotate(&index, &aux, &rows, &[]).is_none());
    }

    #[test]
    fn out_degree_counts_calls_only() {
        let index = synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("a", "rust_function", "a", "m::a", (1, 2)),
                ),
                (
                    "src/a.rs",
                    entity("b", "rust_function", "b", "m::b", (3, 4)),
                ),
                ("src/a.rs", entity("t", "rust_struct", "T", "m::T", (5, 6))),
            ],
            &[("a", "b", "CALLS"), ("a", "t", "REFERENCES")],
        );
        let aux = index.search_aux();
        let coverage = annotate(&index, aux, &[at(&index, "a")], &[at(&index, "b")]).expect("seed");
        assert_eq!(coverage[0].out_degree, 1);
        assert_eq!(coverage[0].direct, 1);
    }
}
