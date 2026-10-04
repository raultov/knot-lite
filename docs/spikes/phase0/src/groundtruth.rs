//! knot ground truth: S1 target files, snapshot validation (T2.3) and the
//! callers-JSON filtering rules (T2.4).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use knot::models::ParsedEntity;

use crate::common::EntityKey;
use crate::entities::{EntityTable, SnapshotEntity};

/// Relationship groups that are not incoming "callers" of the target.
///
/// `overrides` points outward (what the target itself overrides) and
/// `declared_unused` is not a call/reference edge at all.
const EXCLUDED_GROUPS: &[&str] = &["overrides", "declared_unused"];

/// One S1 comparison target, as stored in `targets/*.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub id: String,
    pub name: String,
    /// Repo-relative source file.
    pub file: String,
    /// knot `start_line` (1-based); `0` means "TBD", resolved by validation.
    pub line: usize,
    pub knot_query: String,
    pub category: String,
    pub ops: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

/// The `targets/*.toml` file: one snapshot plus its comparison targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsFile {
    /// knot repo name; also `ParseConfig.repo_name`.
    pub repo: String,
    pub lang: crate::common::Lang,
    /// Snapshot root (sandbox copy).
    pub root: String,
    #[serde(rename = "target")]
    pub targets: Vec<Target>,
}

impl TargetsFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading targets file {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// One caller row of knot's `callers` JSON (fields the spike needs).
#[derive(Debug, Clone, Deserialize)]
struct KnotCallerRow {
    file_path: String,
    #[serde(default)]
    kind: String,
    name: String,
    #[serde(default)]
    rel_type: String,
    repo_name: String,
    start_line: usize,
    target_file_path: String,
    #[serde(default)]
    target_start_line: usize,
}

/// A caller of the target that exists in the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnotCaller {
    /// `(file_path, start_line, name)` of the caller.
    pub key: EntityKey,
    /// knot wire kind of the caller.
    pub kind: String,
    /// JSON key the row came from (e.g. `calls`).
    pub rel_group: String,
    /// e.g. `CALLS`, `REFERENCES`.
    pub rel_type: String,
}

/// knot's ground truth after applying the T2.4 rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilteredGroundTruth {
    pub target_id: String,
    pub callers: Vec<KnotCaller>,
    /// Rows whose target `(file, line)` is not our target.
    pub dropped_other_target: usize,
    /// Callers absent from the snapshot (stale index rows).
    pub dropped_stale_caller: usize,
    /// Callers from another repo (invisible to LSP).
    pub dropped_cross_repo: usize,
    /// Excluded relationship groups with their row counts.
    pub excluded_groups: BTreeMap<String, usize>,
}

/// Load one `knot callers` JSON file and apply the T2.4 filtering rules.
///
/// Check order matters for the counters: a row is attributed to at most one
/// drop reason, and cross-repo is checked before snapshot membership (a
/// cross-repo caller is trivially absent from the snapshot).
///
/// knot's real JSON also carries a `resolution` metadata object (query tier,
/// resolved targets, truncation) alongside the relationship groups; it is not
/// a row list and is skipped here.
pub fn load_and_filter(
    path: &Path,
    target: &Target,
    table: &EntityTable,
) -> Result<FilteredGroundTruth> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading ground truth {}", path.display()))?;
    let groups: BTreeMap<String, serde_json::Value> = serde_json::from_str(&text)
        .with_context(|| format!("parsing ground truth {}", path.display()))?;

    let mut out = FilteredGroundTruth {
        target_id: target.id.clone(),
        callers: Vec::new(),
        dropped_other_target: 0,
        dropped_stale_caller: 0,
        dropped_cross_repo: 0,
        excluded_groups: BTreeMap::new(),
    };

    for (group, value) in groups {
        if !value.is_array() {
            continue;
        }
        let rows: Vec<KnotCallerRow> = serde_json::from_value(value)
            .with_context(|| format!("parsing group {group} of {}", path.display()))?;
        if EXCLUDED_GROUPS.contains(&group.as_str()) {
            out.excluded_groups.insert(group, rows.len());
            continue;
        }
        for row in rows {
            if row.target_file_path != target.file || row.target_start_line != target.line {
                out.dropped_other_target += 1;
                continue;
            }
            if row.repo_name != table.repo {
                out.dropped_cross_repo += 1;
                continue;
            }
            let key = EntityKey {
                file: row.file_path,
                line: row.start_line,
                name: row.name,
            };
            if table.get(&key).is_none() {
                out.dropped_stale_caller += 1;
                continue;
            }
            out.callers.push(KnotCaller {
                key,
                kind: row.kind,
                rel_group: group.clone(),
                rel_type: row.rel_type,
            });
        }
    }
    Ok(out)
}

