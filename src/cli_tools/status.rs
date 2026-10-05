//! The `status` vertical slice: report building and output formatting.
//!
//! CLI and MCP render from one source: the formatters read the report JSON
//! (like knot's formatters), so both surfaces always agree.

use anyhow::{Context as _, Result};
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table};

use crate::capability::languages::LspLang;
use crate::capability::lsp_registry::{LspStatus, detect};
use crate::capability::scan::scan_repository;
use crate::capability::tier::{Freshness, IndexDetails, build_report};
use crate::config::{Config, OutputFormat};
use crate::index::inventory::{collect_inventory, diff, read_inventory};
use crate::index::layout::IndexLayout;
use crate::index::manifest::read_manifest;
use crate::index::select::select_files;
use crate::index::{IndexState, read_index_state};
use crate::query::render::{str_at, u64_at};

/// Build the `knot-lite/status/v1` report against the real environment.
pub fn run_status(cfg: &Config) -> Result<serde_json::Value> {
    run_status_with(cfg, &|lang| detect(lang, &cfg.lsp_overrides))
}

/// Build the report with an injected language-server lookup (tests).
pub fn run_status_with(
    cfg: &Config,
    detect: &dyn Fn(LspLang) -> LspStatus,
) -> Result<serde_json::Value> {
    let scan = scan_repository(&cfg.repo_path)?;
    let layout = IndexLayout::for_repo(&cfg.repo_path);
    let index_state = read_index_state(&layout);
    let details = match &index_state {
        IndexState::Present { .. } => compute_index_details(cfg, &layout),
        _ => None,
    };
    let mut report = build_report(cfg, &scan, index_state, detect);
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
    serde_json::to_value(&report).context("failed to serialize the status report")
}

fn compute_index_details(cfg: &Config, layout: &IndexLayout) -> Option<IndexDetails> {
    let manifest = read_manifest(&layout.manifest).ok()?;
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
        counts: manifest.counts,
        freshness,
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
            Cell::new(lsp_cell(row.get("lsp"))),
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
            lsp_cell(row.get("lsp")),
            str_at(row, &["note"]).unwrap_or("—"),
        ));
    }
    out
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
        crate::index::build::build_index(&cfg, &mut fake, &opts(), &mut |_| {}).expect("build");

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
}
