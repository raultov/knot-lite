//! Shared model types: CLI argument definitions, tool parameter structs and
//! the single-repository scope check.

pub mod cli_args;
pub mod params;
pub mod repo_scope;

pub use params::{CallersParams, ExploreParams, FilesParams, SearchParams};
