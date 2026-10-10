//! Query tool entry points (`run_*`) and their output formatters.
//!
//! Every runner has the same shape, `(cfg, services, params) → anyhow::Result<Value>`,
//! so the CLI and the MCP wrapper share one calling convention. An index that
//! cannot answer surfaces as a [`QueryUnavailable`] inside the `anyhow::Error`
//! (CLI exit code 2, MCP `isError`); everything else is a plain error.

use anyhow::Context as _;
use knot::cli_tools::kinds::{KindFilter, parse_kinds};
use serde::Serialize;
use serde_json::Value;

use crate::config::{Config, OutputFormat};
use crate::error::IndexUnavailable;
use crate::index::layout::IndexLayout;
use crate::index::load::LoadedIndex;
use crate::models::params::{CallersParams, ExploreParams, FilesParams, SearchParams};
use crate::query::callers::{
    CallersContext, DEFAULT_MAX_TARGETS, LspContext, MAX_TARGETS_CAP, callers_doc, callers_response,
};
use crate::query::explore::{explore_doc, explore_response};
use crate::query::files::{files_doc, files_response};
use crate::query::filters::{PathFilter, clamp_i64};
use crate::query::fusion::KEYWORD_WEIGHT;
use crate::query::render::render;
use crate::query::search::{
    DEFAULT_MAX_RESULTS, MAX_RESULTS_CAP, SearchRequest, search_doc, search_index,
};
use crate::query::session::QueryServices;

const SEARCH_TOOL: &str = "search_hybrid_context";
const CALLERS_TOOL: &str = "find_callers";
const EXPLORE_TOOL: &str = "explore_file";
const FILES_TOOL: &str = "list_files";

/// Wrap an [`IndexUnavailable`] as the caller-facing error of `tool`.
fn unavailable_error(reason: IndexUnavailable, tool: &str, cfg: &Config) -> anyhow::Error {
    anyhow::Error::new(reason.for_tool(tool, &cfg.repo_name))
}

/// Load (or reuse) the index and run `body` on it; an unusable index becomes a
/// [`QueryUnavailable`] for `tool`.
fn with_index<T>(
    cfg: &Config,
    services: &QueryServices,
    tool: &str,
    body: impl FnOnce(&LoadedIndex) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let layout = IndexLayout::for_repo(&cfg.repo_path);
    let index = services
        .get_index(&layout)
        .map_err(|reason| unavailable_error(reason, tool, cfg))?;
    body(&index)
}

/// Serialise a typed response into the JSON document the formatters read.
fn to_json(response: &impl Serialize, tool: &str) -> anyhow::Result<Value> {
    serde_json::to_value(response)
        .with_context(|| format!("failed to serialize the {tool} response"))
}

/// `search_hybrid_context` — hybrid (or, with `KNOT_LITE_SEARCH=vector`, cosine)
/// ranking over entities and tier-1 records.
pub fn run_search_hybrid_context(
    cfg: &Config,
    services: &QueryServices,
    params: &SearchParams,
) -> anyhow::Result<Value> {
    let max_results = clamp_i64(params.max_results, DEFAULT_MAX_RESULTS, MAX_RESULTS_CAP);
    let path = PathFilter::parse(params.path.as_deref().unwrap_or(""));
    let kinds = parse_kinds(params.kinds.as_deref());
    with_index(cfg, services, SEARCH_TOOL, |index| {
        let query_vector = services
            .embedder
            .embed_query(&index.manifest.embedding, &params.query)
            .map_err(|reason| unavailable_error(reason, SEARCH_TOOL, cfg))?;
        let request = SearchRequest {
            query: &params.query,
            max_results,
            kinds: &kinds,
            path: path.as_ref(),
            mode: cfg.search.mode,
            keyword_weight: KEYWORD_WEIGHT,
        };
        let response = search_index(index, &cfg.repo_name, &request, &query_vector);
        to_json(&response, SEARCH_TOOL)
    })
}

/// Render a `knot-lite/search/v2` document.
pub fn format_search_output(value: &Value, format: OutputFormat) -> String {
    render(&search_doc(value, format), value, format)
}

