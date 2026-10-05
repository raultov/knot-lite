//! `explore_file`: the entities of one file, grouped by kind family, with how
//! often each is referenced (heuristic incoming edges).
//!
//! The file argument is resolved leniently: absolute paths under the
//! repository are made relative, and a path that is a unique `*/<path>`
//! suffix of an indexed file resolves to it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use knot::cli_tools::kinds::{
    CALLABLE_KINDS, CONFIG_BUILD_KINDS, K8S_HELM_KINDS, PROSE_KINDS, TYPE_KINDS,
};
use serde::Serialize;
use serde_json::Value;

use super::render::{Column, RenderDoc, RenderSection, str_at, u64_at};
use crate::config::OutputFormat;
use crate::index::load::LoadedIndex;
use crate::index::shard::{KIND_CHUNK, KIND_FILE};

/// Schema id of the explore document.
pub const EXPLORE_SCHEMA_V1: &str = "knot-lite/explore/v1";
/// Most candidates listed for an ambiguous path.
pub const MAX_CANDIDATES: usize = 20;
/// Most names listed in the markup summary.
pub const MARKUP_TOP: usize = 10;

/// Which part of a file an entity belongs to. The variant order is the order
/// of the groups in the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KindFamily {
    /// Classes, structs, enums, traits, interfaces, …
    Types,
    /// Functions, methods, macros, subroutines, …
    Callables,
    /// Markdown documents and sections.
    Docs,
    /// Configuration, build-system and infrastructure metadata.
    Config,
    /// HTML ids and CSS classes (attribute-usage markup).
    Markup,
    /// Everything else (modules, constants, …).
    Other,
    /// Tier-1 `chunk` and `file` records.
    Tier1,
}

/// The family of a wire-format kind label (knot's kind tables).
pub fn family_of(kind: &str) -> KindFamily {
    if kind == KIND_CHUNK || kind == KIND_FILE {
        KindFamily::Tier1
    } else if kind == "html_class" || kind == "html_id" {
        KindFamily::Markup
    } else if TYPE_KINDS.contains(&kind) {
        KindFamily::Types
    } else if CALLABLE_KINDS.contains(&kind) {
        KindFamily::Callables
    } else if PROSE_KINDS.contains(&kind) {
        KindFamily::Docs
    } else if CONFIG_BUILD_KINDS.contains(&kind) || K8S_HELM_KINDS.contains(&kind) {
        KindFamily::Config
    } else {
        KindFamily::Other
    }
}

/// Make a user-supplied path repo-relative: an absolute path under `root`
/// loses the root, `\` becomes `/` and a leading `./` is dropped.
pub fn normalise_input_path(root: &Path, input: &str) -> String {
    let path = Path::new(input.trim());
    let relative = if path.is_absolute() {
        path.strip_prefix(root).map_or_else(
            |_| input.trim().to_owned(),
            |rel| rel.to_string_lossy().into_owned(),
        )
    } else {
        input.trim().to_owned()
    };
    let mut normalised = relative.replace('\\', "/");
    while let Some(rest) = normalised.strip_prefix("./") {
        normalised = rest.to_owned();
    }
    normalised
}

/// How the requested file maps onto the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileResolution {
    /// The path is an indexed file.
    Exact(String),
    /// The path is the unique `*/<path>` suffix of an indexed file.
    Suffix(String),
    /// Several indexed files end with the path (sorted).
    Ambiguous(Vec<String>),
    /// Nothing matches.
    NotFound,
}

impl FileResolution {
    fn label(&self) -> &'static str {
        match self {
            Self::Exact(_) => "exact",
            Self::Suffix(_) => "suffix",
            Self::Ambiguous(_) => "ambiguous",
            Self::NotFound => "not_found",
        }
    }
}

/// Resolve `rel` (already normalised) against the indexed files.
pub fn resolve_file(index: &LoadedIndex, rel: &str) -> FileResolution {
    if rel.is_empty() {
        return FileResolution::NotFound;
    }
    if index.by_file.contains_key(rel) {
        return FileResolution::Exact(rel.to_owned());
    }
    let suffix = format!("/{rel}");
    let mut matches: Vec<String> = index
        .by_file
        .keys()
        .filter(|file| file.ends_with(&suffix))
        .cloned()
        .collect();
    match matches.len() {
        0 => FileResolution::NotFound,
        1 => FileResolution::Suffix(matches.remove(0)),
        _ => FileResolution::Ambiguous(matches),
    }
}

