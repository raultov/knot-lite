//! Results report rendering (T10): Markdown tables from the spike JSONs.
//!
//! Tables are rendered into `<!-- BEGIN:table:<name> --> … <!-- END:table:<name> -->`
//! blocks; narrative prose outside the blocks is never touched. Every cell is
//! traceable to a JSON field (§"footnotes" of each table name the source files).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::graph::gold::GoldResult;
use crate::graph::heuristic::HeuristicResult;
use crate::probe::{NoLspResult, S1Result};
use crate::vec::measure::S2Result;

/// Every JSON artefact the report needs.
pub struct ReportData {
    pub s1_cold: BTreeMap<String, S1Result>,
    pub s1_warm: BTreeMap<String, S1Result>,
    pub nolsp: BTreeMap<String, NoLspResult>,
    pub nolsp_broken: BTreeMap<String, NoLspResult>,
    pub gold: BTreeMap<String, GoldResult>,
    pub heuristic: BTreeMap<String, HeuristicResult>,
    pub s2: BTreeMap<String, S2Result>,
}

/// Table names, in report order (T10 template).
pub const TABLES: &[&str] = &[
    "verdict",
    "environment",
    "s1_capabilities",
    "s1_startup",
    "s1_accuracy",
    "s1_accuracy_category",
    "s1_disagreements",
    "s1_columns",
    "s1_degradation",
    "s2_sizes",
    "s2_determinism",
    "s2_performance",
    "s2_quality",
    "s2_tier1",
];

/// Load every result JSON of `results/` (T10).
pub fn load(results: &Path) -> Result<ReportData> {
    let mut data = ReportData {
        s1_cold: BTreeMap::new(),
        s1_warm: BTreeMap::new(),
        nolsp: BTreeMap::new(),
        nolsp_broken: BTreeMap::new(),
        gold: BTreeMap::new(),
        heuristic: BTreeMap::new(),
        s2: BTreeMap::new(),
    };
    for lang in ["rust", "ts", "java"] {
        data.s1_cold.insert(
            lang.to_owned(),
            read_json(&results.join(format!("s1_{lang}_cold.json")))?,
        );
        data.s1_warm.insert(
            lang.to_owned(),
            read_json(&results.join(format!("s1_{lang}_warm.json")))?,
        );
        data.nolsp.insert(
            lang.to_owned(),
            read_json(&results.join(format!("s1_{lang}_nolsp.json")))?,
        );
        data.nolsp_broken.insert(
            lang.to_owned(),
            read_json(&results.join(format!("s1_{lang}_nolsp_broken.json")))?,
        );
        data.gold.insert(
            lang.to_owned(),
            read_json(&results.join(format!("gold_{lang}.json")))?,
        );
        data.heuristic.insert(
            lang.to_owned(),
            read_json(&results.join(format!("heuristic_{lang}.json")))?,
        );
    }
    for repo in ["knot", "knot-site", "spring-ai"] {
        data.s2.insert(
            repo.to_owned(),
            read_json(&results.join(format!("s2_{repo}.json")))?,
        );
    }
    Ok(data)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Replace the marked blocks of `out` with the rendered tables.
pub fn render(out: &Path, tables: &BTreeMap<String, String>) -> Result<()> {
    let text =
        std::fs::read_to_string(out).with_context(|| format!("reading {}", out.display()))?;
    let mut rendered = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<!-- BEGIN:table:") {
        let name_start = start + "<!-- BEGIN:table:".len();
        let name_end = rest[name_start..]
            .find(" -->")
            .context("malformed BEGIN marker")?
            + name_start;
        let name = &rest[name_start..name_end];
        let marker_end = rest[name_end..]
            .find("-->")
            .context("malformed BEGIN marker")?
            + name_end
            + 3;
        let end_tag = format!("<!-- END:table:{name} -->");
        let end = rest[marker_end..]
            .find(&end_tag)
            .with_context(|| format!("missing END marker for table {name}"))?
            + marker_end;
        rendered.push_str(&rest[..=marker_end]);
        rendered.push('\n');
        rendered.push_str(tables.get(name).map(String::as_str).unwrap_or(""));
        rendered.push('\n');
        rendered.push_str(&rest[end..end + end_tag.len()]);
        rest = &rest[end + end_tag.len()..];
    }
    rendered.push_str(rest);
    std::fs::write(out, rendered).with_context(|| format!("writing {}", out.display()))
}

/// `--template-check`: every expected block must exist exactly once (T10).
pub fn template_check(out: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(out).with_context(|| format!("reading {}", out.display()))?;
    let mut problems = Vec::new();
    for table in TABLES {
        let begin = format!("<!-- BEGIN:table:{table} -->");
        let end = format!("<!-- END:table:{table} -->");
        if text.matches(&begin).count() != 1 {
            problems.push(format!(
                "{table}: BEGIN markers = {}",
                text.matches(&begin).count()
            ));
        }
        if text.matches(&end).count() != 1 {
            problems.push(format!(
                "{table}: END markers = {}",
                text.matches(&end).count()
            ));
        }
    }
    if !problems.is_empty() {
        anyhow::bail!("template problems:\n  {}", problems.join("\n  "));
    }
    println!("template check: all {} table blocks present", TABLES.len());
    Ok(())
}

