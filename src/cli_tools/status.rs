//! The `status` vertical slice: report building and output formatting.
//!
//! CLI and MCP render from one source: the formatters read the report JSON
//! (like knot's formatters), so both surfaces always agree.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result};
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table};

use crate::capability::languages::{LspLang, lsp_lang_for_label};
use crate::capability::lsp_registry::{LspStatus, detect};
use crate::capability::scan::scan_repository;
use crate::capability::tier::{Freshness, IndexDetails, LspView, build_report_with};
use crate::config::{Config, OutputFormat};
use crate::index::inventory::{collect_inventory, diff, read_inventory};
use crate::index::layout::IndexLayout;
use crate::index::manifest::{Manifest, read_manifest};
use crate::index::select::select_files;
use crate::index::{IndexState, read_index_state};
use crate::lsp::launch::start_pool;
use crate::lsp::pool::PoolSnapshot;
use crate::lsp::probe::{ProbeResult, probe_languages};
use crate::query::render::{str_at, u64_at};
use crate::query::session::CLI_READY_WAIT;

/// What `--probe-lsp` runs: start the servers of these languages (given the
/// repository's files) and report how each did.
pub type Probe<'a> = &'a dyn Fn(&[LspLang], &[String]) -> BTreeMap<String, ProbeResult>;

/// What a surface adds to the report on top of the repository scan.
#[derive(Default)]
pub struct StatusExtras<'a> {
    /// The live state of the language-server pool (the MCP server has one).
    pub runtime: Option<&'a PoolSnapshot>,
    /// `--probe-lsp` (CLI only, opt-in).
    pub probe: Option<Probe<'a>>,
}

/// Build the `knot-lite/status/v1` report against the real environment.
pub fn run_status(cfg: &Config) -> Result<serde_json::Value> {
    run_status_with(cfg, &|lang| detect(lang, &cfg.lsp_overrides))
}

/// `status --probe-lsp`: the report plus a probe of every language server the
/// repository could use. The probe starts each server through a fresh pool and
/// stops it again; a server that cannot start is a result, not an error.
pub fn run_status_probing(cfg: &Config) -> Result<serde_json::Value> {
    let probe = |languages: &[LspLang], files: &[String]| probe_real(cfg, languages, files);
    let extras = StatusExtras {
        runtime: None,
        probe: Some(&probe),
    };
    run_status_with_extras(cfg, &|lang| detect(lang, &cfg.lsp_overrides), &extras)
}

fn probe_real(
    cfg: &Config,
    languages: &[LspLang],
    files: &[String],
) -> BTreeMap<String, ProbeResult> {
    let unavailable = |why: &str| -> BTreeMap<String, ProbeResult> {
        languages
            .iter()
            .map(|lang| {
                (
                    lang.dir_name().to_owned(),
                    ProbeResult::unavailable(why.to_owned()),
                )
            })
            .collect()
    };
    if !cfg.lsp.enabled {
        return unavailable("language servers are off (KNOT_LITE_LSP=off)");
    }
    match start_pool(cfg, files, CLI_READY_WAIT, None) {
        Some(pool) => probe_languages(&pool, languages, CLI_READY_WAIT),
        None => unavailable("the language-server pool could not be started"),
    }
}

/// The report with the live state of a running pool (the MCP server's).
pub fn run_status_with_runtime(
    cfg: &Config,
    runtime: Option<&PoolSnapshot>,
) -> Result<serde_json::Value> {
    let extras = StatusExtras {
        runtime,
        probe: None,
    };
    run_status_with_extras(cfg, &|lang| detect(lang, &cfg.lsp_overrides), &extras)
}

/// Build the report with an injected language-server lookup (tests).
pub fn run_status_with(
    cfg: &Config,
    detect: &dyn Fn(LspLang) -> LspStatus,
) -> Result<serde_json::Value> {
    run_status_with_extras(cfg, detect, &StatusExtras::default())
}

