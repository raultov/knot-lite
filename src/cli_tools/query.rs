//! Query tool entry points (`run_*`), returning typed unavailability errors
//! until indexing lands in later phases.
//!
//! Each body is a single [`unavailable`] call: the params are the Phase 3/4
//! contract (named `_params` here on purpose — nothing can run yet).

use crate::config::Config;
use crate::error::QueryUnavailable;
use crate::index::layout::{INDEX_DIR, IndexLayout, MANIFEST_FILE, MANIFEST_SCHEMA_V1};
use crate::index::manifest::{IndexState, read_index_state};
use crate::models::params::{CallersParams, ExploreParams, FilesParams, SearchParams};

/// `search_hybrid_context` — semantic search (Phase 3).
pub fn run_search_hybrid_context(
    cfg: &Config,
    _params: &SearchParams,
) -> Result<serde_json::Value, QueryUnavailable> {
    Err(unavailable(cfg, "search_hybrid_context"))
}

/// `find_callers` — reverse references (Phase 4).
pub fn run_find_callers(
    cfg: &Config,
    _params: &CallersParams,
) -> Result<serde_json::Value, QueryUnavailable> {
    Err(unavailable(cfg, "find_callers"))
}

/// `explore_file` — entities declared in one file (Phase 3/4).
pub fn run_explore_file(
    cfg: &Config,
    _params: &ExploreParams,
) -> Result<serde_json::Value, QueryUnavailable> {
    Err(unavailable(cfg, "explore_file"))
}

/// `list_files` — files in the index (Phase 3/4).
pub fn run_list_files(
    cfg: &Config,
    _params: &FilesParams,
) -> Result<serde_json::Value, QueryUnavailable> {
    Err(unavailable(cfg, "list_files"))
}

/// Map the index state to the matching [`QueryUnavailable`] variant.
///
/// Cheap and synchronous: reads only the manifest, never scans the repository.
fn unavailable(cfg: &Config, tool: &str) -> QueryUnavailable {
    let manifest = format!("{INDEX_DIR}/{MANIFEST_FILE}");
    let version = env!("CARGO_PKG_VERSION").to_string();
    let repo = cfg.repo_name.clone();
    let tool = tool.to_string();
    match read_index_state(&IndexLayout::for_repo(&cfg.repo_path)) {
        IndexState::Absent => QueryUnavailable::NotIndexed {
            tool,
            repo,
            manifest,
            version,
        },
        IndexState::Present {
            schema,
            knot_lite_version,
        } => QueryUnavailable::NotImplemented {
            tool,
            repo,
            schema,
            written_by: knot_lite_version,
            version,
        },
        IndexState::Unsupported { schema } => QueryUnavailable::Unsupported {
            tool,
            repo,
            schema,
            expected: MANIFEST_SCHEMA_V1.to_string(),
            version,
        },
        IndexState::Invalid { error } => QueryUnavailable::Invalid {
            tool,
            repo,
            manifest,
            error,
        },
        // Interim mapping until the error rework (P2.8) adds a dedicated variant.
        IndexState::Incomplete => QueryUnavailable::Invalid {
            tool,
            repo,
            manifest,
            error: "an interrupted `knot-lite index` run left the index incomplete; re-run `knot-lite index`"
                .to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::config_for;

    /// All four runners must answer with the same variant shape.
    fn all_unavailable(cfg: &Config) -> Vec<QueryUnavailable> {
        vec![
            run_search_hybrid_context(cfg, &search()).unwrap_err(),
            run_find_callers(cfg, &callers()).unwrap_err(),
            run_explore_file(cfg, &explore()).unwrap_err(),
            run_list_files(cfg, &FilesParams::default()).unwrap_err(),
        ]
    }

    fn search() -> SearchParams {
        SearchParams {
            query: "q".to_string(),
            max_results: None,
            kinds: None,
            path: None,
        }
    }

    fn callers() -> CallersParams {
        CallersParams {
            entity_name: "foo".to_string(),
            max_targets: None,
            kinds: None,
        }
    }

    fn explore() -> ExploreParams {
        ExploreParams {
            file_path: "src/lib.rs".to_string(),
            include_markup: false,
        }
    }

    #[test]
    fn empty_repo_reports_not_indexed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = config_for(tmp.path().to_path_buf());
        for err in all_unavailable(&cfg) {
            let text = err.to_string();
            assert!(matches!(err, QueryUnavailable::NotIndexed { .. }), "{text}");
            assert!(text.contains("has no index"), "{text}");
            assert!(text.contains("'demo'"), "{text}");
            assert!(text.contains("`status`"), "{text}");
        }
    }

    #[test]
    fn v1_manifest_reports_not_implemented() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join(".knot-lite");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("manifest.toml"),
            "schema = \"knot-lite/manifest/v1\"\nknot_lite_version = \"0.0.1\"\n",
        )
        .expect("write manifest");
        let cfg = config_for(tmp.path().to_path_buf());
        for err in all_unavailable(&cfg) {
            let text = err.to_string();
            assert!(
                matches!(err, QueryUnavailable::NotImplemented { .. }),
                "{text}"
            );
            assert!(text.contains("cannot query indexes yet"), "{text}");
            assert!(text.contains("0.0.1"), "{text}");
        }
    }

    #[test]
    fn other_schema_reports_unsupported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join(".knot-lite");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("manifest.toml"),
            "schema = \"knot-lite/manifest/v2\"\nknot_lite_version = \"9.9.9\"\n",
        )
        .expect("write manifest");
        let cfg = config_for(tmp.path().to_path_buf());
        for err in all_unavailable(&cfg) {
            let text = err.to_string();
            assert!(
                matches!(err, QueryUnavailable::Unsupported { .. }),
                "{text}"
            );
            assert!(text.contains("knot-lite/manifest/v1"), "{text}");
        }
    }

    #[test]
    fn tool_names_appear_in_messages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = config_for(tmp.path().to_path_buf());
        let messages: Vec<String> = all_unavailable(&cfg)
            .iter()
            .map(ToString::to_string)
            .collect();
        for tool in [
            "search_hybrid_context",
            "find_callers",
            "explore_file",
            "list_files",
        ] {
            assert!(
                messages.iter().any(|m| m.starts_with(&format!("{tool}:"))),
                "no message starts with {tool}: {messages:?}"
            );
        }
    }
}
