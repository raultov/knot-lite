//! MCP tool `explore_file`.

use rust_mcp_sdk::macros::JsonSchema;

use crate::mcp_tools::impl_query_tool_handle;
use crate::models::params::ExploreParams;

/// Input contract for `explore_file` (Appendix A).
#[rust_mcp_sdk::macros::mcp_tool(
    name = "explore_file",
    title = "File anatomy inspection",
    description = "Read-only file anatomy inspection. Use this to list all classes, methods, and properties within a specific source file without reading its entire contents. \
                   Requires a knot-lite index; call `status` to check. \
                   \n\nUsage: use AFTER identifying an interesting file via 'search_hybrid_context' to understand its structure, or before modifying a file to see its available methods. Do NOT use this for searching across multiple files. \
                   \n\nBehaviour & Return: read-only operation. Returns a Markdown outline of the file's entities (classes, methods, functions, properties) with line numbers, grouped by type, plus the source lines where each entity is declared.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(JsonSchema, serde::Deserialize, Debug)]
pub struct ExploreFileTool {
    /// File to inspect.
    #[json_schema(
        description = "Path to the source file to explore. PREFERRED: a repo-relative path (e.g. 'src/services/user.ts').",
        min_length = 1
    )]
    pub file_path: String,
    /// Repository scope (single-repository server).
    #[json_schema(
        description = "Optional. knot-lite serves a single repository: omit it, or pass this repository's name, 'all' or '*'. Accepts a single name, a comma-separated list or a JSON string array for compatibility with knot.",
        min_length = 1,
        max_length = 255
    )]
    pub repo_name: Option<String>,
    /// Include full markup reference lists.
    #[json_schema(
        description = "Optional: set to true to list all attribute-usage markup tokens (html_class, html_id) in full rather than in a compact summary. Defaults to false."
    )]
    pub include_markup: Option<bool>,
}

impl_query_tool_handle!(
    ExploreFileTool,
    "explore_file",
    ExploreParams,
    crate::cli_tools::run_explore_file
);
