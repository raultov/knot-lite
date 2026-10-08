//! The manifest (`.knot-lite/manifest.toml`): wire types, atomic writing and
//! the side-effect-free index state reader behind `status`.

use std::path::Path;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::layout::{CommitMode, IndexLayout, MANIFEST_SCHEMA_V1};
use super::record_io::atomic_write;

/// knot version knot-lite is built and measured against (P2-D1). Drift-tested
/// against `Cargo.lock`, so a bump forces a deliberate re-measure.
pub const KNOT_VERSION: &str = "1.11.3";

/// The only embedding encoding this version writes (P2-D5).
pub const ENCODING_I8PV_B64: &str = "i8pv_b64";

/// The only fields [`read_index_state`] reads; unknown fields are ignored so
/// the manifest can grow without breaking `status`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ManifestHeader {
    /// Manifest schema identifier, e.g. [`MANIFEST_SCHEMA_V1`].
    pub schema: String,
    /// knot-lite version that wrote the manifest.
    pub knot_lite_version: String,
}

/// The full manifest. TOML tables must follow scalar fields, so the nested
/// structs come last (the declaration order is the wire order).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest schema identifier ([`MANIFEST_SCHEMA_V1`]).
    pub schema: String,
    /// knot-lite version that wrote the manifest.
    pub knot_lite_version: String,
    /// knot version the index was built with ([`KNOT_VERSION`]).
    pub knot_version: String,
    /// Repository name the entity ids were derived from.
    pub repo_name: String,
    /// First creation time, RFC 3339 UTC with seconds (`2026-10-05T10:00:00Z`).
    pub created_at: String,
    /// Last successful run, same format.
    pub updated_at: String,
    /// 64 hex characters over the inventory (freshness stamp).
    pub source_fingerprint: String,
    /// Whether the index is git-ignored or tracked.
    pub commit_mode: CommitMode,
    /// Embedding model and encoding of every vector in the index.
    pub embedding: EmbeddingSpec,
    /// Record counts of the last run.
    pub counts: IndexCounts,
    /// Per-stage timings of the last run, in milliseconds.
    pub timings_ms: IndexTimings,
    /// Pre-computed LSP call graph manifest (P3-D9).
    #[serde(default)]
    pub lsp: Option<LspManifest>,
}

/// Pre-computed LSP metadata section in manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspManifest {
    /// Schema format version (1).
    pub format: u32,
    /// Policy used (`"auto"`, `"all"`, `"none"`).
    pub policy: String,
    /// Minimum fan-in threshold.
    pub min_fanin: usize,
    /// Maximum targets per language.
    pub max_targets: usize,
    /// Time budget in seconds per language.
    pub budget_secs: u64,
    /// Per-language manifest summaries.
    pub languages: std::collections::BTreeMap<String, LspLangManifest>,
}

/// Pre-computed LSP status for one language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspLangManifest {
    /// Status (`"computed"`, `"reused"`, `"skipped"`, `"failed"`, `"none"`).
    pub status: String,
    /// Executable / server name.
    pub server: String,
    /// Server version from `serverInfo`.
    pub version: String,
    /// Inventory fingerprint of this language's files.
    pub files_fingerprint: String,
    /// Number of targets attempted.
    pub targets_selected: usize,
    /// Number of targets that produced an LSP answer.
    pub targets_ok: usize,
    /// Number of targets that failed or timed out.
    pub targets_failed: usize,
    /// Total pre-computed edges written across all shards.
    pub edges: usize,
    /// Wall-clock milliseconds spent pre-computing for this language.
    pub ms: u64,
    /// Diagnostic explanation when skipped or failed.
    pub diagnostic: String,
}

/// Embedding model and wire encoding of the stored vectors. Lives here (not in
/// `embed/`) because the manifest owns its wire form; `embed` re-exports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingSpec {
    /// Canonical model name (e.g. `AllMiniLML6V2`).
    pub model: String,
    /// Vector dimension.
    pub dim: u32,
    /// Wire encoding ([`ENCODING_I8PV_B64`]).
    pub encoding: String,
    /// Prefix applied to queries before embedding.
    pub query_prefix: String,
    /// Prefix applied to passages before embedding.
    pub passage_prefix: String,
}

