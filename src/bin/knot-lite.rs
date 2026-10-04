//! `knot-lite` command-line binary.

use clap::Parser as _;

use knot_lite::cli_tools::{exit_code_for, run_cli};
use knot_lite::models::cli_args::Cli;
use knot_lite::utils;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    utils::init_logging(utils::CLI_LOG_LEVEL).ok();
    let cli = Cli::parse();
    match run_cli(cli) {
        Ok(out) => {
            print!("{out}");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::from(exit_code_for(&e))
        }
    }
}
