//! MCP tool `status`.

use rust_mcp_sdk::macros::JsonSchema;
use rust_mcp_sdk::schema::{CallToolError, CallToolRequestParams, CallToolResult, TextContent};

use crate::cli_tools;
use crate::config::OutputFormat;
use crate::mcp_handler::KnotLiteMcpHandler;
use crate::mcp_tools::parse_args;

/// Input contract for `status` (Appendix A).
#[rust_mcp_sdk::macros::mcp_tool(
    name = "status",
    title = "Repository capability matrix",
    description = "Read-only capability matrix of the repository served by this knot-lite instance: for each language, the file count, whether a parser exists, whether its language server is installed, the resulting tier (3 = parser + language server, 2 = parser only, heuristic graph, 1 = files only, 0 = excluded) and the index state (components, counts, embedding model and freshness against the working tree). Call this FIRST. Returns a Markdown table plus the same data as structured JSON (schema knot-lite/status/v1). For tier-3 languages it also shows whether the language server is running, its version and memory use, and whether language-server results were pre-computed.",
    read_only_hint = true,
    destructive_hint = false,
    idempotent_hint = true,
    open_world_hint = false
)]
#[derive(JsonSchema, serde::Deserialize, Debug)]
pub struct StatusTool {
    /// Repository scope (single-repository server).
    #[json_schema(
        description = "Optional. knot-lite serves a single repository: omit it, or pass this repository's name, 'all' or '*'. Accepts a single name, a comma-separated list or a JSON string array for compatibility with knot.",
        min_length = 1,
        max_length = 255
    )]
    pub repo_name: Option<String>,
}

impl StatusTool {
    /// Handle a `status` tool call: Markdown rendering plus structured JSON.
    pub async fn handle(
        params: CallToolRequestParams,
        handler: &KnotLiteMcpHandler,
    ) -> Result<CallToolResult, CallToolError> {
        let cfg = handler.config()?.clone();
        let _args: StatusTool = parse_args("status", &params, &cfg.repo_name)?;
        let runtime = handler.services.lsp_runtime();
        let json = tokio::task::spawn_blocking(move || {
            cli_tools::run_status_with_runtime(&cfg, runtime.as_ref())
        })
        .await
        .map_err(|e| CallToolError::from_message(format!("status: {e}")))?
        .map_err(|e| CallToolError::from_message(format!("status: {e}")))?;
        let text = cli_tools::format_status_output(&json, OutputFormat::Markdown);
        let structured = json.as_object().cloned().unwrap_or_default();
        Ok(
            CallToolResult::text_content(vec![TextContent::new(text, None, None)])
                .with_structured_content(structured),
        )
    }
}
