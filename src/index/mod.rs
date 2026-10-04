//! Index layout constants and manifest reading (Phase 1 is read-only).

pub mod layout;
pub mod manifest;

pub use layout::{INDEX_DIR, IndexLayout, MANIFEST_FILE, MANIFEST_SCHEMA_V1};
pub use manifest::{IndexState, ManifestHeader, read_index_state};