/// Outcome of validating one target against the snapshot table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetStatus {
    /// Stored line already matches a snapshot entity.
    Ok,
    /// TBD or wrong line, resolved to `resolved_line`.
    Fixed,
    /// No snapshot entity with that name in that file.
    Missing,
}

impl TargetStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fixed => "fixed",
            Self::Missing => "missing",
        }
    }
}

/// One row of the `validate-targets` table (T2.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetValidation {
    pub id: String,
    pub status: TargetStatus,
    pub resolved_line: usize,
    pub kind: String,
    pub fqn: String,
    pub uuid: String,
    pub warnings: Vec<String>,
}

/// Validate every target of a targets file against the snapshot table.
///
/// `parsed` is only consulted to disambiguate overloads via the `notes`
/// signature hints (T2.3 step 2); the table alone has no signatures.
pub fn validate_targets(
    file: &TargetsFile,
    table: &EntityTable,
    parsed: &[ParsedEntity],
) -> Vec<TargetValidation> {
    let mut signatures: BTreeMap<Uuid, &str> = BTreeMap::new();
    for entity in parsed {
        if let Some(signature) = entity.signature.as_deref() {
            signatures.insert(entity.uuid, signature);
        }
    }
    file.targets
        .iter()
        .map(|target| resolve_target(target, table, &signatures))
        .collect()
}

/// Build the snapshot table for `path`'s repo and validate its targets.
///
/// With `write`, resolved lines are rewritten into the TOML (comments and
/// field order preserved). `include_config_files` is the T3.1 fidelity knob.
pub fn validate_targets_file(
    path: &Path,
    write: bool,
    include_config_files: bool,
) -> Result<Vec<TargetValidation>> {
    let file = TargetsFile::load(path)?;
    let root = Path::new(&file.root);
    let (table, parsed) = EntityTable::build_with(&file.repo, root, include_config_files)?;
    eprintln!(
        "# repo={} entities={} files={} root={}",
        file.repo,
        table.entities.len(),
        table.file_count(),
        file.root
    );
    let rows = validate_targets(&file, &table, &parsed);
    if write {
        let resolved: Vec<(String, usize)> = rows
            .iter()
            .filter(|row| row.status == TargetStatus::Fixed && row.resolved_line > 0)
            .map(|row| (row.id.clone(), row.resolved_line))
            .collect();
        if !resolved.is_empty() {
            rewrite_target_lines(path, &resolved)?;
        }
    }
    Ok(rows)
}

/// Render the T2.3 result table.
pub fn format_validation_table(rows: &[TargetValidation]) -> String {
    let mut out = String::from("id | status | line | kind | fqn | uuid\n");
    for row in rows {
        out.push_str(&format!(
            "{} | {} | {} | {} | {} | {}\n",
            row.id,
            row.status.as_str(),
            row.resolved_line,
            row.kind,
            row.fqn,
            row.uuid
        ));
        for warning in &row.warnings {
            out.push_str(&format!("  ! {warning}\n"));
        }
    }
    out
}

