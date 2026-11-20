//! Where one target's callers come from (P3-D10).
//!
//! Per target, the first source that applies answers:
//!
//! 1. `lsp-indexed` — the target is in `lsp-targets` with status `ok`: the
//!    language-server edges pre-computed at index time (data, used even when
//!    live LSP is off);
//! 2. `lsp-live` — a language server is asked now, within the live budget and
//!    the call's deadline, on a file that is unchanged since the index;
//! 3. `heuristic` — the index's own edges.
//!
//! Every source reports an [`Answer`]: callers merged to their innermost
//! container and grouped by relationship, in the shape `find_callers` returns.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

use knot::pipeline::parser::ParseConfig;

use super::callers::{CallerRow, CallerTarget, CallersContext, RelationGroups, TargetLspInfo};
use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::graph::containers::{
    ContainerKey, ContainerLookup, FILE_LEVEL_NAME, FreshContainer, FreshOverlay,
};
use crate::graph::heuristic::EDGE_REL_LABELS;
use crate::index::load::{LoadedIndex, LoadedRecord};
use crate::index::lsp_records::LspTargetLine;
use crate::index::reparse::{
    FreshFile, FreshFiles, MAX_REPARSE_BYTES, MAX_REPARSE_PER_CALL, ReparseError, match_all,
};
use crate::index::shard::ShardLine;
use crate::lsp::normalize::{LspGroups, RawLocation, TargetInfo, group_locations};
use crate::lsp::ops::{TargetShape, shape_of};
use crate::lsp::pool::LspPool;
use crate::lsp::request::{Pacing, SourceFiles, Target, Unlocated, live_request};
use crate::lsp::target::{LiveOutcome, LiveRequest, LiveStatus};

/// Provenance of an answer produced by a language server asked now.
pub const PROVENANCE_LIVE: &str = "lsp-live";
/// Provenance of an answer pre-computed at index time.
pub const PROVENANCE_INDEXED: &str = "lsp-indexed";
/// Provenance of an answer from the index's own edges.
pub const PROVENANCE_HEURISTIC: &str = "heuristic";

/// A merged reference container: its record (if any) and how many reference
/// sites were merged into it.
type ContainerHit = (Option<usize>, usize);

/// The callers of one target as one source reports them.
pub struct Answer {
    /// Callers grouped by relationship.
    pub groups: RelationGroups,
    /// Distinct containers across every group.
    pub caller_count: usize,
    /// References that come from the target's own container (recursion).
    pub self_references: usize,
    /// The containers themselves (sources are compared through them).
    pub containers: BTreeSet<ContainerKey>,
}

/// Accumulates `(container, relationship, sites)` triples into an [`Answer`].
struct Grouper<'a> {
    index: &'a LoadedIndex,
    own: ContainerKey,
    by_relation: BTreeMap<(usize, String), BTreeMap<ContainerKey, ContainerHit>>,
    self_references: usize,
}

impl<'a> Grouper<'a> {
    fn new(index: &'a LoadedIndex, position: usize) -> Self {
        Self {
            index,
            own: own_container(index, position),
            by_relation: BTreeMap::new(),
            self_references: 0,
        }
    }

    fn add(
        &mut self,
        (key, container): (ContainerKey, Option<usize>),
        relation: &str,
        sites: usize,
    ) {
        if key == self.own {
            self.self_references += sites;
            return;
        }
        let order = EDGE_REL_LABELS
            .iter()
            .position(|label| *label == relation)
            .unwrap_or(usize::MAX);
        self.by_relation
            .entry((order, relation.to_owned()))
            .or_default()
            .entry(key)
            .or_insert((container, 0))
            .1 += sites;
    }

    fn finish(self) -> Answer {
        let Self {
            index,
            by_relation,
            self_references,
            ..
        } = self;
        let containers: BTreeSet<ContainerKey> = by_relation
            .values()
            .flat_map(BTreeMap::keys)
            .cloned()
            .collect();
        let groups = by_relation
            .into_iter()
            .map(|((_, relation), hits)| {
                let rows = hits
                    .into_iter()
                    .map(|(key, (container, sites))| caller_row(index, key, container, None, sites))
                    .collect();
                (relation, rows)
            })
            .collect();
        Answer {
            groups: RelationGroups(groups),
            caller_count: containers.len(),
            self_references,
            containers,
        }
    }
}

