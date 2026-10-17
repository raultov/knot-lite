//! `find_callers`: who references an entity.
//!
//! Targets are resolved with knot's tier ladder (exact FQN, FQN suffix, exact
//! name, signature prefix, fuzzy, name prefix; the first with a match wins), callers
//! are reported at container granularity (the function or type that holds the
//! reference site) and grouped by relationship. Each target is answered by a
//! language server (pre-computed at index time, or asked now) when one applies,
//! else by the index's heuristic edges (see [`super::sources`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use knot::cli_tools::kinds::{CALLABLE_KINDS, KindFilter, TYPE_KINDS};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use super::render::{Column, RenderDoc, RenderSection, str_at, u64_at};
use super::sources::{Describer, PROVENANCE_HEURISTIC, PROVENANCE_INDEXED};
use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::config::OutputFormat;
use crate::index::load::LoadedIndex;
use crate::index::shard::ShardLine;
use crate::lsp::pool::LspPool;
use crate::lsp::settings::LspSettings;
use crate::lsp::target::OpCounts;

/// Schema id of the callers document (P3-D11).
pub const CALLERS_SCHEMA_V2: &str = "knot-lite/callers/v2";
/// Default `max_targets`.
pub const DEFAULT_MAX_TARGETS: usize = 25;
/// Largest accepted `max_targets`.
pub const MAX_TARGETS_CAP: usize = 500;

const HEURISTIC_NOTE: &str = "Heuristic edges from knot's resolver (no language server): expect missed dynamic/indirect references and occasional over-attribution (Phase 0: precision 93 % / recall 86 % on Rust).";

/// How a query was matched to targets: knot's own tier enum (P5-D5).
pub use knot::db::graph::MatchTier;

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
/// kind filter. The tiers and their order are knot's
/// ([`knot::db::graph::target_resolution_tiers`]); each tier's predicate is
/// evaluated in memory and the first tier with a match wins. Exact tiers are
/// sorted by `(file, l0, name)`, approximate tiers by [`approximate_key`].
pub fn resolve_targets(
    index: &LoadedIndex,
    query: &str,
    filter: &KindFilter,
) -> Option<(MatchTier, Vec<usize>)> {
    let query = query.trim();
    let lower = query.to_lowercase();
    knot::db::graph::target_resolution_tiers(query)
        .into_iter()
        .find_map(|(tier, _cypher)| {
            let mut matched: Vec<usize> = (0..index.records.len())
                .filter(|&position| {
                    let line = &index.records[position].line;
                    !line.is_tier1()
                        && filter.allows(&line.kind)
                        && tier_matches(tier, line, query, &lower)
                })
                .collect();
            if matched.is_empty() {
                return None;
            }
            if is_approximate(tier) {
                matched.sort_by_cached_key(|&position| approximate_key(index, position, &lower));
            } else {
                matched.sort_by(|&a, &b| {
                    let (a, b) = (&index.records[a], &index.records[b]);
                    (&a.file, a.line.l0, &a.line.name).cmp(&(&b.file, b.line.l0, &b.line.name))
                });
            }
            Some((tier, matched))
        })
}

/// knot's tier predicates (`db/graph/query.rs` `target_resolution_tiers`),
/// evaluated on one record; `lower` is the lower-cased query.
fn tier_matches(tier: MatchTier, line: &ShardLine, query: &str, lower: &str) -> bool {
    let name_and_signature = || format!("{}{}", line.name, line.sig.as_deref().unwrap_or(""));
    match tier {
        MatchTier::ExactFqn => fqn_equals(&line.fqn, query),
        MatchTier::FqnSuffix => fqn_suffix_matches(&line.fqn, query),
        MatchTier::ExactName => line.name == query,
        MatchTier::SignaturePrefix => name_and_signature().starts_with(query),
        MatchTier::Fuzzy => {
            line.fqn.to_lowercase().contains(lower)
                || name_and_signature().to_lowercase().contains(lower)
        }
        MatchTier::NamePrefix => !lower.is_empty() && line.name.to_lowercase().starts_with(lower),
    }
}

/// Whether a tier matches approximately (anything but the exact tiers).
fn is_approximate(tier: MatchTier) -> bool {
    matches!(
        tier,
        MatchTier::SignaturePrefix | MatchTier::Fuzzy | MatchTier::NamePrefix
    )
}

/// Order of the approximate tiers (P5-D6): match quality on the name (equal
/// ignoring case, prefix, contains, elsewhere), then types before callables
/// before the rest, then `(file, l0, name)`.
fn approximate_key(
    index: &LoadedIndex,
    position: usize,
    lower: &str,
) -> (u8, u8, String, usize, String) {
    let record = &index.records[position];
    let name = record.line.name.to_lowercase();
    let quality = if name == lower {
        0
    } else if name.starts_with(lower) {
        1
    } else if name.contains(lower) {
        2
    } else {
        3
    };
    let kind = record.line.kind.as_str();
    let kind_rank = if TYPE_KINDS.contains(&kind) {
        0
    } else if CALLABLE_KINDS.contains(&kind) {
        1
    } else {
        2
    };
    (
        quality,
        kind_rank,
        record.file.clone(),
        record.line.l0,
        record.line.name.clone(),
    )
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
    /// Whether the index knows this container (P6-D7).  `false` only for
    /// overlay containers of changed files that have no indexed counterpart.
    pub indexed: bool,
}

/// Callers grouped by relationship, serialised as a JSON object in the order
/// of [`EDGE_REL_LABELS`](crate::graph::heuristic::EDGE_REL_LABELS).
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

/// What a call may ask of language servers.
pub struct LspContext<'a> {
    /// Resolved settings (live budget, deadline, whether servers may start).
    pub settings: &'a LspSettings,
    /// Absolute repository root, where the current files are read.
    pub repo_root: &'a Path,
    /// The process-wide pool; `None` when live language servers are off.
    pub pool: Option<&'a LspPool>,
}

/// Inputs of [`callers_response`].
pub struct CallersContext<'a> {
    /// The loaded index.
    pub index: &'a LoadedIndex,
    /// Repository name.
    pub repo: &'a str,
    /// The query as given.
    pub query: &'a str,
    /// Targets shown at most.
    pub max_targets: usize,
    /// Kind filter.
    pub filter: &'a KindFilter,
    /// What language servers may be asked.
    pub lsp: LspContext<'a>,
    /// Changed files since the index (P6-D3); `None` skips edit-awareness.
    pub edits: Option<&'a crate::query::edits::EditedFiles>,
}

/// The `lsp` object of one target (§10.1).
#[derive(Debug, Clone, Serialize)]
pub struct TargetLspInfo {
    /// `ok` (live), `indexed`, or why the server did not answer: `not_ready`,
    /// `no_server`, `failed`, `no_position`, `no_column`, `stale_target`,
    /// `unsupported_kind`, `budget`, `deadline`, `timeout`, `error`, `disabled`.
    pub status: &'static str,
    /// In-repository locations per operation; `null` when the operation did not run.
    pub ops: OpCounts,
    /// Locations outside the repository that were dropped.
    pub external: usize,
    /// Java references that only sit in a comment and were dropped.
    pub comment_filtered: usize,
    /// Whether an empty first answer was asked again.
    pub retried_empty: bool,
    /// Query time; for `indexed`, the pre-computation time.
    pub ms: u64,
    /// Caller containers the heuristic found and the server did not.
    pub heuristic_extra: usize,
    /// The target's file changed and the request used the re-parsed position (P6-D6).
    #[serde(default)]
    pub reanchored: bool,
}

