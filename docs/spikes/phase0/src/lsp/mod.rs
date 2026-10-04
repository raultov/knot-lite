//! LSP client stack for the S1 spike: launch specs (T4.1), async-lsp client
//! (T4.2), readiness detection (T4.3) and process-tree RSS sampling (T4.4).

pub mod client;
pub mod readiness;
pub mod rss;
pub mod servers;
