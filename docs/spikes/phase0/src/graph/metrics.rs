//! Precision/recall metrics for the gold comparison (T7.4).
//!
//! Counters are `f64` because adjudication sampling (T7.3 rule 4) extrapolates
//! strata with item weights (`N/n`); in exact mode every weight is 1.0 and the
//! counters are whole numbers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::common::EntityKey;

/// Adjudication label of one union item (T7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Not yet reviewed (`verdict = ""` in a pending file).
    #[serde(rename = "")]
    Pending,
    #[serde(rename = "tp")]
    Tp,
    #[serde(rename = "fp")]
    Fp,
}

/// The five systems compared per target (T7.2).
pub const SYSTEMS: &[&str] = &["lsp", "knot_db", "heuristic", "lsp_calls", "knot_db_calls"];

/// Container sets of one system family for one target (T7.2).
#[derive(Debug, Clone, Default)]
pub struct SystemSets {
    /// incoming ∪ references ∪ implementation.
    pub lsp: BTreeSet<EntityKey>,
    /// incoming only (secondary).
    pub lsp_calls: BTreeSet<EntityKey>,
    /// filtered knot ground truth, all kept groups.
    pub knot_db: BTreeSet<EntityKey>,
    /// `calls` + `macro_calls` groups only (secondary).
    pub knot_db_calls: BTreeSet<EntityKey>,
    /// T6 heuristic containers.
    pub heuristic: BTreeSet<EntityKey>,
}

impl SystemSets {
    pub fn get(&self, name: &str) -> &BTreeSet<EntityKey> {
        match name {
            "lsp" => &self.lsp,
            "knot_db" => &self.knot_db,
            "heuristic" => &self.heuristic,
            "lsp_calls" => &self.lsp_calls,
            "knot_db_calls" => &self.knot_db_calls,
            other => panic!("unknown system: {other}"),
        }
    }

    /// Systems that contain `key`.
    pub fn found_by(&self, key: &EntityKey) -> Vec<String> {
        SYSTEMS
            .iter()
            .filter(|name| self.get(name).contains(key))
            .map(|name| (*name).to_owned())
            .collect()
    }
}

/// One union item with its final adjudication label.
#[derive(Debug, Clone)]
pub struct LabelledItem {
    pub entity: EntityKey,
    /// Sorted names of the systems that found it.
    pub found_by: Vec<String>,
    pub verdict: Verdict,
    /// `1.0` when exact; `N/n` when extrapolated from a sample (T7.3 rule 4).
    pub weight: f64,
}

/// Everything the metrics need about one target.
#[derive(Debug, Clone)]
pub struct TargetInput {
    pub id: String,
    pub category: String,
    pub systems: SystemSets,
    /// The target's full union, labelled.
    pub items: Vec<LabelledItem>,
}

/// Micro metrics block (T7.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Agg {
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub tp: f64,
    pub fp: f64,
    #[serde(rename = "fn")]
    pub false_negatives: f64,
    pub found: f64,
    pub gold: f64,
}

/// Macro metrics: means over per-target values (T7.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MacroAgg {
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    /// Targets contributing to the recall mean (gold non-empty).
    pub targets_with_gold: usize,
    /// Targets contributing to the precision mean (found non-empty).
    pub targets_with_found: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileLevel {
    pub micro: Agg,
    #[serde(rename = "macro")]
    pub macro_avg: MacroAgg,
}

/// Metrics of one system over a language (T7.4 / result schema).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemMetrics {
    pub micro: Agg,
    #[serde(rename = "macro")]
    pub macro_avg: MacroAgg,
    pub by_category: BTreeMap<String, Agg>,
    pub file_level: FileLevel,
}

/// Per-target metrics row of the result schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetGold {
    pub id: String,
    pub gold_size: f64,
    pub per_system: BTreeMap<String, Agg>,
}

