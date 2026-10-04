//! Gold-set construction, adjudication and result assembly (T7.3).
//!
//! Systems compared (T7.2): `lsp`, `knot_db`, `heuristic` (primary) plus
//! `lsp_calls` and `knot_db_calls` (secondary). Items found by both `lsp` and
//! `knot_db` are auto-labelled `tp`; everything else is adjudicated by reading
//! the code (pending TOML with excerpts). More than 150 pending items trigger a
//! deterministic stratified sample of 50 (T7.3 rule 4) and `estimated: true`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::common::{EntityKey, GOLD_SCHEMA, Lang};
use crate::entities::EntityTable;
use crate::graph::heuristic::HeuristicResult;
use crate::graph::metrics::{
    self, LabelledItem, SystemMetrics, SystemSets, TargetGold, TargetInput, Verdict,
};
use crate::graph::normalize::container_for_caller;
use crate::groundtruth::{TargetsFile, load_and_filter};
use crate::probe::S1Result;

/// Above this many pending items the adjudication uses a stratified sample.
const ADJUDICATION_LIMIT: usize = 150;
/// Size of that stratified sample (T7.3 rule 4).
const SAMPLE_SIZE: usize = 50;
/// Deterministic sampling seed (T7.3 rules 2/4).
const SEED: u64 = 42;

/// One row of `results/adjudication/<lang>.toml` (T7.3 rule 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdjudicationItem {
    pub target: String,
    pub entity: EntityKey,
    pub found_by: Vec<String>,
    pub verdict: Verdict,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub evidence: String,
    /// Marked by the adjudicator as one of the most instructive disagreements.
    #[serde(default)]
    pub notable: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AdjudicationFile {
    #[serde(rename = "item", default)]
    items: Vec<AdjudicationItem>,
}

/// A union item before adjudication: who found it.
#[derive(Debug, Clone)]
struct RawItem {
    target_id: String,
    entity: EntityKey,
    found_by: Vec<String>,
    /// `lsp` ∧ `knot_db` agreement → auto `tp` (T7.3 rule 2).
    agreed: bool,
}

/// Per-target container sets of every system (T7.2).
struct TargetSets {
    id: String,
    category: String,
    systems: SystemSets,
}

/// `results/gold_<lang>.json` (schema `knot-rag-spike/gold/v1`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldResult {
    pub schema: String,
    pub lang: Lang,
    pub repo: String,
    pub estimated: bool,
    pub systems: BTreeMap<String, SystemMetrics>,
    pub targets: Vec<TargetGold>,
    pub notable_disagreements: Vec<NotableItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotableItem {
    pub target: String,
    pub entity: EntityKey,
    pub found_by: Vec<String>,
    pub verdict: Verdict,
    pub reason: String,
}

/// Inputs of one `lsp_probe gold` run (T7.3 CLI).
pub struct GoldConfig {
    pub targets: PathBuf,
    pub s1: PathBuf,
    pub heuristic: PathBuf,
    pub groundtruth_dir: PathBuf,
    pub adjudication: PathBuf,
    pub out: PathBuf,
}

/// Collect the five system sets per target (T7.2).
fn collect_sets(
    targets_file: &TargetsFile,
    table: &EntityTable,
    s1: &S1Result,
    heur: &HeuristicResult,
    gt_dir: &Path,
) -> Vec<TargetSets> {
    let s1_by_id: BTreeMap<&str, &crate::probe::TargetResult> = s1
        .targets
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect();
    let heur_by_id: BTreeMap<&str, &crate::graph::heuristic::HeuristicTarget> = heur
        .targets
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect();

    targets_file
        .targets
        .iter()
        .map(|target| {
            let self_container = table.container_at(&target.file, target.line);
            let mut systems = SystemSets::default();
            if let Some(row) = s1_by_id.get(target.id.as_str()) {
                systems.lsp = key_set(&row.lsp_containers.union, &self_container);
                systems.lsp_calls = key_set(&row.lsp_containers.incoming, &self_container);
            }
            if let Some(row) = heur_by_id.get(target.id.as_str()) {
                systems.heuristic = key_set(&row.callers, &self_container);
            }
            let gt_path = gt_dir.join(format!("callers_{}_{}.json", targets_file.repo, target.id));
            if let Ok(gt) = load_and_filter(&gt_path, target, table) {
                for caller in &gt.callers {
                    let container = container_for_caller(table, &caller.key);
                    if container == self_container {
                        continue;
                    }
                    let is_call = matches!(caller.rel_group.as_str(), "calls" | "macro_calls");
                    if is_call {
                        systems.knot_db_calls.insert(container.clone());
                    }
                    systems.knot_db.insert(container);
                }
            }
            TargetSets {
                id: target.id.clone(),
                category: target.category.clone(),
                systems,
            }
        })
        .collect()
}

