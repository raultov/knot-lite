//! `find_callers`: who references an entity, from the pre-computed heuristic
//! edges (provenance `heuristic`; no language server in 0.2.0).
//!
//! Targets are resolved in three tiers (the first with a match wins), callers
//! are reported at container granularity (the function or type that holds the
//! reference site) and grouped by relationship.

use std::collections::{BTreeMap, BTreeSet};

use knot::cli_tools::kinds::KindFilter;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use super::render::{Column, RenderDoc, RenderSection, str_at, u64_at};
use crate::config::OutputFormat;
use crate::graph::containers::{ContainerKey, FILE_LEVEL_NAME};
use crate::graph::heuristic::EDGE_REL_LABELS;
use crate::index::load::LoadedIndex;

/// Schema id of the callers document.
pub const CALLERS_SCHEMA_V1: &str = "knot-lite/callers/v1";
/// Default `max_targets`.
pub const DEFAULT_MAX_TARGETS: usize = 25;
/// Largest accepted `max_targets`.
pub const MAX_TARGETS_CAP: usize = 500;

const HEURISTIC_NOTE: &str = "Heuristic edges from knot's resolver (no language server): expect missed dynamic/indirect references and occasional over-attribution (Phase 0: precision 93 % / recall 86 % on Rust).";

/// How a query was matched to targets. The first tier with a match wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchTier {
    /// `fqn == query` (also with `.` and `::` swapped).
    ExactFqn,
    /// The FQN ends with the query at a separator boundary.
    FqnSuffix,
    /// `name == query`.
    ExactName,
}

/// The query with `::` read as `.` and with `.` read as `::` (both are tried).
fn separator_variants(query: &str) -> [String; 3] {
    [
        query.to_owned(),
        query.replace("::", "."),
        query.replace('.', "::"),
    ]
}

fn fqn_equals(fqn: &str, query: &str) -> bool {
    separator_variants(query)
        .iter()
        .any(|variant| fqn == variant)
}

/// Whether `fqn` ends with `query` at a separator boundary (`::`, `.` or `/`),
/// or equals it: `Embedder::embed` matches `knot::pipeline::embed::Embedder::embed`,
/// `der::embed` does not.
fn fqn_suffix_matches(fqn: &str, query: &str) -> bool {
    separator_variants(query).iter().any(|variant| {
        fqn.strip_suffix(variant.as_str()).is_some_and(|head| {
            head.is_empty() || head.ends_with("::") || head.ends_with('.') || head.ends_with('/')
        })
    })
}

/// Resolve `query` to entity records (never `chunk`/`file`), honouring the
/// kind filter. Targets are sorted by `(file, l0, name)`.
pub fn resolve_targets(
    index: &LoadedIndex,
    query: &str,
    filter: &KindFilter,
) -> Option<(MatchTier, Vec<usize>)> {
    let query = query.trim();
    let eligible = |&position: &usize| {
        let line = &index.records[position].line;
        !line.is_tier1() && filter.allows(&line.kind)
    };
    let scan = |matches: &dyn Fn(&str) -> bool| -> Vec<usize> {
        (0..index.records.len())
            .filter(|position| eligible(position) && matches(&index.records[*position].line.fqn))
            .collect()
    };

    let mut tiers: Vec<(MatchTier, Vec<usize>)> =
        vec![(MatchTier::ExactFqn, scan(&|fqn| fqn_equals(fqn, query)))];
    if query.contains('.') || query.contains("::") {
        tiers.push((
            MatchTier::FqnSuffix,
            scan(&|fqn| fqn_suffix_matches(fqn, query)),
        ));
    }
    tiers.push((
        MatchTier::ExactName,
        index
            .by_name
            .get(query)
            .into_iter()
            .flatten()
            .copied()
            .filter(eligible)
            .collect(),
    ));

    tiers
        .into_iter()
        .find(|(_, matched)| !matched.is_empty())
        .map(|(tier, mut matched)| {
            matched.sort_by(|&a, &b| {
                let (a, b) = (&index.records[a], &index.records[b]);
                (&a.file, a.line.l0, &a.line.name).cmp(&(&b.file, b.line.l0, &b.line.name))
            });
            (tier, matched)
        })
}