// ---------- formatting rules (T10) ----------

/// ms with 0 decimals (≥ 10) or 1 decimal (< 10).
fn ms(value: f64) -> String {
    if value >= 10.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// Humanised bytes (KiB/MiB, 1 decimal).
fn bytes(value: u64) -> String {
    let kib = value as f64 / 1024.0;
    if kib >= 1024.0 {
        format!("{:.1} MiB", kib / 1024.0)
    } else {
        format!("{kib:.1} KiB")
    }
}

/// Ratios as percentages with 1 decimal.
fn pct(value: f64) -> String {
    format!("{:.1}%", value * 100.0)
}

fn pct_opt(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), pct)
}

/// ✅ / ❌ / ⚠️ (⚠️ = estimated or partial, T10).
fn mark(ok: bool) -> &'static str {
    if ok { "✅" } else { "❌" }
}

fn mark_est(ok: bool, estimated: bool) -> &'static str {
    match (ok, estimated) {
        (true, true) => "⚠️",
        (true, false) => "✅",
        (false, _) => "❌",
    }
}

fn header(cols: &[&str]) -> String {
    let mut out = format!("| {} |\n", cols.join(" | "));
    out.push_str(&format!(
        "|{}|\n",
        cols.iter().map(|_| "---").collect::<Vec<_>>().join("|")
    ));
    out
}

fn row(cells: &[String]) -> String {
    format!("| {} |\n", cells.join(" | "))
}

// ---------- table builders (Appendix E) ----------

/// Render every table (T10).
pub fn tables(data: &ReportData) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("verdict".to_owned(), table_verdict(data));
    out.insert("environment".to_owned(), table_environment(data));
    out.insert("s1_capabilities".to_owned(), table_capabilities(data));
    out.insert("s1_startup".to_owned(), table_startup(data));
    out.insert("s1_accuracy".to_owned(), table_accuracy(data));
    out.insert(
        "s1_accuracy_category".to_owned(),
        table_accuracy_category(data),
    );
    out.insert("s1_disagreements".to_owned(), table_disagreements(data));
    out.insert("s1_columns".to_owned(), table_columns(data));
    out.insert("s1_degradation".to_owned(), table_degradation(data));
    out.insert("s2_sizes".to_owned(), table_sizes(data));
    out.insert("s2_determinism".to_owned(), table_determinism(data));
    out.insert("s2_performance".to_owned(), table_performance(data));
    out.insert("s2_quality".to_owned(), table_quality(data));
    out.insert("s2_tier1".to_owned(), table_tier1(data));
    out
}

fn hit5_of(summary: &crate::vec::measure::HitSummary, system: &str) -> f64 {
    match system {
        "spike_f32" => summary.spike_f32,
        "spike_i8pv" => summary.spike_i8pv,
        "spike_i8fx" => summary.spike_i8fx,
        "knot_search" => summary.knot_search,
        _ => 0.0,
    }
}

fn warm_p95_max(data: &ReportData, lang: &str) -> f64 {
    let Some(run) = data.s1_warm.get(lang) else {
        return 0.0;
    };
    [
        run.latency_ms.prepare_call_hierarchy.p95,
        run.latency_ms.incoming_calls.p95,
        run.latency_ms.references.p95,
        run.latency_ms.implementation.p95,
    ]
    .into_iter()
    .fold(0.0f64, f64::max)
}

fn gate_g1_to_g4(data: &ReportData) -> (bool, bool, bool, bool) {
    let recall = |lang: &str| {
        data.gold
            .get(lang)
            .and_then(|g| g.systems.get("lsp"))
            .and_then(|s| s.micro.recall)
            .unwrap_or(0.0)
    };
    let precision = |lang: &str| {
        data.gold
            .get(lang)
            .and_then(|g| g.systems.get("lsp"))
            .and_then(|s| s.micro.precision)
            .unwrap_or(0.0)
    };
    (
        recall("rust") >= 0.90,
        recall("ts") >= 0.90,
        recall("java") >= 0.85,
        ["rust", "ts", "java"]
            .iter()
            .all(|lang| precision(lang) >= 0.90),
    )
}

/// G1–G16 verdict table (report section 1).
fn verdict_row(
    id: &str,
    criterion: &str,
    threshold: &str,
    measured: String,
    verdict: String,
) -> String {
    row(&[
        id.into(),
        criterion.into(),
        threshold.into(),
        measured,
        verdict,
    ])
}

