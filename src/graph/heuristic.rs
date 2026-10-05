//! Tier-2 heuristic edges: knot's pure reference resolver, run in-process.
//!
//! No database: the resolver sees exactly one repository, which is the
//! knot-lite situation. Edges are pre-computed at index time and never at
//! query time (Phase 0: resolve cost 0.9 s for rust, 41 s for java).

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::time::Instant;

use knot::models::{ParsedEntity, RelationshipType, ResolutionEntity};
use knot::pipeline::ingest::resolve_reference_intents;
use serde::Serialize;
use uuid::Uuid;

/// Every `RelationshipType` label except `CONTAINS` (structural, not a
/// reference). Drift-guarded against knot's enum by a test.
pub const EDGE_REL_LABELS: &[&str] = &[
    "CALLS",
    "EXTENDS",
    "IMPLEMENTS",
    "REFERENCES",
    "REFERENCES_DOM",
    "USES_CSS_CLASS",
    "IMPORTS_SCRIPT",
    "IMPORTS_STYLESHEET",
    "MACRO_CALLS",
    "GENERIC_BOUND",
    "DEPENDS_ON",
    "OVERRIDES",
    "USES_BACKEND",
    "USES_PROBE",
    "USES_ACL",
    "INCLUDES",
    "IMPORTS_VMOD",
    "DECLARED_UNUSED",
];

/// Counters of one resolver run (knot's `RunMetrics`, read out of its atomics).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResolverMetrics {
    /// Entities the resolver indexed.
    pub entities_indexed: usize,
    /// Reference intents resolved to exactly one target.
    pub references_resolved: usize,
    /// Intents skipped because several targets matched.
    pub references_ambiguous_skipped: usize,
    /// Intents with no matching target.
    pub references_unresolved: usize,
}

/// The distinct edges of one run.
#[derive(Debug, Clone, Default)]
pub struct ResolvedEdges {
    /// `(caller uuid, target uuid, relationship label)`, deduplicated and sorted.
    pub edges: BTreeSet<(Uuid, Uuid, String)>,
    /// Resolver counters.
    pub metrics: ResolverMetrics,
    /// Wall-clock milliseconds spent in the resolver.
    pub resolve_ms: u64,
}

/// Resolve every reference intent of `entities` (already sorted by the build,
/// so the result does not depend on parse thread order).
pub fn resolve_edges(entities: &[ParsedEntity]) -> ResolvedEdges {
    let mut resolution: Vec<ResolutionEntity> =
        entities.iter().map(ResolutionEntity::from).collect();
    let started = Instant::now();
    let run = resolve_reference_intents(&mut resolution);
    let resolve_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let load = |counter: &std::sync::atomic::AtomicU64| {
        usize::try_from(counter.load(Ordering::Relaxed)).unwrap_or(usize::MAX)
    };
    ResolvedEdges {
        edges: collect_edges(&resolution),
        metrics: ResolverMetrics {
            entities_indexed: load(&run.entities_indexed),
            references_resolved: load(&run.references_resolved),
            references_ambiguous_skipped: load(&run.references_ambiguous_skipped),
            references_unresolved: load(&run.references_unresolved),
        },
        resolve_ms,
    }
}

