//! Business logic entry points (`run_*`) and the CLI driver.

pub mod index;
pub mod query;
pub mod status;

pub use index::{format_index_output, run_index};
pub use query::{
    format_callers_output, format_explore_output, format_files_output, format_search_output,
    run_explore_file, run_find_callers, run_list_files, run_search_hybrid_context,
};
pub use status::{
    StatusExtras, format_status_output, run_status, run_status_probing, run_status_with,
    run_status_with_extras,
};

use std::io::IsTerminal as _;

use anyhow::Result;

use crate::config::Config;
use crate::error::{CLI_EXIT_QUERY_UNAVAILABLE, QueryUnavailable};
use crate::lsp::pool::PoolSnapshot;
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
        Commands::Status { output, probe_lsp } => {
            let report = if probe_lsp {
                run_status_probing(&cfg)?
            } else {
                run_status(&cfg)?
            };
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
            no_lsp,
            output,
        } => {
            check_scope(&cfg, repo.as_deref())?;
            let services = QueryServices::real(&cfg.embed);
            let params =
                CallersParams::from_cli(entity_name.clone(), max_targets, kinds.clone(), no_lsp);
            let value = run_find_callers(&cfg, &services, &params)?;
            report_started_servers(&services);
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

/// Tell a person at a terminal which language servers this call had to start
/// (they take seconds; the answer alone would not say why it was slow).
fn report_started_servers(services: &QueryServices) {
    if !std::io::stderr().is_terminal() {
        return;
    }
    for line in services.lsp_runtime().iter().flat_map(started_server_lines) {
        eprintln!("{line}");
    }
}

/// `lsp: rust-analyzer ready in 12.4 s` for every server that is running.
fn started_server_lines(snapshot: &PoolSnapshot) -> Vec<String> {
    snapshot
        .slots
        .values()
        .filter(|slot| slot.state == "ready")
        .filter_map(|slot| {
            let ready_ms = slot.ready_ms?;
            Some(format!(
                "lsp: {} ready in {:.1} s",
                slot.server,
                ready_ms as f64 / 1000.0
            ))
        })
        .collect()
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
                probe_lsp: false,
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
                no_lsp: false,
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
                no_lsp: false,
                output: OutputFormat::Table,
            },
        );
        let err = run_cli(cli).expect_err("foreign repo");
        assert!(err.to_string().contains("not served here"), "{err}");
        assert_eq!(exit_code_for(&err), 1);
    }

    #[test]
    fn started_servers_are_listed_with_their_ready_time() {
        use crate::lsp::pool::SlotSnapshot;

        let slot = |state: &str, server: &str, ready_ms: Option<u64>| SlotSnapshot {
            state: state.to_owned(),
            server: server.to_owned(),
            version: String::new(),
            pid: None,
            ready_ms,
            uptime_s: None,
            rss_mib: None,
            attempts: 0,
            diagnostic: None,
        };
        let snapshot = PoolSnapshot {
            slots: std::collections::BTreeMap::from([
                ("java".to_owned(), slot("not_started", "jdtls", None)),
                (
                    "rust".to_owned(),
                    slot("ready", "rust-analyzer", Some(12_400)),
                ),
                (
                    "ts".to_owned(),
                    slot("failed", "typescript-language-server", None),
                ),
            ]),
        };
        assert_eq!(
            started_server_lines(&snapshot),
            vec!["lsp: rust-analyzer ready in 12.4 s".to_owned()]
        );
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