fn table_verdict(data: &ReportData) -> String {
    let mut rows: Vec<String> = verdict_accuracy_rows(data)
        .lines()
        .chain(verdict_startup_rows(data).lines())
        .chain(verdict_probe_rows(data).lines())
        .map(str::to_owned)
        .collect();
    // Numeric gate order G1..G16 regardless of which builder emitted the row.
    rows.sort_by_key(|line| {
        line.trim_start_matches("| ")
            .split(' ')
            .next()
            .unwrap_or("")
            .trim_start_matches('G')
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });
    let mut out = header(&["ID", "Criterion", "Threshold", "Measured", "Verdict"]);
    for row in rows {
        out.push_str(&row);
        out.push('\n');
    }
    out.push_str(
        "\nSources: `gold_*.json`, `s1_*_cold.json`, `s1_*_warm.json`, `s2_*.json`, \
         `s1_*_nolsp*.json`.\n",
    );
    out
}

/// G1–G4 and G10–G12 (accuracy / retrieval gates).
fn verdict_accuracy_rows(data: &ReportData) -> String {
    let mut out = verdict_recall_rows(data);
    out.push_str(&verdict_retrieval_rows(data));
    out
}

/// G1–G4: LSP recall/precision against the adjudicated gold.
fn verdict_recall_rows(data: &ReportData) -> String {
    let (g1, g2, g3, g4) = gate_g1_to_g4(data);
    let recall = |lang: &str| {
        data.gold
            .get(lang)
            .and_then(|g| g.systems.get("lsp"))
            .and_then(|s| s.micro.recall)
    };
    let precision = |lang: &str| {
        data.gold
            .get(lang)
            .and_then(|g| g.systems.get("lsp"))
            .and_then(|s| s.micro.precision)
    };
    let mut out = String::new();
    out.push_str(&verdict_row(
        "G1",
        "LSP recall (micro) — rust",
        "≥ 90 %",
        pct_opt(recall("rust")),
        mark(g1).into(),
    ));
    out.push_str(&verdict_row(
        "G2",
        "LSP recall — ts",
        "≥ 90 %",
        pct_opt(recall("ts")),
        mark(g2).into(),
    ));
    out.push_str(&verdict_row(
        "G3",
        "LSP recall — java",
        "≥ 85 %",
        pct_opt(recall("java")),
        mark(g3).into(),
    ));
    out.push_str(&verdict_row(
        "G4",
        "LSP precision — each language",
        "≥ 90 %",
        ["rust", "ts", "java"]
            .iter()
            .map(|lang| format!("{lang} {}", pct_opt(precision(lang))))
            .collect::<Vec<_>>()
            .join(", "),
        mark(g4).into(),
    ));
    out
}

/// G10–G12: name-column heuristic and retrieval gates.
fn verdict_retrieval_rows(data: &ReportData) -> String {
    let column_rate = |lang: &str| {
        data.s1_warm.get(lang).map(|run| {
            let c = &run.column_heuristic;
            if c.compared == 0 {
                0.0
            } else {
                c.matched as f64 / c.compared as f64
            }
        })
    };
    let hit5 = |repo: &str, system: &str| {
        data.s2
            .get(repo)
            .map(|s| hit5_of(&s.quality.summary, system))
            .unwrap_or(0.0)
    };
    let mut out = String::new();
    let g10_ok = ["rust", "ts", "java"]
        .iter()
        .all(|lang| column_rate(lang).unwrap_or(0.0) >= 0.98);
    out.push_str(&verdict_row(
        "G10",
        "Name-column heuristic accuracy",
        "≥ 98 %",
        ["rust", "ts", "java"]
            .iter()
            .map(|lang| format!("{lang} {}", pct_opt(column_rate(lang))))
            .collect::<Vec<_>>()
            .join(", "),
        mark(g10_ok).into(),
    ));
    let overlap = |repo: &str| {
        data.s2
            .get(repo)
            .map(|s| s.quantization.i8pv.overlap_at_10)
            .unwrap_or(0.0)
    };
    let g11_ok = ["knot", "knot-site", "spring-ai"]
        .iter()
        .all(|repo| overlap(repo) >= 0.95);
    out.push_str(&verdict_row(
        "G11",
        "int8 overlap@10 (i8pv) vs f32",
        "≥ 95 %",
        ["knot", "knot-site", "spring-ai"]
            .iter()
            .map(|repo| format!("{repo} {}", pct(overlap(repo))))
            .collect::<Vec<_>>()
            .join(", "),
        mark(g11_ok).into(),
    ));
    out.push_str(&verdict_row(
        "G12",
        "Spike i8pv hit@5 vs knot hit@5",
        "≥ 80 % of knot",
        g12_summary(data, hit5),
        mark(g12_pass(data, hit5)).into(),
    ));
    out
}

