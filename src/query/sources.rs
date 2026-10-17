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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use knot::pipeline::parser::ParseConfig;

use super::callers::{CallerRow, CallerTarget, CallersContext, RelationGroups, TargetLspInfo};
use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::graph::containers::{ContainerKey, FILE_LEVEL_NAME};
use crate::graph::heuristic::EDGE_REL_LABELS;
use crate::index::load::{LoadedIndex, LoadedRecord};
use crate::index::lsp_records::LspTargetLine;
use crate::index::reparse::{FreshFiles, match_all};
use crate::lsp::normalize::{LspGroups, TargetInfo, group_locations};
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
                    .map(|(key, (container, sites))| caller_row(index, key, container, sites))
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
    index: &LoadedIndex,
    outcome: &LiveOutcome,
    shape: TargetShape,
    own: ContainerKey,
) -> usize {
    group_locations(
        &outcome.locations,
        &TargetInfo { shape, own },
        &index.containers,
    )
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
) -> Answer {
    let target = TargetInfo {
        shape: shape_of(&index.records[position].line.kind),
        own,
    };
    let LspGroups {
        groups,
        self_references,
        containers,
    } = group_locations(&outcome.locations, &target, &index.containers);
    let groups = groups
        .into_iter()
        .map(|(relation, hits)| {
            let rows = hits
                .into_iter()
                .map(|hit| caller_row(index, hit.key, hit.record, hit.sites))
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

/// One caller row: the container's record, or the file level.
pub fn caller_row(
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

fn indexed_info(line: &LspTargetLine, heuristic_extra: usize) -> TargetLspInfo {
    TargetLspInfo {
        status: "indexed",
        ops: line.ops,
        external: 0,
        comment_filtered: 0,
        retried_empty: false,
        ms: line.ms,
        heuristic_extra,
        reanchored: false,
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
    }
}

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

/// Extra information about a live request.
#[derive(Debug, Clone)]
struct TargetLiveInfo {
    own: ContainerKey,
    reanchored: bool,
}

/// The source chosen for one target.
enum Plan<'a> {
    Indexed(&'a LspTargetLine),
    Live(&'a LspPool, LiveRequest, TargetLiveInfo),
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
struct AskArgs {
    position: usize,
    heuristic: Answer,
    live_info: TargetLiveInfo,
}

/// Arguments of [`Describer::reanchor`].
struct ReanchorArgs<'r, 'a> {
    record: &'r LoadedRecord,
    position: usize,
    lang: LspLang,
    hint: usize,
    pool: &'a LspPool,
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
    pub fn into_diagnostics(self) -> Diagnostics {
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
                    lsp: Some(indexed_info(line, extra)),
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

    /// Choose the source for `record`, in the order of P3-D10.
    fn plan(&mut self, record: &LoadedRecord, position: usize, hint: usize) -> Plan<'a> {
        let index = self.ctx.index;
        if let Some(line) = index
            .lsp_targets
            .get(&record.line.id)
            .filter(|line| line.status == "ok")
        {
            return Plan::Indexed(line);
        }
        let Some(lang) = lsp_lang_for_label(label_for(Path::new(&record.file))) else {
            return Plan::Heuristic(None);
        };
        let Some(pool) = self.ctx.lsp.pool else {
            return Fallback::status("disabled");
        };
        if shape_of(&record.line.kind) == TargetShape::None {
            return Fallback::status("unsupported_kind");
        }
        if record.line.col.is_none() {
            return Fallback::status("no_column");
        }
        let request = match self.request(record, lang, hint) {
            Ok(request) => request,
            Err(Unlocated::Stale) => {
                // P6-D6: re-parse the changed file and re-anchor the target.
                return self.reanchor(ReanchorArgs {
                    record,
                    position,
                    lang,
                    hint,
                    pool,
                });
            }
            Err(why) => {
                return Plan::Heuristic(Some(Fallback {
                    status: "no_position",
                    detail: Some(why.detail()),
                }));
            }
        };
        if self.live_used >= self.ctx.lsp.settings.max_live_targets {
            return Fallback::status("budget");
        }
        if Instant::now() >= self.deadline {
            return Fallback::status("deadline");
        }
        self.live_used += 1;
        let own = own_container(self.ctx.index, position);
        Plan::Live(
            pool,
            request,
            TargetLiveInfo {
                own,
                reanchored: false,
            },
        )
    }

    /// Re-anchor a stale target in its changed file (P6-D6).
    fn reanchor(&mut self, args: ReanchorArgs<'_, 'a>) -> Plan<'a> {
        let ReanchorArgs {
            record,
            position,
            lang,
            hint,
            pool,
        } = args;
        let parse_cfg = ParseConfig {
            repo_root: self.ctx.lsp.repo_root.to_path_buf(),
            custom_queries_path: None,
            repo_name: self.ctx.repo.to_owned(),
            include_config_files: false,
            repo_path: self.ctx.lsp.repo_root.to_str().map(str::to_owned),
        };
        let fresh = match self
            .fresh
            .get(self.ctx.lsp.repo_root, &record.file, &parse_cfg)
        {
            Ok(fresh) => fresh,
            Err(err) => {
                return Plan::Heuristic(Some(Fallback {
                    status: "stale_target",
                    detail: Some(err.to_owned()),
                }));
            }
        };

        let mapping = match_all(self.ctx.index, &record.file, &fresh.records);
        let matched = fresh
            .records
            .iter()
            .enumerate()
            .find(|(j, _)| mapping[*j] == Some(position));

        let Some((_, fresh_line)) = matched else {
            return Plan::Heuristic(Some(Fallback {
                status: "stale_target",
                detail: Some("the declaration was not found in the changed file".to_owned()),
            }));
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
            Ok(request) => {
                if self.live_used >= self.ctx.lsp.settings.max_live_targets {
                    return Fallback::status("budget");
                }
                if Instant::now() >= self.deadline {
                    return Fallback::status("deadline");
                }
                self.live_used += 1;
                self.reanchored_targets += 1;
                let own = fresh.containers.container_at(&record.file, fresh_line.l0).0;
                Plan::Live(
                    pool,
                    request,
                    TargetLiveInfo {
                        own,
                        reanchored: true,
                    },
                )
            }
            Err(why) => Plan::Heuristic(Some(Fallback {
                status: "no_position",
                detail: Some(why.detail()),
            })),
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
        live_request(&mut self.files, &target, pacing)
    }

    /// Ask the language server; an answer that is not `ok` falls back to the heuristic one.
    fn ask(&mut self, pool: &LspPool, request: LiveRequest, args: AskArgs) -> Described {
        let AskArgs {
            position,
            heuristic,
            live_info: target_info,
        } = args;
        let outcome = pool.query(request);
        if outcome.status != LiveStatus::Ok {
            if let Some(detail) = &outcome.detail {
                self.diagnostics
                    .push((outcome.status.as_str(), detail.clone()));
            }
            return Described {
                answer: heuristic,
                provenance: PROVENANCE_HEURISTIC,
                lsp: Some(live_info(&outcome, 0, target_info.reanchored)),
            };
        }
        let answer = live_answer(self.ctx.index, target_info.own, position, &outcome);
        let extra = heuristic.containers.difference(&answer.containers).count();
        Described {
            answer,
            provenance: PROVENANCE_LIVE,
            lsp: Some(live_info(&outcome, extra, target_info.reanchored)),
        }
    }
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
