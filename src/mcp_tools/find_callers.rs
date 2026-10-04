//! MCP tool `find_callers`.

use rust_mcp_sdk::macros::JsonSchema;

use crate::mcp_tools::impl_query_tool_handle;
use crate::models::params::CallersParams;

/// Input contract for `find_callers` (Appendix A).
#[rust_mcp_sdk::macros::mcp_tool(
    name = "find_callers",
    title = "Find callers (reverse dependencies)",
    description = "Read-only reverse dependency lookup. Use this to find all code that references, calls, extends, or implements a specific entity. \
                   Answers 'who uses this code?' by querying the call graph. Differs from search tools by providing exact dependency tracking. \
                   Requires a knot-lite index; call `status` to check. \
                   \n\nUsage: use for impact analysis before refactoring or to detect dead code. Do NOT use this for semantic feature discovery — use 'search_hybrid_context' instead. \
                   \n\nMatching is precedence-based: exact FQN (containing '.' or '::') → FQN suffix (`Type.member`) → exact name → signature prefix (`accept(List`) → name prefix (queries under 4 characters) → fuzzy substring (queries of 4+ characters). \
                   The first tier that matches wins, so an exact name never returns fuzzy noise. Pass a qualified name (`Namespace.Type.Member`) to disambiguate homonyms. \
                   \n\nBehaviour & Return: read-only graph traversal with no side effects. Returns Markdown grouped by relationship type (Calls, Extends, Implements, References, Overridden by, Overrides) with exact file paths and line numbers. \
                   Each caller entry includes: name, kind, file_path:line_number, and signature. When multiple targets exist, results are grouped by target entity. \
                   \n\nTruncation & completeness: targets are capped by 'max_targets' (default 25, max 500). When more targets match than fit the cap, the response states 'Truncated — N targets matched; showing the first M by FQN'.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(JsonSchema, serde::Deserialize, Debug)]
pub struct FindCallersTool {
    /// Entity to resolve (name, `Type.member` or `Namespace.Type.Member`).
    #[json_schema(
        description = "The name of the function, method, or class to find callers for. Matching is precedence-based: exact FQN → FQN suffix → exact name → signature prefix → name prefix (queries under 4 chars) → fuzzy substring (4+ chars).",
        min_length = 1,
        max_length = 255
    )]
    pub entity_name: String,
    /// Repository scope (single-repository server).
    #[json_schema(
        description = "Optional. knot-lite serves a single repository: omit it, or pass this repository's name, 'all' or '*'. Accepts a single name, a comma-separated list or a JSON string array for compatibility with knot.",
        min_length = 1,
        max_length = 255
    )]
    pub repo_name: Option<String>,
    /// Maximum number of resolved targets (default 25).
    #[json_schema(
        description = "Maximum number of resolved targets to include (default: 25, max: 500). Raise this when the response reports a truncated target list.",
        minimum = 1,
        maximum = 500,
        default = 25
    )]
    pub max_targets: Option<i64>,
    /// Optional entity-kind filter for target resolution.
    #[json_schema(
        description = "Optional entity-kind scope for target resolution: 'all' (or '*') to disable the default code-only scope, or a comma-separated allow-list of exact kinds or aliases ('callable', 'class', 'config', 'rust_function', ...).",
        min_length = 1,
        max_length = 1024
    )]
    pub kinds: Option<String>,
}

impl_query_tool_handle!(
    FindCallersTool,
    "find_callers",
    CallersParams,
    crate::cli_tools::run_find_callers
);