/// One entity (or tier-1 record) of the file.
#[derive(Debug, Clone, Serialize)]
pub struct ExploreEntity {
    /// Record id.
    pub id: String,
    /// Kind label.
    pub kind: String,
    /// Name.
    pub name: String,
    /// Fully qualified name.
    pub fqn: String,
    /// First line.
    pub l0: usize,
    /// Last line.
    pub l1: usize,
    /// Signature, when the entity has one.
    pub signature: Option<String>,
    /// Incoming reference edges.
    pub referenced_by: usize,
}

/// The entities of one family.
#[derive(Debug, Clone, Serialize)]
pub struct ExploreGroup {
    /// The family.
    pub family: KindFamily,
    /// Its entities, sorted by `(l0, name)` (the tier-1 `file` record last).
    pub entities: Vec<ExploreEntity>,
}

/// One name of the markup summary.
#[derive(Debug, Clone, Serialize)]
pub struct MarkupName {
    /// The id or class name.
    pub name: String,
    /// How often it occurs in the file.
    pub count: usize,
}

/// The compact stand-in for the markup family.
#[derive(Debug, Clone, Serialize)]
pub struct MarkupSummary {
    /// Markup entities in the file.
    pub total: usize,
    /// Count per kind (`html_class`, `html_id`).
    pub by_kind: BTreeMap<String, usize>,
    /// The most frequent names.
    pub top: Vec<MarkupName>,
}

/// The `knot-lite/explore/v1` document (§9.4).
#[derive(Debug, Clone, Serialize)]
pub struct ExploreResponse {
    /// [`EXPLORE_SCHEMA_V1`].
    pub schema: &'static str,
    /// knot-lite version.
    pub knot_lite_version: &'static str,
    /// Repository name.
    pub repo: String,
    /// The path as the caller wrote it.
    pub requested: String,
    /// The resolved indexed file, when unambiguous.
    pub file: Option<String>,
    /// `exact | suffix | ambiguous | not_found`.
    pub resolution: &'static str,
    /// Language of the file.
    pub language: Option<String>,
    /// Entity records in the file.
    pub entities: usize,
    /// Tier-1 records (`chunk` / `file`) in the file.
    pub tier1_records: usize,
    /// Entities grouped by family, in family order.
    pub groups: Vec<ExploreGroup>,
    /// Markup summary (when markup exists and `include_markup` is off).
    pub markup: Option<MarkupSummary>,
    /// Indexed files an ambiguous path could mean.
    pub candidates: Vec<String>,
    /// Hints for the reader.
    pub notes: Vec<String>,
}

/// Answer `explore_file` for `requested` against `index`.
pub fn explore_response(
    index: &LoadedIndex,
    repo: &str,
    root: &Path,
    requested: &str,
    include_markup: bool,
) -> ExploreResponse {
    let resolution = resolve_file(index, &normalise_input_path(root, requested));
    let mut response = ExploreResponse {
        schema: EXPLORE_SCHEMA_V1,
        knot_lite_version: env!("CARGO_PKG_VERSION"),
        repo: repo.to_owned(),
        requested: requested.to_owned(),
        file: None,
        resolution: resolution.label(),
        language: None,
        entities: 0,
        tier1_records: 0,
        groups: Vec::new(),
        markup: None,
        candidates: Vec::new(),
        notes: Vec::new(),
    };
    match resolution {
        FileResolution::Exact(file) | FileResolution::Suffix(file) => {
            describe_file(index, &file, include_markup, &mut response);
            response.file = Some(file);
        }
        FileResolution::Ambiguous(matches) => {
            response.notes.push(format!(
                "'{requested}' matches {} indexed files; pass a longer path.",
                matches.len()
            ));
            response.candidates = matches.into_iter().take(MAX_CANDIDATES).collect();
        }
        FileResolution::NotFound => response.notes.push(format!(
            "'{requested}' is not in the index; call list_files to see indexed paths."
        )),
    }
    response
}

