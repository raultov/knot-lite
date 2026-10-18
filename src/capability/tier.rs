//! Tier model (3/2/1/0) and `StatusReport` assembly.
//!
//! Tier 3 means the language-server binary was found: `find_callers` then asks
//! the server (live, or pre-computed at index time) and falls back to the
//! heuristic edges per target when it is unavailable.

use std::collections::BTreeMap;

use serde::Serialize;

use super::languages::{LspLang, lsp_lang_for_label};
use super::lsp_registry::LspStatus;
use super::scan::{ExcludeReason, FileClass, ScanResult};
use crate::config::Config;
use crate::index::layout::{CommitMode, INDEX_DIR, MANIFEST_FILE};
use crate::index::manifest::{IndexCounts, IndexState, LspManifest};
use crate::lsp::pool::{PoolSnapshot, SlotSnapshot};
use crate::lsp::probe::ProbeResult;
use crate::lsp::settings::LspSettings;

/// Schema identifier of the `status` report (`schema` key in the JSON).
pub const STATUS_SCHEMA_V1: &str = "knot-lite/status/v1";

/// Maximum number of keys listed under `files_only.top`.
pub const TOP_KEYS_LIMIT: usize = 10;

/// Note attached to tier-3 rows.
pub const TIER3_NOTE: &str = "Tier 3: find_callers asks the language server (live, or pre-computed by knot-lite index for high fan-in targets) and falls back to heuristic edges per target when the server is unavailable.";

/// Freshness state of an index relative to the working tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Freshness {
    /// `"fresh"`, `"stale"` or `"unknown"`.
    pub state: &'static str,
    /// Files added to the working tree since indexing.
    pub added: usize,
    /// Files removed from the working tree since indexing.
    pub removed: usize,
    /// Files whose content hash changed since indexing.
    pub changed: usize,
}

/// Rich details of a present index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexDetails {
    /// Model name recorded in the manifest.
    pub model: String,
    /// Vector dimension.
    pub dim: u32,
    /// Wire encoding.
    pub encoding: String,
    /// Whether this model was validated by Phase 0 measurements (MiniLM only).
    pub model_validated: bool,
    /// Model name resolved from current configuration.
    pub configured_model: String,
    /// Git tracking mode.
    pub commit_mode: CommitMode,
    /// knot version that built the index.
    pub knot_version: String,
    /// Creation timestamp.
    pub created_at: String,
    /// Last update timestamp.
    pub updated_at: String,
    /// Counts recorded in the manifest.
    pub counts: IndexCounts,
    /// Freshness against current file selection.
    pub freshness: Freshness,
    /// Pre-computed language-server data of the last run, when there was a stage.
    pub lsp: Option<LspDetails>,
}

/// The pre-computation summary of the last index run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LspDetails {
    /// `auto`, `all` or `none`.
    pub policy: String,
    /// `lsp-refs` or `heuristic`.
    pub selection: String,
    /// Per language directory name (`rust`, `ts`, `java`).
    pub languages: BTreeMap<String, LspLangDetails>,
}

/// What the last run did for one language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LspLangDetails {
    /// `computed`, `reused`, `skipped`, `failed` or `none`.
    pub status: String,
    /// Targets that produced an answer.
    pub targets_ok: usize,
    /// Targets that failed or timed out.
    pub targets_failed: usize,
    /// Edges written.
    pub edges: usize,
    /// Milliseconds the language took.
    pub ms: u64,
    /// Candidates the `references` probe answered.
    pub probed: usize,
}

impl From<&LspManifest> for LspDetails {
    fn from(manifest: &LspManifest) -> Self {
        Self {
            policy: manifest.policy.clone(),
            selection: manifest.selection.clone(),
            languages: manifest
                .languages
                .iter()
                .map(|(language, lang)| {
                    (
                        language.clone(),
                        LspLangDetails {
                            status: lang.status.clone(),
                            targets_ok: lang.targets_ok,
                            targets_failed: lang.targets_failed,
                            edges: lang.edges,
                            ms: lang.ms,
                            probed: lang.probed,
                        },
                    )
                })
                .collect(),
        }
    }
}

