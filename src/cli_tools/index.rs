//! `index` subcommand execution and output formatting.

use anyhow::Context as _;
use serde_json::Value;

use crate::config::{Config, OutputFormat};
use crate::embed::EmbedBatch;
use crate::embed::knot_embedder::{KnotEmbedder, validate_model_name};
use crate::index::build::{BuildOptions, ProgressEvent, build_index};
use crate::index::select::select_files;
use crate::lsp::launch::start_pool;
use crate::lsp::pool::LspPool;
use crate::lsp::precompute::{PrecomputeOptions, PrecomputePolicy};
use crate::models::params::IndexParams;
use crate::query::render::{Column, RenderDoc, RenderSection, render, str_at, u64_at};
use crate::query::session::CLI_READY_WAIT;

/// `index` — build or refresh the repository index with knot's real embedder.
pub fn run_index(cfg: &Config, params: &IndexParams) -> anyhow::Result<Value> {
    let model_choice = params.embed_model.as_deref().unwrap_or(&cfg.embed.model);
    validate_model_name(model_choice)?;
    let mut embedder = KnotEmbedder::init(model_choice, &cfg.embed.cache_dir)?;
    let quiet = params.quiet;
    let pool = precompute_pool(cfg, params)?;
    run_index_with(cfg, params, &mut embedder, pool.as_ref(), &mut |event| {
        if !quiet {
            report_progress(&event);
        }
    })
}

/// The policy that runs: `none` when language servers are off.
fn effective_policy(cfg: &Config, params: &IndexParams) -> PrecomputePolicy {
    if cfg.lsp.enabled {
        params.lsp.policy
    } else {
        PrecomputePolicy::None
    }
}

/// The pool pre-computation asks, when there is anything to ask. Creating it
/// starts no server: a warm re-index whose languages are all reused never does.
fn precompute_pool(cfg: &Config, params: &IndexParams) -> anyhow::Result<Option<LspPool>> {
    let policy = effective_policy(cfg, params);
    if policy == PrecomputePolicy::None {
        return Ok(None);
    }
    // Only TypeScript and Rust servers use the file list (files opened up
    // front); the default policy is Java only.
    let files = if policy == PrecomputePolicy::All {
        select_files(&cfg.repo_path)?.parse_rel
    } else {
        Vec::new()
    };
    Ok(start_pool(cfg, &files, CLI_READY_WAIT, None))
}

/// Run `build_index` with an injected embedder, pool and progress listener (tests).
pub fn run_index_with(
    cfg: &Config,
    params: &IndexParams,
    embedder: &mut dyn EmbedBatch,
    pool: Option<&LspPool>,
    progress: &mut dyn FnMut(ProgressEvent),
) -> anyhow::Result<Value> {
    let policy = effective_policy(cfg, params);
    let opts = BuildOptions {
        force: params.force,
        commit_mode: params.commit_mode,
        jobs: params.jobs.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
        }),
        batch_size: params.batch_size,
        lsp: PrecomputeOptions {
            policy,
            ..params.lsp.clone()
        },
    };
    let mut report = build_index(cfg, embedder, &opts, pool, progress)?;
    if policy != params.lsp.policy {
        report.warnings.push(format!(
            "language-server pre-computation is off (policy {} ignored): KNOT_LITE_LSP=off",
            params.lsp.policy.as_str()
        ));
    }
    serde_json::to_value(&report).context("failed to serialize the index report")
}

/// Render a `knot-lite/index-run/v1` document.
pub fn format_index_output(value: &Value, format: OutputFormat) -> String {
    render(&index_doc(value), value, format)
}

fn report_progress(event: &ProgressEvent) {
    match event {
        ProgressEvent::Stage { name } => eprintln!("index: {name}…"),
        ProgressEvent::Embedded { done, total } => {
            eprintln!("index: embed {done}/{total}");
        }
        ProgressEvent::LspTargets { lang, done, total } => {
            eprintln!("index: lsp [{}] {done}/{total}", lang.dir_name());
        }
        ProgressEvent::Done { total_ms } => {
            let secs = (*total_ms as f64) / 1000.0;
            eprintln!("index: done in {secs:.1} s");
        }
    }
}

/// `computed — 198/200 targets, 15234 edges` for one pre-computed language.
fn lsp_language_text(row: &Value) -> String {
    let count = |key: &str| u64_at(row, &[key]).unwrap_or(0);
    let mut text = format!(
        "{} — {}/{} targets, {} edges",
        str_at(row, &["status"]).unwrap_or("-"),
        count("targets_ok"),
        count("targets_selected"),
        count("edges")
    );
    if let Some(diagnostic) = str_at(row, &["diagnostic"]).filter(|d| !d.is_empty()) {
        text.push_str(&format!(" ({diagnostic})"));
    }
    text
}

