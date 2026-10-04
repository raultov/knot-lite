//! Reading `.knot-lite/manifest.toml` to report the index state.
//!
//! Phase 1 never writes the manifest (Phase 2 owns the writer) and never
//! creates directories or files: reading is strictly side-effect free.

use serde::{Deserialize, Serialize};

use super::layout::{IndexLayout, MANIFEST_SCHEMA_V1};

/// The only fields Phase 1 reads; unknown fields are ignored so Phase 2 can
/// extend the manifest without breaking `status`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ManifestHeader {
    /// Manifest schema identifier, e.g. [`MANIFEST_SCHEMA_V1`].
    pub schema: String,
    /// knot-lite version that wrote the manifest.
    pub knot_lite_version: String,
}

/// Index state of a repository, as reported by `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IndexState {
    /// No `.knot-lite/manifest.toml` (the common case in Phase 1).
    Absent,
    /// A manifest this knot-lite version understands.
    Present {
        /// Manifest schema identifier.
        schema: String,
        /// knot-lite version that wrote the manifest.
        knot_lite_version: String,
    },
    /// A manifest written by a future schema.
    Unsupported {
        /// The schema found in the manifest.
        schema: String,
    },
    /// The manifest exists but cannot be read or parsed.
    Invalid {
        /// One-line error message.
        error: String,
    },
}

/// Read the index state from the manifest. Never creates directories or files.
pub fn read_index_state(layout: &IndexLayout) -> IndexState {
    let text = match std::fs::read_to_string(&layout.manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return IndexState::Absent,
        Err(e) => {
            return IndexState::Invalid {
                error: one_line(&e.to_string()),
            };
        }
    };
    match toml::from_str::<ManifestHeader>(&text) {
        Ok(header) => {
            if header.schema == MANIFEST_SCHEMA_V1 {
                IndexState::Present {
                    schema: header.schema,
                    knot_lite_version: header.knot_lite_version,
                }
            } else {
                IndexState::Unsupported {
                    schema: header.schema,
                }
            }
        }
        Err(e) => IndexState::Invalid {
            error: one_line(&e.to_string()),
        },
    }
}

/// Collapse an error message to a single line (kept for stable JSON output).
fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_for(tmp: &tempfile::TempDir) -> IndexLayout {
        IndexLayout::for_repo(tmp.path())
    }

    fn write_manifest(tmp: &tempfile::TempDir, body: &str) {
        let layout = layout_for(tmp);
        std::fs::create_dir_all(&layout.dir).expect("mkdir .knot-lite");
        std::fs::write(&layout.manifest, body).expect("write manifest");
    }

    fn dir_listing(tmp: &tempfile::TempDir) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn absent_when_no_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_index_state(&layout_for(&tmp)), IndexState::Absent);
    }

    #[test]
    fn absent_when_dir_without_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = layout_for(&tmp);
        std::fs::create_dir_all(&layout.dir).expect("mkdir");
        assert_eq!(read_index_state(&layout), IndexState::Absent);
    }

    #[test]
    fn present_for_v1_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Unknown fields and extra tables must be ignored (Phase 2 compatibility).
        write_manifest(
            &tmp,
            "schema = \"knot-lite/manifest/v1\"\nknot_lite_version = \"0.1.0\"\n\n[embedding]\nmodel = \"x\"\n",
        );
        assert_eq!(
            read_index_state(&layout_for(&tmp)),
            IndexState::Present {
                schema: MANIFEST_SCHEMA_V1.to_string(),
                knot_lite_version: "0.1.0".to_string(),
            }
        );
    }

    #[test]
    fn unsupported_for_other_schema() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(
            &tmp,
            "schema = \"knot-lite/manifest/v2\"\nknot_lite_version = \"9.9.9\"\n",
        );
        assert_eq!(
            read_index_state(&layout_for(&tmp)),
            IndexState::Unsupported {
                schema: "knot-lite/manifest/v2".to_string(),
            }
        );
    }

    #[test]
    fn invalid_for_garbage_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(&tmp, "this is not = = toml\n");
        match read_index_state(&layout_for(&tmp)) {
            IndexState::Invalid { error } => {
                assert!(!error.is_empty());
                assert!(!error.contains('\n'));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn invalid_for_missing_schema_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(&tmp, "knot_lite_version = \"0.1.0\"\n");
        match read_index_state(&layout_for(&tmp)) {
            IndexState::Invalid { error } => assert!(error.contains("schema")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn serializes_with_state_tag() {
        let json = serde_json::to_string(&IndexState::Absent).expect("serialize");
        assert_eq!(json, "{\"state\":\"absent\"}");
    }

    #[test]
    fn read_is_side_effect_free() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let before = dir_listing(&tmp);
        let _ = read_index_state(&layout_for(&tmp));
        assert_eq!(before, dir_listing(&tmp));
    }
}
