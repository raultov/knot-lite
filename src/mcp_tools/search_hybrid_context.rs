//! MCP tool `search_hybrid_context`.

use rust_mcp_sdk::macros::JsonSchema;

use crate::mcp_tools::impl_query_tool_handle;
use crate::models::params::SearchParams;

/// Input contract for `search_hybrid_context` (Appendix A).
#[rust_mcp_sdk::macros::mcp_tool(
    name = "search_hybrid_context",
    title = "Semantic search",
    description = "Read-only semantic search over this repository's index. Finds code entities by meaning (e.g. 'user authentication', 'API error handling') instead of exact keywords. \
                   Requires a knot-lite index; call `status` to check. \
                   \n\nUsage: use for initial exploration and when you do not know the exact names of the entities involved. \
                   \n\nRanking contract: results are kind-aware — function/method/class/struct definitions outrank markdown docs, test files, config properties and build-dependency entities for natural-language queries; callers and helpers appear as context attached to a definition, never as substitutes. \
                   The shared entry point of the highest-ranked helpers outranks those helpers themselves. \
                   \n\nRecall contract: entity embeds carry identifier tokens and the tokenized call names of the entity's body, so a paraphrase of what a definition does still surfaces it. \
                   Entities whose identifier shares a word with the query enter the candidate pool by token match alone. \
                   \n\nResult bound: 'max_results' is clamped to 100. There is no pagination; narrow the scope with 'kinds' and 'path' when the bound is not enough.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(JsonSchema, serde::Deserialize, Debug)]
pub struct SearchHybridContextTool {
    /// Query text describing the functionality to find.
    #[json_schema(
        description = "Search query describing what to find (e.g. 'user authentication', 'API error handling')",
        min_length = 1,
        max_length = 500
    )]
    pub query: String,
    /// Maximum number of results (default 5).
    #[json_schema(
        description = "Maximum number of results to return (default: 5, max: 100; larger values are clamped).",
        minimum = 1,
        maximum = 100,
        default = 5
    )]
    pub max_results: Option<i64>,
    /// Repository scope (single-repository server).
    #[json_schema(
        description = "Optional. knot-lite serves a single repository: omit it, or pass this repository's name, 'all' or '*'. Accepts a single name, a comma-separated list or a JSON string array for compatibility with knot.",
        min_length = 1,
        max_length = 255
    )]
    pub repo_name: Option<String>,
    /// Optional entity-kind filter.
    #[json_schema(
        description = "Optional entity-kind filter: exact kinds ('rust_function', 'markdown_section', ...) or aliases ('definition', 'callable', 'class', 'type'). Comma-separated for multiple values.",
        min_length = 1,
        max_length = 255
    )]
    pub kinds: Option<String>,
    /// Optional repo-relative path filter.
    #[json_schema(
        description = "Optional path filter: repo-relative directory prefix ('src/api', matched on a path boundary) or glob ('src/**/*_test.rs').",
        min_length = 1,
        max_length = 500
    )]
    pub path: Option<String>,
}

impl_query_tool_handle!(
    SearchHybridContextTool,
    "search_hybrid_context",
    SearchParams,
    crate::cli_tools::run_search_hybrid_context
);
