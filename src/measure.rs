//! Measurement gates of PLAN_PHASE2_INDEX.md §10 (run explicitly, never by `make check`):
//!
//! KNOT_LITE_SANDBOX=~/.local/share/knot-lite/sandbox \
//! KNOT_LITE_MEASURE_OUT=~/.local/share/knot-lite/measure \
//! cargo test --release --lib measure:: -- --ignored --nocapture --test-threads=1
//!
//! The Phase 3 gates that start real language servers (PLAN_PHASE3_LSP.md §11)
//! additionally need `KNOT_LITE_LSP_CACHE_DIR` (the servers' scratch root, also
//! a persistent directory) and only ever run against the sandbox copies.
//!
//! The sandbox and output directories are required (no default): measurement
//! artefacts are worth keeping, so they never land in a temp directory by
//! accident. The model cache follows knot-lite's own resolution
//! (`KNOT_LITE_FASTEMBED_CACHE_DIR` → … → `$HOME/.cache/knot-lite/fastembed`).

#![cfg(test)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use knot::models::ParsedEntity;
use knot::pipeline::parser::{ParseConfig, parse_files_stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::config::LspOverrides;
use crate::embed::EmbedBatch;
use crate::embed::knot_embedder::KnotEmbedder;
use crate::embed::settings::resolve_embed_settings;
use crate::graph::containers::ContainerKey;
use crate::index::hash::{finish_hex, sha256_hex};
use crate::index::layout::IndexLayout;
use crate::index::load::{LoadedIndex, load_index};
use crate::lsp::launch::start_pool;
use crate::lsp::preopen::PreopenPlan;
use crate::lsp::process::{StartContext, StartOutcome, start_or_degrade, stop};
use crate::lsp::readiness::wait_ready;
use crate::lsp::settings::{LspSettings, resolve_lsp_settings, scratch_dir};
use crate::lsp::spec::{SpecInputs, resolve_program, spec_for};
use crate::lsp::{position, uri};
use crate::query::callers::{CallersContext, LspContext, callers_response};
use crate::query::search::top_k;
use crate::query::session::CLI_READY_WAIT;

/// Directory named by the required environment variable `key`.
fn required_env_dir(key: &str) -> PathBuf {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
        _ => panic!("{key} must be set to a persistent directory (see the module docs)"),
    }
}

fn sandbox_path(repo: &str) -> PathBuf {
    required_env_dir("KNOT_LITE_SANDBOX").join(repo)
}

fn fastembed_cache_dir() -> PathBuf {
    let lookup = |key: &str| std::env::var(key).ok();
    resolve_embed_settings(&lookup, &std::env::temp_dir()).cache_dir
}

fn measure_out_dir() -> PathBuf {
    let dir = required_env_dir("KNOT_LITE_MEASURE_OUT");
    std::fs::create_dir_all(&dir).ok();
    dir
}

// ---------------------------------------------------------------------------
// M1: Identity gate over knot-lite's parse path
// ---------------------------------------------------------------------------

fn parse_snapshot(root: &Path, repo_name: &str) -> anyhow::Result<Vec<ParsedEntity>> {
    let root_str = root.to_string_lossy().into_owned();
    let files = knot::pipeline::input::discover_files(&root_str, false)?;
    let parse_cfg = ParseConfig {
        repo_root: root.to_path_buf(),
        custom_queries_path: None,
        repo_name: repo_name.to_owned(),
        include_config_files: false,
        repo_path: Some(root_str),
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
    let jobs = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|scope| {
        let consumer = scope.spawn(move || {
            let mut entities = Vec::new();
            while let Some(entity) = rx.blocking_recv() {
                entities.push(entity);
            }
            entities
        });
        let producer = scope.spawn(move || parse_files_stream(&files, &parse_cfg, tx, jobs, None));
        producer
            .join()
            .map_err(|_| anyhow::anyhow!("producer panic"))?;
        let mut entities = consumer
            .join()
            .map_err(|_| anyhow::anyhow!("consumer panic"))?;
        crate::index::build::normalise_and_sort(&mut entities);
        Ok(entities)
    })
}

#[derive(Serialize)]
struct M1RepoResult {
    repo: String,
    total_entities: usize,
    diff_uuid_across_runs: usize,
    collisions_in_run: usize,
}

#[test]
#[ignore = "M1 measurement gate: requires sandbox repos at $KNOT_LITE_SANDBOX"]
fn m1_identity_knot_lite_parse() {
    let mut results = Vec::new();
    for repo in ["knot", "knot-site", "spring-ai"] {
        let path = sandbox_path(repo);
        assert!(path.exists(), "sandbox repo {path:?} missing");

        let run1 = parse_snapshot(&path, repo).expect("parse 1");
        let run2 = parse_snapshot(&path, repo).expect("parse 2");
        assert_eq!(run1.len(), run2.len());

        let map1: HashMap<(String, usize, String), Uuid> = run1
            .iter()
            .map(|e| ((e.file_path.clone(), e.start_line, e.name.clone()), e.uuid))
            .collect();
        let map2: HashMap<(String, usize, String), Uuid> = run2
            .iter()
            .map(|e| ((e.file_path.clone(), e.start_line, e.name.clone()), e.uuid))
            .collect();

        let mut diff_uuid = 0;
        for (key, uuid1) in &map1 {
            if let Some(uuid2) = map2.get(key)
                && uuid1 != uuid2
            {
                diff_uuid += 1;
            }
        }

        let mut uuids_seen = BTreeSet::new();
        let mut collisions = 0;
        for e in &run1 {
            if !uuids_seen.insert(e.uuid) {
                collisions += 1;
            }
        }

        let res = M1RepoResult {
            repo: repo.to_owned(),
            total_entities: run1.len(),
            diff_uuid_across_runs: diff_uuid,
            collisions_in_run: collisions,
        };
        println!(
            "M1 {repo}: total={}, diff={}, collisions={}",
            res.total_entities, res.diff_uuid_across_runs, res.collisions_in_run
        );
        assert_eq!(res.diff_uuid_across_runs, 0);
        assert_eq!(res.collisions_in_run, 0);
        results.push(res);
    }
    let text = serde_json::to_string_pretty(&results).expect("json");
    std::fs::write(measure_out_dir().join("m1_identity.json"), text).expect("write");
}

// ---------------------------------------------------------------------------
// M4: Retrieval accuracy on entities.toml / level1.toml
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct QueryEntry {
    id: String,
    repo: String,
    text: String,
    expected: Vec<String>,
}

#[derive(Deserialize)]
struct QueriesFile {
    query: Vec<QueryEntry>,
}

fn is_expected_match(file: &str, expected: &[String]) -> bool {
    expected.iter().any(|exp| {
        if exp.ends_with('/') {
            file.starts_with(exp)
        } else {
            file == exp
        }
    })
}

#[derive(Serialize)]
struct M4QueryResult {
    id: String,
    repo: String,
    text: String,
    entity_domain_hit5: bool,
    tier1_domain_hit5: bool,
    mixed_domain_hit5: bool,
}