/// Distinct `(caller, target, label)` triples; `CONTAINS` is structural, not a
/// reference, so it never becomes an edge.
fn collect_edges(entities: &[ResolutionEntity]) -> BTreeSet<(Uuid, Uuid, String)> {
    entities
        .iter()
        .flat_map(|entity| {
            entity
                .relationships
                .iter()
                .filter(|(_, rel)| !matches!(rel, RelationshipType::Contains))
                .map(|(target, rel)| (entity.uuid, *target, rel.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::models::{EntityKind, ReferenceIntent};

    fn parsed(file: &str, line: usize, name: &str) -> ParsedEntity {
        ParsedEntity::new(
            name,
            EntityKind::RustFunction,
            format!("fqn::{name}"),
            None,
            None,
            "rust",
            file,
            line,
            line + 5,
            None,
            "repo",
        )
    }

    /// Every variant of knot's enum. The exhaustive `match` makes a new upstream
    /// variant a compile error here instead of a silent label drift.
    fn every_relationship() -> Vec<RelationshipType> {
        use RelationshipType::*;
        let all = vec![
            Calls,
            Extends,
            Implements,
            References,
            ReferencesDOM,
            UsesCSSClass,
            ImportsScript,
            ImportsStylesheet,
            MacroCalls,
            Contains,
            GenericBound,
            DependsOn,
            Overrides,
            UsesBackend,
            UsesProbe,
            UsesAcl,
            Includes,
            ImportsVmod,
            DeclaredUnused,
        ];
        for relationship in &all {
            match relationship {
                Calls | Extends | Implements | References | ReferencesDOM | UsesCSSClass
                | ImportsScript | ImportsStylesheet | MacroCalls | Contains | GenericBound
                | DependsOn | Overrides | UsesBackend | UsesProbe | UsesAcl | Includes
                | ImportsVmod | DeclaredUnused => {}
            }
        }
        all
    }

    #[test]
    fn edge_labels_match_knot() {
        let variants = every_relationship();
        let mut seen = BTreeSet::new();
        for relationship in &variants {
            let label = relationship.to_string();
            if matches!(relationship, RelationshipType::Contains) {
                assert!(
                    !EDGE_REL_LABELS.contains(&label.as_str()),
                    "CONTAINS is excluded"
                );
            } else {
                assert!(EDGE_REL_LABELS.contains(&label.as_str()), "{label} missing");
                seen.insert(label);
            }
        }
        assert_eq!(EDGE_REL_LABELS.len(), variants.len() - 1);
        assert_eq!(seen.len(), EDGE_REL_LABELS.len(), "no duplicate labels");
    }

    #[test]
    fn contains_is_excluded_and_duplicates_collapse() {
        let target = Uuid::new_v5(&knot::models::NAMESPACE_KNOT, b"target");
        let mut caller = ResolutionEntity::from(&parsed("src/a.rs", 1, "caller"));
        caller.relationships = vec![
            (target, RelationshipType::Calls),
            (target, RelationshipType::Calls),
            (target, RelationshipType::Contains),
            (target, RelationshipType::References),
        ];
        let edges = collect_edges(&[caller.clone()]);
        assert_eq!(
            edges.len(),
            2,
            "duplicate CALLS collapsed, CONTAINS skipped"
        );
        assert!(edges.contains(&(caller.uuid, target, "CALLS".to_owned())));
        assert!(edges.contains(&(caller.uuid, target, "REFERENCES".to_owned())));
    }

    #[test]
    fn resolve_edges_turns_call_intents_into_edges() {
        let mut caller = parsed("src/caller.rs", 1, "caller_fn");
        let target = parsed("src/target.rs", 1, "target_fn");
        caller.reference_intents = vec![ReferenceIntent::Call {
            method: "target_fn".to_owned(),
            receiver: None,
            line: 3,
            arg_count: None,
        }];
        let resolved = resolve_edges(&[caller.clone(), target.clone()]);
        assert!(
            resolved
                .edges
                .contains(&(caller.uuid, target.uuid, "CALLS".to_owned()))
        );
        assert_eq!(resolved.metrics.entities_indexed, 2);
        assert!(resolved.resolve_ms < 60_000);
    }

    /// knot counts only non-call references (types, inheritance, …) in
    /// `references_resolved` / `references_unresolved`; calls are resolved on
    /// another path that does not touch those counters.
    #[test]
    fn metrics_are_read_from_knots_counters() {
        let mut user = parsed("src/user.rs", 1, "user_fn");
        user.reference_intents = vec![
            ReferenceIntent::TypeReference {
                type_name: "Widget".to_owned(),
                line: 2,
            },
            ReferenceIntent::TypeReference {
                type_name: "Missing".to_owned(),
                line: 3,
            },
        ];
        let widget = ParsedEntity::new(
            "Widget",
            EntityKind::RustStruct,
            "fqn::Widget",
            None,
            None,
            "rust",
            "src/widget.rs",
            1,
            4,
            None,
            "repo",
        );
        let resolved = resolve_edges(&[user.clone(), widget.clone()]);
        assert!(
            resolved
                .edges
                .contains(&(user.uuid, widget.uuid, "REFERENCES".to_owned()))
        );
        assert_eq!(resolved.metrics.references_resolved, 1);
        assert_eq!(resolved.metrics.references_unresolved, 1);
        assert_eq!(resolved.metrics.references_ambiguous_skipped, 0);
    }

    #[test]
    fn no_entities_resolve_to_no_edges() {
        let resolved = resolve_edges(&[]);
        assert!(resolved.edges.is_empty());
        assert_eq!(resolved.metrics, ResolverMetrics::default());
    }

    #[test]
    fn edges_are_sorted_by_caller_target_label() {
        let low = Uuid::from_u128(1);
        let high = Uuid::from_u128(2);
        let mut a = ResolutionEntity::from(&parsed("a.rs", 1, "a"));
        a.relationships = vec![
            (high, RelationshipType::References),
            (low, RelationshipType::Calls),
        ];
        let edges: Vec<_> = collect_edges(&[a]).into_iter().collect();
        assert_eq!(edges[0].1, low);
        assert_eq!(edges[1].1, high);
    }
}