/// Record counts written into the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexCounts {
    /// Files in the inventory (parse set plus tier-1 candidates).
    pub files_inventoried: u64,
    /// Files handed to knot's parsers.
    pub files_parsed: u64,
    /// Parsed files that produced at least one entity.
    pub files_with_entities: u64,
    /// Files chunked as tier 1.
    pub tier1_files: u64,
    /// Entity records.
    pub entities: u64,
    /// Entity records without a vector (empty embed text).
    pub entities_without_vector: u64,
    /// Tier-1 `chunk` records.
    pub chunks: u64,
    /// Tier-1 `file` records.
    pub file_vectors: u64,
    /// All shard records.
    pub records: u64,
    /// Resolved reference edges kept.
    pub edges: u64,
    /// Edges dropped because an endpoint is not an indexed entity.
    pub dropped_edges: u64,
    /// Shard files.
    pub shards: u64,
    /// Edge shard files.
    pub edge_shards: u64,
    /// Entities of language-server languages whose name column was located.
    #[serde(default)]
    pub columns_located: u64,
    /// Entities of language-server languages whose name column was not found.
    #[serde(default)]
    pub columns_missing: u64,
    /// Pre-computed LSP edges.
    #[serde(default)]
    pub lsp_edges: u64,
    /// Pre-computed LSP edge shards.
    #[serde(default)]
    pub lsp_edge_shards: u64,
    /// Pre-computed LSP targets.
    #[serde(default)]
    pub lsp_targets: u64,
}

/// Per-stage wall-clock timings, milliseconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexTimings {
    /// File selection and inventory.
    pub select: u64,
    /// knot parsing.
    pub parse: u64,
    /// Embed-text preparation and record building.
    pub prepare: u64,
    /// Reference resolution.
    pub resolve: u64,
    /// Embedding.
    pub embed: u64,
    /// Writing shards, edges, inventory and manifest.
    pub write: u64,
    /// Language-server call graph pre-computation.
    #[serde(default)]
    pub lsp: Option<u64>,
    /// Whole run.
    pub total: u64,
}

/// Index state of a repository, as reported by `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IndexState {
    /// No `.knot-lite/manifest.toml` and no leftovers of an interrupted run.
    Absent,
    /// A manifest this knot-lite version understands.
    Present {
        /// Manifest schema identifier.
        schema: String,
        /// knot-lite version that wrote the manifest.
        knot_lite_version: String,
    },
    /// A manifest written by a future schema.
    Unsupported {
        /// The schema found in the manifest.
        schema: String,
    },
    /// The manifest exists but cannot be read or parsed.
    Invalid {
        /// One-line error message.
        error: String,
    },
    /// No manifest, but a previous manifest or shards exist: an `index` run
    /// was interrupted (P2-D13).
    Incomplete,
}

/// Read the index state from the manifest. Never creates directories or files.
pub fn read_index_state(layout: &IndexLayout) -> IndexState {
    let text = match std::fs::read_to_string(&layout.manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return absent_or_incomplete(layout),
        Err(e) => {
            return IndexState::Invalid {
                error: one_line(&e.to_string()),
            };
        }
    };
    match toml::from_str::<ManifestHeader>(&text) {
        Ok(header) => {
            if header.schema == MANIFEST_SCHEMA_V1 {
                IndexState::Present {
                    schema: header.schema,
                    knot_lite_version: header.knot_lite_version,
                }
            } else {
                IndexState::Unsupported {
                    schema: header.schema,
                }
            }
        }
        Err(e) => IndexState::Invalid {
            error: one_line(&e.to_string()),
        },
    }
}