/// Rewrite `line = N` of the given targets in place, preserving the rest.
pub fn rewrite_target_lines(path: &Path, resolved: &[(String, usize)]) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading targets file {}", path.display()))?;
    let updates: BTreeMap<&str, usize> = resolved
        .iter()
        .map(|(id, line)| (id.as_str(), *line))
        .collect();

    let mut out = String::with_capacity(text.len());
    let mut block: Vec<&str> = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("[[target]]") && !block.is_empty() {
            append_rewritten(&mut out, &block, &updates);
            block.clear();
        }
        block.push(line);
    }
    if !block.is_empty() {
        append_rewritten(&mut out, &block, &updates);
    }

    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

fn append_rewritten(out: &mut String, block: &[&str], updates: &BTreeMap<&str, usize>) {
    let id = block
        .iter()
        .find_map(|line| toml_string_field(line.trim_start(), "id"));
    let new_line = id.and_then(|id| updates.get(id).copied());
    for line in block {
        match new_line {
            Some(value) if is_line_assignment(line.trim_start()) => {
                out.push_str("line = ");
                out.push_str(&value.to_string());
                out.push('\n');
            }
            _ => out.push_str(line),
        }
    }
}

/// How a target line was resolved — kept for accurate warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// `at 45` / `hint: 45` style note hint (exact start line first).
    LineHint,
    /// Unique signature match for a backtick hint such as `` `call(Prompt)` ``.
    SignatureHint,
    /// No usable hint: the first candidate by line.
    FirstCandidate,
}

/// Resolve one target: exact line first, then name + `notes` hints.
fn resolve_target(
    target: &Target,
    table: &EntityTable,
    signatures: &BTreeMap<Uuid, &str>,
) -> TargetValidation {
    let key = EntityKey {
        file: target.file.clone(),
        line: target.line,
        name: target.name.clone(),
    };
    if target.line > 0
        && let Some(entity) = table.get(&key)
    {
        return validation_row(target, TargetStatus::Ok, entity, Vec::new());
    }

    let candidates: Vec<&SnapshotEntity> = table
        .entities_in(&target.file)
        .iter()
        .map(|&idx| &table.entities[idx])
        .filter(|entity| entity.key.name == target.name)
        .collect();

    let Some((entity, pick)) = pick_candidate(&candidates, &target.notes, signatures) else {
        return TargetValidation {
            id: target.id.clone(),
            status: TargetStatus::Missing,
            resolved_line: 0,
            kind: String::new(),
            fqn: String::new(),
            uuid: String::new(),
            warnings: Vec::new(),
        };
    };

    let mut warnings = Vec::new();
    if target.line == 0 {
        warnings.push(format!(
            "TBD resolved to line {} ({})",
            entity.key.line, entity.fqn
        ));
    } else if target.line != entity.key.line {
        warnings.push(format!(
            "stored line {} did not match; resolved to {}",
            target.line, entity.key.line
        ));
    }
    if candidates.len() > 1 && pick == Pick::FirstCandidate {
        let lines: Vec<String> = candidates.iter().map(|c| c.key.line.to_string()).collect();
        warnings.push(format!(
            "ambiguous name: {} candidates at lines {}; picked the first (no unique notes hint)",
            candidates.len(),
            lines.join(", ")
        ));
    }
    validation_row(target, TargetStatus::Fixed, entity, warnings)
}

fn validation_row(
    target: &Target,
    status: TargetStatus,
    entity: &SnapshotEntity,
    warnings: Vec<String>,
) -> TargetValidation {
    TargetValidation {
        id: target.id.clone(),
        status,
        resolved_line: entity.key.line,
        kind: entity.kind.clone(),
        fqn: entity.fqn.clone(),
        uuid: entity.uuid.to_string(),
        warnings,
    }
}

/// Candidates are ordered by line; the `notes` hints decide: line hint first
/// (exact start line, then span), then signature tokens of a backtick hint,
/// else the first candidate.
fn pick_candidate<'a>(
    candidates: &[&'a SnapshotEntity],
    notes: &str,
    signatures: &BTreeMap<Uuid, &str>,
) -> Option<(&'a SnapshotEntity, Pick)> {
    if let Some(hint) = note_line_hint(notes) {
        let exact = candidates.iter().find(|c| c.key.line == hint);
        let spanning = candidates
            .iter()
            .find(|c| c.key.line <= hint && hint <= c.end_line);
        if let Some(found) = exact.or(spanning) {
            return Some((found, Pick::LineHint));
        }
    }
    if let Some(found) = pick_by_signature(candidates, notes, signatures) {
        return Some((found, Pick::SignatureHint));
    }
    candidates.first().map(|c| (*c, Pick::FirstCandidate))
}