fn key_set(keys: &[EntityKey], self_container: &EntityKey) -> BTreeSet<EntityKey> {
    keys.iter()
        .filter(|key| *key != self_container)
        .cloned()
        .collect()
}

/// Union of every system's items with their `found_by` labels (T7.3 rule 1).
fn build_union(sets: &[TargetSets]) -> Vec<RawItem> {
    let mut out = Vec::new();
    for target in sets {
        let mut entities: BTreeSet<&EntityKey> = BTreeSet::new();
        for name in metrics::SYSTEMS {
            entities.extend(target.systems.get(name).iter());
        }
        for entity in entities {
            let found_by = target.systems.found_by(entity);
            let agreed =
                found_by.iter().any(|n| n == "lsp") && found_by.iter().any(|n| n == "knot_db");
            out.push(RawItem {
                target_id: target.id.clone(),
                entity: entity.clone(),
                found_by,
                agreed,
            });
        }
    }
    out
}

/// Deterministic LCG (no RNG dependency; seed 42 per T7.3).
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as usize % n.max(1)
    }

    /// In-place Fisher–Yates, deterministic for a given seed and length.
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

/// Stratum key of T7.3 rule 4: `target id × "found by"`.
fn stratum_key(item: &RawItem) -> String {
    format!("{}|{}", item.target_id, item.found_by.join(","))
}

