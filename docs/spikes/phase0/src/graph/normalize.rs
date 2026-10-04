//! Location → container normalisation (T7.1).

use std::collections::BTreeSet;
use std::path::Path;

use async_lsp::lsp_types::{Range, Url};

use crate::common::EntityKey;
use crate::entities::EntityTable;
use crate::uri::uri_to_repo_relative;

/// One in-repo LSP result location with its owning container (T7.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LocResult {
    /// Repo-relative file of the raw location.
    pub file: String,
    /// 0-based line of the raw location.
    pub line0: u32,
    /// 0-based UTF-16 column of the raw location.
    pub col0: u32,
    /// Innermost container entity owning that line.
    pub container: EntityKey,
}

/// Outcome of normalising one location: external results (dependencies, JDK,
/// symlinked `node_modules`) are excluded from every comparison set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Normalized {
    External,
    InRepo(LocResult),
}

/// Normalise one LSP `Location`-style result (references / implementation).
pub fn normalize_location(
    uri: &Url,
    range: &Range,
    root: &Path,
    table: &EntityTable,
) -> Normalized {
    match uri_to_repo_relative(uri, root) {
        None => Normalized::External,
        Some(file) => {
            let line0 = range.start.line;
            Normalized::InRepo(LocResult {
                container: table.container_at(&file, line0 as usize + 1),
                file,
                line0,
                col0: range.start.character,
            })
        }
    }
}

/// Normalise a `CallHierarchyItem` (`from` of an incoming call): its
/// `selection_range` start decides the container (T7.1).
pub fn normalize_call_item(
    uri: &Url,
    selection_range: &Range,
    root: &Path,
    table: &EntityTable,
) -> Normalized {
    normalize_location(uri, selection_range, root, table)
}

/// Container key a knot ground-truth caller maps to (T7.1): the innermost
/// container of its declaration line.
pub fn container_for_caller(table: &EntityTable, caller: &EntityKey) -> EntityKey {
    table.container_at(&caller.file, caller.line)
}

/// Distinct container keys of `locations`, self-references excluded (a
/// container equal to the target itself: recursion, declaration-adjacent
/// references) and sorted for deterministic output.
pub fn container_set(locations: &[LocResult], self_container: &EntityKey) -> Vec<EntityKey> {
    let mut keys: BTreeSet<EntityKey> = BTreeSet::new();
    for loc in locations {
        if &loc.container != self_container {
            keys.insert(loc.container.clone());
        }
    }
    keys.into_iter().collect()
}

/// Count of self-referencing results (dropped from every comparison set).
pub fn self_reference_count(locations: &[LocResult], self_container: &EntityKey) -> usize {
    locations
        .iter()
        .filter(|loc| &loc.container == self_container)
        .count()
}

/// File-level set of a container set (secondary metric, T7.1).
pub fn file_set(containers: &[EntityKey]) -> Vec<String> {
    let mut files: BTreeSet<String> = BTreeSet::new();
    for key in containers {
        files.insert(key.file.clone());
    }
    files.into_iter().collect()
}

/// Union of several container sets (the `lsp` system is incoming ∪ references
/// ∪ implementation).
pub fn union_sets(sets: &[&[EntityKey]]) -> Vec<EntityKey> {
    let mut keys: BTreeSet<EntityKey> = BTreeSet::new();
    for set in sets {
        keys.extend(set.iter().cloned());
    }
    keys.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::models::EntityKind;
    use knot::models::ParsedEntity;
    use std::path::PathBuf;

    fn entity(file: &str, line: usize, end: usize, name: &str) -> ParsedEntity {
        ParsedEntity::new(
            name,
            EntityKind::RustFunction,
            format!("fqn::{name}"),
            None,
            None,
            "rust",
            file,
            line,
            end,
            None,
            "repo",
        )
    }

    fn table() -> EntityTable {
        let parsed = vec![
            entity("src/a.rs", 1, 50, "outer"),
            entity("src/a.rs", 5, 9, "inner"),
            entity("src/a.rs", 20, 24, "leaf"),
        ];
        EntityTable::from_parsed("repo", PathBuf::from("/snap"), parsed.as_slice())
    }

    fn loc(file: &str, line0: u32, container_line: usize, name: &str) -> LocResult {
        LocResult {
            file: file.to_owned(),
            line0,
            col0: 0,
            container: EntityKey {
                file: file.to_owned(),
                line: container_line,
                name: name.to_owned(),
            },
        }
    }

    #[test]
    fn location_maps_to_innermost_container() {
        let table = table();
        let url = Url::from_file_path("/snap/src/a.rs").expect("url");
        let range = Range {
            start: async_lsp::lsp_types::Position {
                line: 21,
                character: 4,
            },
            end: async_lsp::lsp_types::Position {
                line: 21,
                character: 8,
            },
        };
        match normalize_location(&url, &range, Path::new("/snap"), &table) {
            Normalized::InRepo(loc) => {
                assert_eq!(loc.file, "src/a.rs");
                assert_eq!(loc.line0, 21);
                assert_eq!(loc.col0, 4);
                // Line 22 (1-based) is inside `leaf` (20..24), the innermost.
                assert_eq!(loc.container.name, "leaf");
            }
            Normalized::External => panic!("expected in-repo location"),
        }
    }

    #[test]
    fn external_locations_are_excluded() {
        let table = table();
        let url = Url::from_file_path("/elsewhere/lib.rs").expect("url");
        let range = Range::default();
        assert_eq!(
            normalize_location(&url, &range, Path::new("/snap"), &table),
            Normalized::External
        );
    }

    #[test]
    fn self_references_are_excluded_and_counted() {
        let self_key = EntityKey {
            file: "src/a.rs".to_owned(),
            line: 1,
            name: "outer".to_owned(),
        };
        let locations = vec![
            loc("src/a.rs", 0, 1, "outer"),
            loc("src/a.rs", 2, 5, "inner"),
            loc("src/a.rs", 3, 5, "inner"),
        ];
        assert_eq!(self_reference_count(&locations, &self_key), 1);
        let set = container_set(&locations, &self_key);
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].name, "inner");
    }

    #[test]
    fn file_and_union_sets_are_sorted_and_distinct() {
        let a = loc("src/b.rs", 0, 1, "x");
        let b = loc("src/a.rs", 0, 1, "y");
        let set_a = container_set(
            std::slice::from_ref(&a),
            &EntityKey {
                file: "none".to_owned(),
                line: 0,
                name: "none".to_owned(),
            },
        );
        let set_b = container_set(
            std::slice::from_ref(&b),
            &EntityKey {
                file: "none".to_owned(),
                line: 0,
                name: "none".to_owned(),
            },
        );
        assert_eq!(file_set(&set_a), vec!["src/b.rs".to_owned()]);
        let union = union_sets(&[&set_a, &set_b, &set_a]);
        assert_eq!(union.len(), 2);
        assert_eq!(union[0].file, "src/a.rs");
    }
}
