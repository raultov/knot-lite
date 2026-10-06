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

use knot::models::ParsedEntity;
use knot::pipeline::parser::{ParseConfig, parse_files_stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::capability::languages::LspLang;
use crate::config::LspOverrides;
use crate::embed::EmbedBatch;
use crate::embed::knot_embedder::KnotEmbedder;
use crate::embed::settings::resolve_embed_settings;
use crate::graph::containers::ContainerKey;
use crate::index::hash::{finish_hex, sha256_hex};
use crate::index::layout::IndexLayout;
use crate::index::load::{LoadedIndex, load_index};
use crate::lsp::preopen::PreopenPlan;
use crate::lsp::process::{StartContext, StartOutcome, start_or_degrade, stop};
use crate::lsp::readiness::wait_ready;
use crate::lsp::settings::{resolve_lsp_settings, scratch_dir};
use crate::lsp::spec::{SpecInputs, resolve_program, spec_for};
use crate::lsp::{position, uri};
use crate::query::search::top_k;

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
            let response = crate::query::callers::callers_response(
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
    let spec = spec_for(&SpecInputs {
        lang: LspLang::Rust,
        program: &program,
        repo_root: repo,
        scratch,
        build_exec: true,
        preopen: &PreopenPlan::default(),
    });
    let ctx = StartContext {
        repo_root: repo,
        scratch,
        logs_keep: 5,
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
