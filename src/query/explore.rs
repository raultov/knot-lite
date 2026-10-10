//! `explore_file`: the entities of one file, grouped by kind family, with how
//! many distinct callers reference each — from the best source per entity
//! (live language server, pre-computed answer, heuristic edges; P5-D14). A
//! file that changed since the index, or that is not indexed, is described by
//! its server outline (P5-D15).
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

use super::callers::LspContext;
use super::explore_refs::{ExploreLive, ExploreLsp, RefTarget, family_rank, on_disk_candidate};
use super::outline::{OutlineRow, match_symbols, symbol_family, symbol_kind_label, symbol_shape};
use super::render::{Column, RenderDoc, RenderSection, str_at, u64_at};
use super::sources::{
    PROVENANCE_HEURISTIC, PROVENANCE_INDEXED, PROVENANCE_LIVE, heuristic_answer, indexed_answer,
    own_container,
};
use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::config::OutputFormat;
use crate::index::load::LoadedIndex;
use crate::index::shard::{KIND_CHUNK, KIND_FILE};
use crate::lsp::ops::{TargetShape, shape_of};
use crate::lsp::outline::OutlineSymbol;
use crate::lsp::request::{SourceFiles, Target, locate_target};

/// Schema id of the explore document (P5-D3).
pub const EXPLORE_SCHEMA_V2: &str = "knot-lite/explore/v2";
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
    /// Record id; `null` for a server symbol that matched no record.
    pub id: Option<String>,
    /// Kind label (`lsp:<kind>` for an unmatched server symbol).
    pub kind: String,
    /// Name.
    pub name: String,
    /// Fully qualified name, when the entity is indexed.
    pub fqn: Option<String>,
    /// First line.
    pub l0: usize,
    /// Last line.
    pub l1: usize,
    /// Signature, when the entity has one.
    pub signature: Option<String>,
    /// Distinct caller containers; `null` when no source counted them.
    pub referenced_by: Option<usize>,
    /// `lsp-live`, `lsp-indexed`, `heuristic`, or `null`.
    pub refs_provenance: Option<&'static str>,
    /// Whether the entity is a record of the index.
    pub indexed: bool,
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

/// `outline` object of explore/v2: where the entity list came from.
#[derive(Debug, Clone, Serialize)]
pub struct OutlineInfo {
    /// `index` or `lsp-live`.
    pub provenance: &'static str,
    /// The server that sent the outline (`lsp-live` only).
    pub server: Option<&'static str>,
    /// `changed_since_index` or `not_indexed` (`lsp-live` only).
    pub reason: Option<&'static str>,
}

impl OutlineInfo {
    const INDEX: Self = Self {
        provenance: "index",
        server: None,
        reason: None,
    };
}

/// The `knot-lite/explore/v2` document (§10.2).
#[derive(Debug, Clone, Serialize)]
pub struct ExploreResponse {
    /// [`EXPLORE_SCHEMA_V2`].
    pub schema: &'static str,
    /// knot-lite version.
    pub knot_lite_version: &'static str,
    /// Repository name.
    pub repo: String,
    /// The path as the caller wrote it.
    pub requested: String,
    /// The resolved file, when unambiguous.
    pub file: Option<String>,
    /// `exact | suffix | ambiguous | not_found | not_indexed`.
    pub resolution: &'static str,
    /// Language of the file.
    pub language: Option<String>,
    /// Entities in the file.
    pub entities: usize,
    /// Tier-1 records (`chunk` / `file`) in the file.
    pub tier1_records: usize,
    /// Where the entity list came from.
    pub outline: OutlineInfo,
    /// What the language server did for this call.
    pub lsp: ExploreLsp,
    /// Entities grouped by family, in family order.
    pub groups: Vec<ExploreGroup>,
    /// Markup summary (when markup exists and `include_markup` is off).
    pub markup: Option<MarkupSummary>,
    /// Indexed files an ambiguous path could mean.
    pub candidates: Vec<String>,
    /// Hints for the reader.
    pub notes: Vec<String>,
}

