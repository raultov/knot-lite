//! Pre-computed LSP call graph at index time (`knot-lite index --lsp-precompute`, P3-D9).
//!
//! For high fan-in targets (heuristic caller containers ≥ `min_fanin`), pre-computes
//! LSP answers and writes them to `.knot-lite/lsp-edges/<lang>/<caller rel>.jsonl` and
//! `.knot-lite/lsp-targets/<lang>.jsonl`.
//!
//! Supports reuse (P3-D9): if the language file fingerprint and parameters match the
//! previous manifest, data is kept untouched without starting a server.
//!
//! Targets are chosen in two stages by default (P5-D17, `--lsp-selection lsp-refs`): a
//! cheap `references` probe over a wider candidate pool, within 40 % of the language's
//! budget, ranks the candidates by the server's own caller count; the full answer of a
//! selected target reuses the probe's locations. `--lsp-selection heuristic` keeps the
//! Phase 3 rule (heuristic fan-in only).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::normalize::{TargetInfo, group_locations};
use super::ops::{OpSet, TargetShape, ops_for, shape_of};
use super::pool::LspPool;
use super::request::{Pacing, SourceFiles, Target, live_request};
use super::target::{LiveOutcome, LiveStatus};
use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::graph::containers::ContainerTable;
use crate::index::hash::{sha256_hex, short16};
use crate::index::inventory::{Inventory, InventoryEntry};
use crate::index::layout::{IndexLayout, rel_of_record};
use crate::index::lsp_records::{
    LspEdgeLine, LspTargetLine, write_lsp_edge_shards, write_lsp_targets,
};
use crate::index::manifest::{LSP_FORMAT_V2, LspLangManifest, LspManifest};
use crate::index::shard::ShardLine;

/// Per-target live query cap during pre-computation.
const PER_TARGET_CAP: Duration = Duration::from_secs(120);

/// Progress is reported every this many targets.
const PROGRESS_EVERY: usize = 10;

/// The probe pool holds this many candidates per selectable target (P5-D17).
const PROBE_POOL_FACTOR: usize = 4;

/// Share of a language's budget the probe may use (P5-D17).
const PROBE_BUDGET_SHARE: f64 = 0.4;

/// How pre-computation targets are chosen (P5-D17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Selection {
    /// Probe the server's reference counts, then pick the most referenced.
    #[default]
    LspRefs,
    /// Heuristic fan-in only (the Phase 3 rule).
    Heuristic,
}

impl Selection {
    /// Wire string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LspRefs => "lsp-refs",
            Self::Heuristic => "heuristic",
        }
    }
}

/// Pre-compute policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PrecomputePolicy {
    /// Java only (the language whose live latency fails G8).
    #[default]
    Auto,
    /// Every language present in the repository.
    All,
    /// Disabled (pre-computed data is removed).
    None,
}

impl PrecomputePolicy {
    /// Wire string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::All => "all",
            Self::None => "none",
        }
    }
}

/// Options for pre-computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecomputeOptions {
    /// Policy switch.
    pub policy: PrecomputePolicy,
    /// Minimum heuristic fan-in threshold.
    pub min_fanin: usize,
    /// Maximum targets per language.
    pub max_targets: usize,
    /// Time budget per language.
    pub budget: Duration,
    /// How targets are chosen.
    pub selection: Selection,
    /// Heuristic fan-in a non-type candidate needs to be probed (`lsp-refs`).
    pub probe_min_fanin: usize,
}

impl Default for PrecomputeOptions {
    fn default() -> Self {
        Self {
            policy: PrecomputePolicy::Auto,
            min_fanin: 20,
            max_targets: 200,
            budget: Duration::from_secs(600),
            selection: Selection::LspRefs,
            probe_min_fanin: 3,
        }
    }
}

/// Inputs provided to [`run`].
pub struct PrecomputeInput<'a> {
    /// All entity records (file, ShardLine).
    pub records: &'a [(String, ShardLine)],
    /// Heuristic caller container count per target id.
    pub heuristic_fanin: &'a HashMap<String, usize>,
    /// Inventory of repository files.
    pub inventory: &'a Inventory,
    /// Previous manifest, if any.
    pub previous: Option<&'a LspManifest>,
    /// Repository root path.
    pub repo_root: &'a Path,
}

/// Progress event emitted during pre-computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// LSP stage started.
    Stage,
    /// Progress on one language's targets.
    LspTargets {
        lang: LspLang,
        done: usize,
        total: usize,
    },
}

/// Which languages are selected by `policy` given `present` languages.
pub fn languages_for(policy: PrecomputePolicy, present: &BTreeSet<LspLang>) -> Vec<LspLang> {
    match policy {
        PrecomputePolicy::Auto => {
            if present.contains(&LspLang::Java) {
                vec![LspLang::Java]
            } else {
                Vec::new()
            }
        }
        PrecomputePolicy::All => [LspLang::Rust, LspLang::Ts, LspLang::Java]
            .into_iter()
            .filter(|lang| present.contains(lang))
            .collect(),
        PrecomputePolicy::None => Vec::new(),
    }
}

/// Compute inventory fingerprint for `lang`.
pub fn fingerprint_for_lang(inventory: &Inventory, lang: LspLang) -> String {
    let mut entries: Vec<&InventoryEntry> = inventory
        .entries
        .iter()
        .filter(|e| lsp_lang_for_label(label_for(Path::new(&e.rel))) == Some(lang))
        .collect();
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));

    let mut buf = String::new();
    for entry in entries {
        buf.push_str(&entry.rel);
        buf.push(':');
        buf.push_str(&entry.sha);
        buf.push('\n');
    }
    short16(sha256_hex(buf.as_bytes()))
}