/// Build the report with live language-server information and/or a probe.
pub fn run_status_with_extras(
    cfg: &Config,
    detect: &dyn Fn(LspLang) -> LspStatus,
    extras: &StatusExtras<'_>,
) -> Result<serde_json::Value> {
    let scan = scan_repository(&cfg.repo_path)?;
    let layout = IndexLayout::for_repo(&cfg.repo_path);
    let index_state = read_index_state(&layout);
    let manifest = match &index_state {
        IndexState::Present { .. } => read_manifest(&layout.manifest).ok(),
        _ => None,
    };
    let details = manifest
        .as_ref()
        .and_then(|manifest| compute_index_details(cfg, &layout, manifest));
    let view = LspView {
        precompute: manifest.as_ref().and_then(|manifest| manifest.lsp.as_ref()),
        runtime: extras.runtime,
    };
    let mut report = build_report_with(cfg, &scan, index_state, detect, &view);
    if let Some(details) = details {
        if details.model != details.configured_model {
            report.notes.push(format!(
                "The index was built with {}; the configured model is {}. Queries use the index's model; run `knot-lite index` to switch.",
                details.model, details.configured_model
            ));
        }
        if !details.model_validated {
            report.notes.push(format!(
                "{} is not validated by knot-lite's measurements (only AllMiniLML6V2 is).",
                details.model
            ));
        }
        report.index.details = Some(details);
    }
    if let Some(probe) = extras.probe {
        let languages: BTreeSet<LspLang> = report
            .languages
            .iter()
            .filter_map(|row| lsp_lang_for_label(&row.language))
            .collect();
        let languages: Vec<LspLang> = languages.into_iter().collect();
        let files: Vec<String> = scan.files.iter().map(|(rel, _)| rel.clone()).collect();
        report.lsp_probe = Some(probe(&languages, &files));
    }
    serde_json::to_value(&report).context("failed to serialize the status report")
}

fn compute_index_details(
    cfg: &Config,
    layout: &IndexLayout,
    manifest: &Manifest,
) -> Option<IndexDetails> {
    let stored_inventory = read_inventory(&layout.inventory).ok();
    let selection = select_files(&cfg.repo_path).ok();
    let current_inventory = selection
        .as_ref()
        .and_then(|sel| collect_inventory(sel).ok());

    let freshness = match (stored_inventory, current_inventory) {
        (Some(stored), Some(current)) => {
            let d = diff(&stored, &current);
            let state = if d.is_fresh() { "fresh" } else { "stale" };
            Freshness {
                state,
                added: d.added,
                removed: d.removed,
                changed: d.changed,
            }
        }
        _ => Freshness {
            state: "unknown",
            added: 0,
            removed: 0,
            changed: 0,
        },
    };

    let model_validated = manifest.embedding.model == "AllMiniLML6V2";

    Some(IndexDetails {
        model: manifest.embedding.model.clone(),
        dim: manifest.embedding.dim,
        encoding: manifest.embedding.encoding.clone(),
        model_validated,
        configured_model: cfg.embed.model.clone(),
        commit_mode: manifest.commit_mode,
        knot_version: manifest.knot_version.clone(),
        created_at: manifest.created_at.clone(),
        updated_at: manifest.updated_at.clone(),
        counts: manifest.counts.clone(),
        freshness,
        lsp: manifest.lsp.as_ref().map(Into::into),
    })
}

/// Render a `knot-lite/status/v1` report in the requested format.
pub fn format_status_output(report: &serde_json::Value, output: OutputFormat) -> String {
    match output {
        OutputFormat::Json => serde_json::to_string_pretty(report).unwrap_or_default(),
        OutputFormat::Table => format_table(report),
        OutputFormat::Markdown => format_markdown(report),
    }
}

