//! Gate J1: Java edit synchronization across persistent workspaces
//! (P6-D10, P6-D11, P6 plan P6.8 steps 1–3) and gate P6-D12 (Rust/TS edits).
//!
//! J1 has three scenarios, each measured in the four modes of P6-D11
//! (`off`, `open`, `watched`, `open+watched`):
//!
//! - **S1** (`j1_s1.json`): the Maven fixture, one fresh pool per session
//!   after an edit plus a re-index (P6.8 step 1, the CLI case).
//! - **S2** (`j1_s2.json`): the Maven fixture, one long pool, no re-index
//!   (P6.8 step 3, the MCP case).
//! - **S3** (`j1_s3.json`): the `spring-ai` work copy, no re-index, with the
//!   new caller in another module (P6.8 step 2).
//!
//! `j1_verdict` reads the three reports and computes the winner from the
//! numbers; nothing about the outcome is hard-coded (decision R-D4).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::edits::{EditGuard, copy_dir_all, ensure_work_copy};
use super::lsp::{measurement_settings, pool_for_root_with, timed_callers};
use super::{read_gate, sandbox_path, work_copy_path, write_gate};
use crate::index::layout::IndexLayout;
use crate::index::load::{LoadedIndex, LoadedRecord, load_index};
use crate::lsp::sync::SyncMode;
use crate::query::callers::CallersResponse;
use crate::query::edits::EditedFiles;
use crate::query::sources::heuristic_answer;

/// The modes of P6-D11; all four are measured in every scenario.
const MODES: [SyncMode; 4] = [
    SyncMode::Off,
    SyncMode::Open,
    SyncMode::Watched,
    SyncMode::OpenWatched,
];

/// The edited file of the Maven fixture.
const ORDER_JAVA: &str = "src/main/java/com/example/shop/model/Order.java";

/// What one session produced. `first_answer_ms` is the first answer obtained
/// after this session's edit: in S1 that includes the language server's cold
/// start on a fresh pool, in S2 and S3 the pool is already warm.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct J1SessionResult {
    session: usize,
    complete: bool,
    provenance: String,
    first_answer_ms: u64,
    edit_detected: bool,
}

/// Results of one mode in one scenario.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct J1ModeResult {
    mode: String,
    completed: usize,
    total: usize,
    avg_ms: u64,
    sessions: Vec<J1SessionResult>,
}

/// The report of one scenario: written by its own test, read by `j1_verdict`.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct J1ScenarioReport {
    gate: String,
    scenario: String,
    modes: Vec<J1ModeResult>,
}

/// The computed verdict of the gate.
#[derive(Debug, Serialize, Deserialize)]
struct J1VerdictReport {
    gate: String,
    winner: String,
    note: String,
    /// The three scenario reports this verdict is computed from.
    scenarios: Vec<J1ScenarioReport>,
    /// Sum of the per-scenario `avg_ms` of each mode.
    cost: Vec<(String, u64)>,
    /// Whether every scenario is complete for each mode.
    passes: Vec<(String, bool)>,
    /// Whether each mode stays within `off avg_ms + SYNC_WAIT` in every scenario.
    bound_ok: Vec<(String, bool)>,
    /// Historical 0.5.0 figure (P6 plan Appendix D), context only.
    baseline_0_5_0_first_answer_ms: u64,
}

/// Whether the answer is live and lists the new caller, plus its provenance.
fn answer_completeness(resp: &CallersResponse, caller: &str) -> (bool, String) {
    let Some(target) = resp.targets.first() else {
        return (false, "no_target".to_owned());
    };
    let has_caller = callers_contain(resp, caller);
    let complete = target.provenance == "lsp-live" && has_caller;
    (complete, target.provenance.to_owned())
}

/// Whether any caller row of the first target is named `caller`.
fn callers_contain(resp: &CallersResponse, caller: &str) -> bool {
    resp.targets.first().is_some_and(|target| {
        target
            .callers
            .0
            .iter()
            .flat_map(|(_, rows)| rows)
            .any(|row| row.name == caller)
    })
}

/// The printable label of a session outcome.
fn outcome(complete: bool) -> &'static str {
    if complete { "COMPLETE" } else { "STALE" }
}

/// Turn the per-session answers into the mode summary.
fn summarise(mode: SyncMode, sessions: Vec<J1SessionResult>) -> J1ModeResult {
    let completed = sessions.iter().filter(|row| row.complete).count();
    let total = sessions.len();
    let avg_ms = if total == 0 {
        0
    } else {
        sessions.iter().map(|row| row.first_answer_ms).sum::<u64>() / total as u64
    };
    J1ModeResult {
        mode: mode.label().to_owned(),
        completed,
        total,
        avg_ms,
        sessions,
    }
}

