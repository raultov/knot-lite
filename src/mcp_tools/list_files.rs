//! MCP tool `list_files`.

use rust_mcp_sdk::macros::JsonSchema;

use crate::mcp_tools::impl_query_tool_handle;
use crate::models::params::FilesParams;

/// Input contract for `list_files` (Appendix A).
#[rust_mcp_sdk::macros::mcp_tool(
    name = "list_files",
    title = "List indexed files",
    description = "Read-only listing of the files in this repository's knot-lite index with per-file entity and chunk counts, in path order. 'path' narrows the listing to a repo-relative directory prefix matched on a path boundary ('src/api' never matches 'src/api-notes.md') or to a glob ('src/**/*.rs': `*` stays inside one path segment, `**` spans any number of segments, `?` is one character; matching is case-sensitive, brackets and braces are literal). At most 2000 files are listed and the reply states the exact total. Requires a knot-lite index (built with `knot-lite index`); call `status` to check.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(JsonSchema, serde::Deserialize, Debug)]
pub struct ListFilesTool {
    /// Optional repo-relative directory prefix.
    #[json_schema(
        description = "Optional repo-relative directory prefix (e.g. 'src/api', matched on a path boundary) or glob pattern (segments: `*` segment wildcard, `**` any depth, `?` one char). Omit to list every indexed file.",
        min_length = 1,
        max_length = 500
    )]
    pub path: Option<String>,
    /// Repository scope (single-repository server).
    #[json_schema(
        description = "Optional. knot-lite serves a single repository: omit it, or pass this repository's name, 'all' or '*'. Accepts a single name, a comma-separated list or a JSON string array for compatibility with knot.",
        min_length = 1,
        max_length = 255
    )]
    pub repo_name: Option<String>,
}

impl_query_tool_handle!(
    ListFilesTool,
    "list_files",
    FilesParams,
    crate::cli_tools::run_list_files,
    crate::cli_tools::format_files_output
);
