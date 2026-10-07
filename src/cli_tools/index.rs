//! `index` subcommand execution and output formatting.

use anyhow::Context as _;
use serde_json::Value;

use crate::config::{Config, OutputFormat};
use crate::embed::EmbedBatch;
use crate::embed::knot_embedder::{KnotEmbedder, validate_model_name};
use crate::index::build::{BuildOptions, ProgressEvent, build_index};
use crate::models::params::IndexParams;
use crate::query::render::{Column, RenderDoc, RenderSection, render, str_at, u64_at};

/// `index` — build or refresh the repository index with knot's real embedder.
pub fn run_index(cfg: &Config, params: &IndexParams) -> anyhow::Result<Value> {
    let model_choice = params.embed_model.as_deref().unwrap_or(&cfg.embed.model);
    validate_model_name(model_choice)?;
    let mut embedder = KnotEmbedder::init(model_choice, &cfg.embed.cache_dir)?;
    let quiet = params.quiet;
    run_index_with(cfg, params, &mut embedder, &mut |event| {
        if !quiet {
            report_progress(&event);
        }
    })
}

/// Run `build_index` with an injected embedder and progress listener (tests).
pub fn run_index_with(
    cfg: &Config,
    params: &IndexParams,
    embedder: &mut dyn EmbedBatch,
    progress: &mut dyn FnMut(ProgressEvent),
) -> anyhow::Result<Value> {
    let opts = BuildOptions {
        force: params.force,
        commit_mode: params.commit_mode,
        jobs: params.jobs.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
        }),
        batch_size: params.batch_size,
        lsp: crate::lsp::precompute::PrecomputeOptions::default(),
    };
    let report = build_index(cfg, embedder, &opts, None, progress)?;
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
        };
        let mut fake = FakeEmbedder::new(16);
        let mut events = Vec::new();
        let value = run_index_with(&cfg, &params, &mut fake, &mut |e| events.push(e)).expect("run");
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
        };
        let mut fake1 = FakeEmbedder::new(16);
        let val1 = run_index_with(&cfg, &params, &mut fake1, &mut |_| {}).expect("run 1");
        let embedded1 = val1["vectors"]["embedded"].as_u64().unwrap();

        let mut fake2 = FakeEmbedder::new(16);
        let val2 = run_index_with(&cfg, &params, &mut fake2, &mut |_| {}).expect("run 2");
        assert_eq!(val2["vectors"]["reused"].as_u64().unwrap(), embedded1);
        assert_eq!(val2["vectors"]["embedded"].as_u64().unwrap(), 0);
    }

    #[test]
    fn index_rejects_unknown_model_via_validate() {
        let err = validate_model_name("unknown-model-xyz").expect_err("must fail");
        assert!(err.to_string().contains("unknown-model-xyz"));
    }
}