/// Run every mode of one scenario and write its report.
fn run_scenario(name: &str, run: impl Fn(SyncMode) -> J1ModeResult) -> J1ScenarioReport {
    let mut modes = Vec::with_capacity(MODES.len());
    for mode in MODES {
        println!("--- {name}: mode {:?} ---", mode);
        modes.push(run(mode));
    }
    let report = J1ScenarioReport {
        gate: "J1".to_owned(),
        scenario: name.to_owned(),
        modes,
    };
    write_gate(
        &format!("j1_{}", name.split('_').next().unwrap_or(name)),
        &report,
    );
    report
}

fn fixture_java_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/fixtures/java")
}

fn setup_maven_fixture(dest: &Path) {
    let src = fixture_java_path();
    let _ = std::fs::remove_dir_all(dest);
    copy_dir_all(&src, dest).expect("copy maven fixture");
}

fn run_warm_index(dir: &Path) {
    let mut cfg = crate::config::test_support::config_for(dir.to_path_buf());
    cfg.lsp = measurement_settings();
    let params = crate::models::params::IndexParams {
        force: false,
        commit_mode: None,
        embed_model: None,
        jobs: None,
        batch_size: 128,
        quiet: true,
        lsp: crate::lsp::precompute::PrecomputeOptions {
            policy: crate::lsp::precompute::PrecomputePolicy::Auto,
            selection: crate::lsp::precompute::Selection::Heuristic,
            min_fanin: 20,
            max_targets: 200,
            budget: std::time::Duration::from_secs(60),
            probe_min_fanin: 3,
        },
    };
    let _ = crate::cli_tools::index::run_index(&cfg, &params).expect("index");
}

fn edit_order_add_caller(dir: &Path, caller_name: &str) {
    let order = dir.join(ORDER_JAVA);
    let content = std::fs::read_to_string(&order).expect("read order");
    if let Some(pos) = content.rfind('}') {
        let (head, tail) = content.split_at(pos);
        let addition = format!(
            "\n    public int {caller_name}() {{\n        return getItems().size();\n    }}\n"
        );
        let full = format!("{head}{addition}{tail}");
        std::fs::write(&order, full).expect("write order");
    }
}

// ---------------------------------------------------------------------------
// S1: the Maven fixture, fresh pool per session, after an edit plus a re-index
// ---------------------------------------------------------------------------

/// One S1 session (P6.8 step 1).
fn run_s1_session(mode: SyncMode, session: usize) -> J1SessionResult {
    let base_dir = work_copy_path("j1_s1");
    setup_maven_fixture(&base_dir);
    run_warm_index(&base_dir);
    let before = load_index(&IndexLayout::for_repo(&base_dir)).expect("index before the edit");

    let mut warm = measurement_settings();
    warm.sync = mode;
    let (widx, ws, _root, wpool) = pool_for_root_with(&base_dir, warm);
    let _ = timed_callers(&widx, &ws, &base_dir, "Order.getItems", Some(&wpool));
    drop(wpool);

    edit_order_add_caller(&base_dir, "size");
    let edit_detected = EditedFiles::new(&before, &base_dir).is_changed(ORDER_JAVA);
    assert!(
        edit_detected,
        "S1 session {session}: the edit must be visible before the re-index"
    );

    run_warm_index(&base_dir);
    let mut settings = measurement_settings();
    settings.sync = mode;
    let (index, settings, _root, pool) = pool_for_root_with(&base_dir, settings);
    let (resp, ms) = timed_callers(&index, &settings, &base_dir, "Order.getItems", Some(&pool));
    drop(pool);

    let (complete, provenance) = answer_completeness(&resp, "size");
    J1SessionResult {
        session,
        complete,
        provenance,
        first_answer_ms: ms,
        edit_detected,
    }
}

/// Scenario S1: `sessions` fresh sessions per mode.
fn run_s1_mode(mode: SyncMode, sessions: usize) -> J1ModeResult {
    let rows = (1..=sessions)
        .map(|i| {
            let row = run_s1_session(mode, i);
            println!(
                "  S1 {:?} session {i}/{sessions}: {} in {} ms",
                mode,
                outcome(row.complete),
                row.first_answer_ms
            );
            row
        })
        .collect();
    summarise(mode, rows)
}

