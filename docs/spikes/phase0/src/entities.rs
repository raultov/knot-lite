//! Snapshot entity table: parse a sandbox copy with knot (no database) and
//! build the lookups used by ground-truth filtering, target validation and
//! LSP location normalisation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use knot::models::ParsedEntity;
use knot::pipeline::parser::{ParseConfig, parse_files_stream};

use crate::common::EntityKey;

/// Directory names that must never contribute entities to a snapshot.
///
/// The sandbox snapshots have no `.git` directory, and the `ignore` crate only
/// honours `.gitignore` inside a git repository, so ignored build output could
/// leak into `discover_files`. The deny-list is the explicit guard the plan
/// asks for (T3.1); anything it filters is logged.
const DENY_LISTED_DIRS: &[&str] = &["node_modules", "target", "dist", ".knot"];

/// One entity of a repository snapshot, as the spike sees it.
#[derive(Debug, Clone)]
pub struct SnapshotEntity {
    /// Deterministic knot UUID (v5 over `repo:file:fqn:start_line`).
    pub uuid: Uuid,
    /// Stable identity: repo-relative file, 1-based start line, name.
    pub key: EntityKey,
    /// 1-based last line of the declaration span.
    pub end_line: usize,
    /// `EntityKind` wire label (its `Display` form, e.g. `rust_function`).
    pub kind: String,
    pub fqn: String,
    pub language: String,
    /// Whether this entity can own other entities (see [`is_container`]).
    pub is_container: bool,
}

/// Fast lookups over every entity of one snapshot.
pub struct EntityTable {
    pub repo: String,
    pub root: PathBuf,
    /// Sorted by `(file, start_line, end_line desc, name)`.
    pub entities: Vec<SnapshotEntity>,
    by_file: HashMap<String, Vec<usize>>,
    by_uuid: HashMap<Uuid, usize>,
    by_key: HashMap<EntityKey, usize>,
}

impl EntityTable {
    /// Parse the whole snapshot with knot (no DB) and build the lookups.
    ///
    /// Mirrors how knot-server indexed the snapshot so UUIDs and line numbers
    /// match: `discover_files` (config files off) → parse → `prepare_entities`.
    ///
    /// Blocking and thread-spawning; call through `spawn_blocking` from async.
    pub fn build(repo: &str, root: &Path) -> Result<(Self, Vec<ParsedEntity>)> {
        Self::build_with(repo, root, false)
    }

    /// [`Self::build`] with an explicit `include_config_files` knob (the T3.1
    /// fidelity experiment compares both settings against knot's counts).
    pub fn build_with(
        repo: &str,
        root: &Path,
        include_config_files: bool,
    ) -> Result<(Self, Vec<ParsedEntity>)> {
        let root = std::fs::canonicalize(root)
            .with_context(|| format!("canonicalizing snapshot root {}", root.display()))?;
        let root_str = root.to_string_lossy().into_owned();
        let mut files = knot::pipeline::input::discover_files(&root_str, include_config_files)
            .with_context(|| format!("discovering files under {root_str}"))?;
        files.retain(|path| !is_deny_listed(path));
        let cfg = ParseConfig {
            repo_root: root.clone(),
            custom_queries_path: None,
            repo_name: repo.to_owned(),
            include_config_files,
            repo_path: Some(root_str),
        };
        let mut parsed = parse_snapshot(&files, &cfg);
        knot::pipeline::prepare::prepare_entities(&mut parsed);
        let table = Self::from_parsed(repo, root, &parsed);
        Ok((table, parsed))
    }

    /// Build lookups from already-parsed entities.
    pub fn from_parsed(repo: &str, root: PathBuf, parsed: &[ParsedEntity]) -> Self {
        let mut entities: Vec<SnapshotEntity> = parsed.iter().map(snapshot_entity).collect();
        entities.sort_by(cmp_snapshot);

        let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_uuid: HashMap<Uuid, usize> = HashMap::new();
        let mut by_key: HashMap<EntityKey, usize> = HashMap::new();
        for (idx, entity) in entities.iter().enumerate() {
            by_file
                .entry(entity.key.file.clone())
                .or_default()
                .push(idx);
            by_uuid.entry(entity.uuid).or_insert(idx);
            by_key.entry(entity.key.clone()).or_insert(idx);
        }

        Self {
            repo: repo.to_owned(),
            root,
            entities,
            by_file,
            by_uuid,
            by_key,
        }
    }

    /// Entity with the exact `(file, start_line, name)` identity.
    pub fn get(&self, key: &EntityKey) -> Option<&SnapshotEntity> {
        self.by_key.get(key).map(|&idx| &self.entities[idx])
    }

    /// Entity with the given knot UUID.
    pub fn by_uuid(&self, id: &Uuid) -> Option<&SnapshotEntity> {
        self.by_uuid.get(id).map(|&idx| &self.entities[idx])
    }