/// Distinct caller containers of a live answer about a target of `shape`
/// declared in `own` — the grouping `find_callers` uses (P5-D14).
pub(crate) fn live_container_count(
    _index: &LoadedIndex,
    outcome: &LiveOutcome,
    shape: TargetShape,
    own: ContainerKey,
    containers: &ContainerLookup<'_>,
) -> usize {
    group_locations(&outcome.locations, &TargetInfo { shape, own }, containers)
        .containers
        .len()
}

/// The innermost container of the record at `position`'s own declaration.
pub(crate) fn own_container(index: &LoadedIndex, position: usize) -> ContainerKey {
    let target = &index.records[position];
    index
        .containers
        .container_at(&target.file, target.line.l0)
        .0
}

/// The container of the record `id` (at its own start line), if it is indexed.
fn container_of_record(index: &LoadedIndex, id: &str) -> Option<(ContainerKey, Option<usize>)> {
    let record = &index.records[*index.by_id.get(id)?];
    Some(index.containers.container_at(&record.file, record.line.l0))
}

/// Who references the record at `position` according to the index's heuristic edges.
pub fn heuristic_answer(index: &LoadedIndex, position: usize) -> Answer {
    let mut grouper = Grouper::new(index, position);
    let target = &index.records[position];
    for (src, relation) in index.incoming.get(&target.line.id).into_iter().flatten() {
        if let Some(container) = container_of_record(index, src) {
            grouper.add(container, relation, 1);
        }
    }
    grouper.finish()
}

/// Who references the record at `position` according to the pre-computed
/// language-server edges.
pub fn indexed_answer(index: &LoadedIndex, position: usize) -> Answer {
    let mut grouper = Grouper::new(index, position);
    let target = &index.records[position];
    for incoming in index
        .lsp_incoming
        .get(&target.line.id)
        .into_iter()
        .flatten()
    {
        let container = match incoming.src.as_deref() {
            Some(id) => container_of_record(index, id),
            None => Some((ContainerKey::file_level(&incoming.rel), None)),
        };
        if let Some(container) = container {
            let sites = usize::try_from(incoming.n).unwrap_or(usize::MAX);
            grouper.add(container, &incoming.relation, sites);
        }
    }
    grouper.finish()
}

/// Who references the record at `position` according to a live outcome.
fn live_answer(
    index: &LoadedIndex,
    own: ContainerKey,
    position: usize,
    outcome: &LiveOutcome,
    containers: &ContainerLookup<'_>,
) -> Answer {
    let target = TargetInfo {
        shape: shape_of(&index.records[position].line.kind),
        own,
    };
    let LspGroups {
        groups,
        self_references,
        containers: group_containers,
    } = group_locations(&outcome.locations, &target, containers);
    let groups = groups
        .into_iter()
        .map(|(relation, hits)| {
            let rows = hits
                .into_iter()
                .map(|hit| caller_row(index, hit.key, hit.record, hit.fresh.as_ref(), hit.sites))
                .collect();
            (relation, rows)
        })
        .collect();
    Answer {
        groups: RelationGroups(groups),
        caller_count: group_containers.len(),
        self_references,
        containers: group_containers,
    }
}

/// One caller row: the container's record, or the file level.
pub fn caller_row(
    index: &LoadedIndex,
    key: ContainerKey,
    container: Option<usize>,
    fresh: Option<&FreshContainer>,
    sites: usize,
) -> CallerRow {
    if let Some(pos) = container {
        let record = &index.records[pos];
        CallerRow {
            id: Some(record.line.id.clone()),
            kind: record.line.kind.clone(),
            name: record.line.name.clone(),
            fqn: Some(record.line.fqn.clone()),
            file: record.file.clone(),
            l0: if key.line > 0 {
                key.line
            } else {
                record.line.l0
            },
            signature: record.line.sig.clone(),
            sites,
            indexed: true,
        }
    } else if let Some(fresh) = fresh {
        CallerRow {
            id: None,
            kind: fresh.kind.clone(),
            name: key.name,
            fqn: Some(fresh.fqn.clone()),
            file: key.file,
            l0: key.line,
            signature: fresh.signature.clone(),
            sites,
            indexed: false,
        }
    } else {
        CallerRow {
            id: None,
            kind: "file".to_owned(),
            name: FILE_LEVEL_NAME.to_owned(),
            fqn: None,
            file: key.file,
            l0: 0,
            signature: None,
            sites,
            indexed: true,
        }
    }
}