#[test]
#[ignore = "J1 S1: Maven fixture, fresh session per edit (KNOT_LITE_MEASURE_WORK, KNOT_LITE_MEASURE_OUT)"]
fn j1_s1_maven_cli() {
    run_scenario("s1_maven_cli", |mode| run_s1_mode(mode, 16));
}

// ---------------------------------------------------------------------------
// S2: the Maven fixture, one long pool, no re-index
// ---------------------------------------------------------------------------

/// Scenario S2 (P6.8 step 3): `iterations` edits in one session per mode.
fn run_s2_mode(mode: SyncMode, iterations: usize) -> J1ModeResult {
    let base_dir = work_copy_path("j1_s2");
    setup_maven_fixture(&base_dir);
    run_warm_index(&base_dir);

    let mut settings = measurement_settings();
    settings.sync = mode;
    let (index, settings, _root, pool) = pool_for_root_with(&base_dir, settings);
    let _ = timed_callers(&index, &settings, &base_dir, "Order.getItems", Some(&pool));

    let mut rows = Vec::with_capacity(iterations);
    for i in 1..=iterations {
        let caller = format!("longCaller{i}");
        edit_order_add_caller(&base_dir, &caller);
        let edit_detected = EditedFiles::new(&index, &base_dir).is_changed(ORDER_JAVA);
        assert!(
            edit_detected,
            "S2 iter {i}: the edit must be visible to changed-set detection"
        );

        let (resp, ms) = timed_callers(&index, &settings, &base_dir, "Order.getItems", Some(&pool));
        let (complete, provenance) = answer_completeness(&resp, &caller);
        println!(
            "  S2 {:?} iter {i}/{iterations}: {} in {ms} ms",
            mode,
            outcome(complete)
        );
        rows.push(J1SessionResult {
            session: i,
            complete,
            provenance,
            first_answer_ms: ms,
            edit_detected,
        });
    }
    drop(pool);
    summarise(mode, rows)
}

#[test]
#[ignore = "J1 S2: Maven fixture, one long session (KNOT_LITE_MEASURE_WORK, KNOT_LITE_MEASURE_OUT)"]
fn j1_s2_maven_mcp() {
    run_scenario("s2_maven_mcp", |mode| run_s2_mode(mode, 8));
}

// ---------------------------------------------------------------------------
// S3: the spring-ai work copy, the new caller in another module, no re-index
// ---------------------------------------------------------------------------

/// The pre-selected S3 pair (P6.8 step 2): a public no-argument method of
/// `spring-ai-model` and a class of `models/spring-ai-openai` that calls it.
#[derive(Debug, Clone)]
struct S3Setup {
    /// FQN of the method, as `find_callers` wants it.
    target_fqn: String,
    /// Method name.
    method: String,
    /// FQN of the declaring class, spelled as a Java type.
    class_fqn: String,
    /// Repo-relative path of the caller file.
    caller_file: String,
}

/// The module of the S3 target.
const S3_MODEL_PREFIX: &str = "spring-ai-model/src/main/java/";
/// The module of the S3 caller; its pom depends on `spring-ai-model`.
const S3_CALLER_PREFIX: &str = "models/spring-ai-openai/src/main/java/";

/// Whether the declaration of `record` is a public instance method.
fn is_public_instance_method(text: &str, record: &LoadedRecord) -> bool {
    let start = record.line.l0.saturating_sub(1);
    let end = record.line.l1.min(start + 6);
    let decl = text
        .lines()
        .skip(start)
        .take(end.saturating_sub(start))
        .find(|line| line.contains(&format!("{}(", record.line.name)))
        .unwrap_or("");
    let head = decl.trim_start();
    (head.starts_with("public ") || head.contains(" public "))
        && !head.contains("static ")
        && !head.contains("abstract ")
}

/// Whether the line looks like a declaration rather than a comment.
fn decl_line(line: &str) -> bool {
    let head = line.trim_start();
    !head.is_empty() && !head.starts_with("//") && !head.starts_with('*') && !head.starts_with("/*")
}

/// Whether the file is one plain class: a class declaration and no interface,
/// record or nested class on any declaration line.
fn is_plain_class_file(text: &str) -> bool {
    let mut class_decls = 0;
    for line in text.lines() {
        if !decl_line(line) {
            continue;
        }
        if line.contains("interface ") || line.contains("record ") {
            return false;
        }
        if line.contains("class ") {
            if line.contains("abstract class") || line.contains("static class") {
                return false;
            }
            class_decls += 1;
        }
    }
    class_decls == 1
}