fn format_table(report: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str(&header_line(report));
    out.push('\n');
    out.push_str(&index_line(report));
    out.push('\n');
    out.push_str(&language_table(report));
    out.push('\n');
    for line in lsp_lines(report) {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(&files_only_line(report));
    out.push('\n');
    out.push_str(&excluded_line(report));
    out.push('\n');
    out
}

fn format_markdown(report: &serde_json::Value) -> String {
    let mut out = String::from("## knot-lite status\n\n");
    out.push_str(&header_line(report));
    out.push('\n');
    out.push_str(&index_line(report));
    out.push_str("\n\n");
    out.push_str(&language_markdown_table(report));
    out.push('\n');
    for line in lsp_lines(report) {
        out.push_str(&format!("- {line}\n"));
    }
    out.push_str(&format!("- {}\n", files_only_line(report)));
    out.push_str(&format!("- {}\n", excluded_line(report)));
    out
}

fn header_line(report: &serde_json::Value) -> String {
    let name = str_at(report, &["repo", "name"]).unwrap_or("-");
    let path = str_at(report, &["repo", "path"]).unwrap_or("-");
    format!("knot-lite status — repository '{name}' ({path})")
}

fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    let len = s.len();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn index_line(report: &serde_json::Value) -> String {
    let index = report.get("index").cloned().unwrap_or_default();
    let state = str_at(&index, &["state"]).unwrap_or("unknown");
    match state {
        "absent" => format!(
            "index: absent ({} not found) — run `knot-lite index` to build it",
            str_at(&index, &["manifest"]).unwrap_or("manifest")
        ),
        "present" => {
            if let Some(details) = index.get("details") {
                let entities = u64_at(details, &["counts", "entities"]).unwrap_or(0);
                let chunks = u64_at(details, &["counts", "chunks"]).unwrap_or(0);
                let file_vecs = u64_at(details, &["counts", "file_vectors"]).unwrap_or(0);
                let tier1_records = chunks + file_vecs;
                let edges = u64_at(details, &["counts", "edges"]).unwrap_or(0);
                let model = str_at(details, &["model"]).unwrap_or("-");
                let dim = u64_at(details, &["dim"]).unwrap_or(0);
                let encoding = str_at(details, &["encoding"]).unwrap_or("-");
                let updated = str_at(details, &["updated_at"]).unwrap_or("-");
                let mode = str_at(details, &["commit_mode"]).unwrap_or("gitignore");

                let f_state = str_at(details, &["freshness", "state"]).unwrap_or("fresh");
                let freshness_str = if f_state == "stale" {
                    let changed = u64_at(details, &["freshness", "changed"]).unwrap_or(0);
                    let added = u64_at(details, &["freshness", "added"]).unwrap_or(0);
                    let removed = u64_at(details, &["freshness", "removed"]).unwrap_or(0);
                    format!(
                        "stale ({changed} changed, {added} added, {removed} removed — run `knot-lite index`)"
                    )
                } else {
                    f_state.to_string()
                };

                format!(
                    "index: present — {} entities, {} tier-1 records, {} edges · {model} ({dim}, {encoding}) · {freshness_str} · updated {updated} · {mode}",
                    format_count(entities),
                    format_count(tier1_records),
                    format_count(edges)
                )
            } else {
                format!(
                    "index: present (schema {}, written by knot-lite {})",
                    str_at(&index, &["schema"]).unwrap_or("-"),
                    str_at(&index, &["knot_lite_version"]).unwrap_or("-")
                )
            }
        }
        "incomplete" => {
            "index: incomplete (an indexing run did not finish — run `knot-lite index`)".to_string()
        }
        "unsupported" => format!(
            "index: unsupported schema '{}'",
            str_at(&index, &["schema"]).unwrap_or("-")
        ),
        "invalid" => format!(
            "index: invalid ({})",
            str_at(&index, &["error"]).unwrap_or("unknown error")
        ),
        other => format!("index: {other}"),
    }
}

fn language_table(report: &serde_json::Value) -> String {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::DynamicFullWidth);
    table.set_header(vec![
        Cell::new("LANGUAGE")
            .set_alignment(CellAlignment::Left)
            .fg(Color::Green),
        Cell::new("FILES")
            .set_alignment(CellAlignment::Right)
            .fg(Color::Cyan),
        Cell::new("TIER")
            .set_alignment(CellAlignment::Right)
            .fg(Color::Yellow),
        Cell::new("GRAPH")
            .set_alignment(CellAlignment::Left)
            .fg(Color::Magenta),
        Cell::new("LSP")
            .set_alignment(CellAlignment::Left)
            .fg(Color::Magenta),
        Cell::new("NOTE").set_alignment(CellAlignment::Left),
    ]);
    for row in &languages_of(report) {
        table.add_row(vec![
            Cell::new(str_at(row, &["language"]).unwrap_or("-")).fg(Color::Green),
            Cell::new(u64_at(row, &["files"]).unwrap_or(0).to_string())
                .set_alignment(CellAlignment::Right),
            Cell::new(u64_at(row, &["tier"]).unwrap_or(0).to_string())
                .set_alignment(CellAlignment::Right),
            Cell::new(str_at(row, &["graph"]).unwrap_or("-")),
            Cell::new(lsp_row_cell(row)),
            Cell::new(str_at(row, &["note"]).unwrap_or("—")),
        ]);
    }
    table.to_string()
}