/// Why a pre-computed answer is no longer current (P6-D8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outdated {
    /// The target's own file changed.
    OwnFile,
    /// A file that references the target changed.
    EdgeFile(String),
    /// A changed file mentions the target's name as an identifier.
    Mention(String),
}

impl Outdated {
    /// Human-readable reason for the diagnostic notes.
    pub fn reason(&self) -> String {
        match self {
            Self::OwnFile => "its file changed".to_owned(),
            Self::EdgeFile(rel) => format!("a file that references it changed ({rel})"),
            Self::Mention(rel) => format!("{rel} changed and mentions it"),
        }
    }
}

/// P6-D8 freshness rule: a pre-computed answer is current iff (a) its own
/// file is unchanged, (b) no pre-computed edge comes from a changed file,
/// and (c) no changed file of its language contains the target's name as an
/// identifier.
pub fn indexed_is_current(
    index: &LoadedIndex,
    edits: Option<&crate::query::edits::EditedFiles>,
    files: &mut SourceFiles<'_>,
    record: &LoadedRecord,
) -> Result<(), Outdated> {
    let Some(edits) = edits else {
        return Ok(());
    };
    // (a) own file unchanged, hashed whatever its size and mtime
    if edits.is_target_changed(&record.file) {
        return Err(Outdated::OwnFile);
    }
    if !edits.has_changes() {
        return Ok(());
    }
    // (b) no pre-computed edge from a changed file
    if let Some(incoming) = index.lsp_incoming.get(&record.line.id) {
        for edge in incoming {
            if edits.is_changed(&edge.rel) {
                return Err(Outdated::EdgeFile(edge.rel.clone()));
            }
        }
    }
    // (c) no changed file of its language contains the target's name as an identifier
    let Some(lang) = lsp_lang_for_label(label_for(Path::new(&record.file))) else {
        return Ok(());
    };
    for rel in edits.changed(lang) {
        if let Ok(source) = files.get(&rel)
            && crate::lsp::position::text_has_ident(source.text(), &record.line.name)
        {
            return Err(Outdated::Mention(rel));
        }
    }
    Ok(())
}

fn indexed_info(line: &LspTargetLine, heuristic_extra: usize, outdated: bool) -> TargetLspInfo {
    TargetLspInfo {
        status: "indexed",
        ops: line.ops,
        external: 0,
        comment_filtered: 0,
        retried_empty: false,
        ms: line.ms,
        heuristic_extra,
        reanchored: false,
        outdated,
    }
}

fn live_info(outcome: &LiveOutcome, heuristic_extra: usize, reanchored: bool) -> TargetLspInfo {
    TargetLspInfo {
        status: outcome.status.as_str(),
        ops: outcome.op_counts,
        external: outcome.external,
        comment_filtered: outcome.comment_filtered,
        retried_empty: outcome.retried_empty,
        ms: outcome.ms,
        heuristic_extra,
        reanchored,
        outdated: false,
    }
}

fn fallback_info(status: &'static str) -> TargetLspInfo {
    TargetLspInfo {
        status,
        ops: Default::default(),
        external: 0,
        comment_filtered: 0,
        retried_empty: false,
        ms: 0,
        heuristic_extra: 0,
        reanchored: false,
        outdated: false,
    }
}

/// Status (and diagnostic) of a changed target file left unparsed by the
/// per-call re-parse budget ([`MAX_REPARSE_PER_CALL`]).
pub const STATUS_REPARSE_LIMIT: &str = "reparse_limit";

/// Why a target that a language server could answer is answered by the heuristic.
struct Fallback {
    status: &'static str,
    detail: Option<String>,
}

impl Fallback {
    fn status(status: &'static str) -> Plan<'static> {
        Plan::Heuristic(Some(Self {
            status,
            detail: None,
        }))
    }
}

fn lsp_ineligible_fallback(record: &LoadedRecord, pool: Option<&LspPool>) -> Option<&'static str> {
    if pool.is_none() {
        return Some("disabled");
    }
    if shape_of(&record.line.kind) == TargetShape::None {
        return Some("unsupported_kind");
    }
    if record.line.col.is_none() {
        return Some("no_column");
    }
    None
}

