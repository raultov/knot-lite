//! From raw language-server locations to callers grouped by container.
//!
//! Two steps, kept apart on purpose:
//!
//! 1. [`normalize`] runs inside the pool, where the server's answer is born: it
//!    maps a URI to a repo-relative path, drops locations outside the
//!    repository (dependencies, the JDK, symlinked `node_modules`) and, for
//!    Java, the ones that only sit in a comment (P3-D17).
//! 2. [`group_locations`] runs in the query thread, against the index the
//!    answer is built from: it maps every location to its innermost container
//!    (the Phase 0 ownership rule), drops the target's own container, and
//!    groups the containers by relationship (§6.5).
//!
//! The pool therefore never sees the index, and both steps are testable
//! without a server. Ported from the spike's `graph/normalize.rs`.

use std::collections::{BTreeMap, BTreeSet};

use async_lsp::lsp_types::{Range, Url};

use super::ops::{Op, TargetShape, implementation_relation};
use super::uri::{CanonicalRoot, uri_to_repo_relative_with};
use crate::capability::languages::LspLang;
use crate::graph::containers::{ContainerKey, ContainerTable};
use crate::graph::heuristic::EDGE_REL_LABELS;

/// An in-repository location a server returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLocation {
    /// The operation that produced it.
    pub op: Op,
    /// Repo-relative path, `/`-separated.
    pub rel: String,
    /// 0-based line.
    pub line0: u32,
    /// 0-based UTF-16 column.
    pub col0: u32,
    /// Reference sites this location stands for: the number of call ranges of an
    /// incoming call, 1 for every other operation.
    pub sites: u32,
}

/// One location as the server returned it.
#[derive(Debug, Clone, Copy)]
pub struct RawHit<'a> {
    /// The operation that produced it.
    pub op: Op,
    /// Document of the location.
    pub uri: &'a Url,
    /// Range of the location (for an incoming call: the caller's selection range).
    pub range: &'a Range,
    /// Reference sites it stands for (see [`RawLocation::sites`]).
    pub sites: u32,
}

/// What [`normalize`] needs to judge a location.
#[derive(Debug, Clone, Copy)]
pub struct LocationFilter<'a> {
    /// The repository root, canonicalised once.
    pub root: &'a CanonicalRoot,
    /// The server's language (only Java filters comment lines).
    pub lang: LspLang,
}

/// Verdict on one location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Normalized {
    /// Outside the repository: excluded from every comparison.
    External,
    /// A Java reference that only appears in a comment (`{@link}`, `@see`, …).
    Comment,
    /// A location inside the repository.
    InRepo(RawLocation),
}

/// Judge one location. `source_line(rel, line0)` returns the current text of a
/// source line; it is consulted only for Java references and implementations,
/// and `None` (unreadable file) keeps the location.
pub fn normalize(
    hit: &RawHit<'_>,
    filter: &LocationFilter<'_>,
    source_line: &dyn Fn(&str, u32) -> Option<String>,
) -> Normalized {
    let Some(rel) = uri_to_repo_relative_with(hit.uri, filter.root) else {
        return Normalized::External;
    };
    let line0 = hit.range.start.line;
    // An incoming call's location is the caller's declaration, never prose.
    let may_be_prose = filter.lang == LspLang::Java && hit.op != Op::Incoming;
    if may_be_prose && source_line(&rel, line0).is_some_and(|line| is_comment_line(&line)) {
        return Normalized::Comment;
    }
    Normalized::InRepo(RawLocation {
        op: hit.op,
        rel,
        line0,
        col0: hit.range.start.character,
        sites: hit.sites,
    })
}

/// Whether a source line is entirely a comment: it starts (after indentation)
/// with `*`, `/*`, `/**` or `//` (Phase 0 §6: the only language-server false
/// positives were jdtls javadoc `{@link}` / `@see` mentions).
pub fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    ["*", "/*", "//"]
        .iter()
        .any(|marker| trimmed.starts_with(marker))
}