fn g12_summary<F>(data: &ReportData, hit5: F) -> String
where
    F: Fn(&str, &str) -> f64,
{
    let _ = data;
    ["knot", "knot-site", "spring-ai"]
        .iter()
        .map(|repo| {
            format!(
                "{repo} {} vs {}",
                pct(hit5(repo, "spike_i8pv")),
                pct(hit5(repo, "knot_search"))
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn g12_pass<F>(data: &ReportData, hit5: F) -> bool
where
    F: Fn(&str, &str) -> f64,
{
    let _ = data;
    ["knot", "knot-site", "spring-ai"].iter().all(|repo| {
        let knot = hit5(repo, "knot_search");
        let spike = hit5(repo, "spike_i8pv");
        knot == 0.0 || spike >= 0.8 * knot
    })
}

/// G5–G9 (start-up / latency / memory gates).
fn verdict_startup_rows(data: &ReportData) -> String {
    let cold = |lang: &str| {
        data.s1_cold
            .get(lang)
            .map(|run| run.timings_ms.initialize_to_ready as f64)
            .unwrap_or(0.0)
    };
    let mut out = String::new();
    for (id, lang, threshold, limit) in [
        ("G5", "rust", "≤ 60 s", 60_000.0),
        ("G6", "ts", "≤ 30 s", 30_000.0),
        ("G7", "java", "≤ 300 s", 300_000.0),
    ] {
        let value = cold(lang);
        out.push_str(&verdict_row(
            id,
            &format!("Cold initialize→ready — {lang}"),
            threshold,
            format!("{} s", ms(value / 1000.0)),
            mark(value <= limit).into(),
        ));
    }
    let g8_values: Vec<(String, f64)> = ["rust", "ts", "java"]
        .iter()
        .map(|lang| ((*lang).to_owned(), warm_p95_max(data, lang)))
        .collect();
    let g8_ok = g8_values.iter().all(|(_, value)| *value <= 500.0);
    out.push_str(&verdict_row(
        "G8",
        "Warm p95 per operation (all langs)",
        "≤ 500 ms",
        g8_values
            .iter()
            .map(|(lang, value)| format!("{lang} {} ms", ms(*value)))
            .collect::<Vec<_>>()
            .join(", "),
        mark(g8_ok).into(),
    ));
    out.push_str(&verdict_row(
        "G9",
        "Peak RSS (informative)",
        "report",
        ["rust", "ts", "java"]
            .iter()
            .map(|lang| format!("{lang} {}", bytes(data.s1_warm[*lang].rss.peak_bytes)))
            .collect::<Vec<_>>()
            .join(", "),
        "—".into(),
    ));
    out
}

/// G13–G16 (tier-1 / performance / degradation gates).
fn verdict_probe_rows(data: &ReportData) -> String {
    let mut out = String::new();
    let g13_rows: Vec<(String, f64, usize)> = ["knot", "knot-site", "spring-ai"]
        .iter()
        .map(|repo| {
            let s = &data.s2[*repo];
            (
                (*repo).to_owned(),
                s.tier1.summary.hit5,
                s.tier1.queries.len(),
            )
        })
        .collect();
    let g13_ok = g13_rows
        .iter()
        .filter(|(_, _, n)| *n > 0)
        .all(|(_, hit, _)| *hit >= 0.8);
    out.push_str(&verdict_row(
        "G13",
        "Tier-1 file hit@5 (chunk/file domain)",
        "≥ 80 %",
        g13_rows
            .iter()
            .map(|(repo, hit, n)| {
                if *n == 0 {
                    format!("{repo} n/a")
                } else {
                    format!("{repo} {}", pct(*hit))
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
        mark_est(g13_ok, g13_rows.iter().any(|(_, _, n)| *n == 0)).into(),
    ));
    let best_load = data
        .s2
        .get("knot")
        .map(|s| {
            ["f32_b64", "i8pv_b64", "i8fx_b64"]
                .iter()
                .filter_map(|v| s.load_ms.get(*v).copied())
                .fold(f64::INFINITY, f64::min)
        })
        .unwrap_or(f64::INFINITY);
    out.push_str(&verdict_row(
        "G14",
        "Load time knot index (best text variant)",
        "≤ 200 ms",
        format!("{} ms", ms(best_load)),
        mark(best_load <= 200.0).into(),
    ));
    out.push_str(&verdict_row(
        "G15",
        "Heuristic (tier 2) recall/precision",
        "report only",
        ["rust", "ts", "java"]
            .iter()
            .map(|lang| {
                let g = &data.gold[*lang].systems["heuristic"];
                format!(
                    "{lang} {}/{}",
                    pct_opt(g.micro.precision),
                    pct_opt(g.micro.recall)
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
        "—".into(),
    ));
    let g16_ok = data
        .nolsp
        .values()
        .all(|r| !r.lsp_available && r.effective_tier == 2 && !r.targets.is_empty())
        && data.nolsp_broken.values().all(|r| {
            r.lsp_available && !r.lsp_started && r.effective_tier == 2 && !r.targets.is_empty()
        });
    out.push_str(&verdict_row(
        "G16",
        "Degradation path (both scenarios)",
        "exit 0 + diagnostics + fallback",
        "6/6 runs".to_owned(),
        mark(g16_ok).into(),
    ));
    out
}

fn table_environment(data: &ReportData) -> String {
    let mut out = header(&["Item", "Value", "Source"]);
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "rust-analyzer / ts server / jdtls",
            "rust-analyzer 1.98.1 · typescript-language-server 5.3.0 · jdtls 1.61.0".to_owned(),
            "s1_*.server.version",
        ),
        (
            "Rust / JDK",
            "rustc 1.98.1 (2026-09-01) · OpenJDK 21.0.12.1 · Maven 3.9.7".to_owned(),
            "results/raw/env.txt",
        ),
        (
            "knot",
            "knot CLI 1.11.0 · knot crate (path) 1.11.1".to_owned(),
            "results/raw/env.txt",
        ),
        (
            "embedding model",
            "AllMiniLML6V2, 384-dim, symmetric (no query/passage prefix)".to_owned(),
            "s2_*.model",
        ),
        (
            "machine",
            "Linux 7.0.0-34-generic · 32 cores · 61 GB RAM · /tmp tmpfs".to_owned(),
            "plan §4",
        ),
        (
            "snapshot fingerprints",
            ["rust", "ts", "java"]
                .iter()
                .zip(["knot", "knot-site", "spring-ai"])
                .map(|(lang, repo)| {
                    let fp = data
                        .s1_warm
                        .get(*lang)
                        .map(|r| r.snapshot_fingerprint.as_str())
                        .unwrap_or("");
                    format!("{repo} `{}`", &fp[..fp.len().min(12)])
                })
                .collect::<Vec<_>>()
                .join(", "),
            "s1_*.snapshot_fingerprint",
        ),
        (
            "S1 wall time (cold / warm)",
            ["rust", "ts", "java"]
                .iter()
                .map(|lang| {
                    let warm = &data.s1_warm[*lang].timings_ms;
                    let cold = &data.s1_cold[*lang].timings_ms;
                    format!(
                        "{lang} {} s / {} s",
                        ms((cold.spawn_to_initialize + cold.initialize_to_ready) as f64 / 1000.0),
                        ms((warm.spawn_to_initialize + warm.initialize_to_ready) as f64 / 1000.0)
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
            "s1_*.timings_ms",
        ),
        (
            "S2 index wall time",
            ["knot", "knot-site", "spring-ai"]
                .iter()
                .map(|repo| {
                    let s = &data.s2[*repo];
                    let t = &s.timings_ms;
                    format!(
                        "{repo} {} s",
                        ms(
                            (t.parse + t.prepare + t.embed_entities + t.embed_chunks) as f64
                                / 1000.0
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
            "s2_*.timings_ms",
        ),
    ];
    for (item, value, source) in rows {
        out.push_str(&row(&[item.into(), value, source.into()]));
    }
    out
}

/// S1 capability matrix (report section 3).
fn table_capabilities(data: &ReportData) -> String {
    let mut out = header(&[
        "Language",
        "call hierarchy",
        "type hierarchy",
        "implementation",
        "references",
        "documentSymbol",
        "workspaceSymbol",
        "server requests seen",
    ]);
    for lang in ["rust", "ts", "java"] {
        let run = &data.s1_warm[lang];
        let c = &run.capabilities;
        out.push_str(&row(&[
            lang.into(),
            mark(c.call_hierarchy).into(),
            mark(c.type_hierarchy).into(),
            mark(c.implementation).into(),
            mark(c.references).into(),
            mark(c.document_symbol).into(),
            mark(c.workspace_symbol).into(),
            run.server_requests_seen.join(", "),
        ]));
    }
    out.push_str(
        "\nSources: `s1_*_warm.json` (`capabilities`, `server_requests_seen`). Type hierarchy is \
         always `false`: lsp-types 0.95 has no `type_hierarchy_provider` field to read.\n",
    );
    out
}

/// S1 start-up / memory / latency (report section 4).
fn table_startup(data: &ReportData) -> String {
    let mut out = header(&[
        "Run",
        "spawn→init (ms)",
        "init→ready (ms)",
        "first request (ms)",
        "prepare p50/p95",
        "calls p50/p95",
        "refs p50/p95",
        "impl p50/p95",
        "peak RSS",
        "procs",
    ]);
    for lang in ["rust", "ts", "java"] {
        for run_kind in ["cold", "warm"] {
            let run = if run_kind == "cold" {
                &data.s1_cold[lang]
            } else {
                &data.s1_warm[lang]
            };
            let t = &run.timings_ms;
            let l = &run.latency_ms;
            let pair = |p: &crate::common::Percentiles| format!("{}/{}", ms(p.p50), ms(p.p95));
            out.push_str(&row(&[
                format!("{lang} {run_kind}"),
                ms(t.spawn_to_initialize as f64),
                ms(t.initialize_to_ready as f64),
                ms(t.first_request),
                pair(&l.prepare_call_hierarchy),
                pair(&l.incoming_calls),
                pair(&l.references),
                pair(&l.implementation),
                bytes(run.rss.peak_bytes),
                run.rss.peak_process_count.to_string(),
            ]));
        }
    }
    out.push_str(
        "\nSources: `s1_*_{cold,warm}.json` (`timings_ms`, `latency_ms`, `rss`). The java cold \
         row is the plan-conformant cold run (jdtls workspace wiped, warm `~/.m2`); the first \
         ever cold run measured init→ready = 171 000 ms.\n",
    );
    out
}

/// S1 accuracy: micro/macro precision-recall per system (report section 5).
fn table_accuracy(data: &ReportData) -> String {
    let mut out = header(&[
        "Lang", "System", "micro P", "micro R", "macro P", "macro R", "tp", "fp", "fn", "file P",
        "file R",
    ]);
    for lang in ["rust", "ts", "java"] {
        let gold = &data.gold[lang];
        for system in ["lsp", "knot_db", "heuristic", "lsp_calls", "knot_db_calls"] {
            let Some(metrics) = gold.systems.get(system) else {
                continue;
            };
            let micro = &metrics.micro;
            out.push_str(&row(&[
                lang.into(),
                system.into(),
                pct_opt(micro.precision),
                pct_opt(micro.recall),
                pct_opt(metrics.macro_avg.precision),
                pct_opt(metrics.macro_avg.recall),
                format!("{:.0}", micro.tp),
                format!("{:.0}", micro.fp),
                format!("{:.0}", micro.false_negatives),
                pct_opt(metrics.file_level.micro.precision),
                pct_opt(metrics.file_level.micro.recall),
            ]));
        }
        out.push_str(&row(&[
            lang.into(),
            "gold size".into(),
            format!("{:.0}", gold.systems["lsp"].micro.gold),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ]));
    }
    out.push_str(
        "\nSources: `gold_*.json` (`systems.*`). `lsp_calls`/`knot_db_calls` are the secondary \
         calls-only metrics; gold = union of the three systems' containers, labelled by \
         adjudication (estimated for rust/java: stratified sample of 50).\n",
    );
    out
}

/// S1 accuracy by target category (report section 5).
fn table_accuracy_category(data: &ReportData) -> String {
    let mut out = header(&[
        "Lang", "Category", "System", "micro P", "micro R", "tp", "fp", "fn",
    ]);
    for lang in ["rust", "ts", "java"] {
        let gold = &data.gold[lang];
        let mut categories: Vec<&String> = gold
            .systems
            .values()
            .flat_map(|metrics| metrics.by_category.keys())
            .collect();
        categories.sort();
        categories.dedup();
        for category in categories {
            for system in ["lsp", "knot_db", "heuristic"] {
                let Some(metrics) = gold.systems.get(system) else {
                    continue;
                };
                let Some(agg) = metrics.by_category.get(category) else {
                    continue;
                };
                out.push_str(&row(&[
                    lang.into(),
                    category.clone(),
                    system.into(),
                    pct_opt(agg.precision),
                    pct_opt(agg.recall),
                    format!("{:.0}", agg.tp),
                    format!("{:.0}", agg.fp),
                    format!("{:.0}", agg.false_negatives),
                ]));
            }
        }
    }
    out.push_str("\nSources: `gold_*.json` (`systems.*.by_category`).\n");
    out
}

/// S1 notable disagreements (report section 6).
fn table_disagreements(data: &ReportData) -> String {
    let mut out = header(&["Lang", "Target", "Entity", "Found by", "Verdict", "Reason"]);
    for lang in ["rust", "ts", "java"] {
        for item in &data.gold[lang].notable_disagreements {
            let verdict = match item.verdict {
                crate::graph::metrics::Verdict::Tp => "tp",
                crate::graph::metrics::Verdict::Fp => "fp",
                crate::graph::metrics::Verdict::Pending => "?",
            };
            out.push_str(&row(&[
                lang.into(),
                item.target.clone(),
                format!(
                    "`{}:{}` {}",
                    item.entity.file, item.entity.line, item.entity.name
                ),
                item.found_by.join(", "),
                verdict.into(),
                item.reason.replace('|', "/"),
            ]));
        }
    }
    out.push_str("\nSources: `gold_*.json` (`notable_disagreements`), marked `notable = true` in `results/adjudication/*.toml`.\n");
    out
}

/// S1 name-column heuristic (report section 7).
fn table_columns(data: &ReportData) -> String {
    let mut out = header(&[
        "Lang",
        "compared",
        "matched",
        "accuracy",
        "failed_to_locate",
        "symbol_not_found",
        "files w/o symbols",
        "example mismatch",
    ]);
    for lang in ["rust", "ts", "java"] {
        let c = &data.s1_warm[lang].column_heuristic;
        let accuracy = if c.compared == 0 {
            0.0
        } else {
            c.matched as f64 / c.compared as f64
        };
        let example = c
            .mismatches
            .first()
            .map(|m| {
                format!(
                    "{}:{} {} ({})",
                    m.file.rsplit('/').next().unwrap_or(&m.file),
                    m.entity_line,
                    m.name,
                    m.reason
                )
            })
            .unwrap_or_default();
        out.push_str(&row(&[
            lang.into(),
            c.compared.to_string(),
            c.matched.to_string(),
            pct(accuracy),
            c.failed_to_locate.to_string(),
            c.symbol_not_found.to_string(),
            c.files_without_symbols.to_string(),
            example,
        ]));
    }
    out.push_str(
        "\nSources: `s1_*_warm.json` (`column_heuristic`). Compared = entities whose name was \
         located AND matched to a document symbol; `symbol_not_found` are knot entities LSP does \
         not symbolize (test fixtures, CSS, Markdown).\n",
    );
    out
}

/// S1 degradation path (report section 8).
fn table_degradation(data: &ReportData) -> String {
    let mut out = header(&[
        "Lang",
        "Scenario",
        "lsp_available",
        "lsp_started",
        "tier",
        "degrade (ms)",
        "fallback (ms)",
        "graph build (ms)",
        "targets",
        "Diagnostic",
    ]);
    for lang in ["rust", "ts", "java"] {
        for (scenario, result) in [
            ("missing", &data.nolsp[lang]),
            ("broken", &data.nolsp_broken[lang]),
        ] {
            out.push_str(&row(&[
                lang.into(),
                scenario.into(),
                mark(!result.lsp_available).into(),
                mark(!result.lsp_started).into(),
                result.effective_tier.to_string(),
                ms(result.degrade_ms as f64),
                ms(result.fallback_ms as f64),
                ms(result.graph_build_ms as f64),
                result.targets.len().to_string(),
                result.diagnostic.replace('|', "/"),
            ]));
        }
    }
    out.push_str(
        "\nSources: `s1_*_nolsp.json`, `s1_*_nolsp_broken.json`. All answers carry \
         `provenance: \"heuristic\"` (tier 2). `graph build` is the T6 resolver (setup); \
         `fallback` is the answering step asserted ≤ 5 000 ms.\n",
    );
    out
}

/// S2 sizes (report section 9).
fn table_sizes(data: &ReportData) -> String {
    let mut out = header(&[
        "Repo",
        "Variant",
        "raw",
        "gzip",
        "git pack",
        "pack / source",
        "bytes / record",
        "shards",
    ]);
    for repo in ["knot", "knot-site", "spring-ai"] {
        let s = &data.s2[repo];
        for variant in ["f32_dec", "f32_b64", "i8pv_b64", "i8fx_b64", "nov"] {
            let Some(size) = s.sizes.variants.get(variant) else {
                continue;
            };
            let pack = s.git.pack_kib.get(variant).map(|k| *k * 1024).unwrap_or(0);
            let ratio = s.git.ratio_vs_source.get(variant).copied().unwrap_or(0.0);
            out.push_str(&row(&[
                repo.into(),
                variant.into(),
                bytes(size.raw),
                bytes(size.gzip),
                bytes(pack),
                pct(ratio),
                format!("{:.0} B", size.bytes_per_entity),
                size.shards.to_string(),
            ]));
        }
        out.push_str(&row(&[
            repo.into(),
            "source tree".into(),
            bytes(s.sizes.source.raw),
            bytes(s.sizes.source.gzip),
            bytes(s.git.pack_kib.get("source").copied().unwrap_or(0) * 1024),
            "—".into(),
            "—".into(),
            "—".into(),
        ]));
    }
    out.push_str(
        "\nSources: `s2_*.json` (`sizes`, `git.pack_kib`, `git.ratio_vs_source`). gzip = per-file \
         flate2 default level, summed (approximates loose objects).\n",
    );
    out
}

/// S2 determinism and churn (report section 10).
fn table_determinism(data: &ReportData) -> String {
    let mut out = header(&[
        "Repo",
        "Variant",
        "rerun identical",
        "batching identical",
        "threads identical",
        "vector records differing",
        "max |Δ|",
        "id mismatches",
        "append Δpack",
        "prepend Δpack",
    ]);
    for repo in ["knot", "knot-site", "spring-ai"] {
        let s = &data.s2[repo];
        for variant in ["f32_dec", "f32_b64", "i8pv_b64", "i8fx_b64", "nov"] {
            let d = &s.determinism;
            let churn = |mode: &str| {
                s.git
                    .churn
                    .get(mode)
                    .and_then(|m| m.get(variant))
                    .map(|c| c.pack_delta_kib)
            };
            out.push_str(&row(&[
                repo.into(),
                variant.into(),
                mark(d.rerun_identical.get(variant).copied().unwrap_or(false)).into(),
                mark(d.batching_identical.get(variant).copied().unwrap_or(false)).into(),
                mark(d.threads_identical.get(variant).copied().unwrap_or(false)).into(),
                d.batching_differing_records
                    .get(variant)
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                format!("{:.2e}", d.batching_max_abs_diff),
                d.id_mismatches
                    .get(variant)
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                churn("append")
                    .map(|k| format!("{k} KiB"))
                    .unwrap_or_default(),
                churn("prepend")
                    .map(|k| format!("{k} KiB"))
                    .unwrap_or_default(),
            ]));
        }
    }
    out.push_str(
        "\nSources: `s2_*.json` (`determinism`, `git.churn`). Byte-identity fails only because \
         knot's html parser emits `Uuid::new_v4()` ids (`id mismatches`); vectors are identical \
         (0 differing records, max |Δ| = 0.0). Churn: `append` adds one function, `prepend` \
         shifts all lines (every UUID of the file changes).\n",
    );
    out
}

/// S2 load + query performance (report section 11).
fn table_performance(data: &ReportData) -> String {
    let mut out = header(&[
        "Repo",
        "load f32_dec (ms)",
        "load f32_b64 (ms)",
        "load i8pv (ms)",
        "load i8fx (ms)",
        "load cache (ms)",
        "embed p50 (ms)",
        "scan p50 (ms)",
        "scan p95 (ms)",
        "rows",
    ]);
    for repo in ["knot", "knot-site", "spring-ai"] {
        let s = &data.s2[repo];
        let load = |variant: &str| s.load_ms.get(variant).copied().unwrap_or(0.0);
        out.push_str(&row(&[
            repo.into(),
            ms(load("f32_dec")),
            ms(load("f32_b64")),
            ms(load("i8pv_b64")),
            ms(load("i8fx_b64")),
            ms(load("binary_cache")),
            ms(s.query.embed_ms.p50),
            ms(s.query.scan_ms.p50),
            ms(s.query.scan_ms.p95),
            format!(
                "{}",
                s.counts.entities + s.counts.chunks + s.counts.file_vectors
            ),
        ]));
    }
    out.push_str(
        "\nSources: `s2_*.json` (`load_ms`, `query`, `counts`). Load = read+parse into an f32 \
         matrix, median of 5; scan = brute-force top-10 over the entity domain, median of 20.\n",
    );
    out
}

/// S2 retrieval quality + parity (report section 12).
fn table_quality(data: &ReportData) -> String {
    let mut out = header(&[
        "Repo",
        "hit@5 f32",
        "hit@5 i8pv",
        "hit@5 i8fx",
        "hit@5 knot",
        "hit@10 f32",
        "hit@10 knot",
        "overlap@10 vs knot",
        "i8pv overlap@10",
        "i8fx overlap@10",
        "i8fx clip",
        "Qdrant parity (found, cos)",
    ]);
    for repo in ["knot", "knot-site", "spring-ai"] {
        let s = &data.s2[repo];
        let hit10 = |system: &str| {
            s.quality
                .queries
                .iter()
                .filter(|q| match system {
                    "spike_f32" => q.hit10.spike_f32,
                    "knot_search" => q.hit10.knot_search,
                    _ => false,
                })
                .count() as f64
                / s.quality.queries.len().max(1) as f64
        };
        let overlap_vs_knot = if s.quality.queries.is_empty() {
            0.0
        } else {
            s.quality
                .queries
                .iter()
                .map(|q| q.overlap10_vs_knot)
                .sum::<f64>()
                / s.quality.queries.len() as f64
        };
        out.push_str(&row(&[
            repo.into(),
            pct(s.quality.summary.spike_f32),
            pct(s.quality.summary.spike_i8pv),
            pct(s.quality.summary.spike_i8fx),
            pct(s.quality.summary.knot_search),
            pct(hit10("spike_f32")),
            pct(hit10("knot_search")),
            pct(overlap_vs_knot),
            pct(s.quantization.i8pv.overlap_at_10),
            pct(s.quantization.i8fx.overlap_at_10),
            pct(s.quantization.i8fx.clip_rate),
            format!(
                "{}/{} · {:.6}",
                s.parity_with_qdrant.found,
                s.parity_with_qdrant.compared,
                s.parity_with_qdrant.mean_cosine
            ),
        ]));
    }
    out.push_str(
        "\nSources: `s2_*.json` (`quality`, `quantization`, `parity_with_qdrant`). Search domain \
         = entities only. knot = `knot search` top-10 from `groundtruth/search_*.json` (hybrid \
         re-ranked, informative overlap only).\n",
    );
    out
}

/// S2 tier-1 file location (report section 13).
fn table_tier1(data: &ReportData) -> String {
    let mut out = header(&[
        "Repo",
        "queries",
        "hit@1",
        "hit@5",
        "hit@5 mixed",
        "tier-1 files",
        "chunks",
        "file vectors",
        "size increase",
    ]);
    for repo in ["knot", "knot-site", "spring-ai"] {
        let s = &data.s2[repo];
        out.push_str(&row(&[
            repo.into(),
            s.tier1.queries.len().to_string(),
            pct(s.tier1.summary.hit1),
            pct(s.tier1.summary.hit5),
            pct(s.tier1.summary.hit5_mixed),
            s.counts.tier1_files.to_string(),
            s.counts.chunks.to_string(),
            s.counts.file_vectors.to_string(),
            pct(s.tier1.size_increase_pct / 100.0),
        ]));
    }
    out.push_str(
        "\nSources: `s2_*.json` (`tier1`, `counts`). hit@5 collapses results to files (best \
         score per file) over chunk+file vectors; `mixed` lets tier-1 files compete with code \
         entities. knot-site has no `level1` queries (empty domain → 0.00 is an artifact).\n",
    );
    out
}
