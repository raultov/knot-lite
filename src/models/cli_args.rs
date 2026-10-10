//! CLI argument definitions for the `knot-lite` CLI tool.
//!
//! Defines the `Cli` struct and `Commands` enum used by `clap` for parsing
//! subcommands: index, status, search, callers, explore, and files. Flags, short
//! options and defaults mirror knot's `src/models/cli_args.rs` where applicable.

use clap::{Args, Parser, Subcommand};

use crate::config::{CommonArgs, OutputFormat};
use crate::lsp::precompute::{PrecomputePolicy, Selection};

#[derive(Parser)]
#[command(name = "knot-lite")]
#[command(about = "Database-free code intelligence for one repository", long_about = None)]
#[command(version)]
pub struct Cli {
    /// Shared repository flags (`--repo-path`, `--repo-name`).
    #[command(flatten)]
    pub common: CommonArgs,

    #[command(subcommand)]
    pub command: Commands,
}

/// Arguments for the `index` subcommand.
#[derive(Clone, PartialEq, Eq, Debug, Args)]
pub struct IndexArgs {
    /// Re-embed every record (ignore reusable vectors)
    #[arg(long)]
    pub force: bool,

    /// Track the index in git: write .knot-lite/.gitattributes instead of .gitignore
    #[arg(long, conflicts_with = "gitignore")]
    pub commit: bool,

    /// Keep the index out of git (default): write .knot-lite/.gitignore
    #[arg(long)]
    pub gitignore: bool,

    /// Embedding model (overrides KNOT_LITE_EMBED_MODEL / KNOT_EMBED_MODEL)
    #[arg(long)]
    pub embed_model: Option<String>,

    /// Parallel parser threads (default: available CPUs)
    #[arg(short, long)]
    pub jobs: Option<usize>,

    /// Passages per embedding call
    #[arg(long, default_value_t = 128)]
    pub batch_size: usize,

    /// No progress on stderr
    #[arg(short, long)]
    pub quiet: bool,

    /// Pre-compute language-server callers for widely used targets: auto = Java
    /// only (default), all = every language with a server, none = skip
    #[arg(long, value_enum, default_value_t = PrecomputePolicy::Auto)]
    pub lsp_precompute: PrecomputePolicy,

    /// Minimum heuristic caller count for a target to be pre-computed (default 20)
    #[arg(long, default_value_t = 20)]
    pub lsp_min_fanin: usize,

    /// Maximum pre-computed targets per language (default 200)
    #[arg(long, default_value_t = 200)]
    pub lsp_max_targets: usize,

    /// Time budget for pre-computation per language, in seconds (default 600)
    #[arg(long, default_value_t = 600)]
    pub lsp_budget_secs: u64,

    /// How pre-computation targets are chosen: heuristic = heuristic fan-in only
    /// (default), lsp-refs = probe the language server's reference counts first
    #[arg(long, value_enum, default_value_t = Selection::Heuristic)]
    pub lsp_selection: Selection,

    /// Heuristic caller count a non-type candidate needs to be probed (default 3)
    #[arg(long, default_value_t = 3)]
    pub lsp_probe_min_fanin: usize,

    /// Output format (default: table)
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,
}

#[derive(Clone, PartialEq, Eq, Debug, Subcommand)]
pub enum Commands {
    /// Build or update the index under <repo>/.knot-lite/ (the only command that writes)
    Index(IndexArgs),

    /// Show the capability matrix (tiers per language) and the index state
    Status {
        /// Start each detected language server once, report readiness time,
        /// version and memory, then stop it
        #[arg(long)]
        probe_lsp: bool,

        /// Output format (default: table)
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
    },

    /// Search for code entities by semantic meaning
    Search {
        /// Search query (e.g., 'user authentication', 'API error handling')
        query: String,

        /// Maximum number of results to return (default: 5, max: 100;
        /// larger values are clamped)
        #[arg(short, long, default_value = "5")]
        max_results: usize,

        /// Repository scope: one name, comma-separated list, or 'all'/'*'
        #[arg(short, long)]
        repo: Option<String>,

        /// Optional entity-kind filter: exact kinds ('rust_function',
        /// 'markdown_section', ...) or aliases ('definition', 'callable',
        /// 'class', 'type'). Comma-separated for multiple values.
        #[arg(short, long)]
        kinds: Option<String>,

        /// Optional path filter: repo-relative directory prefix ('src/api',
        /// matched on a path boundary) or glob pattern (segments: `*` segment
        /// wildcard, `**` any depth, `?` one char).
        #[arg(short, long)]
        path: Option<String>,

        /// Output format (default: table)
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
    },