fn describe_file(
    index: &LoadedIndex,
    file: &str,
    include_markup: bool,
    response: &mut ExploreResponse,
) {
    let positions = index.by_file.get(file).map_or(&[][..], Vec::as_slice);
    let mut families: BTreeMap<KindFamily, Vec<ExploreEntity>> = BTreeMap::new();
    for &position in positions {
        let line = &index.records[position].line;
        if line.is_tier1() {
            response.tier1_records += 1;
        } else {
            response.entities += 1;
        }
        families
            .entry(family_of(&line.kind))
            .or_default()
            .push(entity_row(index, position));
    }
    // A file with entities reports the language of an entity, not of a chunk.
    let lines: Vec<_> = positions.iter().map(|&p| &index.records[p].line).collect();
    response.language = lines
        .iter()
        .find(|line| !line.is_tier1())
        .or(lines.first())
        .map(|line| line.lang.clone());

    if !include_markup && let Some(markup) = families.remove(&KindFamily::Markup) {
        response.markup = Some(markup_summary(&markup));
        response
            .notes
            .push("markup elements are summarised; pass include_markup to list them.".to_owned());
    }
    response.groups = families
        .into_iter()
        .map(|(family, mut entities)| {
            // Sorted by line; the tier-1 `file` record (no entity has that
            // kind) goes after the windows it summarises.
            entities
                .sort_by_key(|entity| (entity.kind == KIND_FILE, entity.l0, entity.name.clone()));
            ExploreGroup { family, entities }
        })
        .collect();
}

fn entity_row(index: &LoadedIndex, position: usize) -> ExploreEntity {
    let line = &index.records[position].line;
    ExploreEntity {
        id: line.id.clone(),
        kind: line.kind.clone(),
        name: line.name.clone(),
        fqn: line.fqn.clone(),
        l0: line.l0,
        l1: line.l1,
        signature: line.sig.clone(),
        referenced_by: index.incoming.get(&line.id).map_or(0, Vec::len),
    }
}

fn markup_summary(markup: &[ExploreEntity]) -> MarkupSummary {
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut names: HashMap<&str, usize> = HashMap::new();
    for entity in markup {
        *by_kind.entry(entity.kind.clone()).or_default() += 1;
        *names.entry(entity.name.as_str()).or_default() += 1;
    }
    let mut top: Vec<MarkupName> = names
        .into_iter()
        .map(|(name, count)| MarkupName {
            name: name.to_owned(),
            count,
        })
        .collect();
    top.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    top.truncate(MARKUP_TOP);
    MarkupSummary {
        total: markup.len(),
        by_kind,
        top,
    }
}

/// Build the format-agnostic document for a `knot-lite/explore/v1` value.
pub fn explore_doc(value: &Value, _format: OutputFormat) -> RenderDoc {
    let repo = str_at(value, &["repo"]).unwrap_or("-");
    let mut summary = vec![format!(
        "file: {} ({}; requested '{}')",
        str_at(value, &["file"]).unwrap_or("-"),
        str_at(value, &["resolution"]).unwrap_or("-"),
        str_at(value, &["requested"]).unwrap_or("")
    )];
    if let Some(language) = str_at(value, &["language"]) {
        summary.push(format!(
            "language: {language} — {} entities, {} tier-1 records",
            u64_at(value, &["entities"]).unwrap_or(0),
            u64_at(value, &["tier1_records"]).unwrap_or(0)
        ));
    }
    if let Some(markup) = value.get("markup").filter(|markup| !markup.is_null()) {
        summary.push(markup_line(markup));
    }

    let mut sections: Vec<RenderSection> = value
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(group_section)
        .collect();
    let candidates: Vec<Vec<String>> = value
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|path| path.as_str().map(|p| vec![p.to_owned()]))
        .collect();
    if !candidates.is_empty() {
        sections.push(RenderSection {
            heading: Some("Candidates".to_owned()),
            columns: vec![Column::left("PATH")],
            rows: candidates,
        });
    }

    RenderDoc {
        title: format!("knot-lite explore — repository '{repo}'"),
        summary,
        sections,
        notes: strings_at(value, "notes"),
    }
}