/// Inputs of [`explore_response`].
pub struct ExploreContext<'a> {
    /// The loaded index.
    pub index: &'a LoadedIndex,
    /// Repository name.
    pub repo: &'a str,
    /// The path as the caller wrote it.
    pub requested: &'a str,
    /// Whether markup entities are listed.
    pub include_markup: bool,
    /// What language servers may be asked (and the repository root).
    pub lsp: LspContext<'a>,
}

/// Note of a changed file described from the index (no outline from a server).
const STALE_NOTE: &str = "the file changed since the last index; entities and lines may be outdated (run knot-lite index)";

/// Answer `explore_file` for `ctx.requested`.
pub fn explore_response(ctx: &ExploreContext<'_>) -> ExploreResponse {
    let index = ctx.index;
    let rel = normalise_input_path(ctx.lsp.repo_root, ctx.requested);
    let resolution = resolve_file(index, &rel);
    let mut response = ExploreResponse {
        schema: EXPLORE_SCHEMA_V2,
        knot_lite_version: env!("CARGO_PKG_VERSION"),
        repo: ctx.repo.to_owned(),
        requested: ctx.requested.to_owned(),
        file: None,
        resolution: resolution.label(),
        language: None,
        entities: 0,
        tier1_records: 0,
        outline: OutlineInfo::INDEX,
        lsp: ExploreLsp {
            status: "not_applicable",
            server: None,
            asked: 0,
            answered: 0,
            ms: 0,
        },
        groups: Vec::new(),
        markup: None,
        candidates: Vec::new(),
        notes: Vec::new(),
    };
    match resolution {
        FileResolution::Exact(file) | FileResolution::Suffix(file) => {
            explore_indexed(ctx, &file, &mut response);
            response.file = Some(file);
        }
        FileResolution::Ambiguous(matches) => {
            response.notes.push(format!(
                "'{}' matches {} indexed files; pass a longer path.",
                ctx.requested,
                matches.len()
            ));
            response.candidates = matches.into_iter().take(MAX_CANDIDATES).collect();
        }
        FileResolution::NotFound => explore_unindexed(ctx, &rel, &mut response),
    }
    response
}

fn live_for<'a>(ctx: &'a ExploreContext<'a>, rel: &'a str) -> ExploreLive<'a> {
    ExploreLive {
        index: ctx.index,
        lsp: &ctx.lsp,
        lang: lsp_lang_for_label(label_for(Path::new(rel))),
        rel,
    }
}

/// An indexed file: the server's outline when it changed since the index,
/// else the indexed entities; then live reference counts.
fn explore_indexed(ctx: &ExploreContext<'_>, file: &str, response: &mut ExploreResponse) {
    let live = live_for(ctx, file);
    if live.lang.is_some() && is_stale(ctx, file) {
        match live.outline() {
            Ok(symbols) => {
                describe_outline(ctx, &live, file, symbols, response);
                response.outline.reason = Some("changed_since_index");
                return;
            }
            Err(lsp) => {
                response.lsp = lsp;
                response.notes.push(STALE_NOTE.to_owned());
            }
        }
    }
    let positions = describe_file(ctx.index, file, ctx.include_markup, response);
    if response.notes.iter().any(|note| note == STALE_NOTE) {
        return;
    }
    let targets = indexed_targets(ctx, file, &positions);
    apply_live_counts(&live, targets, response);
}

/// A path the index does not know: the server's outline of the file on disk,
/// when it is a candidate the index could describe; else `not_found`.
fn explore_unindexed(ctx: &ExploreContext<'_>, rel: &str, response: &mut ExploreResponse) {
    let live = live_for(ctx, rel);
    if live.lang.is_some() && on_disk_candidate(ctx.lsp.repo_root, rel) {
        match live.outline() {
            Ok(symbols) => {
                describe_outline(ctx, &live, rel, symbols, response);
                response.outline.reason = Some("not_indexed");
                response.resolution = "not_indexed";
                response.file = Some(rel.to_owned());
                return;
            }
            Err(lsp) => response.lsp = lsp,
        }
    }
    response.notes.push(format!(
        "'{}' is not in the index; call list_files to see indexed paths.",
        ctx.requested
    ));
}