/// The target whose callers are being grouped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    /// What kind of target it is (decides the label of its implementations).
    pub shape: TargetShape,
    /// The container that holds the target's own declaration: references from
    /// inside it are recursion, not callers.
    pub own: ContainerKey,
}

/// A container that references the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerHit {
    /// The container.
    pub key: ContainerKey,
    /// Index of the container's record in the index; `None` at file level.
    pub record: Option<usize>,
    /// Reference sites merged into the container.
    pub sites: usize,
}

/// Callers of one target, grouped by relationship.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LspGroups {
    /// `(relationship label, callers)` in the order of [`EDGE_REL_LABELS`];
    /// callers are sorted by container.
    pub groups: Vec<(String, Vec<CallerHit>)>,
    /// References that come from the target's own container.
    pub self_references: usize,
    /// Distinct containers across every group.
    pub containers: BTreeSet<ContainerKey>,
}

/// A container's record and reference-site positions within one operation.
#[derive(Default)]
struct Accumulated {
    record: Option<usize>,
    /// `incoming`: the sum of the call ranges; the others: distinct positions.
    sites: usize,
    positions: BTreeSet<(String, u32, u32)>,
}

/// Group the locations of one target by relationship and container (§6.5).
///
/// - `incoming` locations are `CALLS`;
/// - `implementation` locations take the label of the target's shape
///   (`OVERRIDES`, `IMPLEMENTS` or `EXTENDS`);
/// - `references` locations are `REFERENCES`, **minus** the containers already
///   listed under `CALLS` or under the implementation label (a call site is
///   also a reference; it is listed once).
///
/// The target's own container is dropped and counted in
/// [`LspGroups::self_references`] (distinct reference positions; the call
/// hierarchy's own-declaration location only counts when no other operation
/// saw the recursion).
pub fn group_locations(
    locations: &[RawLocation],
    target: &TargetInfo,
    containers: &ContainerTable,
) -> LspGroups {
    let mut per_op: BTreeMap<Op, BTreeMap<ContainerKey, Accumulated>> = BTreeMap::new();
    let mut own_positions: BTreeSet<(String, u32, u32)> = BTreeSet::new();
    let mut own_incoming = 0;
    for location in locations {
        let line = usize::try_from(location.line0).map_or(usize::MAX, |n| n.saturating_add(1));
        let (key, record) = containers.container_at(&location.rel, line);
        if key == target.own {
            if location.op == Op::Incoming {
                own_incoming += 1;
            } else {
                own_positions.insert(position_of(location));
            }
            continue;
        }
        let entry = per_op
            .entry(location.op)
            .or_default()
            .entry(key)
            .or_default();
        entry.record = record;
        match location.op {
            Op::Incoming => entry.sites += usize::try_from(location.sites.max(1)).unwrap_or(1),
            Op::References | Op::Implementation => {
                entry.positions.insert(position_of(location));
                entry.sites = entry.positions.len();
            }
        }
    }

    let implementation_label = implementation_relation(target.shape).unwrap_or("REFERENCES");
    let mut by_relation: BTreeMap<(usize, &str), BTreeMap<ContainerKey, CallerHit>> =
        BTreeMap::new();
    let listed_before_references: BTreeSet<ContainerKey> = [Op::Incoming, Op::Implementation]
        .iter()
        .filter_map(|op| per_op.get(op))
        .flat_map(BTreeMap::keys)
        .cloned()
        .collect();
    for (op, callers) in per_op {
        let label = match op {
            Op::Incoming => "CALLS",
            Op::Implementation => implementation_label,
            Op::References => "REFERENCES",
        };
        for (key, accumulated) in callers {
            if op == Op::References && listed_before_references.contains(&key) {
                continue;
            }
            let hit = by_relation
                .entry((label_order(label), label))
                .or_default()
                .entry(key.clone())
                .or_insert_with(|| CallerHit {
                    key,
                    record: accumulated.record,
                    sites: 0,
                });
            hit.sites += accumulated.sites;
        }
    }

    let groups: Vec<(String, Vec<CallerHit>)> = by_relation
        .into_iter()
        .map(|((_, label), callers)| (label.to_owned(), callers.into_values().collect()))
        .collect();
    let containers = groups
        .iter()
        .flat_map(|(_, callers)| callers.iter().map(|hit| hit.key.clone()))
        .collect();
    LspGroups {
        groups,
        self_references: if own_positions.is_empty() {
            own_incoming
        } else {
            own_positions.len()
        },
        containers,
    }
}