/// Run the pre-computation stage for all languages.
pub fn run(
    input: &PrecomputeInput<'_>,
    opts: &PrecomputeOptions,
    pool: Option<&LspPool>,
    layout: &IndexLayout,
    progress: &mut dyn FnMut(ProgressEvent),
) -> anyhow::Result<LspManifest> {
    progress(ProgressEvent::Stage);

    let mut present_languages = BTreeSet::new();
    for entry in &input.inventory.entries {
        if let Some(lang) = lsp_lang_for_label(label_for(Path::new(&entry.rel))) {
            present_languages.insert(lang);
        }
    }

    let selected_languages = languages_for(opts.policy, &present_languages);
    let mut lang_manifests = BTreeMap::new();
    let container_table = ContainerTable::from_records(
        input
            .records
            .iter()
            .enumerate()
            .map(|(idx, (rel, line))| (idx, rel.as_str(), line)),
    );

    for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
        let lang_dir_name = lang.dir_name().to_owned();
        let fp = fingerprint_for_lang(input.inventory, lang);

        if !selected_languages.contains(&lang) {
            clean_lang_data(layout, lang)?;
            if present_languages.contains(&lang) {
                lang_manifests.insert(lang_dir_name, idle_manifest(lang, "none", fp, ""));
            }
            continue;
        }

        // Check reuse (P3-D9)
        if let Some(prev) = input.previous
            && prev.policy == opts.policy.as_str()
            && prev.min_fanin == opts.min_fanin
            && prev.max_targets == opts.max_targets
            && prev.selection == opts.selection.as_str()
            && prev.probe_min_fanin == opts.probe_min_fanin
            && let Some(prev_lang) = prev.languages.get(&lang_dir_name)
            && matches!(prev_lang.status.as_str(), "computed" | "reused")
            && prev_lang.files_fingerprint == fp
        {
            let mut reused = prev_lang.clone();
            reused.status = "reused".to_owned();
            lang_manifests.insert(lang_dir_name, reused);
            continue;
        }

        // Check if pool is available
        let Some(pool) = pool else {
            clean_lang_data(layout, lang)?;
            lang_manifests.insert(
                lang_dir_name,
                idle_manifest(lang, "skipped", fp, "LSP is disabled (KNOT_LITE_LSP=off)"),
            );
            continue;
        };

        let manifest_lang = precompute_lang(
            &PrecomputeLangCtx {
                input,
                opts,
                pool,
                layout,
                lang,
                fp: &fp,
                container_table: &container_table,
            },
            progress,
        )?;
        lang_manifests.insert(lang_dir_name, manifest_lang);
    }

    Ok(LspManifest {
        format: LSP_FORMAT_V2,
        policy: opts.policy.as_str().to_owned(),
        selection: opts.selection.as_str().to_owned(),
        probe_min_fanin: opts.probe_min_fanin,
        min_fanin: opts.min_fanin,
        max_targets: opts.max_targets,
        budget_secs: opts.budget.as_secs(),
        languages: lang_manifests,
    })
}

/// The manifest of a language nothing was asked about (`none`, `skipped`).
fn idle_manifest(lang: LspLang, status: &str, fp: String, diagnostic: &str) -> LspLangManifest {
    LspLangManifest {
        status: status.to_owned(),
        server: lang.server_program().to_owned(),
        version: String::new(),
        files_fingerprint: fp,
        targets_selected: 0,
        targets_ok: 0,
        targets_failed: 0,
        edges: 0,
        ms: 0,
        diagnostic: diagnostic.to_owned(),
        probed: 0,
        probe_ms: 0,
    }
}

fn clean_lang_data(layout: &IndexLayout, lang: LspLang) -> anyhow::Result<()> {
    let lang_edges_dir = layout.lsp_edges_lang_dir(lang);
    if lang_edges_dir.exists() {
        std::fs::remove_dir_all(&lang_edges_dir)
            .with_context(|| format!("removing {}", lang_edges_dir.display()))?;
    }
    let targets_file = layout.lsp_targets_for(lang);
    if targets_file.exists() {
        std::fs::remove_file(&targets_file)
            .with_context(|| format!("removing {}", targets_file.display()))?;
    }
    // The parents go with the last language (`remove_dir` refuses a non-empty one).
    for parent in [layout.lsp_edges(), layout.lsp_targets()] {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

struct PrecomputeLangCtx<'a> {
    input: &'a PrecomputeInput<'a>,
    opts: &'a PrecomputeOptions,
    pool: &'a LspPool,
    layout: &'a IndexLayout,
    lang: LspLang,
    fp: &'a str,
    container_table: &'a ContainerTable,
}

/// A target selected for pre-computation.
#[derive(Clone, Copy)]
struct Candidate<'a> {
    rel: &'a str,
    line: &'a ShardLine,
    fanin: usize,
    /// Caller containers the probe found, when it was probed.
    refs: Option<usize>,
}

/// Records of `lang` with a located column and at least `min_fanin` caller
/// containers: highest fan-in first, then by id, at most `max_targets`.
fn select_candidates<'a>(
    input: &'a PrecomputeInput<'_>,
    lang: LspLang,
    opts: &PrecomputeOptions,
) -> Vec<Candidate<'a>> {
    let mut candidates: Vec<Candidate<'a>> = located_records(input, lang)
        .filter(|candidate| candidate.fanin >= opts.min_fanin)
        .collect();
    sort_by_fanin(&mut candidates);
    candidates.truncate(opts.max_targets);
    candidates
}

/// The records of `lang` with a located column, as candidates.
fn located_records<'a>(
    input: &'a PrecomputeInput<'_>,
    lang: LspLang,
) -> impl Iterator<Item = Candidate<'a>> {
    input
        .records
        .iter()
        .filter(move |(rel, line)| {
            line.col.is_some() && lsp_lang_for_label(label_for(Path::new(rel))) == Some(lang)
        })
        .map(|(rel, line)| Candidate {
            rel,
            line,
            fanin: input.heuristic_fanin.get(&line.id).copied().unwrap_or(0),
            refs: None,
        })
}

fn sort_by_fanin(candidates: &mut [Candidate<'_>]) {
    candidates.sort_by(|a, b| {
        b.fanin
            .cmp(&a.fanin)
            .then_with(|| a.line.id.cmp(&b.line.id))
    });
}

/// The probe pool (P5-D17): located records of a shape a server knows, with
/// at least `probe_min_fanin` heuristic callers — or any fan-in for interfaces,
/// classes and types, whose references the heuristic under-counts. Highest
/// fan-in first, then by id, at most `PROBE_POOL_FACTOR × max_targets`.
fn probe_pool<'a>(
    input: &'a PrecomputeInput<'_>,
    lang: LspLang,
    opts: &PrecomputeOptions,
) -> Vec<Candidate<'a>> {
    let mut pool: Vec<Candidate<'a>> = located_records(input, lang)
        .filter(|candidate| {
            let shape = shape_of(&candidate.line.kind);
            let typeish = matches!(
                shape,
                TargetShape::InterfaceLike | TargetShape::ClassLike | TargetShape::TypeLike
            );
            shape != TargetShape::None && (typeish || candidate.fanin >= opts.probe_min_fanin)
        })
        .collect();
    sort_by_fanin(&mut pool);
    pool.truncate(PROBE_POOL_FACTOR.saturating_mul(opts.max_targets));
    pool
}