/// A missing manifest is `Incomplete` when an interrupted run left files behind.
fn absent_or_incomplete(layout: &IndexLayout) -> IndexState {
    if layout.previous_manifest.exists() || layout.shards.exists() {
        IndexState::Incomplete
    } else {
        IndexState::Absent
    }
}

/// Read and parse the full manifest.
pub fn read_manifest(path: &Path) -> anyhow::Result<Manifest> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Serialise `manifest` and write it atomically.
pub fn write_manifest_atomic(path: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let text = toml::to_string(manifest).context("serialising the manifest")?;
    atomic_write(path, text.as_bytes())
}

/// Current UTC time as RFC 3339 with seconds precision and a `Z` suffix.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Collapse an error message to a single line (kept for stable JSON output).
pub(crate) fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Shared fixtures for tests that need a fully populated [`Manifest`].
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A fully populated manifest (`demo`, MiniLM-shaped embedding, non-zero counts).
    pub(crate) fn sample_manifest() -> Manifest {
        Manifest {
            schema: MANIFEST_SCHEMA_V1.to_string(),
            knot_lite_version: "0.2.0".to_string(),
            knot_version: KNOT_VERSION.to_string(),
            repo_name: "demo".to_string(),
            created_at: "2026-10-05T10:00:00Z".to_string(),
            updated_at: "2026-10-05T11:30:00Z".to_string(),
            source_fingerprint: "ab".repeat(32),
            commit_mode: CommitMode::Gitignore,
            embedding: EmbeddingSpec {
                model: "AllMiniLML6V2".to_string(),
                dim: 384,
                encoding: ENCODING_I8PV_B64.to_string(),
                query_prefix: String::new(),
                passage_prefix: String::new(),
            },
            counts: IndexCounts {
                files_inventoried: 9,
                files_parsed: 7,
                files_with_entities: 6,
                tier1_files: 2,
                entities: 40,
                entities_without_vector: 1,
                chunks: 3,
                file_vectors: 2,
                records: 45,
                edges: 12,
                dropped_edges: 1,
                shards: 8,
                edge_shards: 4,
                columns_located: 17,
                columns_missing: 2,
                lsp_edges: 0,
                lsp_edge_shards: 0,
                lsp_targets: 0,
            },
            timings_ms: IndexTimings {
                select: 1,
                parse: 2,
                prepare: 3,
                resolve: 4,
                embed: 5,
                write: 6,
                lsp: None,
                total: 21,
            },
            lsp: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::sample_manifest;
    use super::*;

    fn layout_for(tmp: &tempfile::TempDir) -> IndexLayout {
        IndexLayout::for_repo(tmp.path())
    }

    fn write_manifest(tmp: &tempfile::TempDir, body: &str) {
        let layout = layout_for(tmp);
        std::fs::create_dir_all(&layout.dir).expect("mkdir .knot-lite");
        std::fs::write(&layout.manifest, body).expect("write manifest");
    }

    fn dir_listing(tmp: &tempfile::TempDir) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn absent_when_no_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_index_state(&layout_for(&tmp)), IndexState::Absent);
    }

    #[test]
    fn absent_when_dir_without_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        std::fs::create_dir_all(&layout.dir).expect("mkdir");
        assert_eq!(read_index_state(&layout), IndexState::Absent);
    }

    #[test]
    fn present_for_v1_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Unknown fields and extra tables must be ignored (Phase 2 compatibility).
        write_manifest(
            &tmp,
            "schema = \"knot-lite/manifest/v1\"\nknot_lite_version = \"0.1.0\"\n\n[embedding]\nmodel = \"x\"\n",
        );
        assert_eq!(
            read_index_state(&layout_for(&tmp)),
            IndexState::Present {
                schema: MANIFEST_SCHEMA_V1.to_string(),
                knot_lite_version: "0.1.0".to_string(),
            }
        );
    }

    #[test]
    fn unsupported_for_other_schema() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(
            &tmp,
            "schema = \"knot-lite/manifest/v2\"\nknot_lite_version = \"9.9.9\"\n",
        );
        assert_eq!(
            read_index_state(&layout_for(&tmp)),
            IndexState::Unsupported {
                schema: "knot-lite/manifest/v2".to_string(),
            }
        );
    }

    #[test]
    fn invalid_for_garbage_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(&tmp, "this is not = = toml\n");
        match read_index_state(&layout_for(&tmp)) {
            IndexState::Invalid { error } => {
                assert!(!error.is_empty());
                assert!(!error.contains('\n'));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn invalid_for_missing_schema_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(&tmp, "knot_lite_version = \"0.1.0\"\n");
        match read_index_state(&layout_for(&tmp)) {
            IndexState::Invalid { error } => assert!(error.contains("schema")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn serializes_with_state_tag() {
        let json = serde_json::to_string(&IndexState::Absent).expect("serialize");
        assert_eq!(json, "{\"state\":\"absent\"}");
    }

    #[test]
    fn read_is_side_effect_free() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let before = dir_listing(&tmp);
        let _ = read_index_state(&layout_for(&tmp));
        assert_eq!(before, dir_listing(&tmp));
    }

    #[test]
    fn manifest_round_trips_through_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        let manifest = sample_manifest();
        write_manifest_atomic(&layout.manifest, &manifest).expect("write");
        assert_eq!(read_manifest(&layout.manifest).expect("read"), manifest);
    }

    #[test]
    fn manifest_tables_follow_scalars() {
        let text = toml::to_string(&sample_manifest()).expect("serialise");
        assert!(toml::from_str::<Manifest>(&text).is_ok());
        let position = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle} missing"))
        };
        let commit_mode = position("commit_mode");
        assert!(commit_mode < position("[embedding]"));
        assert!(position("[embedding]") < position("[counts]"));
        assert!(position("[counts]") < position("[timings_ms]"));
    }

    #[test]
    fn counts_written_before_the_column_counters_still_load() {
        let text = toml::to_string(&sample_manifest()).expect("serialise");
        let phase2: String = text
            .lines()
            .filter(|line| !line.starts_with("columns_"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(!phase2.contains("columns_"), "fixture must drop both keys");
        let manifest: Manifest = toml::from_str(&phase2).expect("Phase 2 manifest loads");
        assert_eq!(manifest.counts.columns_located, 0);
        assert_eq!(manifest.counts.columns_missing, 0);
        assert_eq!(manifest.counts.entities, 40, "other counters are kept");
    }

    #[test]
    fn manifest_without_lsp_table_still_loads() {
        let text = toml::to_string(&sample_manifest()).expect("serialise");
        let phase2: String = text
            .lines()
            .filter(|line| !line.starts_with("lsp_") && !line.starts_with("columns_"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(
            !phase2.contains("lsp"),
            "fixture must drop every LSP key: {phase2}"
        );
        let manifest: Manifest = toml::from_str(&phase2).expect("Phase 2 manifest loads");
        assert_eq!(manifest.lsp, None);
        assert_eq!(manifest.timings_ms.lsp, None);
        assert_eq!(
            (
                manifest.counts.lsp_edges,
                manifest.counts.lsp_edge_shards,
                manifest.counts.lsp_targets
            ),
            (0, 0, 0)
        );
        assert_eq!(manifest.counts.entities, 40, "other counters are kept");
    }

    #[test]
    fn manifest_lsp_table_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        let language = |status: &str, edges: usize| LspLangManifest {
            status: status.to_owned(),
            server: "jdtls".to_owned(),
            version: "JDT Language Server 1.61".to_owned(),
            files_fingerprint: "0123456789abcdef".to_owned(),
            targets_selected: 200,
            targets_ok: 198,
            targets_failed: 2,
            edges,
            ms: 412_000,
            diagnostic: String::new(),
        };
        let mut manifest = sample_manifest();
        manifest.counts.lsp_edges = 15_234;
        manifest.counts.lsp_edge_shards = 911;
        manifest.counts.lsp_targets = 200;
        manifest.timings_ms.lsp = Some(412_000);
        manifest.lsp = Some(LspManifest {
            format: 1,
            policy: "auto".to_owned(),
            min_fanin: 20,
            max_targets: 200,
            budget_secs: 600,
            languages: std::collections::BTreeMap::from([
                ("java".to_owned(), language("computed", 15_234)),
                ("rust".to_owned(), language("reused", 0)),
            ]),
        });

        write_manifest_atomic(&layout.manifest, &manifest).expect("write");
        assert_eq!(read_manifest(&layout.manifest).expect("read"), manifest);

        let text = std::fs::read_to_string(&layout.manifest).expect("text");
        assert!(text.find("[timings_ms]") < text.find("[lsp]"), "{text}");
        assert!(text.contains("[lsp.languages.java]"), "{text}");
    }

    #[test]
    fn full_manifest_is_present_for_phase1_reader() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        write_manifest_atomic(&layout.manifest, &sample_manifest()).expect("write");
        assert_eq!(
            read_index_state(&layout),
            IndexState::Present {
                schema: MANIFEST_SCHEMA_V1.to_string(),
                knot_lite_version: "0.2.0".to_string(),
            }
        );
    }

    #[test]
    fn extra_fields_are_ignored_by_full_reader() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        let mut text = toml::to_string(&sample_manifest()).expect("serialise");
        text = text.replacen("commit_mode", "future_scalar = 1\ncommit_mode", 1);
        text.push_str("\n[future_table]\nkey = \"value\"\n");
        write_manifest(&tmp, &text);
        assert_eq!(
            read_manifest(&layout.manifest).expect("read"),
            sample_manifest()
        );
    }

    #[test]
    fn incomplete_when_prev_manifest_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        std::fs::create_dir_all(&layout.dir).expect("mkdir");
        std::fs::write(&layout.previous_manifest, "schema = \"x\"\n").expect("write prev");
        assert_eq!(read_index_state(&layout), IndexState::Incomplete);
    }

    #[test]
    fn incomplete_when_shards_without_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        std::fs::create_dir_all(&layout.shards).expect("mkdir shards");
        assert_eq!(read_index_state(&layout), IndexState::Incomplete);
        assert_eq!(
            serde_json::to_string(&IndexState::Incomplete).expect("json"),
            "{\"state\":\"incomplete\"}"
        );
    }

    #[test]
    fn absent_when_only_gitignore_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        std::fs::create_dir_all(&layout.dir).expect("mkdir");
        std::fs::write(&layout.gitignore, "*\n").expect("write");
        assert_eq!(read_index_state(&layout), IndexState::Absent);
    }

    #[test]
    fn write_manifest_atomic_leaves_no_tmp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        write_manifest_atomic(&layout.manifest, &sample_manifest()).expect("write");
        write_manifest_atomic(&layout.manifest, &sample_manifest()).expect("rewrite");
        let names: Vec<String> = std::fs::read_dir(&layout.dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["manifest.toml"]);
    }

    #[test]
    fn now_rfc3339_has_z_suffix_and_seconds() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(now.as_bytes()[10], b'T', "{now}");
    }

    #[test]
    fn knot_version_matches_cargo_lock() {
        let lock: toml::Table = include_str!("../../Cargo.lock")
            .parse()
            .expect("Cargo.lock");
        let packages = lock["package"].as_array().expect("package array");
        let knot = packages
            .iter()
            .filter_map(toml::Value::as_table)
            .find(|pkg| {
                pkg.get("name").and_then(toml::Value::as_str) == Some("knot")
                    && pkg
                        .get("source")
                        .and_then(toml::Value::as_str)
                        .is_some_and(|source| source.starts_with("registry+"))
            })
            .expect("knot from a registry in Cargo.lock");
        assert_eq!(
            knot.get("version").and_then(toml::Value::as_str),
            Some(KNOT_VERSION)
        );
    }
}
