//! Recall channels of the hybrid search (P5-D7, §6.3.1): the candidate pool
//! knot's re-rank orders.
//!
//! Mirrors knot 1.11.3's `search_hybrid_context/{mod,recall,coverage}.rs`
//! (functions there are tied to Qdrant/Neo4j, so the channels are
//! re-implemented over the in-memory index; the constants are copied and cite
//! their source), plus the BM25 keyword channel knot does not have.

use std::collections::{HashMap, HashSet};

use knot::cli_tools::kinds::is_non_code_kind;
use knot::cli_tools::search_hybrid_context::rank::{
    candidate_limit, is_test_path, kind_boost, probe_name_variants, significant_query_tokens,
};

use crate::index::load::LoadedIndex;
use crate::index::search_aux::SearchAux;
use crate::lexicon::bm25::top_of;
use crate::utils::top_scored;

/// Seeded roots of the caller bridge (knot `coverage.rs:24`, `TOP_ROOTS`).
pub const TOP_ROOTS: usize = 8;
/// Seed slots a name-probe row may fill (knot `coverage.rs:31`, `PROBE_ROOTS_MAX`).
pub const PROBE_ROOTS_MAX: usize = 3;
/// Rows the name probe recalls (knot `recall.rs:185`, `PROBE_RESULT_LIMIT`).
pub const PROBE_RESULT_LIMIT: usize = 48;
/// Caller links the bridge reads (knot `coverage.rs:38`, `CALLER_LINK_LIMIT`).
pub const CALLER_LINK_LIMIT: usize = TOP_ROOTS * 32;
/// Second-hop caller links (knot `coverage.rs:44`, `CALLER_LINK_LIMIT_DEPTH2`).
pub const CALLER_LINK_LIMIT_DEPTH2: usize = TOP_ROOTS * 16;

/// Channel names (knot's `_channel` values, plus `keyword`).
pub const CHANNEL_COSINE: &str = "cosine";
/// Second cosine pass over code kinds.
pub const CHANNEL_DEFINITION: &str = "definition";
/// Names equal to (or sharing an identifier token with) a query token.
pub const CHANNEL_PROBE: &str = "probe";
/// Names starting with the query.
pub const CHANNEL_PREFIX: &str = "prefix";
/// BM25 over the lexicon.
pub const CHANNEL_KEYWORD: &str = "keyword";
/// Callers of the seeded roots.
pub const CHANNEL_BRIDGE: &str = "bridge";

/// What the channels read.
pub struct Recall<'a> {
    /// The loaded index.
    pub index: &'a LoadedIndex,
    /// Its search structures.
    pub aux: &'a SearchAux,
    /// The query text.
    pub query: &'a str,
    /// The expanded kind filter (empty = every kind).
    pub kinds: &'a [String],
    /// Cosine of every eligible record, by position; `None` = filtered out.
    pub cosine: &'a [Option<f32>],
    /// BM25 of the eligible records that contain a query term.
    pub keyword: &'a HashMap<usize, f32>,
    /// The requested result count.
    pub max_results: usize,
}

/// One pool candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolRow {
    /// Record position.
    pub position: usize,
    /// Every channel that produced it, the first one first.
    pub channels: Vec<&'static str>,
}

/// The recall result.
#[derive(Debug, Default)]
pub struct Pool {
    /// Name-prefix hits of code kinds: the leading result slots, never re-ranked
    /// (knot `mod.rs:101-120`).
    pub lead: Vec<usize>,
    /// The candidates for the re-rank, in channel order.
    pub rows: Vec<PoolRow>,
    /// The caller-bridge seeds (record positions).
    pub seeds: Vec<usize>,
}

/// A row that may seed the bridge.
struct SeedCandidate<'a> {
    cosine: f32,
    /// Tie-break: `(file, first line, id)`.
    order: (&'a str, usize, &'a str),
    position: usize,
    probe: bool,
}

/// Rows merged in channel order; a later channel only adds its name.
#[derive(Default)]
struct PoolBuilder {
    rows: Vec<PoolRow>,
    at: HashMap<usize, usize>,
    lead: HashSet<usize>,
}