#[test]
#[ignore = "M4 measurement gate: requires indexed sandbox repos and fastembed model"]
fn m4_retrieval() {
    let root = Path::new("docs/spikes/phase0/queries");
    let entities_file: QueriesFile = toml::from_str(
        &std::fs::read_to_string(root.join("entities.toml")).expect("read entities"),
    )
    .expect("parse entities");

    let mut loaded_indexes: HashMap<String, LoadedIndex> = HashMap::new();
    let mut embedders: HashMap<String, KnotEmbedder> = HashMap::new();

    for repo in ["knot", "knot-site", "spring-ai"] {
        let layout = IndexLayout::for_repo(&sandbox_path(repo));
        assert!(
            layout.manifest.exists(),
            "index for {repo} missing; run knot-lite index first"
        );
        let idx = load_index(&layout).expect("load");
        let model = idx.manifest.embedding.model.clone();
        if !embedders.contains_key(&model) {
            let embedder = KnotEmbedder::init(&model, &fastembed_cache_dir()).expect("init model");
            embedders.insert(model.clone(), embedder);
        }
        loaded_indexes.insert(repo.to_owned(), idx);
    }

    let mut results = Vec::new();
    let mut repo_entity_hits: HashMap<String, (usize, usize)> = HashMap::new();

    for q in &entities_file.query {
        let idx = &loaded_indexes[&q.repo];
        let embedder = embedders
            .get_mut(&idx.manifest.embedding.model)
            .expect("embedder");
        let query_vector = embedder.embed_query(&q.text).expect("embed query");

        // Entity domain candidates (non tier-1 records)
        let entity_candidates: Vec<usize> = (0..idx.matrix.rows())
            .filter(|&r| !idx.records[idx.matrix.record[r]].line.is_tier1())
            .collect();
        let top_entity = top_k(&idx.matrix, &entity_candidates, &query_vector, 5);
        let entity_hit = top_entity.iter().any(|(r, _)| {
            let rec = &idx.records[idx.matrix.record[*r]];
            is_expected_match(&rec.file, &q.expected)
        });

        // Tier-1 domain candidates (chunk/file records)
        let tier1_candidates: Vec<usize> = (0..idx.matrix.rows())
            .filter(|&r| idx.records[idx.matrix.record[r]].line.is_tier1())
            .collect();
        let top_tier1 = top_k(&idx.matrix, &tier1_candidates, &query_vector, 5);
        let tier1_hit = top_tier1.iter().any(|(r, _)| {
            let rec = &idx.records[idx.matrix.record[*r]];
            is_expected_match(&rec.file, &q.expected)
        });

        // Mixed domain candidates (all records)
        let mixed_candidates: Vec<usize> = (0..idx.matrix.rows()).collect();
        let top_mixed = top_k(&idx.matrix, &mixed_candidates, &query_vector, 5);
        let mixed_hit = top_mixed.iter().any(|(r, _)| {
            let rec = &idx.records[idx.matrix.record[*r]];
            is_expected_match(&rec.file, &q.expected)
        });

        let entry = repo_entity_hits.entry(q.repo.clone()).or_insert((0, 0));
        entry.1 += 1;
        if entity_hit {
            entry.0 += 1;
        }

        results.push(M4QueryResult {
            id: q.id.clone(),
            repo: q.repo.clone(),
            text: q.text.clone(),
            entity_domain_hit5: entity_hit,
            tier1_domain_hit5: tier1_hit,
            mixed_domain_hit5: mixed_hit,
        });
    }

    println!("=== M4 Retrieval hit@5 (Entity Domain) ===");
    for (repo, (hits, total)) in &repo_entity_hits {
        let pct = (*hits as f64) / (*total as f64) * 100.0;
        println!("  {repo}: {hits}/{total} ({pct:.1}%)");
    }

    let text = serde_json::to_string_pretty(&results).expect("json");
    std::fs::write(measure_out_dir().join("m4_retrieval.json"), text).expect("write");

    if let Some((hits, total)) = repo_entity_hits.get("knot") {
        let pct = (*hits as f64) / (*total as f64);
        assert!(pct >= 0.75, "knot entity hit@5 = {pct} < 75%");
    }
    if let Some((hits, total)) = repo_entity_hits.get("spring-ai") {
        let pct = (*hits as f64) / (*total as f64);
        assert!(pct >= 0.833, "spring-ai entity hit@5 = {pct} < 83.3%");
    }
}

// ---------------------------------------------------------------------------
// M5: Load latency
// ---------------------------------------------------------------------------

#[test]
#[ignore = "M5 measurement gate: requires indexed sandbox repos"]
fn m5_load() {
    let mut timings = HashMap::new();
    for repo in ["knot", "knot-site", "spring-ai"] {
        let layout = IndexLayout::for_repo(&sandbox_path(repo));
        assert!(layout.manifest.exists(), "index for {repo} missing");
        let mut runs = Vec::new();
        for _ in 0..5 {
            let idx = load_index(&layout).expect("load");
            runs.push(idx.load_ms);
        }
        runs.sort();
        let median = runs[2];
        println!("M5 load median {repo}: {median} ms (runs: {runs:?})");
        timings.insert(repo.to_owned(), median);
    }
    let text = serde_json::to_string_pretty(&timings).expect("json");
    std::fs::write(measure_out_dir().join("m5_load.json"), text).expect("write");

    assert!(
        timings["knot"] <= 200,
        "knot load median {} ms > 200 ms",
        timings["knot"]
    );
}

// ---------------------------------------------------------------------------
// M6: Caller parity with spike
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SpikeCaller {
    file: String,
    line: usize,
}

#[derive(Deserialize)]
struct SpikeTarget {
    id: String,
    callers: Vec<SpikeCaller>,
}

#[derive(Deserialize)]
struct TargetFileEntry {
    id: String,
    name: String,
    file: String,
    line: usize,
    knot_query: String,
}

#[derive(Deserialize)]
struct TargetFile {
    target: Vec<TargetFileEntry>,
}

#[derive(Deserialize)]
struct SpikeResult {
    targets: Vec<SpikeTarget>,
}

