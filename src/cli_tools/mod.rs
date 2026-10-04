//! Business logic entry points (`run_*`) and the CLI driver.

pub mod query;
pub mod status;

pub use query::{run_explore_file, run_find_callers, run_list_files, run_search_hybrid_context};
pub use status::{format_status_output, run_status, run_status_with};

use anyhow::Result;

use crate::config::{Config, OutputFormat};
use crate::error::{CLI_EXIT_QUERY_UNAVAILABLE, QueryUnavailable};
use crate::models::cli_args::{Cli, Commands};
use crate::models::params::{CallersParams, ExploreParams, FilesParams, SearchParams};
use crate::models::repo_scope::check_repo_scope_str;

/// Run one CLI command and return its rendered output (printed by the binary).
pub fn run_cli(cli: Cli) -> Result<String> {
    let cfg = Config::load_cli(&cli.common)?;
    match cli.command {
        Commands::Status { output } => {
            let report = run_status(&cfg)?;
            Ok(format_status_output(&report, output))
        }
        Commands::Search {
            query,
            max_results,
            repo,
            kinds,
            path,
            output,
        } => {
            check_scope(&cfg, repo.as_deref())?;
            let params = SearchParams::from_cli(query, max_results, kinds, path);
            let value = run_search_hybrid_context(&cfg, &params)?;
            Ok(render_query(value, output))
        }
        Commands::Callers {
            entity_name,
            repo,
            max_targets,
            kinds,
            output,
        } => {
            check_scope(&cfg, repo.as_deref())?;
            let params = CallersParams::from_cli(entity_name, max_targets, kinds);
            let value = run_find_callers(&cfg, &params)?;
            Ok(render_query(value, output))
        }
        Commands::Explore {
            file_path,
            repo,
            include_markup,
            output,
        } => {
            check_scope(&cfg, repo.as_deref())?;
            let params = ExploreParams::from_cli(file_path, include_markup);
            let value = run_explore_file(&cfg, &params)?;
            Ok(render_query(value, output))
        }
        Commands::Files { path, repo, output } => {
            check_scope(&cfg, repo.as_deref())?;
            let params = FilesParams::from_cli(path);
            let value = run_list_files(&cfg, &params)?;
            Ok(render_query(value, output))
        }
    }
}

/// Map an error to the process exit code (§9.3).
pub fn exit_code_for(error: &anyhow::Error) -> u8 {
    if error.downcast_ref::<QueryUnavailable>().is_some() {
        CLI_EXIT_QUERY_UNAVAILABLE
    } else {
        1
    }
}

fn check_scope(cfg: &Config, repo: Option<&str>) -> Result<()> {
    check_repo_scope_str(repo, &cfg.repo_name).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Render a query result.
///
/// Phase 1 never reaches this: every query answers [`QueryUnavailable`] and the
/// CLI exits with code 2. The `--output` flag of the query subcommands is
/// accepted for compatibility but unused until Phase 3/4 define the query
/// result formats.
fn render_query(value: serde_json::Value, _output: OutputFormat) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CommonArgs;

    fn cli_for(tmp: &tempfile::TempDir, command: Commands) -> Cli {
        Cli {
            common: CommonArgs {
                repo_path: Some(tmp.path().to_string_lossy().into_owned()),
                repo_name: Some("demo".to_string()),
            },
            command,
        }
    }

    #[test]
    fn run_cli_status_json_on_tempdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(
            &tmp,
            Commands::Status {
                output: OutputFormat::Json,
            },
        );
        let out = run_cli(cli).expect("status must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("JSON output");
        assert_eq!(
            parsed.get("schema").and_then(serde_json::Value::as_str),
            Some("knot-lite/status/v1")
        );
    }

    #[test]
    fn run_cli_callers_not_indexed_maps_to_exit_2() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(
            &tmp,
            Commands::Callers {
                entity_name: "foo".to_string(),
                repo: None,
                max_targets: None,
                kinds: None,
                output: OutputFormat::Table,
            },
        );
        let err = run_cli(cli).expect_err("no index");
        assert!(err.to_string().contains("has no index"), "{err}");
        assert_eq!(exit_code_for(&err), 2);
    }

    #[test]
    fn run_cli_foreign_repo_flag_errors_with_exit_1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(
            &tmp,
            Commands::Callers {
                entity_name: "foo".to_string(),
                repo: Some("other".to_string()),
                max_targets: None,
                kinds: None,
                output: OutputFormat::Table,
            },
        );
        let err = run_cli(cli).expect_err("foreign repo");
        assert!(err.to_string().contains("not served here"), "{err}");
        assert_eq!(exit_code_for(&err), 1);
    }

    #[test]
    fn exit_code_for_plain_error_is_1() {
        let err = anyhow::anyhow!("boom");
        assert_eq!(exit_code_for(&err), 1);
    }
}
