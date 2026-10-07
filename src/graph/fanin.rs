//! Heuristic caller fan-in: how many distinct containers reference each target.
//!
//! Pre-computation selects its targets by this number, so it must be the number
//! `find_callers` reports as `caller_count` for the heuristic source: callers
//! are merged to their innermost container, references from the target's own
//! container (recursion) do not count, and an edge with an unknown endpoint is
//! ignored (the loader's `incoming_map` rule).

use std::collections::{BTreeSet, HashMap};

use super::containers::{ContainerKey, ContainerTable};
use crate::index::shard::ShardLine;

/// Distinct caller containers per target id.
///
/// `edges` are `(caller id, target id)` pairs; `records` are every record of
/// the index with its file.
pub fn caller_fanin<'a>(
    edges: impl IntoIterator<Item = (&'a str, &'a str)>,
    records: &[(String, ShardLine)],
) -> HashMap<String, usize> {
    let by_id: HashMap<&str, usize> = records
        .iter()
        .enumerate()
        .map(|(position, (_, line))| (line.id.as_str(), position))
        .collect();
    let table = ContainerTable::from_records(
        records
            .iter()
            .enumerate()
            .map(|(position, (file, line))| (position, file.as_str(), line)),
    );

    let mut callers: HashMap<&str, BTreeSet<ContainerKey>> = HashMap::new();
    for (caller, target) in edges {
        let (Some(&caller_position), Some(&target_position)) =
            (by_id.get(caller), by_id.get(target))
        else {
            continue;
        };
        let (caller_file, caller_line) = &records[caller_position];
        let (target_file, target_line) = &records[target_position];
        let (key, _) = table.container_at(caller_file, caller_line.l0);
        if key == table.container_at(target_file, target_line.l0).0 {
            continue;
        }
        callers.entry(target).or_default().insert(key);
    }
    callers
        .into_iter()
        .map(|(target, containers)| (target.to_owned(), containers.len()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, kind: &str, l0: usize, l1: usize) -> ShardLine {
        ShardLine {
            id: id.to_owned(),
            kind: kind.to_owned(),
            name: id.to_owned(),
            fqn: id.to_owned(),
            lang: "rust".to_owned(),
            l0,
            l1,
            col: None,
            sig: None,
            h: "h".to_owned(),
            q: None,
            s: None,
        }
    }

    fn records() -> Vec<(String, ShardLine)> {
        vec![
            ("a.rs".to_owned(), record("target", "rust_function", 1, 3)),
            ("a.rs".to_owned(), record("one", "rust_function", 5, 8)),
            (
                "a.rs".to_owned(),
                record("one_inner", "rust_function", 6, 7),
            ),
            ("b.rs".to_owned(), record("two", "rust_function", 1, 4)),
        ]
    }

    #[test]
    fn callers_are_merged_to_their_container() {
        // The innermost rule makes `one` and the function nested in it two containers.
        let fanin = caller_fanin(
            [
                ("one", "target"),
                ("one_inner", "target"),
                ("two", "target"),
            ],
            &records(),
        );
        assert_eq!(fanin["target"], 3);
    }

    #[test]
    fn two_edges_from_one_container_count_once() {
        let fanin = caller_fanin([("two", "target"), ("two", "one")], &records());
        assert_eq!(fanin["target"], 1);
        assert_eq!(fanin["one"], 1);
    }

    #[test]
    fn recursion_is_not_a_caller() {
        let fanin = caller_fanin([("target", "target"), ("two", "target")], &records());
        assert_eq!(fanin["target"], 1);
    }

    #[test]
    fn edges_with_an_unknown_endpoint_are_ignored() {
        let fanin = caller_fanin([("ghost", "target"), ("two", "ghost")], &records());
        assert!(fanin.is_empty());
    }
    #[test]
    fn heuristic_fanin_matches_find_callers_caller_count() {
        use crate::index::build::test_support::{fixture, run};
        use crate::index::layout::IndexLayout;
        use crate::index::load::load_index;
        use crate::query::callers::callers_response;

        let (tmp, cfg) = fixture();
        run(&cfg);
        let index = load_index(&IndexLayout::for_repo(tmp.path())).expect("load");
        let records: Vec<(String, ShardLine)> = index
            .records
            .iter()
            .map(|record| (record.file.clone(), record.line.clone()))
            .collect();
        let edges: Vec<(&str, &str)> = index
            .incoming
            .iter()
            .flat_map(|(target, callers)| {
                callers
                    .iter()
                    .map(move |(caller, _)| (caller.as_str(), target.as_str()))
            })
            .collect();
        let fanin = caller_fanin(edges, &records);

        let any = knot::cli_tools::kinds::KindFilter::parse(Some("all"));
        let mut compared = 0;
        let mut with_callers = 0;
        for record in index.records.iter().filter(|r| !r.line.is_tier1()) {
            let response = callers_response(&index, "demo", &record.line.name, 500, &any);
            let Some(target) = response.targets.iter().find(|t| t.id == record.line.id) else {
                continue;
            };
            assert_eq!(
                fanin.get(&target.id).copied().unwrap_or(0),
                target.caller_count,
                "{}",
                target.fqn
            );
            compared += 1;
            with_callers += usize::from(target.caller_count > 0);
        }
        assert!(
            compared > 0 && with_callers > 0,
            "{compared} / {with_callers}"
        );
    }
}
