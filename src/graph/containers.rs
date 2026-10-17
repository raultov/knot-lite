//! Containers: the entities that can own a source line.
//!
//! Callers are reported at **container** granularity (the function, method or
//! type that holds the reference site), which is the granularity Phase 0's
//! gold rule measured. Ported from the spike's `entities.rs`.

use std::collections::HashMap;

use serde::Serialize;

use crate::index::shard::ShardLine;

/// File-level pseudo container name: a line outside every container.
pub const FILE_LEVEL_NAME: &str = "<file>";

/// Kind-label markers (substring match on the wire label) of constructs that
/// can own other entities. `union` is a deliberate addition so `rust_union` is
/// classified on purpose (Phase 0 T3.2).
const MARKERS: &[&str] = &[
    "function",
    "method",
    "constructor",
    "class",
    "interface",
    "struct",
    "union",
    "enum",
    "trait",
    "impl",
    "module",
    "record",
    "object",
    "namespace",
    "macro",
    "closure",
    "annotation",
];

/// Whether the kind label denotes a construct that can own other entities.
/// Entities named `<module>` always count.
pub fn is_container(kind: &str, name: &str) -> bool {
    name == "<module>" || MARKERS.iter().any(|marker| kind.contains(marker))
}

/// Identity of a container: where it starts and what it is called.
/// `line == 0` with [`FILE_LEVEL_NAME`] is the file level.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ContainerKey {
    /// Repo-relative file.
    pub file: String,
    /// 1-based start line of the container (0 at file level).
    pub line: usize,
    /// Container name.
    pub name: String,
}

impl ContainerKey {
    /// The file-level pseudo container of `file`.
    pub fn file_level(file: &str) -> Self {
        Self {
            file: file.to_owned(),
            line: 0,
            name: FILE_LEVEL_NAME.to_owned(),
        }
    }
}

#[derive(Debug)]
struct ContainerSpan {
    l0: usize,
    l1: usize,
    name: String,
    /// Index of the owning record in the caller's record list.
    idx: usize,
}

/// Per-file container spans, for "which container holds line N" lookups.
#[derive(Debug, Default)]
pub struct ContainerTable {
    by_file: HashMap<String, Vec<ContainerSpan>>,
}

impl ContainerTable {
    /// Build the table from `(record index, file, record)` triples. Only
    /// entity records whose kind [`is_container`] contribute; tier-1 records
    /// are ignored.
    pub fn from_records<'a>(
        records: impl Iterator<Item = (usize, &'a str, &'a ShardLine)>,
    ) -> Self {
        let mut by_file: HashMap<String, Vec<ContainerSpan>> = HashMap::new();
        for (idx, file, line) in records {
            if line.is_tier1() || !is_container(&line.kind, &line.name) {
                continue;
            }
            by_file
                .entry(file.to_owned())
                .or_default()
                .push(ContainerSpan {
                    l0: line.l0,
                    l1: line.l1,
                    name: line.name.clone(),
                    idx,
                });
        }
        Self { by_file }
    }

    /// The innermost container of `file` whose `[l0, l1]` contains `line`
    /// (ties go to the later start), with its record index; the file level
    /// (and `None`) when no container holds the line.
    pub fn container_at(&self, file: &str, line: usize) -> (ContainerKey, Option<usize>) {
        self.by_file
            .get(file)
            .into_iter()
            .flatten()
            .filter(|span| span.l0 <= line && line <= span.l1)
            .min_by_key(|span| (span.l1 - span.l0, std::cmp::Reverse(span.l0)))
            .map_or_else(
                || (ContainerKey::file_level(file), None),
                |span| {
                    (
                        ContainerKey {
                            file: file.to_owned(),
                            line: span.l0,
                            name: span.name.clone(),
                        },
                        Some(span.idx),
                    )
                },
            )
    }
}

/// Metadata of a container found in a re-parsed file that has no indexed counterpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshContainer {
    pub kind: String,
    pub fqn: String,
    pub signature: Option<String>,
}

/// An overlay for one re-parsed file (P6-D7).
pub struct FreshOverlay<'a> {
    pub table: &'a ContainerTable,
    pub records: &'a [ShardLine],
    pub mapping: Vec<Option<usize>>,
}

/// Container lookup that uses fresh overlays for changed files and the index
/// table for unchanged files (P6-D7).
pub struct ContainerLookup<'a> {
    index: &'a ContainerTable,
    fresh: HashMap<&'a str, FreshOverlay<'a>>,
}

impl<'a> ContainerLookup<'a> {
    /// A lookup that only consults the index table.
    pub fn index_only(index: &'a ContainerTable) -> Self {
        Self {
            index,
            fresh: HashMap::new(),
        }
    }

    /// A lookup that consults `fresh` overlays for changed files and `index` for the rest.
    pub fn new(index: &'a ContainerTable, fresh: HashMap<&'a str, FreshOverlay<'a>>) -> Self {
        Self { index, fresh }
    }