/// Choose the S3 target and caller file deterministically (lowest record id).
fn select_s3_setup(index: &LoadedIndex, root: &Path) -> S3Setup {
    let pom = std::fs::read_to_string(root.join("models/spring-ai-openai/pom.xml"))
        .expect("spring-ai-openai pom");
    assert!(
        pom.contains("spring-ai-model"),
        "models/spring-ai-openai must depend on spring-ai-model"
    );

    let target = index
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.file.starts_with(S3_MODEL_PREFIX)
                && record.line.kind == "method"
                && record.line.col.is_some()
                && record.line.sig.as_deref() == Some("()")
                && !record.line.tc
        })
        .filter(|(pos, record)| {
            std::fs::read_to_string(root.join(&record.file)).is_ok_and(|text| {
                is_plain_class_file(&text) && is_public_instance_method(&text, record)
            }) && !heuristic_answer(index, *pos).containers.is_empty()
        })
        .min_by_key(|(_, record)| record.line.id.clone())
        .map(|(_, record)| record)
        .expect("an S3 target: a public no-arg method of spring-ai-model");

    let mut callers: Vec<String> = index
        .file_sha16
        .keys()
        .filter(|rel| rel.starts_with(S3_CALLER_PREFIX) && rel.ends_with(".java"))
        .cloned()
        .collect();
    callers.sort();
    let caller_file = callers
        .into_iter()
        .find(|rel| {
            std::fs::read_to_string(root.join(rel))
                .is_ok_and(|text| is_plain_class_file(&text) && text.rfind('}').is_some())
        })
        .expect("an S3 caller class of spring-ai-openai");

    let method = target.line.name.clone();
    let class_fqn = target
        .line
        .fqn
        .strip_suffix(&format!(".{method}"))
        .unwrap_or(&target.line.fqn)
        .to_owned();
    S3Setup {
        target_fqn: target.line.fqn.clone(),
        method,
        class_fqn,
        caller_file,
    }
}

/// Add the S3 caller of `setup` to its file (before the final `}`).
fn edit_s3_add_caller(root: &Path, setup: &S3Setup, caller: &str) {
    let path = root.join(&setup.caller_file);
    let content = std::fs::read_to_string(&path).expect("read S3 caller file");
    let pos = content.rfind('}').expect("a closing brace");
    let (head, tail) = content.split_at(pos);
    let addition = format!(
        "\n    public void {caller}({} t) {{ t.{}(); }}\n",
        setup.class_fqn, setup.method
    );
    std::fs::write(&path, format!("{head}{addition}{tail}")).expect("write S3 caller file");
}

/// One S3 session: a fresh pool on the work copy, one edit, no re-index.
fn run_s3_session(setup: &S3Setup, mode: SyncMode, session: usize) -> J1SessionResult {
    let root = work_copy_path("spring-ai");
    let caller = format!("klJ1Caller{session}");
    let guard = EditGuard::new(root.join(&setup.caller_file));
    edit_s3_add_caller(&root, setup, &caller);

    let mut settings = measurement_settings();
    settings.sync = mode;
    let (index, settings, _root, pool) = pool_for_root_with(&root, settings);
    let edit_detected = EditedFiles::new(&index, &root).is_changed(&setup.caller_file);
    assert!(
        edit_detected,
        "S3 session {session}: the edit must be visible to changed-set detection"
    );

    let (resp, ms) = timed_callers(&index, &settings, &root, &setup.target_fqn, Some(&pool));
    drop(pool);
    drop(guard);

    let (complete, provenance) = answer_completeness(&resp, &caller);
    J1SessionResult {
        session,
        complete,
        provenance,
        first_answer_ms: ms,
        edit_detected,
    }
}

/// Scenario S3 (P6.8 step 2): `sessions` fresh sessions per mode.
fn run_s3_mode(setup: &S3Setup, mode: SyncMode, sessions: usize) -> J1ModeResult {
    let rows = (1..=sessions)
        .map(|i| {
            let row = run_s3_session(setup, mode, i);
            println!(
                "  S3 {:?} session {i}/{sessions}: {} in {} ms",
                mode,
                outcome(row.complete),
                row.first_answer_ms
            );
            row
        })
        .collect();
    summarise(mode, rows)
}

#[test]
#[ignore = "J1 S3: spring-ai work copy, the caller in another module (KNOT_LITE_MEASURE_WORK, KNOT_LITE_MEASURE_OUT)"]
fn j1_s3_spring_ai() {
    let root = work_copy_path("spring-ai");
    let _ = ensure_spring_ai_work_copy(&root);
    let index = load_index(&IndexLayout::for_repo(&root)).expect("spring-ai index");
    let setup = select_s3_setup(&index, &root);
    println!(
        "S3 target: {} in {} (class {}), caller {}",
        setup.method, setup.target_fqn, setup.class_fqn, setup.caller_file
    );
    run_scenario("s3_spring_ai", |mode| run_s3_mode(&setup, mode, 8));
}

