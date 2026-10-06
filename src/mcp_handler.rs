//! Transport-independent MCP handler: `tools()` + `dispatch()`.
//!
//! Mirrors knot's `mcp_handler`: the tool surface is a plain function, the
//! transport is only a caller. Binaries stay thin.

use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::mcp_server::ServerHandler;
use rust_mcp_sdk::schema::{
    CallToolError, CallToolRequestParams, CallToolResult, Implementation, ListToolsResult,
    ListToolsResultCacheScope, PaginatedRequestParams, RpcError, ServerCapabilities,
    ServerCapabilitiesTools, ServerResult, Tool,
};
use rust_mcp_sdk::{McpServer, RequestContext, ServerDetails};

use crate::config::Config;
use crate::mcp_tools::explore_file::ExploreFileTool;
use crate::mcp_tools::find_callers::FindCallersTool;
use crate::mcp_tools::list_files::ListFilesTool;
use crate::mcp_tools::search_hybrid_context::SearchHybridContextTool;
use crate::mcp_tools::status::StatusTool;
use crate::query::session::QueryServices;

/// Refusal message for every tool call in dry-run mode.
pub const DRY_RUN_MESSAGE: &str = "Server is running in dry-run mode. The repository is not opened. This mode is used for protocol validation and quality checks only.";

/// Advertised instructions, surfaced by `server/discover`.
pub const SERVER_INSTRUCTIONS: &str = "knot-lite answers code-intelligence questions about ONE repository without databases.\n\
1. status — call it first: capability matrix (tiers 3/2/1/0 per language) and index state.\n\
2. search_hybrid_context — semantic search over the index.\n\
3. find_callers — reverse references (graph provenance: lsp or heuristic).\n\
4. explore_file — entities declared in one file.\n\
5. list_files — files in the index, optionally by prefix or glob.\n\
Tools 2-5 require a knot-lite index; without one they explain what is missing.\n\
repo_name is optional: this server serves a single repository; 'all', '*' or its own name are accepted.";

/// The MCP handler: one server, one repository (P1-D7).
pub struct KnotLiteMcpHandler {
    /// The served repository (absent in dry-run mode).
    pub config: Option<Arc<Config>>,
    /// Whether to answer without opening the repository.
    pub dry_run: bool,
    /// The cached index and query embedder, shared by every request (P2-D14).
    pub services: Arc<QueryServices>,
}

impl KnotLiteMcpHandler {
    /// A handler serving one repository, with knot's real embedder (loaded
    /// lazily, on the first search).
    pub fn new(config: Config) -> Self {
        let services = Arc::new(QueryServices::real(&config.embed));
        Self::with_services(config, services)
    }

    /// A handler serving one repository with explicit query services (tests).
    pub fn with_services(config: Config, services: Arc<QueryServices>) -> Self {
        Self {
            config: Some(Arc::new(config)),
            dry_run: false,
            services,
        }
    }

    /// A handler that refuses every tool (protocol validation only).
    pub fn new_dry_run() -> Self {
        Self {
            config: None,
            dry_run: true,
            services: Arc::new(QueryServices::with_factory(Arc::new(|_model: &str| {
                anyhow::bail!("dry-run mode never embeds")
            }))),
        }
    }

    /// The served repository, or an error when none is configured.
    pub(crate) fn config(&self) -> Result<&Config, CallToolError> {
        self.config.as_deref().ok_or_else(|| {
            CallToolError::from_message("server has no repository configured".to_string())
        })
    }

    /// The canonical tool surface, independent of transport (see knot's `mcp_handler`).
    pub fn tools() -> Vec<Tool> {
        vec![
            StatusTool::tool(),
            SearchHybridContextTool::tool(),
            FindCallersTool::tool(),
            ExploreFileTool::tool(),
            ListFilesTool::tool(),
        ]
    }

    /// Dispatch one `tools/call` to its tool handler.
    pub async fn dispatch(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, CallToolError> {
        if self.dry_run {
            return Err(CallToolError::from_message(DRY_RUN_MESSAGE.to_string()));
        }
        match params.name.as_str() {
            "status" => StatusTool::handle(params, self).await,
            "search_hybrid_context" => SearchHybridContextTool::handle(params, self).await,
            "find_callers" => FindCallersTool::handle(params, self).await,
            "explore_file" => ExploreFileTool::handle(params, self).await,
            "list_files" => ListFilesTool::handle(params, self).await,
            _ => Err(CallToolError::unknown_tool(params.name)),
        }
    }
}

#[async_trait]
impl ServerHandler for KnotLiteMcpHandler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: &RequestContext,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: Self::tools(),
            // Tool lists are not user-specific; P1.3: no Default on the enum.
            cache_scope: ListToolsResultCacheScope::Public,
            result_type: "complete".to_string(),
            ttl_ms: 0,
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _context: &RequestContext,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ServerResult, CallToolError> {
        self.dispatch(params).await.map(Into::into)
    }
}