fn language_markdown_table(report: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str("| LANGUAGE | FILES | TIER | GRAPH | LSP | NOTE |\n");
    out.push_str("|---|---:|---:|---|---|---|\n");
    for row in &languages_of(report) {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            str_at(row, &["language"]).unwrap_or("-"),
            u64_at(row, &["files"]).unwrap_or(0),
            u64_at(row, &["tier"]).unwrap_or(0),
            str_at(row, &["graph"]).unwrap_or("-"),
            lsp_row_cell(row),
            str_at(row, &["note"]).unwrap_or("—"),
        ));
    }
    out
}

/// The LSP cell of a language row: the binary, then what the index pre-computed
/// and what the running server is doing (both when known).
fn lsp_row_cell(row: &serde_json::Value) -> String {
    let mut cell = lsp_cell(row.get("lsp"));
    if cell == "—" {
        return cell;
    }
    if let Some(runtime) = row.get("lsp_runtime").filter(|v| !v.is_null()) {
        cell.push_str(&format!(
            " · {}",
            str_at(runtime, &["state"]).unwrap_or("unknown")
        ));
        if let Some(ready_ms) = u64_at(runtime, &["ready_ms"]) {
            cell.push_str(&format!(" ({:.1} s)", ready_ms as f64 / 1000.0));
        }
    }
    if let Some(precompute) = str_at(row, &["lsp_precompute"]) {
        cell.push_str(&format!(" · pre-computed: {precompute}"));
    }
    cell
}

/// The settings line, the probe results and the setting warnings.
fn lsp_lines(report: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(settings) = report.get("lsp_settings").filter(|v| v.is_object()) {
        let on = |key: &str| {
            if settings.get(key).and_then(serde_json::Value::as_bool) == Some(true) {
                "on"
            } else {
                "off"
            }
        };
        lines.push(format!(
            "language servers: {} · build code {} · idle {} s · live budget {} targets / {} s · scratch {}",
            on("enabled"),
            on("build_exec"),
            u64_at(settings, &["idle_s"]).unwrap_or(0),
            u64_at(settings, &["max_live_targets"]).unwrap_or(0),
            u64_at(settings, &["deadline_s"]).unwrap_or(0),
            str_at(settings, &["cache_dir"]).unwrap_or("-"),
        ));
    }
    if let Some(probe) = report.get("lsp_probe").and_then(|v| v.as_object()) {
        for (language, result) in probe {
            lines.push(probe_line(language, result));
        }
    }
    if let Some(warnings) = report.get("warnings").and_then(|v| v.as_array()) {
        lines.extend(
            warnings
                .iter()
                .filter_map(|w| w.as_str())
                .map(|w| format!("warning: {w}")),
        );
    }
    lines
}

/// `probe rust: ok — ready in 4.9 s, rust-analyzer 1.98.1, 3323 MiB`.
fn probe_line(language: &str, result: &serde_json::Value) -> String {
    if result.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return format!(
            "probe {language}: failed — {}",
            str_at(result, &["diagnostic"]).unwrap_or("no reason given")
        );
    }
    let mut parts = Vec::new();
    if let Some(ready_ms) = u64_at(result, &["ready_ms"]) {
        parts.push(format!("ready in {:.1} s", ready_ms as f64 / 1000.0));
    }
    if let Some(version) = str_at(result, &["version"]).filter(|v| !v.is_empty()) {
        parts.push(version.to_owned());
    }
    if let Some(rss) = result.get("rss_mib").and_then(serde_json::Value::as_f64) {
        parts.push(format!("{rss:.0} MiB"));
    }
    format!("probe {language}: ok — {}", parts.join(", "))
}

/// Render the LSP cell (§9.2).
fn lsp_cell(lsp: Option<&serde_json::Value>) -> String {
    let Some(lsp) = lsp.filter(|v| !v.is_null()) else {
        return "—".to_string();
    };
    let server = str_at(lsp, &["server"]).unwrap_or("?");
    match str_at(lsp, &["status"]).unwrap_or("") {
        "found" => format!("{server} ✓ {}", str_at(lsp, &["path"]).unwrap_or("-")),
        "override" => format!(
            "{server} ✓ {} (override)",
            str_at(lsp, &["path"]).unwrap_or("-")
        ),
        "missing" => format!(
            "{server} ✗ not found (searched {} dirs)",
            u64_at(lsp, &["searched_dirs"]).unwrap_or(0)
        ),
        "override_invalid" => format!(
            "{server} ✗ override '{}' is not executable",
            str_at(lsp, &["value"]).unwrap_or("-")
        ),
        _ => "—".to_string(),
    }
}