/// Whether `file` differs from the content the index was built from.
fn is_stale(ctx: &ExploreContext<'_>, file: &str) -> bool {
    let Some(indexed) = ctx.index.file_sha16.get(file) else {
        return false;
    };
    let mut files = SourceFiles::new(ctx.lsp.repo_root);
    files
        .get(file)
        .is_ok_and(|source| source.sha16() != indexed.as_str())
}

/// The indexed entities that may be counted live: a located column, a shape a
/// server knows, no fresh pre-computed answer. Their positions come from the
/// current file (unlocatable ones are skipped).
fn indexed_targets(
    ctx: &ExploreContext<'_>,
    file: &str,
    positions: &[(usize, usize)],
) -> Vec<RefTarget> {
    let index = ctx.index;
    let Some(lang) = lsp_lang_for_label(label_for(Path::new(file))) else {
        return Vec::new();
    };
    let mut files = SourceFiles::new(ctx.lsp.repo_root);
    positions
        .iter()
        .filter_map(|&(row, at)| {
            let line = &index.records[at].line;
            let shape = shape_of(&line.kind);
            if line.col.is_none() || shape == TargetShape::None || has_indexed_answer(index, at) {
                return None;
            }
            let target = Target {
                rel: file,
                line,
                lang,
                indexed_sha16: None,
            };
            let position = locate_target(&mut files, &target).ok()?;
            Some(RefTarget {
                row,
                position,
                shape,
                own: own_container(index, at),
                rank: (family_rank(family_of(&line.kind)), line.l0),
            })
        })
        .collect()
}

fn has_indexed_answer(index: &LoadedIndex, at: usize) -> bool {
    index
        .lsp_targets
        .get(&index.records[at].line.id)
        .is_some_and(|line| line.status == "ok")
}

/// Ask the server and write the answered counts into the response rows
/// (rows are numbered in the order the groups list the entities).
fn apply_live_counts(
    live: &ExploreLive<'_>,
    targets: Vec<RefTarget>,
    response: &mut ExploreResponse,
) {
    let (counts, lsp) = live.count(targets);
    response.lsp = lsp;
    let mut slots: Vec<&mut ExploreEntity> = response
        .groups
        .iter_mut()
        .flat_map(|group| group.entities.iter_mut())
        .collect();
    for (row, count) in counts {
        if let Some(entity) = slots.get_mut(row) {
            entity.referenced_by = Some(count);
            entity.refs_provenance = Some(PROVENANCE_LIVE);
        }
    }
}

/// Fill the response from the indexed records of `file`. Returns
/// `(row, record position)` for every listed entity, rows numbered in the
/// order the groups list them.
fn describe_file(
    index: &LoadedIndex,
    file: &str,
    include_markup: bool,
    response: &mut ExploreResponse,
) -> Vec<(usize, usize)> {
    let positions = index.by_file.get(file).map_or(&[][..], Vec::as_slice);
    let mut families: BTreeMap<KindFamily, Vec<(ExploreEntity, usize)>> = BTreeMap::new();
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
            .push((entity_row(index, position), position));
    }
    // A file with entities reports the language of an entity, not of a chunk.
    let lines: Vec<_> = positions.iter().map(|&p| &index.records[p].line).collect();
    response.language = lines
        .iter()
        .find(|line| !line.is_tier1())
        .or(lines.first())
        .map(|line| line.lang.clone());

    if !include_markup && let Some(markup) = families.remove(&KindFamily::Markup) {
        let entities: Vec<ExploreEntity> = markup.into_iter().map(|(entity, _)| entity).collect();
        response.markup = Some(markup_summary(&entities));
        response
            .notes
            .push("markup elements are summarised; pass include_markup to list them.".to_owned());
    }
    finish_groups(families, response)
}

