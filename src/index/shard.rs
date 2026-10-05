//! Shard records: one JSON object per entity, tier-1 chunk or file vector
//! (Appendix B.2). Field order is the wire order.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::hash::{sha256_hex, short16};

/// `kind` of a tier-1 line-window record.
pub const KIND_CHUNK: &str = "chunk";
/// `kind` of a tier-1 whole-file record.
pub const KIND_FILE: &str = "file";

/// One shard record. Every key is always emitted (`null` when absent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShardLine {
    /// knot entity UUID (hyphenated), or a v5 [`chunk_id`] for tier-1 records.
    pub id: String,
    /// `EntityKind` wire label, or [`KIND_CHUNK`] / [`KIND_FILE`].
    pub kind: String,
    /// Entity name; the file name for tier-1 records.
    pub name: String,
    /// Entity FQN; the repo-relative path for tier-1 records.
    pub fqn: String,
    /// knot language label (`label_for(path)` for tier-1 records).
    pub lang: String,
    /// 1-based first line.
    pub l0: usize,
    /// 1-based last line (inclusive).
    pub l1: usize,
    /// Reserved: UTF-16 column of the name (written `null` until Phase 3).
    pub col: Option<u32>,
    /// Signature.
    pub sig: Option<String>,
    /// First 16 hex characters of `sha256(embedded text)`: the reuse key.
    pub h: String,
    /// Base64 of the `i8pv` bytes; `null` when the record has no embed text.
    pub q: Option<String>,
    /// Per-vector scale; `null` together with `q`.
    pub s: Option<f64>,
}

/// Components of [`ShardLine::sort_key`].
pub type SortKey<'a> = (usize, u32, &'a str, &'a str, &'a str, &'a str);

impl ShardLine {
    /// `(l0, col or u32::MAX, kind, name, fqn, id)`; `id` makes the order total.
    pub fn sort_key(&self) -> SortKey<'_> {
        (
            self.l0,
            self.col.unwrap_or(u32::MAX),
            &self.kind,
            &self.name,
            &self.fqn,
            &self.id,
        )
    }

    /// Whether this is a tier-1 record (`chunk` or `file`).
    pub fn is_tier1(&self) -> bool {
        self.kind == KIND_CHUNK || self.kind == KIND_FILE
    }

    /// Whether the record carries a vector.
    pub fn has_vector(&self) -> bool {
        self.q.is_some() && self.s.is_some()
    }
}

/// Sort records into the deterministic shard order.
pub fn sort_lines(lines: &mut [ShardLine]) {
    lines.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
}

/// First 16 hex characters of `sha256(text)`: the reuse key `h`.
pub fn text_hash(text: &str) -> String {
    short16(sha256_hex(text.as_bytes()))
}

