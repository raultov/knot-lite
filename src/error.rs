//! Typed diagnostics for query tools that cannot answer.
//!
//! The query tools fail as **tool errors** (`isError: true` on the wire,
//! P1-D12), never as protocol errors, so the LLM can read the diagnostic and
//! self-correct. The CLI maps these to exit code [`CLI_EXIT_QUERY_UNAVAILABLE`].
//!
//! [`IndexUnavailable`] says why the *index* cannot serve a query, independent
//! of the tool; [`IndexUnavailable::for_tool`] turns it into the
//! [`QueryUnavailable`] the caller sees, carrying the tool and repository name.

use thiserror::Error;

/// Exit code used by the CLI for every [`QueryUnavailable`] diagnostic.
pub const CLI_EXIT_QUERY_UNAVAILABLE: u8 = 2;

/// Why the index cannot serve a query (tool-independent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexUnavailable {
    /// No manifest and no leftovers of an interrupted run.
    Absent {
        /// Repo-relative manifest path.
        manifest: String,
    },
    /// An `index` run is in progress or did not finish.
    Incomplete,
    /// The manifest was written by a different schema generation.
    Unsupported {
        /// Manifest schema found.
        schema: String,
        /// Schema this version understands.
        expected: String,
    },
    /// The manifest exists but cannot be read or parsed.
    Invalid {
        /// Repo-relative manifest path.
        manifest: String,
        /// One-line I/O or TOML error.
        error: String,
    },
    /// A shard or edge file is unreadable or malformed.
    Corrupt {
        /// Repo-relative path of the offending file.
        path: String,
        /// One-line error.
        error: String,
    },
    /// The embedding model recorded in the index cannot be loaded.
    ModelUnavailable {
        /// Model name recorded in the manifest.
        model: String,
        /// One-line error.
        error: String,
        /// The model names this build accepts.
        accepted: String,
    },
}

