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

use std::collections::{BTreeMap, BTreeSet, HashMap};
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

fn work_copy_path(repo: &str) -> PathBuf {
    required_env_dir("KNOT_LITE_MEASURE_WORK").join(repo)
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

/// A target of the Phase 0 spike's target files (shared by M6 and L2/L3).
#[derive(Deserialize)]
struct TargetFileEntry {
    id: String,
    name: String,
    file: String,
    line: usize,
    knot_query: String,
}

/// The Phase 0 spike's target file.
#[derive(Deserialize)]
struct TargetFile {
    target: Vec<TargetFileEntry>,
}

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

mod edits;
mod explore;
mod hybrid;
mod lsp;
mod phase2;
mod precompute;
mod sync;
