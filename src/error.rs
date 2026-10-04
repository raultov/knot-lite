//! Typed diagnostics for query tools that cannot run without an index.
//!
//! The query tools fail as **tool errors** (`isError: true` on the wire,
//! P1-D12), never as protocol errors, so the LLM can read the diagnostic and
//! self-correct. The CLI maps these to exit code [`CLI_EXIT_QUERY_UNAVAILABLE`].

use thiserror::Error;

/// Exit code used by the CLI for every [`QueryUnavailable`] diagnostic.
pub const CLI_EXIT_QUERY_UNAVAILABLE: u8 = 2;

/// Why a query tool cannot answer yet.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum QueryUnavailable {
    /// No `.knot-lite/manifest.toml` exists for the repository.
    #[error(
        "{tool}: knot-lite has no index for repository '{repo}' ({manifest} not found). Indexing is not available in knot-lite {version} yet. Use grep or your editor's language server meanwhile; call `status` for this repository's capability matrix."
    )]
    NotIndexed {
        /// Tool name as registered.
        tool: String,
        /// Repository name.
        repo: String,
        /// Repo-relative manifest path.
        manifest: String,
        /// Running knot-lite version.
        version: String,
    },
    /// A supported index exists but Phase 1 cannot query indexes yet.
    #[error(
        "{tool}: knot-lite found an index for repository '{repo}' (schema {schema}, written by knot-lite {written_by}) but knot-lite {version} cannot query indexes yet."
    )]
    NotImplemented {
        /// Tool name as registered.
        tool: String,
        /// Repository name.
        repo: String,
        /// Manifest schema found.
        schema: String,
        /// Version that wrote the manifest.
        written_by: String,
        /// Running knot-lite version.
        version: String,
    },
    /// The manifest was written by a different schema generation.
    #[error(
        "{tool}: the index of repository '{repo}' uses schema '{schema}', which knot-lite {version} does not support (expected '{expected}'). Re-index with a matching knot-lite version."
    )]
    Unsupported {
        /// Tool name as registered.
        tool: String,
        /// Repository name.
        repo: String,
        /// Manifest schema found.
        schema: String,
        /// Schema this version understands.
        expected: String,
        /// Running knot-lite version.
        version: String,
    },
    /// The manifest exists but cannot be read or parsed.
    #[error(
        "{tool}: the index manifest of repository '{repo}' ({manifest}) could not be read: {error}"
    )]
    Invalid {
        /// Tool name as registered.
        tool: String,
        /// Repository name.
        repo: String,
        /// Repo-relative manifest path.
        manifest: String,
        /// One-line I/O or TOML error.
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_texts_are_stable() {
        let not_indexed = QueryUnavailable::NotIndexed {
            tool: "find_callers".to_string(),
            repo: "demo".to_string(),
            manifest: ".knot-lite/manifest.toml".to_string(),
            version: "0.1.0".to_string(),
        };
        let text = not_indexed.to_string();
        assert!(text.starts_with("find_callers:"));
        assert!(text.contains("'demo'"));
        assert!(text.contains("has no index"));
        assert!(text.contains("`status`"));

        let not_implemented = QueryUnavailable::NotImplemented {
            tool: "search_hybrid_context".to_string(),
            repo: "demo".to_string(),
            schema: "knot-lite/manifest/v1".to_string(),
            written_by: "0.1.0".to_string(),
            version: "0.1.0".to_string(),
        };
        assert!(
            not_implemented
                .to_string()
                .contains("cannot query indexes yet")
        );

        let unsupported = QueryUnavailable::Unsupported {
            tool: "list_files".to_string(),
            repo: "demo".to_string(),
            schema: "knot-lite/manifest/v2".to_string(),
            expected: "knot-lite/manifest/v1".to_string(),
            version: "0.1.0".to_string(),
        };
        assert!(unsupported.to_string().contains("does not support"));

        let invalid = QueryUnavailable::Invalid {
            tool: "explore_file".to_string(),
            repo: "demo".to_string(),
            manifest: ".knot-lite/manifest.toml".to_string(),
            error: "boom".to_string(),
        };
        assert!(invalid.to_string().contains("could not be read: boom"));
    }

    #[test]
    fn exit_code_is_two() {
        assert_eq!(CLI_EXIT_QUERY_UNAVAILABLE, 2);
    }
}