/// Deterministic id of a tier-1 record:
/// `v5(NAMESPACE_KNOT, "{repo}:{rel}:{kind}:{l0}-{l1}")`, hyphenated.
pub fn chunk_id(repo: &str, rel: &str, kind: &str, l0: usize, l1: usize) -> String {
    let name = format!("{repo}:{rel}:{kind}:{l0}-{l1}");
    Uuid::new_v5(&knot::models::NAMESPACE_KNOT, name.as_bytes())
        .hyphenated()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::models::EntityKind;

    fn line(l0: usize, kind: &str, name: &str, id: &str) -> ShardLine {
        ShardLine {
            id: id.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            fqn: format!("m::{name}"),
            lang: "rust".to_owned(),
            l0,
            l1: l0 + 3,
            col: None,
            sig: None,
            h: text_hash(name),
            q: None,
            s: None,
        }
    }

    #[test]
    fn wire_order_is_pinned() {
        let populated = ShardLine {
            id: "0b5c0000-0000-0000-0000-000000000000".to_owned(),
            kind: "rust_function".to_owned(),
            name: "parse_files".to_owned(),
            fqn: "knot::pipeline::parser::parse_files".to_owned(),
            lang: "rust".to_owned(),
            l0: 211,
            l1: 229,
            col: None,
            sig: Some("(files: &[PathBuf], parse_cfg: &ParseConfig)".to_owned()),
            h: "3f2a9c1d0e4b5a6c".to_owned(),
            q: Some("AQL/".to_owned()),
            s: Some(0.001_234_57),
        };
        assert_eq!(
            serde_json::to_string(&populated).expect("json"),
            concat!(
                r#"{"id":"0b5c0000-0000-0000-0000-000000000000","kind":"rust_function","#,
                r#""name":"parse_files","fqn":"knot::pipeline::parser::parse_files","#,
                r#""lang":"rust","l0":211,"l1":229,"col":null,"#,
                r#""sig":"(files: &[PathBuf], parse_cfg: &ParseConfig)","#,
                r#""h":"3f2a9c1d0e4b5a6c","q":"AQL/","s":0.00123457}"#
            )
        );
    }

    #[test]
    fn nulls_are_emitted_not_skipped() {
        let json = serde_json::to_string(&line(1, "chunk", "a", "id")).expect("json");
        for key in [
            r#""col":null"#,
            r#""sig":null"#,
            r#""q":null"#,
            r#""s":null"#,
        ] {
            assert!(json.contains(key), "{key} missing from {json}");
        }
        let back: ShardLine = serde_json::from_str(&json).expect("round trip");
        assert!(!back.has_vector());
    }

    #[test]
    fn sort_is_total_and_deterministic() {
        let mut lines: Vec<ShardLine> = (0..50)
            .map(|i| {
                line(
                    i % 7,
                    if i % 2 == 0 {
                        "rust_function"
                    } else {
                        "rust_struct"
                    },
                    &format!("n{}", i % 5),
                    &format!("id-{i:02}"),
                )
            })
            .collect();
        // Two lines equal in every sort component except `id`.
        lines.push(line(3, "rust_function", "n3", "id-aa"));
        lines.push(line(3, "rust_function", "n3", "id-zz"));
        let mut expected = lines.clone();
        sort_lines(&mut expected);

        // Steps coprime with the length (52) yield true permutations.
        for step in [3usize, 7, 29, 51] {
            let mut permuted: Vec<ShardLine> = (0..lines.len())
                .map(|i| lines[(i * step) % lines.len()].clone())
                .collect();
            assert_ne!(permuted, lines, "step {step} must reorder the input");
            sort_lines(&mut permuted);
            assert_eq!(permuted, expected, "permutation step {step}");
        }
        let position = |id: &str| expected.iter().position(|l| l.id == id).expect(id);
        assert!(position("id-aa") < position("id-zz"));
    }

    #[test]
    fn sort_puts_known_columns_before_unknown_ones() {
        let mut with_col = line(5, "rust_function", "a", "id-1");
        with_col.col = Some(4);
        let without_col = line(5, "rust_function", "a", "id-0");
        let mut lines = vec![without_col.clone(), with_col.clone()];
        sort_lines(&mut lines);
        assert_eq!(lines, vec![with_col, without_col]);
    }

    #[test]
    fn text_hash_known_vector() {
        assert_eq!(text_hash("abc"), "ba7816bf8f01cfea");
        assert_eq!(text_hash("abc").len(), 16);
    }

    #[test]
    fn chunk_id_is_stable_v5() {
        let expected = Uuid::new_v5(&knot::models::NAMESPACE_KNOT, b"demo:src/a.sh:chunk:1-60")
            .hyphenated()
            .to_string();
        assert_eq!(chunk_id("demo", "src/a.sh", KIND_CHUNK, 1, 60), expected);
        assert_ne!(
            chunk_id("demo", "src/a.sh", KIND_CHUNK, 1, 60),
            chunk_id("demo", "src/a.sh", KIND_FILE, 1, 60)
        );
    }

    #[test]
    fn tier1_kinds_are_not_knot_kinds() {
        let wire: Vec<&str> = EntityKind::all_wire_kinds().collect();
        assert!(!wire.is_empty());
        assert!(!wire.contains(&KIND_CHUNK));
        assert!(!wire.contains(&KIND_FILE));
    }

    #[test]
    fn tier1_and_vector_predicates() {
        let mut record = line(1, KIND_CHUNK, "a", "id");
        assert!(record.is_tier1());
        assert!(!record.has_vector());
        record.q = Some("AA==".to_owned());
        assert!(!record.has_vector(), "scale missing");
        record.s = Some(0.5);
        assert!(record.has_vector());
        assert!(!line(1, "rust_function", "a", "id").is_tier1());
    }
}