/// Make sure the `spring-ai` work copy exists and carries its index.
fn ensure_spring_ai_work_copy(root: &Path) -> PathBuf {
    let src = sandbox_path("spring-ai");
    if !root.join(".knot-lite/manifest.toml").exists() {
        ensure_work_copy("spring-ai");
    }
    assert!(
        root.join(".knot-lite/manifest.toml").exists(),
        "the spring-ai work copy must carry its index ({} -> {})",
        src.display(),
        root.display()
    );
    root.to_path_buf()
}

// ---------------------------------------------------------------------------
// Verdict: computed from the three reports, never hard-coded (R-D4)
// ---------------------------------------------------------------------------

/// The 0.5.0 first-answer figure (P6 plan Appendix D), context only.
const BASELINE_0_5_0_FIRST_ANSWER_MS: u64 = 9_700;

/// Every scenario a mode must complete to pass J1 (P6 §11).
const REQUIRED: [(&str, usize); 3] = [
    ("s1_maven_cli", 16),
    ("s2_maven_mcp", 8),
    ("s3_spring_ai", 8),
];

/// Compute the verdict from the three scenario reports.
fn compute_verdict(reports: &[J1ScenarioReport]) -> J1VerdictReport {
    let off_avg: Vec<u64> = reports
        .iter()
        .map(|report| {
            report
                .modes
                .iter()
                .find(|m| m.mode == "off")
                .map_or(0, |m| m.avg_ms)
        })
        .collect();

    let mut cost = Vec::new();
    let mut passes = Vec::new();
    let mut bound_ok = Vec::new();
    for mode in MODES {
        let label = mode.label().to_owned();
        let mut total = 0;
        let mut complete_all = true;
        let mut within_bound = true;
        for (idx, report) in reports.iter().enumerate() {
            let Some(row) = report.modes.iter().find(|m| m.mode == label) else {
                complete_all = false;
                within_bound = false;
                continue;
            };
            total += row.avg_ms;
            let need = REQUIRED[idx].1;
            if row.completed < need || row.total != need {
                complete_all = false;
            }
            if row.avg_ms > off_avg[idx] + 10_000 {
                within_bound = false;
            }
        }
        cost.push((label.clone(), total));
        passes.push((label.clone(), complete_all));
        bound_ok.push((label.clone(), within_bound));
    }

    let watched_cost = cost
        .iter()
        .find(|(name, _)| name == "watched")
        .map_or(0, |(_, ms)| *ms);
    let qualifies = |name: &str| {
        passes.iter().any(|(p_name, ok)| p_name == name && *ok)
            && bound_ok.iter().any(|(b_name, ok)| b_name == name && *ok)
    };
    // The noise rule may only keep `watched` when `watched` itself qualifies.
    let watched_ok = qualifies("watched");
    let best = cost
        .iter()
        .filter(|(name, _)| qualifies(name))
        .min_by_key(|(_, ms)| *ms)
        .map(|(name, _)| name.clone());

    let (winner, note) = match best {
        Some(name) if name == "watched" => (
            name,
            "watched is the cheapest mode that completes every scenario and stays within the bound"
                .to_owned(),
        ),
        Some(name)
            if watched_ok
                && watched_cost > 0
                && name_cost(&cost, &name) * 105 >= watched_cost * 100 =>
        {
            (
                "watched".to_owned(),
                format!(
                    "{name} is within 5% of watched and both pass; watched stays the default (noise rule, decision D2)"
                ),
            )
        }
        Some(name) => (
            name.clone(),
            format!("{name} is the cheapest mode that completes every scenario within the bound"),
        ),
        None => (
            "none".to_owned(),
            "no mode completed every scenario within the bound (decision R-D4: stop and report)"
                .to_owned(),
        ),
    };

    J1VerdictReport {
        gate: "J1".to_owned(),
        winner,
        note,
        scenarios: reports.to_vec(),
        cost,
        passes,
        bound_ok,
        baseline_0_5_0_first_answer_ms: BASELINE_0_5_0_FIRST_ANSWER_MS,
    }
}

/// The cost of one mode, as recorded by [`compute_verdict`].
fn name_cost(cost: &[(String, u64)], name: &str) -> u64 {
    cost.iter()
        .find(|(n, _)| n == name)
        .map_or(0, |(_, ms)| *ms)
}