fn position_of(location: &RawLocation) -> (String, u32, u32) {
    (location.rel.clone(), location.line0, location.col0)
}

/// Position of a relationship label in [`EDGE_REL_LABELS`] (unknown labels last).
fn label_order(label: &str) -> usize {
    EDGE_REL_LABELS
        .iter()
        .position(|candidate| *candidate == label)
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use async_lsp::lsp_types::Position;

    use super::*;
    use crate::index::shard::ShardLine;
    use crate::lsp::uri::path_to_uri;

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
        }
    }

    /// `src/a.rs`: `outer` 1–50 containing `inner` 5–9 and `leaf` 20–24;
    /// `src/b.rs`: `other` 1–10.
    fn table() -> ContainerTable {
        let records = [
            ("src/a.rs", record("rust_function", "outer", 1, 50)),
            ("src/a.rs", record("rust_function", "inner", 5, 9)),
            ("src/a.rs", record("rust_function", "leaf", 20, 24)),
            ("src/b.rs", record("rust_function", "other", 1, 10)),
        ];
        ContainerTable::from_records(
            records
                .iter()
                .enumerate()
                .map(|(index, (file, line))| (index, *file, line)),
        )
    }

    fn key(file: &str, line: usize, name: &str) -> ContainerKey {
        ContainerKey {
            file: file.to_owned(),
            line,
            name: name.to_owned(),
        }
    }

    /// A target that lives in a file the table does not know about.
    fn target(shape: TargetShape) -> TargetInfo {
        TargetInfo {
            shape,
            own: key("src/target.rs", 1, "target"),
        }
    }

    fn raw(op: Op, rel: &str, line0: u32, col0: u32, sites: u32) -> RawLocation {
        RawLocation {
            op,
            rel: rel.to_owned(),
            line0,
            col0,
            sites,
        }
    }

    fn labels(groups: &LspGroups) -> Vec<&str> {
        groups
            .groups
            .iter()
            .map(|(label, _)| label.as_str())
            .collect()
    }

    fn names(groups: &LspGroups, label: &str) -> Vec<(String, usize)> {
        groups
            .groups
            .iter()
            .find(|(candidate, _)| candidate == label)
            .map(|(_, hits)| {
                hits.iter()
                    .map(|hit| (hit.key.name.clone(), hit.sites))
                    .collect()
            })
            .unwrap_or_default()
    }

    // ---- normalize ---------------------------------------------------------

    /// A temp repository and a location in `rel` on `line0`.
    struct Probe {
        dir: tempfile::TempDir,
        root: CanonicalRoot,
    }

    impl Probe {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = CanonicalRoot::new(dir.path());
            Self { dir, root }
        }

        fn judge(&self, lang: LspLang, op: Op, file: (&str, &str), line0: u32) -> Normalized {
            let (rel, source) = file;
            let uri = path_to_uri(&self.dir.path().join(rel)).expect("uri");
            let range = Range::new(Position::new(line0, 4), Position::new(line0, 9));
            let lines: HashMap<(String, u32), String> = source
                .lines()
                .enumerate()
                .map(|(i, text)| {
                    (
                        (rel.to_owned(), u32::try_from(i).expect("small")),
                        text.to_owned(),
                    )
                })
                .collect();
            normalize(
                &RawHit {
                    op,
                    uri: &uri,
                    range: &range,
                    sites: 1,
                },
                &LocationFilter {
                    root: &self.root,
                    lang,
                },
                &|file, line| lines.get(&(file.to_owned(), line)).cloned(),
            )
        }
    }

    #[test]
    fn in_repo_location_has_relative_path_and_position() {
        let probe = Probe::new();
        let verdict = probe.judge(
            LspLang::Rust,
            Op::References,
            ("src/a.rs", "fn a() {}\n"),
            0,
        );
        assert_eq!(
            verdict,
            Normalized::InRepo(raw(Op::References, "src/a.rs", 0, 4, 1))
        );
    }

    #[test]
    fn external_uri_is_external() {
        let probe = Probe::new();
        let outside = tempfile::tempdir().expect("tempdir");
        let uri = path_to_uri(&outside.path().join("lib.rs")).expect("uri");
        let range = Range::default();
        let verdict = normalize(
            &RawHit {
                op: Op::References,
                uri: &uri,
                range: &range,
                sites: 1,
            },
            &LocationFilter {
                root: &probe.root,
                lang: LspLang::Rust,
            },
            &|_, _| None,
        );
        assert_eq!(verdict, Normalized::External);
    }

    #[test]
    fn java_javadoc_line_is_comment_filtered() {
        let probe = Probe::new();
        let source = "/**\n * See {@link Foo#bar()}.\n */\nclass A {\n    // calls bar()\n    /* bar */\n    int x = foo.bar();\n}\n";
        for line0 in [0, 1, 2, 4, 5] {
            assert_eq!(
                probe.judge(LspLang::Java, Op::References, ("A.java", source), line0),
                Normalized::Comment,
                "line {line0}"
            );
        }
        assert!(matches!(
            probe.judge(LspLang::Java, Op::References, ("A.java", source), 6),
            Normalized::InRepo(_)
        ));
        assert_eq!(
            probe.judge(LspLang::Java, Op::Implementation, ("A.java", source), 1),
            Normalized::Comment,
            "implementations are filtered too"
        );
    }

    #[test]
    fn java_incoming_call_locations_are_never_filtered() {
        let probe = Probe::new();
        let verdict = probe.judge(LspLang::Java, Op::Incoming, ("A.java", " * odd\n"), 0);
        assert!(matches!(verdict, Normalized::InRepo(_)), "{verdict:?}");
    }

    #[test]
    fn java_location_in_an_unreadable_file_is_kept() {
        let probe = Probe::new();
        let uri = path_to_uri(&probe.dir.path().join("Gone.java")).expect("uri");
        let range = Range::default();
        let verdict = normalize(
            &RawHit {
                op: Op::References,
                uri: &uri,
                range: &range,
                sites: 1,
            },
            &LocationFilter {
                root: &probe.root,
                lang: LspLang::Java,
            },
            &|_, _| None,
        );
        assert!(matches!(verdict, Normalized::InRepo(_)));
    }

    #[test]
    fn rust_doc_line_is_kept() {
        let probe = Probe::new();
        let source = "/// See [`foo`] and [`bar`].\nfn foo() {}\n";
        assert!(matches!(
            probe.judge(LspLang::Rust, Op::References, ("src/a.rs", source), 0),
            Normalized::InRepo(_)
        ));
    }

    #[test]
    fn ts_jsdoc_line_is_kept() {
        let probe = Probe::new();
        let source = "/**\n * @see foo\n */\nexport function foo() {}\n";
        assert!(matches!(
            probe.judge(LspLang::Ts, Op::References, ("src/a.ts", source), 1),
            Normalized::InRepo(_)
        ));
    }

    #[test]
    fn comment_line_detection() {
        for comment in [
            "* x",
            "   * x",
            "/* x",
            "\t/** x */",
            "// x",
            "  /// x",
            "*/",
        ] {
            assert!(is_comment_line(comment), "{comment:?}");
        }
        for code in ["int x = 1;", "  foo(a * b);", "", "   ", "a // trailing"] {
            assert!(!is_comment_line(code), "{code:?}");
        }
    }

    // ---- group_locations ---------------------------------------------------

    #[test]
    fn location_maps_to_innermost_container() {
        let groups = group_locations(
            &[raw(Op::References, "src/a.rs", 21, 4, 1)],
            &target(TargetShape::Callable),
            &table(),
        );
        // Line 22 (1-based) is inside `leaf` (20–24), the innermost; `outer` also holds it.
        assert_eq!(names(&groups, "REFERENCES"), vec![("leaf".to_owned(), 1)]);
        assert_eq!(groups.groups[0].1[0].key, key("src/a.rs", 20, "leaf"));
        assert_eq!(groups.groups[0].1[0].record, Some(2));
    }

    #[test]
    fn file_level_location_maps_to_file_container() {
        let groups = group_locations(
            &[raw(Op::References, "src/a.rs", 98, 0, 1)],
            &target(TargetShape::Callable),
            &table(),
        );
        let hit = &groups.groups[0].1[0];
        assert_eq!(hit.key, ContainerKey::file_level("src/a.rs"));
        assert_eq!(hit.record, None);
        let unknown_file = group_locations(
            &[raw(Op::References, "docs/x.md", 0, 0, 1)],
            &target(TargetShape::Callable),
            &table(),
        );
        assert_eq!(
            unknown_file.groups[0].1[0].key,
            ContainerKey::file_level("docs/x.md")
        );
    }

    #[test]
    fn self_container_is_counted_not_listed() {
        let own = TargetInfo {
            shape: TargetShape::Callable,
            own: key("src/a.rs", 5, "inner"),
        };
        let locations = [
            raw(Op::References, "src/a.rs", 6, 4, 1),
            raw(Op::References, "src/a.rs", 7, 4, 1),
            raw(Op::References, "src/a.rs", 7, 4, 1),
            raw(Op::References, "src/b.rs", 2, 0, 1),
        ];
        let groups = group_locations(&locations, &own, &table());
        assert_eq!(
            groups.self_references, 2,
            "distinct positions inside `inner`"
        );
        assert_eq!(names(&groups, "REFERENCES"), vec![("other".to_owned(), 1)]);
        assert!(!groups.containers.contains(&own.own));
    }

    #[test]
    fn recursion_seen_by_both_operations_counts_once() {
        let own = TargetInfo {
            shape: TargetShape::Callable,
            own: key("src/a.rs", 5, "inner"),
        };
        // The call hierarchy reports the declaration, references the call site.
        let both = [
            raw(Op::Incoming, "src/a.rs", 4, 3, 1),
            raw(Op::References, "src/a.rs", 7, 8, 1),
        ];
        assert_eq!(group_locations(&both, &own, &table()).self_references, 1);
        let incoming_only = [raw(Op::Incoming, "src/a.rs", 4, 3, 1)];
        let groups = group_locations(&incoming_only, &own, &table());
        assert_eq!(
            groups.self_references, 1,
            "the call hierarchy alone still counts"
        );
        assert!(groups.groups.is_empty());
    }

    #[test]
    fn call_site_listed_once_under_calls() {
        let locations = [
            // `leaf` calls the target (2 call ranges) and the same site is also a reference.
            raw(Op::Incoming, "src/a.rs", 19, 3, 2),
            raw(Op::References, "src/a.rs", 21, 4, 1),
            raw(Op::References, "src/a.rs", 22, 4, 1),
            // `other` only mentions it.
            raw(Op::References, "src/b.rs", 3, 0, 1),
        ];
        let groups = group_locations(&locations, &target(TargetShape::Callable), &table());
        assert_eq!(labels(&groups), vec!["CALLS", "REFERENCES"]);
        assert_eq!(names(&groups, "CALLS"), vec![("leaf".to_owned(), 2)]);
        assert_eq!(names(&groups, "REFERENCES"), vec![("other".to_owned(), 1)]);
        assert_eq!(groups.containers.len(), 2, "caller_count counts leaf once");
    }

    #[test]
    fn implementation_group_label_follows_shape() {
        let locations = [
            raw(Op::Implementation, "src/a.rs", 21, 4, 1),
            // Also a reference: listed under the implementation label only.
            raw(Op::References, "src/a.rs", 22, 4, 1),
            raw(Op::References, "src/b.rs", 3, 0, 1),
        ];
        for (shape, label) in [
            (TargetShape::Method, "OVERRIDES"),
            (TargetShape::InterfaceLike, "IMPLEMENTS"),
            (TargetShape::ClassLike, "EXTENDS"),
        ] {
            let groups = group_locations(&locations, &target(shape), &table());
            assert_eq!(
                names(&groups, label),
                vec![("leaf".to_owned(), 1)],
                "{shape:?}"
            );
            assert_eq!(
                names(&groups, "REFERENCES"),
                vec![("other".to_owned(), 1)],
                "{shape:?}: leaf is not repeated under REFERENCES"
            );
        }
    }

    #[test]
    fn implementation_without_a_relation_label_degrades_to_references() {
        let groups = group_locations(
            &[raw(Op::Implementation, "src/a.rs", 21, 4, 1)],
            &target(TargetShape::TypeLike),
            &table(),
        );
        assert_eq!(labels(&groups), vec!["REFERENCES"]);
    }

    #[test]
    fn sites_count_distinct_positions() {
        let locations = [
            raw(Op::References, "src/a.rs", 21, 4, 1),
            raw(Op::References, "src/a.rs", 21, 4, 1),
            raw(Op::References, "src/a.rs", 21, 12, 1),
            raw(Op::References, "src/a.rs", 22, 4, 1),
        ];
        let groups = group_locations(&locations, &target(TargetShape::Callable), &table());
        assert_eq!(names(&groups, "REFERENCES"), vec![("leaf".to_owned(), 3)]);
    }

    #[test]
    fn incoming_sites_are_the_call_ranges_with_a_floor_of_one() {
        let locations = [
            raw(Op::Incoming, "src/a.rs", 5, 4, 3),
            raw(Op::Incoming, "src/b.rs", 2, 4, 0),
        ];
        let groups = group_locations(&locations, &target(TargetShape::Callable), &table());
        assert_eq!(
            names(&groups, "CALLS"),
            vec![("inner".to_owned(), 3), ("other".to_owned(), 1)]
        );
    }

    #[test]
    fn groups_follow_edge_rel_label_order() {
        let locations = [
            raw(Op::Implementation, "src/a.rs", 21, 4, 1),
            raw(Op::References, "src/b.rs", 3, 0, 1),
            raw(Op::Incoming, "src/a.rs", 6, 4, 1),
        ];
        let method = group_locations(&locations, &target(TargetShape::Method), &table());
        assert_eq!(labels(&method), vec!["CALLS", "REFERENCES", "OVERRIDES"]);
        let class = group_locations(&locations, &target(TargetShape::ClassLike), &table());
        assert_eq!(labels(&class), vec!["CALLS", "EXTENDS", "REFERENCES"]);
        let interface = group_locations(&locations, &target(TargetShape::InterfaceLike), &table());
        assert_eq!(
            labels(&interface),
            vec!["CALLS", "IMPLEMENTS", "REFERENCES"]
        );
        let rel_order = |label: &str| {
            EDGE_REL_LABELS
                .iter()
                .position(|candidate| *candidate == label)
                .expect("known label")
        };
        assert!(rel_order("CALLS") < rel_order("EXTENDS"));
        assert!(rel_order("IMPLEMENTS") < rel_order("REFERENCES"));
        assert!(rel_order("REFERENCES") < rel_order("OVERRIDES"));
    }

    #[test]
    fn callers_within_a_group_are_sorted_by_container() {
        let locations = [
            raw(Op::References, "src/b.rs", 3, 0, 1),
            raw(Op::References, "src/a.rs", 21, 0, 1),
            raw(Op::References, "src/a.rs", 6, 0, 1),
        ];
        let groups = group_locations(&locations, &target(TargetShape::Callable), &table());
        let order: Vec<&str> = groups.groups[0]
            .1
            .iter()
            .map(|hit| hit.key.name.as_str())
            .collect();
        assert_eq!(order, vec!["inner", "leaf", "other"]);
    }

    #[test]
    fn no_locations_means_no_groups() {
        let groups = group_locations(&[], &target(TargetShape::Callable), &table());
        assert_eq!(groups, LspGroups::default());
    }
}