#[test]
#[ignore = "M6 measurement gate: requires indexed sandbox repos and spike result JSONs"]
fn m6_caller_parity() {
    let spikes = [
        ("rust", "knot", "rust_knot.toml", "heuristic_rust.json"),
        ("ts", "knot-site", "ts_knot_site.toml", "heuristic_ts.json"),
        (
            "java",
            "spring-ai",
            "java_spring_ai.toml",
            "heuristic_java.json",
        ),
    ];

    for (lang, repo, target_toml, file) in spikes {
        let targets_file: TargetFile = toml::from_str(
            &std::fs::read_to_string(Path::new("docs/spikes/phase0/targets").join(target_toml))
                .expect("read target toml"),
        )
        .expect("parse target toml");
        let target_map: HashMap<String, TargetFileEntry> = targets_file
            .target
            .into_iter()
            .map(|t| (t.id.clone(), t))
            .collect();

        let spike_path = Path::new("docs/spikes/phase0/results").join(file);
        let spike_res: SpikeResult =
            serde_json::from_str(&std::fs::read_to_string(&spike_path).expect("read spike"))
                .expect("parse spike");
        let layout = IndexLayout::for_repo(&sandbox_path(repo));
        let index = load_index(&layout).expect("load");

        println!("=== M6 Parity {repo} ({lang}) ===");
        for target in &spike_res.targets {
            let spike_set: BTreeSet<ContainerKey> = target
                .callers
                .iter()
                .map(|c| index.containers.container_at(&c.file, c.line).0)
                .collect();

            let target_info = &target_map[&target.id];
            let response = crate::query::callers::test_support::heuristic_response(
                &index,
                repo,
                &target_info.knot_query,
                500,
                &knot::cli_tools::kinds::KindFilter::parse(Some("all")),
            );

            let mut lite_set = BTreeSet::new();
            // Find matching target in response by (file, line)
            if let Some(t) = response.targets.iter().find(|t| {
                t.file == target_info.file
                    && (t.l0 <= target_info.line && target_info.line <= t.l1
                        || (target_info.line == 0 && t.l0 == 0))
            }) {
                for r in t.callers.0.iter().flat_map(|(_, rows)| rows) {
                    lite_set.insert(ContainerKey {
                        file: r.file.clone(),
                        line: r.l0,
                        name: r.name.clone(),
                    });
                }
            }

            let equal = spike_set == lite_set;
            let missing: Vec<_> = spike_set.difference(&lite_set).collect();
            let extra: Vec<_> = lite_set.difference(&spike_set).collect();

            println!(
                "  target {} ({}): equal={}, missing={}, extra={}",
                target.id,
                target_info.knot_query,
                equal,
                missing.len(),
                extra.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// L5 smoke (Phase 3, P3.6): a real rust-analyzer leaves the repository untouched
// ---------------------------------------------------------------------------

/// SHA-256 over the relative path and content of every file and the path of
/// every directory below `root`, skipping the top-level names in `skip`.
/// Symlinks are fingerprinted by their target text and never followed.
fn tree_fingerprint(root: &Path, skip: &[&str]) -> String {
    fn walk(base: &Path, dir: &Path, skip: &[&str], out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|entry| entry.expect("entry"))
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .expect("under base")
                .to_string_lossy()
                .replace('\\', "/");
            if skip.contains(&rel.as_str()) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).expect("metadata");
            if meta.is_dir() {
                out.push((format!("{rel}/"), String::new()));
                walk(base, &path, skip, out);
            } else if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap_or_default();
                out.push((rel, format!("link:{}", target.display())));
            } else {
                out.push((rel, sha256_hex(&std::fs::read(&path).expect("read"))));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, skip, &mut files);
    let mut hasher = Sha256::new();
    for (rel, digest) in files {
        hasher.update(format!("{rel}\0{digest}\n").as_bytes());
    }
    finish_hex(hasher)
}

#[derive(Serialize)]
struct L5SmokeReport {
    server_version: String,
    initialize_ms: u64,
    ready_ms: u64,
    ready_reason: String,
    target: String,
    references: usize,
    references_ms: u64,
    stop_ms: u64,
    scratch: String,
}

/// Start rust-analyzer on `repo`, wait until it is ready, ask for the references
/// of `parse_files`, and stop it.
async fn l5_smoke_rust(repo: &Path, scratch: &Path) -> L5SmokeReport {
    let program = resolve_program(LspLang::Rust, &LspOverrides::default())
        .unwrap_or_else(|degraded| panic!("{}", degraded.diagnostic));
    let owner = crate::lsp::owner::OwnerToken::new_for(LspLang::Rust);
    let spec = spec_for(&SpecInputs {
        lang: LspLang::Rust,
        program: &program,
        repo_root: repo,
        scratch,
        build_exec: true,
        preopen: &PreopenPlan::default(),
        owner: &owner,
    });
    let ctx = StartContext {
        repo_root: repo,
        scratch,
        logs_keep: 5,
        owner: &owner,
    };
    let StartOutcome::Ready(mut started) = start_or_degrade(&spec, &ctx).await else {
        panic!("rust-analyzer did not start (see the stderr log under {scratch:?}/logs)");
    };
    let ready = wait_ready(
        &mut started.session.events,
        spec.readiness,
        std::time::Duration::from_secs(2),
        spec.ready_timeout,
    )
    .await;
    assert!(ready.ready, "not ready: {ready:?}");

    let rel = "src/pipeline/parser/mod.rs";
    let abs = repo.join(rel);
    let source = std::fs::read_to_string(&abs).expect("parse_files source");
    let line_no = source
        .lines()
        .position(|line| line.contains("fn parse_files("))
        .expect("`fn parse_files(` in the sandbox")
        + 1;
    let name = position::locate_name(&source, "parse_files", line_no, line_no, LspLang::Rust)
        .expect("parse_files located");
    started.session.did_open(&abs).expect("didOpen");
    let file_uri = uri::path_to_uri(&abs).expect("uri");
    let asked = std::time::Instant::now();
    let outcome = started
        .session
        .references(
            &file_uri,
            async_lsp::lsp_types::Position::new(name.line0, name.col_utf16),
            false,
            spec.first_request_timeout,
        )
        .await;
    let references = outcome
        .result
        .expect("references answered")
        .expect("references not null");
    assert!(!references.is_empty(), "parse_files has callers in knot");

    let report_so_far = (
        started.session.server_version.clone(),
        started.session.timings.initialize_ms,
    );
    let stopping = std::time::Instant::now();
    stop(*started, std::time::Duration::from_secs(15)).await;
    L5SmokeReport {
        server_version: report_so_far.0,
        initialize_ms: report_so_far.1,
        ready_ms: u64::try_from(ready.elapsed.as_millis()).unwrap_or(u64::MAX),
        ready_reason: ready.reason,
        target: format!("{rel}:{}:{}", name.line0 + 1, name.col_utf16),
        references: references.len(),
        references_ms: u64::try_from(asked.elapsed().as_millis()).unwrap_or(u64::MAX),
        stop_ms: u64::try_from(stopping.elapsed().as_millis()).unwrap_or(u64::MAX),
        scratch: scratch.display().to_string(),
    }
}

#[test]
#[ignore = "L5 smoke: starts a real rust-analyzer on the sandbox `knot` (KNOT_LITE_SANDBOX, KNOT_LITE_MEASURE_OUT, KNOT_LITE_LSP_CACHE_DIR)"]
fn l5_smoke_containment_rust() {
    let repo = sandbox_path("knot").canonicalize().expect("sandbox knot");
    // The scratch root is a measurement artefact: no default, like the others.
    required_env_dir("KNOT_LITE_LSP_CACHE_DIR");
    let settings = resolve_lsp_settings(&|key| std::env::var(key).ok(), &std::env::temp_dir());
    let scratch = scratch_dir(&settings, &repo);
    let index_dir = repo.join(".knot-lite");
    let source_before = tree_fingerprint(&repo, &[".knot-lite", ".knot"]);
    let index_before = index_dir
        .is_dir()
        .then(|| tree_fingerprint(&index_dir, &[]));

    let report = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(l5_smoke_rust(&repo, &scratch));

    println!(
        "L5 smoke rust: {}",
        serde_json::to_string(&report).expect("json")
    );
    assert_eq!(
        tree_fingerprint(&repo, &[".knot-lite", ".knot"]),
        source_before,
        "rust-analyzer changed the repository"
    );
    assert_eq!(
        index_dir
            .is_dir()
            .then(|| tree_fingerprint(&index_dir, &[])),
        index_before,
        "rust-analyzer changed the index directory"
    );
    assert!(
        scratch.join("target-ra").is_dir(),
        "cargo's target dir must live in the scratch directory"
    );
    let text = serde_json::to_string_pretty(&report).expect("json");
    std::fs::write(measure_out_dir().join("l5_smoke_rust.json"), text).expect("write");
}

// ---------------------------------------------------------------------------
// Phase 3 gates L1, L2, L3 (PLAN_PHASE3_LSP.md §11): real language servers on
// the sandbox copies, which must already be indexed by the current build
// ---------------------------------------------------------------------------

/// Nearest-rank percentile of `values` (0 when empty).
fn percentile(values: &[u64], pct: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (pct / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn write_gate(name: &str, value: &impl Serialize) {
    let text = serde_json::to_string_pretty(value).expect("json");
    std::fs::write(measure_out_dir().join(format!("{name}.json")), text).expect("write gate");
}

#[derive(Deserialize)]
struct S1File {
    targets: Vec<S1Target>,
}

#[derive(Deserialize)]
struct S1Target {
    id: String,
    #[serde(default)]
    ops: S1Ops,
}

#[derive(Deserialize, Default)]
struct S1Ops {
    incoming: Option<S1Op>,
    references: Option<S1Op>,
    implementation: Option<S1Op>,
}

#[derive(Deserialize)]
struct S1Op {
    #[serde(default)]
    locations: Vec<S1Location>,
}

#[derive(Deserialize)]
struct S1Location {
    file: String,
    line0: usize,
}

/// Whether line `line0` of `file` (under `root`) is entirely a comment today.
fn is_comment_line_at(root: &Path, file: &str, line0: usize) -> bool {
    std::fs::read_to_string(uri::join_rel(root, file))
        .ok()
        .and_then(|text| {
            text.lines()
                .nth(line0)
                .map(crate::lsp::normalize::is_comment_line)
        })
        .unwrap_or(false)
}

/// What one Phase 0 target looks like against the language server of today.
#[derive(Serialize)]
struct ParityRow {
    id: String,
    query: String,
    provenance: String,
    status: String,
    ms: u64,
    phase0: usize,
    ours: usize,
    shared: usize,
    missing: Vec<String>,
    /// Missing containers whose every Phase 0 location is a comment line today
    /// (the references P3-D17 drops on purpose).
    explained_missing: usize,
    extra: Vec<String>,
}

fn container_label(key: &ContainerKey) -> String {
    format!("{}:{} {}", key.file, key.line, key.name)
}

/// The settings of a measurement run: the environment's, with generous limits.
fn measurement_settings() -> LspSettings {
    required_env_dir("KNOT_LITE_LSP_CACHE_DIR");
    let settings = LspSettings {
        max_live_targets: 500,
        deadline: std::time::Duration::from_secs(900),
        ..resolve_lsp_settings(&|key| std::env::var(key).ok(), &std::env::temp_dir())
    };
    assert!(settings.enabled, "KNOT_LITE_LSP must not be off");
    settings
}

struct ParitySpec {
    repo: &'static str,
    lang: &'static str,
    targets: &'static str,
    s1: &'static str,
    /// Share of targets whose container sets must equal Phase 0's, or differ
    /// only by comment-only references (P3-D17).
    min_identical_pct: f64,
    /// Micro recall against Phase 0's union.
    min_recall_pct: f64,
    /// p95 of the warm `lsp-live` time, when the gate applies to the language.
    max_p95_ms: Option<u64>,
}

/// L2 (parity with Phase 0) and L3 (warm live latency) for one language.
/// Everything one language's L2/L3 run needs: the index, a real pool and the reference.
struct ParityRun<'a> {
    spec: &'a ParitySpec,
    root: PathBuf,
    index: LoadedIndex,
    settings: LspSettings,
    pool: crate::lsp::pool::LspPool,
    s1: S1File,
}

impl<'a> ParityRun<'a> {
    /// Open the sandbox index, start the pool and read the reference (Phase 0's
    /// own files, or `KNOT_LITE_L2_ORACLE`: a directory with `targets/` and
    /// `results/` when the spike was re-run on the sandbox).
    fn start(spec: &'a ParitySpec) -> (Self, TargetFile) {
        let root = sandbox_path(spec.repo)
            .canonicalize()
            .expect("sandbox repo");
        let mut index = load_index(&IndexLayout::for_repo(&root)).expect("load the sandbox index");
        if std::env::var("KNOT_LITE_MEASURE_LIVE_ONLY").is_ok_and(|v| v == "1") {
            // Measure the language server, not the data a previous index run stored.
            index.lsp_targets.clear();
            index.lsp_incoming.clear();
        }
        let mut cfg = crate::config::test_support::config_for(root.clone());
        cfg.lsp = measurement_settings();
        let files: Vec<String> = index.file_sha16.keys().cloned().collect();
        let pool =
            start_pool(&cfg, &files, CLI_READY_WAIT, Some(index.stamp.clone())).expect("pool");
        let oracle = std::env::var_os("KNOT_LITE_L2_ORACLE")
            .map_or_else(|| PathBuf::from("docs/spikes/phase0"), PathBuf::from);
        let targets: TargetFile = toml::from_str(
            &std::fs::read_to_string(oracle.join("targets").join(spec.targets))
                .expect("read targets"),
        )
        .expect("parse targets");
        let s1: S1File = serde_json::from_str(
            &std::fs::read_to_string(oracle.join("results").join(spec.s1)).expect("read s1"),
        )
        .expect("parse s1");
        let run = Self {
            spec,
            root,
            index,
            settings: cfg.lsp,
            pool,
            s1,
        };
        (run, targets)
    }

    /// `find_callers` for `query` and how long it took, in milliseconds.
    fn ask(&self, query: &str) -> (crate::query::callers::CallersResponse, u64) {
        let any = knot::cli_tools::kinds::KindFilter::parse(Some("all"));
        let started = std::time::Instant::now();
        let response = callers_response(&CallersContext {
            index: &self.index,
            repo: self.spec.repo,
            query,
            max_targets: 500,
            filter: &any,
            lsp: LspContext {
                settings: &self.settings,
                repo_root: &self.root,
                pool: Some(&self.pool),
            },
        });
        (
            response,
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        )
    }

    /// The containers the reference found for `id`, with the locations they came from.
    fn reference(&self, id: &str, own: &ContainerKey) -> Vec<(&S1Location, ContainerKey)> {
        let Some(target) = self.s1.targets.iter().find(|t| t.id == id) else {
            return Vec::new();
        };
        [
            &target.ops.incoming,
            &target.ops.references,
            &target.ops.implementation,
        ]
        .into_iter()
        .flatten()
        .flat_map(|op| &op.locations)
        .map(|loc| {
            (
                loc,
                self.index
                    .containers
                    .container_at(&loc.file, loc.line0 + 1)
                    .0,
            )
        })
        .filter(|(_, key)| key != own)
        .collect()
    }

    /// Ask about one target twice (the first answer starts the server and
    /// warms its caches; the second is measured) and compare with the reference.
    fn row(&self, entry: &TargetFileEntry) -> ParityRow {
        let record = self.index.records.iter().find(|r| {
            !r.line.is_tier1()
                && r.file == entry.file
                && r.line.name == entry.name
                && r.line.l0 <= entry.line
                && entry.line <= r.line.l1
        });
        let Some(record) = record else {
            println!(
                "  {}: NOT IN THE INDEX ({}:{})",
                entry.id, entry.file, entry.line
            );
            return ParityRow::absent(entry);
        };
        let query = record.line.fqn.clone();
        let _ = self.ask(&query);
        let (response, ms) = self.ask(&query);
        let own = self
            .index
            .containers
            .container_at(&record.file, record.line.l0)
            .0;
        let locations = self.reference(&entry.id, &own);
        let phase0: BTreeSet<ContainerKey> = locations.iter().map(|(_, key)| key.clone()).collect();
        let target = response.targets.iter().find(|t| t.id == record.line.id);
        let ours: BTreeSet<ContainerKey> = target.map(answer_keys).unwrap_or_default();
        let missing_keys: Vec<&ContainerKey> = phase0.difference(&ours).collect();
        // A missing container whose every reference is a comment line today is
        // what P3-D17 drops on purpose.
        let explained_missing = missing_keys
            .iter()
            .filter(|key| {
                locations
                    .iter()
                    .filter(|(_, k)| k == **key)
                    .all(|(loc, _)| is_comment_line_at(&self.root, &loc.file, loc.line0))
            })
            .count();
        ParityRow {
            id: entry.id.clone(),
            query,
            provenance: target.map_or("none", |t| t.provenance).to_owned(),
            status: target
                .and_then(|t| t.lsp.as_ref())
                .map_or("none", |lsp| lsp.status)
                .to_owned(),
            ms,
            phase0: phase0.len(),
            ours: ours.len(),
            shared: phase0.intersection(&ours).count(),
            missing: missing_keys
                .iter()
                .map(|key| container_label(key))
                .collect(),
            explained_missing,
            extra: ours.difference(&phase0).map(container_label).collect(),
        }
    }
}

impl ParityRow {
    /// A target the index does not contain.
    fn absent(entry: &TargetFileEntry) -> Self {
        Self {
            id: entry.id.clone(),
            query: entry.knot_query.clone(),
            provenance: "none".to_owned(),
            status: "not_in_index".to_owned(),
            ms: 0,
            phase0: 0,
            ours: 0,
            shared: 0,
            missing: Vec::new(),
            explained_missing: 0,
            extra: Vec::new(),
        }
    }

    fn answered(&self) -> bool {
        matches!(self.provenance.as_str(), "lsp-live" | "lsp-indexed")
    }

    /// No difference, or only comment-only references.
    fn identical_or_explained(&self) -> bool {
        self.extra.is_empty() && self.explained_missing == self.missing.len()
    }
}

/// Print, record and assert the L2/L3 verdict of one language.
fn summarise_parity(spec: &ParitySpec, rows: &[ParityRow]) {
    let asked = rows.len();
    let answered: Vec<&ParityRow> = rows.iter().filter(|r| r.answered()).collect();
    let identical = answered
        .iter()
        .filter(|r| r.missing.is_empty() && r.extra.is_empty())
        .count();
    let identical_or_explained = answered
        .iter()
        .filter(|r| r.identical_or_explained())
        .count();
    let phase0_total: usize = rows.iter().map(|r| r.phase0).sum();
    let shared_total: usize = answered.iter().map(|r| r.shared).sum();
    let live_ms: Vec<u64> = rows
        .iter()
        .filter(|r| r.provenance == "lsp-live")
        .map(|r| r.ms)
        .collect();
    let identical_pct = 100.0 * identical as f64 / asked as f64;
    let explained_pct = 100.0 * identical_or_explained as f64 / asked as f64;
    let recall_pct = 100.0 * shared_total as f64 / phase0_total.max(1) as f64;
    let (p50, p95) = (percentile(&live_ms, 50.0), percentile(&live_ms, 95.0));
    println!(
        "  => answered {}/{asked}, identical {identical} ({identical_pct:.1} %), identical or explained {identical_or_explained} ({explained_pct:.1} %), micro recall {recall_pct:.1} %, live n={} p50 {p50} ms p95 {p95} ms",
        answered.len(),
        live_ms.len()
    );
    write_gate(
        &format!("l2_l3_{}", spec.lang),
        &serde_json::json!({
            "repo": spec.repo, "lang": spec.lang, "targets": asked,
            "answered_by_a_language_server": answered.len(),
            "identical": identical, "identical_pct": identical_pct,
            "identical_or_explained": identical_or_explained, "identical_or_explained_pct": explained_pct,
            "micro_recall_pct": recall_pct,
            "live": { "n": live_ms.len(), "p50_ms": p50, "p95_ms": p95 },
            "thresholds": {
                "min_identical_pct": spec.min_identical_pct,
                "min_recall_pct": spec.min_recall_pct,
                "max_p95_ms": spec.max_p95_ms,
            },
            "rows": rows,
        }),
    );

    assert_eq!(
        answered.len(),
        asked,
        "every target must be answered by a language server"
    );
    assert!(
        explained_pct >= spec.min_identical_pct,
        "L2 {}: {explained_pct:.1} % of targets identical or explained < {}",
        spec.lang,
        spec.min_identical_pct
    );
    assert!(
        recall_pct >= spec.min_recall_pct,
        "L2 {}: micro recall {recall_pct:.1} % < {}",
        spec.lang,
        spec.min_recall_pct
    );
    if let Some(max) = spec.max_p95_ms {
        assert!(p95 <= max, "L3 {}: p95 {p95} ms > {max} ms", spec.lang);
    }
}

/// L2 (parity with the reference) and L3 (warm live latency) for one language.
fn l2_l3(spec: &ParitySpec) {
    let (run, targets) = ParityRun::start(spec);
    println!("=== L2/L3 {} ({}) ===", spec.repo, spec.lang);
    let rows: Vec<ParityRow> = targets
        .target
        .iter()
        .map(|entry| {
            let row = run.row(entry);
            println!(
                "  {} {:>6} ms {:<11} {:<8} phase0 {:>3} ours {:>3} shared {:>3} missing {} (explained {}) extra {}",
                row.id,
                row.ms,
                row.provenance,
                row.status,
                row.phase0,
                row.ours,
                row.shared,
                row.missing.len(),
                row.explained_missing,
                row.extra.len()
            );
            row
        })
        .collect();
    drop(run);
    summarise_parity(spec, &rows);
}

#[test]
#[ignore = "L2/L3: starts a real rust-analyzer on the sandbox `knot` (needs the P3 index and KNOT_LITE_LSP_CACHE_DIR)"]
fn l2_l3_rust() {
    l2_l3(&ParitySpec {
        repo: "knot",
        lang: "rust",
        targets: "rust_knot.toml",
        s1: "s1_rust_warm.json",
        min_identical_pct: 90.0,
        min_recall_pct: 95.0,
        max_p95_ms: Some(2_000),
    });
}

#[test]
#[ignore = "L2/L3: starts a real typescript-language-server on the sandbox `knot-site` (needs the P3 index and KNOT_LITE_LSP_CACHE_DIR)"]
fn l2_l3_ts() {
    l2_l3(&ParitySpec {
        repo: "knot-site",
        lang: "ts",
        targets: "ts_knot_site.toml",
        s1: "s1_ts_warm.json",
        min_identical_pct: 90.0,
        min_recall_pct: 95.0,
        max_p95_ms: Some(2_000),
    });
}

#[test]
#[ignore = "L2/L3: starts a real jdtls on the sandbox `spring-ai` (needs the P3 index and KNOT_LITE_LSP_CACHE_DIR; minutes)"]
fn l2_l3_java() {
    l2_l3(&ParitySpec {
        repo: "spring-ai",
        lang: "java",
        targets: "java_spring_ai.toml",
        s1: "s1_java_warm.json",
        min_identical_pct: 90.0,
        min_recall_pct: 90.0,
        // Report only: the Java targets that are not pre-computed are slow (G8).
        max_p95_ms: None,
    });
}

// ---------------------------------------------------------------------------
// L1: the `col` stored in the index against the server's own selection ranges
// ---------------------------------------------------------------------------

/// A symbol of a `documentSymbol` answer, flattened.
struct Symbol {
    name: String,
    first_line0: u32,
    last_line0: u32,
    selection_line0: u32,
    selection_col: u32,
}

fn flatten_symbols(symbols: &[async_lsp::lsp_types::DocumentSymbol], out: &mut Vec<Symbol>) {
    for symbol in symbols {
        // jdtls names methods `name(Type)`; tsserver and rust-analyzer may add
        // generics: the declared name is what comes before them.
        let name = symbol
            .name
            .split(['(', '<', ' ', ':'])
            .next()
            .unwrap_or_default()
            .to_owned();
        out.push(Symbol {
            name,
            first_line0: symbol.range.start.line,
            last_line0: symbol.range.end.line,
            selection_line0: symbol.selection_range.start.line,
            selection_col: symbol.selection_range.start.character,
        });
        flatten_symbols(symbol.children.as_deref().unwrap_or_default(), out);
    }
}

/// Start the server of `lang` on `repo` and wait until it is ready.
async fn start_ready_server(
    lang: LspLang,
    repo: &Path,
    scratch: &Path,
    files: &[String],
) -> (
    Box<crate::lsp::process::Started>,
    crate::lsp::spec::ServerSpec,
) {
    let program = resolve_program(lang, &LspOverrides::default())
        .unwrap_or_else(|degraded| panic!("{}", degraded.diagnostic));
    let preopen = crate::lsp::preopen::plan(repo, files);
    let owner = crate::lsp::owner::OwnerToken::new_for(lang);
    let spec = spec_for(&SpecInputs {
        lang,
        program: &program,
        repo_root: repo,
        scratch,
        build_exec: true,
        preopen: &preopen,
        owner: &owner,
    });
    let ctx = StartContext {
        repo_root: repo,
        scratch,
        logs_keep: 5,
        owner: &owner,
    };
    let StartOutcome::Ready(mut started) = start_or_degrade(&spec, &ctx).await else {
        panic!(
            "{} did not start (see {scratch:?}/logs)",
            lang.server_program()
        );
    };
    for rel in &preopen.ts_files {
        let _ = started.session.did_open(&uri::join_rel(repo, rel));
    }
    let ready = wait_ready(
        &mut started.session.events,
        spec.readiness,
        std::time::Duration::from_secs(2),
        spec.ready_timeout,
    )
    .await;
    assert!(
        ready.ready,
        "{} not ready: {ready:?}",
        lang.server_program()
    );
    (started, spec)
}

#[derive(Serialize)]
struct ColumnReport {
    repo: String,
    lang: &'static str,
    files_sampled: usize,
    files_without_symbols: usize,
    entities_with_col: usize,
    compared: usize,
    matched: usize,
    accuracy_pct: f64,
    mismatches: Vec<String>,
}

/// Compare the stored `col` of up to `max_files` evenly spread files of `lang`.
async fn l1_language(repo: &str, lang: LspLang, max_files: usize) -> ColumnReport {
    let root = sandbox_path(repo).canonicalize().expect("sandbox repo");
    let index = load_index(&IndexLayout::for_repo(&root)).expect("load the sandbox index");
    let settings = measurement_settings();
    let scratch = scratch_dir(&settings, &root);
    let files: Vec<String> = index.file_sha16.keys().cloned().collect();
    let candidates: Vec<&String> = index
        .by_file
        .keys()
        .filter(|rel| lsp_lang_for_label(label_for(Path::new(rel.as_str()))) == Some(lang))
        .collect();
    let step = candidates.len().div_ceil(max_files).max(1);
    let sample: Vec<&&String> = candidates.iter().step_by(step).collect();

    let (mut started, spec) = start_ready_server(lang, &root, &scratch, &files).await;
    let mut report = ColumnReport {
        repo: repo.to_owned(),
        lang: lang.dir_name(),
        files_sampled: sample.len(),
        files_without_symbols: 0,
        entities_with_col: 0,
        compared: 0,
        matched: 0,
        accuracy_pct: 0.0,
        mismatches: Vec::new(),
    };
    for rel in sample {
        let abs = uri::join_rel(&root, rel);
        let source = std::fs::read_to_string(&abs).expect("read source");
        let lines: Vec<&str> = source.lines().collect();
        started.session.did_open(&abs).expect("didOpen");
        let file_uri = uri::path_to_uri(&abs).expect("uri");
        let outcome = started
            .session
            .document_symbol(&file_uri, spec.first_request_timeout)
            .await;
        let mut symbols = Vec::new();
        if let Ok(Some(async_lsp::lsp_types::DocumentSymbolResponse::Nested(nested))) =
            outcome.result
        {
            flatten_symbols(&nested, &mut symbols);
        }
        if symbols.is_empty() {
            report.files_without_symbols += 1;
            continue;
        }
        for position in index.by_file[rel.as_str()].iter() {
            let record = &index.records[*position].line;
            if record.col.is_none() {
                continue;
            }
            report.entities_with_col += 1;
            let Some(expected) =
                position::locate_name_in_lines(&lines, &record.name, record.l0, record.l1, lang)
            else {
                continue;
            };
            let candidates: Vec<&Symbol> = symbols
                .iter()
                .filter(|s| {
                    s.name == record.name
                        && s.first_line0 <= expected.line0
                        && expected.line0 <= s.last_line0
                })
                .collect();
            if candidates.is_empty() {
                continue;
            }
            report.compared += 1;
            if candidates.iter().any(|s| {
                s.selection_line0 == expected.line0 && s.selection_col == expected.col_utf16
            }) {
                report.matched += 1;
            } else if report.mismatches.len() < 25 {
                report.mismatches.push(format!(
                    "{rel}:{} {} ours {}:{} server {}",
                    record.l0,
                    record.name,
                    expected.line0,
                    expected.col_utf16,
                    candidates
                        .iter()
                        .map(|s| format!("{}:{}", s.selection_line0, s.selection_col))
                        .collect::<Vec<_>>()
                        .join("|")
                ));
            }
        }
    }
    stop(*started, std::time::Duration::from_secs(30)).await;
    report.accuracy_pct = 100.0 * report.matched as f64 / report.compared.max(1) as f64;
    report
}

fn l1(repo: &str, lang: LspLang, max_files: usize) {
    let report = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(l1_language(repo, lang, max_files));
    println!(
        "L1 {repo} ({}): {} files, {} entities with col, {} compared, {} matched ({:.2} %), {} files without symbols",
        report.lang,
        report.files_sampled,
        report.entities_with_col,
        report.compared,
        report.matched,
        report.accuracy_pct,
        report.files_without_symbols
    );
    for mismatch in &report.mismatches {
        println!("  mismatch: {mismatch}");
    }
    write_gate(&format!("l1_{}", report.lang), &report);
    assert!(
        report.compared >= 50,
        "too few entities compared: {}",
        report.compared
    );
    assert!(
        report.accuracy_pct >= 98.0,
        "L1 {}: {:.2} % < 98 %",
        report.lang,
        report.accuracy_pct
    );
}

#[test]
#[ignore = "L1: starts a real rust-analyzer on the sandbox `knot` (needs the P3 index and KNOT_LITE_LSP_CACHE_DIR)"]
fn l1_column_accuracy_rust() {
    l1("knot", LspLang::Rust, 150);
}

#[test]
#[ignore = "L1: starts a real typescript-language-server on the sandbox `knot-site` (needs the P3 index and KNOT_LITE_LSP_CACHE_DIR)"]
fn l1_column_accuracy_ts() {
    l1("knot-site", LspLang::Ts, 150);
}

#[test]
#[ignore = "L1: starts a real jdtls on the sandbox `spring-ai` (needs the P3 index and KNOT_LITE_LSP_CACHE_DIR; minutes)"]
fn l1_column_accuracy_java() {
    l1("spring-ai", LspLang::Java, 120);
}

// ---------------------------------------------------------------------------
// L7: pre-computed answers are fast and agree with the live ones
// ---------------------------------------------------------------------------

/// The containers a target's answer lists, as comparable keys.
fn answer_keys(target: &crate::query::callers::CallerTarget) -> BTreeSet<ContainerKey> {
    target
        .callers
        .0
        .iter()
        .flat_map(|(_, rows)| rows)
        .map(|row| ContainerKey {
            file: row.file.clone(),
            line: row.l0,
            name: row.name.clone(),
        })
        .collect()
}

/// `find_callers` for `query` over `index` (kinds `all`), with its wall time in milliseconds.
fn timed_callers(
    index: &LoadedIndex,
    settings: &LspSettings,
    root: &Path,
    query: &str,
    pool: Option<&crate::lsp::pool::LspPool>,
) -> (crate::query::callers::CallersResponse, u64) {
    let any = knot::cli_tools::kinds::KindFilter::parse(Some("all"));
    let started = std::time::Instant::now();
    let response = callers_response(&CallersContext {
        index,
        repo: "spring-ai",
        query,
        max_targets: 500,
        filter: &any,
        lsp: LspContext {
            settings,
            repo_root: root,
            pool,
        },
    });
    (
        response,
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    )
}

/// One pre-computed target: how its indexed answer compares with the live one.
struct IndexedTarget {
    id: String,
    fqn: String,
    indexed_ms: u64,
    keys: BTreeSet<ContainerKey>,
}

/// Ten pre-computed targets, evenly spread, answered from the index alone.
fn l7_indexed_answers(
    index: &LoadedIndex,
    settings: &LspSettings,
    root: &Path,
) -> Vec<IndexedTarget> {
    let mut ids: Vec<&String> = index
        .lsp_targets
        .values()
        .filter(|line| line.status == "ok")
        .map(|line| &line.id)
        .collect();
    ids.sort();
    let step = ids.len().div_ceil(10).max(1);
    let chosen: Vec<&String> = ids.into_iter().step_by(step).take(10).collect();
    assert!(!chosen.is_empty(), "no pre-computed Java target to measure");
    chosen
        .into_iter()
        .map(|id| {
            let record = &index.records[index.by_id[id]];
            let (response, indexed_ms) =
                timed_callers(index, settings, root, &record.line.fqn, None);
            let target = response
                .targets
                .iter()
                .find(|t| &t.id == id)
                .expect("target");
            assert_eq!(target.provenance, "lsp-indexed", "{}", record.line.fqn);
            IndexedTarget {
                id: id.clone(),
                fqn: record.line.fqn.clone(),
                indexed_ms,
                keys: answer_keys(target),
            }
        })
        .collect()
}

/// Live Java target known to be slow (18.8 s in L7) and the Rust target of the
/// polyglot fixture.
const L11_JAVA: &str = "org.springframework.ai.model.MutableResponseMetadata.put";
const L11_RUST: &str = "total_area";

/// What the parallel run of L11 produced.
struct L11Parallel {
    java_ms: u64,
    rust_rows: Vec<u64>,
    snapshots: Vec<(u64, String)>,
    java_end_at: Instant,
    rust_end_at: Instant,
}

/// Load the index of the repository named by `var` and create the pool. The pool
/// starts no server: the first query of a language does.
fn pool_for_repo(var: &str) -> (LoadedIndex, LspSettings, PathBuf, crate::lsp::pool::LspPool) {
    let root = std::env::var(var).unwrap_or_else(|_| panic!("{var} is required"));
    let root = PathBuf::from(root).canonicalize().expect("measured repo");
    let index = load_index(&IndexLayout::for_repo(&root)).expect("load the measured index");
    let settings = measurement_settings();
    let files: Vec<String> = index.file_sha16.keys().cloned().collect();
    let mut cfg = crate::config::test_support::config_for(root.clone());
    cfg.lsp = settings.clone();
    let pool = start_pool(&cfg, &files, CLI_READY_WAIT, Some(index.stamp.clone())).expect("pool");
    (index, settings, root, pool)
}

/// Warm both languages (the cold starts are not part of the assertion);
/// returns the Java warm-up wall time.
fn l11_warm_up(
    index: &LoadedIndex,
    settings: &LspSettings,
    root: &Path,
    pool: &crate::lsp::pool::LspPool,
) -> u64 {
    let _ = timed_callers(index, settings, root, L11_RUST, Some(pool));
    let (warm, warm_ms) = timed_callers(index, settings, root, L11_JAVA, Some(pool));
    assert_eq!(
        warm.targets.first().map(|t| t.provenance),
        Some("lsp-live"),
        "the Java warm-up is a live answer"
    );
    warm_ms
}

/// Run the slow Java query and the Rust queries in parallel, sampling the pool.
fn l11_run_parallel(
    index: &LoadedIndex,
    settings: &LspSettings,
    root: &Path,
    pool: &crate::lsp::pool::LspPool,
) -> L11Parallel {
    let java_done = AtomicBool::new(false);
    let mut snapshots: Vec<(u64, String)> = Vec::new();
    let mut rust_rows: Vec<u64> = Vec::new();
    let (java_ms, java_end_at, rust_end_at) = std::thread::scope(|scope| {
        let java = scope.spawn(|| {
            let (response, ms) = timed_callers(index, settings, root, L11_JAVA, Some(pool));
            assert_eq!(
                response.targets.first().map(|t| t.provenance),
                Some("lsp-live"),
                "the slow Java target is answered live"
            );
            java_done.store(true, Ordering::SeqCst);
            (ms, Instant::now())
        });
        std::thread::sleep(Duration::from_secs(1));
        let rust = scope.spawn(|| {
            let mut ms_list = Vec::new();
            for _ in 0..5 {
                let (response, ms) = timed_callers(index, settings, root, L11_RUST, Some(pool));
                assert_eq!(
                    response.targets.first().map(|t| t.provenance),
                    Some("lsp-live"),
                    "the Rust targets are answered live while Java runs"
                );
                ms_list.push(ms);
            }
            (ms_list, Instant::now())
        });

        // A snapshot every 200 ms while Java runs: it must not wait for it.
        while !java_done.load(Ordering::SeqCst) {
            let started = Instant::now();
            let snap = pool.snapshot();
            let state = snap
                .slots
                .get("java")
                .map(|slot| slot.state.clone())
                .unwrap_or_default();
            snapshots.push((
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                state,
            ));
            std::thread::sleep(Duration::from_millis(200));
        }

        let (java_ms, java_end) = java.join().expect("java thread");
        let (rust_times, rust_end) = rust.join().expect("rust thread");
        rust_rows.extend(rust_times);
        (java_ms, java_end, rust_end)
    });
    L11Parallel {
        java_ms,
        rust_rows,
        snapshots,
        java_end_at,
        rust_end_at,
    }
}

/// Persist the L11 measurement as `l11.json` in the measure-out directory.
fn l11_write_report(parallel: &L11Parallel) {
    let finished_before_java = parallel.rust_end_at <= parallel.java_end_at;
    let report = serde_json::json!({
        "java_ms": parallel.java_ms,
        "rust_ms": parallel.rust_rows,
        "snapshot_ms": parallel.snapshots.iter().map(|(ms, _)| ms).collect::<Vec<_>>(),
        "java_state_during_query": parallel.snapshots.first().map(|(_, s)| s.clone()),
        "java_finished_last": finished_before_java,
    });
    let text = serde_json::to_string_pretty(&report).expect("json");
    std::fs::write(measure_out_dir().join("l11.json"), text).expect("write");
}

/// The five invariants the L11 measurement must satisfy.
fn l11_check_invariants(parallel: &L11Parallel) {
    let worst_rust = parallel.rust_rows.iter().copied().max().unwrap_or(0);
    let worst_snapshot = parallel
        .snapshots
        .iter()
        .map(|(ms, _)| *ms)
        .max()
        .unwrap_or(0);
    let ready_while_ran = parallel.snapshots.iter().all(|(_, state)| state == "ready");
    let finished_before_java = parallel.rust_end_at <= parallel.java_end_at;
    assert!(
        worst_rust <= 2000,
        "a Rust query waited for Java: {worst_rust} ms ({:?})",
        parallel.rust_rows
    );
    assert!(
        finished_before_java,
        "the Rust queries outlived the Java target ({} ms)",
        parallel.java_ms
    );
    assert!(
        worst_snapshot <= 250,
        "snapshot waited for the running query: {worst_snapshot} ms"
    );
    assert!(
        ready_while_ran,
        "the Java slot is ready while it answers: {:?}",
        parallel.snapshots
    );
    assert!(
        parallel.java_ms >= 1500,
        "the Java target must actually be slow: {} ms",
        parallel.java_ms
    );
    assert!(
        parallel.snapshots.len() >= 3,
        "the snapshot loop ran: {:?}",
        parallel.snapshots
    );
}

#[test]
#[ignore = "L11: needs the polyglot repository (KNOT_LITE_L11_REPO) and real rust-analyzer + jdtls (minutes)"]
fn l11_languages_run_in_parallel() {
    let (index, settings, root, pool) = pool_for_repo("KNOT_LITE_L11_REPO");
    let warm_ms = l11_warm_up(&index, &settings, &root, &pool);
    let parallel = l11_run_parallel(&index, &settings, &root, &pool);
    drop(index);
    let _ = warm_ms;
    l11_write_report(&parallel);
    l11_check_invariants(&parallel);
}

/// The Gradle daemons running now: `(pid, carries a knot-lite owner mark)`. The
/// mark is on the command line (`gradle-owner-<token>` in the `-XX:ErrorFile`
/// path) and in the environment (`KNOT_LITE_LSP_OWNER`).
fn gradle_daemons() -> Vec<(u32, bool)> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_environ(UpdateKind::Always)
            .without_tasks(),
    );
    let owner_env = format!("{}=", crate::lsp::owner::OWNER_ENV);
    let mut found: Vec<(u32, bool)> = system
        .processes()
        .iter()
        .filter(|(_, p)| {
            p.cmd().iter().any(|arg| {
                arg.to_string_lossy()
                    .contains("org.gradle.launcher.daemon.bootstrap.GradleDaemon")
            })
        })
        .map(|(pid, p)| {
            let on_cmd = p
                .cmd()
                .iter()
                .any(|arg| arg.to_string_lossy().contains("gradle-owner-"));
            let in_env = p
                .environ()
                .iter()
                .any(|entry| entry.to_string_lossy().starts_with(&owner_env));
            (pid.as_u32(), on_cmd || in_env)
        })
        .collect();
    found.sort_unstable();
    found
}

/// A warm jdtls workspace with an unchanged build file never calls Gradle, so no
/// daemon would start: drop the project's `.gradle/` and touch its build files,
/// which makes jdtls synchronise the project again.
fn force_gradle_sync(project: &Path) {
    let _ = std::fs::remove_dir_all(project.join(".gradle"));
    for name in ["build.gradle", "settings.gradle"] {
        if let Ok(file) = std::fs::File::options()
            .write(true)
            .open(project.join(name))
        {
            file.set_modified(std::time::SystemTime::now())
                .expect("touch the build file");
        }
    }
}

/// Poll until no Gradle daemon is left beyond `baseline`; the seconds it took.
fn wait_daemons_gone(baseline: &[u32], limit: Duration) -> Option<f64> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if gradle_daemons()
            .iter()
            .all(|(pid, _)| baseline.contains(pid))
        {
            return Some(started.elapsed().as_secs_f64());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

#[test]
#[ignore = "L12: needs a Gradle project jdtls can import (KNOT_LITE_L12_REPO, indexed) and its Java entity (KNOT_LITE_L12_TARGET); real jdtls + Gradle (minutes)"]
fn l12_gradle_daemon_is_stopped() {
    let target = std::env::var("KNOT_LITE_L12_TARGET").expect("KNOT_LITE_L12_TARGET is required");
    force_gradle_sync(Path::new(
        &std::env::var("KNOT_LITE_L12_REPO").expect("KNOT_LITE_L12_REPO is required"),
    ));
    let baseline: Vec<u32> = gradle_daemons().into_iter().map(|(pid, _)| pid).collect();
    let (index, settings, root, pool) = pool_for_repo("KNOT_LITE_L12_REPO");

    let (response, live_ms) = timed_callers(&index, &settings, &root, &target, Some(&pool));
    let provenance = response.targets.first().map(|t| t.provenance);
    let marked: Vec<u32> = gradle_daemons()
        .into_iter()
        .filter(|(_, carries_mark)| *carries_mark)
        .map(|(pid, _)| pid)
        .collect();
    // Stopping the pool is what a CLI exit does.
    drop(pool);
    let gone_after = wait_daemons_gone(&baseline, Duration::from_secs(20));
    let report = serde_json::json!({
        "target": target,
        "provenance": provenance,
        "live_ms": live_ms,
        "baseline_daemons": baseline,
        "marked_daemons_while_running": marked,
        "daemons_gone_after_s": gone_after,
    });
    let text = serde_json::to_string_pretty(&report).expect("json");
    std::fs::write(measure_out_dir().join("l12_inprocess.json"), text).expect("write");

    assert_eq!(provenance, Some("lsp-live"), "the target is answered live");
    assert!(
        !marked.is_empty(),
        "no GradleDaemon carrying the owner mark was seen while jdtls ran"
    );
    assert!(
        gone_after.is_some(),
        "a marked GradleDaemon outlived the pool: {:?}",
        gradle_daemons()
    );
}

#[test]
#[ignore = "L7: needs the sandbox `spring-ai` indexed with the Java pre-computation, and starts a real jdtls (KNOT_LITE_LSP_CACHE_DIR; minutes)"]
fn l7_precompute() {
    let root = sandbox_path("spring-ai")
        .canonicalize()
        .expect("sandbox repo");
    let mut index = load_index(&IndexLayout::for_repo(&root)).expect("load the sandbox index");
    let settings = measurement_settings();
    let java = index
        .manifest
        .lsp
        .clone()
        .expect("the index was built with a pre-computation stage")
        .languages["java"]
        .clone();
    println!("L7 index: {java:?}");

    // 1. The indexed answers, with no pool at all.
    let indexed = l7_indexed_answers(&index, &settings, &root);

    // 2. The live answers of the same targets (the pre-computed data hidden).
    index.lsp_targets.clear();
    let mut cfg = crate::config::test_support::config_for(root.clone());
    cfg.lsp = settings.clone();
    let files: Vec<String> = index.file_sha16.keys().cloned().collect();
    let pool = start_pool(&cfg, &files, CLI_READY_WAIT, Some(index.stamp.clone())).expect("pool");
    let mut rows = Vec::new();
    let mut identical = 0;
    for entry in &indexed {
        let _ = timed_callers(&index, &settings, &root, &entry.fqn, Some(&pool));
        let (response, live_ms) = timed_callers(&index, &settings, &root, &entry.fqn, Some(&pool));
        let target = response
            .targets
            .iter()
            .find(|t| t.id == entry.id)
            .expect("target");
        let live_keys = answer_keys(target);
        let same = entry.keys == live_keys;
        identical += usize::from(same);
        println!(
            "  {}: indexed {} ms ({} containers), live {live_ms} ms ({}) [{}] identical={same}",
            entry.fqn,
            entry.indexed_ms,
            entry.keys.len(),
            live_keys.len(),
            target.provenance
        );
        rows.push(serde_json::json!({
            "fqn": entry.fqn, "indexed_ms": entry.indexed_ms, "live_ms": live_ms,
            "indexed": entry.keys.len(), "live": live_keys.len(),
            "live_provenance": target.provenance, "identical": same,
            "only_indexed": entry.keys.difference(&live_keys).map(container_label).collect::<Vec<_>>(),
            "only_live": live_keys.difference(&entry.keys).map(container_label).collect::<Vec<_>>(),
        }));
    }
    drop(pool);

    let indexed_ms: Vec<u64> = indexed.iter().map(|t| t.indexed_ms).collect();
    let p95 = percentile(&indexed_ms, 95.0);
    let identical_pct = 100.0 * identical as f64 / indexed.len() as f64;
    println!(
        "  => indexed p95 {p95} ms; parity {identical}/{} ({identical_pct:.0} %)",
        indexed.len()
    );
    write_gate(
        "l7_precompute",
        &serde_json::json!({
            "java": { "status": java.status, "targets_selected": java.targets_selected,
                      "targets_ok": java.targets_ok, "targets_failed": java.targets_failed,
                      "edges": java.edges, "ms": java.ms, "server": java.version },
            "measured_targets": indexed.len(),
            "indexed_p95_ms": p95, "parity_identical": identical, "parity_pct": identical_pct,
            "rows": rows,
        }),
    );
    assert!(p95 <= 200, "L7: indexed p95 {p95} ms > 200 ms");
    assert!(
        identical_pct >= 90.0,
        "L7: parity {identical_pct:.0} % < 90 %"
    );
}