    /// Innermost entity whose `[start_line, end_line]` contains `line` (1-based).
    pub fn innermost_at(&self, file: &str, line: usize) -> Option<&SnapshotEntity> {
        self.by_file
            .get(file)?
            .iter()
            .map(|&idx| &self.entities[idx])
            .filter(|e| e.key.line <= line && line <= e.end_line)
            .min_by_key(|e| (e.end_line - e.key.line, std::cmp::Reverse(e.key.line)))
    }

    /// Innermost *container* entity containing `line`; falls back to the
    /// synthetic file-level key `EntityKey { file, line: 0, name: "<file>" }`
    /// when the line is not inside any container.
    pub fn container_at(&self, file: &str, line: usize) -> EntityKey {
        self.by_file
            .get(file)
            .into_iter()
            .flatten()
            .map(|&idx| &self.entities[idx])
            .filter(|e| e.is_container && e.key.line <= line && line <= e.end_line)
            .min_by_key(|e| (e.end_line - e.key.line, std::cmp::Reverse(e.key.line)))
            .map(|e| e.key.clone())
            .unwrap_or_else(|| EntityKey {
                file: file.to_owned(),
                line: 0,
                name: "<file>".to_owned(),
            })
    }

    /// Indices (into `entities`) of every entity declared in `file`.
    pub fn entities_in(&self, file: &str) -> &[usize] {
        self.by_file
            .get(file)
            .map(|idxs| idxs.as_slice())
            .unwrap_or_default()
    }

    /// Number of distinct files contributing entities.
    pub fn file_count(&self) -> usize {
        self.by_file.len()
    }
}

/// Whether the kind label denotes a construct that can own other entities.
///
/// Matching is substring-based on the wire label; the list starts from the one
/// the plan (T3.2) prescribes. Deviation: `union` was added so the Rust
/// `rust_union` kind (a struct-like container) is classified deliberately too.
/// The unit tests pin the classification of every rust/ts/java wire label so
/// the list stays deliberate.
pub fn is_container(kind_wire: &str, name: &str) -> bool {
    if name == "<module>" {
        return true;
    }
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
    MARKERS.iter().any(|marker| kind_wire.contains(marker))
}

/// Parse every file the way knot's own pipeline does.
///
/// Deviation from the plan (which prescribed `knot::pipeline::parser::parse_files`):
/// that convenience wrapper deadlocks on any snapshot with more entities than
/// the channel capacity (1024) — it `blocking_send`s into an undrained channel
/// and only drains after the producer returns. knot's production path
/// (`pipeline::runner`) drives `parse_files_stream` with a concurrent consumer;
/// the spike does the same, on plain threads so no async context is required.
fn parse_snapshot(files: &[PathBuf], cfg: &ParseConfig) -> Vec<ParsedEntity> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());

    std::thread::scope(|scope| {
        let consumer = scope.spawn(move || {
            let mut out = Vec::new();
            while let Some(entity) = rx.blocking_recv() {
                out.push(entity);
            }
            out
        });
        let producer = scope.spawn(move || {
            parse_files_stream(files, cfg, tx, cpus, None);
        });
        producer.join().expect("parse producer thread");
        consumer.join().expect("parse consumer thread")
    })
}

/// True for directory names the deny-list rejects.
fn is_deny_listed(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| DENY_LISTED_DIRS.contains(&name))
    })
}

fn snapshot_entity(entity: &ParsedEntity) -> SnapshotEntity {
    let kind = entity.kind.to_string();
    SnapshotEntity {
        uuid: entity.uuid,
        key: EntityKey {
            file: entity.file_path.clone(),
            line: entity.start_line,
            name: entity.name.clone(),
        },
        end_line: entity.end_line,
        is_container: is_container(&kind, &entity.name),
        kind,
        fqn: entity.fqn.clone(),
        language: entity.language.clone(),
    }
}

/// `(file, start_line, end_line desc, name)` — outer entities first, so a
/// deterministic order backs every lookup and result file.
fn cmp_snapshot(a: &SnapshotEntity, b: &SnapshotEntity) -> std::cmp::Ordering {
    a.key
        .file
        .cmp(&b.key.file)
        .then(a.key.line.cmp(&b.key.line))
        .then(b.end_line.cmp(&a.end_line))
        .then(a.key.name.cmp(&b.key.name))
}