/// The language-server settings in force, as `status` shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LspSettingsView {
    /// Whether language servers may be started.
    pub enabled: bool,
    /// Whether the repository's build code may run (rust-analyzer, jdtls).
    pub build_exec: bool,
    /// Root of the per-repository scratch directories.
    pub cache_dir: String,
    /// Whether the MCP server starts servers eagerly.
    pub warmup: bool,
    /// Idle seconds before a server is stopped (0 = never).
    pub idle_s: u64,
    /// Live targets per `find_callers` call.
    pub max_live_targets: usize,
    /// Live seconds per `find_callers` call.
    pub deadline_s: u64,
}

impl From<&LspSettings> for LspSettingsView {
    fn from(settings: &LspSettings) -> Self {
        Self {
            enabled: settings.enabled,
            build_exec: settings.build_exec,
            cache_dir: settings.cache_root.display().to_string(),
            warmup: settings.warmup,
            idle_s: settings.idle.as_secs(),
            max_live_targets: settings.max_live_targets,
            deadline_s: settings.deadline.as_secs(),
        }
    }
}

/// What the report knows about the language servers beyond their binaries.
#[derive(Debug, Clone, Copy, Default)]
pub struct LspView<'a> {
    /// The manifest's pre-computation table, when the index has one.
    pub precompute: Option<&'a LspManifest>,
    /// The live state of the pool (MCP only; the CLI never starts servers for `status`).
    pub runtime: Option<&'a PoolSnapshot>,
}

/// One language row of the capability matrix.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TierRow {
    /// Language label (Appendix B).
    pub language: String,
    /// Number of parsed files with this label.
    pub files: usize,
    /// Whether a parser exists (always `true` for parsed rows).
    pub parser: bool,
    /// Language-server detection result (only for LSP-capable labels).
    pub lsp: Option<LspStatus>,
    /// Tier: 3 (parser + LSP) or 2 (parser only).
    pub tier: u8,
    /// Graph source: `"lsp"` or `"heuristic"`.
    pub graph: &'static str,
    /// Caveat or install hint.
    pub note: Option<String>,
    /// What the last index run did for this language (`computed`, `reused`,
    /// `skipped`, `failed`, `none`); `null` without a pre-computation stage.
    pub lsp_precompute: Option<String>,
    /// The live state of this language's server; `null` outside the MCP server.
    pub lsp_runtime: Option<SlotSnapshot>,
    /// Edit-sync mode configured for this language server (P6-D10, §10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_sync: Option<&'static str>,
}

/// Repository identity in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoInfo {
    /// Logical repository name.
    pub name: String,
    /// Repository root (display form).
    pub path: String,
}

/// Index state plus the repo-relative manifest path and optional details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexInfo {
    /// The state itself (`state` tag).
    #[serde(flatten)]
    pub state: IndexState,
    /// Repo-relative path of the manifest.
    pub manifest: String,
    /// Rich details when state is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<IndexDetails>,
}

/// One `files_only` group key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyCount {
    /// Group key (`.sh`, `Makefile`, …).
    pub key: String,
    /// Number of files with this key.
    pub files: usize,
}

/// Tier-1 summary: all files-only files plus the biggest groups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilesOnlySummary {
    /// Total number of files-only files.
    pub files: usize,
    /// At most [`TOP_KEYS_LIMIT`] keys, by count desc then key asc.
    pub top: Vec<KeyCount>,
}

/// Tier-0 summary: excluded files by reason (all reasons always present).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExcludedSummary {
    /// Total number of excluded files.
    pub files: usize,
    /// Counts per [`ExcludeReason`], including zeros.
    pub by_reason: BTreeMap<ExcludeReason, usize>,
}

/// Aggregate file counts per tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Totals {
    /// All classified files.
    pub files: usize,
    /// Files in tier-3 rows.
    pub tier3: usize,
    /// Files in tier-2 rows.
    pub tier2: usize,
    /// Files-only files (tier 1).
    pub tier1: usize,
    /// Excluded files (tier 0).
    pub tier0: usize,
}

