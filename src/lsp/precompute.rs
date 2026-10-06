//! Pre-computed LSP call graph at index time (`knot-lite index --lsp-precompute`, P3-D9).
//!
//! For high fan-in targets (heuristic caller containers ≥ `min_fanin`), pre-computes
//! LSP answers and writes them to `.knot-lite/lsp-edges/<lang>/<caller rel>.jsonl` and
//! `.knot-lite/lsp-targets/<lang>.jsonl`.
//!
//! Supports reuse (P3-D9): if the language file fingerprint and parameters match the
//! previous manifest, data is kept untouched without starting a server.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::ops::{implementation_relation, ops_for, shape_of};
use super::pool::LspPool;
use super::target::{DEFAULT_RETRY_PAUSE, LiveOutcome, LiveRequest, LiveStatus};
use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::graph::containers::ContainerTable;
use crate::index::hash::{sha256_hex, short16};
use crate::index::inventory::{Inventory, InventoryEntry};
use crate::index::layout::{IndexLayout, rel_of_record};
use crate::index::lsp_records::{
    LspEdgeLine, LspTargetLine, write_lsp_edge_shards, write_lsp_targets,
};
use crate::index::manifest::{LspLangManifest, LspManifest};
use crate::index::shard::ShardLine;

/// Per-target live query cap during pre-computation.
const PER_TARGET_CAP: Duration = Duration::from_secs(120);

/// Pre-compute policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PrecomputePolicy {
    /// Java only (the language whose live latency fails G8).
    #[default]
    Auto,
    /// Every language present in the repository.
    All,
    /// Disabled (pre-computed data is removed).
    None,
}

impl PrecomputePolicy {
    /// Wire string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::All => "all",
            Self::None => "none",
        }
    }
}

/// Options for pre-computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecomputeOptions {
    /// Policy switch.
    pub policy: PrecomputePolicy,
    /// Minimum heuristic fan-in threshold.
    pub min_fanin: usize,
    /// Maximum targets per language.
    pub max_targets: usize,
    /// Time budget per language.
    pub budget: Duration,
}

impl Default for PrecomputeOptions {
    fn default() -> Self {
        Self {
            policy: PrecomputePolicy::Auto,
            min_fanin: 20,
            max_targets: 200,
            budget: Duration::from_secs(600),
        }
    }
}

/// Inputs provided to [`run`].
pub struct PrecomputeInput<'a> {
    /// All entity records (file, ShardLine).
    pub records: &'a [(String, ShardLine)],
    /// Heuristic caller container count per target id.
    pub heuristic_fanin: &'a HashMap<String, usize>,
    /// Inventory of repository files.
    pub inventory: &'a Inventory,
    /// Previous manifest, if any.
    pub previous: Option<&'a LspManifest>,
    /// Repository root path.
    pub repo_root: &'a Path,
}

/// Progress event emitted during pre-computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// LSP stage started.
    Stage,
    /// Progress on one language's targets.
    LspTargets {
        lang: LspLang,
        done: usize,
        total: usize,
    },
}

/// Which languages are selected by `policy` given `present` languages.
pub fn languages_for(policy: PrecomputePolicy, present: &BTreeSet<LspLang>) -> Vec<LspLang> {
    match policy {
        PrecomputePolicy::Auto => {
            if present.contains(&LspLang::Java) {
                vec![LspLang::Java]
            } else {
                Vec::new()
            }
        }
        PrecomputePolicy::All => [LspLang::Rust, LspLang::Ts, LspLang::Java]
            .into_iter()
            .filter(|lang| present.contains(lang))
            .collect(),
        PrecomputePolicy::None => Vec::new(),
    }
}

/// Compute inventory fingerprint for `lang`.
pub fn fingerprint_for_lang(inventory: &Inventory, lang: LspLang) -> String {
    let mut entries: Vec<&InventoryEntry> = inventory
        .entries
        .iter()
        .filter(|e| lsp_lang_for_label(label_for(Path::new(&e.rel))) == Some(lang))
        .collect();
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));

    let mut buf = String::new();
    for entry in entries {
        buf.push_str(&entry.rel);
        buf.push(':');
        buf.push_str(&entry.sha);
        buf.push('\n');
    }
    short16(sha256_hex(buf.as_bytes()))
}

