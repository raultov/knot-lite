//! Transport-independent MCP handler: `tools()` + `dispatch()`.
//!
//! Mirrors knot's `mcp_handler`: the tool surface is a plain function, the
//! transport is only a caller. Binaries stay thin.

use std::sync::Arc;

use rust_mcp_sdk::schema::{CallToolError, Tool};

use crate::config::Config;
use crate::mcp_tools::explore_file::ExploreFileTool;
use crate::mcp_tools::find_callers::FindCallersTool;
use crate::mcp_tools::list_files::ListFilesTool;
use crate::mcp_tools::search_hybrid_context::SearchHybridContextTool;
use crate::mcp_tools::status::StatusTool;

/// Refusal message for every tool call in dry-run mode.
pub const DRY_RUN_MESSAGE: &str = "Server is running in dry-run mode. The repository is not opened. This mode is used for protocol validation and quality checks only.";

/// The MCP handler: one server, one repository (P1-D7).
pub struct KnotLiteMcpHandler {
    /// The served repository (absent in dry-run mode).
    pub config: Option<Arc<Config>>,
    /// Whether to answer without opening the repository.
    pub dry_run: bool,
}

impl KnotLiteMcpHandler {
    /// A handler serving one repository.
    pub fn new(config: Config) -> Self {
        Self {
            config: Some(Arc::new(config)),
            dry_run: false,
        }
    }

    /// A handler that refuses every tool (protocol validation only).
    pub fn new_dry_run() -> Self {
        Self {
            config: None,
            dry_run: true,
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
}