/// Build the MCP server details advertised on `server/discover`.
pub fn build_server_details() -> ServerDetails {
    ServerDetails {
        server_info: Implementation {
            name: "knot-lite".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("knot-lite Code Intelligence".into()),
            description: Some(
                "Database-free semantic search and call graph for one repository (file-based vectors + language servers)"
                    .into(),
            ),
            icons: vec![],
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools {
                list_changed: None,
            }),
            ..Default::default()
        },
        instructions: Some(SERVER_INSTRUCTIONS.into()),
        meta: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_mcp_sdk::schema::ClientCapabilities;
    use rust_mcp_sdk::schema::RequestMetaObject;

    const EXPECTED_TOOLS: [&str; 5] = [
        "status",
        "search_hybrid_context",
        "find_callers",
        "explore_file",
        "list_files",
    ];

    fn source_between(start: &str, end: &str) -> String {
        let src = include_str!("mcp_handler.rs");
        let start_idx = src
            .find(start)
            .unwrap_or_else(|| panic!("source marker not found: {start}"));
        let rest = &src[start_idx..];
        let end_idx = rest
            .find(end)
            .unwrap_or_else(|| panic!("source marker not found: {end}"));
        rest[..end_idx].to_string()
    }

    fn call_params(
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> CallToolRequestParams {
        CallToolRequestParams {
            name: name.to_string(),
            arguments: Some(args),
            input_responses: None,
            request_state: None,
            meta: RequestMetaObject::new("2026-07-28", ClientCapabilities::default()),
        }
    }

    fn tempdir_handler() -> (tempfile::TempDir, KnotLiteMcpHandler) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = crate::config::test_support::config_for(tmp.path().to_path_buf());
        (tmp, KnotLiteMcpHandler::new(cfg))
    }

    #[test]
    fn tools_returns_the_full_surface() {
        let names: Vec<String> = KnotLiteMcpHandler::tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, EXPECTED_TOOLS);
    }

    #[test]
    fn every_tool_has_name_description_and_object_schema() {
        for tool in KnotLiteMcpHandler::tools() {
            assert!(!tool.name.is_empty(), "empty tool name");
            let desc = tool.description.as_deref().unwrap_or_default();
            assert!(!desc.is_empty(), "{} has no description", tool.name);
            assert_eq!(tool.input_schema.type_(), "object", "{}", tool.name);
        }
    }

    #[test]
    fn list_tools_request_delegates_to_tools_fn() {
        let body = source_between(
            "async fn handle_list_tools_request",
            "async fn handle_call_tool_request",
        );
        assert!(
            body.contains("Self::tools()"),
            "handle_list_tools_request must delegate to Self::tools()"
        );
        assert!(
            !body.contains("Tool::tool()"),
            "handle_list_tools_request must not re-inline the tool list"
        );
    }

    #[test]
    fn call_tool_request_delegates_to_dispatch() {
        let body = source_between(
            "async fn handle_call_tool_request",
            "/// Build the MCP server details",
        );
        assert!(
            body.contains("self.dispatch("),
            "handle_call_tool_request must delegate to self.dispatch()"
        );
        assert!(
            !body.contains("SearchHybridContextTool::handle"),
            "handle_call_tool_request must not re-inline the dispatch match"
        );
    }

    #[tokio::test]
    async fn dispatch_refuses_every_tool_in_dry_run_mode() {
        let handler = KnotLiteMcpHandler::new_dry_run();
        for name in EXPECTED_TOOLS {
            let err = handler
                .dispatch(call_params(name, serde_json::Map::new()))
                .await
                .expect_err("dry-run must refuse");
            assert_eq!(err.to_string(), DRY_RUN_MESSAGE);
        }
    }

    #[tokio::test]
    async fn dispatch_rejects_unknown_tool() {
        let (_tmp, handler) = tempdir_handler();
        let err = handler
            .dispatch(call_params("nope", serde_json::Map::new()))
            .await
            .expect_err("unknown tool must fail");
        let result = CallToolResult::from(err);
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn status_dispatch_succeeds_on_tempdir() {
        let (_tmp, handler) = tempdir_handler();
        let result = handler
            .dispatch(call_params("status", serde_json::Map::new()))
            .await
            .expect("status must succeed");
        assert_ne!(result.is_error, Some(true));
        let structured = result.structured_content.expect("structured content");
        assert_eq!(
            structured.get("schema").and_then(|v| v.as_str()),
            Some("knot-lite/status/v1")
        );
    }

    fn indexed_handler() -> (
        tempfile::TempDir,
        Config,
        KnotLiteMcpHandler,
        Arc<QueryServices>,
    ) {
        let (tmp, cfg) = crate::index::build::test_support::fixture();
        let mut fake = crate::embed::fake::FakeEmbedder::new(16);
        crate::index::build::build_index(
            &cfg,
            &mut fake,
            &crate::index::build::test_support::opts(),
            None,
            &mut |_| {},
        )
        .expect("build");
        let services = Arc::new(QueryServices::with_factory(Arc::new(|_model| {
            Ok(Box::new(crate::embed::fake::FakeEmbedder::new(16))
                as Box<dyn crate::embed::EmbedBatch + Send>)
        })));
        let handler = KnotLiteMcpHandler::with_services(cfg.clone(), Arc::clone(&services));
        (tmp, cfg, handler, services)
    }

    #[tokio::test]
    async fn dispatch_search_returns_structured_content() {
        let (_tmp, _cfg, handler, _services) = indexed_handler();
        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), serde_json::json!("target"));
        let result = handler
            .dispatch(call_params("search_hybrid_context", args))
            .await
            .expect("search");
        assert_ne!(result.is_error, Some(true));
        let rust_mcp_sdk::schema::ContentBlock::TextContent(text) = &result.content[0] else {
            panic!("expected text content");
        };
        assert!(
            text.text.starts_with("## knot-lite search"),
            "{}",
            text.text
        );
        let structured = result.structured_content.expect("structured content");
        assert_eq!(
            structured.get("schema").and_then(|v| v.as_str()),
            Some("knot-lite/search/v1")
        );
    }

    #[tokio::test]
    async fn dispatch_callers_lists_fixture_callers() {
        let (_tmp, _cfg, handler, _services) = indexed_handler();
        let mut args = serde_json::Map::new();
        args.insert("entity_name".to_string(), serde_json::json!("target"));
        let result = handler
            .dispatch(call_params("find_callers", args))
            .await
            .expect("callers");
        assert_ne!(result.is_error, Some(true));
        let structured = result.structured_content.expect("structured content");
        assert_eq!(
            structured.get("schema").and_then(|v| v.as_str()),
            Some("knot-lite/callers/v1")
        );
        let count = structured["targets"][0]["caller_count"].as_u64().unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn dispatch_reuses_loaded_index() {
        let (_tmp, _cfg, handler, services) = indexed_handler();
        let mut args1 = serde_json::Map::new();
        args1.insert("entity_name".to_string(), serde_json::json!("target"));
        handler
            .dispatch(call_params("find_callers", args1))
            .await
            .expect("1");
        let mut args2 = serde_json::Map::new();
        args2.insert("query".to_string(), serde_json::json!("target"));
        handler
            .dispatch(call_params("search_hybrid_context", args2))
            .await
            .expect("2");
        assert_eq!(services.index.loads(), 1);
    }

    #[tokio::test]
    async fn dispatch_reloads_after_reindex() {
        let (_tmp, cfg, handler, services) = indexed_handler();
        let mut args1 = serde_json::Map::new();
        args1.insert("entity_name".to_string(), serde_json::json!("target"));
        handler
            .dispatch(call_params("find_callers", args1))
            .await
            .expect("1");
        assert_eq!(services.index.loads(), 1);

        // Re-index after editing a file
        let path = cfg.repo_path.join("src/lib.rs");
        let body = std::fs::read_to_string(&path).expect("read");
        std::fs::write(&path, format!("{body}\n// edit")).expect("edit");
        let mut fake = crate::embed::fake::FakeEmbedder::new(16);
        crate::index::build::build_index(
            &cfg,
            &mut fake,
            &crate::index::build::test_support::opts(),
            None,
            &mut |_| {},
        )
        .expect("rebuild");

        let mut args2 = serde_json::Map::new();
        args2.insert("entity_name".to_string(), serde_json::json!("target"));
        handler
            .dispatch(call_params("find_callers", args2))
            .await
            .expect("2");
        assert_eq!(services.index.loads(), 2);
    }

    #[tokio::test]
    async fn query_dispatch_reports_not_indexed() {
        let (_tmp, handler) = tempdir_handler();
        let mut args = serde_json::Map::new();
        args.insert("entity_name".to_string(), serde_json::json!("foo"));
        let err = handler
            .dispatch(call_params("find_callers", args))
            .await
            .expect_err("no index in an empty repo");
        assert!(err.to_string().contains("has no index"), "{err}");
    }

    #[test]
    fn server_details_are_complete() {
        let details = build_server_details();
        assert_eq!(details.server_info.name, "knot-lite");
        assert!(details.server_info.version.contains('.'));
        let instructions = details.instructions.unwrap_or_default();
        for tool in EXPECTED_TOOLS {
            assert!(instructions.contains(tool), "instructions miss {tool}");
        }
        assert!(details.capabilities.tools.is_some());
    }
}