/// Wire labels of the three spiked languages — used by the classification
/// tests and by the debug print of `EntityKind::all_wire_kinds()`.
#[cfg(test)]
fn spiked_wire_kinds() -> Vec<(&'static str, bool)> {
    vec![
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
        // TypeScript / JavaScript
        ("class", true),
        ("interface", true),
        ("method", true),
        ("function", true),
        ("enum", true),
        ("constant", false),
        // Java
        // (java shares the generic class/interface/method/function/enum labels)
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::models::EntityKind;

    fn entity(file: &str, line: usize, end: usize, name: &str, kind: EntityKind) -> ParsedEntity {
        ParsedEntity::new(
            name,
            kind,
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

    fn table_of(parsed: &[ParsedEntity]) -> EntityTable {
        EntityTable::from_parsed("repo", PathBuf::from("/snap"), parsed)
    }

    #[test]
    fn container_classification_is_deliberate() {
        for (label, expected) in spiked_wire_kinds() {
            assert_eq!(
                is_container(label, "some_name"),
                expected,
                "classification of {label} changed"
            );
        }
        // Labels outside the three spiked languages, as prescribed by T3.2.
        assert!(is_container("csharp_constructor", "ctor"));
        assert!(is_container("kotlin_companion_object", "Companion"));
        assert!(!is_container("field", "x"));
        assert!(!is_container("variable", "y"));
        assert!(!is_container("config_property", "k"));
    }

    #[test]
    fn every_wire_kind_is_classified_without_panic() {
        // Prints the full label table once (run with --nocapture to inspect).
        for label in EntityKind::all_wire_kinds() {
            println!("{label} -> {}", is_container(label, "name"));
        }
    }

    #[test]
    fn module_name_is_container() {
        assert!(is_container("constant", "<module>"));
    }

    #[test]
    fn lookups_and_sort_order() {
        let parsed = vec![
            entity("src/b.rs", 10, 20, "inner", EntityKind::RustFunction),
            entity("src/a.rs", 1, 50, "outer", EntityKind::RustStruct),
            entity("src/a.rs", 5, 9, "fn_in_struct", EntityKind::RustFunction),
        ];
        let table = table_of(&parsed);

        assert_eq!(table.entities.len(), 3);
        // Sorted by (file, start_line, end_line desc, name).
        assert_eq!(table.entities[0].key.name, "outer");
        assert_eq!(table.entities[1].key.name, "fn_in_struct");
        assert_eq!(table.entities[2].key.name, "inner");

        let key = EntityKey {
            file: "src/a.rs".to_owned(),
            line: 1,
            name: "outer".to_owned(),
        };
        assert!(table.get(&key).is_some());
        assert!(
            table
                .get(&EntityKey {
                    file: "src/a.rs".to_owned(),
                    line: 1,
                    name: "nope".to_owned(),
                })
                .is_none()
        );

        let uuid = table.get(&key).expect("outer exists").uuid;
        assert_eq!(table.by_uuid(&uuid).expect("uuid lookup").key.name, "outer");

        assert_eq!(table.entities_in("src/a.rs").len(), 2);
        assert!(table.entities_in("missing.rs").is_empty());
    }

    #[test]
    fn innermost_prefers_smallest_span() {
        let parsed = vec![
            entity("src/a.rs", 1, 50, "outer", EntityKind::RustStruct),
            entity("src/a.rs", 5, 9, "inner", EntityKind::RustFunction),
        ];
        let table = table_of(&parsed);
        assert_eq!(
            table
                .innermost_at("src/a.rs", 7)
                .map(|e| e.key.name.as_str()),
            Some("inner")
        );
        assert_eq!(
            table
                .innermost_at("src/a.rs", 30)
                .map(|e| e.key.name.as_str()),
            Some("outer")
        );
        assert!(table.innermost_at("src/a.rs", 100).is_none());
    }

    #[test]
    fn container_at_falls_back_to_file_key() {
        let parsed = vec![
            entity("src/a.rs", 1, 50, "outer", EntityKind::RustStruct),
            entity("src/a.rs", 5, 9, "leaf", EntityKind::RustConstant),
        ];
        let table = table_of(&parsed);
        // Line 7 sits inside the non-container `leaf` and the container `outer`.
        assert_eq!(table.container_at("src/a.rs", 7).name, "outer");
        // Line 100 is inside nothing → synthetic file-level key.
        assert_eq!(
            table.container_at("src/a.rs", 100),
            EntityKey {
                file: "src/a.rs".to_owned(),
                line: 0,
                name: "<file>".to_owned(),
            }
        );
    }

    #[test]
    fn deny_list_rejects_build_output_dirs() {
        assert!(is_deny_listed(Path::new("a/node_modules/x.ts")));
        assert!(is_deny_listed(Path::new("target/debug/x.rs")));
        assert!(is_deny_listed(Path::new("m/dist/app.js")));
        assert!(is_deny_listed(Path::new(".knot/index.json")));
        assert!(!is_deny_listed(Path::new("src/targeting.rs")));
        assert!(!is_deny_listed(Path::new("src/lib.rs")));
    }
}
