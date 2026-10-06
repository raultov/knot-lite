//! Business logic entry points (`run_*`) and the CLI driver.

pub mod index;
pub mod query;
pub mod status;

pub use index::{format_index_output, run_index};
pub use query::{
    format_callers_output, format_explore_output, format_files_output, format_search_output,
    run_explore_file, run_find_callers, run_list_files, run_search_hybrid_context,
};
pub use status::{format_status_output, run_status, run_status_with};

use anyhow::Result;

use crate::config::Config;
use crate::error::{CLI_EXIT_QUERY_UNAVAILABLE, QueryUnavailable};
use crate::models::cli_args::{Cli, Commands};
use crate::models::params::{CallersParams, ExploreParams, FilesParams, IndexParams, SearchParams};
use crate::models::repo_scope::check_repo_scope_str;
use crate::query::session::QueryServices;

/// Run one CLI command and return its rendered output (printed by the binary).
pub fn run_cli(cli: Cli) -> Result<String> {
    let cfg = Config::load_cli(&cli.common)?;
    match cli.command {
        Commands::Index(ref args) => {
            let params = IndexParams::from_cli(args);
            let value = run_index(&cfg, &params)?;
            Ok(format_index_output(&value, args.output))
        }
        Commands::Status { output } => {
            let report = run_status(&cfg)?;
            Ok(format_status_output(&report, output))
        }
        Commands::Search {
            ref query,
            max_results,
            ref repo,
            ref kinds,
            ref path,
            output,
        } => {
            check_scope(&cfg, repo.as_deref())?;
            let services = QueryServices::real(&cfg.embed);
            let params =
                SearchParams::from_cli(query.clone(), max_results, kinds.clone(), path.clone());
            let value = run_search_hybrid_context(&cfg, &services, &params)?;
            Ok(format_search_output(&value, output))
        }
        Commands::Callers {
            ref entity_name,
            ref repo,
            max_targets,
            ref kinds,
            output,
        } => {
            check_scope(&cfg, repo.as_deref())?;
            let services = QueryServices::real(&cfg.embed);
            let params = CallersParams::from_cli(entity_name.clone(), max_targets, kinds.clone());
            let value = run_find_callers(&cfg, &services, &params)?;
            Ok(format_callers_output(&value, output))
        }
        Commands::Explore {
            ref file_path,
            ref repo,
            include_markup,
            output,
        } => {
            check_scope(&cfg, repo.as_deref())?;
            let services = QueryServices::real(&cfg.embed);
            let params = ExploreParams::from_cli(file_path.clone(), include_markup);
            let value = run_explore_file(&cfg, &services, &params)?;
            Ok(format_explore_output(&value, output))
        }
        Commands::Files {
            ref path,
            ref repo,
            output,
        } => {
            check_scope(&cfg, repo.as_deref())?;
            let services = QueryServices::real(&cfg.embed);
            let params = FilesParams::from_cli(path.clone());
            let value = run_list_files(&cfg, &services, &params)?;
            Ok(format_files_output(&value, output))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CommonArgs, OutputFormat};

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

    #[test]
    fn run_cli_files_on_fixture_index_renders_table() {
        let (tmp, cfg) = crate::index::build::test_support::fixture();
        let mut fake = crate::embed::fake::FakeEmbedder::new(16);
        crate::index::build::build_index(
            &cfg,
            &mut fake,
            &crate::index::build::test_support::opts(),
            None,
            &mut |_| {},
        )
        .expect("build");

        let cli = cli_for(
            &tmp,
            Commands::Files {
                path: None,
                repo: None,
                output: OutputFormat::Table,
            },
        );
        let out = run_cli(cli).expect("files must succeed");
        assert!(out.contains("PATH") && out.contains("src/lib.rs"), "{out}");
    }
}
