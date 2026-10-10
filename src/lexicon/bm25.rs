//! BM25 over the lexicon (P5-D10): `k1 = 1.2`, `b = 0.75`,
//! `idf = ln(1 + (N − df + 0.5) / (df + 0.5))`.
//!
//! Documents are record positions of the loaded index; only records with a
//! vector are documents, so every keyword hit can also be scored by cosine.

use std::collections::HashMap;

use super::store::LexiconLine;

const K1: f32 = 1.2;
const B: f32 = 0.75;

/// An inverted index of term frequencies per record position.
#[derive(Debug, Default)]
pub struct Bm25Index {
    /// Term → `(record position, term frequency)`, positions ascending.
    postings: HashMap<String, Vec<(u32, u16)>>,
    /// Document length per record position (0 for records that are not documents).
    doc_len: Vec<u32>,
    /// Documents in the index.
    docs: usize,
    /// Mean document length.
    avgdl: f32,
}

impl Bm25Index {
    /// Build from stored lexicon lines; `position` maps a record id to its
    /// position, `None` for ids that are not documents (unknown, or without a
    /// vector). `records` is the number of records of the index.
    pub fn from_lines(
        lines: impl IntoIterator<Item = LexiconLine>,
        records: usize,
        position: impl Fn(&str) -> Option<usize>,
    ) -> Self {
        let mut builder = Builder::new(records);
        for line in lines {
            if let Some(at) = position(&line.id) {
                builder.add(at, line.n, line.t);
            }
        }
        builder.finish()
    }

    /// The names-only fallback: each document's text is `"<name> <fqn> <signature>"`.
    pub fn from_names<'a>(
        documents: impl IntoIterator<Item = (usize, [&'a str; 3])>,
        records: usize,
    ) -> Self {
        let mut builder = Builder::new(records);
        for (at, parts) in documents {
            let line = LexiconLine::from_text("", &parts.join(" "));
            builder.add(at, line.n, line.t);
        }
        builder.finish()
    }

    /// Documents in the index.
    pub fn docs(&self) -> usize {
        self.docs
    }

    /// BM25 of every document that contains at least one of `query_terms`
    /// and passes `eligible`, by record position.
    pub fn scores(
        &self,
        query_terms: &[String],
        eligible: impl Fn(usize) -> bool,
    ) -> HashMap<usize, f32> {
        let mut scores: HashMap<usize, f32> = HashMap::new();
        if self.docs == 0 {
            return scores;
        }
        let n = self.docs as f32;
        for term in query_terms {
            let Some(postings) = self.postings.get(term) else {
                continue;
            };
            let df = postings.len() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for &(at, tf) in postings {
                let at = at as usize;
                if !eligible(at) {
                    continue;
                }
                let tf = f32::from(tf);
                let dl = self.doc_len.get(at).copied().unwrap_or(0) as f32;
                let norm = K1 * (1.0 - B + B * dl / self.avgdl.max(f32::EPSILON));
                *scores.entry(at).or_default() += idf * tf * (K1 + 1.0) / (tf + norm);
            }
        }
        scores
    }

    /// The `k` best documents for `query_terms` among `eligible` ones: score
    /// descending, ties by record position.
    pub fn top(
        &self,
        query_terms: &[String],
        eligible: impl Fn(usize) -> bool,
        k: usize,
    ) -> Vec<(usize, f32)> {
        top_of(&self.scores(query_terms, eligible), k)
    }
}

/// The `k` highest entries of `scores` (positive only): score descending,
/// ties by record position.
pub fn top_of(scores: &HashMap<usize, f32>, k: usize) -> Vec<(usize, f32)> {
    let mut ranked: Vec<(usize, f32)> = scores
        .iter()
        .filter(|(_, score)| **score > 0.0)
        .map(|(&at, &score)| (at, score))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(k);
    ranked
}

struct Builder {
    postings: HashMap<String, Vec<(u32, u16)>>,
    doc_len: Vec<u32>,
    docs: usize,
    total_len: u64,
}

impl Builder {
    fn new(records: usize) -> Self {
        Self {
            postings: HashMap::new(),
            doc_len: vec![0; records],
            docs: 0,
            total_len: 0,
        }
    }

    fn add(&mut self, at: usize, len: u32, terms: Vec<(String, u16)>) {
        let (Some(slot), Ok(position)) = (self.doc_len.get_mut(at), u32::try_from(at)) else {
            return;
        };
        if *slot > 0 || terms.is_empty() {
            return;
        }
        *slot = len.max(1);
        self.docs += 1;
        self.total_len += u64::from(*slot);
        for (term, tf) in terms {
            self.postings.entry(term).or_default().push((position, tf));
        }
    }

