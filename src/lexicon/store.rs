//! Lexicon records: one JSON line per record with embed text, next to its
//! shard (`.knot-lite/lexicon/<rel>.jsonl`, §10.4). Field order is the wire order.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::tokenize::tokenize;

/// The term bag of one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LexiconLine {
    /// Record id (the shard line's `id`).
    pub id: String,
    /// Terms emitted for the record (its length for BM25, after the cap).
    pub n: u32,
    /// `(term, frequency)` pairs sorted by term (bytewise). A frequency cannot
    /// exceed the tokenizer's cap, which is below `u16::MAX`.
    pub t: Vec<(String, u16)>,
}

impl LexiconLine {
    /// The term bag of `text` for record `id`.
    pub fn from_text(id: &str, text: &str) -> Self {
        let terms = tokenize(text);
        let n = u32::try_from(terms.len()).unwrap_or(u32::MAX);
        let mut bag: BTreeMap<String, u16> = BTreeMap::new();
        for term in terms {
            let frequency = bag.entry(term).or_default();
            *frequency = frequency.saturating_add(1);
        }
        Self {
            id: id.to_owned(),
            n,
            t: bag.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexicon_lines_round_trip() {
        let line = LexiconLine::from_text("r1", "find callers: FindCallers finds callers");
        assert_eq!(line.n, 7);
        assert_eq!(
            line.t,
            vec![
                ("callers".to_owned(), 3),
                ("find".to_owned(), 2),
                ("findcallers".to_owned(), 1),
                ("finds".to_owned(), 1),
            ]
        );
        let json = serde_json::to_string(&line).expect("json");
        assert_eq!(
            json,
            r#"{"id":"r1","n":7,"t":[["callers",3],["find",2],["findcallers",1],["finds",1]]}"#
        );
        let back: LexiconLine = serde_json::from_str(&json).expect("back");
        assert_eq!(back, line);
    }

    #[test]
    fn term_count_is_capped() {
        let line = LexiconLine::from_text("r", &"x1 ".repeat(70_000));
        assert_eq!(line.n, 2048);
        assert_eq!(line.t, vec![("x1".to_owned(), 2048)]);
    }
}
