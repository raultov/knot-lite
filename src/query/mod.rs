//! Query-time machinery shared by the CLI and the MCP server: cached
//! services, parameter filters, the renderer and the query bodies.

pub mod callers;
pub mod coverage;
pub mod edits;
pub mod explore;
pub mod explore_refs;
pub mod files;
pub mod filters;
pub mod fusion;
pub mod hybrid;
pub mod outline;
pub mod pool;
pub mod render;
pub mod search;
pub mod session;
pub mod sources;
