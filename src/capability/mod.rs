//! Repository capability detection: language labels, language-server lookup,
//! file scanning and the tier model behind `status`.

pub mod languages;
pub mod lsp_registry;
pub mod scan;
pub mod tier;
pub mod which;

pub use languages::{LspLang, label_for, lsp_lang_for_label};
pub use lsp_registry::{LspStatus, detect, install_hint};
pub use scan::{ExcludeReason, FileClass, ScanResult, scan_repository};
pub use tier::{STATUS_SCHEMA_V1, StatusReport, TierRow, build_report};