/// Pick a stratified sample of pending items (seed-42 shuffle, then one item
/// per stratum per round) and return it with per-stratum weights `N/n`.
fn sample_pending(pending: Vec<RawItem>) -> (Vec<(RawItem, f64)>, bool) {
    if pending.len() <= ADJUDICATION_LIMIT {
        let items = pending.into_iter().map(|item| (item, 1.0)).collect();
        return (items, false);
    }
    let mut strata: BTreeMap<String, Vec<RawItem>> = BTreeMap::new();
    for item in pending {
        strata.entry(stratum_key(&item)).or_default().push(item);
    }
    let totals: BTreeMap<String, usize> = strata
        .iter()
        .map(|(key, items)| (key.clone(), items.len()))
        .collect();

    let mut rng = Lcg::new(SEED);
    for items in strata.values_mut() {
        rng.shuffle(items);
    }

    // Round-robin across strata so the sample covers as many strata as possible.
    let mut picked: BTreeMap<String, Vec<RawItem>> = BTreeMap::new();
    let mut round = 0;
    let mut picked_count = 0;
    while picked_count < SAMPLE_SIZE {
        let mut progressed = false;
        for (key, items) in &strata {
            if picked_count >= SAMPLE_SIZE {
                break;
            }
            if let Some(item) = items.get(round) {
                picked.entry(key.clone()).or_default().push(item.clone());
                picked_count += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
        round += 1;
    }

    let mut out = Vec::new();
    for (key, items) in picked {
        let total = totals.get(&key).copied().unwrap_or(1);
        let weight = total as f64 / items.len().max(1) as f64;
        for item in items {
            out.push((item, weight));
        }
    }
    (out, true)
}

/// 3-line source excerpt of `container`, centred on the first mention of the
/// target name (falls back to the declaration lines).
fn excerpt(
    root: &Path,
    table: &EntityTable,
    container: &EntityKey,
    target_name: &str,
) -> Vec<String> {
    let Some(entity) = table.get(container) else {
        return Vec::new();
    };
    let path = root.join(&container.file);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = entity.key.line.max(1) - 1;
    let end = (entity.end_line.min(lines.len())).max(entity.key.line);
    let focus = (start..end)
        .find(|&idx| lines[idx].contains(target_name))
        .unwrap_or(start);
    let from = focus.saturating_sub(1);
    let to = (focus + 2).min(lines.len().saturating_sub(1));
    (from..=to)
        .map(|idx| format!("{:>5} | {}", idx + 1, lines[idx].trim_end()))
        .collect()
}

/// Shared inputs of the pending-file writer.
struct PendingContext<'a> {
    root: &'a Path,
    table: &'a EntityTable,
    names: BTreeMap<&'a str, &'a str>,
}

fn write_pending(
    path: &Path,
    ctx: &PendingContext<'_>,
    sampled: &[(RawItem, f64)],
    spot_check: &[RawItem],
    estimated: bool,
) -> Result<()> {
    let mut out = String::new();
    out.push_str("# knot-rag gold adjudication (T7.3) — PENDING ITEMS\n");
    out.push_str("# Fill `verdict` with \"tp\" or \"fp\" plus `reason` and `evidence`.\n");
    out.push_str("# `notable = true` marks an item for `notable_disagreements` (max 15).\n");
    if estimated {
        out.push_str("#\n# NOTE: >150 items needed adjudication; this is a stratified sample\n");
        out.push_str("# (seed 42, strata = target x found_by). Metrics will be `estimated`.\n");
    }
    out.push_str("# spot_check: auto-tp agreements sampled for verification (T7.3 rule 2);\n");
    out.push_str("# change to fp with a reason if the code says otherwise.\n");

    for item in spot_check {
        out.push_str("\n[[item]]\n");
        out.push_str(&render_item(ctx, item, 1.0, true));
    }
    for (item, weight) in sampled {
        out.push_str("\n[[item]]\n");
        out.push_str(&render_item(ctx, item, *weight, false));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn render_item(ctx: &PendingContext<'_>, item: &RawItem, weight: f64, agreed: bool) -> String {
    let target_name = ctx
        .names
        .get(item.target_id.as_str())
        .copied()
        .unwrap_or("");
    let code = excerpt(ctx.root, ctx.table, &item.entity, target_name);
    item_lines(item, weight, &code, agreed)
}

fn item_lines(item: &RawItem, weight: f64, code: &[String], agreed: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("target = \"{}\"\n", item.target_id));
    out.push_str(&format!(
        "entity = {{ file = \"{}\", line = {}, name = \"{}\" }}\n",
        item.entity.file, item.entity.line, item.entity.name
    ));
    out.push_str(&format!(
        "found_by = [{}]\n",
        item.found_by
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    if agreed {
        out.push_str("verdict = \"tp\"\n");
    } else {
        out.push_str("verdict = \"\"\n");
    }
    out.push_str("reason = \"\"\n");
    out.push_str("evidence = \"\"\n");
    if weight != 1.0 {
        out.push_str(&format!("# weight = {weight:.3} (extrapolated stratum)\n"));
    }
    for line in code {
        out.push_str(&format!("# {line}\n"));
    }
    out
}

/// Deterministic spot-check sample of agreed items: 10 per language (T7.3 rule 2).
fn spot_check_sample(agreed: &[RawItem]) -> Vec<RawItem> {
    const SPOT_CHECK: usize = 10;
    let mut pool = agreed.to_vec();
    let mut rng = Lcg::new(SEED);
    rng.shuffle(&mut pool);
    pool.truncate(SPOT_CHECK);
    pool
}

fn load_adjudication(path: &Path) -> Result<BTreeMap<(String, EntityKey), AdjudicationItem>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: AdjudicationFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut out = BTreeMap::new();
    for item in parsed.items {
        out.insert((item.target.clone(), item.entity.clone()), item);
    }
    Ok(out)
}

/// Label the union: adjudication verdicts win, agreements default to `tp`,
/// unsampled pending items are covered by their stratum weights.
/// Label the union: adjudication verdicts win, agreements default to `tp`,
/// unsampled pending items are covered by their stratum weights.
fn label_items(
    sets: Vec<TargetSets>,
    union: &[RawItem],
    sampled: &[(RawItem, f64)],
    verdicts: &BTreeMap<(String, EntityKey), AdjudicationItem>,
) -> Result<Vec<TargetInput>> {
    let weights: BTreeMap<(String, EntityKey), f64> = sampled
        .iter()
        .map(|(item, weight)| ((item.target_id.clone(), item.entity.clone()), *weight))
        .collect();

    // Every sampled item must carry a verdict (T7.3 rule 3).
    let mut unreviewed = Vec::new();
    for id in weights.keys() {
        match verdicts.get(id) {
            Some(entry) if entry.verdict != Verdict::Pending => {}
            _ => unreviewed.push(id.1.clone()),
        }
    }
    if !unreviewed.is_empty() {
        bail!(
            "{} sampled item(s) still have verdict = \"\" (first: {}:{} {})",
            unreviewed.len(),
            unreviewed[0].file,
            unreviewed[0].line,
            unreviewed[0].name
        );
    }

    let mut by_target: BTreeMap<String, Vec<LabelledItem>> = BTreeMap::new();
    for item in union {
        let id = (item.target_id.clone(), item.entity.clone());
        let verdict = if let Some(entry) = verdicts.get(&id) {
            if entry.verdict == Verdict::Pending {
                continue;
            }
            entry.verdict
        } else if item.agreed {
            Verdict::Tp
        } else {
            // Unreviewed non-sampled items are carried by stratum weights.
            continue;
        };
        let weight = weights.get(&id).copied().unwrap_or(1.0);
        by_target
            .entry(item.target_id.clone())
            .or_default()
            .push(LabelledItem {
                entity: item.entity.clone(),
                found_by: item.found_by.clone(),
                verdict,
                weight,
            });
    }

    Ok(sets
        .into_iter()
        .map(|set| TargetInput {
            items: by_target.remove(&set.id).unwrap_or_default(),
            id: set.id,
            category: set.category,
            systems: set.systems,
        })
        .collect())
}

/// `lsp_probe gold` (T7.3 CLI): pending file or full metrics.
pub fn run_gold(cfg: &GoldConfig) -> Result<()> {
    let targets_file = TargetsFile::load(&cfg.targets)?;
    let root = PathBuf::from(&targets_file.root);
    let (table, _parsed) = EntityTable::build(&targets_file.repo, &root)?;
    let s1: S1Result = read_json(&cfg.s1)?;
    let heur: HeuristicResult = read_json(&cfg.heuristic)?;

    let sets = collect_sets(&targets_file, &table, &s1, &heur, &cfg.groundtruth_dir);
    let union = build_union(&sets);
    let agreed: Vec<RawItem> = union.iter().filter(|item| item.agreed).cloned().collect();
    let pending: Vec<RawItem> = union.iter().filter(|item| !item.agreed).cloned().collect();
    let (sampled, estimated) = sample_pending(pending);

    if !cfg.adjudication.exists() {
        let names: BTreeMap<&str, &str> = targets_file
            .targets
            .iter()
            .map(|target| (target.id.as_str(), target.name.as_str()))
            .collect();
        let ctx = PendingContext {
            root: &root,
            table: &table,
            names,
        };
        let spot = spot_check_sample(&agreed);
        write_pending(&cfg.adjudication, &ctx, &sampled, &spot, estimated)?;
        println!(
            "wrote {} ({} pending of {} union items, {} auto-tp, {} spot-check)",
            cfg.adjudication.display(),
            sampled.len(),
            union.len(),
            agreed.len(),
            spot.len()
        );
        return Ok(());
    }

    let verdicts = load_adjudication(&cfg.adjudication)?;
    let inputs = label_items(sets, &union, &sampled, &verdicts)?;
    let (systems, targets) = metrics::compute(&inputs);
    let notable = notable_items(&verdicts);
    let result = GoldResult {
        schema: GOLD_SCHEMA.to_owned(),
        lang: s1.lang,
        repo: s1.repo.clone(),
        estimated,
        systems,
        targets,
        notable_disagreements: notable,
    };
    crate::common::write_json_pretty(&cfg.out, &result)?;
    println!(
        "wrote {} (estimated={}, systems: {})",
        cfg.out.display(),
        result.estimated,
        result.systems.len()
    );
    Ok(())
}

fn notable_items(verdicts: &BTreeMap<(String, EntityKey), AdjudicationItem>) -> Vec<NotableItem> {
    verdicts
        .values()
        .filter(|item| item.notable)
        .take(15)
        .map(|item| NotableItem {
            target: item.target.clone(),
            entity: item.entity.clone(),
            found_by: item.found_by.clone(),
            verdict: item.verdict,
            reason: item.reason.clone(),
        })
        .collect()
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}
