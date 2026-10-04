//! Logging setup (stderr) and shared runtime helpers.
//!
//! All logging goes to **stderr**: stdout carries the CLI result or the MCP
//! stdio stream and must never be polluted (P1-D13 makes this a hard gate).

use anyhow::Result;
use tracing_subscriber::EnvFilter;

/// Default log level for the CLI binary.
pub const CLI_LOG_LEVEL: &str = "error";

/// Default log level for the MCP server binary.
pub const MCP_LOG_LEVEL: &str = "info";

/// Initialise `tracing` with `RUST_LOG` (falling back to `default_level`),
/// writing to stderr without ANSI colours.
pub fn init_logging(default_level: &str) -> Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to initialize logging: {e}"))
}