    /// Find all references to an entity (reverse dependency lookup)
    Callers {
        /// Entity name to find references for. Matching is precedence-based:
        /// exact FQN → FQN suffix ('Type.member', 'module::name') → exact name.
        entity_name: String,

        /// Repository scope: one name, comma-separated list, or 'all'/'*'
        #[arg(short, long)]
        repo: Option<String>,

        /// Maximum number of resolved targets (default: 25, max: 500). Raise
        /// when the response reports a truncated target list.
        #[arg(short = 'm', long)]
        max_targets: Option<usize>,

        /// Optional entity-kind filter for target resolution: 'all'/'*' to
        /// disable the default code-only scope (docs/config/build metadata
        /// stay hidden by default), or exact kinds/aliases ('callable',
        /// 'config', 'rust_function', ...) comma-separated for more values.
        #[arg(short = 'k', long)]
        kinds: Option<String>,

        /// Do not ask language servers during this call (pre-computed
        /// language-server results in the index are still used)
        #[arg(long)]
        no_lsp: bool,

        /// Output format (default: table)
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
    },

    /// Explore all entities in a source file
    Explore {
        /// Path to the source file
        file_path: String,

        /// Repository scope: one name, comma-separated list, or 'all'/'*'
        #[arg(short, long)]
        repo: Option<String>,

        /// Include full markup reference list (class/id attribute tokens)
        #[arg(long)]
        include_markup: bool,

        /// Output format (default: table)
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
    },

    /// List the indexed files of the repository (read-only), optionally under a
    /// directory prefix (`src/api`) or matching a glob (`src/**/*.rs`)
    Files {
        /// Optional repo-relative directory prefix ('src/api', matched on a path
        /// boundary) or glob pattern (segments: `*` segment wildcard, `**` any
        /// depth, `?` one char)
        #[arg(short, long)]
        path: Option<String>,

        /// Repository scope: one name, comma-separated list, or 'all'/'*'
        #[arg(short, long)]
        repo: Option<String>,

        /// Output format (default: table)
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let mut full = vec!["knot-lite"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).expect("parse")
    }

    #[test]
    fn index_defaults() {
        let cli = parse(&["index"]);
        match cli.command {
            Commands::Index(args) => {
                assert!(!args.force);
                assert!(!args.commit);
                assert!(!args.gitignore);
                assert_eq!(args.embed_model, None);
                assert_eq!(args.jobs, None);
                assert_eq!(args.batch_size, 128);
                assert!(!args.quiet);
                assert_eq!(args.output, OutputFormat::Table);
                assert_eq!(args.lsp_precompute, PrecomputePolicy::Auto);
                assert_eq!(args.lsp_selection, Selection::Heuristic);
                assert_eq!(args.lsp_probe_min_fanin, 3);
            }
            other => panic!("expected Index, got {other:?}"),
        }
    }

    #[test]
    fn index_selection_flags_parse_and_reject_invalid() {
        let cli = parse(&[
            "index",
            "--lsp-selection",
            "heuristic",
            "--lsp-probe-min-fanin",
            "5",
        ]);
        match cli.command {
            Commands::Index(args) => {
                assert_eq!(args.lsp_selection, Selection::Heuristic);
                assert_eq!(args.lsp_probe_min_fanin, 5);
            }
            other => panic!("expected Index, got {other:?}"),
        }
        assert_eq!(
            match parse(&["index", "--lsp-selection", "lsp-refs"]).command {
                Commands::Index(args) => args.lsp_selection,
                other => panic!("expected Index, got {other:?}"),
            },
            Selection::LspRefs
        );
        assert!(Cli::try_parse_from(["knot-lite", "index", "--lsp-selection", "fanin"]).is_err());
    }

    #[test]
    fn index_commit_conflicts_with_gitignore() {
        assert!(Cli::try_parse_from(["knot-lite", "index", "--commit", "--gitignore"]).is_err());
    }

    #[test]
    fn index_all_flags_parse() {
        let cli = parse(&[
            "index",
            "--force",
            "--commit",
            "--embed-model",
            "BGEBaseENV15",
            "-j",
            "4",
            "--batch-size",
            "64",
            "-q",
            "-o",
            "json",
        ]);
        match cli.command {
            Commands::Index(args) => {
                assert!(args.force);
                assert!(args.commit);
                assert_eq!(args.embed_model.as_deref(), Some("BGEBaseENV15"));
                assert_eq!(args.jobs, Some(4));
                assert_eq!(args.batch_size, 64);
                assert!(args.quiet);
                assert_eq!(args.output, OutputFormat::Json);
            }
            other => panic!("expected Index, got {other:?}"),
        }
    }