    /// The container holding `line` of `file`.
    ///
    /// Returns `(key, indexed_record_idx, fresh_unmatched_info)`.
    pub fn container_at(
        &self,
        file: &str,
        line: usize,
    ) -> (ContainerKey, Option<usize>, Option<FreshContainer>) {
        if let Some(overlay) = self.fresh.get(file) {
            let (key, fresh_idx) = overlay.table.container_at(file, line);
            if let Some(idx) = fresh_idx {
                if let Some(indexed_idx) = overlay.mapping.get(idx).copied().flatten() {
                    return (key, Some(indexed_idx), None);
                } else if let Some(fresh_line) = overlay.records.get(idx) {
                    return (
                        key,
                        None,
                        Some(FreshContainer {
                            kind: fresh_line.kind.clone(),
                            fqn: fresh_line.fqn.clone(),
                            signature: fresh_line.sig.clone(),
                        }),
                    );
                }
            }
            return (key, None, None);
        }
        let (key, record) = self.index.container_at(file, line);
        (key, record, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::models::EntityKind;

    fn record(kind: &str, name: &str, l0: usize, l1: usize) -> ShardLine {
        ShardLine {
            id: format!("{name}-{l0}"),
            kind: kind.to_owned(),
            name: name.to_owned(),
            fqn: name.to_owned(),
            lang: "rust".to_owned(),
            l0,
            l1,
            col: None,
            sig: None,
            h: String::new(),
            q: None,
            s: None,
            tc: false,
        }
    }

    fn table(file: &str, lines: &[ShardLine]) -> ContainerTable {
        ContainerTable::from_records(lines.iter().enumerate().map(|(i, line)| (i, file, line)))
    }

    #[test]
    fn container_classification_is_deliberate() {
        let expected: &[(&str, bool)] = &[
            // Rust
            ("rust_struct", true),
            ("rust_enum", true),
            ("rust_union", true),
            ("rust_trait", true),
            ("rust_impl", true),
            ("rust_function", true),
            ("rust_method", true),
            ("rust_macro_def", true),
            ("rust_macro_invoke", true),
            ("rust_module", true),
            ("rust_type_alias", false),
            ("rust_constant", false),
            ("rust_static", false),
            // TypeScript / JavaScript / Java share the generic labels
            ("class", true),
            ("interface", true),
            ("method", true),
            ("function", true),
            ("enum", true),
            ("constant", false),
            // Outside the three spiked languages
            ("csharp_constructor", true),
            ("kotlin_companion_object", true),
            ("field", false),
            ("variable", false),
            ("config_property", false),
        ];
        for (label, is_container_kind) in expected {
            assert_eq!(
                is_container(label, "some_name"),
                *is_container_kind,
                "classification of {label} changed"
            );
        }
        assert!(
            is_container("constant", "<module>"),
            "`<module>` always counts"
        );
    }

    #[test]
    fn every_knot_wire_kind_is_classified_without_panic() {
        let kinds: Vec<&str> = EntityKind::all_wire_kinds().collect();
        assert!(kinds.len() > 20, "knot exposes its kind labels");
        let containers = kinds.iter().filter(|kind| is_container(kind, "n")).count();
        assert!(containers > 0 && containers < kinds.len());
    }

    #[test]
    fn nested_containers_pick_innermost() {
        // module ⊃ impl ⊃ method
        let lines = [
            record("rust_module", "m", 1, 100),
            record("rust_impl", "Widget", 10, 60),
            record("rust_method", "run", 20, 30),
            record("rust_constant", "K", 22, 22),
        ];
        let table = table("src/a.rs", &lines);
        let (key, idx) = table.container_at("src/a.rs", 25);
        assert_eq!((key.name.as_str(), key.line, idx), ("run", 20, Some(2)));
        let (key, idx) = table.container_at("src/a.rs", 22);
        assert_eq!(key.name, "run", "a constant is not a container");
        assert_eq!(idx, Some(2));
        let (key, _) = table.container_at("src/a.rs", 50);
        assert_eq!(key.name, "Widget");
        let (key, _) = table.container_at("src/a.rs", 5);
        assert_eq!(key.name, "m");
    }

    #[test]
    fn line_outside_containers_is_file_level() {
        let lines = [record("rust_function", "f", 10, 20)];
        let table = table("src/a.rs", &lines);
        for (file, line) in [("src/a.rs", 5), ("src/a.rs", 21), ("src/other.rs", 12)] {
            let (key, idx) = table.container_at(file, line);
            assert_eq!(key, ContainerKey::file_level(file), "{file}:{line}");
            assert_eq!(idx, None);
        }
        assert_eq!(ContainerKey::file_level("x").line, 0);
        assert_eq!(ContainerKey::file_level("x").name, "<file>");
    }

    #[test]
    fn tie_breaks_on_later_start() {
        // Two containers with the same length both hold line 15.
        let lines = [
            record("rust_function", "earlier", 10, 19),
            record("rust_function", "later", 11, 20),
        ];
        let table = table("src/a.rs", &lines);
        let (key, idx) = table.container_at("src/a.rs", 15);
        assert_eq!((key.name.as_str(), idx), ("later", Some(1)));
    }

    #[test]
    fn tier1_records_never_contribute() {
        let lines = [
            record("chunk", "run.sh", 1, 60),
            record("file", "run.sh", 1, 75),
            record("rust_function", "f", 1, 5),
        ];
        let table = table("run.sh", &lines);
        assert_eq!(
            table.container_at("run.sh", 30).0,
            ContainerKey::file_level("run.sh")
        );
        assert_eq!(table.container_at("run.sh", 3).0.name, "f");
    }

    #[test]
    fn container_key_orders_by_file_line_name() {
        let mut keys = [
            ContainerKey {
                file: "b".into(),
                line: 1,
                name: "x".into(),
            },
            ContainerKey {
                file: "a".into(),
                line: 9,
                name: "x".into(),
            },
            ContainerKey {
                file: "a".into(),
                line: 2,
                name: "z".into(),
            },
            ContainerKey {
                file: "a".into(),
                line: 2,
                name: "y".into(),
            },
        ];
        keys.sort();
        let order: Vec<(&str, usize, &str)> = keys
            .iter()
            .map(|k| (k.file.as_str(), k.line, k.name.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![("a", 2, "y"), ("a", 2, "z"), ("a", 9, "x"), ("b", 1, "x")]
        );
    }
}