/// `find_callers` — who references an entity (heuristic edges, container granularity).
pub fn run_find_callers(
    cfg: &Config,
    services: &QueryServices,
    params: &CallersParams,
) -> anyhow::Result<Value> {
    let max_targets = clamp_i64(params.max_targets, DEFAULT_MAX_TARGETS, MAX_TARGETS_CAP);
    let filter = KindFilter::parse(params.kinds.as_deref());
    with_index(cfg, services, CALLERS_TOOL, |index| {
        let pool = params
            .live_lsp
            .then(|| services.lsp_pool(cfg, index))
            .flatten();
        let response = callers_response(&CallersContext {
            index,
            repo: &cfg.repo_name,
            query: &params.entity_name,
            max_targets,
            filter: &filter,
            lsp: LspContext {
                settings: &cfg.lsp,
                repo_root: &cfg.repo_path,
                pool: pool.as_deref(),
            },
        });
        to_json(&response, CALLERS_TOOL)
    })
}

/// Render a `knot-lite/callers/v2` document.
pub fn format_callers_output(value: &Value, format: OutputFormat) -> String {
    render(&callers_doc(value, format), value, format)
}

/// `explore_file` — the entities of one file, grouped by kind family.
pub fn run_explore_file(
    cfg: &Config,
    services: &QueryServices,
    params: &ExploreParams,
) -> anyhow::Result<Value> {
    with_index(cfg, services, EXPLORE_TOOL, |index| {
        let response = explore_response(
            index,
            &cfg.repo_name,
            &cfg.repo_path,
            &params.file_path,
            params.include_markup,
        );
        to_json(&response, EXPLORE_TOOL)
    })
}

/// Render a `knot-lite/explore/v1` document.
pub fn format_explore_output(value: &Value, format: OutputFormat) -> String {
    render(&explore_doc(value, format), value, format)
}

/// `list_files` — the files in the index, optionally under a directory prefix or matching a glob.
pub fn run_list_files(
    cfg: &Config,
    services: &QueryServices,
    params: &FilesParams,
) -> anyhow::Result<Value> {
    let prefix = PathFilter::parse(params.path.as_deref().unwrap_or(""));
    with_index(cfg, services, FILES_TOOL, |index| {
        let response = files_response(index, &cfg.repo_name, prefix.as_ref());
        to_json(&response, FILES_TOOL)
    })
}