#[test]
#[ignore = "J1 verdict: read j1_s1/j1_s2/j1_s3 and compute the winner (KNOT_LITE_MEASURE_OUT)"]
fn j1_verdict() {
    let reports: Vec<J1ScenarioReport> = REQUIRED
        .iter()
        .map(|(name, _)| read_gate(&format!("j1_{}", name.split('_').next().unwrap_or(name))))
        .collect();
    for (report, (name, need)) in reports.iter().zip(REQUIRED) {
        for mode in &report.modes {
            println!(
                "  {} {:>13}: {}/{} complete (need {need}), avg {} ms",
                name, mode.mode, mode.completed, mode.total, mode.avg_ms
            );
        }
    }
    let verdict = compute_verdict(&reports);
    println!("J1 winner: {} ({})", verdict.winner, verdict.note);
    write_gate("j1_sync", &verdict);
}

// ---------------------------------------------------------------------------
// P6-D12: Rust and TypeScript edits in a long session
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
struct RustTsEditResult {
    lang: String,
    /// `fixture` (the E2E fixture) or `work_copy` (the `knot` / `knot-site` work copies).
    subject: String,
    iterations_completed: usize,
    iterations_total: usize,
    passed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct RustTsReport {
    gate: String,
    results: Vec<RustTsEditResult>,
}

/// One P6-D12 case: the repository, the query and the new caller it adds.
#[derive(Debug, Clone)]
struct D12Case {
    /// Language label (`rust`, `typescript`).
    lang: &'static str,
    /// `fixture` (the E2E fixture) or `work_copy` (P61.14).
    subject: &'static str,
    /// Directory name under `KNOT_LITE_MEASURE_WORK`.
    work_name: &'static str,
    /// Fixture directory, for `subject == "fixture"`.
    fixture: Option<&'static str>,
    /// Sandbox repository, for `subject == "work_copy"`.
    repo: &'static str,
    query: String,
    /// Repo-relative file the new caller is appended to.
    caller_file: String,
    /// New caller name, with `{i}` for the iteration.
    caller_name: String,
    /// New caller text, with `{i}` for the iteration.
    caller_text: String,
    /// Line inserted at the top of the caller file before the first edit.
    import_line: Option<String>,
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/e2e/fixtures")
        .join(name)
}

/// The repository of one P6-D12 case: a fresh fixture or an existing work copy.
fn d12_setup(case: &D12Case) -> PathBuf {
    let root = work_copy_path(case.work_name);
    match case.subject {
        "fixture" => {
            let src = fixture_path(case.fixture.expect("a fixture name"));
            let _ = std::fs::remove_dir_all(&root);
            copy_dir_all(&src, &root).expect("copy fixture");
            if case.lang == "typescript" {
                let node_modules_src = sandbox_path("knot-site").join("node_modules");
                #[cfg(unix)]
                if node_modules_src.exists() {
                    let _ =
                        std::os::unix::fs::symlink(&node_modules_src, root.join("node_modules"));
                }
            }
            run_warm_index(&root);
        }
        _ => {
            ensure_work_copy(case.repo);
        }
    }
    root
}

/// Run one P6-D12 case: `iterations` edit + query rounds in one long session.
fn run_d12_case(case: &D12Case, iterations: usize) -> RustTsEditResult {
    let root = d12_setup(case);
    let mut settings = measurement_settings();
    settings.sync = SyncMode::Off;
    let (index, settings, _root, pool) = pool_for_root_with(&root, settings);
    let _ = timed_callers(&index, &settings, &root, &case.query, Some(&pool));

    let abs = root.join(&case.caller_file);
    let mut completed = 0;
    for i in 1..=iterations {
        let mut content = std::fs::read(&abs).expect("read caller file");
        if i == 1
            && let Some(import) = &case.import_line
            && !String::from_utf8_lossy(&content).contains(import.trim())
        {
            let mut with_import = import.as_bytes().to_vec();
            with_import.push(b'\n');
            with_import.extend_from_slice(&content);
            content = with_import;
        }
        let caller = case.caller_name.replace("{i}", &i.to_string());
        content.extend_from_slice(case.caller_text.replace("{i}", &i.to_string()).as_bytes());
        std::fs::write(&abs, &content).expect("write caller file");

        let edit_detected = EditedFiles::new(&index, &root).is_changed(&case.caller_file);
        assert!(
            edit_detected,
            "{} iter {i}: the edit must be visible to changed-set detection",
            case.lang
        );

        let (resp, _) = timed_callers(&index, &settings, &root, &case.query, Some(&pool));
        let has = callers_contain(&resp, &caller);
        if has {
            completed += 1;
        }
        println!(
            "  {} {} iter {i}/{iterations}: {}",
            case.lang,
            case.subject,
            outcome(has)
        );
    }
    drop(pool);
    RustTsEditResult {
        lang: case.lang.to_owned(),
        subject: case.subject.to_owned(),
        iterations_completed: completed,
        iterations_total: iterations,
        passed: completed == iterations,
    }
}

/// `crate::`-style path of a Rust FQN of the current crate.
fn crate_path(fqn: &str, name: &str) -> String {
    let rest = fqn
        .strip_suffix(&format!("::{name}"))
        .or_else(|| fqn.strip_suffix(&format!(".{name}")))
        .unwrap_or(fqn);
    match rest.split_once("::") {
        Some((_, tail)) if !tail.is_empty() => format!("crate::{tail}::{name}"),
        _ => format!("crate::{name}"),
    }
}

/// Choose the `knot` work-copy case: a public function and another module of
/// the same crate that references it.
fn select_d12_rust(index: &LoadedIndex, root: &Path) -> D12Case {
    let target = index
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.file.starts_with("src/")
                && !record.file.starts_with("src/bin/")
                && record.line.kind == "rust_function"
                && record.line.col.is_some()
                && !record.line.tc
        })
        .filter(|(pos, record)| {
            std::fs::read_to_string(root.join(&record.file)).is_ok_and(|text| {
                text.lines().any(|line| {
                    line.trim_start()
                        .starts_with(&format!("pub fn {}", record.line.name))
                })
            }) && !heuristic_answer(index, *pos).containers.is_empty()
        })
        .min_by_key(|(_, record)| record.line.id.clone())
        .map(|(_, record)| record)
        .expect("a public function of the knot work copy");

    let mut callers: Vec<String> = index
        .file_sha16
        .keys()
        .filter(|rel| {
            rel.starts_with("src/")
                && !rel.starts_with("src/bin/")
                && rel.ends_with(".rs")
                && **rel != target.file
        })
        .cloned()
        .collect();
    callers.sort();
    let caller_file = callers
        .first()
        .expect("a knot module for the new caller")
        .clone();
    println!(
        "  P6-D12 rust work copy: {} in {} <- {}",
        target.line.fqn, target.file, caller_file
    );

    D12Case {
        lang: "rust",
        subject: "work_copy",
        work_name: "knot",
        fixture: None,
        repo: "knot",
        query: target.line.fqn.clone(),
        caller_file,
        caller_name: "kl_d12_caller_{i}".to_owned(),
        caller_text: format!(
            "\npub fn kl_d12_caller_{{i}}() {{\n    let _ = {};\n}}\n",
            crate_path(&target.line.fqn, &target.line.name)
        ),
        import_line: None,
    }
}