fn files_only_line(report: &serde_json::Value) -> String {
    let files_only = report.get("files_only").cloned().unwrap_or_default();
    let files = u64_at(&files_only, &["files"]).unwrap_or(0);
    let mut parts = Vec::new();
    if let Some(top) = files_only.get("top").and_then(|v| v.as_array()) {
        for entry in top {
            parts.push(format!(
                "{} {}",
                str_at(entry, &["key"]).unwrap_or("?"),
                u64_at(entry, &["files"]).unwrap_or(0)
            ));
        }
    }
    if parts.is_empty() {
        format!("files-only (tier 1): {files} files")
    } else {
        format!("files-only (tier 1): {files} files — {}", parts.join(", "))
    }
}

fn excluded_line(report: &serde_json::Value) -> String {
    let excluded = report.get("excluded").cloned().unwrap_or_default();
    let files = u64_at(&excluded, &["files"]).unwrap_or(0);
    let by_reason = excluded.get("by_reason").cloned().unwrap_or_default();
    let counts = [
        ("binary", u64_at(&by_reason, &["binary"]).unwrap_or(0)),
        ("lock file", u64_at(&by_reason, &["lock_file"]).unwrap_or(0)),
        ("too large", u64_at(&by_reason, &["too_large"]).unwrap_or(0)),
    ];
    let listing = counts
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("excluded (tier 0): {files} files — {listing} (gitignored files are not counted)")
}