/// Build the format-agnostic document for a `knot-lite/index-run/v1` value.
pub fn index_doc(value: &Value) -> RenderDoc {
    let repo_name = str_at(value, &["repo", "name"]).unwrap_or("-");
    let repo_path = str_at(value, &["repo", "path"]).unwrap_or("-");
    let mode = str_at(value, &["commit_mode"]).unwrap_or("gitignore");
    let model = str_at(value, &["embedding", "model"]).unwrap_or("-");
    let dim = u64_at(value, &["embedding", "dim"]).unwrap_or(0);
    let encoding = str_at(value, &["embedding", "encoding"]).unwrap_or("-");
    let total_ms = u64_at(value, &["timings_ms", "total"]).unwrap_or(0);
    let secs = (total_ms as f64) / 1000.0;

    let summary = vec![
        format!("commit_mode: {mode}"),
        format!("embedding: {model} ({dim}, {encoding})"),
        format!("time: {secs:.1} s"),
    ];

    let mut rows = Vec::new();

    if let Some(counts) = value.get("counts").and_then(Value::as_object) {
        for (k, v) in counts {
            rows.push(vec![format!("counts.{k}"), v.to_string()]);
        }
    }
    if let Some(vectors) = value.get("vectors").and_then(Value::as_object) {
        for (k, v) in vectors {
            rows.push(vec![format!("vectors.{k}"), v.to_string()]);
        }
    }
    if let Some(files) = value.get("files").and_then(Value::as_object) {
        for (k, v) in files {
            rows.push(vec![format!("files.{k}"), v.to_string()]);
        }
    }
    if let Some(resolver) = value.get("resolver").and_then(Value::as_object) {
        for (k, v) in resolver {
            rows.push(vec![format!("resolver.{k}"), v.to_string()]);
        }
    }
    if let Some(timings) = value.get("timings_ms").and_then(Value::as_object) {
        for (k, v) in timings {
            rows.push(vec![format!("timings_ms.{k}"), v.to_string()]);
        }
    }
    if let Some(languages) = value.pointer("/lsp/languages").and_then(Value::as_object) {
        for (language, row) in languages {
            rows.push(vec![format!("lsp.{language}"), lsp_language_text(row)]);
        }
    }

    let notes = value
        .get("warnings")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();

    RenderDoc {
        title: format!("knot-lite index — repository '{repo_name}' ({repo_path})"),
        summary,
        sections: vec![RenderSection {
            caption: None,
            heading: None,
            columns: vec![Column::left("ITEM"), Column::right("VALUE")],
            rows,
        }],
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::config_for;
    use crate::embed::fake::FakeEmbedder;
    use crate::index::layout::IndexLayout;
    use crate::lsp::fake::{FakeServers, fixture_target_script};

    #[test]
    fn index_json_has_schema_and_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::index::fixture::write_fixture_repo(tmp.path()).expect("fixture");
        let cfg = config_for(tmp.path().to_path_buf());
        let params = IndexParams {
            force: false,
            commit_mode: None,
            embed_model: None,
            jobs: Some(2),
            batch_size: 16,
            quiet: true,
            lsp: PrecomputeOptions::default(),
        };
        let mut fake = FakeEmbedder::new(16);
        let mut events = Vec::new();
        let value =
            run_index_with(&cfg, &params, &mut fake, None, &mut |e| events.push(e)).expect("run");
        assert_eq!(value["schema"], crate::index::build::INDEX_RUN_SCHEMA_V1);
        assert!(value["counts"]["entities"].as_u64().unwrap() > 0);
        assert!(!events.is_empty());

        let table = format_index_output(&value, OutputFormat::Table);
        assert!(
            table.contains("knot-lite index — repository 'demo'"),
            "{table}"
        );
        assert!(
            table.contains("ITEM") && table.contains("counts.entities"),
            "{table}"
        );
    }

    #[test]
    fn index_twice_reports_reuse() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::index::fixture::write_fixture_repo(tmp.path()).expect("fixture");
        let cfg = config_for(tmp.path().to_path_buf());
        let params = IndexParams {
            force: false,
            commit_mode: None,
            embed_model: None,
            jobs: Some(2),
            batch_size: 16,
            quiet: true,
            lsp: PrecomputeOptions::default(),
        };
        let mut fake1 = FakeEmbedder::new(16);
        let val1 = run_index_with(&cfg, &params, &mut fake1, None, &mut |_| {}).expect("run 1");
        let embedded1 = val1["vectors"]["embedded"].as_u64().unwrap();

        let mut fake2 = FakeEmbedder::new(16);
        let val2 = run_index_with(&cfg, &params, &mut fake2, None, &mut |_| {}).expect("run 2");
        assert_eq!(val2["vectors"]["reused"].as_u64().unwrap(), embedded1);
        assert_eq!(val2["vectors"]["embedded"].as_u64().unwrap(), 0);
    }

    #[test]
    fn index_rejects_unknown_model_via_validate() {
        let err = validate_model_name("unknown-model-xyz").expect_err("must fail");
        assert!(err.to_string().contains("unknown-model-xyz"));
    }
    /// The fixture repository with language servers on and Rust pre-computed
    /// for every target with at least two callers (only `target`).
    fn precompute_setup() -> (tempfile::TempDir, Config, IndexParams) {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::index::fixture::write_fixture_repo(tmp.path()).expect("fixture");
        let mut cfg = config_for(tmp.path().to_path_buf());
        cfg.lsp.enabled = true;
        let params = IndexParams {
            force: false,
            commit_mode: None,
            embed_model: None,
            jobs: Some(2),
            batch_size: 16,
            quiet: true,
            lsp: PrecomputeOptions {
                policy: PrecomputePolicy::All,
                min_fanin: 2,
                ..PrecomputeOptions::default()
            },
        };
        (tmp, cfg, params)
    }

    /// Index with a fresh fake pool; returns the report and the servers it started.
    fn index_with_fake_pool(
        root: &std::path::Path,
        cfg: &Config,
        params: &IndexParams,
    ) -> (Value, usize) {
        let servers = FakeServers::new(fixture_target_script(root));
        let starts = servers.starts();
        let pool = servers.pool();
        let mut fake = FakeEmbedder::new(16);
        let value = run_index_with(cfg, params, &mut fake, Some(&pool), &mut |_| {}).expect("run");
        (value, starts.load(std::sync::atomic::Ordering::SeqCst))
    }

    #[test]
    fn index_warm_rerun_starts_no_server() {
        let (tmp, cfg, params) = precompute_setup();
        let (first, started) = index_with_fake_pool(tmp.path(), &cfg, &params);
        assert_eq!(first["lsp"]["languages"]["rust"]["status"], "computed");
        assert_eq!(first["counts"]["lsp_targets"], 1);
        assert!(
            first["counts"]["lsp_edges"].as_u64().unwrap() >= 3,
            "{first}"
        );
        assert_eq!(first["counts"]["lsp_edge_shards"], 2);
        assert_eq!(started, 1);

        let (second, started) = index_with_fake_pool(tmp.path(), &cfg, &params);
        assert_eq!(second["lsp"]["languages"]["rust"]["status"], "reused");
        assert_eq!(started, 0, "nothing changed: no server may start");
        assert_eq!(second["counts"]["lsp_edges"], first["counts"]["lsp_edges"]);
    }

    #[test]
    fn index_lsp_off_env_forces_none() {
        let (tmp, mut cfg, params) = precompute_setup();
        cfg.lsp.enabled = false;
        assert_eq!(effective_policy(&cfg, &params), PrecomputePolicy::None);

        let mut fake = FakeEmbedder::new(16);
        let value = run_index_with(&cfg, &params, &mut fake, None, &mut |_| {}).expect("run");
        assert!(value["lsp"].is_null(), "{value}");
        let warnings = value["warnings"].to_string();
        assert!(warnings.contains("KNOT_LITE_LSP=off"), "{warnings}");
        assert!(warnings.contains("policy all"), "{warnings}");
        let table = format_index_output(&value, OutputFormat::Table);
        assert!(table.contains("KNOT_LITE_LSP=off"), "{table}");
        assert!(!IndexLayout::for_repo(tmp.path()).lsp_edges().exists());
    }

    #[test]
    fn policy_none_removes_data_from_a_previous_index() {
        let (tmp, cfg, params) = precompute_setup();
        index_with_fake_pool(tmp.path(), &cfg, &params);
        let layout = IndexLayout::for_repo(tmp.path());
        assert!(layout.lsp_edges().exists() && layout.lsp_targets().exists());

        let none = IndexParams {
            lsp: PrecomputeOptions {
                policy: PrecomputePolicy::None,
                ..params.lsp.clone()
            },
            ..params
        };
        let mut fake = FakeEmbedder::new(16);
        let value = run_index_with(&cfg, &none, &mut fake, None, &mut |_| {}).expect("run");
        assert_eq!(value["lsp"]["policy"], "none");
        assert!(!layout.lsp_edges().exists(), "stale edges would be served");
        assert!(!layout.lsp_targets().exists());
        assert_eq!(value["counts"]["lsp_edges"], 0);

        // With nothing left to remove the stage does not even appear.
        let mut fake = FakeEmbedder::new(16);
        let value = run_index_with(&cfg, &none, &mut fake, None, &mut |_| {}).expect("run");
        assert!(value["lsp"].is_null());
    }

    #[test]
    fn index_table_lists_the_precomputed_languages() {
        let (tmp, cfg, params) = precompute_setup();
        let (value, _) = index_with_fake_pool(tmp.path(), &cfg, &params);
        let table = format_index_output(&value, OutputFormat::Table);
        assert!(table.contains("lsp.rust"), "{table}");
        assert!(table.contains("computed"), "{table}");
    }
}