impl PoolBuilder {
    fn merge(&mut self, positions: impl IntoIterator<Item = usize>, channel: &'static str) {
        for position in positions {
            if self.lead.contains(&position) {
                continue;
            }
            match self.at.get(&position) {
                Some(&row) => {
                    if !self.rows[row].channels.contains(&channel) {
                        self.rows[row].channels.push(channel);
                    }
                }
                None => {
                    self.at.insert(position, self.rows.len());
                    self.rows.push(PoolRow {
                        position,
                        channels: vec![channel],
                    });
                }
            }
        }
    }

    fn contains(&self, position: usize) -> bool {
        self.at.contains_key(&position) || self.lead.contains(&position)
    }
}

impl Recall<'_> {
    /// Run every channel and seed the bridge (§6.3.1 order: cosine,
    /// definition, probe, demoted prefix, keyword, then the bridge).
    pub fn collect(&self) -> Pool {
        let window = candidate_limit(self.max_results);
        let (lead, demoted) = self.prefix_hits();
        let mut pool = PoolBuilder {
            lead: lead.iter().copied().collect(),
            ..PoolBuilder::default()
        };
        pool.merge(self.cosine_window(window, |_| true), CHANNEL_COSINE);
        if should_run_definition_channel(self.kinds) {
            pool.merge(
                self.cosine_window(window, |at| self.is_code(at)),
                CHANNEL_DEFINITION,
            );
        }
        pool.merge(self.probe_hits(), CHANNEL_PROBE);
        pool.merge(demoted, CHANNEL_PREFIX);
        pool.merge(
            top_of(self.keyword, window).into_iter().map(|(at, _)| at),
            CHANNEL_KEYWORD,
        );
        let seeds = self.seed_roots(&pool.rows);
        let bridge = self.bridge(&seeds, &pool, window);
        pool.merge(bridge, CHANNEL_BRIDGE);
        Pool {
            lead,
            rows: pool.rows,
            seeds,
        }
    }

    fn cosine_of(&self, at: usize) -> Option<f32> {
        self.cosine.get(at).copied().flatten()
    }

    /// An entity record of a code kind (not prose, config, or a tier-1 window).
    fn is_code(&self, at: usize) -> bool {
        let line = &self.index.records[at].line;
        !line.is_tier1() && !is_non_code_kind(&line.kind)
    }

    /// The `k` eligible records with the highest cosine among those `keep` accepts.
    fn cosine_window(&self, k: usize, keep: impl Fn(usize) -> bool) -> Vec<usize> {
        let scored: Vec<(usize, f32)> = (0..self.cosine.len())
            .filter(|&at| keep(at))
            .filter_map(|at| self.cosine_of(at).map(|cosine| (at, cosine)))
            .collect();
        top_scored(scored, k)
            .into_iter()
            .map(|(at, _)| at)
            .collect()
    }

    /// Name probe (knot `recall.rs:191-214`): names equal to a significant query
    /// token (lower-case or PascalCase) or sharing an identifier token with one,
    /// the [`PROBE_RESULT_LIMIT`] best by cosine.
    fn probe_hits(&self) -> Vec<usize> {
        let tokens = significant_query_tokens(self.query);
        if tokens.is_empty() {
            return Vec::new();
        }
        let by_name = probe_name_variants(&tokens)
            .into_iter()
            .filter_map(|name| self.index.by_name.get(&name))
            .flatten()
            .copied();
        let by_token = tokens
            .iter()
            .filter_map(|token| self.aux.name_tokens.get(token))
            .flatten()
            .map(|&at| at as usize);
        let candidates: HashSet<usize> = by_name.chain(by_token).collect();
        let scored = candidates
            .into_iter()
            .filter_map(|at| self.cosine_of(at).map(|cosine| (at, cosine)))
            .collect();
        top_scored(scored, PROBE_RESULT_LIMIT)
            .into_iter()
            .map(|(at, _)| at)
            .collect()
    }

    /// Entity names starting with the whole query, case-insensitive (knot
    /// `db/graph/query.rs:1234-1260`): exact matches first, then shorter names,
    /// FQN, id; at most `max_results`. Code kinds become the leading slots,
    /// prose/config ones are demoted into the pool (`recall.rs:133-173`).
    fn prefix_hits(&self) -> (Vec<usize>, Vec<usize>) {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut hits: Vec<(bool, usize, &str, &str, usize)> = (0..self.cosine.len())
            .filter(|&at| self.cosine_of(at).is_some())
            .filter_map(|at| {
                let line = &self.index.records[at].line;
                let name = line.name.to_lowercase();
                (!line.is_tier1() && name.starts_with(&query)).then(|| {
                    (
                        name != query,
                        name.chars().count(),
                        line.fqn.as_str(),
                        line.id.as_str(),
                        at,
                    )
                })
            })
            .collect();
        hits.sort_unstable();
        hits.truncate(self.max_results);
        hits.into_iter()
            .map(|(.., at)| at)
            .partition(|&at| !is_non_code_kind(&self.index.records[at].line.kind))
    }

    /// Bridge seeds (knot `coverage.rs:60-106`): production rows of a
    /// non-negative kind boost, by cosine descending then `(file, line, id)`,
    /// at most [`PROBE_ROOTS_MAX`] of them from the probe, [`TOP_ROOTS`] in all.
    /// Rows that entered through the keyword channel are not seeded.
    fn seed_roots(&self, rows: &[PoolRow]) -> Vec<usize> {
        let mut seedable: Vec<SeedCandidate<'_>> = rows
            .iter()
            .filter(|row| row.channels[0] != CHANNEL_KEYWORD)
            .filter_map(|row| {
                let record = &self.index.records[row.position];
                let line = &record.line;
                let production =
                    kind_boost(&line.kind) >= 0.0 && !is_test_path(&record.file) && !line.tc;
                production.then(|| SeedCandidate {
                    cosine: self.cosine_of(row.position).unwrap_or(0.0),
                    order: (record.file.as_str(), line.l0, line.id.as_str()),
                    position: row.position,
                    probe: row.channels[0] == CHANNEL_PROBE,
                })
            })
            .collect();
        seedable.sort_by(|a, b| {
            b.cosine
                .total_cmp(&a.cosine)
                .then_with(|| a.order.cmp(&b.order))
        });
        let mut seeds = Vec::new();
        let mut probes = 0;
        for candidate in seedable {
            if seeds.len() == TOP_ROOTS {
                break;
            }
            if candidate.probe {
                if probes >= PROBE_ROOTS_MAX {
                    continue;
                }
                probes += 1;
            }
            seeds.push(candidate.position);
        }
        seeds
    }

    /// Caller bridge (knot `coverage.rs:171-253`): callers of the seeds not yet
    /// in the pool, the best `window` by cosine; when that leaves room, callers
    /// of callers (never a seed itself) fill the rest.
    fn bridge(&self, seeds: &[usize], pool: &PoolBuilder, window: usize) -> Vec<usize> {
        let direct = self.caller_links(seeds, |seed| self.aux.calls_in[seed].iter(), false);
        if direct.is_empty() {
            return Vec::new();
        }
        let mut hits = self.fetch(&direct, |at| !pool.contains(at), window);
        if hits.len() < window {
            let second = self.caller_links(
                seeds,
                |seed| {
                    self.aux.calls_in[seed]
                        .iter()
                        .flat_map(|&h| self.aux.calls_in[h as usize].iter())
                },
                true,
            );
            let union: HashSet<usize> = direct.union(&second).copied().collect();
            let taken: HashSet<usize> = hits.iter().copied().collect();
            hits.extend(self.fetch(
                &union,
                |at| !pool.contains(at) && !taken.contains(&at),
                window - hits.len(),
            ));
        }
        hits
    }

    /// The callers named by the first `limit` `(caller id, seed id)` links in
    /// id order (knot's Cypher `ORDER BY caller_uuid, target_uuid LIMIT`).
    fn caller_links<'s, I>(
        &'s self,
        seeds: &[usize],
        callers_of: impl Fn(usize) -> I,
        second_hop: bool,
    ) -> HashSet<usize>
    where
        I: Iterator<Item = &'s u32>,
    {
        let id = |at: usize| self.index.records[at].line.id.as_str();
        let mut links: Vec<(&str, &str, usize)> = seeds
            .iter()
            .flat_map(|&seed| {
                callers_of(seed)
                    .map(|&caller| caller as usize)
                    .filter(|caller| !second_hop || !seeds.contains(caller))
                    .map(move |caller| (id(caller), id(seed), caller))
            })
            .collect();
        links.sort_unstable();
        links.dedup();
        let limit = if second_hop {
            CALLER_LINK_LIMIT_DEPTH2
        } else {
            CALLER_LINK_LIMIT
        };
        links
            .into_iter()
            .take(limit)
            .map(|(.., caller)| caller)
            .collect()
    }

    /// The `limit` eligible `candidates` that `keep` accepts, best cosine first.
    fn fetch(
        &self,
        candidates: &HashSet<usize>,
        keep: impl Fn(usize) -> bool,
        limit: usize,
    ) -> Vec<usize> {
        let scored = candidates
            .iter()
            .copied()
            .filter(|&at| keep(at))
            .filter_map(|at| self.cosine_of(at).map(|cosine| (at, cosine)))
            .collect();
        top_scored(scored, limit)
            .into_iter()
            .map(|(at, _)| at)
            .collect()
    }
}