/// Why [`locate_fresh`] found no fresh record for a target.
enum Unanchored {
    /// The file is gone or cannot be re-parsed, or the declaration is gone (P6-D6).
    Stale(Fallback),
    /// The per-call re-parse budget is spent: the file was not looked at, so
    /// nothing says the declaration is gone.
    OverBudget,
    /// The file is over the re-parse size limit: it was not parsed, so
    /// nothing says the declaration is gone.
    TooLarge,
}

/// The note of a changed target file too large to re-parse; one per file
/// (notes are de-duplicated).
fn too_large_note(rel: &str) -> (&'static str, String) {
    (
        STATUS_REPARSE_LIMIT,
        format!(
            "{rel} changed since the last index and is too large to re-parse (over {} KiB); its targets were answered from the last index; run knot-lite index to refresh.",
            MAX_REPARSE_BYTES / 1024
        ),
    )
}

/// The fresh re-parse of a target's changed file and the target's record in
/// it; a `stale_target` fallback when the file cannot be re-parsed or the
/// declaration is gone (P6-D6).
fn locate_fresh<'f>(
    fresh: &'f mut FreshFiles,
    ctx: &CallersContext<'_>,
    record: &LoadedRecord,
    position: usize,
) -> Result<(&'f FreshFile, &'f ShardLine), Unanchored> {
    let stale = |detail: String| {
        Unanchored::Stale(Fallback {
            status: "stale_target",
            detail: Some(detail),
        })
    };
    let parse_cfg = ParseConfig {
        repo_root: ctx.lsp.repo_root.to_path_buf(),
        custom_queries_path: None,
        repo_name: ctx.repo.to_owned(),
        include_config_files: false,
        repo_path: ctx.lsp.repo_root.to_str().map(str::to_owned),
    };
    let file = fresh
        .get(ctx.lsp.repo_root, &record.file, &parse_cfg)
        .map_err(|err| match err {
            ReparseError::Limit => Unanchored::OverBudget,
            ReparseError::TooLarge => Unanchored::TooLarge,
            ReparseError::Failed(detail) => stale(detail.to_owned()),
        })?;
    let mapping = match_all(ctx.index, &record.file, &file.records);
    file.records
        .iter()
        .zip(mapping)
        .find(|(_, matched)| *matched == Some(position))
        .map(|(line, _)| (file, line))
        .ok_or_else(|| stale("the declaration was not found in the changed file".to_owned()))
}

fn check_live_limits(used: usize, max: usize, deadline: Instant) -> Option<&'static str> {
    if used >= max {
        Some("budget")
    } else if Instant::now() >= deadline {
        Some("deadline")
    } else {
        None
    }
}

/// Extra information about a live request.
#[derive(Debug, Clone)]
struct TargetLiveInfo<'a> {
    own: ContainerKey,
    reanchored: bool,
    precomputed: Option<&'a LspTargetLine>,
}