    fn finish(mut self) -> Bm25Index {
        for postings in self.postings.values_mut() {
            postings.sort_unstable_by_key(|&(at, _)| at);
        }
        let avgdl = if self.docs == 0 {
            0.0
        } else {
            self.total_len as f32 / self.docs as f32
        };
        Bm25Index {
            postings: self.postings,
            doc_len: self.doc_len,
            docs: self.docs,
            avgdl,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::tokenize::query_terms;

    fn line(id: &str, text: &str) -> LexiconLine {
        LexiconLine::from_text(id, text)
    }

    /// Three documents: "alpha beta", "alpha alpha gamma delta", "beta".
    fn corpus() -> Bm25Index {
        let ids = ["d0", "d1", "d2"];
        Bm25Index::from_lines(
            vec![
                line("d0", "alpha beta"),
                line("d1", "alpha alpha gamma delta"),
                line("d2", "beta"),
            ],
            3,
            |id| ids.iter().position(|known| *known == id),
        )
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn idf_and_length_normalisation_reference_values() {
        // N = 3, avgdl = (2 + 4 + 1) / 3 = 7/3.
        // "alpha": df = 2, idf = ln(1 + 1.5/2.5) = ln 1.6 = 0.470004.
        //   d0: tf 1, dl 2 → 0.470004 * 2.2 / (1 + 1.2 * (0.25 + 0.75 * 6/7)) = 0.470004 * 2.2 / 2.071429 = 0.499179
        //   d1: tf 2, dl 4 → 0.470004 * 4.4 / (2 + 1.2 * (0.25 + 0.75 * 12/7)) = 0.470004 * 4.4 / 3.842857 = 0.538146
        let index = corpus();
        let scores = index.scores(&["alpha".to_owned()], |_| true);
        assert_eq!(scores.len(), 2);
        assert!(close(scores[&0], 0.499_179), "{scores:?}");
        assert!(close(scores[&1], 0.538_146), "{scores:?}");
        // "gamma": df = 1, idf = ln(1 + 2.5/1.5) = ln(2.666667) = 0.980829; d1 tf 1, dl 4:
        //   0.980829 * 2.2 / (1 + 1.2 * (0.25 + 0.75 * 12/7)) = 0.980829 * 2.2 / 2.842857 = 0.759036
        let both = index.scores(&["alpha".to_owned(), "gamma".to_owned()], |_| true);
        assert!(close(both[&1], 0.538_146 + 0.759_036), "{both:?}");
        assert_eq!(index.docs(), 3);
    }

    #[test]
    fn ties_break_by_position() {
        let ids = ["a", "b", "c"];
        let index = Bm25Index::from_lines(
            vec![
                line("c", "same text"),
                line("a", "same text"),
                line("b", "same text"),
            ],
            3,
            |id| ids.iter().position(|known| *known == id),
        );
        let top = index.top(&["same".to_owned()], |_| true, 3);
        assert_eq!(
            top.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(index.top(&["same".to_owned()], |_| true, 2).len(), 2);
    }

    #[test]
    fn eligible_filter_respected() {
        let index = corpus();
        let top = index.top(&["beta".to_owned()], |at| at != 0, 5);
        assert_eq!(top.iter().map(|(at, _)| *at).collect::<Vec<_>>(), vec![2]);
        assert!(index.top(&["missing".to_owned()], |_| true, 5).is_empty());
    }

    #[test]
    fn unknown_ids_and_duplicates_are_ignored() {
        let index = Bm25Index::from_lines(
            vec![line("x", "alpha"), line("d0", "alpha"), line("d0", "beta")],
            1,
            |id| (id == "d0").then_some(0),
        );
        assert_eq!(index.docs(), 1);
        assert!(index.scores(&["beta".to_owned()], |_| true).is_empty());
    }

    #[test]
    fn names_only_documents() {
        let index = Bm25Index::from_names(
            vec![
                (
                    0,
                    ["resolve_edges", "graph::resolve_edges", "(index: &Index)"],
                ),
                (1, ["Widget", "ui::Widget", ""]),
            ],
            2,
        );
        let top = index.top(&query_terms("resolve the edges"), |_| true, 5);
        assert_eq!(top[0].0, 0);
        assert_eq!(top.len(), 1);
    }
}