/// State of one language server, in the top-level `lsp.languages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LangRuntimeState {
    /// `ready`, `starting`, `failed`, `disabled`, `missing` or `not_started`.
    pub state: String,
    /// Server executable.
    pub server: String,
    /// Version the server reported.
    pub version: String,
    /// Milliseconds the server took to become ready.
    pub ready_ms: Option<u64>,
}

/// The top-level `lsp` object (§10.1).
#[derive(Debug, Clone, Serialize)]
pub struct TopLspInfo {
    /// Whether live language servers were allowed.
    pub live_enabled: bool,
    /// Live targets allowed per call.
    pub live_budget: usize,
    /// Live time allowed per call.
    pub deadline_ms: u64,
    /// The language servers this call touched.
    pub languages: BTreeMap<String, LangRuntimeState>,
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
    /// The source of this answer: `lsp-live`, `lsp-indexed` or `heuristic`.
    pub provenance: &'static str,
    /// What the language server did; `null` when none was considered.
    pub lsp: Option<TargetLspInfo>,
    /// Distinct containers across every relationship group.
    pub caller_count: usize,
    /// References that come from the target's own container (recursion).
    pub self_references: usize,
    /// Callers grouped by relationship.
    pub callers: RelationGroups,
}

/// What changed-file detection did on this call (P6-D3, P6-D7).
#[derive(Debug, Clone, Serialize)]
pub struct CallersEdits {
    pub changed_files: usize,
    pub reparsed: usize,
    pub ms: u64,
}

/// The `knot-lite/callers/v2` document (§10.1).
#[derive(Debug, Clone, Serialize)]
pub struct CallersResponse {
    /// [`CALLERS_SCHEMA_V2`].
    pub schema: &'static str,
    /// knot-lite version.
    pub knot_lite_version: &'static str,
    /// Repository name.
    pub repo: String,
    /// The query as given.
    pub query: String,
    /// The one provenance every target shares, or `mixed`; `heuristic` with no targets.
    pub provenance: &'static str,
    /// The index the answer came from.
    pub index: CallersIndexInfo,
    /// Language-server summary; `null` when servers are off and the index has no
    /// pre-computed data.
    pub lsp: Option<TopLspInfo>,
    /// When changed files mattered, their counts and detection time (P6-D3, P6-D7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edits: Option<CallersEdits>,
    /// How the query was resolved.
    pub resolution: Resolution,
    /// The resolved targets (possibly truncated).
    pub targets: Vec<CallerTarget>,
    /// Caveats and hints.
    pub notes: Vec<String>,
}

/// Answer `find_callers`.
pub fn callers_response(ctx: &CallersContext<'_>) -> CallersResponse {
    let (tier, matched) = match resolve_targets(ctx.index, ctx.query, ctx.filter) {
        Some((tier, matched)) => (Some(tier), matched),
        None => (None, Vec::new()),
    };
    let mut describer = Describer::new(ctx);
    let targets: Vec<CallerTarget> = matched
        .iter()
        .take(ctx.max_targets)
        .map(|&position| describer.describe(position))
        .collect();
    let languages = touched_languages(ctx, &targets);
    let reanchored = describer.reanchored_count();
    let reparsed = describer.reparsed_count();
    let edits = ctx.edits.and_then(|e| {
        let stats = e.stats();
        if reanchored > 0 || reparsed > 0 {
            Some(CallersEdits {
                changed_files: stats.changed,
                reparsed,
                ms: stats.ms,
            })
        } else {
            None
        }
    });
    let notes = response_notes(
        ctx,
        &Findings {
            tier,
            total: matched.len(),
            targets: &targets,
            languages: &languages,
            diagnostics: &describer.into_diagnostics(),
        },
    );
    CallersResponse {
        schema: CALLERS_SCHEMA_V2,
        knot_lite_version: env!("CARGO_PKG_VERSION"),
        repo: ctx.repo.to_owned(),
        query: ctx.query.to_owned(),
        provenance: overall_provenance(&targets),
        index: CallersIndexInfo {
            updated_at: ctx.index.manifest.updated_at.clone(),
        },
        lsp: top_lsp(ctx, languages),
        edits,
        resolution: Resolution {
            tier,
            kind_filter: ctx.filter.wire_label(),
            targets_total: matched.len(),
            targets_shown: targets.len(),
            truncated: targets.len() < matched.len(),
        },
        targets,
        notes,
    }
}

/// The provenance all targets share, `mixed` when they differ.
fn overall_provenance(targets: &[CallerTarget]) -> &'static str {
    match targets.first() {
        None => PROVENANCE_HEURISTIC,
        Some(first) if targets.iter().all(|t| t.provenance == first.provenance) => first.provenance,
        Some(_) => "mixed",
    }
}

fn lang_of(file: &str) -> Option<LspLang> {
    lsp_lang_for_label(label_for(Path::new(file)))
}

/// The state of every language server this call considered.
fn touched_languages(
    ctx: &CallersContext<'_>,
    targets: &[CallerTarget],
) -> BTreeMap<String, LangRuntimeState> {
    let Some(pool) = ctx.lsp.pool else {
        return BTreeMap::new();
    };
    let touched: BTreeSet<LspLang> = targets
        .iter()
        .filter(|target| target.lsp.is_some())
        .filter_map(|target| lang_of(&target.file))
        .collect();
    if touched.is_empty() {
        return BTreeMap::new();
    }
    let snapshot = pool.snapshot();
    touched
        .into_iter()
        .filter_map(|lang| {
            let slot = snapshot.slots.get(lang.dir_name())?;
            Some((
                lang.dir_name().to_owned(),
                LangRuntimeState {
                    state: slot.state.clone(),
                    server: slot.server.clone(),
                    version: slot.version.clone(),
                    ready_ms: slot.ready_ms,
                },
            ))
        })
        .collect()
}

fn top_lsp(
    ctx: &CallersContext<'_>,
    languages: BTreeMap<String, LangRuntimeState>,
) -> Option<TopLspInfo> {
    let live_enabled = ctx.lsp.pool.is_some();
    (live_enabled || !ctx.index.lsp_targets.is_empty()).then(|| TopLspInfo {
        live_enabled,
        live_budget: ctx.lsp.settings.max_live_targets,
        deadline_ms: u64::try_from(ctx.lsp.settings.deadline.as_millis()).unwrap_or(u64::MAX),
        languages,
    })
}

/// What the targets of one call turned out to be, for the notes.
struct Findings<'a> {
    tier: Option<MatchTier>,
    /// Every target the query resolved to (before `max_targets`).
    total: usize,
    targets: &'a [CallerTarget],
    languages: &'a BTreeMap<String, LangRuntimeState>,
    diagnostics: &'a [(&'static str, String)],
}

impl Findings<'_> {
    /// Targets whose language server status is `status`.
    fn with_status(&self, status: &str) -> Vec<&CallerTarget> {
        self.targets
            .iter()
            .filter(|target| target.lsp.as_ref().is_some_and(|lsp| lsp.status == status))
            .collect()
    }

    fn count_provenance(&self, provenance: &str) -> usize {
        self.targets
            .iter()
            .filter(|target| target.provenance == provenance)
            .count()
    }
}

