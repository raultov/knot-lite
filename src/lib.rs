//! knot-lite — database-free code intelligence for one repository.
//!
//! Mirrors knot's architecture: business logic in `cli_tools::run_*`, thin MCP
//! wrappers in `mcp_tools`, a transport-independent `mcp_handler`
//! (`tools()` + `dispatch()`), and minimal binaries.

pub mod capability;
pub mod cli_tools;
pub mod config;
pub mod error;
pub mod index;
pub mod mcp_handler;
pub mod mcp_tools;
pub mod models;
pub mod utils;
