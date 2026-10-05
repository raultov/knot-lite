//! Query tool entry points (`run_*`).
//!
//! Interim (P2.8): the runners only diagnose the index through the loader; the
//! real query bodies replace them in P2.9–P2.11.

use crate::config::Config;
use crate::error::QueryUnavailable;
use crate::index::layout::IndexLayout;
use crate::index::load::load_index;
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

/// Diagnose why `tool` cannot answer: the loader's reason when the index is
/// unusable; a loadable index cannot answer yet either (interim).
fn unavailable(cfg: &Config, tool: &str) -> QueryUnavailable {
    match load_index(&IndexLayout::for_repo(&cfg.repo_path)) {
        Err(reason) => reason.for_tool(tool, &cfg.repo_name),
        Ok(_) => QueryUnavailable::Invalid {
            tool: tool.to_owned(),
            repo: cfg.repo_name.clone(),
            manifest: IndexLayout::manifest_rel(),
            error: "this knot-lite build cannot answer queries yet".to_owned(),
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
            assert!(text.contains("`knot-lite index`"), "{text}");
        }
    }

    fn write_manifest(tmp: &tempfile::TempDir, body: &str) {
        let dir = tmp.path().join(".knot-lite");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("manifest.toml"), body).expect("write manifest");
    }

    #[test]
    fn header_only_manifest_reports_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(
            &tmp,
            "schema = \"knot-lite/manifest/v1\"\nknot_lite_version = \"0.0.1\"\n",
        );
        let cfg = config_for(tmp.path().to_path_buf());
        for err in all_unavailable(&cfg) {
            let text = err.to_string();
            assert!(matches!(err, QueryUnavailable::Invalid { .. }), "{text}");
            assert!(text.contains("could not be read"), "{text}");
            assert!(text.contains(".knot-lite/manifest.toml"), "{text}");
        }
    }

    #[test]
    fn leftover_previous_manifest_reports_incomplete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join(".knot-lite");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("manifest.prev.toml"), "x = 1\n").expect("write");
        let cfg = config_for(tmp.path().to_path_buf());
        for err in all_unavailable(&cfg) {
            assert!(matches!(err, QueryUnavailable::Incomplete { .. }), "{err}");
        }
    }

    #[test]
    fn other_schema_reports_unsupported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(
            &tmp,
            "schema = \"knot-lite/manifest/v2\"\nknot_lite_version = \"9.9.9\"\n",
        );
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
