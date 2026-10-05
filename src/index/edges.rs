//! Edge records: one resolved reference per line (Appendix B.3), stored in
//! the shard of the **caller's** file (P2-D7).

use serde::{Deserialize, Serialize};

/// One reference edge. The derived `Ord` (`src`, `dst`, `rel`) is the shard
/// sort order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeLine {
    /// Caller entity id.
    pub src: String,
    /// Target entity id.
    pub dst: String,
    /// Relationship label (`CALLS`, `EXTENDS`, …; never `CONTAINS`).
    pub rel: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(src: &str, dst: &str, rel: &str) -> EdgeLine {
        EdgeLine {
            src: src.to_owned(),
            dst: dst.to_owned(),
            rel: rel.to_owned(),
        }
    }

    #[test]
    fn edge_wire_order_is_pinned() {
        assert_eq!(
            serde_json::to_string(&edge("a-1", "b-2", "CALLS")).expect("json"),
            r#"{"src":"a-1","dst":"b-2","rel":"CALLS"}"#
        );
    }

    #[test]
    fn edges_sort_by_src_dst_rel() {
        let mut edges = vec![
            edge("b", "a", "CALLS"),
            edge("a", "z", "CALLS"),
            edge("a", "b", "REFERENCES"),
            edge("a", "b", "CALLS"),
        ];
        edges.sort();
        assert_eq!(
            edges,
            vec![
                edge("a", "b", "CALLS"),
                edge("a", "b", "REFERENCES"),
                edge("a", "z", "CALLS"),
                edge("b", "a", "CALLS"),
            ]
        );
    }
}