/// Run the pre-computation stage for all languages.
pub fn run(
    input: &PrecomputeInput<'_>,
    opts: &PrecomputeOptions,
    pool: Option<&LspPool>,
    layout: &IndexLayout,
    progress: &mut dyn FnMut(ProgressEvent),
) -> anyhow::Result<LspManifest> {
    progress(ProgressEvent::Stage);

    let mut present_languages = BTreeSet::new();
    for entry in &input.inventory.entries {
        if let Some(lang) = lsp_lang_for_label(label_for(Path::new(&entry.rel))) {
            present_languages.insert(lang);
        }
    }

    let selected_languages = languages_for(opts.policy, &present_languages);
    let mut lang_manifests = BTreeMap::new();
    let container_table = ContainerTable::from_records(
        input
            .records
            .iter()
            .enumerate()
            .map(|(idx, (rel, line))| (idx, rel.as_str(), line)),
    );

    for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
        let lang_dir_name = lang.dir_name().to_owned();
        let fp = fingerprint_for_lang(input.inventory, lang);

        if !selected_languages.contains(&lang) {
            clean_lang_data(layout, lang)?;
            if present_languages.contains(&lang) {
                lang_manifests.insert(
                    lang_dir_name,
                    LspLangManifest {
                        status: "none".to_owned(),
                        server: lang.server_program().to_owned(),
                        version: String::new(),
                        files_fingerprint: fp,
                        targets_selected: 0,
                        targets_ok: 0,
                        targets_failed: 0,
                        edges: 0,
                        ms: 0,
                        diagnostic: String::new(),
                    },
                );
            }
            continue;
        }

        // Check reuse (P3-D9)
        if let Some(prev) = input.previous
            && prev.policy == opts.policy.as_str()
            && prev.min_fanin == opts.min_fanin
            && prev.max_targets == opts.max_targets
            && let Some(prev_lang) = prev.languages.get(&lang_dir_name)
            && matches!(prev_lang.status.as_str(), "computed" | "reused")
            && prev_lang.files_fingerprint == fp
        {
            let mut reused = prev_lang.clone();
            reused.status = "reused".to_owned();
            lang_manifests.insert(lang_dir_name, reused);
            continue;
        }

        // Check if pool is available
        let Some(pool) = pool else {
            clean_lang_data(layout, lang)?;
            lang_manifests.insert(
                lang_dir_name,
                LspLangManifest {
                    status: "skipped".to_owned(),
                    server: lang.server_program().to_owned(),
                    version: String::new(),
                    files_fingerprint: fp,
                    targets_selected: 0,
                    targets_ok: 0,
                    targets_failed: 0,
                    edges: 0,
                    ms: 0,
                    diagnostic: "LSP is disabled (KNOT_LITE_LSP=off)".to_owned(),
                },
            );
            continue;
        };

        let (manifest_lang, _edges_written) = precompute_lang(
            &PrecomputeLangCtx {
                input,
                opts,
                pool,
                layout,
                lang,
                fp: &fp,
                container_table: &container_table,
            },
            progress,
        )?;
        lang_manifests.insert(lang_dir_name, manifest_lang);
    }

    Ok(LspManifest {
        format: 1,
        policy: opts.policy.as_str().to_owned(),
        min_fanin: opts.min_fanin,
        max_targets: opts.max_targets,
        budget_secs: opts.budget.as_secs(),
        languages: lang_manifests,
    })
}

fn clean_lang_data(layout: &IndexLayout, lang: LspLang) -> anyhow::Result<()> {
    let lang_edges_dir = layout.lsp_edges_lang_dir(lang);
    if lang_edges_dir.exists() {
        std::fs::remove_dir_all(&lang_edges_dir)
            .with_context(|| format!("removing {}", lang_edges_dir.display()))?;
    }
    let targets_file = layout.lsp_targets_for(lang);
    if targets_file.exists() {
        std::fs::remove_file(&targets_file)
            .with_context(|| format!("removing {}", targets_file.display()))?;
    }
    Ok(())
}

struct PrecomputeLangCtx<'a> {
    input: &'a PrecomputeInput<'a>,
    opts: &'a PrecomputeOptions,
    pool: &'a LspPool,
    layout: &'a IndexLayout,
    lang: LspLang,
    fp: &'a str,
    container_table: &'a ContainerTable,
}