/// Sort each family (by line; the tier-1 `file` record after the windows it
/// summarises) and number the rows in listing order.
fn finish_groups(
    families: BTreeMap<KindFamily, Vec<(ExploreEntity, usize)>>,
    response: &mut ExploreResponse,
) -> Vec<(usize, usize)> {
    let mut numbered = Vec::new();
    response.groups = families
        .into_iter()
        .map(|(family, mut entities)| {
            entities.sort_by_key(|(entity, _)| {
                (entity.kind == KIND_FILE, entity.l0, entity.name.clone())
            });
            let entities = entities
                .into_iter()
                .map(|(entity, position)| {
                    numbered.push((numbered.len(), position));
                    entity
                })
                .collect();
            ExploreGroup { family, entities }
        })
        .collect();
    numbered
}

/// Fill the response from a server outline: matched symbols keep the record's
/// identity (with current lines) and its indexed count as a start; unmatched
/// ones are listed as `lsp:<kind>`. Then the symbols are counted live.
fn describe_outline(
    ctx: &ExploreContext<'_>,
    live: &ExploreLive<'_>,
    rel: &str,
    symbols: Vec<OutlineSymbol>,
    response: &mut ExploreResponse,
) {
    let index = ctx.index;
    let indexed_file = index.by_file.contains_key(rel).then_some(rel);
    let rows = match_symbols(index, indexed_file, symbols);
    let mut families: BTreeMap<KindFamily, Vec<(ExploreEntity, OutlineRow)>> = BTreeMap::new();
    for row in rows {
        let (family, entity) = outline_entity(index, &row);
        families.entry(family).or_default().push((entity, row));
    }
    response.entities = families.values().map(Vec::len).sum();
    response.language = Some(label_for(Path::new(rel)).to_owned());
    response.outline = OutlineInfo {
        provenance: PROVENANCE_LIVE,
        server: live.lang.map(LspLang::server_program),
        reason: None,
    };
    let mut targets = Vec::new();
    response.groups = families
        .into_iter()
        .map(|(family, mut entities)| {
            entities.sort_by_key(|(entity, _)| (entity.l0, entity.name.clone()));
            let entities = entities
                .into_iter()
                .map(|(entity, row)| {
                    let number = targets.len();
                    targets.push(outline_target(index, rel, number, family, &row));
                    entity
                })
                .collect();
            ExploreGroup { family, entities }
        })
        .collect();
    let targets: Vec<RefTarget> = targets.into_iter().flatten().collect();
    apply_live_counts(live, targets, response);
}

/// The entity row of an outline symbol and its family.
fn outline_entity(index: &LoadedIndex, row: &OutlineRow) -> (KindFamily, ExploreEntity) {
    let symbol = &row.symbol;
    let (l0, l1) = (
        symbol.first_line0 as usize + 1,
        symbol.last_line0 as usize + 1,
    );
    match row.record {
        Some(at) => {
            let line = &index.records[at].line;
            let mut entity = entity_row(index, at);
            entity.l0 = l0;
            entity.l1 = l1;
            (family_of(&line.kind), entity)
        }
        None => (
            symbol_family(symbol.kind),
            ExploreEntity {
                id: None,
                kind: symbol_kind_label(symbol.kind),
                name: symbol.name.clone(),
                fqn: None,
                l0,
                l1,
                signature: None,
                referenced_by: None,
                refs_provenance: None,
                indexed: false,
            },
        ),
    }
}

/// The live-count target of an outline symbol, at its current position.
fn outline_target(
    index: &LoadedIndex,
    rel: &str,
    row: usize,
    family: KindFamily,
    outline: &OutlineRow,
) -> Option<RefTarget> {
    let symbol = &outline.symbol;
    let shape = symbol_shape(symbol.kind);
    if shape == TargetShape::None {
        return None;
    }
    let line1 = symbol.selection_line0 as usize + 1;
    Some(RefTarget {
        row,
        position: async_lsp::lsp_types::Position::new(symbol.selection_line0, symbol.selection_col),
        shape,
        own: index.containers.container_at(rel, line1).0,
        rank: (family_rank(family), line1),
    })
}