impl IndexUnavailable {
    /// Attach the tool and repository names for the caller-facing diagnostic.
    pub fn for_tool(self, tool: &str, repo: &str) -> QueryUnavailable {
        let tool = tool.to_owned();
        let repo = repo.to_owned();
        match self {
            Self::Absent { manifest } => QueryUnavailable::NotIndexed {
                tool,
                repo,
                manifest,
            },
            Self::Incomplete => QueryUnavailable::Incomplete { tool, repo },
            Self::Unsupported { schema, expected } => QueryUnavailable::Unsupported {
                tool,
                repo,
                schema,
                expected,
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            Self::Invalid { manifest, error } => QueryUnavailable::Invalid {
                tool,
                repo,
                manifest,
                error,
            },
            Self::Corrupt { path, error } => QueryUnavailable::Corrupt {
                tool,
                repo,
                path,
                error,
            },
            Self::ModelUnavailable {
                model,
                error,
                accepted,
            } => QueryUnavailable::ModelUnavailable {
                tool,
                repo,
                model,
                error,
                accepted,
            },
        }
    }
}

/// Why a query tool cannot answer.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum QueryUnavailable {
    /// No `.knot-lite/manifest.toml` exists for the repository.
    #[error(
        "{tool}: knot-lite has no index for repository '{repo}' ({manifest} not found). Build it with `knot-lite index` (CLI) in the repository root or with --repo-path; call `status` for this repository's capability matrix."
    )]
    NotIndexed {
        /// Tool name as registered.
        tool: String,
        /// Repository name.
        repo: String,
        /// Repo-relative manifest path.
        manifest: String,
    },
    /// An indexing run is in progress or did not finish.
    #[error(
        "{tool}: the knot-lite index of repository '{repo}' is incomplete (an indexing run is in progress or did not finish). Re-run `knot-lite index`."
    )]
    Incomplete {
        /// Tool name as registered.
        tool: String,
        /// Repository name.
        repo: String,
    },
    /// The manifest was written by a different schema generation.
    #[error(
        "{tool}: the index of repository '{repo}' uses schema '{schema}', which knot-lite {version} does not support (expected '{expected}'). Re-run `knot-lite index --force` with this knot-lite version."
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
        "{tool}: the index manifest of repository '{repo}' ({manifest}) could not be read: {error}. Re-run `knot-lite index --force`."
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
    /// A shard or edge file is unreadable or malformed.
    #[error(
        "{tool}: the knot-lite index of repository '{repo}' is corrupt ({path}: {error}). Re-run `knot-lite index --force`."
    )]
    Corrupt {
        /// Tool name as registered.
        tool: String,
        /// Repository name.
        repo: String,
        /// Repo-relative path of the offending file.
        path: String,
        /// One-line error.
        error: String,
    },
    /// The embedding model recorded in the index cannot be loaded.
    #[error(
        "{tool}: the embedding model '{model}' recorded in the index of repository '{repo}' cannot be loaded: {error}. Re-run `knot-lite index --force` with a supported model (accepted: {accepted})."
    )]
    ModelUnavailable {
        /// Tool name as registered.
        tool: String,
        /// Repository name.
        repo: String,
        /// Model name recorded in the manifest.
        model: String,
        /// One-line error.
        error: String,
        /// The model names this build accepts.
        accepted: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(unavailable: IndexUnavailable) -> String {
        unavailable.for_tool("find_callers", "demo").to_string()
    }

    #[test]
    fn display_texts_are_stable() {
        assert_eq!(
            text(IndexUnavailable::Absent {
                manifest: ".knot-lite/manifest.toml".to_owned()
            }),
            "find_callers: knot-lite has no index for repository 'demo' (.knot-lite/manifest.toml not found). Build it with `knot-lite index` (CLI) in the repository root or with --repo-path; call `status` for this repository's capability matrix."
        );
        assert_eq!(
            text(IndexUnavailable::Incomplete),
            "find_callers: the knot-lite index of repository 'demo' is incomplete (an indexing run is in progress or did not finish). Re-run `knot-lite index`."
        );
        assert_eq!(
            text(IndexUnavailable::Unsupported {
                schema: "knot-lite/manifest/v2".to_owned(),
                expected: "knot-lite/manifest/v1".to_owned(),
            }),
            format!(
                "find_callers: the index of repository 'demo' uses schema 'knot-lite/manifest/v2', which knot-lite {} does not support (expected 'knot-lite/manifest/v1'). Re-run `knot-lite index --force` with this knot-lite version.",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(
            text(IndexUnavailable::Invalid {
                manifest: ".knot-lite/manifest.toml".to_owned(),
                error: "boom".to_owned(),
            }),
            "find_callers: the index manifest of repository 'demo' (.knot-lite/manifest.toml) could not be read: boom. Re-run `knot-lite index --force`."
        );
        assert_eq!(
            text(IndexUnavailable::Corrupt {
                path: ".knot-lite/shards/a.rs.jsonl".to_owned(),
                error: "line 3".to_owned(),
            }),
            "find_callers: the knot-lite index of repository 'demo' is corrupt (.knot-lite/shards/a.rs.jsonl: line 3). Re-run `knot-lite index --force`."
        );
        assert_eq!(
            text(IndexUnavailable::ModelUnavailable {
                model: "AllMiniLML6V2".to_owned(),
                error: "no network".to_owned(),
                accepted: "AllMiniLML6V2, BGEBaseENV15".to_owned(),
            }),
            "find_callers: the embedding model 'AllMiniLML6V2' recorded in the index of repository 'demo' cannot be loaded: no network. Re-run `knot-lite index --force` with a supported model (accepted: AllMiniLML6V2, BGEBaseENV15)."
        );
    }

    #[test]
    fn for_tool_carries_tool_and_repo_into_every_variant() {
        let variants = [
            IndexUnavailable::Absent {
                manifest: "m".into(),
            },
            IndexUnavailable::Incomplete,
            IndexUnavailable::Unsupported {
                schema: "s".into(),
                expected: "e".into(),
            },
            IndexUnavailable::Invalid {
                manifest: "m".into(),
                error: "x".into(),
            },
            IndexUnavailable::Corrupt {
                path: "p".into(),
                error: "x".into(),
            },
            IndexUnavailable::ModelUnavailable {
                model: "m".into(),
                error: "x".into(),
                accepted: "a".into(),
            },
        ];
        for variant in variants {
            let message = variant.clone().for_tool("list_files", "other").to_string();
            assert!(
                message.starts_with("list_files: "),
                "{variant:?}: {message}"
            );
            assert!(message.contains("'other'"), "{variant:?}: {message}");
        }
    }

    #[test]
    fn exit_code_is_two() {
        assert_eq!(CLI_EXIT_QUERY_UNAVAILABLE, 2);
    }
}
