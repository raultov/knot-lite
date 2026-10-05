//! MCP tool `find_callers`.

use rust_mcp_sdk::macros::JsonSchema;

use crate::mcp_tools::impl_query_tool_handle;
use crate::models::params::CallersParams;

/// Input contract for `find_callers` (Appendix A).
#[rust_mcp_sdk::macros::mcp_tool(
    name = "find_callers",
    title = "Find callers (reverse dependencies)",
    description = "Read-only reverse dependency lookup over this repository's knot-lite index: which code references, calls, extends or implements an entity. Edges are pre-computed at index time by knot's heuristic resolver (provenance 'heuristic'; knot-lite's Phase 0 measured about 93 % precision and 86 % recall on Rust); language servers are not used yet. Requires a knot-lite index (built with `knot-lite index`); call `status` to check. \n\nTarget matching is precedence-based and stops at the first tier with a match: exact FQN → FQN suffix ('Type.member' or 'module::name'; '.' and '::' are interchangeable) → exact name. Name-prefix and fuzzy matching are not available in this version. Documentation and config/build kinds are excluded from target resolution by default; pass kinds='all' to include them, or an explicit kind list. \n\nCallers are reported at container granularity (the innermost function, method or type that encloses the reference), grouped by relationship type; references from inside the target itself are counted, not listed. 'max_targets' (default 25, max 500) bounds homonym fan-out and truncation is reported.",
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
    crate::cli_tools::run_find_callers,
    crate::cli_tools::format_callers_output
);
