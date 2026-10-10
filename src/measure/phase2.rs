//! Phase 2 gates M1, M4, M5, M6 (PLAN_PHASE2_INDEX.md §10).

use super::*;

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