/// Caveats and hints, in a fixed order (§10.5).
fn response_notes(ctx: &CallersContext<'_>, findings: &Findings<'_>) -> Vec<String> {
    let mut notes = Vec::new();
    let query = ctx.query.trim();
    match findings.tier {
        None => notes.push(format!(
            "no indexed entity matches '{query}' (tiers tried: {}; kind filter {}). Use search_hybrid_context to discover names.",
            tiers_tried(query),
            ctx.filter.wire_label()
        )),
        Some(tier) if is_approximate(tier) => notes.push(format!(
            "No entity is named exactly '{query}'; showing {} approximate matches ({}). Use a qualified name to narrow.",
            findings.total,
            tier.as_str()
        )),
        Some(_) => {}
    }
    notes.extend(not_ready_notes(findings));
    let settings = ctx.lsp.settings;
    let budget = findings.with_status("budget").len();
    if budget > 0 {
        notes.push(format!(
            "Language-server answers were limited to the first {} targets (KNOT_LITE_LSP_MAX_LIVE_TARGETS); {budget} target(s) answered from heuristic edges.",
            settings.max_live_targets
        ));
    }
    let reanchored = findings
        .targets
        .iter()
        .filter(|t| t.lsp.as_ref().is_some_and(|l| l.reanchored))
        .count();
    if reanchored > 0 {
        notes.push(format!(
            "re-anchored {reanchored} target(s) in files changed since the last index"
        ));
    }
    let stale = findings.with_status("stale_target").len();
    if stale > 0 {
        notes.push(format!(
            "{stale} target(s) changed since the last index and were answered from heuristic edges; run knot-lite index to refresh."
        ));
    }
    let deadline = findings.with_status("deadline").len();
    if deadline > 0 {
        notes.push(format!(
            "The language-server deadline ({} s, KNOT_LITE_LSP_DEADLINE_SECS) expired; {deadline} target(s) answered from heuristic edges.",
            settings.deadline.as_secs()
        ));
    }
    notes.extend(diagnostic_notes(findings.diagnostics));
    let indexed = findings.count_provenance(PROVENANCE_INDEXED);
    if indexed > 0 {
        notes.push(format!(
            "{indexed} target(s) answered from language-server results pre-computed at {}.",
            ctx.index.manifest.updated_at
        ));
    }
    if findings.count_provenance(PROVENANCE_HEURISTIC) > 0 {
        notes.push(HEURISTIC_NOTE.to_owned());
    }
    notes
}