/// `index` object of the response.
#[derive(Debug, Clone, Serialize)]
pub struct CallersIndexInfo {
    /// When the index was last built.
    pub updated_at: String,
}

/// `resolution` object of the response.
#[derive(Debug, Clone, Serialize)]
pub struct Resolution {
    /// The winning tier; `null` when nothing matched.
    pub tier: Option<MatchTier>,
    /// `code_default`, `any` or `explicit`.
    pub kind_filter: &'static str,
    /// Every target the query resolved to.
    pub targets_total: usize,
    /// Targets included in the answer.
    pub targets_shown: usize,
    /// Whether `max_targets` cut the list.
    pub truncated: bool,
}

/// A container that references a target (a function, a type, or the file level).
#[derive(Debug, Clone, Serialize)]
pub struct CallerRow {
    /// Record id of the container; `null` at file level.
    pub id: Option<String>,
    /// Kind of the container; `file` at file level.
    pub kind: String,
    /// Container name; `<file>` at file level.
    pub name: String,
    /// Container FQN, when it has a record.
    pub fqn: Option<String>,
    /// Repo-relative file.
    pub file: String,
    /// First line of the container (0 at file level).
    pub l0: usize,
    /// Container signature, when it has one.
    pub signature: Option<String>,
    /// Reference sites merged into this container.
    pub sites: usize,
}

/// Callers grouped by relationship, serialised as a JSON object in the order
/// of [`EDGE_REL_LABELS`].
#[derive(Debug, Clone, Default)]
pub struct RelationGroups(pub Vec<(String, Vec<CallerRow>)>);

impl Serialize for RelationGroups {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (relation, rows) in &self.0 {
            map.serialize_entry(relation, rows)?;
        }
        map.end()
    }
}

/// One resolved target and who references it.
#[derive(Debug, Clone, Serialize)]
pub struct CallerTarget {
    /// Record id.
    pub id: String,
    /// Entity kind.
    pub kind: String,
    /// Entity name.
    pub name: String,
    /// Fully qualified name.
    pub fqn: String,
    /// Repo-relative file.
    pub file: String,
    /// First line.
    pub l0: usize,
    /// Last line.
    pub l1: usize,
    /// Signature, when the entity has one.
    pub signature: Option<String>,
    /// Distinct containers across every relationship group.
    pub caller_count: usize,
    /// References that come from the target's own container (recursion).
    pub self_references: usize,
    /// Callers grouped by relationship.
    pub callers: RelationGroups,
}

/// The `knot-lite/callers/v1` document (§9.3).
#[derive(Debug, Clone, Serialize)]
pub struct CallersResponse {
    /// [`CALLERS_SCHEMA_V1`].
    pub schema: &'static str,
    /// knot-lite version.
    pub knot_lite_version: &'static str,
    /// Repository name.
    pub repo: String,
    /// The query as given.
    pub query: String,
    /// Always `heuristic` in 0.2.0.
    pub provenance: &'static str,
    /// The index the answer came from.
    pub index: CallersIndexInfo,
    /// How the query was resolved.
    pub resolution: Resolution,
    /// The resolved targets (possibly truncated).
    pub targets: Vec<CallerTarget>,
    /// Caveats and hints.
    pub notes: Vec<String>,
}

/// A merged reference container: its record (if any) and how many entities
/// referenced the target from inside it.
type ContainerHit = (Option<usize>, usize);

