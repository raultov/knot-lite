//! What only `search_hybrid_context` needs from an index, built on its first
//! search and kept with the [`LoadedIndex`] (P5-D10): the BM25 keyword index
//! and the heuristic `CALLS` graph by record position.

use std::time::Instant;

use super::layout::IndexLayout;
use super::load::LoadedIndex;
use super::manifest::LEXICON_FORMAT_V1;
use super::record_io::{list_record_files, read_jsonl};
use crate::lexicon::bm25::Bm25Index;
use crate::lexicon::store::LexiconLine;
use crate::lexicon::tokenize::TOKENIZER;

/// Note of a search answered with the names-only keyword channel.
pub const NAMES_ONLY_NOTE: &str = "keyword channel uses names only (index built without a lexicon); run 'knot-lite index' to include documentation and file text";

/// The relationship label of a call edge.
const CALLS: &str = "CALLS";

/// The search-only structures of one loaded index.
#[derive(Debug)]
pub struct SearchAux {
    /// BM25 over the lexicon, or over names when the index has none.
    pub lexicon: Bm25Index,
    /// Whether [`Self::lexicon`] comes from the stored lexicon (`false`: names only).
    pub lexicon_full: bool,
    /// Callees (heuristic `CALLS` edges) of every record position, ascending.
    pub calls_out: Vec<Vec<u32>>,
    /// Callers (heuristic `CALLS` edges) of every record position, ascending.
    pub calls_in: Vec<Vec<u32>>,
    /// Milliseconds the build took.
    pub build_ms: u64,
}