/// An indexed record's row, counted from the pre-computed answer when it is
/// fresh, else from the heuristic edges (distinct caller containers).
fn entity_row(index: &LoadedIndex, position: usize) -> ExploreEntity {
    let line = &index.records[position].line;
    let (referenced_by, provenance) = if line.is_tier1() {
        (None, None)
    } else if has_indexed_answer(index, position) {
        (
            Some(indexed_answer(index, position).caller_count),
            Some(PROVENANCE_INDEXED),
        )
    } else {
        (
            Some(heuristic_answer(index, position).caller_count),
            Some(PROVENANCE_HEURISTIC),
        )
    };
    ExploreEntity {
        id: Some(line.id.clone()),
        kind: line.kind.clone(),
        name: line.name.clone(),
        fqn: Some(line.fqn.clone()),
        l0: line.l0,
        l1: line.l1,
        signature: line.sig.clone(),
        referenced_by,
        refs_provenance: provenance,
        indexed: true,
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

/// Build the format-agnostic document for a `knot-lite/explore/v2` value.
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
    summary.extend(source_lines(value));
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
            caption: None,
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

/// `outline: …` (when a server sent it) and `language server: …` (when one applies).
fn source_lines(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if str_at(value, &["outline", "provenance"]) == Some(PROVENANCE_LIVE) {
        lines.push(format!(
            "outline: {PROVENANCE_LIVE} ({}, {})",
            str_at(value, &["outline", "server"]).unwrap_or("-"),
            str_at(value, &["outline", "reason"]).unwrap_or("-")
        ));
    }
    let status = str_at(value, &["lsp", "status"]).unwrap_or("not_applicable");
    if status != "not_applicable" {
        lines.push(format!(
            "language server: {status} — {} asked, {} answered in {} ms",
            u64_at(value, &["lsp", "asked"]).unwrap_or(0),
            u64_at(value, &["lsp", "answered"]).unwrap_or(0),
            u64_at(value, &["lsp", "ms"]).unwrap_or(0)
        ));
    }
    lines
}

/// `L` live, `I` pre-computed, `H` heuristic, `-` none.
fn source_letter(entity: &Value) -> &'static str {
    match str_at(entity, &["refs_provenance"]) {
        Some(PROVENANCE_LIVE) => "L",
        Some(PROVENANCE_INDEXED) => "I",
        Some(PROVENANCE_HEURISTIC) => "H",
        _ => "-",
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
        caption: None,
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
            Column::left("SRC"),
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
                    u64_at(entity, &["referenced_by"])
                        .map_or_else(|| "-".to_owned(), |n| n.to_string()),
                    source_letter(entity).to_owned(),
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

    /// Explore with language servers off (no pool), the repository at `root`.
    fn explore_at(
        index: &LoadedIndex,
        root: &Path,
        requested: &str,
        markup: bool,
    ) -> ExploreResponse {
        let settings = crate::config::test_support::config_for(root.to_path_buf()).lsp;
        explore_response(&ExploreContext {
            index,
            repo: "demo",
            requested,
            include_markup: markup,
            lsp: LspContext {
                settings: &settings,
                repo_root: root,
                pool: None,
            },
        })
    }

    fn explore(index: &LoadedIndex, requested: &str, markup: bool) -> ExploreResponse {
        explore_at(index, Path::new("/repo"), requested, markup)
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
        let callables: Vec<(&str, Option<usize>)> = group(&response, KindFamily::Callables)
            .expect("callables")
            .entities
            .iter()
            .map(|e| (e.name.as_str(), e.referenced_by))
            .collect();
        assert_eq!(
            callables,
            vec![("target", Some(3)), ("caller", Some(0))],
            "sorted by line; distinct heuristic caller containers"
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
        let response = explore_at(&index, tmp.path(), &absolute.to_string_lossy(), false);
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
        assert_eq!(value["schema"], "knot-lite/explore/v2");
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
            vec!["rust_function", "target", "3-5", "3", "H", "()"]
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

/// P5.12: reference sources and outlines from a (fake) language server.
#[cfg(test)]
mod lsp_tests {
    use std::time::Duration;

    use async_lsp::lsp_types::{DocumentSymbol, DocumentSymbolResponse, Position, SymbolKind};
    use serde_json::json;

    use super::*;
    use crate::index::build::test_support::{fixture, run};
    use crate::index::layout::IndexLayout;
    use crate::index::load::load_index;
    use crate::index::load::test_support::{entity, synthetic_index};
    use crate::index::lsp_records::LspTargetLine;
    use crate::lsp::fake::{FakeScript, FakeServers, fixture_target_script};
    use crate::lsp::pool::{LspPool, PoolSettings};
    use crate::lsp::settings::{LspSettings, resolve_lsp_settings};
    use crate::lsp::target::OpCounts;

    fn settings() -> LspSettings {
        resolve_lsp_settings(&|_| None, Path::new("/tmp"))
    }

    fn fixture_index() -> (tempfile::TempDir, LoadedIndex) {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let index = load_index(&IndexLayout::for_repo(tmp.path())).expect("load");
        (tmp, index)
    }

    fn explore_with(
        index: &LoadedIndex,
        root: &Path,
        settings: &LspSettings,
        pool: Option<&LspPool>,
        requested: &str,
    ) -> ExploreResponse {
        explore_response(&ExploreContext {
            index,
            repo: "demo",
            requested,
            include_markup: false,
            lsp: LspContext {
                settings,
                repo_root: root,
                pool,
            },
        })
    }

    /// Every entity of the response, in listing order.
    fn rows(response: &ExploreResponse) -> Vec<&ExploreEntity> {
        response
            .groups
            .iter()
            .flat_map(|group| group.entities.iter())
            .collect()
    }

    fn row<'a>(response: &'a ExploreResponse, name: &str) -> &'a ExploreEntity {
        response
            .groups
            .iter()
            .flat_map(|group| group.entities.iter())
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} missing: {:?}", rows(response)))
    }

    /// A nested symbol whose name sits at `(line, col)` (0-based).
    fn symbol(name: &str, kind: SymbolKind, (line, col): (u32, u32), last: u32) -> DocumentSymbol {
        serde_json::from_value(json!({
            "name": name, "kind": kind,
            "range": {"start": {"line": line, "character": 0}, "end": {"line": last, "character": 1}},
            "selectionRange": {"start": {"line": line, "character": col}, "end": {"line": line, "character": col + 3}},
        }))
        .expect("symbol")
    }

    /// The fixture server, plus the outline of `rel`.
    fn with_outline(root: &Path, rel: &str, symbols: Vec<DocumentSymbol>) -> FakeServers {
        let base = fixture_target_script(root);
        let rel = rel.to_owned();
        FakeServers::new(move |lang| FakeScript {
            symbols: vec![(rel.clone(), DocumentSymbolResponse::Nested(symbols.clone()))],
            ..base(lang)
        })
    }

    #[test]
    fn fresh_file_counts_sources_by_precedence() {
        let (tmp, mut index) = fixture_index();
        let caller = index.by_name["caller"][0];
        index.lsp_targets.insert(
            index.records[caller].line.id.clone(),
            LspTargetLine {
                id: index.records[caller].line.id.clone(),
                status: "ok".to_owned(),
                ms: 1,
                ops: OpCounts::default(),
                server: "rust-analyzer".to_owned(),
            },
        );
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        let response = explore_with(&index, tmp.path(), &settings(), Some(&pool), "src/lib.rs");
        assert_eq!(response.schema, "knot-lite/explore/v2");
        assert_eq!(response.outline.provenance, "index");
        // `target`: asked live (references only): caller, helper, run.
        assert_eq!(
            (
                row(&response, "target").referenced_by,
                row(&response, "target").refs_provenance
            ),
            (Some(3), Some("lsp-live"))
        );
        // `caller`: a fresh pre-computed answer, no request.
        assert_eq!(
            row(&response, "caller").refs_provenance,
            Some("lsp-indexed")
        );
        // `util` (a module): no server shape, heuristic.
        assert_eq!(row(&response, "util").refs_provenance, Some("heuristic"));
        assert_eq!(response.lsp.status, "ok");
        assert_eq!((response.lsp.asked, response.lsp.answered), (1, 1));
        assert_eq!(response.lsp.server, Some("rust-analyzer"));
    }

    #[test]
    fn heuristic_counts_distinct_containers() {
        let index = synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("t", "rust_function", "t", "a::t", (1, 2)),
                ),
                (
                    "src/a.rs",
                    entity("c", "rust_function", "c", "a::c", (4, 9)),
                ),
            ],
            &[("c", "t", "CALLS"), ("c", "t", "REFERENCES")],
        );
        let response = explore_with(&index, Path::new("/repo"), &settings(), None, "src/a.rs");
        assert_eq!(
            row(&response, "t").referenced_by,
            Some(1),
            "two edges, one caller container"
        );
        assert_eq!(row(&response, "t").refs_provenance, Some("heuristic"));
    }

    #[test]
    fn lsp_off_reports_off_and_no_server_language_not_applicable() {
        let (tmp, index) = fixture_index();
        let response = explore_with(&index, tmp.path(), &settings(), None, "src/lib.rs");
        assert_eq!(response.lsp.status, "off");
        assert!(
            rows(&response)
                .iter()
                .all(|entity| entity.refs_provenance == Some("heuristic"))
        );
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        let docs = explore_with(
            &index,
            tmp.path(),
            &settings(),
            Some(&pool),
            "docs/guide.md",
        );
        assert_eq!(docs.lsp.status, "not_applicable");
        assert_eq!(docs.lsp.server, None);
    }

    #[test]
    fn not_ready_server_keeps_index_counts() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path()))
            .starting_for(Duration::from_millis(500))
            .pool_with(PoolSettings {
                ready_wait: Duration::from_millis(20),
                ..PoolSettings::default()
            });
        let response = explore_with(&index, tmp.path(), &settings(), Some(&pool), "src/lib.rs");
        assert_eq!(response.lsp.status, "not_ready");
        assert_eq!(row(&response, "target").refs_provenance, Some("heuristic"));
    }

    #[test]
    fn live_budget_and_max_respected_partial_status() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        let mut tight = settings();
        tight.deadline = Duration::ZERO;
        let response = explore_with(&index, tmp.path(), &tight, Some(&pool), "src/lib.rs");
        assert_eq!((response.lsp.status, response.lsp.asked), ("partial", 0));

        let lsp_ctx = LspContext {
            settings: &settings(),
            repo_root: tmp.path(),
            pool: Some(&pool),
        };
        let live = ExploreLive {
            index: &index,
            lsp: &lsp_ctx,
            lang: Some(LspLang::Rust),
            rel: "src/lib.rs",
        };
        let own = index.containers.container_at("src/lib.rs", 3).0;
        let targets: Vec<RefTarget> = (0..45)
            .map(|row| RefTarget {
                row,
                position: Position::new(2, 7),
                shape: TargetShape::Callable,
                own: own.clone(),
                rank: (2, row),
            })
            .collect();
        let (counts, lsp) = live.count(targets);
        assert_eq!((lsp.status, lsp.asked, counts.len()), ("partial", 40, 40));
    }

    #[test]
    fn types_and_callables_asked_first() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        let lsp_ctx = LspContext {
            settings: &settings(),
            repo_root: tmp.path(),
            pool: Some(&pool),
        };
        let live = ExploreLive {
            index: &index,
            lsp: &lsp_ctx,
            lang: Some(LspLang::Rust),
            rel: "src/lib.rs",
        };
        let own = index.containers.container_at("src/lib.rs", 3).0;
        let target = |row, rank| RefTarget {
            row,
            position: Position::new(2, 7),
            shape: TargetShape::Callable,
            own: own.clone(),
            rank,
        };
        let (counts, lsp) = live.count(vec![
            target(0, (2, 1)),
            target(1, (1, 5)),
            target(2, (0, 9)),
        ]);
        assert_eq!(lsp.status, "ok");
        assert_eq!(
            counts.iter().map(|(row, _)| *row).collect::<Vec<_>>(),
            vec![2, 1, 0]
        );
    }

    #[test]
    fn stale_file_uses_outline_and_matches_records() {
        let (tmp, index) = fixture_index();
        let lib = tmp.path().join("src/lib.rs");
        let mut text = std::fs::read_to_string(&lib).expect("read");
        text.push_str("\npub fn added() -> u32 {\n    target()\n}\n");
        std::fs::write(&lib, text).expect("write");
        let servers = with_outline(
            tmp.path(),
            "src/lib.rs",
            vec![
                symbol("target", SymbolKind::FUNCTION, (2, 7), 4),
                symbol("caller", SymbolKind::FUNCTION, (6, 7), 8),
                symbol("added", SymbolKind::FUNCTION, (10, 7), 12),
            ],
        );
        let pool = servers.pool();
        let response = explore_with(&index, tmp.path(), &settings(), Some(&pool), "src/lib.rs");
        assert_eq!(response.outline.provenance, "lsp-live");
        assert_eq!(response.outline.reason, Some("changed_since_index"));
        assert_eq!(response.outline.server, Some("rust-analyzer"));
        let target = row(&response, "target");
        assert!(target.indexed && target.id.is_some());
        assert_eq!(
            (target.referenced_by, target.refs_provenance),
            (Some(3), Some("lsp-live"))
        );
        let added = row(&response, "added");
        assert_eq!(
            (added.id.as_deref(), added.kind.as_str(), added.indexed),
            (None, "lsp:function", false)
        );
        assert_eq!((added.l0, added.l1), (11, 13));
        assert_eq!(response.entities, 3);
    }

    #[test]
    fn unindexed_file_outline_not_indexed_reason() {
        let (tmp, index) = fixture_index();
        std::fs::write(tmp.path().join("src/new.rs"), "pub fn fresh() {}\n").expect("write");
        let servers = with_outline(
            tmp.path(),
            "src/new.rs",
            vec![symbol("fresh", SymbolKind::FUNCTION, (0, 7), 0)],
        );
        let pool = servers.pool();
        let response = explore_with(&index, tmp.path(), &settings(), Some(&pool), "src/new.rs");
        assert_eq!(
            (response.resolution, response.file.as_deref()),
            ("not_indexed", Some("src/new.rs"))
        );
        assert_eq!(response.outline.reason, Some("not_indexed"));
        assert_eq!(row(&response, "fresh").kind, "lsp:function");
        assert_eq!(response.language.as_deref(), Some("rust"));
    }

    #[test]
    fn excluded_or_outside_repo_file_has_no_outline() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        for requested in ["target/debug/junk.rs", "../outside.rs", "src/missing.rs"] {
            let response = explore_with(&index, tmp.path(), &settings(), Some(&pool), requested);
            assert_eq!(response.resolution, "not_found", "{requested}");
            assert_eq!(response.outline.provenance, "index", "{requested}");
            assert!(response.groups.is_empty(), "{requested}");
        }
    }

    #[test]
    fn explore_v2_shape_and_render_columns() {
        let (tmp, index) = fixture_index();
        let pool = FakeServers::new(fixture_target_script(tmp.path())).pool();
        let value = serde_json::to_value(explore_with(
            &index,
            tmp.path(),
            &settings(),
            Some(&pool),
            "src/lib.rs",
        ))
        .expect("json");
        for key in ["outline", "lsp", "groups", "notes"] {
            assert!(value.get(key).is_some(), "{key} missing");
        }
        let target = &value["groups"][0]["entities"][0];
        for key in ["id", "referenced_by", "refs_provenance", "indexed"] {
            assert!(target.get(key).is_some(), "{key} missing in {target}");
        }
        let doc = explore_doc(&value, OutputFormat::Table);
        assert!(
            doc.summary
                .iter()
                .any(|line| line.starts_with("language server: ok — 2 asked, 2 answered")),
            "{:?}",
            doc.summary
        );
        assert_eq!(doc.sections[0].rows[0][4], "L");
    }
}