/// The source chosen for one target.
enum Plan<'a> {
    Indexed(&'a LspTargetLine),
    /// Pre-computed answer served as the P6-D9 fallback (outdated).
    IndexedOutdated(&'a LspTargetLine),
    Live(&'a LspPool, LiveRequest, TargetLiveInfo<'a>),
    /// `None`: no language server is relevant (not an LSP language).
    Heuristic(Option<Fallback>),
}

/// A target's answer with the source it came from.
struct Described {
    answer: Answer,
    provenance: &'static str,
    lsp: Option<TargetLspInfo>,
}

/// The diagnostics a call collected (`(status, detail)`), shown as notes.
pub type Diagnostics = Vec<(&'static str, String)>;

/// Arguments of [`Describer::ask`] (kept together to stay under the
/// `too_many_arguments` threshold).
struct AskArgs<'a> {
    position: usize,
    heuristic: Answer,
    live_info: TargetLiveInfo<'a>,
}

/// Arguments of [`Describer::reanchor`].
struct ReanchorArgs<'r, 'a> {
    record: &'r LoadedRecord,
    position: usize,
    lang: LspLang,
    hint: usize,
    pool: &'a LspPool,
    precomputed: Option<&'a LspTargetLine>,
}

/// Describes the targets of one `find_callers` call, sharing its live budget,
/// deadline and file reads.
pub struct Describer<'c, 'a> {
    ctx: &'c CallersContext<'a>,
    files: SourceFiles<'a>,
    fresh: FreshFiles,
    deadline: Instant,
    live_used: usize,
    reanchored_targets: usize,
    /// Targets in changed files left unparsed by the re-parse budget.
    over_reparse_budget: usize,
    diagnostics: Diagnostics,
}

impl<'c, 'a> Describer<'c, 'a> {
    /// A describer whose deadline starts now.
    pub fn new(ctx: &'c CallersContext<'a>) -> Self {
        Self {
            ctx,
            files: SourceFiles::new(ctx.lsp.repo_root),
            fresh: FreshFiles::new(),
            deadline: Instant::now() + ctx.lsp.settings.deadline,
            live_used: 0,
            reanchored_targets: 0,
            over_reparse_budget: 0,
            diagnostics: Vec::new(),
        }
    }

    /// Number of targets that were re-anchored in changed files.
    pub fn reanchored_count(&self) -> usize {
        self.reanchored_targets
    }

    /// Number of files successfully re-parsed.
    pub fn reparsed_count(&self) -> usize {
        self.fresh.count()
    }

    /// What the live attempts reported, to be shown as notes.
    pub fn into_diagnostics(mut self) -> Diagnostics {
        if self.over_reparse_budget > 0 {
            self.diagnostics.push((
                STATUS_REPARSE_LIMIT,
                format!(
                    "{} target(s) in changed files were not re-parsed (limit of {MAX_REPARSE_PER_CALL} files per call) and were answered from the last index; run knot-lite index to refresh.",
                    self.over_reparse_budget
                ),
            ));
        }
        self.diagnostics
    }

    /// The target at `position`, answered by the first source that applies.
    pub fn describe(&mut self, position: usize) -> CallerTarget {
        let index = self.ctx.index;
        let record = &index.records[position];
        let heuristic = heuristic_answer(index, position);
        let described = match self.plan(record, position, heuristic.containers.len()) {
            Plan::Indexed(line) => {
                let answer = indexed_answer(index, position);
                let extra = heuristic.containers.difference(&answer.containers).count();
                Described {
                    answer,
                    provenance: PROVENANCE_INDEXED,
                    lsp: Some(indexed_info(line, extra, false)),
                }
            }
            Plan::IndexedOutdated(line) => {
                let answer = indexed_answer(index, position);
                let extra = heuristic.containers.difference(&answer.containers).count();
                Described {
                    answer,
                    provenance: PROVENANCE_INDEXED,
                    lsp: Some(indexed_info(line, extra, true)),
                }
            }
            Plan::Live(pool, request, live_info) => self.ask(
                pool,
                request,
                AskArgs {
                    position,
                    heuristic,
                    live_info,
                },
            ),
            Plan::Heuristic(fallback) => {
                let lsp = fallback.map(|fallback| {
                    if let Some(detail) = fallback.detail {
                        self.diagnostics.push((fallback.status, detail));
                    }
                    fallback_info(fallback.status)
                });
                Described {
                    answer: heuristic,
                    provenance: PROVENANCE_HEURISTIC,
                    lsp,
                }
            }
        };
        target_of(record, described)
    }

    fn check_precomputed(
        &mut self,
        record: &LoadedRecord,
    ) -> (Option<&'a LspTargetLine>, Option<Outdated>) {
        let precomputed = self
            .ctx
            .index
            .lsp_targets
            .get(&record.line.id)
            .filter(|line| line.status == "ok");
        let Some(line) = precomputed else {
            return (None, None);
        };
        match indexed_is_current(self.ctx.index, self.ctx.edits, &mut self.files, record) {
            Ok(()) => (Some(line), None),
            Err(reason) => {
                self.diagnostics.push((
                    "precomputed_outdated",
                    format!(
                        "pre-computed answer of {} is outdated ({}); asked the language server",
                        record.line.name,
                        reason.reason()
                    ),
                ));
                (Some(line), Some(reason))
            }
        }
    }

    /// Choose the source for `record`, in the order of P3-D10.
    fn plan(&mut self, record: &LoadedRecord, position: usize, hint: usize) -> Plan<'a> {
        let (precomputed, outdated_reason) = self.check_precomputed(record);
        if let (Some(line), None) = (precomputed, &outdated_reason) {
            return Plan::Indexed(line);
        }
        let fallback_plan = |this: &mut Self| {
            if let Some(line) = precomputed {
                let reason_str = outdated_reason
                    .as_ref()
                    .map(|r| r.reason())
                    .unwrap_or_default();
                this.diagnostics.push((
                    "precomputed_outdated_fallback",
                    format!(
                        "pre-computed answer of {} is outdated ({}); callers added there are missing (run knot-lite index)",
                        record.line.name,
                        reason_str
                    ),
                ));
                Plan::IndexedOutdated(line)
            } else {
                Plan::Heuristic(None)
            }
        };
        let Some(lang) = lsp_lang_for_label(label_for(Path::new(&record.file))) else {
            return self
                .stale_if_deleted(record, position)
                .unwrap_or_else(|| fallback_plan(self));
        };
        if let Some(reason) = lsp_ineligible_fallback(record, self.ctx.lsp.pool) {
            if let Some(stale) = self.stale_if_deleted(record, position) {
                return stale;
            }
            return if precomputed.is_some() {
                fallback_plan(self)
            } else {
                Fallback::status(reason)
            };
        }
        let pool = self.ctx.lsp.pool.expect("pool confirmed");
        let request = match self.request(record, lang, hint) {
            Ok(request) => request,
            Err(Unlocated::Stale) => {
                let reanchor_plan = self.reanchor(ReanchorArgs {
                    record,
                    position,
                    lang,
                    hint,
                    pool,
                    precomputed,
                });
                if matches!(
                    &reanchor_plan,
                    Plan::Heuristic(Some(fb)) if fb.status != "stale_target"
                ) && precomputed.is_some()
                {
                    return fallback_plan(self);
                }
                return reanchor_plan;
            }
            Err(why) => {
                // The file may be gone or unreadable (deleted, FIFO, escaping
                // symlink): that is a stale_target, as when no server is on.
                if let Some(stale) = self.stale_if_deleted(record, position) {
                    return stale;
                }
                return if precomputed.is_some() {
                    fallback_plan(self)
                } else {
                    Plan::Heuristic(Some(Fallback {
                        status: "no_position",
                        detail: Some(why.detail()),
                    }))
                };
            }
        };
        if let Some(reason) = check_live_limits(
            self.live_used,
            self.ctx.lsp.settings.max_live_targets,
            self.deadline,
        ) {
            return if precomputed.is_some() {
                fallback_plan(self)
            } else {
                Fallback::status(reason)
            };
        }
        self.live_used += 1;
        let own = own_container(self.ctx.index, position);
        Plan::Live(
            pool,
            request,
            TargetLiveInfo {
                own,
                reanchored: false,
                precomputed,
            },
        )
    }

    /// When `record`'s own file changed since the index and its declaration
    /// cannot be found there any more, it is a `stale_target` whether or not a
    /// server is on or a pre-computed answer exists. A changed file past the
    /// re-parse budget or size limit returns `None` (with a note).
    fn stale_if_deleted(&mut self, record: &LoadedRecord, position: usize) -> Option<Plan<'a>> {
        if self.file_changed(record) {
            match locate_fresh(&mut self.fresh, self.ctx, record, position) {
                Ok(_) => {}
                Err(Unanchored::Stale(stale)) => return Some(Plan::Heuristic(Some(stale))),
                Err(Unanchored::OverBudget) => self.over_reparse_budget += 1,
                Err(Unanchored::TooLarge) => self.diagnostics.push(too_large_note(&record.file)),
            }
        }
        None
    }

    /// Whether `record`'s own file changed (or was removed) since the index.
    ///
    /// [`EditedFiles`] only covers files whose language has a language server,
    /// and hashes this one whatever its size and mtime; when `edits` is
    /// enabled, every other indexed file is checked against `file_sha16` on
    /// demand.
    fn file_changed(&mut self, record: &LoadedRecord) -> bool {
        let Some(edits) = self.ctx.edits else {
            return false;
        };
        if lsp_lang_for_label(label_for(Path::new(&record.file))).is_some() {
            return edits.is_target_changed(&record.file);
        }
        let Some(indexed) = self.ctx.index.file_sha16.get(&record.file) else {
            return false;
        };
        !self
            .files
            .get(&record.file)
            .is_ok_and(|source| source.sha16() == indexed.as_str())
    }

    /// Re-anchor a stale target in its changed file (P6-D6).
    fn reanchor(&mut self, args: ReanchorArgs<'_, 'a>) -> Plan<'a> {
        let ReanchorArgs {
            record,
            position,
            lang,
            hint,
            pool,
            precomputed,
        } = args;
        // The file no longer has the indexed content (that is why the target
        // is re-anchored): hash it into the call's changed set even when the
        // coarse filters skip it (same size, mtime set back), so the request
        // lists it and its references are mapped through the fresh parse.
        self.file_changed(record);
        let (fresh, fresh_line) = match locate_fresh(&mut self.fresh, self.ctx, record, position) {
            Ok(found) => found,
            Err(Unanchored::Stale(stale)) => return Plan::Heuristic(Some(stale)),
            Err(Unanchored::OverBudget) => {
                self.over_reparse_budget += 1;
                return Fallback::status(STATUS_REPARSE_LIMIT);
            }
            Err(Unanchored::TooLarge) => {
                self.diagnostics.push(too_large_note(&record.file));
                return Fallback::status(STATUS_REPARSE_LIMIT);
            }
        };

        let fresh_target = Target {
            rel: &record.file,
            line: fresh_line,
            lang,
            indexed_sha16: None,
        };
        let pacing = Pacing {
            heuristic_hint: hint,
            deadline: self.deadline,
        };
        match live_request(&mut self.files, &fresh_target, pacing) {
            Ok(mut request) => {
                if let Some(reason) = check_live_limits(
                    self.live_used,
                    self.ctx.lsp.settings.max_live_targets,
                    self.deadline,
                ) {
                    return Fallback::status(reason);
                }
                request.changed_files = changed_files(self.ctx, lang);
                self.live_used += 1;
                self.reanchored_targets += 1;
                let own = fresh.containers.container_at(&record.file, fresh_line.l0).0;
                Plan::Live(
                    pool,
                    request,
                    TargetLiveInfo {
                        own,
                        reanchored: true,
                        precomputed,
                    },
                )
            }
            Err(why) => self.stale_if_deleted(record, position).unwrap_or_else(|| {
                Plan::Heuristic(Some(Fallback {
                    status: "no_position",
                    detail: Some(why.detail()),
                }))
            }),
        }
    }

    fn request(
        &mut self,
        record: &LoadedRecord,
        lang: LspLang,
        hint: usize,
    ) -> Result<LiveRequest, Unlocated> {
        let target = Target {
            rel: &record.file,
            line: &record.line,
            lang,
            indexed_sha16: self
                .ctx
                .index
                .file_sha16
                .get(&record.file)
                .map(String::as_str),
        };
        let pacing = Pacing {
            heuristic_hint: hint,
            deadline: self.deadline,
        };
        let mut req = live_request(&mut self.files, &target, pacing)?;
        req.changed_files = changed_files(self.ctx, lang);
        Ok(req)
    }

    /// Ask the language server; an answer that is not `ok` falls back to the heuristic one.
    fn ask(&mut self, pool: &LspPool, request: LiveRequest, args: AskArgs<'a>) -> Described {
        let AskArgs {
            position,
            heuristic,
            live_info: target_info,
        } = args;
        let lang = request.lang;
        let outcome = pool.query(request);
        if outcome.sync_synced > 0 {
            let note = if outcome.sync_left > 0 {
                format!(
                    "synced {} changed file(s) with {}; {} left for the next request",
                    outcome.sync_synced,
                    lang.server_program(),
                    outcome.sync_left
                )
            } else {
                format!(
                    "synced {} changed file(s) with {}",
                    outcome.sync_synced,
                    lang.server_program()
                )
            };
            self.diagnostics.push(("sync", note));
        }
        if outcome.status != LiveStatus::Ok {
            if let Some(detail) = &outcome.detail {
                self.diagnostics
                    .push((outcome.status.as_str(), detail.clone()));
            }
            if let Some(line) = target_info.precomputed {
                let answer = indexed_answer(self.ctx.index, position);
                let extra = heuristic.containers.difference(&answer.containers).count();
                self.diagnostics.push((
                    "precomputed_outdated_fallback",
                    format!(
                        "pre-computed answer of {} is outdated; callers added there are missing (run knot-lite index)",
                        self.ctx.index.records[position].line.name
                    ),
                ));
                return Described {
                    answer,
                    provenance: PROVENANCE_INDEXED,
                    lsp: Some(indexed_info(line, extra, true)),
                };
            }
            return Described {
                answer: heuristic,
                provenance: PROVENANCE_HEURISTIC,
                lsp: Some(live_info(&outcome, 0, target_info.reanchored)),
            };
        }
        let (lookup, note) = build_lookup(&mut self.fresh, self.ctx, &outcome.locations);
        if let Some(note) = note {
            self.diagnostics.push((STATUS_REPARSE_LIMIT, note));
        }
        let answer = live_answer(self.ctx.index, target_info.own, position, &outcome, &lookup);
        let extra = heuristic.containers.difference(&answer.containers).count();
        Described {
            answer,
            provenance: PROVENANCE_LIVE,
            lsp: Some(live_info(&outcome, extra, target_info.reanchored)),
        }
    }
}