/// Unique candidate whose signature carries every token of a backtick hint.
///
/// Tokens keep their case on purpose: in `` `call(Prompt)` `` the type
/// `Prompt` must not match a parameter named `prompt`.
fn pick_by_signature<'a>(
    candidates: &[&'a SnapshotEntity],
    notes: &str,
    signatures: &BTreeMap<Uuid, &str>,
) -> Option<&'a SnapshotEntity> {
    for hint in note_backtick_hints(notes) {
        let hint_tokens = signature_tokens(&hint);
        if hint_tokens.is_empty() {
            continue;
        }
        let matching: Vec<&&SnapshotEntity> = candidates
            .iter()
            .filter(|cand| {
                signatures
                    .get(&cand.uuid)
                    .is_some_and(|sig| contains_all_tokens(sig, &hint_tokens))
            })
            .collect();
        if matching.len() == 1 {
            return matching.first().map(|c| **c);
        }
    }
    None
}

/// `at 45` / `hint: 45` style line hints in the free-text notes.
fn note_line_hint(notes: &str) -> Option<usize> {
    for marker in ["at ", "hint: "] {
        for (idx, _) in notes.match_indices(marker) {
            if let Some(value) = parse_usize(&notes[idx + marker.len()..]) {
                return Some(value);
            }
        }
    }
    None
}