/// Render a `knot-lite/files/v1` document.
pub fn format_files_output(value: &Value, format: OutputFormat) -> String {
    render(&files_doc(value, format), value, format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::config_for;
    use crate::embed::fake::FakeEmbedder;
    use crate::error::QueryUnavailable;
    use crate::index::build::test_support::{DIM, fixture, run};
    use crate::query::session::EmbedderFactory;
    use std::sync::Arc;

    fn services() -> QueryServices {
        QueryServices::with_factory(Arc::new(|_model: &str| {
            Ok(Box::new(FakeEmbedder::new(DIM)) as Box<dyn crate::embed::EmbedBatch + Send>)
        }))
    }

    fn failing_services() -> QueryServices {
        let factory: EmbedderFactory =
            Arc::new(|_model: &str| anyhow::bail!("model files missing"));
        QueryServices::with_factory(factory)
    }

    fn search(query: &str) -> SearchParams {
        SearchParams {
            query: query.to_owned(),
            max_results: None,
            kinds: None,
            path: None,
        }
    }

    fn indexed() -> (tempfile::TempDir, Config) {
        let (tmp, cfg) = fixture();
        run(&cfg);
        (tmp, cfg)
    }

    fn unavailable_of(error: &anyhow::Error) -> &QueryUnavailable {
        error
            .downcast_ref::<QueryUnavailable>()
            .expect("QueryUnavailable")
    }

    fn files_of(value: &Value) -> Vec<String> {
        value["results"]
            .as_array()
            .expect("results")
            .iter()
            .map(|hit| hit["file"].as_str().expect("file").to_owned())
            .collect()
    }

    /// The text embedded for the first window of the fixture's Makefile.
    const MAKEFILE_CHUNK: &str = "Makefile (lines 1-2)\nall:\n\techo build";

    #[test]
    fn vector_mode_finds_exact_text_first() {
        let (_tmp, mut cfg) = indexed();
        cfg.search.mode = crate::config::SearchMode::Vector;
        let value =
            run_search_hybrid_context(&cfg, &services(), &search(MAKEFILE_CHUNK)).expect("search");
        let top = &value["results"][0];
        assert_eq!(top["file"], "Makefile");
        assert_eq!(top["kind"], "chunk");
        assert_eq!(top["rank"], 1);
        let score = top["score"].as_f64().expect("score");
        assert!(score > 0.98, "an exact text scores ~1.0, got {score}");
        assert_eq!(top["cosine"], top["score"]);
        assert_eq!(top["keyword"], Value::Null);
        assert_eq!(top["channels"], serde_json::json!(["cosine"]));
        assert_eq!(value["schema"], "knot-lite/search/v2");
        assert_eq!(value["provenance"], "vector");
        assert_eq!(value["ranking"]["mode"], "vector");
        assert_eq!(value["ranking"]["pool"], Value::Null);
        assert_eq!(value["repo"], "demo");
    }

    #[test]
    fn hybrid_mode_recalls_exact_text_with_both_channels() {
        let (_tmp, mut cfg) = indexed();
        cfg.search.mode = crate::config::SearchMode::Hybrid;
        let value =
            run_search_hybrid_context(&cfg, &services(), &search(MAKEFILE_CHUNK)).expect("search");
        assert_eq!(value["provenance"], "hybrid");
        assert_eq!(value["ranking"]["mode"], "hybrid");
        assert_eq!(value["ranking"]["lexicon"], "full");
        assert_eq!(value["ranking"]["keyword_weight"], 1.0);
        let hits = value["results"].as_array().expect("results");
        let chunk = hits
            .iter()
            .find(|hit| hit["file"] == "Makefile" && hit["l0"] == 1)
            .expect("the exact chunk is among the results");
        assert!(chunk["cosine"].as_f64().expect("cosine") > 0.98);
        assert!(chunk["keyword"].as_f64().expect("keyword") > 0.0);
        let channels = chunk["channels"].as_array().expect("channels");
        assert!(channels.contains(&serde_json::json!("cosine")), "{chunk}");
        assert!(channels.contains(&serde_json::json!("keyword")), "{chunk}");
    }

    #[test]
    fn search_response_reports_filters_candidates_and_index() {
        let (_tmp, cfg) = indexed();
        let value =
            run_search_hybrid_context(&cfg, &services(), &search("anything")).expect("search");
        assert_eq!(value["max_results"], 5);
        assert_eq!(value["results"].as_array().expect("results").len(), 5);
        assert!(value["candidates"].as_u64().expect("candidates") >= 19);
        assert_eq!(value["filters"]["kinds"], serde_json::json!([]));
        assert_eq!(value["filters"]["path"], Value::Null);
        assert_eq!(value["index"]["model"], "knot-lite-fake-sha256");
        assert!(
            value["index"]["updated_at"]
                .as_str()
                .expect("updated_at")
                .ends_with('Z')
        );
        assert_eq!(
            value["notes"][0],
            "Pure vector ranking (cosine over int8 vectors); no keyword channel or re-ranking."
        );
        assert_eq!(
            value["provenance"], "vector",
            "vector is the default (P5-D18)"
        );
    }

    #[test]
    fn search_kinds_filter_rust_function() {
        let (_tmp, cfg) = indexed();
        let params = SearchParams {
            kinds: Some("rust_function".to_owned()),
            max_results: Some(50),
            ..search("target")
        };
        let value = run_search_hybrid_context(&cfg, &services(), &params).expect("search");
        let hits = value["results"].as_array().expect("results");
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|hit| hit["kind"] == "rust_function"),
            "{hits:?}"
        );
        assert_eq!(
            value["filters"]["kinds"],
            serde_json::json!(["rust_function"])
        );
    }

    #[test]
    fn search_kinds_chunk_returns_only_tier1() {
        let (_tmp, cfg) = indexed();
        let params = SearchParams {
            kinds: Some("chunk".to_owned()),
            max_results: Some(50),
            ..search("echo")
        };
        let value = run_search_hybrid_context(&cfg, &services(), &params).expect("search");
        let hits = value["results"].as_array().expect("results");
        assert_eq!(hits.len(), 3, "2 windows of run.sh + 1 of the Makefile");
        assert!(hits.iter().all(|hit| hit["kind"] == "chunk"));
        let files = files_of(&value);
        assert!(
            files
                .iter()
                .all(|f| f == "Makefile" || f == "scripts/run.sh"),
            "{files:?}"
        );
    }

    #[test]
    fn search_path_prefix_boundary() {
        let (tmp, cfg) = indexed();
        // A sibling directory that merely shares the textual prefix.
        std::fs::create_dir_all(tmp.path().join("srcx")).expect("mkdir");
        std::fs::write(tmp.path().join("srcx/other.rs"), "pub fn other() {}\n").expect("write");
        run(&cfg);

        let everything = SearchParams {
            kinds: Some("rust_function".to_owned()),
            max_results: Some(100),
            ..search("fn")
        };
        let unfiltered = run_search_hybrid_context(&cfg, &services(), &everything).expect("search");
        assert!(
            files_of(&unfiltered).contains(&"srcx/other.rs".to_owned()),
            "the sibling is indexed"
        );

        let filtered = SearchParams {
            path: Some("src/".to_owned()),
            ..everything
        };
        let value = run_search_hybrid_context(&cfg, &services(), &filtered).expect("search");
        let files = files_of(&value);
        assert!(!files.is_empty());
        assert!(files.iter().all(|f| f.starts_with("src/")), "{files:?}");
        assert_eq!(value["filters"]["path"], "src");
    }

    #[test]
    fn search_reports_path_mode() {
        let (_tmp, cfg) = indexed();
        let params = SearchParams {
            path: Some("src/*.rs".to_owned()),
            max_results: Some(100),
            ..search("x")
        };
        let value = run_search_hybrid_context(&cfg, &services(), &params).expect("glob");
        let files = files_of(&value);
        assert!(!files.is_empty());
        assert!(
            files
                .iter()
                .all(|f| f.starts_with("src/") && f.ends_with(".rs")),
            "{files:?}"
        );
        assert_eq!(value["filters"]["path"], "src/*.rs");
        assert_eq!(value["filters"]["path_mode"], "glob");
        let prefix = run_search_hybrid_context(
            &cfg,
            &services(),
            &SearchParams {
                path: Some("src".to_owned()),
                ..search("x")
            },
        )
        .expect("prefix");
        assert_eq!(prefix["filters"]["path_mode"], "prefix");
        let table = format_search_output(&value, OutputFormat::Table);
        assert!(table.contains("path: src/*.rs (glob)"), "{table}");
    }

    #[test]
    fn search_no_match_note() {
        let (_tmp, cfg) = indexed();
        let params = SearchParams {
            path: Some("src/**/*.zig".to_owned()),
            ..search("x")
        };
        let value = run_search_hybrid_context(&cfg, &services(), &params).expect("search");
        assert!(files_of(&value).is_empty());
        let notes: Vec<&str> = value["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            notes.contains(&"path 'src/**/*.zig' matched no indexed file"),
            "{notes:?}"
        );
    }

    #[test]
    fn vector_mode_equals_phase2_ranking() {
        use crate::embed::EmbedBatch as _;
        use crate::index::layout::IndexLayout;
        use crate::index::load::load_index;
        use crate::query::search::top_k;

        let (tmp, mut cfg) = indexed();
        cfg.search.mode = crate::config::SearchMode::Vector;
        let params = SearchParams {
            max_results: Some(10),
            ..search("helper widget")
        };
        let value = run_search_hybrid_context(&cfg, &services(), &params).expect("search");
        let index = load_index(&IndexLayout::for_repo(tmp.path())).expect("load");
        let query = FakeEmbedder::new(DIM)
            .embed_query("helper widget")
            .expect("embed");
        let rows: Vec<usize> = (0..index.matrix.rows()).collect();
        let phase2: Vec<String> = top_k(&index.matrix, &rows, &query, 10)
            .into_iter()
            .map(|(row, _)| index.records[index.matrix.record[row]].line.id.clone())
            .collect();
        let ids: Vec<String> = value["results"]
            .as_array()
            .expect("results")
            .iter()
            .map(|hit| hit["id"].as_str().expect("id").to_owned())
            .collect();
        assert_eq!(ids, phase2);
    }

    #[test]
    fn search_v2_shape_and_internal_fields_never_serialised() {
        let (_tmp, mut cfg) = indexed();
        cfg.search.mode = crate::config::SearchMode::Hybrid;
        let params = SearchParams {
            max_results: Some(8),
            ..search("helper calls target")
        };
        let value = run_search_hybrid_context(&cfg, &services(), &params).expect("search");
        assert_eq!(value["schema"], "knot-lite/search/v2");
        let keys: Vec<&str> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "schema",
            "knot_lite_version",
            "repo",
            "query",
            "provenance",
            "ranking",
            "index",
            "filters",
            "max_results",
            "candidates",
            "results",
            "notes",
        ] {
            assert!(keys.contains(&key), "{key} missing: {keys:?}");
        }
        assert!(value["ranking"]["pool"].as_u64().expect("pool") > 0);
        let hits = value["results"].as_array().expect("results");
        assert_eq!(hits.len(), 8);
        for (rank, hit) in hits.iter().enumerate() {
            assert_eq!(hit["rank"], rank + 1);
            assert!(hit["cosine"].is_f64(), "{hit}");
            assert!(!hit["channels"].as_array().expect("channels").is_empty());
        }
        let scores: Vec<f64> = hits
            .iter()
            .filter_map(|hit| hit["score"].as_f64())
            .collect();
        assert!(
            scores.windows(2).all(|pair| pair[0] >= pair[1]),
            "{scores:?}"
        );
        let text = value.to_string();
        assert!(
            !text.contains("\"_kl\"") && !text.contains("\"_channel\""),
            "{text}"
        );
    }

    #[test]
    fn search_clamps_max_results() {
        let (_tmp, cfg) = indexed();
        let mut params = search("x");
        params.max_results = Some(0);
        let one = run_search_hybrid_context(&cfg, &services(), &params).expect("search");
        assert_eq!(
            (
                one["max_results"].clone(),
                one["results"].as_array().expect("r").len()
            ),
            (serde_json::json!(1), 1)
        );

        params.max_results = Some(1000);
        let many = run_search_hybrid_context(&cfg, &services(), &params).expect("search");
        assert_eq!(many["max_results"], 100);
        let total = many["candidates"].as_u64().expect("candidates") as usize;
        assert_eq!(many["results"].as_array().expect("r").len(), total.min(100));
    }

    #[test]
    fn search_on_absent_index_is_not_indexed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = config_for(tmp.path().to_path_buf());
        let error = run_search_hybrid_context(&cfg, &services(), &search("x")).expect_err("absent");
        let text = error.to_string();
        assert!(
            matches!(unavailable_of(&error), QueryUnavailable::NotIndexed { .. }),
            "{text}"
        );
        assert!(
            text.starts_with("search_hybrid_context: knot-lite has no index"),
            "{text}"
        );
    }

    #[test]
    fn search_with_unloadable_model_is_model_unavailable() {
        let (_tmp, cfg) = indexed();
        let error =
            run_search_hybrid_context(&cfg, &failing_services(), &search("x")).expect_err("model");
        match unavailable_of(&error) {
            QueryUnavailable::ModelUnavailable {
                model,
                error,
                accepted,
                ..
            } => {
                assert_eq!(model, "knot-lite-fake-sha256");
                assert!(error.contains("model files missing"), "{error}");
                assert!(accepted.contains("AllMiniLML6V2"), "{accepted}");
            }
            other => panic!("expected ModelUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn search_renderers_snapshot() {
        let (_tmp, cfg) = indexed();
        let value =
            run_search_hybrid_context(&cfg, &services(), &search(MAKEFILE_CHUNK)).expect("search");

        let table = format_search_output(&value, OutputFormat::Table);
        assert!(
            table.starts_with("knot-lite search — repository 'demo'\nquery: "),
            "{table}"
        );
        for header in ["SCORE", "COS", "KW", "KIND", "NAME", "LOCATION", "CALLERS"] {
            assert!(table.contains(header), "{header} missing in {table}");
        }
        assert!(table.contains("Makefile:1-2"), "{table}");

        let markdown = format_search_output(&value, OutputFormat::Markdown);
        assert!(markdown.starts_with("## knot-lite search"), "{markdown}");
        assert!(
            markdown
                .contains("| # | SCORE | COS | KW | KIND | NAME | LOCATION | CALLERS | CHANNELS |"),
            "{markdown}"
        );

        let json: Value =
            serde_json::from_str(&format_search_output(&value, OutputFormat::Json)).expect("json");
        assert_eq!(json["schema"], "knot-lite/search/v2");
    }

    #[test]
    fn search_is_deterministic() {
        let (_tmp, cfg) = indexed();
        let services = services();
        let first =
            run_search_hybrid_context(&cfg, &services, &search("same query")).expect("search");
        let second =
            run_search_hybrid_context(&cfg, &services, &search("same query")).expect("search");
        assert_eq!(first, second);
        assert_eq!(
            services.index.loads(),
            1,
            "the second query reuses the loaded index"
        );
        assert_eq!(services.embedder.inits(), 1);
    }

    fn explore_params(file: &str) -> ExploreParams {
        ExploreParams {
            file_path: file.to_owned(),
            include_markup: false,
        }
    }

    /// Every runner, so one test can assert the same diagnosis for all of them.
    fn all_runner_errors(cfg: &Config) -> Vec<(&'static str, anyhow::Error)> {
        let services = services();
        vec![
            (
                SEARCH_TOOL,
                run_search_hybrid_context(cfg, &services, &search("x")).unwrap_err(),
            ),
            (
                CALLERS_TOOL,
                run_find_callers(cfg, &services, &callers("x")).unwrap_err(),
            ),
            (
                EXPLORE_TOOL,
                run_explore_file(cfg, &services, &explore_params("src/lib.rs")).unwrap_err(),
            ),
            (
                FILES_TOOL,
                run_list_files(cfg, &services, &FilesParams::default()).unwrap_err(),
            ),
        ]
    }

    #[test]
    fn every_tool_reports_not_indexed_on_an_empty_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = config_for(tmp.path().to_path_buf());
        for (tool, error) in all_runner_errors(&cfg) {
            let text = error.to_string();
            assert!(
                matches!(unavailable_of(&error), QueryUnavailable::NotIndexed { .. }),
                "{text}"
            );
            assert!(text.starts_with(&format!("{tool}: ")), "{text}");
            assert!(text.contains("`knot-lite index`"), "{text}");
        }
    }

    #[test]
    fn every_tool_reports_incomplete_and_unsupported_states() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = config_for(tmp.path().to_path_buf());
        let dir = tmp.path().join(".knot-lite");
        std::fs::create_dir_all(&dir).expect("mkdir");

        std::fs::write(dir.join("manifest.prev.toml"), "x = 1\n").expect("write");
        for (_, error) in all_runner_errors(&cfg) {
            assert!(
                matches!(unavailable_of(&error), QueryUnavailable::Incomplete { .. }),
                "{error}"
            );
        }
        std::fs::remove_file(dir.join("manifest.prev.toml")).expect("remove");

        std::fs::write(
            dir.join("manifest.toml"),
            "schema = \"knot-lite/manifest/v2\"\nknot_lite_version = \"9.9.9\"\n",
        )
        .expect("write");
        for (_, error) in all_runner_errors(&cfg) {
            let text = error.to_string();
            assert!(
                matches!(unavailable_of(&error), QueryUnavailable::Unsupported { .. }),
                "{text}"
            );
            assert!(text.contains("knot-lite/manifest/v1"), "{text}");
        }
    }

    #[test]
    fn explore_runner_resolves_paths_and_renders() {
        let (tmp, cfg) = indexed();
        let by_suffix =
            run_explore_file(&cfg, &services(), &explore_params("util.rs")).expect("explore");
        assert_eq!(by_suffix["file"], "src/util.rs");
        assert_eq!(by_suffix["resolution"], "suffix");
        let absolute = tmp.path().join("src/lib.rs").to_string_lossy().into_owned();
        let by_path =
            run_explore_file(&cfg, &services(), &explore_params(&absolute)).expect("explore");
        assert_eq!(by_path["file"], "src/lib.rs");

        let table = format_explore_output(&by_path, OutputFormat::Table);
        assert!(
            table.starts_with("knot-lite explore — repository 'demo'"),
            "{table}"
        );
        assert!(
            table.contains("SIGNATURE") && table.contains("target"),
            "{table}"
        );
        let markdown = format_explore_output(&by_path, OutputFormat::Markdown);
        assert!(markdown.starts_with("## knot-lite explore"), "{markdown}");
        assert!(markdown.contains("### callables (2)"), "{markdown}");
        let missing =
            run_explore_file(&cfg, &services(), &explore_params("nope.rs")).expect("explore");
        assert_eq!(missing["resolution"], "not_found");
    }

    #[test]
    fn files_runner_lists_prefixes_and_globs() {
        let (_tmp, cfg) = indexed();
        let all = run_list_files(&cfg, &services(), &FilesParams::default()).expect("files");
        assert_eq!(all["total"], 7);
        let src = run_list_files(
            &cfg,
            &services(),
            &FilesParams {
                path: Some("src".to_owned()),
            },
        )
        .expect("files");
        assert_eq!(src["total"], 2);
        assert_eq!(src["path"], "src");

        let glob = run_list_files(
            &cfg,
            &services(),
            &FilesParams {
                path: Some("**/*.rs".to_owned()),
            },
        )
        .expect("glob");
        assert_eq!(glob["total"], 2);
        assert_eq!(glob["path_mode"], "glob");
        assert_eq!(src["path_mode"], "prefix");

        let table = format_files_output(&src, OutputFormat::Table);
        assert!(
            table.contains("PATH") && table.contains("src/lib.rs"),
            "{table}"
        );
        let markdown = format_files_output(&src, OutputFormat::Markdown);
        assert!(
            markdown.contains("| PATH | LANGUAGE | ENTITIES | CHUNKS |"),
            "{markdown}"
        );
        let empty = run_list_files(
            &cfg,
            &services(),
            &FilesParams {
                path: Some("nope".to_owned()),
            },
        )
        .expect("files");
        assert!(format_files_output(&empty, OutputFormat::Markdown).contains("_No results._"));
    }

    fn callers(name: &str) -> CallersParams {
        CallersParams {
            entity_name: name.to_owned(),
            max_targets: None,
            kinds: None,
            live_lsp: true,
        }
    }

    #[test]
    fn callers_runner_answers_over_the_fixture_index() {
        let (_tmp, cfg) = indexed();
        let value = run_find_callers(&cfg, &services(), &callers("target")).expect("callers");
        assert_eq!(value["schema"], "knot-lite/callers/v2");
        assert_eq!(value["targets"][0]["caller_count"], 3);
        let table = format_callers_output(&value, OutputFormat::Table);
        assert!(
            table.starts_with("knot-lite callers — repository 'demo'"),
            "{table}"
        );
        assert!(table.contains("REL") && table.contains("helper"), "{table}");
        let markdown = format_callers_output(&value, OutputFormat::Markdown);
        assert!(markdown.starts_with("## knot-lite callers"), "{markdown}");
        assert!(markdown.contains("### rust_function target"), "{markdown}");
    }

    #[test]
    fn no_lsp_never_creates_a_pool_and_reports_the_targets_as_disabled() {
        let (_tmp, mut cfg) = indexed();
        // Servers are allowed; `--no-lsp` is what keeps this call away from them.
        cfg.lsp.enabled = true;
        let services = services();
        let params = CallersParams {
            live_lsp: false,
            ..callers("target")
        };
        let value = run_find_callers(&cfg, &services, &params).expect("callers");
        assert!(services.lsp_runtime().is_none(), "no pool may be created");
        assert_eq!(value["provenance"], "heuristic");
        assert_eq!(value["targets"][0]["lsp"]["status"], "disabled");
        assert_eq!(value["targets"][0]["caller_count"], 3);
        assert!(value["lsp"].is_null());
    }

    #[test]
    fn callers_runner_clamps_max_targets_and_honours_kinds() {
        let (_tmp, cfg) = indexed();
        let mut params = callers("Install");
        let hidden = run_find_callers(&cfg, &services(), &params).expect("callers");
        assert_eq!(
            hidden["resolution"]["targets_total"], 0,
            "docs are hidden by default"
        );
        params.kinds = Some("all".to_owned());
        params.max_targets = Some(0);
        let shown = run_find_callers(&cfg, &services(), &params).expect("callers");
        assert_eq!(shown["resolution"]["targets_total"], 1);
        assert_eq!(shown["resolution"]["kind_filter"], "any");
        assert_eq!(
            shown["targets"].as_array().expect("targets").len(),
            1,
            "0 is clamped to 1"
        );
    }

    #[test]
    fn callers_runner_on_absent_index_is_not_indexed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = config_for(tmp.path().to_path_buf());
        let error = run_find_callers(&cfg, &services(), &callers("x")).expect_err("absent");
        assert!(matches!(
            unavailable_of(&error),
            QueryUnavailable::NotIndexed { .. }
        ));
        assert!(
            error
                .to_string()
                .starts_with("find_callers: knot-lite has no index"),
            "{error}"
        );
    }
}