/// The changed files of `lang` a live request carries for edit sync.
fn changed_files(ctx: &CallersContext<'_>, lang: LspLang) -> std::sync::Arc<Vec<String>> {
    std::sync::Arc::new(ctx.edits.map(|e| e.changed(lang)).unwrap_or_default())
}

fn build_lookup<'s, 'c, 'a>(
    fresh: &'s mut FreshFiles,
    ctx: &'c CallersContext<'a>,
    locations: &[RawLocation],
) -> (ContainerLookup<'s>, Option<String>)
where
    'a: 's,
{
    let index = ctx.index;
    let Some(edits) = ctx.edits else {
        return (ContainerLookup::index_only(&index.containers), None);
    };
    if !edits.has_changes() {
        return (ContainerLookup::index_only(&index.containers), None);
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for loc in locations {
        if edits.is_changed(&loc.rel) {
            *counts.entry(loc.rel.as_str()).or_default() += 1;
        }
    }
    if counts.is_empty() {
        return (ContainerLookup::index_only(&index.containers), None);
    }

    let mut sorted_files: Vec<(&str, usize)> = counts.into_iter().collect();
    sorted_files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let parse_cfg = ParseConfig {
        repo_root: ctx.lsp.repo_root.to_path_buf(),
        custom_queries_path: None,
        repo_name: ctx.repo.to_owned(),
        include_config_files: false,
        repo_path: ctx.lsp.repo_root.to_str().map(str::to_owned),
    };

    let mut approximate_files = 0usize;
    for (rel, _) in sorted_files {
        if fresh.count() < MAX_REPARSE_PER_CALL {
            let _ = fresh.get(ctx.lsp.repo_root, rel, &parse_cfg);
        } else if !fresh.contains(rel) {
            approximate_files += 1;
        }
    }

    let note = (approximate_files > 0).then(|| {
        format!(
            "{approximate_files} changed file(s) attributed through the last index (re-parse limit)"
        )
    });

    let mut overlays = HashMap::new();
    for (rel, fresh_file) in fresh.files() {
        let mapping = match_all(index, rel, &fresh_file.records);
        overlays.insert(
            rel,
            FreshOverlay {
                table: &fresh_file.containers,
                records: &fresh_file.records,
                mapping,
            },
        );
    }

    (ContainerLookup::new(&index.containers, overlays), note)
}

fn target_of(record: &LoadedRecord, described: Described) -> CallerTarget {
    let Described {
        answer,
        provenance,
        lsp,
    } = described;
    CallerTarget {
        id: record.line.id.clone(),
        kind: record.line.kind.clone(),
        name: record.line.name.clone(),
        fqn: record.line.fqn.clone(),
        file: record.file.clone(),
        l0: record.line.l0,
        l1: record.line.l1,
        signature: record.line.sig.clone(),
        provenance,
        lsp,
        caller_count: answer.caller_count,
        self_references: answer.self_references,
        callers: answer.groups,
    }
}