/// The `knot-lite/status/v1` report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    /// Always [`STATUS_SCHEMA_V1`].
    pub schema: String,
    /// The knot-lite version that produced the report.
    pub knot_lite_version: String,
    /// Repository identity.
    pub repo: RepoInfo,
    /// Index state.
    pub index: IndexInfo,
    /// Language rows, tier 3 first.
    pub languages: Vec<TierRow>,
    /// Tier-1 summary.
    pub files_only: FilesOnlySummary,
    /// Tier-0 summary.
    pub excluded: ExcludedSummary,
    /// Aggregate counts.
    pub totals: Totals,
    /// Fixed caveats.
    pub notes: Vec<String>,
    /// The language-server settings in force.
    pub lsp_settings: LspSettingsView,
    /// Ranking of `search_hybrid_context` (`KNOT_LITE_SEARCH`).
    pub search_mode: crate::config::SearchMode,
    /// Invalid or clamped settings, one line each.
    pub warnings: Vec<String>,
    /// What `--probe-lsp` found, per language (only when it ran).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_probe: Option<BTreeMap<String, ProbeResult>>,
}

/// Assemble the report without live language-server information.
pub fn build_report(
    cfg: &Config,
    scan: &ScanResult,
    index: IndexState,
    detect: &dyn Fn(LspLang) -> LspStatus,
) -> StatusReport {
    build_report_with(cfg, scan, index, detect, &LspView::default())
}

/// Assemble the report. `detect` is called at most once per [`LspLang`].
pub fn build_report_with(
    cfg: &Config,
    scan: &ScanResult,
    index: IndexState,
    detect: &dyn Fn(LspLang) -> LspStatus,
    view: &LspView<'_>,
) -> StatusReport {
    let (per_label, files_only, excluded) = summarize(&scan.files);
    let mut detected: BTreeMap<LspLang, LspStatus> = BTreeMap::new();
    let mut languages: Vec<TierRow> = per_label
        .iter()
        .map(|(label, files)| {
            let lang = lsp_lang_for_label(label);
            let lsp = lang.map(|lang| detected.entry(lang).or_insert_with(|| detect(lang)).clone());
            let mut row = tier_row(label, *files, lsp);
            if let Some(lang) = lang {
                row.lsp_precompute = view
                    .precompute
                    .and_then(|manifest| manifest.languages.get(lang.dir_name()))
                    .map(|lang| lang.status.clone());
                row.lsp_runtime = view
                    .runtime
                    .and_then(|runtime| runtime.slots.get(lang.dir_name()))
                    .cloned();
                row.lsp_sync = Some(cfg.lsp.sync.label());
            }
            row
        })
        .collect();
    sort_rows(&mut languages);

    let totals = totals(&languages, &files_only, &excluded);
    StatusReport {
        schema: STATUS_SCHEMA_V1.to_string(),
        knot_lite_version: env!("CARGO_PKG_VERSION").to_string(),
        repo: RepoInfo {
            name: cfg.repo_name.clone(),
            path: cfg.repo_path.display().to_string(),
        },
        index: IndexInfo {
            state: index,
            manifest: format!("{INDEX_DIR}/{MANIFEST_FILE}"),
            details: None,
        },
        languages,
        files_only,
        excluded,
        totals,
        notes: vec![
            "Gitignored files are not counted.".to_string(),
            TIER3_NOTE.to_string(),
        ],
        lsp_settings: LspSettingsView::from(&cfg.lsp),
        search_mode: cfg.search.mode,
        warnings: cfg
            .lsp
            .warnings
            .iter()
            .chain(&cfg.search.warnings)
            .cloned()
            .collect(),
        lsp_probe: None,
    }
}