/// `tp`/`gold`/`found` → precision/recall, with undefined cases as `None`.
pub fn agg_from_counts(tp: f64, found: f64, gold: f64) -> Agg {
    Agg {
        precision: (found > 0.0).then(|| tp / found),
        recall: (gold > 0.0).then(|| tp / gold),
        tp,
        fp: (found - tp).max(0.0),
        false_negatives: (gold - tp).max(0.0),
        found,
        gold,
    }
}

/// Running sums for one system across targets.
#[derive(Debug, Default)]
struct Accumulator {
    micro: Agg,
    macro_precision_sum: f64,
    macro_precision_n: usize,
    macro_recall_sum: f64,
    macro_recall_n: usize,
    by_category: BTreeMap<String, Agg>,
    file_micro: Agg,
    file_macro_precision_sum: f64,
    file_macro_precision_n: usize,
    file_macro_recall_sum: f64,
    file_macro_recall_n: usize,
}

impl Accumulator {
    fn add_target(&mut self, target: &TargetInput, name: &str) {
        let gold = gold_weight(target);
        let set = target.systems.get(name);
        let tp = set.iter().map(|key| item_weight(target, key)).sum::<f64>();
        let agg = agg_from_counts(tp, set.len() as f64, gold);

        fold_counts(&mut self.micro, &agg);
        fold_ratio_means(
            &mut self.macro_precision_sum,
            &mut self.macro_precision_n,
            agg.precision,
        );
        fold_ratio_means(
            &mut self.macro_recall_sum,
            &mut self.macro_recall_n,
            agg.recall,
        );
        self.by_category
            .entry(target.category.clone())
            .or_default()
            .fold_counts(&agg);

        let file_agg = file_level_agg(target, set);
        fold_counts(&mut self.file_micro, &file_agg);
        fold_ratio_means(
            &mut self.file_macro_precision_sum,
            &mut self.file_macro_precision_n,
            file_agg.precision,
        );
        fold_ratio_means(
            &mut self.file_macro_recall_sum,
            &mut self.file_macro_recall_n,
            file_agg.recall,
        );
    }

    fn finish(mut self) -> SystemMetrics {
        derive_ratios(&mut self.micro);
        derive_ratios(&mut self.file_micro);
        for agg in self.by_category.values_mut() {
            derive_ratios(agg);
        }
        SystemMetrics {
            micro: self.micro,
            macro_avg: MacroAgg {
                precision: mean(self.macro_precision_sum, self.macro_precision_n),
                recall: mean(self.macro_recall_sum, self.macro_recall_n),
                targets_with_gold: self.macro_recall_n,
                targets_with_found: self.macro_precision_n,
            },
            by_category: self.by_category,
            file_level: FileLevel {
                micro: self.file_micro,
                macro_avg: MacroAgg {
                    precision: mean(self.file_macro_precision_sum, self.file_macro_precision_n),
                    recall: mean(self.file_macro_recall_sum, self.file_macro_recall_n),
                    targets_with_gold: self.file_macro_recall_n,
                    targets_with_found: self.file_macro_precision_n,
                },
            },
        }
    }
}

impl Agg {
    fn fold_counts(&mut self, other: &Agg) {
        self.tp += other.tp;
        self.fp += other.fp;
        self.false_negatives += other.false_negatives;
        self.found += other.found;
        self.gold += other.gold;
    }
}

fn fold_counts(into: &mut Agg, other: &Agg) {
    into.fold_counts(other);
}

fn fold_ratio_means(sum: &mut f64, n: &mut usize, value: Option<f64>) {
    if let Some(value) = value {
        *sum += value;
        *n += 1;
    }
}

fn mean(sum: f64, n: usize) -> Option<f64> {
    (n > 0).then(|| sum / n as f64)
}

fn derive_ratios(agg: &mut Agg) {
    agg.precision = (agg.found > 0.0).then(|| agg.tp / agg.found);
    agg.recall = (agg.gold > 0.0).then(|| agg.tp / agg.gold);
}

