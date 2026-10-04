//! S1 probe run flow (T5.2) and result schema (T5.3).
//!
//! Layout deviation (logged in the plan): the S1 result types and the step
//! functions live together here instead of splitting the schema into
//! `common.rs`, so the producer and its JSON shape stay in sync.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_lsp::lsp_types::{
    DocumentSymbol, DocumentSymbolResponse, Location, Position, Range, Url,
};
use serde::{Deserialize, Serialize};

use crate::common::{self, EntityKey, Lang, Percentiles, S1_SCHEMA};
use crate::entities::EntityTable;
use crate::graph::normalize::{
    LocResult, Normalized, container_set, file_set, normalize_call_item, normalize_location,
    self_reference_count, union_sets,
};
use crate::groundtruth::{self, Target, TargetsFile};
use crate::lsp::client::{self, LspSession, RequestOutcome};
use crate::lsp::readiness::{self, ReadyOutcome};
use crate::lsp::rss::{RssReport, RssSampler};
use crate::lsp::servers::{self, ServerSpec};
use crate::position::{self, NamePos};
use crate::uri::path_to_uri;

/// Cold = wipe the server's persistent scratch area first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RunKind {
    Cold,
    Warm,
}

/// One probe run over one targets file (T5.1 `run`).
pub struct RunConfig {
    pub targets: PathBuf,
    pub run: RunKind,
    pub out: PathBuf,
    pub max_targets: Option<usize>,
    pub skip_column_check: bool,
    pub spike_root: PathBuf,
    pub groundtruth_dir: PathBuf,
}

/// Position of a name in LSP coordinates (result JSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PosInfo {
    pub line0: u32,
    pub col_utf16: u32,
}