/// Whether the definition pass runs (knot `recall.rs:83-93`): always without
/// a kind filter, and for a filter that mixes code and non-code kinds; not for
/// a filter of only one sort (the cosine window already is that pass).
fn should_run_definition_channel(kinds: &[String]) -> bool {
    if kinds.is_empty() {
        return true;
    }
    let all = |non_code: bool| kinds.iter().all(|kind| is_non_code_kind(kind) == non_code);
    !(all(true) || all(false))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;

    use crate::index::load::LoadedIndex;
    use crate::index::search_aux::SearchAux;
    use crate::lexicon::bm25::Bm25Index;

    /// Position of the record named `name`.
    pub(crate) fn at(index: &LoadedIndex, name: &str) -> usize {
        index
            .records
            .iter()
            .position(|record| record.line.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    /// A hand-made [`SearchAux`]: `calls` are `(caller, callee)` names; every
    /// entity name is indexed by its identifier tokens.
    pub(crate) fn aux_for(index: &LoadedIndex, calls: &[(&str, &str)]) -> SearchAux {
        let n = index.records.len();
        let mut calls_out = vec![Vec::new(); n];
        let mut calls_in = vec![Vec::new(); n];
        for (caller, callee) in calls {
            let (caller, callee) = (at(index, caller), at(index, callee));
            calls_out[caller].push(u32::try_from(callee).expect("u32"));
            calls_in[callee].push(u32::try_from(caller).expect("u32"));
        }
        for list in calls_out.iter_mut().chain(calls_in.iter_mut()) {
            list.sort_unstable();
        }
        let mut name_tokens: HashMap<String, Vec<u32>> = HashMap::new();
        for (position, record) in index.records.iter().enumerate() {
            for token in knot::utils::identifiers::identifier_tokens(&record.line.name) {
                let list = name_tokens.entry(token).or_default();
                let position = u32::try_from(position).expect("u32");
                if !list.contains(&position) {
                    list.push(position);
                }
            }
        }
        SearchAux {
            lexicon: Bm25Index::default(),
            lexicon_full: false,
            calls_out,
            calls_in,
            name_tokens,
            build_ms: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{at, aux_for};
    use super::*;
    use crate::index::load::test_support::{entity, synthetic_index};

    fn recall<'a>(
        index: &'a LoadedIndex,
        aux: &'a SearchAux,
        query: &'a str,
        cosine: &'a [Option<f32>],
        keyword: &'a HashMap<usize, f32>,
    ) -> Recall<'a> {
        Recall {
            index,
            aux,
            query,
            kinds: &[],
            cosine,
            keyword,
            max_results: 5,
        }
    }

    /// Cosine of every record: `scores` by name, `0.1` for the rest.
    fn cosines(index: &LoadedIndex, scores: &[(&str, f32)]) -> Vec<Option<f32>> {
        let mut cosine = vec![Some(0.1); index.records.len()];
        for (name, score) in scores {
            cosine[at(index, name)] = Some(*score);
        }
        cosine
    }

    fn row<'a>(pool: &'a Pool, index: &LoadedIndex, name: &str) -> &'a PoolRow {
        let position = at(index, name);
        pool.rows
            .iter()
            .find(|row| row.position == position)
            .unwrap_or_else(|| panic!("{name} not in the pool"))
    }

    fn function(id: &str, name: &str, l0: usize) -> crate::index::shard::ShardLine {
        entity(
            id,
            "rust_function",
            name,
            &format!("m::{name}"),
            (l0, l0 + 1),
        )
    }

    #[test]
    fn pool_dedup_keeps_first_channel_and_all_channels_listed() {
        let index = synthetic_index(
            vec![
                ("src/a.rs", function("a", "alpha", 1)),
                ("src/a.rs", function("b", "beta", 3)),
            ],
            &[],
        );
        let aux = aux_for(&index, &[]);
        let cosine = cosines(&index, &[("alpha", 0.9)]);
        let keyword = HashMap::from([(at(&index, "alpha"), 2.0), (at(&index, "beta"), 1.0)]);
        let pool = recall(&index, &aux, "zzz", &cosine, &keyword).collect();
        assert_eq!(pool.rows.len(), 2, "no duplicates");
        assert_eq!(
            row(&pool, &index, "alpha").channels,
            vec![CHANNEL_COSINE, CHANNEL_DEFINITION, CHANNEL_KEYWORD]
        );
        assert_eq!(row(&pool, &index, "beta").channels[0], CHANNEL_COSINE);
    }

    #[test]
    fn definition_pass_respects_kind_filter() {
        let empty: HashMap<usize, f32> = HashMap::new();
        let owned =
            |kinds: &[&str]| -> Vec<String> { kinds.iter().map(|k| (*k).to_owned()).collect() };
        assert!(should_run_definition_channel(&[]));
        assert!(should_run_definition_channel(&owned(&[
            "rust_function",
            "markdown_section"
        ])));
        assert!(!should_run_definition_channel(&owned(&[
            "markdown_section",
            "markdown_document"
        ])));
        assert!(!should_run_definition_channel(&owned(&[
            "rust_function",
            "rust_method"
        ])));
        // The pass itself only takes code entities.
        let index = synthetic_index(
            vec![
                (
                    "docs/a.md",
                    entity("d", "markdown_section", "Intro", "a::Intro", (1, 9)),
                ),
                ("src/a.rs", function("f", "run", 1)),
            ],
            &[],
        );
        let aux = aux_for(&index, &[]);
        let recall = recall(&index, &aux, "x", &[Some(0.9), Some(0.2)], &empty);
        let doc = at(&index, "Intro");
        assert!(
            !recall
                .cosine_window(5, |at| recall.is_code(at))
                .contains(&doc)
        );
    }

    #[test]
    fn name_probe_uses_knot_variants_and_identifier_tokens() {
        let empty: HashMap<usize, f32> = HashMap::new();
        let index = synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("t", "rust_struct", "Lookup", "m::Lookup", (1, 2)),
                ),
                ("src/a.rs", function("f", "build_lookup_maps", 3)),
                ("src/a.rs", function("g", "unrelated", 5)),
            ],
            &[],
        );
        let aux = aux_for(&index, &[]);
        let cosine = vec![Some(0.1); 3];
        let recall = recall(&index, &aux, "how are lookup tables made", &cosine, &empty);
        let mut probed = recall.probe_hits();
        probed.sort_unstable();
        let mut expected = vec![at(&index, "Lookup"), at(&index, "build_lookup_maps")];
        expected.sort_unstable();
        assert_eq!(probed, expected);
    }

    #[test]
    fn prefix_hits_lead_for_code_and_are_demoted_for_prose() {
        let empty: HashMap<usize, f32> = HashMap::new();
        let index = synthetic_index(
            vec![
                (
                    "docs/p.md",
                    entity("d", "markdown_section", "Parsing", "p::Parsing", (1, 9)),
                ),
                (
                    "src/p.rs",
                    entity("s", "rust_struct", "Parser", "p::Parser", (1, 9)),
                ),
                ("src/p.rs", function("f", "parse", 11)),
            ],
            &[],
        );
        let aux = aux_for(&index, &[]);
        let cosine = vec![Some(0.1); 3];
        let pool = recall(&index, &aux, "Pars", &cosine, &empty).collect();
        // Shorter names first: `parse` (5 characters) before `Parser` (6).
        assert_eq!(pool.lead, vec![at(&index, "parse"), at(&index, "Parser")]);
        assert!(
            pool.rows
                .iter()
                .all(|row| row.position != at(&index, "Parser"))
        );
        assert!(
            row(&pool, &index, "Parsing")
                .channels
                .contains(&CHANNEL_PREFIX)
        );
        // A name equal to the query ignoring case comes first.
        let pool = recall(&index, &aux, "PARSER", &cosine, &empty).collect();
        assert_eq!(pool.lead, vec![at(&index, "Parser")]);
    }

    #[test]
    fn seed_roots_cap_probe_hits_and_skip_tests_and_keywords() {
        let empty: HashMap<usize, f32> = HashMap::new();
        let mut records: Vec<(&str, crate::index::shard::ShardLine)> = (0..6)
            .map(|i| {
                (
                    "src/a.rs",
                    function(&format!("p{i}"), &format!("p{i}"), i * 2 + 1),
                )
            })
            .collect();
        records.push(("src/a.rs", function("c", "code", 20)));
        records.push(("tests/t.rs", function("t", "in_tests_dir", 1)));
        let mut tc = function("u", "in_cfg_test", 30);
        tc.tc = true;
        records.push(("src/a.rs", tc));
        records.push(("src/a.rs", function("k", "keyword_only", 40)));
        let index = synthetic_index(records, &[]);
        let aux = aux_for(&index, &[]);
        let cosine = vec![Some(0.5); index.records.len()];
        let recall = recall(&index, &aux, "x", &cosine, &empty);
        let pool_row = |name: &str, channel| PoolRow {
            position: at(&index, name),
            channels: vec![channel],
        };
        let mut rows: Vec<PoolRow> = (0..6)
            .map(|i| pool_row(&format!("p{i}"), CHANNEL_PROBE))
            .collect();
        rows.push(pool_row("code", CHANNEL_COSINE));
        rows.push(pool_row("in_tests_dir", CHANNEL_COSINE));
        rows.push(pool_row("in_cfg_test", CHANNEL_COSINE));
        rows.push(pool_row("keyword_only", CHANNEL_KEYWORD));
        let seeds = recall.seed_roots(&rows);
        let probes = seeds
            .iter()
            .filter(|&&s| index.records[s].line.name.starts_with('p'))
            .count();
        assert_eq!(probes, PROBE_ROOTS_MAX);
        assert!(seeds.contains(&at(&index, "code")));
        for excluded in ["in_tests_dir", "in_cfg_test", "keyword_only"] {
            assert!(!seeds.contains(&at(&index, excluded)), "{excluded}");
        }
        assert_eq!(seeds.len(), PROBE_ROOTS_MAX + 1);
    }

    #[test]
    fn bridge_adds_callers_of_seed_roots_then_callers_of_callers() {
        let empty: HashMap<usize, f32> = HashMap::new();
        let index = synthetic_index(
            vec![
                ("src/a.rs", function("r", "root", 1)),
                ("src/a.rs", function("h", "helper", 3)),
                ("src/a.rs", function("e", "entry", 5)),
                ("src/a.rs", function("o", "other", 7)),
            ],
            &[],
        );
        let aux = aux_for(&index, &[("helper", "root"), ("entry", "helper")]);
        let cosine = cosines(
            &index,
            &[
                ("root", 0.9),
                ("helper", 0.05),
                ("entry", 0.04),
                ("other", 0.03),
            ],
        );
        let mut recall = recall(&index, &aux, "zzz", &cosine, &empty);
        // A window of 24 holds every record, so make the root the only pool row.
        let pool = PoolBuilder {
            rows: vec![PoolRow {
                position: at(&index, "root"),
                channels: vec![CHANNEL_COSINE],
            }],
            at: HashMap::from([(at(&index, "root"), 0)]),
            lead: HashSet::new(),
        };
        recall.max_results = 5;
        let bridged = recall.bridge(&[at(&index, "root")], &pool, 24);
        assert_eq!(bridged, vec![at(&index, "helper"), at(&index, "entry")]);
        assert!(recall.bridge(&[at(&index, "other")], &pool, 24).is_empty());
    }
}