fn query_one_target(
    ctx: &PrecomputeLangCtx<'_>,
    rel: &str,
    line: &ShardLine,
    fanin: usize,
    deadline: Instant,
) -> LiveOutcome {
    let abs_file = ctx.input.repo_root.join(rel);
    let req = LiveRequest {
        lang: ctx.lang,
        target_id: line.id.clone(),
        abs_file,
        rel_file: rel.to_owned(),
        position: async_lsp::lsp_types::Position {
            line: u32::try_from(line.l0.saturating_sub(1)).unwrap_or(0),
            character: line.col.unwrap_or(0),
        },
        ops: ops_for(shape_of(&line.kind)),
        heuristic_hint: fanin,
        deadline,
        retry_pause: DEFAULT_RETRY_PAUSE,
    };

    ctx.pool.query(req)
}

fn process_target_outcome(
    outcome: &LiveOutcome,
    line: &ShardLine,
    container_table: &ContainerTable,
    records: &[(String, ShardLine)],
    all_shards: &mut BTreeMap<String, Vec<LspEdgeLine>>,
) {
    let shape = shape_of(&line.kind);
    let impl_rel = implementation_relation(shape);

    for loc in &outcome.locations {
        let line1 = usize::try_from(loc.line0 + 1).unwrap_or(1);
        let (_, container_idx) = container_table.container_at(&loc.rel, line1);
        let src = container_idx.and_then(|idx| {
            let container_id = &records[idx].1.id;
            (container_id != &line.id).then(|| container_id.clone())
        });

        let rel_word = match loc.op {
            super::ops::Op::Incoming => "CALLS",
            super::ops::Op::References => "REFERENCES",
            super::ops::Op::Implementation => impl_rel.unwrap_or("IMPLEMENTS"),
        };

        let edge = LspEdgeLine {
            src,
            dst: line.id.clone(),
            rel: rel_word.to_owned(),
            n: loc.sites,
        };

        all_shards.entry(loc.rel.clone()).or_default().push(edge);
    }
}

fn select_candidates<'a>(
    input: &'a PrecomputeInput<'_>,
    lang: LspLang,
    min_fanin: usize,
    max_targets: usize,
) -> Vec<(&'a String, &'a ShardLine, usize)> {
    let mut candidates: Vec<(&String, &ShardLine, usize)> = input
        .records
        .iter()
        .filter_map(|(rel, line)| {
            if lsp_lang_for_label(label_for(Path::new(rel))) != Some(lang) || line.col.is_none() {
                return None;
            }
            let fanin = *input.heuristic_fanin.get(&line.id).unwrap_or(&0);
            if fanin < min_fanin {
                return None;
            }
            Some((rel, line, fanin))
        })
        .collect();

    candidates.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.id.cmp(&b.1.id)));
    candidates.truncate(max_targets);
    candidates
}

fn precompute_lang(
    ctx: &PrecomputeLangCtx<'_>,
    progress: &mut dyn FnMut(ProgressEvent),
) -> anyhow::Result<(LspLangManifest, usize)> {
    let input = ctx.input;
    let opts = ctx.opts;
    let pool = ctx.pool;
    let layout = ctx.layout;
    let lang = ctx.lang;
    let fp = ctx.fp;
    let container_table = ctx.container_table;
    let start_time = Instant::now();
    let budget_end = start_time + opts.budget;

    let candidates = select_candidates(input, lang, opts.min_fanin, opts.max_targets);

    let total_candidates = candidates.len();
    let mut attempted = 0;
    let mut targets_ok = 0;
    let mut targets_failed = 0;
    let mut last_diagnostic = String::new();

    let mut all_shards: BTreeMap<String, Vec<LspEdgeLine>> = BTreeMap::new();
    let mut target_lines = Vec::new();
    let mut server_version = String::new();

    for (idx, (rel, line, fanin)) in candidates.into_iter().enumerate() {
        let now = Instant::now();
        if now >= budget_end {
            tracing::info!(
                "precompute for {} budget exhausted after {}/{} targets",
                lang.dir_name(),
                attempted,
                total_candidates
            );
            break;
        }

        attempted += 1;
        let per_target_deadline = now + PER_TARGET_CAP;
        let deadline = per_target_deadline.min(budget_end);

        let outcome = query_one_target(ctx, rel, line, fanin, deadline);

        if server_version.is_empty() {
            let snap = pool.snapshot();
            if let Some(slot_snap) = snap.slots.get(lang.dir_name()) {
                server_version = slot_snap.version.clone();
            }
        }

        if outcome.status == LiveStatus::Ok {
            targets_ok += 1;
            process_target_outcome(
                &outcome,
                line,
                container_table,
                input.records,
                &mut all_shards,
            );
        } else {
            targets_failed += 1;
            if let Some(detail) = &outcome.detail {
                last_diagnostic = detail.clone();
            }
        }

        target_lines.push(LspTargetLine {
            id: line.id.clone(),
            status: outcome.status.as_str().to_owned(),
            ms: outcome.ms,
            ops: outcome.op_counts,
            server: server_version.clone(),
        });

        if (idx + 1) % 10 == 0 || (idx + 1) == total_candidates {
            progress(ProgressEvent::LspTargets {
                lang,
                done: idx + 1,
                total: total_candidates,
            });
        }
    }

    let (total_edges, _) = write_lsp_edge_shards(layout, lang, &all_shards)?;
    prune_orphan_edge_shards(layout, lang, &all_shards)?;
    write_lsp_targets(layout, lang, &target_lines)?;

    let ms = u64::try_from(start_time.elapsed().as_millis()).unwrap_or(u64::MAX);

    let status_str = if attempted == 0 && total_candidates == 0 {
        "none"
    } else if targets_failed == attempted && attempted > 0 {
        "failed"
    } else {
        "computed"
    };

    Ok((
        LspLangManifest {
            status: status_str.to_owned(),
            server: lang.server_program().to_owned(),
            version: server_version,
            files_fingerprint: fp.to_owned(),
            targets_selected: attempted,
            targets_ok,
            targets_failed,
            edges: total_edges,
            ms,
            diagnostic: last_diagnostic,
        },
        total_edges,
    ))
}