fn parse_usize(text: &str) -> Option<usize> {
    let digits: String = text
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Spans enclosed in backticks, e.g. `` `call(Prompt)` ``.
fn note_backtick_hints(notes: &str) -> Vec<String> {
    let mut hints = Vec::new();
    let mut rest = notes;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        hints.push(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }
    hints
}

/// Case-preserving alphanumeric tokens of a signature fragment.
fn signature_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn contains_all_tokens(signature: &str, tokens: &[String]) -> bool {
    let haystack = signature_tokens(signature);
    tokens.iter().all(|token| haystack.contains(token))
}

fn is_line_assignment(line: &str) -> bool {
    line.strip_prefix("line")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn toml_string_field<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(field)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::EntityTable;
    use knot::models::EntityKind;
    use std::path::PathBuf;

    fn entity(file: &str, line: usize, name: &str, signature: Option<&str>) -> ParsedEntity {
        ParsedEntity::new(
            name,
            EntityKind::RustFunction,
            format!("fqn::{name}"),
            signature.map(str::to_owned),
            None,
            "rust",
            file,
            line,
            line + 10,
            None,
            "repo",
        )
    }

    fn table_of(parsed: &[ParsedEntity]) -> EntityTable {
        EntityTable::from_parsed("repo", PathBuf::from("/snap"), parsed)
    }

    fn target(id: &str, name: &str, file: &str, line: usize, notes: &str) -> Target {
        Target {
            id: id.to_owned(),
            name: name.to_owned(),
            file: file.to_owned(),
            line,
            knot_query: name.to_owned(),
            category: "free_fn".to_owned(),
            ops: vec!["incoming".to_owned()],
            notes: notes.to_owned(),
        }
    }

    fn targets_of(targets: Vec<Target>) -> TargetsFile {
        TargetsFile {
            repo: "repo".to_owned(),
            lang: crate::common::Lang::Rust,
            root: "/snap".to_owned(),
            targets,
        }
    }

    /// A `knot callers` JSON fixture exercising every T2.4 rule:
    /// kept, other-target, cross-repo, stale, `overrides`, `overridden_by`,
    /// `declared_unused`, plus the `resolution` metadata object the real CLI
    /// emits (not rows — must be skipped, never crash the filter).
    const FIXTURE: &str = r#"{
      "resolution": {
        "fuzzy": false,
        "hidden_kinds": [],
        "hidden_non_code": 0,
        "kind_filter": "any",
        "query": "parse_files",
        "targets": [
          {
            "file_path": "src/t.rs", "fqn": "fqn::parse_files",
            "kind": "rust_function", "name": "parse_files",
            "repo_name": "repo", "start_line": 210, "uuid": "0000"
          }
        ],
        "tier": "exact_name",
        "total_targets": 1,
        "truncated": false
      },
      "calls": [
        {
          "file_path": "src/caller.rs", "kind": "rust_function", "name": "kept_caller",
          "rel_type": "CALLS", "repo_name": "repo", "start_line": 5,
          "target_file_path": "src/t.rs", "target_start_line": 210
        },
        {
          "file_path": "src/caller.rs", "kind": "rust_function", "name": "other_target",
          "rel_type": "CALLS", "repo_name": "repo", "start_line": 6,
          "target_file_path": "src/t.rs", "target_start_line": 999
        },
        {
          "file_path": "src/foreign.rs", "kind": "rust_function", "name": "cross_repo",
          "rel_type": "CALLS", "repo_name": "other", "start_line": 1,
          "target_file_path": "src/t.rs", "target_start_line": 210
        },
        {
          "file_path": "src/gone.rs", "kind": "rust_function", "name": "stale_caller",
          "rel_type": "CALLS", "repo_name": "repo", "start_line": 3,
          "target_file_path": "src/t.rs", "target_start_line": 210
        }
      ],
      "references": [
        {
          "file_path": "src/caller.rs", "kind": "rust_function", "name": "kept_caller",
          "rel_type": "REFERENCES", "repo_name": "repo", "start_line": 5,
          "target_file_path": "src/t.rs", "target_start_line": 210
        }
      ],
      "overridden_by": [
        {
          "file_path": "src/caller.rs", "kind": "rust_method", "name": "kept_caller",
          "rel_type": "OVERRIDDEN_BY", "repo_name": "repo", "start_line": 5,
          "target_file_path": "src/t.rs", "target_start_line": 210
        }
      ],
      "overrides": [
        {
          "file_path": "src/caller.rs", "kind": "rust_method", "name": "kept_caller",
          "rel_type": "OVERRIDES", "repo_name": "repo", "start_line": 5,
          "target_file_path": "src/t.rs", "target_start_line": 210
        }
      ],
      "declared_unused": [
        {
          "file_path": "src/caller.rs", "kind": "rust_function", "name": "kept_caller",
          "rel_type": "DECLARED_UNUSED", "repo_name": "repo", "start_line": 5,
          "target_file_path": "src/t.rs", "target_start_line": 210
        }
      ]
    }"#;

    #[test]
    fn load_and_filter_applies_every_rule() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("callers_repo_rust-01.json");
        std::fs::write(&path, FIXTURE).expect("write fixture");

        let parsed = vec![
            entity("src/t.rs", 210, "parse_files", None),
            entity("src/caller.rs", 5, "kept_caller", None),
        ];
        let table = table_of(&parsed);
        let target = target("rust-01", "parse_files", "src/t.rs", 210, "");

        let gt = load_and_filter(&path, &target, &table).expect("filter");
        assert_eq!(gt.target_id, "rust-01");
        // calls + references + overridden_by rows of the snapshot caller.
        assert_eq!(gt.callers.len(), 3);
        assert_eq!(gt.dropped_other_target, 1);
        assert_eq!(gt.dropped_cross_repo, 1);
        assert_eq!(gt.dropped_stale_caller, 1);
        assert_eq!(gt.excluded_groups.get("overrides"), Some(&1));
        assert_eq!(gt.excluded_groups.get("declared_unused"), Some(&1));
        assert!(!gt.excluded_groups.contains_key("overridden_by"));
    }

    #[test]
    fn validate_resolves_tbd_by_notes_line_hint() {
        // The 40-line entity's span also contains 45; the exact-start hint wins.
        let parsed = vec![
            entity("src/t.rs", 40, "call", None),
            entity("src/t.rs", 45, "call", None),
        ];
        let table = table_of(&parsed);
        let file = targets_of(vec![target(
            "j01",
            "call",
            "src/t.rs",
            0,
            "hint: abstract at 45",
        )]);
        let rows = validate_targets(&file, &table, &parsed);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, TargetStatus::Fixed);
        assert_eq!(rows[0].resolved_line, 45);
    }

    #[test]
    fn validate_resolves_overload_by_signature_hint() {
        let parsed = vec![
            entity("src/t.rs", 40, "call", Some("void call(String prompt)")),
            entity("src/t.rs", 45, "call", Some("Prompt call(Prompt prompt)")),
        ];
        let table = table_of(&parsed);
        let file = targets_of(vec![target(
            "j02",
            "call",
            "src/t.rs",
            0,
            "abstract overload `call(Prompt)`",
        )]);
        let rows = validate_targets(&file, &table, &parsed);
        assert_eq!(rows[0].resolved_line, 45);
    }

    #[test]
    fn validate_warns_when_ambiguous_without_hints() {
        let parsed = vec![
            entity("src/t.rs", 40, "call", Some("void call(String a)")),
            entity("src/t.rs", 45, "call", Some("void call(int b)")),
        ];
        let table = table_of(&parsed);
        let file = targets_of(vec![target("j03", "call", "src/t.rs", 0, "3 overloads")]);
        let rows = validate_targets(&file, &table, &parsed);
        assert_eq!(rows[0].resolved_line, 40);
        assert!(rows[0].warnings.iter().any(|w| w.contains("ambiguous")));
    }

    #[test]
    fn validate_reports_missing_and_ok() {
        let parsed = vec![entity("src/t.rs", 210, "parse_files", None)];
        let table = table_of(&parsed);
        let file = targets_of(vec![
            target("rust-01", "parse_files", "src/t.rs", 210, ""),
            target("rust-02", "nope", "src/t.rs", 0, ""),
        ]);
        let rows = validate_targets(&file, &table, &parsed);
        assert_eq!(rows[0].status, TargetStatus::Ok);
        assert_eq!(rows[1].status, TargetStatus::Missing);
        assert_eq!(rows[1].resolved_line, 0);
        assert!(format_validation_table(&rows).contains("missing"));
    }

    #[test]
    fn rewrite_updates_only_matching_target_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.toml");
        let original = r#"repo = "knot"