    #[test]
    fn index_lsp_flags_parse_with_defaults() {
        match parse(&["index"]).command {
            Commands::Index(args) => {
                assert_eq!(
                    (
                        args.lsp_precompute,
                        args.lsp_min_fanin,
                        args.lsp_max_targets,
                        args.lsp_budget_secs
                    ),
                    (PrecomputePolicy::Auto, 20, 200, 600)
                );
            }
            other => panic!("expected Index, got {other:?}"),
        }
        match parse(&[
            "index",
            "--lsp-precompute",
            "all",
            "--lsp-min-fanin",
            "5",
            "--lsp-max-targets",
            "50",
            "--lsp-budget-secs",
            "30",
        ])
        .command
        {
            Commands::Index(args) => {
                assert_eq!(
                    (
                        args.lsp_precompute,
                        args.lsp_min_fanin,
                        args.lsp_max_targets,
                        args.lsp_budget_secs
                    ),
                    (PrecomputePolicy::All, 5, 50, 30)
                );
            }
            other => panic!("expected Index, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["knot-lite", "index", "--lsp-precompute", "java"]).is_err());
    }

    #[test]
    fn status_parses_with_default_output() {
        let cli = parse(&["status"]);
        assert_eq!(
            cli.command,
            Commands::Status {
                probe_lsp: false,
                output: OutputFormat::Table
            }
        );
    }

    #[test]
    fn status_probe_flag_parses() {
        assert_eq!(
            parse(&["status", "--probe-lsp", "-o", "json"]).command,
            Commands::Status {
                probe_lsp: true,
                output: OutputFormat::Json
            }
        );
    }

    #[test]
    fn search_parses_every_flag() {
        let cli = parse(&[
            "search",
            "q",
            "-m",
            "10",
            "-r",
            "demo",
            "-k",
            "rust_function",
            "-p",
            "src/",
            "-o",
            "json",
        ]);
        match cli.command {
            Commands::Search {
                query,
                max_results,
                repo,
                kinds,
                path,
                output,
            } => {
                assert_eq!(query, "q");
                assert_eq!(max_results, 10);
                assert_eq!(repo.as_deref(), Some("demo"));
                assert_eq!(kinds.as_deref(), Some("rust_function"));
                assert_eq!(path.as_deref(), Some("src/"));
                assert_eq!(output, OutputFormat::Json);
            }
            other => panic!("expected Search, got {other:?}"),
        }
    }

    #[test]
    fn search_defaults_max_results_to_five() {
        match parse(&["search", "q"]).command {
            Commands::Search { max_results, .. } => assert_eq!(max_results, 5),
            other => panic!("expected Search, got {other:?}"),
        }
    }

    #[test]
    fn callers_parses_every_flag() {
        match parse(&[
            "callers", "E", "-r", "demo", "-m", "3", "-k", "callable", "-o", "markdown",
        ])
        .command
        {
            Commands::Callers {
                entity_name,
                repo,
                max_targets,
                kinds,
                no_lsp,
                output,
            } => {
                assert_eq!(entity_name, "E");
                assert_eq!(repo.as_deref(), Some("demo"));
                assert_eq!(max_targets, Some(3));
                assert_eq!(kinds.as_deref(), Some("callable"));
                assert!(!no_lsp);
                assert_eq!(output, OutputFormat::Markdown);
            }
            other => panic!("expected Callers, got {other:?}"),
        }
    }

    #[test]
    fn callers_no_lsp_flag_parses() {
        match parse(&["callers", "E", "--no-lsp"]).command {
            Commands::Callers { no_lsp, .. } => assert!(no_lsp),
            other => panic!("expected Callers, got {other:?}"),
        }
    }

    #[test]
    fn explore_parses_every_flag() {
        match parse(&[
            "explore",
            "src/lib.rs",
            "-r",
            "demo",
            "--include-markup",
            "-o",
            "json",
        ])
        .command
        {
            Commands::Explore {
                file_path,
                repo,
                include_markup,
                output,
            } => {
                assert_eq!(file_path, "src/lib.rs");
                assert_eq!(repo.as_deref(), Some("demo"));
                assert!(include_markup);
                assert_eq!(output, OutputFormat::Json);
            }
            other => panic!("expected Explore, got {other:?}"),
        }
    }

    #[test]
    fn files_parses_every_flag() {
        match parse(&["files", "-p", "src/", "-r", "demo"]).command {
            Commands::Files { path, repo, output } => {
                assert_eq!(path.as_deref(), Some("src/"));
                assert_eq!(repo.as_deref(), Some("demo"));
                assert_eq!(output, OutputFormat::Table);
            }
            other => panic!("expected Files, got {other:?}"),
        }
    }

    #[test]
    fn global_repo_path_accepted_before_and_after_subcommand() {
        let before = parse(&["--repo-path", "/tmp/x", "status"]);
        assert_eq!(before.common.repo_path.as_deref(), Some("/tmp/x"));
        let after = parse(&["status", "--repo-path", "/tmp/x"]);
        assert_eq!(after.common.repo_path.as_deref(), Some("/tmp/x"));
    }

    #[test]
    fn no_subcommand_is_an_error() {
        assert!(Cli::try_parse_from(["knot-lite"]).is_err());
    }
}