/// The selection after the probe: probed candidates with at least `min_fanin`
/// caller containers by that count (then fan-in, id), then the candidates the
/// probe did not reach that pass the Phase 3 rule; at most `max_targets`.
fn select_by_refs<'a>(
    input: &'a PrecomputeInput<'_>,
    lang: LspLang,
    opts: &PrecomputeOptions,
    pool: &[Candidate<'a>],
    probes: &HashMap<String, Probe>,
) -> Vec<Candidate<'a>> {
    let mut selected: Vec<Candidate<'a>> = pool
        .iter()
        .filter_map(|candidate| {
            let probe = probes.get(&candidate.line.id)?;
            (probe.count >= opts.min_fanin).then_some(Candidate {
                refs: Some(probe.count),
                ..*candidate
            })
        })
        .collect();
    selected.sort_by(|a, b| {
        b.refs
            .cmp(&a.refs)
            .then_with(|| b.fanin.cmp(&a.fanin))
            .then_with(|| a.line.id.cmp(&b.line.id))
    });
    let unprobed = select_candidates(
        input,
        lang,
        &PrecomputeOptions {
            max_targets: usize::MAX,
            ..opts.clone()
        },
    )
    .into_iter()
    .filter(|candidate| !probes.contains_key(&candidate.line.id));
    selected.extend(unprobed);
    selected.truncate(opts.max_targets);
    selected
}

/// Ask the server about one candidate (with `ops`, or the operations of its
/// kind); a target that cannot be positioned is answered without a request.
fn ask_target(
    ctx: &PrecomputeLangCtx<'_>,
    files: &mut SourceFiles<'_>,
    candidate: &Candidate<'_>,
    deadline: Instant,
    ops: Option<OpSet>,
) -> LiveOutcome {
    let target = Target {
        rel: candidate.rel,
        line: candidate.line,
        lang: ctx.lang,
        indexed_sha16: None,
    };
    let pacing = Pacing {
        heuristic_hint: candidate.fanin,
        deadline,
    };
    match live_request(files, &target, pacing) {
        Ok(mut request) => {
            if let Some(ops) = ops {
                request.ops = ops;
            }
            ctx.pool.query(request)
        }
        Err(why) => LiveOutcome::without_answer(LiveStatus::NoPosition, why.detail()),
    }
}

/// What a candidate's target is for the grouping of its answer.
fn target_info(ctx: &PrecomputeLangCtx<'_>, candidate: &Candidate<'_>) -> TargetInfo {
    let line = candidate.line;
    TargetInfo {
        shape: shape_of(&line.kind),
        own: ctx.container_table.container_at(candidate.rel, line.l0).0,
    }
}

/// Turn the locations of one answered target into edges, grouped by caller
/// file: the same grouping a live answer gets (§6.5), one edge per
/// (container, relationship).
fn collect_edges(
    ctx: &PrecomputeLangCtx<'_>,
    candidate: &Candidate<'_>,
    outcome: &LiveOutcome,
    shards: &mut BTreeMap<String, Vec<LspEdgeLine>>,
) {
    let groups = group_locations(
        &outcome.locations,
        &target_info(ctx, candidate),
        ctx.container_table,
    );
    for (relation, hits) in groups.groups {
        for hit in hits {
            shards.entry(hit.key.file).or_default().push(LspEdgeLine {
                src: hit
                    .record
                    .map(|position| ctx.input.records[position].1.id.clone()),
                dst: candidate.line.id.clone(),
                rel: relation.clone(),
                n: u32::try_from(hit.sites).unwrap_or(u32::MAX),
            });
        }
    }
}

/// One candidate's `references` answer from the probe.
struct Probe {
    outcome: LiveOutcome,
    count: usize,
}

/// What the probe of one language gathered.
#[derive(Default)]
struct ProbeRun {
    probes: HashMap<String, Probe>,
    ms: u64,
    no_server: bool,
}

/// Probe `pool` with `references` only until `probe_end`.
fn run_probe(
    ctx: &PrecomputeLangCtx<'_>,
    files: &mut SourceFiles<'_>,
    pool: &[Candidate<'_>],
    probe_end: Instant,
) -> ProbeRun {
    let started = Instant::now();
    let mut run = ProbeRun::default();
    for candidate in pool {
        let now = Instant::now();
        if now >= probe_end {
            break;
        }
        let deadline = (now + PER_TARGET_CAP).min(probe_end);
        let outcome = ask_target(
            ctx,
            files,
            candidate,
            deadline,
            Some(OpSet::references_only()),
        );
        match outcome.status {
            LiveStatus::Ok => {
                let count = group_locations(
                    &outcome.locations,
                    &target_info(ctx, candidate),
                    ctx.container_table,
                )
                .containers
                .len();
                run.probes
                    .insert(candidate.line.id.clone(), Probe { outcome, count });
            }
            LiveStatus::NoServer => {
                run.no_server = true;
                break;
            }
            _ => {}
        }
    }
    run.ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    run
}

/// The full answer of a probed candidate: the operations of its kind except
/// `references`, merged with the probe's locations; no request at all when
/// `references` was the only one (types, values).
fn complete_with_probe(
    ctx: &PrecomputeLangCtx<'_>,
    files: &mut SourceFiles<'_>,
    candidate: &Candidate<'_>,
    deadline: Instant,
    probe: &Probe,
) -> LiveOutcome {
    let mut ops = ops_for(shape_of(&candidate.line.kind));
    ops.references = false;
    if ops.is_empty() {
        return probe.outcome.clone();
    }
    let mut outcome = ask_target(ctx, files, candidate, deadline, Some(ops));
    if outcome.status == LiveStatus::Ok {
        outcome
            .locations
            .extend(probe.outcome.locations.iter().cloned());
        outcome.op_counts.references = probe.outcome.op_counts.references;
        outcome.ms = outcome.ms.saturating_add(probe.outcome.ms);
    }
    outcome
}

/// What asking about the candidates of one language gathered.
#[derive(Default)]
struct LangRun {
    attempted: usize,
    ok: usize,
    failed: usize,
    diagnostic: String,
    server_version: String,
    shards: BTreeMap<String, Vec<LspEdgeLine>>,
    targets: Vec<LspTargetLine>,
}

impl LangRun {
    fn record(&mut self, candidate: &Candidate<'_>, outcome: &LiveOutcome, selection: Selection) {
        if outcome.status == LiveStatus::Ok {
            self.ok += 1;
        } else {
            self.failed += 1;
            if let Some(detail) = &outcome.detail {
                self.diagnostic.clone_from(detail);
            }
        }
        self.targets.push(LspTargetLine {
            id: candidate.line.id.clone(),
            status: outcome.status.as_str().to_owned(),
            ms: outcome.ms,
            ops: outcome.op_counts,
            server: self.server_version.clone(),
            refs: candidate.refs,
            h: (selection == Selection::LspRefs).then_some(candidate.fanin),
        });
    }

    /// `computed`, `failed` (nothing answered) or `none` (nothing asked).
    fn status(&self) -> &'static str {
        match (self.attempted, self.ok) {
            (0, _) => "none",
            (_, 0) => "failed",
            _ => "computed",
        }
    }
}