fn languages_of(report: &serde_json::Value) -> Vec<serde_json::Value> {
    report
        .get("languages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::lsp_registry::LspStatus;
    use crate::embed::fake::FakeEmbedder;
    use crate::index::build::test_support::{DIM, fixture, opts, run_with};

    /// Minimal fixture: one parsed rust file, one files-only file, one excluded file.
    fn fixture_cfg() -> (tempfile::TempDir, Config) {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn f() {}\n").expect("write rs");
        std::fs::write(tmp.path().join("run.sh"), "echo hi\n").expect("write sh");
        std::fs::write(tmp.path().join("img.png"), [0x89u8, 0x50, 0x00, 0x01]).expect("write png");
        let cfg = crate::config::test_support::config_for(tmp.path().to_path_buf());
        (tmp, cfg)
    }

    fn stub_detect() -> LspStatus {
        LspStatus::Found {
            server: "rust-analyzer".to_string(),
            path: "/bin/rust-analyzer".to_string(),
        }
    }

    #[test]
    fn run_status_with_produces_deterministic_json() {
        let (_tmp, cfg) = fixture_cfg();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        assert_eq!(str_at(&report, &["schema"]), Some("knot-lite/status/v1"));
        assert_eq!(str_at(&report, &["index", "state"]), Some("absent"));
        let again = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        assert_eq!(
            serde_json::to_string(&report).expect("json"),
            serde_json::to_string(&again).expect("json")
        );
    }

    #[test]
    fn table_contains_headers_and_labels() {
        let (_tmp, cfg) = fixture_cfg();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        let table = format_status_output(&report, OutputFormat::Table);
        assert!(table.contains("LANGUAGE"), "{table}");
        for label in ["rust", ".sh", "binary"] {
            assert!(table.contains(label), "missing {label} in {table}");
        }
        assert!(
            table.contains("knot-lite status — repository 'demo'"),
            "{table}"
        );
        assert!(table.contains("run `knot-lite index` to build it"));
    }

    #[test]
    fn markdown_starts_with_title() {
        let (_tmp, cfg) = fixture_cfg();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        let md = format_status_output(&report, OutputFormat::Markdown);
        assert!(md.starts_with("## knot-lite status"), "{md}");
        assert!(md.contains("| LANGUAGE | FILES | TIER | GRAPH | LSP | NOTE |"));
    }

    #[test]
    fn json_round_trips() {
        let (_tmp, cfg) = fixture_cfg();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        let text = format_status_output(&report, OutputFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(parsed, report);
    }

    #[test]
    fn run_status_is_side_effect_free() {
        let (tmp, cfg) = fixture_cfg();
        let mut before: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        before.sort();
        let _ = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        let mut after: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn status_reports_index_details_and_fresh() {
        let (_tmp, cfg) = fixture();
        let mut fake = FakeEmbedder::new(DIM);
        crate::index::build::build_index(&cfg, &mut fake, &opts(), None, &mut |_| {})
            .expect("build");

        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        assert_eq!(str_at(&report, &["index", "state"]), Some("present"));
        assert_eq!(
            str_at(&report, &["index", "details", "freshness", "state"]),
            Some("fresh")
        );
        assert_eq!(
            u64_at(&report, &["index", "details", "freshness", "added"]),
            Some(0)
        );

        let table = format_status_output(&report, OutputFormat::Table);
        assert!(table.contains("index: present — 14 entities, 5 tier-1 records, 6 edges · knot-lite-fake-sha256 (16, i8pv_b64) · fresh · updated"), "{table}");
    }

    #[test]
    fn status_reports_stale_counts() {
        let (tmp, cfg) = fixture();
        run_with(&cfg, &opts());

        // Modify one file, add one file, delete one file
        let path = tmp.path().join("src/lib.rs");
        let body = std::fs::read_to_string(&path).expect("read");
        std::fs::write(&path, format!("{body}\n// edit")).expect("edit");
        std::fs::write(tmp.path().join("src/new.rs"), "pub fn n() {}\n").expect("add");
        std::fs::remove_file(tmp.path().join("Makefile")).expect("delete");

        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        assert_eq!(
            str_at(&report, &["index", "details", "freshness", "state"]),
            Some("stale")
        );
        assert_eq!(
            u64_at(&report, &["index", "details", "freshness", "changed"]),
            Some(1)
        );
        assert_eq!(
            u64_at(&report, &["index", "details", "freshness", "added"]),
            Some(1)
        );
        assert_eq!(
            u64_at(&report, &["index", "details", "freshness", "removed"]),
            Some(1)
        );

        let table = format_status_output(&report, OutputFormat::Table);
        assert!(
            table.contains("stale (1 changed, 1 added, 1 removed — run `knot-lite index`)"),
            "{table}"
        );
    }

    #[test]
    fn status_incomplete_index_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = crate::config::test_support::config_for(tmp.path().to_path_buf());
        let layout = IndexLayout::for_repo(tmp.path());
        std::fs::create_dir_all(&layout.dir).expect("mkdir");
        std::fs::write(&layout.previous_manifest, "schema = \"v1\"\n").expect("write");

        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        let table = format_status_output(&report, OutputFormat::Table);
        assert!(
            table.contains(
                "index: incomplete (an indexing run did not finish — run `knot-lite index`)"
            ),
            "{table}"
        );
    }

    #[test]
    fn status_model_mismatch_note() {
        let (_tmp, mut cfg) = fixture();
        run_with(&cfg, &opts());

        cfg.embed.model = "BGEBaseENV15".to_string();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        let notes = report["notes"].as_array().expect("notes");
        let note_strs: Vec<&str> = notes.iter().filter_map(|v| v.as_str()).collect();
        assert!(note_strs.iter().any(|n| n.contains(
            "The index was built with knot-lite-fake-sha256; the configured model is BGEBaseENV15"
        )));
        assert!(
            note_strs
                .iter()
                .any(|n| n.contains("knot-lite-fake-sha256 is not validated"))
        );
    }

    #[test]
    fn status_is_read_only_with_index() {
        use std::path::Path;

        let (tmp, cfg) = fixture();
        run_with(&cfg, &opts());

        fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in std::fs::read_dir(dir).expect("read_dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    let rel = path
                        .strip_prefix(base)
                        .expect("rel")
                        .to_string_lossy()
                        .into_owned();
                    out.push((rel, std::fs::read(&path).expect("read")));
                }
            }
        }

        fn full_tree_snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
            let mut out = Vec::new();
            walk(dir, dir, &mut out);
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        }

        let before = full_tree_snapshot(tmp.path());
        let _ = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        let after = full_tree_snapshot(tmp.path());
        assert_eq!(before, after);
    }
    /// The row of `language` in a report.
    fn row<'a>(report: &'a serde_json::Value, language: &str) -> &'a serde_json::Value {
        report["languages"]
            .as_array()
            .expect("languages")
            .iter()
            .find(|row| row["language"] == language)
            .unwrap_or_else(|| panic!("no {language} row in {report}"))
    }

    /// An index whose manifest says the Rust files were pre-computed.
    fn indexed_with_precompute() -> (tempfile::TempDir, Config) {
        use crate::index::manifest::{LspLangManifest, LspManifest, write_manifest_atomic};

        let (tmp, cfg) = fixture();
        run_with(&cfg, &opts());
        let layout = IndexLayout::for_repo(tmp.path());
        let mut manifest = read_manifest(&layout.manifest).expect("manifest");
        manifest.lsp = Some(LspManifest {
            format: 1,
            policy: "all".to_owned(),
            min_fanin: 20,
            max_targets: 200,
            budget_secs: 600,
            languages: BTreeMap::from([(
                "rust".to_owned(),
                LspLangManifest {
                    status: "reused".to_owned(),
                    server: "rust-analyzer".to_owned(),
                    version: String::new(),
                    files_fingerprint: "0123456789abcdef".to_owned(),
                    targets_selected: 12,
                    targets_ok: 11,
                    targets_failed: 1,
                    edges: 340,
                    ms: 5_000,
                    diagnostic: String::new(),
                },
            )]),
        });
        write_manifest_atomic(&layout.manifest, &manifest).expect("write");
        (tmp, cfg)
    }

    #[test]
    fn tier3_note_text_is_pinned() {
        assert_eq!(
            crate::capability::tier::TIER3_NOTE,
            "Tier 3: find_callers asks the language server (live, or pre-computed by knot-lite index for high fan-in targets) and falls back to heuristic edges per target when the server is unavailable."
        );
        let (_tmp, cfg) = fixture_cfg();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        assert!(
            report["notes"]
                .to_string()
                .contains("Tier 3: find_callers asks the language server"),
            "{report}"
        );
        assert!(
            !report
                .to_string()
                .contains("does not start language servers")
        );
    }

    #[test]
    fn status_rows_include_lsp_precompute() {
        let (_tmp, cfg) = indexed_with_precompute();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        assert_eq!(row(&report, "rust")["lsp_precompute"], "reused");
        // A language without a server has nothing to pre-compute.
        assert!(row(&report, "cargo")["lsp_precompute"].is_null());

        let details = &report["index"]["details"];
        assert_eq!(details["lsp"]["policy"], "all");
        let rust = &details["lsp"]["languages"]["rust"];
        assert_eq!(
            (
                rust["status"].as_str(),
                rust["targets_ok"].as_u64(),
                rust["edges"].as_u64()
            ),
            (Some("reused"), Some(11), Some(340))
        );
        assert!(details["counts"]["columns_located"].as_u64().is_some());

        let table = format_status_output(&report, OutputFormat::Table);
        assert!(table.contains("pre-computed: reused"), "{table}");
    }

    #[test]
    fn status_after_a_default_index_reports_none_for_the_languages_present() {
        let (_tmp, cfg) = fixture();
        run_with(&cfg, &opts());
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        // The default policy pre-computes Java only; the fixture has none.
        assert_eq!(row(&report, "rust")["lsp_precompute"], "none");
    }

    #[test]
    fn status_of_a_phase2_index_has_null_rows_and_no_lsp_details() {
        use crate::index::manifest::write_manifest_atomic;

        let (tmp, cfg) = fixture();
        run_with(&cfg, &opts());
        let layout = IndexLayout::for_repo(tmp.path());
        let mut manifest = read_manifest(&layout.manifest).expect("manifest");
        manifest.lsp = None;
        write_manifest_atomic(&layout.manifest, &manifest).expect("write");

        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        assert!(row(&report, "rust")["lsp_precompute"].is_null());
        assert!(report["index"]["details"]["lsp"].is_null());
    }

    #[test]
    fn status_cli_runtime_is_null() {
        let (_tmp, cfg) = fixture_cfg();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        for language in report["languages"].as_array().expect("rows") {
            assert!(language["lsp_runtime"].is_null(), "{language}");
        }
        assert!(report.get("lsp_probe").is_none(), "only with --probe-lsp");
    }

    #[test]
    fn status_runtime_rows_come_from_the_pool_snapshot() {
        use crate::lsp::pool::SlotSnapshot;

        let (_tmp, cfg) = fixture_cfg();
        let snapshot = PoolSnapshot {
            slots: BTreeMap::from([(
                "rust".to_owned(),
                SlotSnapshot {
                    state: "ready".to_owned(),
                    server: "rust-analyzer".to_owned(),
                    version: "rust-analyzer 1.98.1".to_owned(),
                    pid: Some(1234),
                    ready_ms: Some(4937),
                    uptime_s: Some(310),
                    rss_mib: Some(3322.6),
                    attempts: 0,
                    diagnostic: None,
                },
            )]),
        };
        let extras = StatusExtras {
            runtime: Some(&snapshot),
            probe: None,
        };
        let report = run_status_with_extras(&cfg, &|_| stub_detect(), &extras).expect("report");
        let runtime = &row(&report, "rust")["lsp_runtime"];
        assert_eq!(runtime["state"], "ready");
        assert_eq!(runtime["pid"], 1234);
        assert_eq!(runtime["ready_ms"], 4937);
        assert_eq!(runtime["rss_mib"], 3322.6);
        assert!(runtime["diagnostic"].is_null());

        let table = format_status_output(&report, OutputFormat::Table);
        assert!(table.contains("· ready (4.9 s)"), "{table}");
    }

    #[test]
    fn status_probe_reports_degraded_language_without_failing() {
        let (_tmp, cfg) = fixture_cfg();
        let pool = crate::lsp::fake::FakeServers::missing().pool();
        let probe = |languages: &[LspLang], _files: &[String]| {
            probe_languages(&pool, languages, std::time::Duration::from_secs(10))
        };
        let extras = StatusExtras {
            runtime: None,
            probe: Some(&probe),
        };
        let report =
            run_status_with_extras(&cfg, &|_| stub_detect(), &extras).expect("a probe is data");
        let rust = &report["lsp_probe"]["rust"];
        assert_eq!(rust["ok"], false);
        assert!(
            rust["diagnostic"]
                .as_str()
                .is_some_and(|d| d.contains("not found in PATH")),
            "{rust}"
        );
        // Only the languages the repository has are probed.
        assert_eq!(
            report["lsp_probe"]
                .as_object()
                .expect("probe")
                .keys()
                .collect::<Vec<_>>(),
            vec!["rust"]
        );
        let table = format_status_output(&report, OutputFormat::Table);
        assert!(
            table.contains("probe rust: failed — rust-analyzer not found in PATH"),
            "{table}"
        );
    }

    #[test]
    fn status_probe_reports_a_server_that_starts() {
        let (_tmp, cfg) = fixture_cfg();
        let pool =
            crate::lsp::fake::FakeServers::new(|_| crate::lsp::fake::FakeScript::full()).pool();
        let probe = |languages: &[LspLang], _files: &[String]| {
            probe_languages(&pool, languages, std::time::Duration::from_secs(10))
        };
        let extras = StatusExtras {
            runtime: None,
            probe: Some(&probe),
        };
        let report = run_status_with_extras(&cfg, &|_| stub_detect(), &extras).expect("report");
        assert_eq!(report["lsp_probe"]["rust"]["ok"], true);
        let table = format_status_output(&report, OutputFormat::Table);
        assert!(table.contains("probe rust: ok — ready in"), "{table}");
    }

    #[test]
    fn status_lists_lsp_setting_warnings() {
        let (_tmp, mut cfg) = fixture_cfg();
        cfg.lsp
            .warnings
            .push("KNOT_LITE_LSP_IDLE_SECS = 'soon' is not a number; using 300".to_owned());
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        assert_eq!(
            report["warnings"],
            serde_json::json!(["KNOT_LITE_LSP_IDLE_SECS = 'soon' is not a number; using 300"])
        );
        let table = format_status_output(&report, OutputFormat::Table);
        assert!(
            table.contains("warning: KNOT_LITE_LSP_IDLE_SECS = 'soon' is not a number"),
            "{table}"
        );
        let markdown = format_status_output(&report, OutputFormat::Markdown);
        assert!(
            markdown.contains("- warning: KNOT_LITE_LSP_IDLE_SECS"),
            "{markdown}"
        );
    }

    #[test]
    fn status_shows_the_language_server_settings() {
        let (_tmp, cfg) = fixture_cfg();
        let report = run_status_with(&cfg, &|_| stub_detect()).expect("report");
        let settings = &report["lsp_settings"];
        assert_eq!(settings["enabled"], false);
        assert_eq!(settings["max_live_targets"], 10);
        assert_eq!(settings["deadline_s"], 30);
        assert!(settings["cache_dir"].is_string());
        let table = format_status_output(&report, OutputFormat::Table);
        assert!(table.contains("language servers: off"), "{table}");
        assert!(table.contains("live budget 10 targets / 30 s"), "{table}");
    }
}