/// The parent directory of a repo-relative path.
fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// Choose the `knot-site` work-copy case: an exported function of a directory
/// that has another module which imports it.
fn select_d12_ts(index: &LoadedIndex, root: &Path) -> D12Case {
    let mut per_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for rel in index.file_sha16.keys().filter(|rel| rel.ends_with(".ts")) {
        *per_dir.entry(dir_of(rel).to_owned()).or_default() += 1;
    }
    let target = index
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.file.starts_with("src/")
                && record.line.kind == "function"
                && record.line.col.is_some()
                && !record.line.tc
                && per_dir.get(dir_of(&record.file)).is_some_and(|n| *n >= 2)
        })
        .filter(|(pos, record)| {
            std::fs::read_to_string(root.join(&record.file)).is_ok_and(|text| {
                text.lines().any(|line| {
                    line.trim_start()
                        .starts_with(&format!("export function {}", record.line.name))
                })
            }) && !heuristic_answer(index, *pos).containers.is_empty()
        })
        .min_by_key(|(_, record)| record.line.id.clone())
        .map(|(_, record)| record)
        .expect("an exported function of the knot-site work copy");

    let dir = dir_of(&target.file).to_owned();
    let mut callers: Vec<String> = index
        .file_sha16
        .keys()
        .filter(|rel| dir_of(rel) == dir && rel.ends_with(".ts") && **rel != target.file)
        .cloned()
        .collect();
    callers.sort();
    let caller_file = callers
        .first()
        .expect("a knot-site module of the same directory")
        .clone();
    let stem = target
        .file
        .rsplit_once('/')
        .and_then(|(_, file)| file.strip_suffix(".ts"))
        .unwrap_or("");
    println!(
        "  P6-D12 typescript work copy: {} in {} <- {}",
        target.line.fqn, target.file, caller_file
    );

    D12Case {
        lang: "typescript",
        subject: "work_copy",
        work_name: "knot-site",
        fixture: None,
        repo: "knot-site",
        query: target.line.fqn.clone(),
        caller_file,
        caller_name: "klD12Caller{i}".to_owned(),
        caller_text: "\nexport function klD12Caller{i}(): unknown {\n    return {name};\n}\n"
            .replace("{name}", &target.line.name),
        import_line: Some(format!(
            "import {{ {} }} from './{stem}';",
            target.line.name
        )),
    }
}