fn precompute_lang(
    ctx: &PrecomputeLangCtx<'_>,
    progress: &mut dyn FnMut(ProgressEvent),
) -> anyhow::Result<LspLangManifest> {
    let started = Instant::now();
    let budget_end = started + ctx.opts.budget;
    let mut files = SourceFiles::new(ctx.input.repo_root);
    let (candidates, probe) = match ctx.opts.selection {
        Selection::Heuristic => (
            select_candidates(ctx.input, ctx.lang, ctx.opts),
            ProbeRun::default(),
        ),
        Selection::LspRefs => {
            let pool = probe_pool(ctx.input, ctx.lang, ctx.opts);
            let probe_end = started + ctx.opts.budget.mul_f64(PROBE_BUDGET_SHARE);
            let probe = run_probe(ctx, &mut files, &pool, probe_end);
            let selected = select_by_refs(ctx.input, ctx.lang, ctx.opts, &pool, &probe.probes);
            (selected, probe)
        }
    };
    let mut run = LangRun::default();
    let plan = SelectedPlan {
        candidates,
        probe,
        budget_end,
    };
    if !plan.probe.no_server {
        ask_selected(ctx, &mut files, &plan, &mut run, progress);
    }
    let probe = plan.probe;

    let (edges, _) = write_lsp_edge_shards(ctx.layout, ctx.lang, &run.shards)?;
    prune_orphan_edge_shards(ctx.layout, ctx.lang, &run.shards)?;
    write_lsp_targets(ctx.layout, ctx.lang, &run.targets)?;

    Ok(LspLangManifest {
        status: run.status().to_owned(),
        server: ctx.lang.server_program().to_owned(),
        version: run.server_version,
        files_fingerprint: ctx.fp.to_owned(),
        targets_selected: run.attempted,
        targets_ok: run.ok,
        targets_failed: run.failed,
        edges,
        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        diagnostic: run.diagnostic,
        probed: probe.probes.len(),
        probe_ms: probe.ms,
    })
}

/// The selected targets, the probe that ranked them and the language's budget.
struct SelectedPlan<'a> {
    candidates: Vec<Candidate<'a>>,
    probe: ProbeRun,
    budget_end: Instant,
}

/// Ask the server the full answer of every selected candidate, within the
/// language's budget (a probed candidate reuses its probe's `references`).
fn ask_selected(
    ctx: &PrecomputeLangCtx<'_>,
    files: &mut SourceFiles<'_>,
    plan: &SelectedPlan<'_>,
    run: &mut LangRun,
    progress: &mut dyn FnMut(ProgressEvent),
) {
    let total = plan.candidates.len();
    for (index, candidate) in plan.candidates.iter().enumerate() {
        let now = Instant::now();
        if now >= plan.budget_end {
            tracing::info!(
                "precompute for {} stopped by its budget after {}/{total} targets",
                ctx.lang.dir_name(),
                run.attempted
            );
            break;
        }
        run.attempted += 1;
        let deadline = (now + PER_TARGET_CAP).min(plan.budget_end);
        let outcome = match plan.probe.probes.get(&candidate.line.id) {
            Some(probed) => complete_with_probe(ctx, files, candidate, deadline, probed),
            None => ask_target(ctx, files, candidate, deadline, None),
        };
        if run.server_version.is_empty() {
            run.server_version = server_version(ctx.pool, ctx.lang);
        }
        if outcome.status == LiveStatus::Ok {
            collect_edges(ctx, candidate, &outcome, &mut run.shards);
        }
        run.record(candidate, &outcome, ctx.opts.selection);
        if outcome.status == LiveStatus::NoServer {
            // Terminal for this process: asking the other targets cannot help.
            break;
        }
        if (index + 1) % PROGRESS_EVERY == 0 || index + 1 == total {
            progress(ProgressEvent::LspTargets {
                lang: ctx.lang,
                done: index + 1,
                total,
            });
        }
    }
}

/// The version string the server of `lang` reported (empty before it is ready).
fn server_version(pool: &LspPool, lang: LspLang) -> String {
    pool.snapshot()
        .slots
        .get(lang.dir_name())
        .map(|slot| slot.version.clone())
        .unwrap_or_default()
}

