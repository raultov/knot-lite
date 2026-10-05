//! The file-based index under `<repo>/.knot-lite/`: layout, manifest, file
//! selection, inventory and the record writers.

pub mod build;
pub mod chunk;
pub mod edges;
pub mod encode;
#[doc(hidden)]
pub mod fixture;
mod hash;
pub mod inventory;
pub mod layout;
pub mod manifest;
pub mod record_io;
pub mod select;
pub mod shard;

pub use layout::{CommitMode, INDEX_DIR, IndexLayout, MANIFEST_FILE, MANIFEST_SCHEMA_V1};
pub use manifest::{IndexState, Manifest, ManifestHeader, read_index_state};