/// The four P6-D12 cases. The work-copy cases put the new caller in another
/// file: an edit in the target's own file is synced by the `didOpen` reopen on
/// an mtime change, which would hide a missed file watch.
fn d12_cases() -> Vec<D12Case> {
    vec![
        D12Case {
            lang: "rust",
            subject: "fixture",
            work_name: "rust_long_session",
            fixture: Some("rust"),
            repo: "knot",
            query: "total_area".to_owned(),
            caller_file: "src/shapes.rs".to_owned(),
            caller_name: "caller_{i}".to_owned(),
            caller_text: "\npub fn caller_{i}() -> f64 { crate::geometry::total_area(&[]) }\n"
                .to_owned(),
            import_line: None,
        },
        D12Case {
            lang: "typescript",
            subject: "fixture",
            work_name: "ts_long_session",
            fixture: Some("typescript"),
            repo: "knot-site",
            query: "formatName".to_owned(),
            caller_file: "src/util/format.ts".to_owned(),
            caller_name: "caller{i}".to_owned(),
            caller_text: "\nexport function caller{i}(u: User): string { return formatName(u); }\n"
                .to_owned(),
            import_line: None,
        },
        {
            let root = ensure_work_copy("knot");
            let index = load_index(&IndexLayout::for_repo(&root)).expect("knot index");
            select_d12_rust(&index, &root)
        },
        {
            let root = ensure_work_copy("knot-site");
            let index = load_index(&IndexLayout::for_repo(&root)).expect("knot-site index");
            select_d12_ts(&index, &root)
        },
    ]
}

#[test]
#[ignore = "P6-D12: Rust and TS edit sessions in long-lived MCP session"]
fn rust_ts_edits_gate() {
    println!("=== P6-D12 Gate: Rust and TS in long session (sync=off) ===");
    let results = d12_cases()
        .iter()
        .map(|case| {
            println!("--- P6-D12 {} {} ---", case.lang, case.subject);
            if case.subject == "work_copy" {
                println!(
                    "  query {}, new caller goes to {}",
                    case.query, case.caller_file
                );
            }
            run_d12_case(case, 8)
        })
        .collect();
    write_gate(
        "rust_ts_edits",
        &RustTsReport {
            gate: "P6-D12".to_owned(),
            results,
        },
    );
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    /// A scenario where every mode has `avg_ms` and completes `need` unless listed in `short`.
    fn scenario(name: &str, need: usize, costs: [u64; 4], short: &[&str]) -> J1ScenarioReport {
        let modes = MODES
            .iter()
            .zip(costs)
            .map(|(mode, avg_ms)| J1ModeResult {
                mode: mode.label().to_owned(),
                completed: if short.contains(&mode.label()) {
                    need - 1
                } else {
                    need
                },
                total: need,
                avg_ms,
                sessions: Vec::new(),
            })
            .collect();
        J1ScenarioReport {
            gate: "J1".to_owned(),
            scenario: name.to_owned(),
            modes,
        }
    }

    fn reports(costs: [u64; 4], short: &[&str]) -> Vec<J1ScenarioReport> {
        REQUIRED
            .iter()
            .map(|(name, need)| scenario(name, *need, costs, short))
            .collect()
    }

    #[test]
    fn noise_rule_keeps_watched_when_it_passes() {
        let verdict = compute_verdict(&reports([100, 1_010, 1_000, 990], &["off"]));
        assert_eq!(verdict.winner, "watched");
        assert!(verdict.note.contains("noise rule"));
    }

    #[test]
    fn noise_rule_never_keeps_a_failing_watched() {
        let verdict = compute_verdict(&reports([100, 1_010, 1_000, 990], &["off", "watched"]));
        assert_eq!(verdict.winner, "open+watched");
    }

    #[test]
    fn no_passing_mode_means_none() {
        let all = ["off", "open", "watched", "open+watched"];
        assert_eq!(compute_verdict(&reports([1, 1, 1, 1], &all)).winner, "none");
    }
}
