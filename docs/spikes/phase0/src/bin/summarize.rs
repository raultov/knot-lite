//! Results report renderer (T10): fills the Appendix E tables of
//! `docs/spikes/phase0-results.md` from the spike JSONs.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use knot_rag_spike_phase0::report;

#[derive(Parser)]
#[command(
    name = "summarize",
    about = "Render the Phase 0 results report tables from the JSON artefacts"
)]
struct Cli {
    /// Directory with the result JSONs.
    #[arg(long, default_value = "results")]
    results: PathBuf,
    /// Report to update (only `<!-- BEGIN:table:… -->` blocks are touched).
    #[arg(long, default_value = "../phase0-results.md")]
    out: PathBuf,
    /// Only verify that every expected table block exists.
    #[arg(long)]
    template_check: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.template_check {
        return report::template_check(&cli.out);
    }
    let data = report::load(&cli.results)?;
    let tables = report::tables(&data);
    report::render(&cli.out, &tables)?;
    println!(
        "rendered {} tables into {}",
        tables.len(),
        cli.out.display()
    );
    Ok(())
}
