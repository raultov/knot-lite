//! Level-2 heuristic graph: knot's pure resolver run in-process (T6).
//!
//! No Neo4j, no Qdrant — what knot-rag can answer with parsers + resolver
//! alone. Note for the report: the resolver only sees **one repository** (the
//! snapshot), which is exactly the knot-rag situation; knot's Neo4j pipeline
//! loaded a global cross-repo context.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use knot::models::{ParsedEntity, RelationshipType, ResolutionEntity};
use knot::pipeline::ingest::resolve_reference_intents;

use crate::common::{EntityKey, HEUR_SCHEMA, Lang};
use crate::entities::EntityTable;
use crate::groundtruth::Target;

/// target uuid -> distinct `(caller uuid, relationship label)` edges.
pub type IncomingEdges = HashMap<Uuid, BTreeSet<(Uuid, String)>>;

/// Inverted caller graph produced by knot's resolver (T6).
pub struct HeuristicGraph {
    pub incoming: IncomingEdges,
    pub resolve_ms: u128,
    /// Counters exposed by knot's `RunMetrics` (kept as text for the result JSON).
    pub metrics_debug: String,
}

/// Resolve all reference intents and invert the resulting edges (T6 steps 1–3).
pub fn build(parsed: &[ParsedEntity]) -> HeuristicGraph {
    let mut entities: Vec<ResolutionEntity> = parsed.iter().map(ResolutionEntity::from).collect();
    let started = Instant::now();
    let metrics = resolve_reference_intents(&mut entities);
    let resolve_ms = started.elapsed().as_millis();
    let metrics_debug = format!(
        "entities_indexed={} references_resolved={} references_ambiguous_skipped={} references_unresolved={}",
        metrics.entities_indexed.load(Ordering::Relaxed),
        metrics.references_resolved.load(Ordering::Relaxed),
        metrics.references_ambiguous_skipped.load(Ordering::Relaxed),
        metrics.references_unresolved.load(Ordering::Relaxed),
    );
    HeuristicGraph {
        incoming: invert_edges(&entities),
        resolve_ms,
        metrics_debug,
    }
}

/// `(caller uuid, label)` per target uuid; `Contains` is structural, not a
/// reference, so it never enters the caller graph (T6 step 3).
fn invert_edges(entities: &[ResolutionEntity]) -> IncomingEdges {
    let mut incoming = IncomingEdges::new();
    for entity in entities {
        for (target_uuid, rel) in &entity.relationships {
            if matches!(rel, RelationshipType::Contains) {
                continue;
            }
            incoming
                .entry(*target_uuid)
                .or_default()
                .insert((entity.uuid, rel.to_string()));
        }
    }
    incoming
}

/// Per-target caller containers with per-relationship edge counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeuristicTarget {
    pub id: String,
    /// Container keys of the callers (self-references excluded).
    pub callers: Vec<EntityKey>,
    /// Edge count per relationship label (`CALLS`, `REFERENCES`, …).
    pub by_rel: BTreeMap<String, usize>,
    /// Edges dropped because the caller container is the target itself.
    pub self_count: usize,
}

/// `results/heuristic_<lang>.json` (schema `knot-rag-spike/heuristic/v1`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeuristicResult {
    pub schema: String,
    pub lang: Lang,
    pub repo: String,
    pub resolve_ms: u128,
    pub entities: usize,
    pub metrics_debug: String,
    pub targets: Vec<HeuristicTarget>,
}

/// T6 step 4: convert every target's callers to container keys.
pub fn measure_targets(
    graph: &HeuristicGraph,
    table: &EntityTable,
    targets: &[Target],
) -> Vec<HeuristicTarget> {
    targets
        .iter()
        .map(|target| measure_one(graph, table, target))
        .collect()
}

fn measure_one(graph: &HeuristicGraph, table: &EntityTable, target: &Target) -> HeuristicTarget {
    let key = EntityKey {
        file: target.file.clone(),
        line: target.line,
        name: target.name.clone(),
    };
    let mut callers: BTreeSet<EntityKey> = BTreeSet::new();
    let mut by_rel: BTreeMap<String, usize> = BTreeMap::new();
    let mut self_count = 0;
    let self_container = table.container_at(&target.file, target.line);

    if let Some(entity) = table.get(&key)
        && let Some(edges) = graph.incoming.get(&entity.uuid)
    {
        for (caller_uuid, rel) in edges {
            let Some(caller) = table.by_uuid(caller_uuid) else {
                continue;
            };
            // Callers are mapped to containers so the granularity matches LSP
            // (T6 step 4 / T7.1).
            let container = table.container_at(&caller.key.file, caller.key.line);
            if container == self_container {
                self_count += 1;
                continue;
            }
            *by_rel.entry(rel.clone()).or_insert(0) += 1;
            callers.insert(container);
        }
    }

    HeuristicTarget {
        id: target.id.clone(),
        callers: callers.into_iter().collect(),
        by_rel,
        self_count,
    }
}