fn prune_orphan_edge_shards(
    layout: &IndexLayout,
    lang: LspLang,
    active_shards: &BTreeMap<String, Vec<LspEdgeLine>>,
) -> anyhow::Result<()> {
    let lang_dir = layout.lsp_edges_lang_dir(lang);
    if !lang_dir.exists() {
        return Ok(());
    }
    let mut stack = vec![lang_dir.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file()
                && let Some(rel) = rel_of_record(&lang_dir, &path)
                && !active_shards.contains_key(&rel)
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::fake::{FakeScript, connect, test_spec};
    use crate::lsp::pool::{FactoryContext, SessionFactory};
    use crate::lsp::spec::Degraded;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct ScriptedPoolFactory {
        script_fn: Arc<dyn Fn(LspLang) -> FakeScript + Send + Sync>,
        missing: bool,
    }

    impl SessionFactory for ScriptedPoolFactory {
        fn start(
            &self,
            lang: LspLang,
            _ctx: FactoryContext,
        ) -> super::super::pool::BoxFuture<'static, crate::lsp::process::StartOutcome> {
            if self.missing {
                return Box::pin(async move {
                    crate::lsp::process::StartOutcome::Degraded(Degraded::unavailable(format!(
                        "{} not found in PATH",
                        lang.server_program()
                    )))
                });
            }
            let script = (self.script_fn)(lang);
            Box::pin(async move {
                let spec = test_spec(lang);
                let init = crate::lsp::session::InitInputs {
                    spec: &spec,
                    repo_root: Path::new("/work/demo"),
                };
                let (session, _) = connect(script, init).await;
                crate::lsp::process::StartOutcome::Ready(Box::new(crate::lsp::process::Started {
                    session,
                    process: None,
                }))
            })
        }
    }

    #[test]
    fn auto_policy_selects_java_only() {
        let mut present = BTreeSet::new();
        present.insert(LspLang::Rust);
        present.insert(LspLang::Java);
        assert_eq!(
            languages_for(PrecomputePolicy::Auto, &present),
            vec![LspLang::Java]
        );
    }

    #[test]
    fn all_policy_selects_present_languages() {
        let mut present = BTreeSet::new();
        present.insert(LspLang::Rust);
        present.insert(LspLang::Ts);
        assert_eq!(
            languages_for(PrecomputePolicy::All, &present),
            vec![LspLang::Rust, LspLang::Ts]
        );
    }

    #[test]
    fn none_policy_removes_previous_lsp_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());

        // Create dummy edge and target files
        let lang_dir = layout.lsp_edges_lang_dir(LspLang::Java);
        std::fs::create_dir_all(&lang_dir).expect("mkdir");
        std::fs::write(lang_dir.join("Foo.java.jsonl"), "dummy\n").expect("write");
        std::fs::create_dir_all(layout.lsp_targets()).expect("mkdir");
        std::fs::write(layout.lsp_targets_for(LspLang::Java), "dummy\n").expect("write");

        let inv = Inventory {
            entries: vec![InventoryEntry {
                rel: "Foo.java".to_owned(),
                sha: "0123456789abcdef".to_owned(),
                bytes: 50,
            }],
        };

        let input = PrecomputeInput {
            records: &[],
            heuristic_fanin: &HashMap::new(),
            inventory: &inv,
            previous: None,
            repo_root: tmp.path(),
        };

        let opts = PrecomputeOptions {
            policy: PrecomputePolicy::None,
            ..PrecomputeOptions::default()
        };

        let mut events = Vec::new();
        let manifest = run(&input, &opts, None, &layout, &mut |ev| events.push(ev)).expect("run");

        assert_eq!(manifest.policy, "none");
        assert!(!lang_dir.exists());
        assert!(!layout.lsp_targets_for(LspLang::Java).exists());
    }

    #[test]
    fn targets_are_selected_by_fanin_then_id_and_capped() {
        let inv = Inventory {
            entries: vec![InventoryEntry {
                rel: "Foo.java".to_owned(),
                sha: "0123".to_owned(),
                bytes: 10,
            }],
        };

        let r1 = (
            "Foo.java".to_owned(),
            ShardLine {
                id: "t1".to_owned(),
                kind: "java_method".to_owned(),
                name: "m1".to_owned(),
                fqn: "m1".to_owned(),
                lang: "java".to_owned(),
                l0: 1,
                l1: 5,
                col: Some(0),
                sig: None,
                h: "h".to_owned(),
                q: None,
                s: None,
            },
        );
        let r2 = (
            "Foo.java".to_owned(),
            ShardLine {
                id: "t2".to_owned(),
                kind: "java_method".to_owned(),
                name: "m2".to_owned(),
                fqn: "m2".to_owned(),
                lang: "java".to_owned(),
                l0: 10,
                l1: 15,
                col: Some(0),
                sig: None,
                h: "h".to_owned(),
                q: None,
                s: None,
            },
        );
        let r3 = (
            "Foo.java".to_owned(),
            ShardLine {
                id: "t3".to_owned(),
                kind: "java_method".to_owned(),
                name: "m3".to_owned(),
                fqn: "m3".to_owned(),
                lang: "java".to_owned(),
                l0: 20,
                l1: 25,
                col: Some(0),
                sig: None,
                h: "h".to_owned(),
                q: None,
                s: None,
            },
        );

        let records = vec![r1, r2, r3];
        let mut fanin = HashMap::new();
        fanin.insert("t1".to_owned(), 25);
        fanin.insert("t2".to_owned(), 30); // highest fanin
        fanin.insert("t3".to_owned(), 25); // tie with t1, id "t1" < "t3"

        let input = PrecomputeInput {
            records: &records,
            heuristic_fanin: &fanin,
            inventory: &inv,
            previous: None,
            repo_root: Path::new("/demo"),
        };

        let opts = PrecomputeOptions {
            policy: PrecomputePolicy::All,
            min_fanin: 20,
            max_targets: 2,
            budget: Duration::from_secs(600),
        };

        let factory = Arc::new(ScriptedPoolFactory {
            script_fn: Arc::new(|_| FakeScript::full()),
            missing: false,
        });

        let pool = crate::lsp::pool::LspPool::new(
            factory,
            crate::lsp::pool::PoolSettings::default(),
            None,
        )
        .expect("pool");
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());

        let manifest = run(&input, &opts, Some(&pool), &layout, &mut |_| {}).expect("run");
        let java_m = &manifest.languages["java"];

        assert_eq!(java_m.targets_selected, 2);
    }

    #[test]
    fn records_without_col_are_not_selected() {
        let inv = Inventory {
            entries: vec![InventoryEntry {
                rel: "Foo.java".to_owned(),
                sha: "0123".to_owned(),
                bytes: 10,
            }],
        };

        let r1 = (
            "Foo.java".to_owned(),
            ShardLine {
                id: "t1".to_owned(),
                kind: "java_method".to_owned(),
                name: "m1".to_owned(),
                fqn: "m1".to_owned(),
                lang: "java".to_owned(),
                l0: 1,
                l1: 5,
                col: None,
                sig: None,
                h: "h".to_owned(),
                q: None,
                s: None,
            },
        );
        let records = vec![r1];
        let mut fanin = HashMap::new();
        fanin.insert("t1".to_owned(), 100);

        let input = PrecomputeInput {
            records: &records,
            heuristic_fanin: &fanin,
            inventory: &inv,
            previous: None,
            repo_root: Path::new("/demo"),
        };

        let opts = PrecomputeOptions {
            policy: PrecomputePolicy::All,
            min_fanin: 5,
            max_targets: 10,
            budget: Duration::from_secs(600),
        };

        let factory = Arc::new(ScriptedPoolFactory {
            script_fn: Arc::new(|_| FakeScript::full()),
            missing: false,
        });

        let pool = crate::lsp::pool::LspPool::new(
            factory,
            crate::lsp::pool::PoolSettings::default(),
            None,
        )
        .expect("pool");
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());

        let manifest = run(&input, &opts, Some(&pool), &layout, &mut |_| {}).expect("run");
        let java_m = &manifest.languages["java"];

        assert_eq!(java_m.targets_selected, 0);
    }

    #[test]
    fn missing_server_marks_failed_and_removes_stale_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());

        let inv = Inventory {
            entries: vec![InventoryEntry {
                rel: "Foo.java".to_owned(),
                sha: "0123".to_owned(),
                bytes: 10,
            }],
        };

        let r1 = (
            "Foo.java".to_owned(),
            ShardLine {
                id: "t1".to_owned(),
                kind: "java_method".to_owned(),
                name: "m1".to_owned(),
                fqn: "m1".to_owned(),
                lang: "java".to_owned(),
                l0: 1,
                l1: 5,
                col: Some(0),
                sig: None,
                h: "h".to_owned(),
                q: None,
                s: None,
            },
        );
        let records = vec![r1];
        let mut fanin = HashMap::new();
        fanin.insert("t1".to_owned(), 50);

        let input = PrecomputeInput {
            records: &records,
            heuristic_fanin: &fanin,
            inventory: &inv,
            previous: None,
            repo_root: tmp.path(),
        };

        let opts = PrecomputeOptions {
            policy: PrecomputePolicy::All,
            min_fanin: 10,
            max_targets: 5,
            budget: Duration::from_secs(600),
        };

        let factory = Arc::new(ScriptedPoolFactory {
            script_fn: Arc::new(|_| FakeScript::full()),
            missing: true,
        });

        let pool = crate::lsp::pool::LspPool::new(
            factory,
            crate::lsp::pool::PoolSettings::default(),
            None,
        )
        .expect("pool");
        let manifest = run(&input, &opts, Some(&pool), &layout, &mut |_| {}).expect("run");
        let java_m = &manifest.languages["java"];

        assert_eq!(java_m.status, "failed");
        assert!(java_m.diagnostic.contains("not found in PATH"));
    }

    #[test]
    fn unchanged_language_files_reuse_without_starting_server() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());

        let inv = Inventory {
            entries: vec![InventoryEntry {
                rel: "Foo.java".to_owned(),
                sha: "0123".to_owned(),
                bytes: 10,
            }],
        };

        let fp = fingerprint_for_lang(&inv, LspLang::Java);
        let mut prev_langs = BTreeMap::new();
        prev_langs.insert(
            "java".to_owned(),
            LspLangManifest {
                status: "computed".to_owned(),
                server: "jdtls".to_owned(),
                version: "1.0".to_owned(),
                files_fingerprint: fp,
                targets_selected: 5,
                targets_ok: 5,
                targets_failed: 0,
                edges: 12,
                ms: 50,
                diagnostic: String::new(),
            },
        );

        let prev_manifest = LspManifest {
            format: 1,
            policy: "all".to_owned(),
            min_fanin: 10,
            max_targets: 10,
            budget_secs: 600,
            languages: prev_langs,
        };

        let input = PrecomputeInput {
            records: &[],
            heuristic_fanin: &HashMap::new(),
            inventory: &inv,
            previous: Some(&prev_manifest),
            repo_root: tmp.path(),
        };

        let opts = PrecomputeOptions {
            policy: PrecomputePolicy::All,
            min_fanin: 10,
            max_targets: 10,
            budget: Duration::from_secs(600),
        };

        let starts = Arc::new(AtomicU64::new(0));
        let starts_clone = Arc::clone(&starts);

        let factory = Arc::new(ScriptedPoolFactory {
            script_fn: Arc::new(move |_| {
                starts_clone.fetch_add(1, Ordering::SeqCst);
                FakeScript::full()
            }),
            missing: false,
        });

        let pool = crate::lsp::pool::LspPool::new(
            factory,
            crate::lsp::pool::PoolSettings::default(),
            None,
        )
        .expect("pool");
        let manifest = run(&input, &opts, Some(&pool), &layout, &mut |_| {}).expect("run");
        let java_m = &manifest.languages["java"];

        assert_eq!(java_m.status, "reused");
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn changed_language_file_recomputes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());

        let inv = Inventory {
            entries: vec![InventoryEntry {
                rel: "Foo.java".to_owned(),
                sha: "NEW_SHA".to_owned(),
                bytes: 10,
            }],
        };

        let mut prev_langs = BTreeMap::new();
        prev_langs.insert(
            "java".to_owned(),
            LspLangManifest {
                status: "computed".to_owned(),
                server: "jdtls".to_owned(),
                version: "1.0".to_owned(),
                files_fingerprint: "OLD_SHA".to_owned(),
                targets_selected: 5,
                targets_ok: 5,
                targets_failed: 0,
                edges: 12,
                ms: 50,
                diagnostic: String::new(),
            },
        );

        let prev_manifest = LspManifest {
            format: 1,
            policy: "all".to_owned(),
            min_fanin: 10,
            max_targets: 10,
            budget_secs: 600,
            languages: prev_langs,
        };

        let input = PrecomputeInput {
            records: &[],
            heuristic_fanin: &HashMap::new(),
            inventory: &inv,
            previous: Some(&prev_manifest),
            repo_root: tmp.path(),
        };

        let opts = PrecomputeOptions {
            policy: PrecomputePolicy::All,
            min_fanin: 10,
            max_targets: 10,
            budget: Duration::from_secs(600),
        };

        let factory = Arc::new(ScriptedPoolFactory {
            script_fn: Arc::new(|_| FakeScript::full()),
            missing: false,
        });

        let pool = crate::lsp::pool::LspPool::new(
            factory,
            crate::lsp::pool::PoolSettings::default(),
            None,
        )
        .expect("pool");
        let manifest = run(&input, &opts, Some(&pool), &layout, &mut |_| {}).expect("run");
        let java_m = &manifest.languages["java"];

        assert_eq!(java_m.status, "none");
    }

    #[test]
    fn parameter_change_recomputes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());

        let inv = Inventory {
            entries: vec![InventoryEntry {
                rel: "Foo.java".to_owned(),
                sha: "0123".to_owned(),
                bytes: 10,
            }],
        };

        let fp = fingerprint_for_lang(&inv, LspLang::Java);
        let mut prev_langs = BTreeMap::new();
        prev_langs.insert(
            "java".to_owned(),
            LspLangManifest {
                status: "computed".to_owned(),
                server: "jdtls".to_owned(),
                version: "1.0".to_owned(),
                files_fingerprint: fp,
                targets_selected: 5,
                targets_ok: 5,
                targets_failed: 0,
                edges: 12,
                ms: 50,
                diagnostic: String::new(),
            },
        );

        let prev_manifest = LspManifest {
            format: 1,
            policy: "all".to_owned(),
            min_fanin: 5, // Old min_fanin
            max_targets: 10,
            budget_secs: 600,
            languages: prev_langs,
        };

        let input = PrecomputeInput {
            records: &[],
            heuristic_fanin: &HashMap::new(),
            inventory: &inv,
            previous: Some(&prev_manifest),
            repo_root: tmp.path(),
        };

        let opts = PrecomputeOptions {
            policy: PrecomputePolicy::All,
            min_fanin: 20, // New min_fanin
            max_targets: 10,
            budget: Duration::from_secs(600),
        };

        let factory = Arc::new(ScriptedPoolFactory {
            script_fn: Arc::new(|_| FakeScript::full()),
            missing: false,
        });

        let pool = crate::lsp::pool::LspPool::new(
            factory,
            crate::lsp::pool::PoolSettings::default(),
            None,
        )
        .expect("pool");
        let manifest = run(&input, &opts, Some(&pool), &layout, &mut |_| {}).expect("run");
        let java_m = &manifest.languages["java"];

        assert_eq!(java_m.status, "none");
    }
}