impl SearchAux {
    /// Build from `index`; `layout` is where its lexicon lives (`None` for an
    /// index assembled in memory).
    pub fn build(index: &LoadedIndex, layout: Option<&IndexLayout>) -> Self {
        let started = Instant::now();
        let stored = layout.and_then(|layout| stored_lexicon(index, layout));
        let lexicon_full = stored.is_some();
        let lexicon = stored.unwrap_or_else(|| names_only(index));
        let (calls_out, calls_in) = call_graph(index);
        Self {
            lexicon,
            lexicon_full,
            calls_out,
            calls_in,
            build_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// The stored lexicon, when the manifest names one this version reads and
/// every file parses; anything else falls back to names (with a debug log).
fn stored_lexicon(index: &LoadedIndex, layout: &IndexLayout) -> Option<Bm25Index> {
    let table = index.manifest.lexicon.as_ref()?;
    if table.format != LEXICON_FORMAT_V1 || table.tokenizer != TOKENIZER {
        tracing::debug!(
            format = table.format,
            tokenizer = table.tokenizer,
            "lexicon written by another format or tokenizer; using names only"
        );
        return None;
    }
    let mut lines: Vec<LexiconLine> = Vec::new();
    let read = list_record_files(&layout.lexicon()).and_then(|files| {
        files.iter().try_for_each(|path| {
            lines.extend(read_jsonl::<LexiconLine>(path)?);
            Ok(())
        })
    });
    if let Err(error) = read {
        tracing::warn!("cannot read the lexicon ({error:#}); using names only");
        return None;
    }
    Some(Bm25Index::from_lines(lines, index.records.len(), |id| {
        index
            .by_id
            .get(id)
            .copied()
            .filter(|&at| index.records[at].row.is_some())
    }))
}

/// BM25 over `name fqn signature` of every record with a vector.
fn names_only(index: &LoadedIndex) -> Bm25Index {
    Bm25Index::from_names(
        index
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.row.is_some())
            .map(|(at, record)| {
                let line = &record.line;
                (
                    at,
                    [
                        line.name.as_str(),
                        line.fqn.as_str(),
                        line.sig.as_deref().unwrap_or(""),
                    ],
                )
            }),
        index.records.len(),
    )
}

/// `(callees, callers)` per record position from the heuristic `CALLS` edges.
fn call_graph(index: &LoadedIndex) -> (Vec<Vec<u32>>, Vec<Vec<u32>>) {
    let mut calls_out: Vec<Vec<u32>> = vec![Vec::new(); index.records.len()];
    let mut calls_in: Vec<Vec<u32>> = vec![Vec::new(); index.records.len()];
    let position = |id: &str| index.by_id.get(id).and_then(|&at| u32::try_from(at).ok());
    for (target, callers) in &index.incoming {
        let Some(callee) = position(target) else {
            continue;
        };
        for (caller, _) in callers.iter().filter(|(_, rel)| rel == CALLS) {
            if let Some(caller) = position(caller) {
                calls_out[caller as usize].push(callee);
                calls_in[callee as usize].push(caller);
            }
        }
    }
    for list in calls_out.iter_mut().chain(calls_in.iter_mut()) {
        list.sort_unstable();
        list.dedup();
    }
    (calls_out, calls_in)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build::test_support::{fixture, run};
    use crate::index::load::load_index;
    use crate::index::load::test_support::{entity, synthetic_index};
    use crate::index::manifest::{read_manifest, write_manifest_atomic};
    use crate::lexicon::tokenize::query_terms;

    fn fixture_index() -> (tempfile::TempDir, LoadedIndex) {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let index = load_index(&IndexLayout::for_repo(tmp.path())).expect("load");
        (tmp, index)
    }

    fn position_of(index: &LoadedIndex, name: &str) -> usize {
        index
            .records
            .iter()
            .position(|record| record.line.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    #[test]
    fn stored_lexicon_is_used_for_every_record_with_a_vector() {
        let (_tmp, index) = fixture_index();
        let aux = index.search_aux();
        assert!(aux.lexicon_full);
        let with_vector = index.records.iter().filter(|r| r.row.is_some()).count();
        assert_eq!(aux.lexicon.docs(), with_vector);
        // A word of the fixture's Markdown guide body is found (the full text,
        // not just names).
        let top = aux.lexicon.top(&query_terms("guide"), |_| true, 3);
        assert!(!top.is_empty());
    }

    #[test]
    fn search_aux_built_once_per_loaded_index() {
        let (_tmp, index) = fixture_index();
        let first: *const SearchAux = index.search_aux();
        let second: *const SearchAux = index.search_aux();
        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn names_only_fallback_without_a_lexicon() {
        let (tmp, _) = fixture_index();
        let layout = IndexLayout::for_repo(tmp.path());
        let mut manifest = read_manifest(&layout.manifest).expect("manifest");
        manifest.lexicon = None;
        write_manifest_atomic(&layout.manifest, &manifest).expect("write");
        let index = load_index(&layout).expect("load");
        let aux = index.search_aux();
        assert!(!aux.lexicon_full);
        let with_vector = index.records.iter().filter(|r| r.row.is_some()).count();
        assert_eq!(aux.lexicon.docs(), with_vector);
        let target = position_of(&index, "target");
        let top = aux.lexicon.top(&query_terms("target"), |_| true, 1);
        assert_eq!(top[0].0, target);
    }

    #[test]
    fn tokenizer_mismatch_counts_as_absent() {
        let (tmp, _) = fixture_index();
        let layout = IndexLayout::for_repo(tmp.path());
        let mut manifest = read_manifest(&layout.manifest).expect("manifest");
        manifest.lexicon.as_mut().expect("[lexicon]").tokenizer = "kl-lex-0".to_owned();
        write_manifest_atomic(&layout.manifest, &manifest).expect("write");
        assert!(!load_index(&layout).expect("load").search_aux().lexicon_full);
    }

    #[test]
    fn unreadable_lexicon_counts_as_absent() {
        let (tmp, _) = fixture_index();
        let layout = IndexLayout::for_repo(tmp.path());
        std::fs::write(
            layout.lexicon_for("src/lib.rs").expect("path"),
            "not json\n",
        )
        .expect("write");
        assert!(!load_index(&layout).expect("load").search_aux().lexicon_full);
    }

    #[test]
    fn in_memory_index_uses_names_only() {
        let index = synthetic_index(
            vec![(
                "src/a.rs",
                entity("1", "rust_function", "run", "a::run", (1, 2)),
            )],
            &[],
        );
        let aux = index.search_aux();
        assert!(!aux.lexicon_full);
        assert_eq!(aux.lexicon.docs(), 0, "no vectors, no documents");
    }

    #[test]
    fn call_graph_has_calls_edges_only_both_ways() {
        let index = synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("t", "rust_function", "t", "a::t", (1, 2)),
                ),
                (
                    "src/a.rs",
                    entity("c1", "rust_function", "c1", "a::c1", (3, 4)),
                ),
                (
                    "src/a.rs",
                    entity("c2", "rust_function", "c2", "a::c2", (5, 6)),
                ),
                (
                    "src/a.rs",
                    entity("ty", "rust_struct", "Ty", "a::Ty", (7, 8)),
                ),
            ],
            &[
                ("c1", "t", "CALLS"),
                ("c2", "t", "CALLS"),
                ("c2", "ty", "REFERENCES"),
            ],
        );
        let aux = index.search_aux();
        let at = |name: &str| u32::try_from(position_of(&index, name)).expect("u32");
        let mut callers = vec![at("c1"), at("c2")];
        callers.sort_unstable();
        assert_eq!(aux.calls_in[at("t") as usize], callers);
        assert_eq!(aux.calls_out[at("c2") as usize], vec![at("t")]);
        assert!(aux.calls_in[at("Ty") as usize].is_empty());
    }
}