/// Parse the snapshot, resolve, and write `results/heuristic_<lang>.json`.
pub fn run_heuristic(targets_path: &Path, out: &Path) -> Result<()> {
    let targets_file = crate::groundtruth::TargetsFile::load(targets_path)?;
    let root = Path::new(&targets_file.root);
    let (table, parsed) = EntityTable::build(&targets_file.repo, root)?;
    let graph = build(&parsed);
    eprintln!(
        "# heuristic resolve: {} ms ({} entities) {}",
        graph.resolve_ms,
        parsed.len(),
        graph.metrics_debug
    );
    let rows = measure_targets(&graph, &table, &targets_file.targets);
    for row in &rows {
        eprintln!(
            "# target {}: {} callers, by_rel {:?}, self {}",
            row.id,
            row.callers.len(),
            row.by_rel,
            row.self_count
        );
    }
    let result = HeuristicResult {
        schema: HEUR_SCHEMA.to_owned(),
        lang: targets_file.lang,
        repo: targets_file.repo.clone(),
        resolve_ms: graph.resolve_ms,
        entities: parsed.len(),
        metrics_debug: graph.metrics_debug.clone(),
        targets: rows,
    };
    crate::common::write_json_pretty(out, &result)?;
    println!(
        "wrote {} ({} targets, resolve {} ms)",
        out.display(),
        result.targets.len(),
        result.resolve_ms
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::models::EntityKind;
    use std::path::PathBuf;

    fn resolution(
        uuid: Uuid,
        name: &str,
        relationships: Vec<(Uuid, RelationshipType)>,
    ) -> ResolutionEntity {
        ResolutionEntity {
            uuid,
            name: name.to_owned(),
            fqn: format!("fqn::{name}"),
            file_path: "src/a.rs".to_owned(),
            kind: EntityKind::RustFunction,
            enclosing_class: None,
            enclosing_class_fqn: None,
            signature: None,
            reference_intents: Vec::new(),
            relationships,
            alias_module_path: None,
            original_export_name: None,
            default_export: None,
            is_test_context: false,
        }
    }

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

    #[test]
    fn invert_edges_skips_contains_and_dedupes() {
        let target = Uuid::new_v5(&knot::models::NAMESPACE_KNOT, b"target");
        let caller = Uuid::new_v5(&knot::models::NAMESPACE_KNOT, b"caller");
        let entities = vec![resolution(
            caller,
            "caller",
            vec![
                (target, RelationshipType::Calls),
                (target, RelationshipType::Calls),
                (target, RelationshipType::Contains),
                (target, RelationshipType::References),
            ],
        )];
        let incoming = invert_edges(&entities);
        let edges = incoming.get(&target).expect("edges for target");
        assert_eq!(
            edges.len(),
            2,
            "duplicate CALLS collapsed, Contains skipped"
        );
        assert!(edges.contains(&(caller, "CALLS".to_owned())));
        assert!(edges.contains(&(caller, "REFERENCES".to_owned())));
    }

    #[test]
    fn build_resolves_call_intents_into_incoming_edges() {
        let mut caller = parsed("src/caller.rs", 1, "caller_fn");
        let mut target = parsed("src/target.rs", 1, "target_fn");
        // `caller_fn` body calls `target_fn`.
        caller.reference_intents = vec![knot::models::ReferenceIntent::Call {
            method: "target_fn".to_owned(),
            receiver: None,
            line: 3,
            arg_count: None,
        }];
        target.reference_intents = Vec::new();
        let parsed = vec![caller.clone(), target.clone()];

        let graph = build(&parsed);
        let edges = graph
            .incoming
            .get(&target.uuid)
            .expect("target receives edges");
        assert!(edges.contains(&(caller.uuid, "CALLS".to_owned())));
        assert!(graph.resolve_ms < 60_000);
        assert!(graph.metrics_debug.contains("references_resolved="));
    }

    #[test]
    fn measure_targets_excludes_self_and_counts_by_rel() {
        let caller = parsed("src/caller.rs", 1, "caller_fn");
        let recursive = parsed("src/target.rs", 10, "target_fn");
        let table = EntityTable::from_parsed(
            "repo",
            PathBuf::from("/snap"),
            &[caller.clone(), recursive.clone()],
        );
        let mut incoming = IncomingEdges::new();
        incoming.insert(
            recursive.uuid,
            BTreeSet::from([
                (caller.uuid, "CALLS".to_owned()),
                (recursive.uuid, "CALLS".to_owned()), // self-reference
            ]),
        );
        incoming.insert(
            caller.uuid,
            BTreeSet::from([(caller.uuid, "REFERENCES".to_owned())]),
        );
        let graph = HeuristicGraph {
            incoming,
            resolve_ms: 0,
            metrics_debug: String::new(),
        };
        let target = Target {
            id: "t1".to_owned(),
            name: "target_fn".to_owned(),
            file: "src/target.rs".to_owned(),
            line: 10,
            knot_query: "target_fn".to_owned(),
            category: "free_fn".to_owned(),
            ops: vec!["incoming".to_owned()],
            notes: String::new(),
        };

        let rows = measure_targets(&graph, &table, &[target]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].self_count, 1);
        assert_eq!(rows[0].callers.len(), 1);
        assert_eq!(rows[0].callers[0].name, "caller_fn");
        assert_eq!(rows[0].by_rel.get("CALLS"), Some(&1));
        assert!(!rows[0].by_rel.contains_key("REFERENCES"));
    }
}