/// Answer `find_callers` for `query` against `index`.
pub fn callers_response(
    index: &LoadedIndex,
    repo: &str,
    query: &str,
    max_targets: usize,
    filter: &KindFilter,
) -> CallersResponse {
    let resolved = resolve_targets(index, query, filter);
    let (tier, matched) = match resolved {
        Some((tier, matched)) => (Some(tier), matched),
        None => (None, Vec::new()),
    };
    let total = matched.len();
    let targets: Vec<CallerTarget> = matched
        .iter()
        .take(max_targets)
        .map(|&position| describe_target(index, position))
        .collect();

    let mut notes = Vec::new();
    if tier.is_none() {
        notes.push(format!(
            "no indexed entity matches '{}' (tiers tried: exact FQN, FQN suffix, exact name; kind filter {}). Use search_hybrid_context to discover names.",
            query.trim(),
            filter.wire_label()
        ));
    }
    notes.push(HEURISTIC_NOTE.to_owned());

    CallersResponse {
        schema: CALLERS_SCHEMA_V1,
        knot_lite_version: env!("CARGO_PKG_VERSION"),
        repo: repo.to_owned(),
        query: query.to_owned(),
        provenance: "heuristic",
        index: CallersIndexInfo {
            updated_at: index.manifest.updated_at.clone(),
        },
        resolution: Resolution {
            tier,
            kind_filter: filter.wire_label(),
            targets_total: total,
            targets_shown: targets.len(),
            truncated: targets.len() < total,
        },
        targets,
        notes,
    }
}

/// Who references the record at `position`, merged to containers.
fn describe_target(index: &LoadedIndex, position: usize) -> CallerTarget {
    let target = &index.records[position];
    let own = index
        .containers
        .container_at(&target.file, target.line.l0)
        .0;

    let mut by_relation: BTreeMap<(usize, String), BTreeMap<ContainerKey, ContainerHit>> =
        BTreeMap::new();
    let mut self_references = 0;
    for (src, relation) in index.incoming.get(&target.line.id).into_iter().flatten() {
        let Some(&caller_position) = index.by_id.get(src) else {
            continue;
        };
        let caller = &index.records[caller_position];
        let (key, container) = index.containers.container_at(&caller.file, caller.line.l0);
        if key == own {
            self_references += 1;
            continue;
        }
        let order = EDGE_REL_LABELS
            .iter()
            .position(|label| label == relation)
            .unwrap_or(usize::MAX);
        by_relation
            .entry((order, relation.clone()))
            .or_default()
            .entry(key)
            .or_insert((container, 0))
            .1 += 1;
    }

    let caller_count = by_relation
        .values()
        .flat_map(BTreeMap::keys)
        .collect::<BTreeSet<_>>()
        .len();
    let groups = by_relation
        .into_iter()
        .map(|((_, relation), containers)| {
            let rows = containers
                .into_iter()
                .map(|(key, (container, sites))| caller_row(index, key, container, sites))
                .collect();
            (relation, rows)
        })
        .collect();

    CallerTarget {
        id: target.line.id.clone(),
        kind: target.line.kind.clone(),
        name: target.line.name.clone(),
        fqn: target.line.fqn.clone(),
        file: target.file.clone(),
        l0: target.line.l0,
        l1: target.line.l1,
        signature: target.line.sig.clone(),
        caller_count,
        self_references,
        callers: RelationGroups(groups),
    }
}

fn caller_row(
    index: &LoadedIndex,
    key: ContainerKey,
    container: Option<usize>,
    sites: usize,
) -> CallerRow {
    match container.map(|position| &index.records[position]) {
        Some(record) => CallerRow {
            id: Some(record.line.id.clone()),
            kind: record.line.kind.clone(),
            name: record.line.name.clone(),
            fqn: Some(record.line.fqn.clone()),
            file: record.file.clone(),
            l0: record.line.l0,
            signature: record.line.sig.clone(),
            sites,
        },
        None => CallerRow {
            id: None,
            kind: "file".to_owned(),
            name: FILE_LEVEL_NAME.to_owned(),
            fqn: None,
            file: key.file,
            l0: 0,
            signature: None,
            sites,
        },
    }
}

