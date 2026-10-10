//! MCP tool `search_hybrid_context`.

use rust_mcp_sdk::macros::JsonSchema;

use crate::mcp_tools::impl_query_tool_handle;
use crate::models::params::SearchParams;

/// Input contract for `search_hybrid_context` (Appendix A).
#[rust_mcp_sdk::macros::mcp_tool(
    name = "search_hybrid_context",
    title = "Semantic search",
    description = "Read-only semantic search over this repository's knot-lite index: code entities parsed by knot (functions, methods, types, documentation sections, …) and line-window chunks of files knot cannot parse (scripts, Makefiles, YAML, …). By default every record is ranked by cosine similarity with the query (embedded with the index's model). The server can be started with KNOT_LITE_SEARCH=hybrid for an opt-in hybrid ranking: candidates from cosine, a pass over definitions, names that match query words, a BM25 keyword channel over the indexed text and the callers of the best matches, ordered by knot's kind-aware re-rank fused with the keyword order (reciprocal-rank fusion). The response's 'ranking.mode' says which one answered; each result reports its score, cosine, keyword score and the channels that found it. Requires a knot-lite index (built with `knot-lite index`); call `status` to check. \n\nUsage: initial exploration when you do not know the names involved; follow up with explore_file and find_callers. \n\nFilters: 'kinds' accepts exact kinds or the aliases definition, callable, class/type, docs, config, plus 'chunk' and 'file' for the line-window records; 'path' is a repo-relative directory prefix matched on a path boundary or a glob (`*` one segment, `**` any depth, `?` one character). \n\nResult bound: 'max_results' is clamped to 100; there is no pagination.",
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
        description = "Optional kind filter: exact kinds ('rust_function', 'markdown_section', 'chunk', 'file') or aliases ('definition', 'callable', 'class', 'type', 'docs', 'config'). Comma-separated.",
        min_length = 1,
        max_length = 255
    )]
    pub kinds: Option<String>,
    /// Optional repo-relative path filter.
    #[json_schema(
        description = "Optional repo-relative directory prefix (e.g. 'src/api', matched on a path boundary) or glob pattern (segments: `*` segment wildcard, `**` any depth, `?` one char).",
        min_length = 1,
        max_length = 500
    )]
    pub path: Option<String>,
}

impl_query_tool_handle!(
    SearchHybridContextTool,
    "search_hybrid_context",
    SearchParams,
    crate::cli_tools::run_search_hybrid_context,
    crate::cli_tools::format_search_output
);