/// File-level view of one target/system (distinct files of containers).
fn file_level_agg(target: &TargetInput, set: &BTreeSet<EntityKey>) -> Agg {
    let gold_files: BTreeSet<&str> = target
        .items
        .iter()
        .filter(|item| item.verdict == Verdict::Tp)
        .map(|item| item.entity.file.as_str())
        .collect();
    let found_files: BTreeSet<&str> = set.iter().map(|key| key.file.as_str()).collect();
    let tp_files = found_files.intersection(&gold_files).count() as f64;
    agg_from_counts(tp_files, found_files.len() as f64, gold_files.len() as f64)
}

/// Σ weight of the `tp` items of one target = |gold| (estimated).
fn gold_weight(target: &TargetInput) -> f64 {
    target
        .items
        .iter()
        .filter(|item| item.verdict == Verdict::Tp)
        .map(|item| item.weight)
        .sum()
}

/// Weight of the labelled item for `key` (0 when it is not `tp`).
fn item_weight(target: &TargetInput, key: &EntityKey) -> f64 {
    target
        .items
        .iter()
        .find(|item| &item.entity == key)
        .map_or(0.0, |item| {
            if item.verdict == Verdict::Tp {
                item.weight
            } else {
                0.0
            }
        })
}

/// Compute every system's metrics from labelled target inputs (T7.4).
pub fn compute(targets: &[TargetInput]) -> (BTreeMap<String, SystemMetrics>, Vec<TargetGold>) {
    let mut accumulators: BTreeMap<String, Accumulator> = SYSTEMS
        .iter()
        .map(|name| ((*name).to_owned(), Accumulator::default()))
        .collect();
    let mut per_target = Vec::new();

    for target in targets {
        let gold = gold_weight(target);
        let mut row = BTreeMap::new();
        for name in SYSTEMS {
            let set = target.systems.get(name);
            let tp = set.iter().map(|key| item_weight(target, key)).sum::<f64>();
            let agg = agg_from_counts(tp, set.len() as f64, gold);
            row.insert((*name).to_owned(), agg);
            accumulators
                .get_mut(*name)
                .expect("all systems pre-seeded")
                .add_target(target, name);
        }
        per_target.push(TargetGold {
            id: target.id.clone(),
            gold_size: gold,
            per_system: row,
        });
    }

    let systems = accumulators
        .into_iter()
        .map(|(name, acc)| (name, acc.finish()))
        .collect();
    (systems, per_target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(file: &str, line: usize, name: &str) -> EntityKey {
        EntityKey {
            file: file.to_owned(),
            line,
            name: name.to_owned(),
        }
    }

    fn item(entity: EntityKey, found_by: &[&str], verdict: Verdict) -> LabelledItem {
        LabelledItem {
            entity,
            found_by: found_by.iter().map(|s| (*s).to_owned()).collect(),
            verdict,
            weight: 1.0,
        }
    }

    fn target(items: Vec<LabelledItem>, sets: SystemSets) -> TargetInput {
        TargetInput {
            id: "t1".to_owned(),
            category: "free_fn".to_owned(),
            systems: sets,
            items,
        }
    }

    #[test]
    fn empty_sets_give_undefined_ratios() {
        let (systems, rows) = compute(&[target(Vec::new(), SystemSets::default())]);
        let lsp = &systems["lsp"];
        assert_eq!(lsp.micro.precision, None);
        assert_eq!(lsp.micro.recall, None);
        assert_eq!(lsp.micro.tp, 0.0);
        assert_eq!(rows[0].gold_size, 0.0);
    }

    #[test]
    fn perfect_agreement_scores_one() {
        let entity = key("src/a.rs", 10, "caller");
        let mut sets = SystemSets::default();
        sets.lsp.insert(entity.clone());
        sets.lsp_calls.insert(entity.clone());
        sets.knot_db.insert(entity.clone());
        sets.heuristic.insert(entity.clone());
        let items = vec![item(
            entity,
            &["heuristic", "knot_db", "lsp", "lsp_calls"],
            Verdict::Tp,
        )];
        let (systems, rows) = compute(&[target(items, sets)]);
        for name in ["lsp", "knot_db", "heuristic"] {
            let metrics = &systems[name];
            assert_eq!(metrics.micro.precision, Some(1.0), "{name}");
            assert_eq!(metrics.micro.recall, Some(1.0), "{name}");
        }
        assert_eq!(rows[0].gold_size, 1.0);
    }

    #[test]
    fn disjoint_sets_score_zero_recall() {
        let gold = key("src/a.rs", 10, "gold_caller");
        let noise = key("src/b.rs", 20, "noise");
        let mut sets = SystemSets::default();
        sets.lsp.insert(noise.clone()); // found only noise
        sets.knot_db.insert(gold.clone());
        let items = vec![
            item(gold, &["knot_db"], Verdict::Tp),
            item(noise, &["lsp"], Verdict::Fp),
        ];
        let (systems, _) = compute(&[target(items, sets)]);
        let lsp = &systems["lsp"];
        assert_eq!(lsp.micro.precision, Some(0.0));
        assert_eq!(lsp.micro.recall, Some(0.0));
        assert_eq!(lsp.micro.tp, 0.0);
        assert_eq!(lsp.micro.fp, 1.0);
        let knot = &systems["knot_db"];
        assert_eq!(knot.micro.recall, Some(1.0));
    }

    #[test]
    fn weights_extrapolate_sampled_strata() {
        // Stratum of 4 union items, 1 of 4 adjudicated (tp, weight N/n = 4):
        // the system set holds all 4 items exactly, only the sample is labelled.
        let sampled_key = key("src/a.rs", 1, "sampled");
        let mut sets = SystemSets::default();
        sets.lsp.insert(sampled_key.clone());
        for line in [10, 20, 30] {
            sets.lsp.insert(key("src/a.rs", line, "unsampled"));
        }
        let mut sampled = item(sampled_key, &["lsp"], Verdict::Tp);
        sampled.weight = 4.0;
        let (systems, rows) = compute(&[target(vec![sampled], sets)]);
        assert_eq!(rows[0].gold_size, 4.0);
        let lsp = &systems["lsp"];
        assert_eq!(lsp.micro.tp, 4.0);
        assert_eq!(lsp.micro.found, 4.0);
        assert_eq!(lsp.micro.fp, 0.0);
        assert_eq!(lsp.micro.precision, Some(1.0));
    }

    #[test]
    fn file_level_collapses_containers_per_file() {
        let a1 = key("src/a.rs", 1, "one");
        let a2 = key("src/a.rs", 20, "two");
        let mut sets = SystemSets::default();
        sets.lsp.insert(a1.clone());
        sets.lsp.insert(a2.clone());
        let items = vec![
            item(a1, &["lsp"], Verdict::Tp),
            item(a2, &["lsp"], Verdict::Tp),
        ];
        let (systems, _) = compute(&[target(items, sets)]);
        let file_level = &systems["lsp"].file_level.micro;
        assert_eq!(file_level.tp, 1.0);
        assert_eq!(file_level.found, 1.0);
        assert_eq!(file_level.recall, Some(1.0));
    }

    #[test]
    fn macro_averages_skip_undefined_targets() {
        let entity = key("src/a.rs", 1, "caller");
        let mut sets = SystemSets::default();
        sets.lsp.insert(entity.clone());
        // Target 1: perfect. Target 2: empty (both ratios undefined).
        let t1 = target(vec![item(entity, &["lsp"], Verdict::Tp)], sets);
        let t2 = TargetInput {
            id: "t2".to_owned(),
            category: "method".to_owned(),
            systems: SystemSets::default(),
            items: Vec::new(),
        };
        let (systems, _) = compute(&[t1, t2]);
        let lsp = &systems["lsp"];
        assert_eq!(lsp.macro_avg.precision, Some(1.0));
        assert_eq!(lsp.macro_avg.recall, Some(1.0));
        assert_eq!(lsp.macro_avg.targets_with_gold, 1);
        assert_eq!(lsp.macro_avg.targets_with_found, 1);
    }
}