impl From<NamePos> for PosInfo {
    fn from(pos: NamePos) -> Self {
        Self {
            line0: pos.line0,
            col_utf16: pos.col_utf16,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfoResult {
    pub program: String,
    pub args: Vec<String>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseInfo {
    pub files: usize,
    pub entities: usize,
    pub parse_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesInfo {
    pub call_hierarchy: bool,
    pub type_hierarchy: bool,
    pub implementation: bool,
    pub references: bool,
    pub document_symbol: bool,
    pub workspace_symbol: bool,
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingsInfo {
    pub spawn_to_initialize: u64,
    pub initialize_to_ready: u64,
    pub first_request: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyInfo {
    pub ready: bool,
    pub reason: String,
    pub jdtls_status_timeline: Vec<(u64, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RssInfo {
    pub peak_bytes: u64,
    pub at_ready_bytes: u64,
    pub end_bytes: u64,
    pub peak_process_count: usize,
    pub timeline: Vec<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnMismatch {
    pub file: String,
    pub name: String,
    pub entity_line: usize,
    pub reason: String,
    pub expected: PosInfo,
    pub symbol: Option<PosInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnCheck {
    /// Entities whose name position was computed and matched against a symbol.
    pub compared: usize,
    /// Subset of `compared` whose `selection_range.start` equals the computed position.
    pub matched: usize,
    /// Entities whose name could not be located in the source.
    pub failed_to_locate: usize,
    /// Entities in symbolized files with no same-name document symbol.
    pub symbol_not_found: usize,
    /// Files skipped: the server offers no document symbols for them
    /// (e.g. Markdown `markdown_section` entities).
    pub files_without_symbols: usize,
    /// Up to 50 examples (`differs` and `symbol_not_found`).
    pub mismatches: Vec<ColumnMismatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingOp {
    pub supported: bool,
    pub prepare_ms: f64,
    pub calls_ms: f64,
    pub retries: u32,
    pub error: Option<String>,
    pub raw_count: usize,
    pub external_count: usize,
    pub self_count: usize,
    pub locations: Vec<LocResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefOp {
    pub supported: bool,
    pub ms: f64,
    pub retries: u32,
    pub error: Option<String>,
    pub raw_count: usize,
    pub external_count: usize,
    pub self_count: usize,
    pub locations: Vec<LocResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetOps {
    pub incoming: Option<IncomingOp>,
    pub references: Option<RefOp>,
    pub implementation: Option<RefOp>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainersByOp {
    pub incoming: Vec<EntityKey>,
    pub references: Vec<EntityKey>,
    pub implementation: Vec<EntityKey>,
    pub union: Vec<EntityKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetResult {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub category: String,
    pub status: String,
    pub position: Option<PosInfo>,
    pub retried_empty: bool,
    pub ops: TargetOps,
    pub lsp_containers: ContainersByOp,
    pub lsp_files: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyInfo {
    pub prepare_call_hierarchy: Percentiles,
    pub incoming_calls: Percentiles,
    pub references: Percentiles,
    pub implementation: Percentiles,
}

/// The `results/s1_<lang>_<run>.json` document (T5.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S1Result {
    pub schema: String,
    pub lang: Lang,
    pub repo: String,
    pub run: String,
    pub root: String,
    pub snapshot_fingerprint: String,
    pub server: ServerInfoResult,
    pub started_at: String,
    pub parse: ParseInfo,
    pub capabilities: CapabilitiesInfo,
    pub timings_ms: TimingsInfo,
    pub ready: ReadyInfo,
    pub rss: RssInfo,
    pub server_requests_seen: Vec<String>,
    pub notifications_seen: BTreeMap<String, u64>,
    pub column_heuristic: ColumnCheck,
    pub targets: Vec<TargetResult>,
    pub latency_ms: LatencyInfo,
}

/// Request latencies collected across the run (T5.2 item 9 / step "first").
#[derive(Debug, Default)]
struct LatencySamples {
    prepare: Vec<f64>,
    calls: Vec<f64>,
    references: Vec<f64>,
    implementation: Vec<f64>,
    first_request: Option<f64>,
}

impl LatencySamples {
    fn note_first(&mut self, latency_ms: f64) {
        self.first_request.get_or_insert(latency_ms);
    }

    fn into_info(self) -> (LatencyInfo, f64) {
        let info = LatencyInfo {
            prepare_call_hierarchy: common::percentiles(&self.prepare),
            incoming_calls: common::percentiles(&self.calls),
            references: common::percentiles(&self.references),
            implementation: common::percentiles(&self.implementation),
        };
        (info, self.first_request.unwrap_or_default())
    }
}

/// Borrowed context for the per-target loop (keeps signatures small).
struct TargetContext<'a> {
    session: &'a mut LspSession,
    table: &'a EntityTable,
    root: &'a Path,
    lang: Lang,
    gt_callers: BTreeMap<String, usize>,
    latencies: &'a mut LatencySamples,
}

/// Execute one `lsp_probe run` (T5.2).
pub async fn run_probe(cfg: &RunConfig) -> Result<()> {
    let started_at = common::now_rfc3339();
    let targets_file = TargetsFile::load(&cfg.targets)?;
    let root = PathBuf::from(&targets_file.root);

    let (table, parsed, parse_info) = step_parse(&targets_file)?;
    step_validate(&targets_file, &table, &parsed)?;
    let gt_callers = step_ground_truth(&targets_file, &table, &cfg.groundtruth_dir);
    step_cold_reset(targets_file.lang, &root, &cfg.spike_root, cfg.run)?;

    let spec = servers::spec_for(targets_file.lang, &root, &cfg.spike_root);
    let (mut session, sampler) = step_start(&spec, &root, &cfg.spike_root).await?;
    step_preopen(&mut session, targets_file.lang, &root)?;
    let ready = step_wait_ready(&mut session, &spec).await;
    sampler.mark("ready");

    let mut latencies = LatencySamples::default();
    let column = if cfg.skip_column_check {
        ColumnCheck::default()
    } else {
        step_column_check(
            &mut session,
            &table,
            &root,
            targets_file.lang,
            &mut latencies,
        )
        .await
    };

    let targets = {
        let mut context = TargetContext {
            session: &mut session,
            table: &table,
            root: &root,
            lang: targets_file.lang,
            gt_callers,
            latencies: &mut latencies,
        };
        step_targets(&mut context, &targets_file, cfg.max_targets).await
    };

    let capabilities = capability_flags(&session.init_result.capabilities);
    let server_requests_seen: Vec<String> = session
        .seen_server_requests
        .lock()
        .map(|seen| seen.iter().cloned().collect())
        .unwrap_or_default();
    let notifications_seen: BTreeMap<String, u64> = session
        .notif_counts
        .lock()
        .map(|counts| counts.clone())
        .unwrap_or_default();
    let version = server_version(&session);
    let timings = session.timings;
    let jdtls_status_timeline = session.events.borrow().jdtls_status_timeline.clone();

    sampler.mark("end");
    let rss_report = sampler.stop().await;
    client::shutdown(session, Duration::from_secs(30)).await?;

    let (latency_ms, first_request) = latencies.into_info();
    let result = S1Result {
        schema: S1_SCHEMA.to_owned(),
        lang: targets_file.lang,
        repo: targets_file.repo.clone(),
        run: run_label(cfg.run).to_owned(),
        root: targets_file.root.clone(),
        snapshot_fingerprint: read_fingerprint(&cfg.spike_root, &targets_file.repo),
        server: ServerInfoResult {
            program: spec.program.clone(),
            args: spec.args.clone(),
            version,
        },
        started_at,
        parse: parse_info,
        capabilities,
        timings_ms: TimingsInfo {
            spawn_to_initialize: timings.spawn_ms + timings.initialize_ms,
            initialize_to_ready: ready.elapsed.as_millis() as u64,
            first_request,
        },
        ready: ReadyInfo {
            ready: ready.ready,
            reason: ready.reason,
            jdtls_status_timeline,
        },
        rss: rss_info(&rss_report),
        server_requests_seen,
        notifications_seen,
        column_heuristic: column,
        targets,
        latency_ms,
    };
    common::write_json_pretty(&cfg.out, &result)?;
    println!(
        "wrote {} ({} targets, ready={})",
        cfg.out.display(),
        result.targets.len(),
        result.ready.ready
    );
    Ok(())
}

fn run_label(run: RunKind) -> &'static str {
    match run {
        RunKind::Cold => "cold",
        RunKind::Warm => "warm",
    }
}

/// Step 1: parse the snapshot with knot, timed (T5.2).
fn step_parse(
    targets_file: &TargetsFile,
) -> Result<(EntityTable, Vec<knot::models::ParsedEntity>, ParseInfo)> {
    let started = std::time::Instant::now();
    let (table, parsed) = EntityTable::build(&targets_file.repo, Path::new(&targets_file.root))?;
    let parse_ms = started.elapsed().as_millis() as u64;
    let files = table.file_count();
    let entities = table.entities.len();
    eprintln!("# parse: {files} files, {entities} entities, {parse_ms} ms");
    Ok((
        table,
        parsed,
        ParseInfo {
            files,
            entities,
            parse_ms,
        },
    ))
}

/// Step 2: validate targets against the snapshot; abort on missing (T5.2).
fn step_validate(
    targets_file: &TargetsFile,
    table: &EntityTable,
    parsed: &[knot::models::ParsedEntity],
) -> Result<()> {
    let rows = groundtruth::validate_targets(targets_file, table, parsed);
    let missing: Vec<&str> = rows
        .iter()
        .filter(|row| row.status == groundtruth::TargetStatus::Missing)
        .map(|row| row.id.as_str())
        .collect();
    if !missing.is_empty() {
        anyhow::bail!("targets missing from the snapshot: {}", missing.join(", "));
    }
    Ok(())
}

/// Step 3: filtered ground-truth caller counts per target (T5.2, logged only).
fn step_ground_truth(
    targets_file: &TargetsFile,
    table: &EntityTable,
    gt_dir: &Path,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for target in &targets_file.targets {
        let path = gt_dir.join(format!("callers_{}_{}.json", targets_file.repo, target.id));
        let count = groundtruth::load_and_filter(&path, target, table)
            .map(|gt| gt.callers.len())
            .unwrap_or_else(|err| {
                eprintln!("# ground truth unavailable for {}: {err:#}", target.id);
                0
            });
        counts.insert(target.id.clone(), count);
    }
    counts
}

/// Step 4: cold runs wipe the server's persistent scratch area (T5.2).
fn step_cold_reset(lang: Lang, root: &Path, spike_root: &Path, run: RunKind) -> Result<()> {
    if run != RunKind::Cold {
        return Ok(());
    }
    let victim = match lang {
        Lang::Rust => spike_root.join("target-ra"),
        Lang::Java => spike_root.join("jdtls-ws").join(file_name(root)),
        Lang::Ts => return Ok(()), // tsserver has no persistent cache
    };
    if victim.exists() {
        std::fs::remove_dir_all(&victim)
            .with_context(|| format!("removing {}", victim.display()))?;
    }
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "workspace".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Step 5: start the server and the RSS sampler (T5.2).
async fn step_start(
    spec: &ServerSpec,
    root: &Path,
    spike_root: &Path,
) -> Result<(LspSession, RssSampler)> {
    let session = client::start(spec, root, &spike_root.join("logs")).await?;
    let pid = session.child.id().context("LSP child pid")?;
    Ok((session, RssSampler::start(pid, Duration::from_millis(250))))
}

/// Step 6: pre-open one file per tsconfig project (T5.2).
fn step_preopen(session: &mut LspSession, lang: Lang, root: &Path) -> Result<()> {
    let files: &[&str] = match lang {
        Lang::Ts => &[
            "src/main.tsx",
            "functions/mcp.ts",
            "scripts/fetch-updates.mjs",
        ],
        Lang::Rust | Lang::Java => &[],
    };
    for file in files {
        let path = root.join(file);
        if path.is_file() {
            session.did_open(&path)?;
        }
    }
    Ok(())
}

/// Step 7: wait for the server's ready signal (T5.2).
async fn step_wait_ready(session: &mut LspSession, spec: &ServerSpec) -> ReadyOutcome {
    readiness::wait_ready(
        &mut session.events,
        spec.readiness,
        Duration::from_secs(2),
        spec.ready_timeout,
    )
    .await
}

/// Step 8: verify `position.rs` against the server's own symbol ranges (T5.2).
async fn step_column_check(
    session: &mut LspSession,
    table: &EntityTable,
    root: &Path,
    lang: Lang,
    latencies: &mut LatencySamples,
) -> ColumnCheck {
    let files = column_check_files(table);
    let mut check = ColumnCheck::default();
    for file in files {
        let path = root.join(&file);
        if session.did_open(&path).is_err() {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(uri) = path_to_uri(&path) else {
            continue;
        };
        let outcome = session.document_symbol(&uri).await;
        latencies.note_first(outcome.latency.as_millis() as f64);
        let Ok(Some(response)) = outcome.result else {
            continue;
        };
        let symbols = flatten_symbols(&response);
        if symbols.is_empty() {
            // Nothing to compare against (Markdown, config, ...).
            check.files_without_symbols += 1;
            continue;
        }
        let input = SymbolFile {
            file: &file,
            source: &source,
            lang,
            symbols: &symbols,
        };
        check_file_symbols(&mut check, table, &input);
    }
    eprintln!(
        "# column check: compared={} matched={} failed_to_locate={} symbol_not_found={} no_symbol_files={}",
        check.compared,
        check.matched,
        check.failed_to_locate,
        check.symbol_not_found,
        check.files_without_symbols
    );
    check
}

/// Deterministic sample: files with >= 3 entities, sorted, every k-th (T5.2).
fn column_check_files(table: &EntityTable) -> Vec<String> {
    const WANT: usize = 30;
    let mut files: Vec<String> = table
        .entities
        .iter()
        .map(|entity| entity.key.file.clone())
        .collect();
    files.sort();
    files.dedup();
    let files: Vec<String> = files
        .into_iter()
        .filter(|file| table.entities_in(file).len() >= 3)
        .collect();
    if files.len() <= WANT {
        return files;
    }
    let step = files.len().div_ceil(WANT);
    files.into_iter().step_by(step).take(WANT).collect()
}

/// One symbolized file under column-check inspection.
struct SymbolFile<'a> {
    file: &'a str,
    source: &'a str,
    lang: Lang,
    symbols: &'a [SymbolView],
}

fn check_file_symbols(check: &mut ColumnCheck, table: &EntityTable, input: &SymbolFile<'_>) {
    for &idx in table.entities_in(input.file) {
        let entity = &table.entities[idx];
        if entity.key.name == "<module>" {
            continue;
        }
        let Some(pos) = position::locate_name(
            input.source,
            &entity.key.name,
            entity.key.line,
            entity.end_line,
            input.lang,
        ) else {
            check.failed_to_locate += 1;
            continue;
        };
        let expected = PosInfo::from(pos);
        let line0 = entity.key.line - 1;
        let symbol = input.symbols.iter().find(|sym| {
            symbol_name_matches(&sym.name, &entity.key.name)
                && range_contains_line(&sym.range, line0 as u32)
        });
        let Some(symbol) = symbol else {
            check.symbol_not_found += 1;
            if check.mismatches.len() < 50 {
                check.mismatches.push(ColumnMismatch {
                    file: input.file.to_owned(),
                    name: entity.key.name.clone(),
                    entity_line: entity.key.line,
                    reason: "symbol_not_found".to_owned(),
                    expected,
                    symbol: None,
                });
            }
            continue;
        };
        check.compared += 1;
        let actual = PosInfo {
            line0: symbol.selection_range.start.line,
            col_utf16: symbol.selection_range.start.character,
        };
        if actual == expected {
            check.matched += 1;
        } else if check.mismatches.len() < 50 {
            check.mismatches.push(ColumnMismatch {
                file: input.file.to_owned(),
                name: entity.key.name.clone(),
                entity_line: entity.key.line,
                reason: "differs".to_owned(),
                expected,
                symbol: Some(actual),
            });
        }
    }
}

fn range_contains_line(range: &Range, line0: u32) -> bool {
    range.start.line <= line0 && line0 <= range.end.line
}

/// Symbol names may carry signatures (jdtls: `foo()`), knot names never do.
fn symbol_name_matches(symbol_name: &str, entity_name: &str) -> bool {
    symbol_name == entity_name
        || symbol_name
            .split('(')
            .next()
            .is_some_and(|head| head.trim() == entity_name)
}

/// Flattened view of a `documentSymbol` entry.
struct SymbolView {
    name: String,
    range: Range,
    selection_range: Range,
}

fn flatten_symbols(response: &DocumentSymbolResponse) -> Vec<SymbolView> {
    match response {
        DocumentSymbolResponse::Flat(infos) => infos
            .iter()
            .map(|info| SymbolView {
                name: info.name.clone(),
                range: info.location.range,
                selection_range: info.location.range,
            })
            .collect(),
        DocumentSymbolResponse::Nested(symbols) => {
            let mut out = Vec::new();
            push_nested(&mut out, symbols);
            out
        }
    }
}

fn push_nested(out: &mut Vec<SymbolView>, symbols: &[DocumentSymbol]) {
    for symbol in symbols {
        out.push(SymbolView {
            name: symbol.name.clone(),
            range: symbol.range,
            selection_range: symbol.selection_range,
        });
        if let Some(children) = &symbol.children {
            push_nested(out, children);
        }
    }
}

/// Step 9: one result row per target, sequentially (T5.2).
async fn step_targets(
    ctx: &mut TargetContext<'_>,
    targets_file: &TargetsFile,
    max_targets: Option<usize>,
) -> Vec<TargetResult> {
    let mut out = Vec::new();
    let targets = &targets_file.targets;
    let targets: &[Target] = match max_targets {
        Some(max) => &targets[..targets.len().min(max)],
        None => targets.as_slice(),
    };
    for target in targets {
        let row = run_one_target(ctx, target).await;
        eprintln!(
            "# target {} status={} retried_empty={}",
            target.id, row.status, row.retried_empty
        );
        out.push(row);
    }
    out
}

async fn run_one_target(ctx: &mut TargetContext<'_>, target: &Target) -> TargetResult {
    let path = ctx.root.join(&target.file);
    let source = std::fs::read_to_string(&path).unwrap_or_default();
    let key = EntityKey {
        file: target.file.clone(),
        line: target.line,
        name: target.name.clone(),
    };
    let entity = ctx.table.get(&key);
    let position = entity.and_then(|entity| {
        position::locate_name(
            &source,
            &target.name,
            entity.key.line,
            entity.end_line,
            ctx.lang,
        )
    });

    let mut base = TargetResult {
        id: target.id.clone(),
        name: target.name.clone(),
        file: target.file.clone(),
        line: target.line,
        category: target.category.clone(),
        status: "ok".to_owned(),
        position: position.map(PosInfo::from),
        retried_empty: false,
        ops: TargetOps::default(),
        lsp_containers: ContainersByOp::default(),
        lsp_files: Vec::new(),
    };
    let Some(pos) = position else {
        base.status = "no_position".to_owned();
        return base;
    };

    if ctx.session.did_open(&path).is_err() {
        base.status = "no_position".to_owned();
        return base;
    }
    let Ok(uri) = path_to_uri(&path) else {
        base.status = "no_position".to_owned();
        return base;
    };
    let self_container = ctx.table.container_at(&target.file, target.line);
    let lsp_position = Position {
        line: pos.line0,
        character: pos.col_utf16,
    };

    let (mut ops, mut sets) = collect_ops(ctx, target, &uri, lsp_position, &self_container).await;
    if ops_are_empty(&ops) && ctx.gt_callers.get(&target.id).copied().unwrap_or(0) >= 1 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (ops2, sets2) = collect_ops(ctx, target, &uri, lsp_position, &self_container).await;
        base.retried_empty = true;
        ops = ops2;
        sets = sets2;
    }
    base.lsp_files = file_set(&sets.union);
    base.ops = ops;
    base.lsp_containers = sets;
    base
}

fn ops_are_empty(ops: &TargetOps) -> bool {
    let empty = |count: Option<&usize>| count.is_none_or(|count| *count == 0);
    empty(ops.incoming.as_ref().map(|op| &op.raw_count))
        && empty(ops.references.as_ref().map(|op| &op.raw_count))
        && empty(ops.implementation.as_ref().map(|op| &op.raw_count))
}

/// Run the requested ops for one target and normalise the results (T5.2 9).
async fn collect_ops(
    ctx: &mut TargetContext<'_>,
    target: &Target,
    uri: &Url,
    pos: Position,
    self_container: &EntityKey,
) -> (TargetOps, ContainersByOp) {
    let mut ops = TargetOps::default();
    let mut incoming_keys = Vec::new();
    let mut references_keys = Vec::new();
    let mut implementation_keys = Vec::new();

    if target.ops.iter().any(|op| op == "incoming") {
        let (op, keys) = collect_incoming(ctx, uri, pos, self_container).await;
        incoming_keys = keys;
        ops.incoming = Some(op);
    }
    if target.ops.iter().any(|op| op == "references") {
        let (op, keys) = collect_ref_op(ctx, OpKind::References, uri, pos, self_container).await;
        references_keys = keys;
        ops.references = Some(op);
    }
    if target.ops.iter().any(|op| op == "implementation") {
        let (op, keys) =
            collect_ref_op(ctx, OpKind::Implementation, uri, pos, self_container).await;
        implementation_keys = keys;
        ops.implementation = Some(op);
    }

    let union = union_sets(&[&incoming_keys, &references_keys, &implementation_keys]);
    let sets = ContainersByOp {
        incoming: incoming_keys,
        references: references_keys,
        implementation: implementation_keys,
        union,
    };
    (ops, sets)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    References,
    Implementation,
}

async fn collect_incoming(
    ctx: &mut TargetContext<'_>,
    uri: &Url,
    pos: Position,
    self_container: &EntityKey,
) -> (IncomingOp, Vec<EntityKey>) {
    if ctx
        .session
        .init_result
        .capabilities
        .call_hierarchy_provider
        .is_none()
    {
        return (empty_incoming(false), Vec::new());
    }

    let prepare = ctx.session.prepare_call_hierarchy(uri, pos).await;
    ctx.latencies.note_first(prepare.latency.as_millis() as f64);
    ctx.latencies
        .prepare
        .push(prepare.latency.as_millis() as f64);
    let prepare_ms = prepare.latency.as_millis() as f64;
    let mut retries = prepare.retries;

    let items = match prepare.result {
        Ok(Some(items)) => items,
        Ok(None) => {
            return (
                IncomingOp {
                    prepare_ms,
                    ..empty_incoming(true)
                },
                Vec::new(),
            );
        }
        Err(err) => {
            return (
                IncomingOp {
                    prepare_ms,
                    error: Some(err),
                    ..empty_incoming(true)
                },
                Vec::new(),
            );
        }
    };
    let Some(item) = pick_call_item(items, pos) else {
        return (
            IncomingOp {
                prepare_ms,
                ..empty_incoming(true)
            },
            Vec::new(),
        );
    };

    let calls = ctx.session.incoming_calls(item).await;
    let calls_ms = calls.latency.as_millis() as f64;
    retries += calls.retries;
    ctx.latencies.calls.push(calls_ms);

    let timing = IncomingTiming {
        prepare_ms,
        calls_ms,
        retries,
    };
    incoming_from_result(calls.result, ctx.root, ctx.table, self_container, timing)
}

/// Latency/retry accounting of the two call-hierarchy requests.
struct IncomingTiming {
    prepare_ms: f64,
    calls_ms: f64,
    retries: u32,
}

fn incoming_from_result(
    result: Result<Option<Vec<async_lsp::lsp_types::CallHierarchyIncomingCall>>, String>,
    root: &Path,
    table: &EntityTable,
    self_container: &EntityKey,
    timing: IncomingTiming,
) -> (IncomingOp, Vec<EntityKey>) {
    let (data, error) = match result {
        Ok(data) => (data, None),
        Err(err) => (None, Some(err)),
    };
    let Some(incoming) = data else {
        return (
            IncomingOp {
                prepare_ms: timing.prepare_ms,
                calls_ms: timing.calls_ms,
                retries: timing.retries,
                error,
                ..empty_incoming(true)
            },
            Vec::new(),
        );
    };
    let mut locations = Vec::new();
    let mut external_count = 0;
    for call in &incoming {
        match normalize_call_item(&call.from.uri, &call.from.selection_range, root, table) {
            Normalized::External => external_count += 1,
            Normalized::InRepo(loc) => locations.push(loc),
        }
    }
    let keys = container_set(&locations, self_container);
    let op = IncomingOp {
        supported: true,
        prepare_ms: timing.prepare_ms,
        calls_ms: timing.calls_ms,
        retries: timing.retries,
        error,
        raw_count: incoming.len(),
        external_count,
        self_count: self_reference_count(&locations, self_container),
        locations,
    };
    (op, keys)
}

fn empty_incoming(supported: bool) -> IncomingOp {
    IncomingOp {
        supported,
        prepare_ms: 0.0,
        calls_ms: 0.0,
        retries: 0,
        error: None,
        raw_count: 0,
        external_count: 0,
        self_count: 0,
        locations: Vec::new(),
    }
}

fn pick_call_item(
    items: Vec<async_lsp::lsp_types::CallHierarchyItem>,
    pos: Position,
) -> Option<async_lsp::lsp_types::CallHierarchyItem> {
    let contains_pos = |item: &&async_lsp::lsp_types::CallHierarchyItem| {
        item.selection_range.start.line <= pos.line
            && pos.line <= item.selection_range.end.line
            && item.selection_range.start.character <= pos.character
            && pos.character <= item.selection_range.end.character
    };
    items
        .iter()
        .find(contains_pos)
        .cloned()
        .or_else(|| items.into_iter().next())
}

/// `textDocument/references` or `textDocument/implementation` (T5.2 9.4/9.5).
async fn collect_ref_op(
    ctx: &mut TargetContext<'_>,
    kind: OpKind,
    uri: &Url,
    pos: Position,
    self_container: &EntityKey,
) -> (RefOp, Vec<EntityKey>) {
    let supported = match kind {
        OpKind::References => ctx
            .session
            .init_result
            .capabilities
            .references_provider
            .is_some(),
        OpKind::Implementation => ctx
            .session
            .init_result
            .capabilities
            .implementation_provider
            .is_some(),
    };
    if !supported {
        return (empty_ref(false), Vec::new());
    }
    // `references` answers `Option<Vec<Location>>`, `implementation` is already
    // normalised to `Vec<Location>`; unify so both ops share one code path.
    let outcome: RequestOutcome<Vec<Location>> = match kind {
        OpKind::References => {
            let raw = ctx.session.references(uri, pos, false).await;
            RequestOutcome {
                result: raw.result.map(|locations| locations.unwrap_or_default()),
                latency: raw.latency,
                total: raw.total,
                retries: raw.retries,
            }
        }
        OpKind::Implementation => ctx.session.implementation(uri, pos).await,
    };
    ctx.latencies.note_first(outcome.latency.as_millis() as f64);
    let samples = match kind {
        OpKind::References => &mut ctx.latencies.references,
        OpKind::Implementation => &mut ctx.latencies.implementation,
    };
    samples.push(outcome.latency.as_millis() as f64);

    match outcome.result {
        Ok(locations) => {
            let raw_count = locations.len();
            let mut in_repo = Vec::new();
            let mut external_count = 0;
            for location in &locations {
                match normalize_location(&location.uri, &location.range, ctx.root, ctx.table) {
                    Normalized::External => external_count += 1,
                    Normalized::InRepo(loc) => in_repo.push(loc),
                }
            }
            let keys = container_set(&in_repo, self_container);
            let op = RefOp {
                supported: true,
                ms: outcome.latency.as_millis() as f64,
                retries: outcome.retries,
                error: None,
                raw_count,
                external_count,
                self_count: self_reference_count(&in_repo, self_container),
                locations: in_repo,
            };
            (op, keys)
        }
        Err(err) => (
            RefOp {
                supported: true,
                ms: outcome.latency.as_millis() as f64,
                retries: outcome.retries,
                error: Some(err),
                ..empty_ref(true)
            },
            Vec::new(),
        ),
    }
}

fn empty_ref(supported: bool) -> RefOp {
    RefOp {
        supported,
        ms: 0.0,
        retries: 0,
        error: None,
        raw_count: 0,
        external_count: 0,
        self_count: 0,
        locations: Vec::new(),
    }
}

/// Capability summary from the initialize result (T5.3).
///
/// `type_hierarchy` is always `false`: lsp-types 0.95 (the version async-lsp
/// pins) has no `type_hierarchy_provider` server capability field, so the flag
/// cannot be read. S1 does not measure type hierarchy anyway.
fn capability_flags(caps: &async_lsp::lsp_types::ServerCapabilities) -> CapabilitiesInfo {
    CapabilitiesInfo {
        call_hierarchy: caps.call_hierarchy_provider.is_some(),
        type_hierarchy: false,
        implementation: caps.implementation_provider.is_some(),
        references: caps.references_provider.is_some(),
        document_symbol: caps.document_symbol_provider.is_some(),
        workspace_symbol: caps.workspace_symbol_provider.is_some(),
        raw: serde_json::to_value(caps).unwrap_or(serde_json::Value::Null),
    }
}

fn server_version(session: &LspSession) -> String {
    session
        .init_result
        .server_info
        .as_ref()
        .map(|info| match &info.version {
            Some(version) => format!("{} {version}", info.name),
            None => info.name.clone(),
        })
        .unwrap_or_default()
}

fn rss_info(report: &RssReport) -> RssInfo {
    let mark_bytes = |label: &str| {
        report
            .marks
            .iter()
            .find(|(name, _, _)| name == label)
            .map_or(0, |(_, _, bytes)| *bytes)
    };
    RssInfo {
        peak_bytes: report.peak_bytes,
        at_ready_bytes: mark_bytes("ready"),
        end_bytes: mark_bytes("end"),
        peak_process_count: report.peak_process_count,
        timeline: report.timeline.clone(),
    }
}

/// Per-repo snapshot fingerprint recorded in T0.3.
fn read_fingerprint(spike_root: &Path, repo: &str) -> String {
    let path = spike_root.join("work").join("FINGERPRINTS.txt");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    text.lines()
        .find_map(|line| {
            let (name, hash) = line.split_once(' ')?;
            (name == repo).then(|| hash.trim().to_owned())
        })
        .unwrap_or_default()
}

/// `results/s1_<lang>_nolsp.json` — degradation-path result (T8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoLspResult {
    pub schema: String,
    pub lang: Lang,
    pub repo: String,
    /// `missing` (binary absent) or `broken` (spawns but dies before initialize).
    pub scenario: String,
    pub lsp_available: bool,
    pub lsp_started: bool,
    pub diagnostic: String,
    pub effective_tier: u8,
    /// Setup cost of the tier-2 graph (T6 resolver).
    pub graph_build_ms: u64,
    /// Start attempt → degradation decision.
    pub degrade_ms: u64,
    /// Degradation decision → tier-2 answers (asserted ≤ 5 s).
    pub fallback_ms: u64,
    pub targets: Vec<NoLspTarget>,
}

/// A tier-2 fallback answer (T8 step 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoLspTarget {
    pub id: String,
    pub provenance: String,
    pub callers: Vec<EntityKey>,
    pub by_rel: BTreeMap<String, usize>,
}

/// Inputs of one `lsp_probe nolsp` run (T8 CLI).
pub struct NoLspConfig {
    pub targets: PathBuf,
    pub out: PathBuf,
    pub spike_root: PathBuf,
    /// Hidden `--program` override for the "present but broken" scenario.
    pub program: Option<PathBuf>,
}

/// T8: run the production start path against a sanitized `PATH` and fall back
/// to the tier-2 heuristic graph, asserting the documented behaviour.
pub async fn run_nolsp(cfg: &NoLspConfig) -> Result<()> {
    let targets_file = TargetsFile::load(&cfg.targets)?;
    let root = PathBuf::from(&targets_file.root);
    let (table, parsed) = EntityTable::build(&targets_file.repo, &root)?;

    let graph_started = Instant::now();
    let graph = crate::graph::heuristic::build(&parsed);
    let graph_build_ms = graph_started.elapsed().as_millis() as u64;

    let mut spec = servers::spec_for(targets_file.lang, &root, &cfg.spike_root);
    if let Some(program) = &cfg.program {
        spec.program = program.to_string_lossy().into_owned();
    }
    spec.search_path = Some(servers::sanitized_path_without(&spec.program));

    let degrade_started = Instant::now();
    let start = client::start_or_degrade(&spec, &root, &cfg.spike_root.join("logs")).await?;
    let degrade_ms = degrade_started.elapsed().as_millis() as u64;

    let (lsp_available, lsp_started, diagnostic) = match &start {
        client::ServerStart::Ready(_) => {
            anyhow::bail!(
                "expected degradation but {} started and initialized",
                spec.program
            )
        }
        client::ServerStart::Degraded(degraded) => (
            degraded.lsp_available,
            degraded.lsp_started,
            degraded.diagnostic.clone(),
        ),
    };

    let fallback_started = Instant::now();
    let rows = crate::graph::heuristic::measure_targets(&graph, &table, &targets_file.targets);
    let fallback_ms = fallback_started.elapsed().as_millis() as u64;

    let scenario = if cfg.program.is_some() {
        "broken"
    } else {
        "missing"
    };
    check_degradation(
        scenario,
        lsp_available,
        lsp_started,
        &diagnostic,
        fallback_ms,
    )?;

    let result = NoLspResult {
        schema: S1_SCHEMA.to_owned(),
        lang: targets_file.lang,
        repo: targets_file.repo.clone(),
        scenario: scenario.to_owned(),
        lsp_available,
        lsp_started,
        diagnostic,
        effective_tier: 2,
        graph_build_ms,
        degrade_ms,
        fallback_ms,
        targets: rows
            .into_iter()
            .map(|row| NoLspTarget {
                id: row.id,
                provenance: "heuristic".to_owned(),
                callers: row.callers,
                by_rel: row.by_rel,
            })
            .collect(),
    };
    if result.targets.len() != targets_file.targets.len() {
        anyhow::bail!(
            "fallback answers missing: {} rows for {} targets",
            result.targets.len(),
            targets_file.targets.len()
        );
    }
    common::write_json_pretty(&cfg.out, &result)?;
    println!(
        "wrote {} (scenario={}, available={}, started={}, degrade_ms={}, fallback_ms={})",
        cfg.out.display(),
        result.scenario,
        result.lsp_available,
        result.lsp_started,
        result.degrade_ms,
        result.fallback_ms
    );
    Ok(())
}

/// Assert the T8 expected behaviour for both scenarios.
fn check_degradation(
    scenario: &str,
    lsp_available: bool,
    lsp_started: bool,
    diagnostic: &str,
    fallback_ms: u64,
) -> Result<()> {
    if diagnostic.trim().is_empty() {
        anyhow::bail!("diagnostic must be human-readable, got an empty string");
    }
    if fallback_ms > 5_000 {
        anyhow::bail!("tier-2 fallback took {fallback_ms} ms (> 5000 ms)");
    }
    match scenario {
        "missing" => {
            if lsp_available {
                anyhow::bail!("missing scenario must report lsp_available=false");
            }
            if !diagnostic.contains("not found in PATH") {
                anyhow::bail!("missing diagnostic lacks the PATH explanation: {diagnostic}");
            }
        }
        "broken" => {
            if !lsp_available {
                anyhow::bail!("broken scenario must report lsp_available=true");
            }
            if lsp_started {
                anyhow::bail!("broken scenario must report lsp_started=false");
            }
            if !diagnostic.contains("before initialize") {
                anyhow::bail!("broken diagnostic lacks the exit explanation: {diagnostic}");
            }
        }
        other => anyhow::bail!("unknown scenario {other}"),
    }
    Ok(())
}