fn markup_line(markup: &Value) -> String {
    let kinds: Vec<String> = markup
        .get("by_kind")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(kind, count)| format!("{kind} {}", count.as_u64().unwrap_or(0)))
        .collect();
    let top: Vec<String> = markup
        .get("top")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|name| {
            format!(
                "{} ({})",
                str_at(name, &["name"]).unwrap_or("?"),
                u64_at(name, &["count"]).unwrap_or(0)
            )
        })
        .collect();
    format!(
        "markup: {} elements ({}); most frequent: {}",
        u64_at(markup, &["total"]).unwrap_or(0),
        kinds.join(", "),
        top.join(", ")
    )
}

fn group_section(group: &Value) -> RenderSection {
    let entities = group
        .get("entities")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    RenderSection {
        heading: Some(format!(
            "{} ({})",
            str_at(group, &["family"]).unwrap_or("-"),
            entities.len()
        )),
        columns: vec![
            Column::left("KIND"),
            Column::left("NAME"),
            Column::left("LINES"),
            Column::right("REFS"),
            Column::left("SIGNATURE"),
        ],
        rows: entities
            .iter()
            .map(|entity| {
                vec![
                    str_at(entity, &["kind"]).unwrap_or("-").to_owned(),
                    str_at(entity, &["name"]).unwrap_or("-").to_owned(),
                    format!(
                        "{}-{}",
                        u64_at(entity, &["l0"]).unwrap_or(0),
                        u64_at(entity, &["l1"]).unwrap_or(0)
                    ),
                    u64_at(entity, &["referenced_by"]).unwrap_or(0).to_string(),
                    str_at(entity, &["signature"]).unwrap_or("").to_owned(),
                ]
            })
            .collect(),
    }
}

