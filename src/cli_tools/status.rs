//! The `status` vertical slice: report building and output formatting.
//!
//! CLI and MCP render from one source: the formatters read the report JSON
//! (like knot's formatters), so both surfaces always agree.

use anyhow::{Context as _, Result};
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table};

use crate::capability::languages::LspLang;
use crate::capability::lsp_registry::{LspStatus, detect};
use crate::capability::scan::scan_repository;
use crate::capability::tier::build_report;
use crate::config::{Config, OutputFormat};
use crate::index::layout::IndexLayout;
use crate::index::manifest::read_index_state;

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
    let index = read_index_state(&IndexLayout::for_repo(&cfg.repo_path));
    let report = build_report(cfg, &scan, index, detect);
    serde_json::to_value(&report).context("failed to serialize the status report")
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

fn index_line(report: &serde_json::Value) -> String {
    let index = report.get("index").cloned().unwrap_or_default();
    let state = str_at(&index, &["state"]).unwrap_or("unknown");
    match state {
        "absent" => format!(
            "index: absent ({} not found)",
            str_at(&index, &["manifest"]).unwrap_or("manifest")
        ),
        "present" => format!(
            "index: present (schema {}, written by knot-lite {})",
            str_at(&index, &["schema"]).unwrap_or("-"),
            str_at(&index, &["knot_lite_version"]).unwrap_or("-")
        ),
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

fn str_at<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .try_fold(value, |acc, key| acc.get(key))
        .and_then(|v| v.as_str())
}

fn u64_at(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .try_fold(value, |acc, key| acc.get(key))
        .and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::lsp_registry::LspStatus;

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
}
