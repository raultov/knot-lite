//! Tool/CLI parameter structs shared by `cli_tools` and `mcp_tools`.
//!
//! These mirror the MCP tool fields of Appendix A minus `repo_name` (which is
//! validated by [`crate::models::repo_scope`] and never reaches a `run_*`
//! function). They are the contract for Phases 3/4: the query runners accept
//! them unchanged.

use crate::index::layout::CommitMode;
use crate::models::cli_args::IndexArgs;

/// Parameters of `index` (CLI subcommand).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexParams {
    /// Re-embed every record (ignore reusable vectors).
    pub force: bool,
    /// Effective commit mode (`Some(Commit)` or `Some(Gitignore)` or `None`).
    pub commit_mode: Option<CommitMode>,
    /// Optional embedding model override.
    pub embed_model: Option<String>,
    /// Optional parallel parser threads.
    pub jobs: Option<usize>,
    /// Passages per embedding call.
    pub batch_size: usize,
    /// Quiet output on stderr.
    pub quiet: bool,
}

impl IndexParams {
    /// Map from the CLI `index` subcommand arguments.
    pub fn from_cli(args: &IndexArgs) -> Self {
        let commit_mode = if args.commit {
            Some(CommitMode::Commit)
        } else if args.gitignore {
            Some(CommitMode::Gitignore)
        } else {
            None
        };
        Self {
            force: args.force,
            commit_mode,
            embed_model: args.embed_model.clone(),
            jobs: args.jobs,
            batch_size: args.batch_size,
            quiet: args.quiet,
        }
    }
}

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

    /// Map from the `search_hybrid_context` tool arguments (`repo_name` is
    /// stripped by `parse_args` before this runs).
    pub fn from_tool(
        tool: crate::mcp_tools::search_hybrid_context::SearchHybridContextTool,
    ) -> Self {
        Self {
            query: tool.query,
            max_results: tool.max_results,
            kinds: tool.kinds,
            path: tool.path,
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

    /// Map from the `find_callers` tool arguments.
    pub fn from_tool(tool: crate::mcp_tools::find_callers::FindCallersTool) -> Self {
        Self {
            entity_name: tool.entity_name,
            max_targets: tool.max_targets,
            kinds: tool.kinds,
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

    /// Map from the `explore_file` tool arguments.
    pub fn from_tool(tool: crate::mcp_tools::explore_file::ExploreFileTool) -> Self {
        Self {
            file_path: tool.file_path,
            include_markup: tool.include_markup.unwrap_or(false),
        }
    }
}

impl FilesParams {
    /// Map from the CLI `files` subcommand.
    pub fn from_cli(path: Option<String>) -> Self {
        Self { path }
    }

    /// Map from the `list_files` tool arguments.
    pub fn from_tool(tool: crate::mcp_tools::list_files::ListFilesTool) -> Self {
        Self { path: tool.path }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_params_from_cli() {
        let args = IndexArgs {
            force: true,
            commit: true,
            gitignore: false,
            embed_model: Some("BGEBaseENV15".to_string()),
            jobs: Some(4),
            batch_size: 64,
            quiet: true,
            output: crate::config::OutputFormat::Json,
        };
        let params = IndexParams::from_cli(&args);
        assert_eq!(
            params,
            IndexParams {
                force: true,
                commit_mode: Some(CommitMode::Commit),
                embed_model: Some("BGEBaseENV15".to_string()),
                jobs: Some(4),
                batch_size: 64,
                quiet: true,
            }
        );

        let gi_args = IndexArgs {
            commit: false,
            gitignore: true,
            ..args
        };
        assert_eq!(
            IndexParams::from_cli(&gi_args).commit_mode,
            Some(CommitMode::Gitignore)
        );
    }

    #[test]
    fn search_from_cli_keeps_all_fields() {
        let params = SearchParams::from_cli(
            "user auth".to_string(),
            10,
            Some("definition".to_string()),
            Some("src/api".to_string()),
        );
        assert_eq!(params.query, "user auth");
        assert_eq!(params.max_results, Some(10));
        assert_eq!(params.kinds.as_deref(), Some("definition"));
        assert_eq!(params.path.as_deref(), Some("src/api"));
    }

    #[test]
    fn callers_from_cli_maps_optional_max_targets() {
        let with_max = CallersParams::from_cli("handle".to_string(), Some(15), None);
        assert_eq!(with_max.max_targets, Some(15));
        let without_max = CallersParams::from_cli("handle".to_string(), None, None);
        assert_eq!(without_max.max_targets, None);
    }

    #[test]
    fn explore_and_files_from_cli() {
        let explore = ExploreParams::from_cli("src/lib.rs".to_string(), true);
        assert_eq!(explore.file_path, "src/lib.rs");
        assert!(explore.include_markup);

        let files = FilesParams::from_cli(Some("src/".to_string()));
        assert_eq!(files.path.as_deref(), Some("src/"));
    }
}