fn strings_at(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build::test_support::{fixture, run};
    use crate::index::layout::IndexLayout;
    use crate::index::load::load_index;
    use crate::index::load::test_support::{entity, synthetic_index};
    use crate::index::shard::ShardLine;

    fn fixture_index() -> (tempfile::TempDir, LoadedIndex) {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let index = load_index(&IndexLayout::for_repo(tmp.path())).expect("load");
        (tmp, index)
    }

    fn explore(index: &LoadedIndex, requested: &str, markup: bool) -> ExploreResponse {
        explore_response(index, "demo", Path::new("/repo"), requested, markup)
    }

    fn group(response: &ExploreResponse, family: KindFamily) -> Option<&ExploreGroup> {
        response.groups.iter().find(|g| g.family == family)
    }

    #[test]
    fn family_of_covers_knot_tables() {
        for (kind, family) in [
            ("rust_struct", KindFamily::Types),
            ("class", KindFamily::Types),
            ("rust_function", KindFamily::Callables),
            ("method", KindFamily::Callables),
            ("markdown_section", KindFamily::Docs),
            ("cargo_package", KindFamily::Config),
            ("k8s_deployment", KindFamily::Config),
            ("html_id", KindFamily::Markup),
            ("html_class", KindFamily::Markup),
            ("rust_module", KindFamily::Other),
            ("constant", KindFamily::Other),
            ("chunk", KindFamily::Tier1),
            ("file", KindFamily::Tier1),
        ] {
            assert_eq!(family_of(kind), family, "{kind}");
        }
        // The group order is the variant order.
        assert!(KindFamily::Types < KindFamily::Callables && KindFamily::Other < KindFamily::Tier1);
    }

    #[test]
    fn explore_groups_fixture_lib_rs() {
        let (_tmp, index) = fixture_index();
        let response = explore(&index, "src/lib.rs", false);
        assert_eq!(
            (response.resolution, response.file.as_deref()),
            ("exact", Some("src/lib.rs"))
        );
        assert_eq!(response.language.as_deref(), Some("rust"));
        assert_eq!((response.entities, response.tier1_records), (3, 0));

        let families: Vec<KindFamily> = response.groups.iter().map(|g| g.family).collect();
        assert_eq!(families, vec![KindFamily::Callables, KindFamily::Other]);
        let callables: Vec<(&str, usize)> = group(&response, KindFamily::Callables)
            .expect("callables")
            .entities
            .iter()
            .map(|e| (e.name.as_str(), e.referenced_by))
            .collect();
        assert_eq!(
            callables,
            vec![("target", 3), ("caller", 0)],
            "sorted by line; refs from incoming"
        );
        assert_eq!(
            group(&response, KindFamily::Other).expect("other").entities[0].name,
            "util"
        );
        assert!(response.markup.is_none() && response.notes.is_empty());
    }

    #[test]
    fn explore_suffix_resolution() {
        let (_tmp, index) = fixture_index();
        let response = explore(&index, "util.rs", false);
        assert_eq!(
            (response.resolution, response.file.as_deref()),
            ("suffix", Some("src/util.rs"))
        );
        assert_eq!(response.requested, "util.rs");
        assert_eq!(
            resolve_file(&index, "app.ts"),
            FileResolution::Suffix("web/app.ts".to_owned())
        );
    }

    fn two_mod_rs() -> LoadedIndex {
        let function = |id: &str, name: &str| entity(id, "rust_function", name, name, (1, 2));
        synthetic_index(
            vec![
                ("src/a/mod.rs", function("1", "one")),
                ("src/b/mod.rs", function("2", "two")),
                ("src/lib.rs", function("3", "three")),
            ],
            &[],
        )
    }

    #[test]
    fn explore_ambiguous_lists_candidates() {
        let index = two_mod_rs();
        let response = explore(&index, "mod.rs", false);
        assert_eq!(response.resolution, "ambiguous");
        assert_eq!(response.file, None);
        assert!(response.groups.is_empty());
        assert_eq!(response.candidates, vec!["src/a/mod.rs", "src/b/mod.rs"]);
        assert!(response.notes[0].contains("matches 2 indexed files"));
        // A longer path disambiguates.
        assert_eq!(
            explore(&index, "a/mod.rs", false).file.as_deref(),
            Some("src/a/mod.rs")
        );
    }

    #[test]
    fn explore_ambiguity_is_capped() {
        let records = (0..30)
            .map(|i| {
                let file: &'static str = Box::leak(format!("pkg{i:02}/mod.rs").into_boxed_str());
                (
                    file,
                    entity(&i.to_string(), "rust_function", "f", "f", (1, 2)),
                )
            })
            .collect();
        let response = explore(&synthetic_index(records, &[]), "mod.rs", false);
        assert_eq!(response.candidates.len(), MAX_CANDIDATES);
        assert_eq!(response.candidates[0], "pkg00/mod.rs");
    }

    #[test]
    fn explore_absolute_path_is_normalised() {
        let root = Path::new("/work/repo");
        for (input, expected) in [
            ("/work/repo/src/lib.rs", "src/lib.rs"),
            ("./src/lib.rs", "src/lib.rs"),
            ("././src\\lib.rs", "src/lib.rs"),
            ("src/lib.rs", "src/lib.rs"),
            ("/elsewhere/src/lib.rs", "/elsewhere/src/lib.rs"),
            ("  src/lib.rs  ", "src/lib.rs"),
        ] {
            assert_eq!(normalise_input_path(root, input), expected, "{input:?}");
        }
        let (tmp, index) = fixture_index();
        let absolute = tmp.path().join("src/lib.rs");
        let response = explore_response(
            &index,
            "demo",
            tmp.path(),
            &absolute.to_string_lossy(),
            false,
        );
        assert_eq!(response.file.as_deref(), Some("src/lib.rs"));
    }

    fn markup_index() -> LoadedIndex {
        let markup = |id: &str, kind: &str, name: &str, line: usize| {
            entity(id, kind, name, &format!("{kind}:{name}"), (line, line))
        };
        synthetic_index(
            vec![
                ("web/index.html", markup("1", "html_class", "btn", 3)),
                ("web/index.html", markup("2", "html_class", "btn", 8)),
                ("web/index.html", markup("3", "html_class", "card", 5)),
                ("web/index.html", markup("4", "html_id", "main", 1)),
                (
                    "web/index.html",
                    entity("5", "function", "boot", "boot", (10, 12)),
                ),
            ],
            &[],
        )
    }

    #[test]
    fn explore_markup_summary_vs_full() {
        let index = markup_index();
        let summarised = explore(&index, "web/index.html", false);
        let summary = summarised.markup.as_ref().expect("markup summary");
        assert_eq!(summary.total, 4);
        assert_eq!(summary.by_kind["html_class"], 3);
        assert_eq!(summary.by_kind["html_id"], 1);
        assert_eq!(summary.top[0].name, "btn");
        assert_eq!(summary.top[0].count, 2);
        assert!(group(&summarised, KindFamily::Markup).is_none());
        assert!(summarised.notes[0].contains("include_markup"));
        assert_eq!(
            summarised.entities, 5,
            "the summary still counts the elements"
        );

        let full = explore(&index, "web/index.html", true);
        assert!(full.markup.is_none());
        let listed = &group(&full, KindFamily::Markup)
            .expect("markup group")
            .entities;
        assert_eq!(listed.len(), 4);
        assert_eq!(listed[0].name, "main", "sorted by line");
    }

    #[test]
    fn markup_top_is_capped_and_ordered() {
        let records: Vec<(&str, ShardLine)> = (0..15)
            .map(|i| {
                let name = format!("c{i:02}");
                (
                    "a.html",
                    entity(&i.to_string(), "html_class", &name, &name, (i + 1, i + 1)),
                )
            })
            .collect();
        let response = explore(&synthetic_index(records, &[]), "a.html", false);
        let top = &response.markup.expect("summary").top;
        assert_eq!(top.len(), MARKUP_TOP);
        assert_eq!(top[0].name, "c00", "equal counts sort by name");
    }

    #[test]
    fn explore_tier1_file_shows_windows() {
        let (_tmp, index) = fixture_index();
        let response = explore(&index, "scripts/run.sh", false);
        assert_eq!((response.entities, response.tier1_records), (0, 3));
        let tier1 = &group(&response, KindFamily::Tier1)
            .expect("tier-1 group")
            .entities;
        let kinds: Vec<&str> = tier1.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["chunk", "chunk", "file"]);
        assert_eq!((tier1[0].l0, tier1[0].l1), (1, 50));
        assert_eq!(response.language.as_deref(), Some("other"));
    }

    #[test]
    fn explore_unknown_file_hint() {
        let (_tmp, index) = fixture_index();
        for requested in ["nope/missing.rs", ""] {
            let response = explore(&index, requested, false);
            assert_eq!(response.resolution, "not_found", "{requested:?}");
            assert!(response.groups.is_empty() && response.file.is_none());
            assert!(
                response.notes[0].contains("call list_files to see indexed paths"),
                "{:?}",
                response.notes
            );
        }
    }

    #[test]
    fn explore_renderers_snapshot() {
        let (_tmp, index) = fixture_index();
        let value = serde_json::to_value(explore(&index, "src/lib.rs", false)).expect("json");
        assert_eq!(value["schema"], "knot-lite/explore/v1");
        assert_eq!(value["resolution"], "exact");
        assert_eq!(value["markup"], Value::Null);
        assert_eq!(value["groups"][0]["family"], "callables");

        let doc = explore_doc(&value, OutputFormat::Table);
        assert_eq!(doc.title, "knot-lite explore — repository 'demo'");
        assert_eq!(
            doc.summary[0],
            "file: src/lib.rs (exact; requested 'src/lib.rs')"
        );
        assert_eq!(
            doc.summary[1],
            "language: rust — 3 entities, 0 tier-1 records"
        );
        assert_eq!(doc.sections[0].heading.as_deref(), Some("callables (2)"));
        assert_eq!(
            doc.sections[0].rows[0],
            vec!["rust_function", "target", "3-5", "3", "()"]
        );

        let ambiguous =
            serde_json::to_value(explore(&two_mod_rs(), "mod.rs", false)).expect("json");
        let doc = explore_doc(&ambiguous, OutputFormat::Markdown);
        assert_eq!(doc.sections[0].heading.as_deref(), Some("Candidates"));
        assert_eq!(doc.sections[0].rows.len(), 2);

        let markup =
            serde_json::to_value(explore(&markup_index(), "web/index.html", false)).expect("json");
        assert!(
            explore_doc(&markup, OutputFormat::Table)
                .summary
                .iter()
                .any(|l| l.starts_with(
                    "markup: 4 elements (html_class 3, html_id 1); most frequent: btn (2)"
                ))
        );
    }
}