/// Bucket the classified files into labels, files-only keys and exclusions.
fn summarize(
    files: &[(String, FileClass)],
) -> (BTreeMap<String, usize>, FilesOnlySummary, ExcludedSummary) {
    let mut per_label: BTreeMap<String, usize> = BTreeMap::new();
    let mut only_keys: BTreeMap<String, usize> = BTreeMap::new();
    let mut only_files = 0usize;
    let mut by_reason: BTreeMap<ExcludeReason, usize> = BTreeMap::new();
    for reason in [
        ExcludeReason::Binary,
        ExcludeReason::LockFile,
        ExcludeReason::TooLarge,
    ] {
        by_reason.insert(reason, 0);
    }
    let mut excluded_files = 0usize;

    for (_, class) in files {
        match class {
            FileClass::Parsed { label } => {
                *per_label.entry((*label).to_string()).or_default() += 1;
            }
            FileClass::FilesOnly { key } => {
                only_files += 1;
                *only_keys.entry(key.clone()).or_default() += 1;
            }
            FileClass::Excluded(reason) => {
                excluded_files += 1;
                *by_reason.entry(*reason).or_default() += 1;
            }
        }
    }

    let mut top: Vec<KeyCount> = only_keys
        .into_iter()
        .map(|(key, files)| KeyCount { key, files })
        .collect();
    top.sort_by(|a, b| b.files.cmp(&a.files).then(a.key.cmp(&b.key)));
    top.truncate(TOP_KEYS_LIMIT);

    (
        per_label,
        FilesOnlySummary {
            files: only_files,
            top,
        },
        ExcludedSummary {
            files: excluded_files,
            by_reason,
        },
    )
}

/// Build one language row from its LSP status (Phase 1 tier rules).
fn tier_row(label: &str, files: usize, lsp: Option<LspStatus>) -> TierRow {
    let (tier, graph, note) = match &lsp {
        Some(LspStatus::Found { .. } | LspStatus::Override { .. }) => {
            (3, "lsp", Some(TIER3_NOTE.to_string()))
        }
        Some(LspStatus::Missing { hint, .. } | LspStatus::OverrideInvalid { hint, .. }) => {
            (2, "heuristic", Some(hint.clone()))
        }
        None => (2, "heuristic", None),
    };
    TierRow {
        language: label.to_string(),
        files,
        parser: true,
        lsp,
        tier,
        graph,
        note,
        lsp_precompute: None,
        lsp_runtime: None,
        lsp_sync: None,
    }
}

/// Tier desc, files desc, language asc.
fn sort_rows(rows: &mut [TierRow]) {
    rows.sort_by(|a, b| {
        b.tier
            .cmp(&a.tier)
            .then(b.files.cmp(&a.files))
            .then(a.language.cmp(&b.language))
    });
}