/// The tiers knot's ladder tries for `query`, in order, in words.
fn tiers_tried(query: &str) -> String {
    knot::db::graph::target_resolution_tiers(query)
        .into_iter()
        .map(|(tier, _)| match tier {
            MatchTier::ExactFqn => "exact FQN",
            MatchTier::FqnSuffix => "FQN suffix",
            MatchTier::ExactName => "exact name",
            MatchTier::SignaturePrefix => "signature prefix",
            MatchTier::Fuzzy => "fuzzy",
            MatchTier::NamePrefix => "name prefix",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One note per language whose server was still starting.
fn not_ready_notes(findings: &Findings<'_>) -> Vec<String> {
    let waiting = findings.with_status("not_ready");
    findings
        .languages
        .iter()
        .filter_map(|(dir_name, state)| {
            let of_language: Vec<&&CallerTarget> = waiting
                .iter()
                .filter(|target| lang_of(&target.file).is_some_and(|l| l.dir_name() == dir_name))
                .collect();
            let elapsed_ms = of_language
                .iter()
                .filter_map(|target| target.lsp.as_ref().map(|lsp| lsp.ms))
                .max()?;
            Some(format!(
                "{} is still starting ({}, {} s): {} target(s) answered from heuristic edges. Retry shortly for language-server answers.",
                state.server,
                state.state,
                elapsed_ms / 1000,
                of_language.len()
            ))
        })
        .collect()
}

/// The reasons the servers gave for not answering, once each.
fn diagnostic_notes(diagnostics: &[(&'static str, String)]) -> BTreeSet<String> {
    diagnostics
        .iter()
        .map(|(status, detail)| match *status {
            "no_server" => detail.clone(),
            other => format!("language-server query {other}: {detail}"),
        })
        .collect()
}

/// Build the format-agnostic document for a `knot-lite/callers/v2` value.
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
        format!(
            "source: {}",
            str_at(value, &["provenance"]).unwrap_or(PROVENANCE_HEURISTIC)
        ),
    ];
    summary.extend(server_summary(value));
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
            "{} {} — {}:{} [{}] ({} callers, {} self-references)",
            str_at(target, &["kind"]).unwrap_or("-"),
            str_at(target, &["name"]).unwrap_or("-"),
            str_at(target, &["file"]).unwrap_or("-"),
            u64_at(target, &["l0"]).unwrap_or(0),
            str_at(target, &["provenance"]).unwrap_or(PROVENANCE_HEURISTIC),
            u64_at(target, &["caller_count"]).unwrap_or(0),
            u64_at(target, &["self_references"]).unwrap_or(0)
        )),
        caption: target.get("lsp").and_then(lsp_caption),
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

/// `incoming 3 · references 5 · implementation –` for an answered target, or
/// the reason a language server did not answer.
fn lsp_caption(lsp: &Value) -> Option<String> {
    let status = str_at(lsp, &["status"])?;
    if !matches!(status, "ok" | "indexed") {
        return Some(format!("language server: {status} (heuristic answer)"));
    }
    let count = |op: &str| {
        lsp.get("ops")
            .and_then(|ops| ops.get(op))
            .and_then(Value::as_u64)
            .map_or_else(|| "–".to_owned(), |n| n.to_string())
    };
    Some(format!(
        "incoming {} · references {} · implementation {}",
        count("incoming"),
        count("references"),
        count("implementation")
    ))
}

/// `servers: rust ready · java starting` for the servers a call touched.
fn server_summary(value: &Value) -> Option<String> {
    let languages = value.pointer("/lsp/languages")?.as_object()?;
    let parts: Vec<String> = languages
        .iter()
        .map(|(language, state)| format!("{language} {}", str_at(state, &["state"]).unwrap_or("-")))
        .collect();
    (!parts.is_empty()).then(|| format!("servers: {}", parts.join(" · ")))
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

/// Builders shared by the tests of the query layer.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::lsp::settings::resolve_lsp_settings;

    /// [`callers_response`] with language servers off: the heuristic answer.
    pub(crate) fn heuristic_response(
        index: &LoadedIndex,
        repo: &str,
        query: &str,
        max_targets: usize,
        filter: &KindFilter,
    ) -> CallersResponse {
        let settings = resolve_lsp_settings(&|_| None, Path::new("/tmp"));
        callers_response(&CallersContext {
            index,
            repo,
            query,
            max_targets,
            filter,
            lsp: LspContext {
                settings: &settings,
                repo_root: Path::new("/nonexistent"),
                pool: None,
            },
            edits: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::heuristic_response;
    use super::*;
    use crate::index::build::test_support::{fixture, run};
    use crate::index::layout::IndexLayout;
    use crate::index::load::LspIncoming;
    use crate::index::load::test_support::{entity, synthetic_index};
    use crate::index::load::{LoadedIndex, load_index};
    use crate::index::lsp_records::LspTargetLine;

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
        // `pha::run` is a textual suffix of `pkg::Alpha::run` but not at a separator:
        // the suffix tier misses and knot's fuzzy tier (a substring) answers instead.
        let (tier, targets) = resolve_targets(&index, "pha::run", &code()).expect("fuzzy");
        assert_eq!(tier, MatchTier::Fuzzy);
        assert_eq!(names(&index, &targets), vec!["pkg::Alpha::run"]);
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

    /// Records named for the approximate tiers: types, callables and a constant.
    fn approximates() -> LoadedIndex {
        let mut parse_args = entity(
            "f-pa",
            "rust_function",
            "parse_args",
            "cli::parse_args",
            (3, 9),
        );
        parse_args.sig = Some("(raw: &[String]) -> Args".to_owned());
        let mut parse = entity("f-p", "rust_function", "parse", "cli::parse", (20, 30));
        parse.sig = Some("(input: &str) -> Ast".to_owned());
        synthetic_index(
            vec![
                ("src/cli.rs", parse_args),
                ("src/cli.rs", parse),
                (
                    "src/model.rs",
                    entity("t-pr", "rust_struct", "Parser", "model::Parser", (1, 8)),
                ),
                (
                    "src/model.rs",
                    entity(
                        "c-pl",
                        "rust_constant",
                        "PARSE_LIMIT",
                        "model::PARSE_LIMIT",
                        (10, 10),
                    ),
                ),
                (
                    "src/zz.rs",
                    entity("f-run", "rust_function", "run", "parser_glue::run", (1, 2)),
                ),
                (
                    "docs/parse.md",
                    entity(
                        "d",
                        "markdown_section",
                        "Parsing",
                        "docs/parse.md::Parsing",
                        (1, 4),
                    ),
                ),
            ],
            &[],
        )
    }

    fn tier_names(tiers: &[(MatchTier, &str)]) -> Vec<MatchTier> {
        tiers.iter().map(|(tier, _)| *tier).collect()
    }

    #[test]
    fn ladder_comes_from_knot_for_bare_dotted_paren_short_queries() {
        use knot::db::graph::target_resolution_tiers as ladder;
        assert_eq!(
            tier_names(&ladder("parse")),
            vec![MatchTier::ExactName, MatchTier::Fuzzy]
        );
        assert_eq!(
            tier_names(&ladder("cli::parse")),
            vec![
                MatchTier::ExactFqn,
                MatchTier::FqnSuffix,
                MatchTier::ExactName,
                MatchTier::Fuzzy
            ]
        );
        assert_eq!(
            tier_names(&ladder("parse(")),
            vec![
                MatchTier::ExactName,
                MatchTier::SignaturePrefix,
                MatchTier::Fuzzy
            ]
        );
        assert_eq!(
            tier_names(&ladder("par")),
            vec![MatchTier::ExactName, MatchTier::NamePrefix]
        );
    }

    #[test]
    fn bare_name_skips_exact_fqn() {
        // A record whose FQN is the bare word `glue` and another named `glue`:
        // without a separator knot's ladder never tries the FQN tier, so the
        // name tier answers (both records, the FQN one only if named so).
        let index = synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("fq", "rust_function", "other", "glue", (1, 2)),
                ),
                (
                    "src/b.rs",
                    entity("nm", "rust_function", "glue", "b::glue", (1, 2)),
                ),
            ],
            &[],
        );
        let (tier, targets) = resolve_targets(&index, "glue", &code()).expect("match");
        assert_eq!(tier, MatchTier::ExactName);
        assert_eq!(names(&index, &targets), vec!["b::glue"]);
    }

    #[test]
    fn signature_prefix_requires_paren() {
        let index = approximates();
        let (tier, targets) = resolve_targets(&index, "parse_args(raw", &code()).expect("sig");
        assert_eq!(tier, MatchTier::SignaturePrefix);
        assert_eq!(names(&index, &targets), vec!["cli::parse_args"]);
        // Without `(` the tier is not tried; `parse_args` is an exact name.
        let (tier, _) = resolve_targets(&index, "parse_args", &code()).expect("name");
        assert_eq!(tier, MatchTier::ExactName);
    }

    #[test]
    fn fuzzy_requires_four_chars_and_is_case_insensitive() {
        let index = approximates();
        let (tier, targets) = resolve_targets(&index, "PARSE_A", &code()).expect("fuzzy");
        assert_eq!(tier, MatchTier::Fuzzy);
        assert_eq!(names(&index, &targets), vec!["cli::parse_args"]);
        // Three characters: name prefix, never a substring.
        let (tier, _) =
            resolve_targets(&index, "ars", &code()).map_or((None, vec![]), |(t, v)| (Some(t), v));
        assert_eq!(
            tier, None,
            "`ars` is inside parse_args but not a name prefix"
        );
    }

    #[test]
    fn fuzzy_matches_fqn_or_name_plus_signature() {
        let index = approximates();
        // Only in an FQN (`parser_glue::run`) and in names: both count.
        let (tier, targets) = resolve_targets(&index, "glue", &code()).expect("fuzzy");
        assert_eq!(tier, MatchTier::Fuzzy);
        assert_eq!(names(&index, &targets), vec!["parser_glue::run"]);
        // Only in a signature (`-> Ast`).
        let (tier, targets) = resolve_targets(&index, "-> ast", &code()).expect("fuzzy");
        assert_eq!(tier, MatchTier::Fuzzy);
        assert_eq!(names(&index, &targets), vec!["cli::parse"]);
    }

    #[test]
    fn name_prefix_for_short_queries_only_on_name() {
        let index = approximates();
        let (tier, targets) = resolve_targets(&index, "Pa", &code()).expect("prefix");
        assert_eq!(tier, MatchTier::NamePrefix);
        assert_eq!(
            names(&index, &targets),
            vec![
                "model::Parser",
                "cli::parse_args",
                "cli::parse",
                "model::PARSE_LIMIT"
            ]
        );
        // `par` is the start of the FQN `parser_glue::run`, but name prefixes ignore FQNs.
        let (_, targets) = resolve_targets(&index, "par", &code()).expect("prefix");
        assert!(!names(&index, &targets).contains(&"parser_glue::run".to_owned()));
    }

    #[test]
    fn approximate_tier_ordering_quality_then_kind_then_location() {
        let index = approximates();
        let (tier, targets) = resolve_targets(&index, "pars", &code()).expect("fuzzy");
        assert_eq!(tier, MatchTier::Fuzzy);
        // Name prefixes first (type, callables by location, constant), then
        // the FQN-only match.
        assert_eq!(
            names(&index, &targets),
            vec![
                "model::Parser",
                "cli::parse_args",
                "cli::parse",
                "model::PARSE_LIMIT",
                "parser_glue::run"
            ]
        );
        // Equality ignoring case ranks first.
        let (_, targets) = resolve_targets(&index, "PARSER", &code()).expect("fuzzy");
        assert_eq!(names(&index, &targets)[0], "model::Parser");
    }

    #[test]
    fn approximate_tier_adds_note() {
        let index = approximates();
        let response = heuristic_response(&index, "demo", "pars", 25, &code());
        assert_eq!(response.resolution.tier, Some(MatchTier::Fuzzy));
        assert!(
            response.notes.contains(
                &"No entity is named exactly 'pars'; showing 5 approximate matches (fuzzy). Use a qualified name to narrow."
                    .to_owned()
            ),
            "{:?}",
            response.notes
        );
        let exact = heuristic_response(&index, "demo", "parse", 25, &code());
        assert!(!exact.notes.iter().any(|n| n.contains("approximate")));
    }

    #[test]
    fn exact_name_wins_over_fuzzy() {
        let index = approximates();
        let (tier, targets) = resolve_targets(&index, "parse", &code()).expect("name");
        assert_eq!(tier, MatchTier::ExactName);
        assert_eq!(names(&index, &targets), vec!["cli::parse"]);
    }

    #[test]
    fn kinds_filter_applies_to_every_tier() {
        let index = approximates();
        let (_, code_only) = resolve_targets(&index, "pars", &code()).expect("fuzzy");
        assert!(!names(&index, &code_only).contains(&"docs/parse.md::Parsing".to_owned()));
        let (_, with_docs) = resolve_targets(&index, "pars", &any()).expect("fuzzy");
        assert!(names(&index, &with_docs).contains(&"docs/parse.md::Parsing".to_owned()));
        let types = KindFilter::parse(Some("type"));
        let (_, only_types) = resolve_targets(&index, "Pa", &types).expect("prefix");
        assert_eq!(names(&index, &only_types), vec!["model::Parser"]);
    }

    #[test]
    fn tier_serialises_snake_case_and_unmatched_note_lists_tiers() {
        for (tier, wire) in [
            (MatchTier::SignaturePrefix, "signature_prefix"),
            (MatchTier::Fuzzy, "fuzzy"),
            (MatchTier::NamePrefix, "name_prefix"),
        ] {
            assert_eq!(serde_json::to_value(tier).expect("json"), wire);
        }
        let index = approximates();
        let response = heuristic_response(&index, "demo", "nothing::here", 25, &code());
        assert_eq!(response.resolution.tier, None);
        assert!(
            response.notes[0].contains(
                "(tiers tried: exact FQN, FQN suffix, exact name, fuzzy; kind filter code_default)"
            ),
            "{:?}",
            response.notes
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
        let response = heuristic_response(&index, "demo", "run", 2, &code());
        assert_eq!(response.targets.len(), 2);
        assert_eq!(response.resolution.targets_total, 3);
        assert_eq!(response.resolution.targets_shown, 2);
        assert!(response.resolution.truncated);
        let full = heuristic_response(&index, "demo", "run", 25, &code());
        assert!(!full.resolution.truncated);
    }

    #[test]
    fn callers_are_containers_with_sites() {
        let (_tmp, index) = fixture_index();
        let response = heuristic_response(&index, "demo", "target", 25, &code());
        assert_eq!(response.resolution.tier, Some(MatchTier::ExactName));
        assert_eq!(response.targets.len(), 1);
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
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
        let response = heuristic_response(&index, "demo", "Widget", 25, &code());
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
        let response = heuristic_response(&index, "demo", "target", 25, &code());
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
        let response = heuristic_response(&index, "demo", "target", 25, &code());
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
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
        let response = heuristic_response(&index, "demo", "does_not_exist", 25, &code());
        assert!(response.targets.is_empty());
        assert_eq!(response.resolution.tier, None);
        assert_eq!(response.resolution.targets_total, 0);
        assert_eq!(response.provenance, "heuristic");
        // The heuristic caveat is only for targets that were answered heuristically.
        assert_eq!(response.notes.len(), 1, "{:?}", response.notes);
        assert!(
            response.notes[0].starts_with("no indexed entity matches 'does_not_exist'"),
            "{:?}",
            response.notes
        );
        assert!(response.notes[0].contains("kind filter code_default"));
        assert!(response.notes[0].contains("search_hybrid_context"));
    }

    #[test]
    fn target_without_callers_is_listed_with_zero() {
        let (_tmp, index) = fixture_index();
        let response = heuristic_response(&index, "demo", "caller", 25, &code());
        assert_eq!(response.targets.len(), 1);
        assert_eq!(response.targets[0].caller_count, 0);
        assert_eq!(response.notes, vec![HEURISTIC_NOTE]);
    }

    #[test]
    fn callers_renderers_snapshot() {
        let (_tmp, index) = fixture_index();
        let response = heuristic_response(&index, "demo", "target", 25, &code());
        let value = serde_json::to_value(&response).expect("json");
        assert_eq!(value["schema"], "knot-lite/callers/v2");
        assert_eq!(value["provenance"], "heuristic");
        assert_eq!(value["resolution"]["tier"], "exact_name");
        assert_eq!(value["resolution"]["kind_filter"], "code_default");
        assert!(value["lsp"].is_null(), "servers off, nothing pre-computed");

        let doc = callers_doc(&value, OutputFormat::Table);
        assert_eq!(doc.title, "knot-lite callers — repository 'demo'");
        assert_eq!(
            doc.sections[0].heading.as_deref(),
            Some("rust_function target — src/lib.rs:3 [heuristic] (3 callers, 0 self-references)")
        );
        assert_eq!(
            doc.sections[0].caption.as_deref(),
            Some("language server: disabled (heuristic answer)")
        );
        assert_eq!(doc.sections[0].rows.len(), 3);
        assert_eq!(
            doc.sections[0].rows[0],
            vec!["CALLS", "caller", "rust_function", "src/lib.rs:7", "1"]
        );
        assert!(doc.summary.contains(&"source: heuristic".to_owned()));
        assert!(
            doc.notes
                .last()
                .expect("note")
                .starts_with("Heuristic edges")
        );
    }
    // ---- language-server sources (P3.11) ----

    use async_lsp::lsp_types::Position;

    use crate::lsp::fake::{FakeScript, FakeServers, Scene, fixture_target_script};
    use crate::lsp::pool::PoolSettings;
    use crate::lsp::settings::resolve_lsp_settings;

    fn default_settings() -> LspSettings {
        resolve_lsp_settings(&|_| None, Path::new("/tmp"))
    }

    fn respond(
        index: &LoadedIndex,
        root: &Path,
        settings: &LspSettings,
        pool: Option<&LspPool>,
        query: &str,
    ) -> CallersResponse {
        let edits = crate::query::edits::EditedFiles::new(index, root);
        callers_response(&CallersContext {
            index,
            repo: "demo",
            query,
            max_targets: 25,
            filter: &code(),
            lsp: LspContext {
                settings,
                repo_root: root,
                pool,
            },
            edits: Some(&edits),
        })
    }

    fn id_of(index: &LoadedIndex, name: &str) -> String {
        index.records[index.by_name[name][0]].line.id.clone()
    }

    fn rows(target: &CallerTarget, relation: &str) -> Vec<(String, usize)> {
        target
            .callers
            .0
            .iter()
            .filter(|(rel, _)| rel == relation)
            .flat_map(|(_, rows)| rows.iter().map(|r| (r.name.clone(), r.sites)))
            .collect()
    }

    fn status_of(target: &CallerTarget) -> &'static str {
        target.lsp.as_ref().map_or("-", |lsp| lsp.status)
    }

    #[test]
    fn live_target_uses_pool() {
        let (tmp, index) = fixture_index();
        let servers = FakeServers::new(fixture_target_script(tmp.path()));
        let starts = servers.starts();
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );

        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(target.provenance, "lsp-live");
        let lsp = target.lsp.as_ref().expect("lsp");
        assert_eq!(lsp.status, "ok");
        assert_eq!(
            (lsp.ops.incoming, lsp.ops.references, lsp.ops.implementation),
            (Some(2), Some(3), None)
        );
        assert_eq!(lsp.heuristic_extra, 0);
        // The call sites are not listed again as references; `run` only references.
        assert_eq!(
            rows(target, "CALLS"),
            vec![("caller".to_owned(), 1), ("helper".to_owned(), 1)]
        );
        assert_eq!(rows(target, "REFERENCES"), vec![("run".to_owned(), 1)]);
        assert_eq!((target.caller_count, target.self_references), (3, 0));
        assert_eq!(response.provenance, "lsp-live");
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 1);

        let top = response.lsp.as_ref().expect("top-level lsp");
        assert!(top.live_enabled);
        assert_eq!((top.live_budget, top.deadline_ms), (10, 30_000));
        assert_eq!(top.languages["rust"].state, "ready");
        assert_eq!(top.languages["rust"].server, "rust-analyzer");
    }

    #[test]
    fn heuristic_note_dropped_when_all_targets_lsp() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        assert!(response.notes.is_empty(), "{:?}", response.notes);
    }

    #[test]
    fn indexed_target_uses_lsp_indexed_without_pool() {
        let (tmp, mut index) = fixture_index();
        let target = id_of(&index, "target");
        let (caller, helper) = (id_of(&index, "caller"), id_of(&index, "helper"));
        index.lsp_targets.insert(
            target.clone(),
            LspTargetLine {
                id: target.clone(),
                status: "ok".to_owned(),
                ms: 812,
                ops: OpCounts {
                    incoming: Some(2),
                    references: Some(3),
                    implementation: None,
                },
                server: "rust-analyzer".to_owned(),
                refs: None,
                h: None,
            },
        );
        let edge = |rel: &str, src: Option<&str>, relation: &str, n: u32| LspIncoming {
            rel: rel.to_owned(),
            src: src.map(str::to_owned),
            relation: relation.to_owned(),
            n,
        };
        index.lsp_incoming.insert(
            target,
            vec![
                edge("src/lib.rs", Some(&caller), "CALLS", 1),
                edge("src/util.rs", Some(&helper), "CALLS", 2),
                edge("src/main.rs", None, "REFERENCES", 1),
            ],
        );

        // No pool: pre-computed edges are data, used even with servers off.
        let response = respond(&index, tmp.path(), &default_settings(), None, "target");
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(target.provenance, "lsp-indexed");
        let lsp = target.lsp.as_ref().expect("lsp");
        assert_eq!((lsp.status, lsp.ms), ("indexed", 812));
        assert_eq!(
            rows(target, "CALLS"),
            vec![("caller".to_owned(), 1), ("helper".to_owned(), 2)]
        );
        assert_eq!(rows(target, "REFERENCES"), vec![("<file>".to_owned(), 1)]);
        assert_eq!(target.caller_count, 3);
        // The heuristic also found `run`, which the pre-computed answer does not have.
        assert_eq!(lsp.heuristic_extra, 1);

        let top = response.lsp.as_ref().expect("pre-computed data counts");
        assert!(!top.live_enabled);
        assert!(top.languages.is_empty());
        assert_eq!(response.provenance, "lsp-indexed");
        assert_eq!(
            response.notes,
            vec![format!(
                "1 target(s) answered from language-server results pre-computed at {}.",
                index.manifest.updated_at
            )]
        );
    }

    #[test]
    fn heuristic_extra_counts_containers_lsp_did_not_return() {
        let (tmp, index) = fixture_index();
        let scene = Scene::new(tmp.path());
        let pool = FakeServers::new(move |_| {
            let position = Position::new(2, 7);
            FakeScript {
                prepare: vec![(position, vec![scene.item("target", "src/lib.rs", 2)])],
                incoming: vec![(
                    "target".to_owned(),
                    vec![scene.call("caller", "src/lib.rs", 6)],
                )],
                ..FakeScript::full()
            }
        })
        .pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        // The server's answer wins; `helper` and `run` are the heuristic's extras.
        assert_eq!(target.caller_count, 1);
        assert_eq!(target.lsp.as_ref().expect("lsp").heuristic_extra, 2);
        assert_eq!(rows(target, "CALLS"), vec![("caller".to_owned(), 1)]);
    }

    /// Two files that each declare `pub fn run() {}` plus an index of them.
    fn homonyms_on_disk(
        records: Vec<(&str, crate::index::shard::ShardLine)>,
    ) -> (tempfile::TempDir, LoadedIndex) {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        for file in ["src/a.rs", "src/b.rs"] {
            std::fs::write(tmp.path().join(file), "pub fn run() {}\n").expect("write");
        }
        (tmp, synthetic_index(records, &[]))
    }

    fn located(
        id: &str,
        kind: &str,
        name: &str,
        lines: (usize, usize),
    ) -> crate::index::shard::ShardLine {
        let mut line = entity(id, kind, name, &format!("pkg::{name}"), lines);
        line.col = Some(7);
        line
    }

    fn two_runs() -> (tempfile::TempDir, LoadedIndex) {
        homonyms_on_disk(vec![
            ("src/a.rs", located("a", "rust_function", "run", (1, 1))),
            ("src/b.rs", located("b", "rust_function", "run", (1, 1))),
        ])
    }

    #[test]
    fn live_budget_limits_lsp_targets_in_resolution_order() {
        let (tmp, index) = two_runs();
        let settings = LspSettings {
            max_live_targets: 1,
            ..default_settings()
        };
        let pool = FakeServers::new(|_| FakeScript::full()).pool();
        let response = respond(&index, tmp.path(), &settings, Some(&pool), "run");

        let statuses: Vec<(&str, &str)> = response
            .targets
            .iter()
            .map(|t| (t.file.as_str(), status_of(t)))
            .collect();
        assert_eq!(statuses, vec![("src/a.rs", "ok"), ("src/b.rs", "budget")]);
        assert_eq!(response.provenance, "mixed");
        assert!(response.notes.contains(&"Language-server answers were limited to the first 1 targets (KNOT_LITE_LSP_MAX_LIVE_TARGETS); 1 target(s) answered from heuristic edges.".to_owned()), "{:?}", response.notes);
        // One target is heuristic, so the caveat stays.
        assert!(response.notes.contains(&HEURISTIC_NOTE.to_owned()));
    }

    #[test]
    fn deadline_stops_further_live_targets() {
        let (tmp, index) = two_runs();
        let settings = LspSettings {
            deadline: std::time::Duration::ZERO,
            ..default_settings()
        };
        let servers = FakeServers::new(|_| FakeScript::full());
        let starts = servers.starts();
        let pool = servers.pool();
        let response = respond(&index, tmp.path(), &settings, Some(&pool), "run");

        assert!(response.targets.iter().all(|t| status_of(t) == "deadline"));
        assert!(response.targets.iter().all(|t| t.provenance == "heuristic"));
        assert!(response.notes.contains(&"The language-server deadline (0 s, KNOT_LITE_LSP_DEADLINE_SECS) expired; 2 target(s) answered from heuristic edges.".to_owned()), "{:?}", response.notes);
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn unsupported_kind_falls_back_to_heuristic() {
        let (tmp, index) = homonyms_on_disk(vec![(
            "src/a.rs",
            located("i", "rust_impl", "Thing", (1, 1)),
        )]);
        let pool = FakeServers::missing().pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "Thing",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            (target.provenance, status_of(target)),
            ("heuristic", "unsupported_kind")
        );
    }

    #[test]
    fn missing_col_falls_back() {
        let (tmp, index) = homonyms_on_disk(vec![(
            "src/a.rs",
            entity("f", "rust_function", "run", "pkg::run", (1, 1)),
        )]);
        let pool = FakeServers::missing().pool();
        let response = respond(&index, tmp.path(), &default_settings(), Some(&pool), "run");
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            (target.provenance, status_of(target)),
            ("heuristic", "no_column")
        );
    }

    #[test]
    fn non_lsp_languages_are_not_asked_and_report_no_lsp() {
        let (tmp, index) = homonyms_on_disk(vec![(
            "docs/guide.md",
            entity(
                "g",
                "markdown_section",
                "Setup",
                "docs/guide.md::Setup",
                (1, 4),
            ),
        )]);
        let pool = FakeServers::missing().pool();
        let filter = any();
        let response = callers_response(&CallersContext {
            index: &index,
            repo: "demo",
            query: "Setup",
            max_targets: 25,
            filter: &filter,
            lsp: LspContext {
                settings: &default_settings(),
                repo_root: tmp.path(),
                pool: Some(&pool),
            },
            edits: None,
        });
        assert!(response.targets[0].lsp.is_none());
        let value = serde_json::to_value(&response).expect("json");
        assert!(value["targets"][0]["lsp"].is_null());
    }

    #[test]
    fn shifted_target_is_asked_at_fresh_position() {
        let (tmp, index) = fixture_index();
        let lib = tmp.path().join("src/lib.rs");
        let text = std::fs::read_to_string(&lib).expect("read");
        let shifted =
            format!("// 1\n// 2\n// 3\n// 4\n// 5\n// 6\n// 7\n// 8\n// 9\n// 10\n{text}");
        std::fs::write(&lib, shifted).expect("write");

        let scene = Scene::new(tmp.path());
        let fresh_pos = async_lsp::lsp_types::Position::new(12, 7);
        let servers = FakeServers::new(move |_| FakeScript {
            prepare: vec![(fresh_pos, vec![scene.item("target", "src/lib.rs", 12)])],
            incoming: vec![(
                "target".to_owned(),
                vec![scene.call("caller", "src/lib.rs", 16)],
            )],
            references: vec![(fresh_pos, vec![scene.location("src/lib.rs", 17)])],
            ..FakeScript::full()
        });
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            (target.provenance, status_of(target)),
            ("lsp-live", "ok"),
            "target is asked at fresh position and answered live"
        );
        assert!(target.lsp.as_ref().is_some_and(|l| l.reanchored));
        assert!(
            response
                .notes
                .iter()
                .any(|n| n.contains("re-anchored 1 target(s)"))
        );
    }

    #[test]
    fn deleted_declaration_is_stale_target() {
        let (tmp, index) = fixture_index();
        let lib = tmp.path().join("src/lib.rs");
        std::fs::write(&lib, "pub fn unrelated() {}\n").expect("write");
        let servers = FakeServers::new(fixture_target_script(tmp.path()));
        let starts = servers.starts();
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            (target.provenance, status_of(target)),
            ("heuristic", "stale_target")
        );
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(response.notes.iter().any(|n| {
            n.contains("changed since the last index and were answered from heuristic edges")
        }));
    }

    #[test]
    fn reanchored_flag_and_note() {
        let (tmp, index) = fixture_index();
        let lib = tmp.path().join("src/lib.rs");
        let text = std::fs::read_to_string(&lib).expect("read");
        std::fs::write(&lib, format!("{text}\n// edited\n")).expect("edit");
        let servers = FakeServers::new(fixture_target_script(tmp.path()));
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert!(target.lsp.as_ref().unwrap().reanchored);
        assert!(
            response
                .notes
                .iter()
                .any(|n| n.starts_with("re-anchored 1 target(s)"))
        );
    }

    #[test]
    fn unchanged_file_keeps_old_path() {
        let (tmp, index) = fixture_index();
        let servers = FakeServers::new(fixture_target_script(tmp.path()));
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert!(!target.lsp.as_ref().unwrap().reanchored);
        assert!(!response.notes.iter().any(|n| n.contains("re-anchored")));
    }

    #[test]
    fn reparse_limit_respected() {
        let (tmp, index) = fixture_index();
        let lib = tmp.path().join("src/lib.rs");
        let big = "pub fn target() {}\n".to_owned() + &"// pad\n".repeat(40_000);
        std::fs::write(&lib, big).expect("write");
        let servers = FakeServers::new(fixture_target_script(tmp.path()));
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            (target.provenance, status_of(target)),
            ("heuristic", "stale_target")
        );
    }

    #[test]
    fn location_in_changed_file_uses_fresh_container() {
        let (tmp, index) = fixture_index();
        let util = tmp.path().join("src/util.rs");
        let text = std::fs::read_to_string(&util).expect("read");
        let shifted = "// pad\n".repeat(10) + &text;
        std::fs::write(&util, shifted).expect("write");

        let scene = Scene::new(tmp.path());
        let pos = Position::new(2, 7);
        let servers = FakeServers::new(move |_| FakeScript {
            prepare: vec![(pos, vec![scene.item("target", "src/lib.rs", 2)])],
            incoming: vec![(
                "target".to_owned(),
                vec![scene.call("helper", "src/util.rs", 10)],
            )],
            references: vec![(pos, vec![scene.location("src/util.rs", 11)])],
            ..FakeScript::full()
        });
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        assert_eq!(target.provenance, "lsp-live");
        let calls = &target
            .callers
            .0
            .iter()
            .find(|(rel, _)| rel == "CALLS")
            .unwrap()
            .1;
        let helper = calls.iter().find(|c| c.name == "helper").unwrap();
        assert_eq!(helper.l0, 11, "helper container must use the fresh line 11");
        assert!(helper.indexed, "matched container keeps indexed true");
        assert!(helper.id.is_some(), "matched container keeps its id");
    }

    #[test]
    fn new_container_has_indexed_false() {
        let (tmp, index) = fixture_index();
        let util = tmp.path().join("src/util.rs");
        let text = std::fs::read_to_string(&util).expect("read");
        let with_new = format!("{text}\n\npub fn brand_new_caller() {{\n    target();\n}}\n");
        std::fs::write(&util, with_new).expect("write");

        let scene = Scene::new(tmp.path());
        let pos = Position::new(2, 7);
        let servers = FakeServers::new(move |_| FakeScript {
            prepare: vec![(pos, vec![scene.item("target", "src/lib.rs", 2)])],
            incoming: vec![(
                "target".to_owned(),
                vec![scene.call("brand_new_caller", "src/util.rs", 15)],
            )],
            references: vec![(pos, vec![scene.location("src/util.rs", 16)])],
            ..FakeScript::full()
        });
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        let calls = &target
            .callers
            .0
            .iter()
            .find(|(rel, _)| rel == "CALLS")
            .unwrap()
            .1;
        let new_c = calls.iter().find(|c| c.name == "brand_new_caller").unwrap();
        assert!(
            !new_c.indexed,
            "new container unknown to index must have indexed == false"
        );
        assert_eq!(new_c.id, None, "new container has id None");
    }

    #[test]
    fn self_reference_uses_fresh_own_container() {
        let (tmp, index) = fixture_index();
        let lib = tmp.path().join("src/lib.rs");
        let text = std::fs::read_to_string(&lib).expect("read");
        let shifted = "// pad\n".repeat(10) + &text;
        std::fs::write(&lib, shifted).expect("write");

        let scene = Scene::new(tmp.path());
        let fresh_pos = Position::new(12, 7);
        let servers = FakeServers::new(move |_| FakeScript {
            prepare: vec![(fresh_pos, vec![scene.item("target", "src/lib.rs", 12)])],
            incoming: vec![(
                "target".to_owned(),
                vec![
                    scene.call("target", "src/lib.rs", 13),
                    scene.call("caller", "src/lib.rs", 16),
                ],
            )],
            references: vec![(
                fresh_pos,
                vec![
                    scene.location("src/lib.rs", 14),
                    scene.location("src/lib.rs", 17),
                ],
            )],
            ..FakeScript::full()
        });
        let pool = servers.pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            target.self_references, 1,
            "recursion inside fresh own container is excluded and counted"
        );
    }

    #[test]
    fn not_ready_falls_back_with_note() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path()))
            .starting_for(std::time::Duration::from_millis(600))
            .pool_with(PoolSettings {
                ready_wait: std::time::Duration::from_millis(50),
                ..PoolSettings::default()
            });
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            (target.provenance, status_of(target)),
            ("heuristic", "not_ready")
        );
        let note = response
            .notes
            .iter()
            .find(|note| note.contains("is still starting"))
            .unwrap_or_else(|| panic!("{:?}", response.notes));
        assert!(
            note.starts_with("rust-analyzer is still starting (starting, "),
            "{note}"
        );
        assert!(note.ends_with("s): 1 target(s) answered from heuristic edges. Retry shortly for language-server answers."), "{note}");
    }

    #[test]
    fn missing_server_reports_its_diagnostic() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::missing().pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            (target.provenance, status_of(target)),
            ("heuristic", "no_server")
        );
        assert!(
            response
                .notes
                .contains(&"rust-analyzer not found in PATH".to_owned()),
            "{:?}",
            response.notes
        );
    }

    #[test]
    fn summary_provenance_mixed_and_uniform() {
        let target = |provenance| CallerTarget {
            id: String::new(),
            kind: String::new(),
            name: String::new(),
            fqn: String::new(),
            file: String::new(),
            l0: 0,
            l1: 0,
            signature: None,
            provenance,
            lsp: None,
            caller_count: 0,
            self_references: 0,
            callers: RelationGroups::default(),
        };
        assert_eq!(overall_provenance(&[]), "heuristic");
        assert_eq!(overall_provenance(&[target("lsp-live")]), "lsp-live");
        assert_eq!(
            overall_provenance(&[target("lsp-indexed"), target("lsp-indexed")]),
            "lsp-indexed"
        );
        assert_eq!(
            overall_provenance(&[target("lsp-live"), target("heuristic")]),
            "mixed"
        );
    }

    #[test]
    fn render_shows_source_column() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let value = serde_json::to_value(&response).expect("json");

        let doc = callers_doc(&value, OutputFormat::Table);
        assert_eq!(
            doc.sections[0].heading.as_deref(),
            Some("rust_function target — src/lib.rs:3 [lsp-live] (3 callers, 0 self-references)")
        );
        assert_eq!(
            doc.sections[0].caption.as_deref(),
            Some("incoming 2 · references 3 · implementation –")
        );
        assert!(doc.summary.contains(&"source: lsp-live".to_owned()));
        assert!(
            doc.summary.contains(&"servers: rust ready".to_owned()),
            "{:?}",
            doc.summary
        );

        let table = crate::query::render::render(&doc, &value, OutputFormat::Table);
        assert!(
            table.contains("[lsp-live]") && table.contains("incoming 2 · references 3"),
            "{table}"
        );
        let markdown = crate::query::render::render(&doc, &value, OutputFormat::Markdown);
        assert!(markdown.contains("### rust_function target — src/lib.rs:3 [lsp-live] (3 callers, 0 self-references)\n\nincoming 2 · references 3 · implementation –\n\n"), "{markdown}");
    }

    #[test]
    fn m6_heuristic_answers_unchanged_when_lsp_disabled() {
        let (tmp, index) = fixture_index();
        let response = respond(&index, tmp.path(), &default_settings(), None, "target");
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(target.provenance, "heuristic");
        assert_eq!(status_of(target), "disabled");
        assert_eq!(target.caller_count, 3);
        assert_eq!(
            rows(target, "CALLS"),
            vec![
                ("caller".to_owned(), 1),
                ("helper".to_owned(), 1),
                ("run".to_owned(), 1)
            ]
        );
        assert!(response.lsp.is_none());
        assert_eq!(response.notes, vec![HEURISTIC_NOTE.to_owned()]);
    }

    /// Replace what changes between runs (ids, times, versions) with markers.
    fn normalised(mut value: Value) -> Value {
        fn walk(value: &mut Value) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map.iter_mut() {
                        match key.as_str() {
                            "id" => *child = Value::from("<id>"),
                            "updated_at" => *child = Value::from("<time>"),
                            "knot_lite_version" => *child = Value::from("<version>"),
                            "ms" | "ready_ms" => *child = Value::from(0),
                            _ => walk(child),
                        }
                    }
                }
                Value::Array(items) => items.iter_mut().for_each(walk),
                _ => {}
            }
        }
        walk(&mut value);
        value
    }

    #[test]
    fn v2_schema_and_wire_shape_are_pinned() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        let response = respond(
            &index,
            tmp.path(),
            &default_settings(),
            Some(&pool),
            "target",
        );
        let caller = |name: &str, fqn: &str, kind: &str, file: &str, l0: u32, sig: &str| {
            serde_json::json!({
                "id": "<id>", "kind": kind, "name": name, "fqn": fqn, "file": file,
                "l0": l0, "signature": sig, "sites": 1, "indexed": true
            })
        };
        let expected = serde_json::json!({
            "schema": "knot-lite/callers/v2",
            "knot_lite_version": "<version>",
            "repo": "demo",
            "query": "target",
            "provenance": "lsp-live",
            "index": { "updated_at": "<time>" },
            "lsp": {
                "live_enabled": true,
                "live_budget": 10,
                "deadline_ms": 30000,
                "languages": {
                    "rust": { "state": "ready", "server": "rust-analyzer", "version": "", "ready_ms": 0 }
                }
            },
            "resolution": {
                "tier": "exact_name", "kind_filter": "code_default",
                "targets_total": 1, "targets_shown": 1, "truncated": false
            },
            "targets": [{
                "id": "<id>", "kind": "rust_function", "name": "target",
                "fqn": "fixture::target", "file": "src/lib.rs", "l0": 3, "l1": 5,
                "signature": "()",
                "provenance": "lsp-live",
                "lsp": {
                    "status": "ok",
                    "ops": { "incoming": 2, "references": 3, "implementation": null },
                    "external": 0, "comment_filtered": 0, "retried_empty": false,
                    "ms": 0, "heuristic_extra": 0, "reanchored": false
                },
                "caller_count": 3, "self_references": 0,
                "callers": {
                    "CALLS": [
                        caller("caller", "fixture::caller", "rust_function", "src/lib.rs", 7, "()"),
                        caller("helper", "fixture::util::helper", "rust_function", "src/util.rs", 1, "()")
                    ],
                    "REFERENCES": [
                        caller("run", "fixture::util::Widget::run", "rust_method", "src/util.rs", 8, "(&self)")
                    ]
                }
            }],
            "notes": []
        });
        let actual = normalised(serde_json::to_value(&response).expect("json"));
        assert_eq!(actual, expected, "{actual:#}");
    }

    #[test]
    fn a_precomputed_target_that_did_not_answer_is_not_used() {
        let (tmp, mut index) = fixture_index();
        let target = id_of(&index, "target");
        index.lsp_targets.insert(
            target.clone(),
            LspTargetLine {
                id: target,
                status: "timeout".to_owned(),
                ms: 1,
                ops: OpCounts::default(),
                server: String::new(),
                refs: None,
                h: None,
            },
        );
        let response = respond(&index, tmp.path(), &default_settings(), None, "target");
        let target = &response.targets[0];
        println!("TARGET={:#?}", target);
        assert_eq!(
            (target.provenance, status_of(target)),
            ("heuristic", "disabled")
        );
    }
}