/// Build the format-agnostic document for a `knot-lite/callers/v1` value.
pub fn callers_doc(value: &Value, _format: OutputFormat) -> RenderDoc {
    let repo = str_at(value, &["repo"]).unwrap_or("-");
    let resolution = value.get("resolution").cloned().unwrap_or_default();
    let targets = value
        .get("targets")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);

    let mut summary = vec![
        format!("query: {}", str_at(value, &["query"]).unwrap_or("")),
        format!(
            "resolution: {} (kind filter {}), {} of {} targets",
            str_at(&resolution, &["tier"]).unwrap_or("none"),
            str_at(&resolution, &["kind_filter"]).unwrap_or("-"),
            u64_at(&resolution, &["targets_shown"]).unwrap_or(0),
            u64_at(&resolution, &["targets_total"]).unwrap_or(0)
        ),
    ];
    if resolution.get("truncated").and_then(Value::as_bool) == Some(true) {
        summary.push("the target list is truncated (raise max_targets)".to_owned());
    }

    RenderDoc {
        title: format!("knot-lite callers — repository '{repo}'"),
        summary,
        sections: targets.iter().map(target_section).collect(),
        notes: value
            .get("notes")
            .and_then(Value::as_array)
            .map(|notes| {
                notes
                    .iter()
                    .filter_map(|note| note.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn target_section(target: &Value) -> RenderSection {
    let rows = target
        .get("callers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .flat_map(|(relation, callers)| {
            callers
                .as_array()
                .into_iter()
                .flatten()
                .map(move |caller| caller_cells(relation, caller))
        })
        .collect();
    RenderSection {
        heading: Some(format!(
            "{} {} — {}:{} ({} callers, {} self-references)",
            str_at(target, &["kind"]).unwrap_or("-"),
            str_at(target, &["name"]).unwrap_or("-"),
            str_at(target, &["file"]).unwrap_or("-"),
            u64_at(target, &["l0"]).unwrap_or(0),
            u64_at(target, &["caller_count"]).unwrap_or(0),
            u64_at(target, &["self_references"]).unwrap_or(0)
        )),
        columns: vec![
            Column::left("REL"),
            Column::left("CALLER"),
            Column::left("KIND"),
            Column::left("LOCATION"),
            Column::right("SITES"),
        ],
        rows,
    }
}

fn caller_cells(relation: &str, caller: &Value) -> Vec<String> {
    vec![
        relation.to_owned(),
        str_at(caller, &["name"]).unwrap_or("-").to_owned(),
        str_at(caller, &["kind"]).unwrap_or("-").to_owned(),
        format!(
            "{}:{}",
            str_at(caller, &["file"]).unwrap_or("-"),
            u64_at(caller, &["l0"]).unwrap_or(0)
        ),
        u64_at(caller, &["sites"]).unwrap_or(0).to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build::test_support::{fixture, run};
    use crate::index::layout::IndexLayout;
    use crate::index::load::test_support::{entity, synthetic_index};
    use crate::index::load::{LoadedIndex, load_index};

    fn code() -> KindFilter {
        KindFilter::parse(None)
    }

    fn any() -> KindFilter {
        KindFilter::parse(Some("all"))
    }

    fn names(index: &LoadedIndex, positions: &[usize]) -> Vec<String> {
        positions
            .iter()
            .map(|&p| index.records[p].line.fqn.clone())
            .collect()
    }

    fn fixture_index() -> (tempfile::TempDir, LoadedIndex) {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let index = load_index(&IndexLayout::for_repo(tmp.path())).expect("load");
        (tmp, index)
    }

    /// `run` methods of two types, a function `run`, and a doc section `run`.
    fn homonyms() -> LoadedIndex {
        synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("a-fn", "rust_function", "run", "pkg::run", (1, 3)),
                ),
                (
                    "src/a.rs",
                    entity("a-m1", "rust_method", "run", "pkg::Alpha::run", (5, 7)),
                ),
                (
                    "src/b.rs",
                    entity("b-m2", "rust_method", "run", "pkg::Beta::run", (2, 4)),
                ),
                (
                    "docs/run.md",
                    entity(
                        "d-sec",
                        "markdown_section",
                        "run",
                        "docs/run.md::run",
                        (1, 9),
                    ),
                ),
            ],
            &[],
        )
    }

    #[test]
    fn tier_exact_fqn_wins_over_name() {
        let index = homonyms();
        // `pkg::run` is an FQN; `run` also names four entities, but the FQN tier wins.
        let (tier, targets) = resolve_targets(&index, "pkg::run", &code()).expect("match");
        assert_eq!(tier, MatchTier::ExactFqn);
        assert_eq!(names(&index, &targets), vec!["pkg::run"]);
        // The dotted spelling resolves to the same entity.
        let (tier, targets) = resolve_targets(&index, "pkg.Alpha.run", &code()).expect("match");
        assert_eq!(tier, MatchTier::ExactFqn);
        assert_eq!(names(&index, &targets), vec!["pkg::Alpha::run"]);
    }

    #[test]
    fn tier_fqn_suffix_with_dot_and_double_colon() {
        let index = homonyms();
        for query in ["Alpha::run", "Alpha.run"] {
            let (tier, targets) = resolve_targets(&index, query, &code()).expect(query);
            assert_eq!(tier, MatchTier::FqnSuffix, "{query}");
            assert_eq!(names(&index, &targets), vec!["pkg::Alpha::run"], "{query}");
        }
    }

    #[test]
    fn tier_fqn_suffix_requires_boundary() {
        let index = homonyms();
        // `pha::run` is a textual suffix of `pkg::Alpha::run` but not at a separator.
        assert!(resolve_targets(&index, "pha::run", &code()).is_none());
        assert!(fqn_suffix_matches(
            "knot::pipeline::embed::Embedder::embed",
            "Embedder::embed"
        ));
        assert!(fqn_suffix_matches(
            "knot::pipeline::embed::Embedder::embed",
            "Embedder.embed"
        ));
        assert!(!fqn_suffix_matches(
            "knot::pipeline::embed::Embedder::embed",
            "der::embed"
        ));
        assert!(
            fqn_suffix_matches("a/b.rs::x", "b.rs::x"),
            "`/` is a boundary too"
        );
        assert!(
            fqn_suffix_matches("x::y", "x::y"),
            "the whole FQN is a boundary"
        );
    }

    #[test]
    fn tier_exact_name_groups_homonyms() {
        let index = homonyms();
        let (tier, targets) = resolve_targets(&index, "run", &code()).expect("match");
        assert_eq!(tier, MatchTier::ExactName);
        // Sorted by (file, l0); the markdown section is hidden by the code filter.
        assert_eq!(
            names(&index, &targets),
            vec!["pkg::run", "pkg::Alpha::run", "pkg::Beta::run"]
        );
    }

    #[test]
    fn kind_filter_default_hides_docs() {
        let index = homonyms();
        let (_, with_docs) = resolve_targets(&index, "run", &any()).expect("match");
        assert_eq!(with_docs.len(), 4);
        let (_, only_docs) =
            resolve_targets(&index, "run", &KindFilter::parse(Some("docs"))).expect("match");
        assert_eq!(names(&index, &only_docs), vec!["docs/run.md::run"]);
        // An index whose only match is a doc section: invisible by default.
        let docs_only = synthetic_index(
            vec![(
                "docs/x.md",
                entity("s", "markdown_section", "Setup", "docs/x.md::Setup", (1, 4)),
            )],
            &[],
        );
        assert!(resolve_targets(&docs_only, "Setup", &code()).is_none());
        assert!(resolve_targets(&docs_only, "Setup", &any()).is_some());
    }

    #[test]
    fn tier1_records_are_never_targets() {
        let mut chunk = entity("c", "chunk", "run.sh", "scripts/run.sh", (1, 60));
        chunk.sig = None;
        let index = synthetic_index(vec![("scripts/run.sh", chunk)], &[]);
        assert!(resolve_targets(&index, "run.sh", &any()).is_none());
        assert!(resolve_targets(&index, "scripts/run.sh", &any()).is_none());
    }

    #[test]
    fn max_targets_truncates_and_reports_total() {
        let index = homonyms();
        let response = callers_response(&index, "demo", "run", 2, &code());
        assert_eq!(response.targets.len(), 2);
        assert_eq!(response.resolution.targets_total, 3);
        assert_eq!(response.resolution.targets_shown, 2);
        assert!(response.resolution.truncated);
        let full = callers_response(&index, "demo", "run", 25, &code());
        assert!(!full.resolution.truncated);
    }

    #[test]
    fn callers_are_containers_with_sites() {
        let (_tmp, index) = fixture_index();
        let response = callers_response(&index, "demo", "target", 25, &code());
        assert_eq!(response.resolution.tier, Some(MatchTier::ExactName));
        assert_eq!(response.targets.len(), 1);
        let target = &response.targets[0];
        assert_eq!(
            (target.name.as_str(), target.fqn.as_str()),
            ("target", "fixture::target")
        );
        assert_eq!(target.caller_count, 3);
        assert_eq!(target.self_references, 0);

        let value = serde_json::to_value(&response).expect("json");
        let calls = value["targets"][0]["callers"]["CALLS"]
            .as_array()
            .expect("CALLS group");
        let callers: Vec<(&str, &str, u64)> = calls
            .iter()
            .map(|c| {
                (
                    c["file"].as_str().expect("f"),
                    c["name"].as_str().expect("n"),
                    c["sites"].as_u64().expect("s"),
                )
            })
            .collect();
        assert_eq!(
            callers,
            vec![
                ("src/lib.rs", "caller", 1),
                ("src/util.rs", "helper", 1),
                ("src/util.rs", "run", 1)
            ]
        );
        assert!(
            calls
                .iter()
                .all(|c| c["id"].is_string() && c["fqn"].is_string())
        );
        assert_eq!(
            value["targets"][0]["callers"]
                .as_object()
                .expect("object")
                .len(),
            1
        );
    }

    #[test]
    fn recursion_counts_as_self_reference() {
        let (_tmp, index) = fixture_index();
        // knot records `struct Widget` as referencing itself (its `impl` block).
        let response = callers_response(&index, "demo", "Widget", 25, &code());
        let widget = &response.targets[0];
        assert_eq!(widget.fqn, "fixture::util::Widget");
        assert_eq!(widget.self_references, 1);
        assert_eq!(widget.caller_count, 0);
        assert!(widget.callers.0.is_empty());
    }

    #[test]
    fn file_level_callers_render_as_file_rows() {
        // A reference site outside every container: attributed to the file level.
        let index = synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("t", "rust_function", "target", "pkg::target", (10, 12)),
                ),
                (
                    "src/a.rs",
                    entity("k", "rust_constant", "K", "pkg::K", (1, 1)),
                ),
            ],
            &[("k", "t", "CALLS")],
        );
        let response = callers_response(&index, "demo", "target", 25, &code());
        let rows = &response.targets[0].callers.0[0].1;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            (
                row.id.as_deref(),
                row.kind.as_str(),
                row.name.as_str(),
                row.l0
            ),
            (None, "file", "<file>", 0)
        );
        assert_eq!(row.file, "src/a.rs");
        assert_eq!(response.targets[0].caller_count, 1);
    }

    #[test]
    fn several_sites_in_one_container_merge() {
        let index = synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("t", "rust_function", "target", "pkg::target", (30, 32)),
                ),
                (
                    "src/a.rs",
                    entity("f", "rust_function", "outer", "pkg::outer", (1, 20)),
                ),
                (
                    "src/a.rs",
                    entity("c1", "rust_constant", "A", "pkg::A", (5, 5)),
                ),
                (
                    "src/a.rs",
                    entity("c2", "rust_constant", "B", "pkg::B", (7, 7)),
                ),
            ],
            &[
                ("c1", "t", "CALLS"),
                ("c2", "t", "CALLS"),
                ("f", "t", "REFERENCES"),
            ],
        );
        let response = callers_response(&index, "demo", "target", 25, &code());
        let target = &response.targets[0];
        assert_eq!(target.caller_count, 1, "all three sites sit inside `outer`");
        let groups: Vec<(&str, usize, usize)> = target
            .callers
            .0
            .iter()
            .map(|(rel, rows)| (rel.as_str(), rows.len(), rows[0].sites))
            .collect();
        // CALLS comes before REFERENCES (EDGE_REL_LABELS order); two sites merged.
        assert_eq!(groups, vec![("CALLS", 1, 2), ("REFERENCES", 1, 1)]);
    }

    #[test]
    fn relation_groups_serialise_in_label_order() {
        let groups = RelationGroups(vec![
            ("CALLS".to_owned(), Vec::new()),
            ("REFERENCES".to_owned(), Vec::new()),
            ("IMPLEMENTS".to_owned(), Vec::new()),
        ]);
        assert_eq!(
            serde_json::to_string(&groups).expect("json"),
            r#"{"CALLS":[],"REFERENCES":[],"IMPLEMENTS":[]}"#
        );
    }

    #[test]
    fn no_match_answer_has_hint() {
        let (_tmp, index) = fixture_index();
        let response = callers_response(&index, "demo", "does_not_exist", 25, &code());
        assert!(response.targets.is_empty());
        assert_eq!(response.resolution.tier, None);
        assert_eq!(response.resolution.targets_total, 0);
        assert!(
            response.notes[0].starts_with("no indexed entity matches 'does_not_exist'"),
            "{:?}",
            response.notes
        );
        assert!(response.notes[0].contains("kind filter code_default"));
        assert!(response.notes[0].contains("search_hybrid_context"));
        assert_eq!(
            response.notes.last().map(String::as_str),
            Some(HEURISTIC_NOTE)
        );
    }

    #[test]
    fn target_without_callers_is_listed_with_zero() {
        let (_tmp, index) = fixture_index();
        let response = callers_response(&index, "demo", "caller", 25, &code());
        assert_eq!(response.targets.len(), 1);
        assert_eq!(response.targets[0].caller_count, 0);
        assert_eq!(response.notes, vec![HEURISTIC_NOTE]);
    }

    #[test]
    fn callers_renderers_snapshot() {
        let (_tmp, index) = fixture_index();
        let response = callers_response(&index, "demo", "target", 25, &code());
        let value = serde_json::to_value(&response).expect("json");
        assert_eq!(value["schema"], "knot-lite/callers/v1");
        assert_eq!(value["provenance"], "heuristic");
        assert_eq!(value["resolution"]["tier"], "exact_name");
        assert_eq!(value["resolution"]["kind_filter"], "code_default");

        let doc = callers_doc(&value, OutputFormat::Table);
        assert_eq!(doc.title, "knot-lite callers — repository 'demo'");
        assert_eq!(
            doc.sections[0].heading.as_deref(),
            Some("rust_function target — src/lib.rs:3 (3 callers, 0 self-references)")
        );
        assert_eq!(doc.sections[0].rows.len(), 3);
        assert_eq!(
            doc.sections[0].rows[0],
            vec!["CALLS", "caller", "rust_function", "src/lib.rs:7", "1"]
        );
        assert!(
            doc.notes
                .last()
                .expect("note")
                .starts_with("Heuristic edges")
        );
    }
}