fn totals(
    languages: &[TierRow],
    files_only: &FilesOnlySummary,
    excluded: &ExcludedSummary,
) -> Totals {
    let tier3 = languages
        .iter()
        .filter(|row| row.tier == 3)
        .map(|row| row.files)
        .sum();
    let tier2 = languages
        .iter()
        .filter(|row| row.tier == 2)
        .map(|row| row.files)
        .sum();
    Totals {
        files: tier3 + tier2 + files_only.files + excluded.files,
        tier3,
        tier2,
        tier1: files_only.files,
        tier0: excluded.files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        crate::config::test_support::config_for(std::path::PathBuf::from("/tmp/repo"))
    }

    fn found() -> LspStatus {
        LspStatus::Found {
            server: "s".to_string(),
            path: "/bin/s".to_string(),
        }
    }

    fn missing() -> LspStatus {
        LspStatus::Missing {
            server: "s".to_string(),
            searched_dirs: 2,
            hint: "install it".to_string(),
        }
    }

    fn scan_of(entries: &[(&str, FileClass)]) -> ScanResult {
        ScanResult {
            files: entries
                .iter()
                .map(|(p, c)| ((*p).to_string(), c.clone()))
                .collect(),
        }
    }

    fn parsed(label: &'static str) -> FileClass {
        FileClass::Parsed { label }
    }

    fn only(key: &str) -> FileClass {
        FileClass::FilesOnly {
            key: key.to_string(),
        }
    }

    #[test]
    fn rust_found_is_tier3_and_java_missing_is_tier2_with_hint() {
        let scan = scan_of(&[("a.rs", parsed("rust")), ("B.java", parsed("java"))]);
        let report = build_report(&cfg(), &scan, IndexState::Absent, &|lang| match lang {
            LspLang::Rust => found(),
            _ => missing(),
        });
        let rust = &report.languages[0];
        assert_eq!(rust.language, "rust");
        assert_eq!(rust.tier, 3);
        assert_eq!(rust.graph, "lsp");
        assert_eq!(rust.note.as_deref(), Some(TIER3_NOTE));
        let java = &report.languages[1];
        assert_eq!(java.language, "java");
        assert_eq!(java.tier, 2);
        assert_eq!(java.graph, "heuristic");
        assert_eq!(java.note.as_deref(), Some("install it"));
    }

    #[test]
    fn ts_and_js_share_one_detect_call() {
        let scan = scan_of(&[
            ("a.ts", parsed("typescript")),
            ("b.js", parsed("javascript")),
        ]);
        let calls = std::cell::Cell::new(0usize);
        let report = build_report(&cfg(), &scan, IndexState::Absent, &|_| {
            calls.set(calls.get() + 1);
            found()
        });
        assert_eq!(calls.get(), 1, "typescript and javascript share one lookup");
        assert_eq!(report.languages.len(), 2);
        assert!(report.languages.iter().all(|row| row.tier == 3));
    }

    #[test]
    fn markdown_is_tier2_without_lsp() {
        let scan = scan_of(&[("README.md", parsed("markdown"))]);
        let report = build_report(&cfg(), &scan, IndexState::Absent, &|_| {
            panic!("detect must not be called for non-LSP labels")
        });
        let row = &report.languages[0];
        assert_eq!(row.tier, 2);
        assert_eq!(row.graph, "heuristic");
        assert_eq!(row.lsp, None);
        assert_eq!(row.note, None);
    }

    #[test]
    fn totals_add_up() {
        let scan = scan_of(&[
            ("a.rs", parsed("rust")),
            ("b.rs", parsed("rust")),
            ("c.md", parsed("markdown")),
            ("run.sh", only(".sh")),
            ("img.png", FileClass::Excluded(ExcludeReason::Binary)),
            ("Cargo.lock", FileClass::Excluded(ExcludeReason::LockFile)),
        ]);
        let report = build_report(&cfg(), &scan, IndexState::Absent, &|_| found());
        assert_eq!(report.totals.tier3, 2);
        assert_eq!(report.totals.tier2, 1);
        assert_eq!(report.totals.tier1, 1);
        assert_eq!(report.totals.tier0, 2);
        assert_eq!(report.totals.files, 6);
        assert_eq!(report.excluded.files, 2);
        assert_eq!(report.excluded.by_reason[&ExcludeReason::Binary], 1);
        assert_eq!(report.excluded.by_reason[&ExcludeReason::LockFile], 1);
        assert_eq!(report.excluded.by_reason[&ExcludeReason::TooLarge], 0);
    }

    #[test]
    fn ordering_is_deterministic() {
        let entries = [
            ("a.ts", parsed("typescript")),
            ("b.js", parsed("javascript")),
            ("c.rs", parsed("rust")),
            ("d.md", parsed("markdown")),
            ("e.md", parsed("markdown")),
        ];
        let forward = build_report(&cfg(), &scan_of(&entries), IndexState::Absent, &|_| found());
        let mut shuffled = entries.to_vec();
        shuffled.reverse();
        let backward = build_report(&cfg(), &scan_of(&shuffled), IndexState::Absent, &|_| {
            found()
        });
        assert_eq!(
            serde_json::to_string(&forward).expect("json"),
            serde_json::to_string(&backward).expect("json")
        );
        let order: Vec<&str> = forward
            .languages
            .iter()
            .map(|row| row.language.as_str())
            .collect();
        // Tier desc, then files desc, then language asc:
        // tier 3 (1 file each, language asc) then tier 2 markdown (2 files).
        assert_eq!(order, vec!["javascript", "rust", "typescript", "markdown"]);
    }

    #[test]
    fn files_only_top_is_capped_at_ten() {
        let mut entries: Vec<(String, FileClass)> = Vec::new();
        for i in 0..12 {
            entries.push((format!("f{i}"), only(&format!(".ext{i}"))));
        }
        let report = build_report(
            &cfg(),
            &ScanResult { files: entries },
            IndexState::Absent,
            &|_| found(),
        );
        assert_eq!(report.files_only.top.len(), TOP_KEYS_LIMIT);
        assert_eq!(report.files_only.files, 12);
        assert_eq!(
            report.files_only.top[0].key, ".ext0",
            "count 1 each, key asc"
        );
    }
}