lang = "rust"
root = "/snap"

[[target]]
id = "rust-01"
name = "parse_files"
file = "src/t.rs"
line = 0
notes = "keep me"

[[target]]
id = "rust-02"
name = "other"
file = "src/u.rs"
line = 0
"#;
        std::fs::write(&path, original).expect("write toml");
        rewrite_target_lines(&path, &[("rust-01".to_owned(), 210)]).expect("rewrite");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains(
            "id = \"rust-01\"\nname = \"parse_files\"\nfile = \"src/t.rs\"\nline = 210\nnotes = \"keep me\""
        ));
        assert!(
            text.contains("id = \"rust-02\"\nname = \"other\"\nfile = \"src/u.rs\"\nline = 0\n")
        );
    }

    #[test]
    fn note_hints_are_parsed() {
        assert_eq!(note_line_hint("hint: `create(ChatModel)` at 63"), Some(63));
        assert_eq!(note_line_hint("hint: 255"), Some(255));
        assert_eq!(note_line_hint("3 overloads; target the no-arg one"), None);
        assert_eq!(
            note_backtick_hints("target the `SearchRequest` overload"),
            vec!["SearchRequest".to_owned()]
        );
        assert!(note_backtick_hints("no hints here").is_empty());
        // Case-sensitive: the type `Prompt` never matches parameter `prompt`.
        assert!(contains_all_tokens(
            "Prompt call(Prompt prompt)",
            &signature_tokens("call(Prompt)")
        ));
        assert!(!contains_all_tokens(
            "void call(String prompt)",
            &signature_tokens("call(Prompt)")
        ));
    }
}