fn prune_orphan_edge_shards(
    layout: &IndexLayout,
    lang: LspLang,
    active_shards: &BTreeMap<String, Vec<LspEdgeLine>>,
) -> anyhow::Result<()> {
    let lang_dir = layout.lsp_edges_lang_dir(lang);
    if !lang_dir.exists() {
        return Ok(());
    }
    let mut stack = vec![lang_dir.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file()
                && let Some(rel) = rel_of_record(&lang_dir, &path)
                && !active_shards.contains_key(&rel)
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use async_lsp::lsp_types::Position;
    use tempfile::TempDir;

    use super::*;
    use crate::index::lsp_records::{read_lsp_edge_shard, read_lsp_targets};
    use crate::lsp::fake::{FakeScript, FakeServers, Scene};
    use crate::lsp::uri::join_rel;

    const LIB: &str = "pub fn target() {}\npub fn caller_a() {\n    target();\n}\npub fn caller_b() {\n    target();\n}\n";

    fn shard_line(id: &str, name: &str, l0: usize, l1: usize, col: Option<u32>) -> ShardLine {
        ShardLine {
            id: id.to_owned(),
            kind: "rust_function".to_owned(),
            name: name.to_owned(),
            fqn: name.to_owned(),
            lang: "rust".to_owned(),
            l0,
            l1,
            col,
            sig: None,
            h: "h".to_owned(),
            q: None,
            s: None,
            tc: false,
        }
    }

    /// A repository on disk plus the index data pre-computation reads.
    struct Rig {
        tmp: TempDir,
        layout: IndexLayout,
        records: Vec<(String, ShardLine)>,
        fanin: HashMap<String, usize>,
        inventory: Inventory,
    }

    impl Rig {
        /// `target` is called by `caller_a` and `caller_b` (ids `t`, `ca`, `cb`).
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
            std::fs::write(join_rel(tmp.path(), "src/lib.rs"), LIB).expect("write lib");
            std::fs::write(join_rel(tmp.path(), "src/main.rs"), "fn main() {}\n")
                .expect("write main");
            let records = vec![
                (
                    "src/lib.rs".to_owned(),
                    shard_line("t", "target", 1, 1, Some(7)),
                ),
                (
                    "src/lib.rs".to_owned(),
                    shard_line("ca", "caller_a", 2, 4, Some(7)),
                ),
                (
                    "src/lib.rs".to_owned(),
                    shard_line("cb", "caller_b", 5, 7, Some(7)),
                ),
            ];
            let inventory = Inventory::new(vec![
                InventoryEntry {
                    rel: "src/lib.rs".to_owned(),
                    sha: "aaaa".to_owned(),
                    bytes: 1,
                },
                InventoryEntry {
                    rel: "src/main.rs".to_owned(),
                    sha: "bbbb".to_owned(),
                    bytes: 1,
                },
            ]);
            Self {
                layout: IndexLayout::for_repo(tmp.path()),
                tmp,
                records,
                fanin: HashMap::from([("t".to_owned(), 5)]),
                inventory,
            }
        }

        /// What a Rust server answers for `target` at (0, 7): `caller_a` calls it
        /// (and references it from the same lines), `caller_b` references it,
        /// `main.rs` references it outside any function, and the target's own
        /// body mentions itself.
        fn script(&self) -> impl Fn(LspLang) -> FakeScript + Send + Sync + 'static {
            let scene = Scene::new(self.tmp.path());
            move |_| {
                let position = Position::new(0, 7);
                FakeScript {
                    prepare: vec![(position, vec![scene.item("target", "src/lib.rs", 0)])],
                    incoming: vec![(
                        "target".to_owned(),
                        vec![scene.call("caller_a", "src/lib.rs", 1)],
                    )],
                    references: vec![(
                        position,
                        vec![
                            scene.location("src/lib.rs", 0),
                            scene.location("src/lib.rs", 2),
                            scene.location("src/lib.rs", 5),
                            scene.location("src/main.rs", 0),
                        ],
                    )],
                    ..FakeScript::full()
                }
            }
        }

        /// Run against a fresh pool of the standard servers; returns the
        /// manifest and how many servers the pool started.
        fn run_standard(
            &self,
            opts: &PrecomputeOptions,
            previous: Option<&LspManifest>,
        ) -> (LspManifest, usize) {
            let servers = FakeServers::new(self.script());
            let starts = servers.starts();
            let pool = servers.pool();
            let manifest = self.run(opts, Some(&pool), previous);
            (manifest, starts.load(Ordering::SeqCst))
        }

        fn run(
            &self,
            opts: &PrecomputeOptions,
            pool: Option<&LspPool>,
            previous: Option<&LspManifest>,
        ) -> LspManifest {
            let input = PrecomputeInput {
                records: &self.records,
                heuristic_fanin: &self.fanin,
                inventory: &self.inventory,
                previous,
                repo_root: self.tmp.path(),
            };
            run(&input, opts, pool, &self.layout, &mut |_| {}).expect("run")
        }

        fn edges_of(&self, caller_rel: &str) -> Vec<LspEdgeLine> {
            let path = self
                .layout
                .lsp_edges_for(LspLang::Rust, caller_rel)
                .expect("path");
            read_lsp_edge_shard(&path).expect("shard")
        }

        fn targets(&self) -> Vec<LspTargetLine> {
            read_lsp_targets(&self.layout.lsp_targets_for(LspLang::Rust)).expect("targets")
        }
    }

    fn all(min_fanin: usize) -> PrecomputeOptions {
        PrecomputeOptions {
            policy: PrecomputePolicy::All,
            min_fanin,
            max_targets: 10,
            budget: Duration::from_secs(600),
            selection: Selection::Heuristic,
            probe_min_fanin: 3,
        }
    }

    fn edge(src: Option<&str>, rel: &str, n: u32) -> LspEdgeLine {
        LspEdgeLine {
            src: src.map(str::to_owned),
            dst: "t".to_owned(),
            rel: rel.to_owned(),
            n,
        }
    }

    #[test]
    fn auto_policy_selects_java_only() {
        let present = BTreeSet::from([LspLang::Rust, LspLang::Java]);
        assert_eq!(
            languages_for(PrecomputePolicy::Auto, &present),
            vec![LspLang::Java]
        );
        let rust_only = BTreeSet::from([LspLang::Rust]);
        assert!(languages_for(PrecomputePolicy::Auto, &rust_only).is_empty());
    }

    #[test]
    fn all_policy_selects_present_languages() {
        let present = BTreeSet::from([LspLang::Rust, LspLang::Ts]);
        assert_eq!(
            languages_for(PrecomputePolicy::All, &present),
            vec![LspLang::Rust, LspLang::Ts]
        );
        assert!(languages_for(PrecomputePolicy::None, &present).is_empty());
    }

    #[test]
    fn none_policy_removes_previous_lsp_data() {
        let rig = Rig::new();
        let lang_dir = rig.layout.lsp_edges_lang_dir(LspLang::Rust);
        std::fs::create_dir_all(&lang_dir).expect("mkdir");
        std::fs::write(lang_dir.join("lib.rs.jsonl"), "dummy\n").expect("write");
        std::fs::create_dir_all(rig.layout.lsp_targets()).expect("mkdir");
        std::fs::write(rig.layout.lsp_targets_for(LspLang::Rust), "dummy\n").expect("write");

        let opts = PrecomputeOptions {
            policy: PrecomputePolicy::None,
            ..PrecomputeOptions::default()
        };
        let manifest = rig.run(&opts, None, None);

        assert_eq!(manifest.policy, "none");
        assert_eq!(manifest.languages["rust"].status, "none");
        assert!(!lang_dir.exists());
        assert!(!rig.layout.lsp_targets_for(LspLang::Rust).exists());
    }

    #[test]
    fn edges_are_written_per_caller_file_sorted() {
        let rig = Rig::new();
        let (manifest, _) = rig.run_standard(&all(2), None);

        let rust = &manifest.languages["rust"];
        assert_eq!(rust.status, "computed");
        assert_eq!((rust.targets_selected, rust.targets_ok), (1, 1));
        // `caller_a` calls the target: its reference on the same lines is not repeated.
        // The target's own mention is recursion and is dropped.
        assert_eq!(
            rig.edges_of("src/lib.rs"),
            vec![
                edge(Some("ca"), "CALLS", 1),
                edge(Some("cb"), "REFERENCES", 1)
            ]
        );
        assert_eq!(rust.edges, 3);
    }

    #[test]
    fn file_level_callers_have_null_src() {
        let rig = Rig::new();
        rig.run_standard(&all(2), None);
        assert_eq!(
            rig.edges_of("src/main.rs"),
            vec![edge(None, "REFERENCES", 1)]
        );
    }

    #[test]
    fn targets_file_lists_every_attempt_with_status() {
        let mut rig = Rig::new();
        // `ghost` is not in the file: it cannot be positioned and is never asked.
        rig.records.push((
            "src/lib.rs".to_owned(),
            shard_line("g", "ghost", 5, 7, Some(7)),
        ));
        rig.fanin.insert("g".to_owned(), 4);
        rig.run_standard(&all(2), None);

        let targets = rig.targets();
        let statuses: Vec<(&str, &str)> = targets
            .iter()
            .map(|line| (line.id.as_str(), line.status.as_str()))
            .collect();
        assert_eq!(statuses, vec![("g", "no_position"), ("t", "ok")]);
        let answered = targets.iter().find(|line| line.id == "t").expect("t");
        assert_eq!(answered.ops.incoming, Some(1));
        assert_eq!(answered.ops.references, Some(4));
        assert_eq!(answered.ops.implementation, None);
    }

    #[test]
    fn targets_are_selected_by_fanin_then_id_and_capped() {
        let mut rig = Rig::new();
        rig.records = ["t1", "t2", "t3"]
            .iter()
            .zip([1, 10, 20])
            .map(|(id, l0)| {
                (
                    "src/lib.rs".to_owned(),
                    shard_line(id, id, l0, l0 + 1, Some(0)),
                )
            })
            .collect();
        rig.fanin = HashMap::from([
            ("t1".to_owned(), 25),
            ("t2".to_owned(), 30),
            ("t3".to_owned(), 25),
        ]);
        let opts = PrecomputeOptions {
            max_targets: 2,
            ..all(20)
        };
        let input = PrecomputeInput {
            records: &rig.records,
            heuristic_fanin: &rig.fanin,
            inventory: &rig.inventory,
            previous: None,
            repo_root: rig.tmp.path(),
        };
        let ids: Vec<&str> = select_candidates(&input, LspLang::Rust, &opts)
            .iter()
            .map(|candidate| candidate.line.id.as_str())
            .collect();
        // Highest fan-in first; the tie between `t1` and `t3` goes to the smaller id.
        assert_eq!(ids, vec!["t2", "t1"]);
    }

    #[test]
    fn records_without_col_and_below_min_fanin_are_not_selected() {
        let mut rig = Rig::new();
        rig.records[0].1.col = None;
        let opts = all(2);
        let input = PrecomputeInput {
            records: &rig.records,
            heuristic_fanin: &rig.fanin,
            inventory: &rig.inventory,
            previous: None,
            repo_root: rig.tmp.path(),
        };
        assert!(select_candidates(&input, LspLang::Rust, &opts).is_empty());

        rig.records[0].1.col = Some(7);
        let input = PrecomputeInput {
            records: &rig.records,
            heuristic_fanin: &rig.fanin,
            inventory: &rig.inventory,
            previous: None,
            repo_root: rig.tmp.path(),
        };
        assert!(select_candidates(&input, LspLang::Rust, &all(6)).is_empty());
        assert_eq!(select_candidates(&input, LspLang::Rust, &all(5)).len(), 1);
    }

    #[test]
    fn budget_stops_selection_and_counts_attempted_only() {
        let mut rig = Rig::new();
        for id in ["u", "v"] {
            rig.records.push((
                "src/lib.rs".to_owned(),
                shard_line(id, "target", 1, 1, Some(7)),
            ));
            rig.fanin.insert(id.to_owned(), 5);
        }
        // Every request outlives the whole budget: the first target uses it up.
        let pool = FakeServers::new(|_| FakeScript {
            delay: Duration::from_millis(600),
            ..FakeScript::full()
        })
        .pool();
        let opts = PrecomputeOptions {
            budget: Duration::from_millis(250),
            ..all(2)
        };
        let manifest = rig.run(&opts, Some(&pool), None);

        let rust = &manifest.languages["rust"];
        assert_eq!(rust.targets_selected, 1, "{rust:?}");
        assert_eq!(rig.targets().len(), 1);
    }

    #[test]
    fn unchanged_language_files_reuse_without_starting_server() {
        let rig = Rig::new();
        let (first, started) = rig.run_standard(&all(2), None);
        assert_eq!(started, 1);
        let edges_before = rig.edges_of("src/lib.rs");

        let (second, started) = rig.run_standard(&all(2), Some(&first));
        assert_eq!(second.languages["rust"].status, "reused");
        assert_eq!(started, 0);
        assert_eq!(rig.edges_of("src/lib.rs"), edges_before);
    }

    #[test]
    fn changed_language_file_recomputes() {
        let mut rig = Rig::new();
        let (first, _) = rig.run_standard(&all(2), None);
        rig.inventory = Inventory::new(vec![InventoryEntry {
            rel: "src/lib.rs".to_owned(),
            sha: "changed".to_owned(),
            bytes: 1,
        }]);

        let (second, started) = rig.run_standard(&all(2), Some(&first));
        assert_eq!(second.languages["rust"].status, "computed");
        assert_eq!(started, 1);
    }

    #[test]
    fn parameter_change_recomputes() {
        let rig = Rig::new();
        let (first, _) = rig.run_standard(&all(2), None);

        let (second, started) = rig.run_standard(&all(3), Some(&first));
        assert_eq!(second.languages["rust"].status, "computed");
        assert_eq!(second.min_fanin, 3);
        assert_eq!(started, 1);
    }

    #[test]
    fn missing_server_marks_failed_and_removes_stale_data() {
        let mut rig = Rig::new();
        rig.records.push((
            "src/lib.rs".to_owned(),
            shard_line("u", "target", 1, 1, Some(7)),
        ));
        rig.fanin.insert("u".to_owned(), 5);
        let stale = rig
            .layout
            .lsp_edges_for(LspLang::Rust, "src/old.rs")
            .expect("path");
        std::fs::create_dir_all(stale.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &stale,
            "{\"src\":null,\"dst\":\"x\",\"rel\":\"CALLS\",\"n\":1}\n",
        )
        .expect("write");

        let pool = FakeServers::missing().pool();
        let manifest = rig.run(&all(2), Some(&pool), None);

        let rust = &manifest.languages["rust"];
        assert_eq!(rust.status, "failed");
        assert!(rust.diagnostic.contains("not found in PATH"), "{rust:?}");
        assert!(!stale.exists(), "stale edge shard survived");
        // A missing server is terminal: the second target is not tried.
        assert_eq!(rust.targets_selected, 1);
        assert_eq!(rig.targets()[0].status, "no_server");
    }

    #[test]
    fn disabled_lsp_skips_with_a_diagnostic() {
        let rig = Rig::new();
        let manifest = rig.run(&all(2), None, None);
        let rust = &manifest.languages["rust"];
        assert_eq!(rust.status, "skipped");
        assert!(rust.diagnostic.contains("KNOT_LITE_LSP"), "{rust:?}");
    }

    #[test]
    fn fingerprint_ignores_other_languages_and_tracks_content() {
        let rig = Rig::new();
        let base = fingerprint_for_lang(&rig.inventory, LspLang::Rust);
        let mut changed = rig.inventory.entries.clone();
        changed[1].sha = "different".to_owned(); // main.rs is Rust too
        assert_ne!(
            base,
            fingerprint_for_lang(&Inventory::new(changed), LspLang::Rust)
        );
        let mut other = rig.inventory.entries.clone();
        other.push(InventoryEntry {
            rel: "docs/readme.md".to_owned(),
            sha: "x".to_owned(),
            bytes: 1,
        });
        assert_eq!(
            base,
            fingerprint_for_lang(&Inventory::new(other), LspLang::Rust)
        );
    }

    fn lsp_refs(min_fanin: usize) -> PrecomputeOptions {
        PrecomputeOptions {
            selection: Selection::LspRefs,
            ..all(min_fanin)
        }
    }

    /// Methods every started server received, in order.
    fn requests_seen(
        servers: &std::sync::Mutex<Vec<std::sync::Arc<crate::lsp::fake::FakeCounters>>>,
    ) -> Vec<String> {
        servers
            .lock()
            .expect("counters")
            .iter()
            .flat_map(|counters| counters.requests())
            .collect()
    }

    fn input_of(rig: &Rig) -> PrecomputeInput<'_> {
        PrecomputeInput {
            records: &rig.records,
            heuristic_fanin: &rig.fanin,
            inventory: &rig.inventory,
            previous: None,
            repo_root: rig.tmp.path(),
        }
    }

    #[test]
    fn full_stage_reuses_probe_references() {
        let rig = Rig::new();
        let servers = FakeServers::new(rig.script());
        let counters = servers.counters();
        let pool = servers.pool();
        let manifest = rig.run(&lsp_refs(2), Some(&pool), None);
        let rust = &manifest.languages["rust"];
        assert_eq!(
            (rust.status.as_str(), rust.targets_ok, rust.probed),
            ("computed", 1, 1)
        );
        // The probe asked `references`; the full answer only the call hierarchy.
        assert_eq!(
            requests_seen(&counters),
            vec![
                "textDocument/references",
                "textDocument/prepareCallHierarchy",
                "callHierarchy/incomingCalls"
            ]
        );
        // The same edges as the heuristic selection's full answer.
        assert_eq!(
            rig.edges_of("src/lib.rs"),
            vec![
                edge(Some("ca"), "CALLS", 1),
                edge(Some("cb"), "REFERENCES", 1)
            ]
        );
        let target = &rig.targets()[0];
        assert_eq!((target.refs, target.h), (Some(3), Some(5)));
        assert_eq!(
            (target.ops.incoming, target.ops.references),
            (Some(1), Some(4))
        );
        assert_eq!(
            (
                manifest.format,
                manifest.selection.as_str(),
                manifest.probe_min_fanin
            ),
            (2, "lsp-refs", 3)
        );
    }

    #[test]
    fn type_like_targets_need_no_second_request() {
        let mut rig = Rig::new();
        rig.records[0].1.kind = "rust_struct".to_owned();
        let servers = FakeServers::new(rig.script());
        let counters = servers.counters();
        let pool = servers.pool();
        let manifest = rig.run(&lsp_refs(2), Some(&pool), None);
        assert_eq!(manifest.languages["rust"].targets_ok, 1);
        assert_eq!(requests_seen(&counters), vec!["textDocument/references"]);
        assert_eq!(rig.targets()[0].ops.references, Some(4));
    }

    #[test]
    fn heuristic_mode_writes_phase4_lines() {
        let rig = Rig::new();
        rig.run_standard(&all(2), None);
        let text =
            std::fs::read_to_string(rig.layout.lsp_targets_for(LspLang::Rust)).expect("read");
        assert!(
            !text.contains("\"refs\"") && !text.contains("\"h\""),
            "{text}"
        );
        assert_eq!(rig.targets()[0].refs, None);
    }

    #[test]
    fn probe_pool_includes_types_regardless_of_fanin() {
        let mut rig = Rig::new();
        rig.records = vec![
            (
                "src/lib.rs".to_owned(),
                shard_line("f0", "f0", 1, 2, Some(0)),
            ),
            (
                "src/lib.rs".to_owned(),
                shard_line("f9", "f9", 3, 4, Some(0)),
            ),
            (
                "src/lib.rs".to_owned(),
                shard_line("s0", "s0", 5, 6, Some(0)),
            ),
            (
                "src/lib.rs".to_owned(),
                shard_line("m0", "m0", 7, 8, Some(0)),
            ),
        ];
        rig.records[2].1.kind = "rust_struct".to_owned();
        rig.records[3].1.kind = "rust_module".to_owned();
        rig.fanin = HashMap::from([("f9".to_owned(), 9)]);
        let input = input_of(&rig);
        let ids: Vec<&str> = probe_pool(&input, LspLang::Rust, &lsp_refs(20))
            .iter()
            .map(|candidate| candidate.line.id.as_str())
            .collect();
        // f9 (fan-in 9 ≥ 3), the struct with fan-in 0; not f0 (below 3), not the module.
        assert_eq!(ids, vec!["f9", "s0"]);
    }

    #[test]
    fn probe_pool_cap_and_order() {
        let mut rig = Rig::new();
        rig.records = (0..12)
            .map(|i| {
                let id = format!("f{i:02}");
                (
                    "src/lib.rs".to_owned(),
                    shard_line(&id, &id, i * 2 + 1, i * 2 + 2, Some(0)),
                )
            })
            .collect();
        rig.fanin = (0..12).map(|i| (format!("f{i:02}"), 3 + i % 3)).collect();
        let opts = PrecomputeOptions {
            max_targets: 2,
            ..lsp_refs(20)
        };
        let input = input_of(&rig);
        let ids: Vec<String> = probe_pool(&input, LspLang::Rust, &opts)
            .iter()
            .map(|candidate| candidate.line.id.clone())
            .collect();
        assert_eq!(
            ids,
            vec!["f02", "f05", "f08", "f11", "f01", "f04", "f07", "f10"]
        );
    }

    #[test]
    fn selection_orders_by_lsp_count_then_heuristic_then_id_and_appends_unprobed() {
        let mut rig = Rig::new();
        rig.records = ["a", "b", "c", "d", "e"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                (
                    "src/lib.rs".to_owned(),
                    shard_line(id, id, i * 3 + 1, i * 3 + 2, Some(0)),
                )
            })
            .collect();
        rig.fanin = HashMap::from([
            ("a".to_owned(), 30),
            ("b".to_owned(), 25),
            ("c".to_owned(), 25),
            ("d".to_owned(), 4),
            ("e".to_owned(), 40),
        ]);
        let input = input_of(&rig);
        let opts = lsp_refs(20);
        let pool = probe_pool(&input, LspLang::Rust, &opts);
        let probe = |count| Probe {
            outcome: LiveOutcome::without_answer(LiveStatus::Ok, ""),
            count,
        };
        // a: 22 callers, b and c: 50 each, d: 3 (below min_fanin); e: not probed.
        let probes = HashMap::from([
            ("a".to_owned(), probe(22)),
            ("b".to_owned(), probe(50)),
            ("c".to_owned(), probe(50)),
            ("d".to_owned(), probe(3)),
        ]);
        let ids: Vec<(&str, Option<usize>)> =
            select_by_refs(&input, LspLang::Rust, &opts, &pool, &probes)
                .iter()
                .map(|candidate| (candidate.line.id.as_str(), candidate.refs))
                .collect();
        assert_eq!(
            ids,
            vec![
                ("b", Some(50)),
                ("c", Some(50)),
                ("a", Some(22)),
                ("e", None)
            ]
        );
    }

    #[test]
    fn probe_stops_at_its_share_of_the_budget() {
        let rig = Rig::new();
        let opts = PrecomputeOptions {
            budget: Duration::ZERO,
            ..lsp_refs(2)
        };
        let (manifest, _) = rig.run_standard(&opts, None);
        let rust = &manifest.languages["rust"];
        assert_eq!((rust.probed, rust.targets_selected), (0, 0));
    }

    #[test]
    fn skip_unchanged_includes_selection_options() {
        let rig = Rig::new();
        let (first, _) = rig.run_standard(&all(2), None);
        let (again, starts) = rig.run_standard(&all(2), Some(&first));
        assert_eq!(
            (again.languages["rust"].status.as_str(), starts),
            ("reused", 0)
        );
        let (switched, starts) = rig.run_standard(&lsp_refs(2), Some(&first));
        assert_eq!(
            (switched.languages["rust"].status.as_str(), starts),
            ("computed", 1)
        );
        let probe_changed = PrecomputeOptions {
            probe_min_fanin: 9,
            ..lsp_refs(2)
        };
        let (changed, starts) = rig.run_standard(&probe_changed, Some(&switched));
        assert_eq!(
            (changed.languages["rust"].status.as_str(), starts),
            ("computed", 1)
        );
    }

    #[test]
    fn manifest_format_2_round_trip_and_format_1_reads_as_heuristic() {
        let rig = Rig::new();
        let (manifest, _) = rig.run_standard(&lsp_refs(2), None);
        let text = toml::to_string(&manifest).expect("toml");
        let back: LspManifest = toml::from_str(&text).expect("parse");
        assert_eq!(back, manifest);
        let old = "format = 1\npolicy = \"all\"\nmin_fanin = 20\nmax_targets = 200\nbudget_secs = 600\n\n[languages]\n";
        let old: LspManifest = toml::from_str(old).expect("format 1");
        assert_eq!(
            (old.selection.as_str(), old.probe_min_fanin),
            ("heuristic", 0)
        );
    }
}
