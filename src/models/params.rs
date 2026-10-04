//! Tool/CLI parameter structs shared by `cli_tools` and `mcp_tools`.
//!
//! These mirror the MCP tool fields of Appendix A minus `repo_name` (which is
//! validated by [`crate::models::repo_scope`] and never reaches a `run_*`
//! function). They are the contract for Phases 3/4: the query runners accept
//! them unchanged.

/// Parameters of `search_hybrid_context`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchParams {
    /// Semantic query text.
    pub query: String,
    /// Maximum number of results (tool default 5).
    pub max_results: Option<i64>,
    /// Optional entity-kind filter, comma-separated.
    pub kinds: Option<String>,
    /// Optional repo-relative path prefix filter.
    pub path: Option<String>,
}

/// Parameters of `find_callers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallersParams {
    /// Entity to resolve (name or `Type.member`).
    pub entity_name: String,
    /// Maximum number of targets (tool default 25).
    pub max_targets: Option<i64>,
    /// Optional entity-kind filter, comma-separated.
    pub kinds: Option<String>,
}

/// Parameters of `explore_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExploreParams {
    /// Repo-relative file path.
    pub file_path: String,
    /// Whether to include doc/markup context.
    pub include_markup: bool,
}

/// Parameters of `list_files`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilesParams {
    /// Optional repo-relative path prefix or glob.
    pub path: Option<String>,
}

impl SearchParams {
    /// Map from the CLI `search` subcommand.
    pub fn from_cli(
        query: String,
        max_results: usize,
        kinds: Option<String>,
        path: Option<String>,
    ) -> Self {
        Self {
            query,
            max_results: Some(i64::try_from(max_results).unwrap_or(i64::MAX)),
            kinds,
            path,
        }
    }
}

impl CallersParams {
    /// Map from the CLI `callers` subcommand.
    pub fn from_cli(
        entity_name: String,
        max_targets: Option<usize>,
        kinds: Option<String>,
    ) -> Self {
        Self {
            entity_name,
            max_targets: max_targets.map(|n| i64::try_from(n).unwrap_or(i64::MAX)),
            kinds,
        }
    }
}

impl ExploreParams {
    /// Map from the CLI `explore` subcommand.
    pub fn from_cli(file_path: String, include_markup: bool) -> Self {
        Self {
            file_path,
            include_markup,
        }
    }
}

impl FilesParams {
    /// Map from the CLI `files` subcommand.
    pub fn from_cli(path: Option<String>) -> Self {
        Self { path }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_from_cli_keeps_all_fields() {
        let params = SearchParams::from_cli(
            "q".to_string(),
            7,
            Some("rust_function".to_string()),
            Some("src/".to_string()),
        );
        assert_eq!(params.query, "q");
        assert_eq!(params.max_results, Some(7));
        assert_eq!(params.kinds.as_deref(), Some("rust_function"));
        assert_eq!(params.path.as_deref(), Some("src/"));
    }

    #[test]
    fn callers_from_cli_maps_optional_max_targets() {
        let with = CallersParams::from_cli("E".to_string(), Some(3), None);
        assert_eq!(with.max_targets, Some(3));
        let without = CallersParams::from_cli("E".to_string(), None, None);
        assert_eq!(without.max_targets, None);
    }

    #[test]
    fn explore_and_files_from_cli() {
        assert_eq!(
            ExploreParams::from_cli("src/lib.rs".to_string(), true),
            ExploreParams {
                file_path: "src/lib.rs".to_string(),
                include_markup: true,
            }
        );
        assert_eq!(
            FilesParams::from_cli(Some("src/".to_string())),
            FilesParams {
                path: Some("src/".to_string())
            }
        );
        assert_eq!(FilesParams::from_cli(None), FilesParams::default());
    }
}
