//! S1 probe binary (LSP graph spike).
//!
//! `validate-targets` lands with T2.3, `smoke` with T4.5; the measurement `run`
//! subcommand is added by T5. Keep this binary thin: argument parsing plus
//! library calls.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use knot_rag_spike_phase0::common::Lang;
use knot_rag_spike_phase0::graph::gold;
use knot_rag_spike_phase0::groundtruth::{self, TargetStatus};
use knot_rag_spike_phase0::lsp::{client, readiness, rss, servers};
use knot_rag_spike_phase0::probe::{self, RunConfig, RunKind};
use knot_rag_spike_phase0::uri::path_to_uri;

#[derive(Parser)]
#[command(name = "lsp_probe", about = "S1 LSP feasibility probe")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate S1 targets against the snapshot entity table (T2.3).
    ValidateTargets {
        /// Path to a `targets/*.toml` file.
        #[arg(long)]
        targets: PathBuf,
        /// Rewrite the TOML with the resolved line numbers.
        #[arg(long)]
        write: bool,
        /// Parse config files too (T3.1 fidelity experiment against knot's counts).
        #[arg(long)]
        include_config_files: bool,
    },
    /// Start one server, reach ready, run documentSymbol, shut down (T4.5).
    Smoke {
        /// Language server to smoke-test.
        #[arg(long)]
        lang: Lang,
        /// Sandbox snapshot root (never a real repository).
        #[arg(long)]
        root: PathBuf,
        /// Scratch area for server side effects and logs.
        #[arg(long, default_value = "/tmp/knot-rag-spike")]
        spike_root: PathBuf,
        /// File to open and inspect (defaults per language).
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Run the full S1 measurement over one targets file (T5).
    Run {
        /// Path to a `targets/*.toml` file.
        #[arg(long)]
        targets: PathBuf,
        /// Cold wipes the server scratch area first; warm keeps it.
        #[arg(long)]
        run: RunKind,
        /// Result JSON to write.
        #[arg(long)]
        out: PathBuf,
        /// Limit the number of measured targets (quick iterations).
        #[arg(long)]
        max_targets: Option<usize>,
        /// Skip the position-vs-documentSymbol column check.
        #[arg(long)]
        skip_column_check: bool,
        /// Scratch area for server side effects and logs.
        #[arg(long, default_value = "/tmp/knot-rag-spike")]
        spike_root: PathBuf,
        /// Directory holding the raw knot ground-truth JSON.
        #[arg(long, default_value = "groundtruth")]
        groundtruth_dir: PathBuf,
    },
    /// Run the level-2 heuristic graph measurement (T6).
    Heuristic {
        /// Path to a `targets/*.toml` file.
        #[arg(long)]
        targets: PathBuf,
        /// Result JSON to write.
        #[arg(long)]
        out: PathBuf,
    },
    /// Degradation path: no usable LSP, tier-2 answers (T8).
    Nolsp {
        /// Path to a `targets/*.toml` file.
        #[arg(long)]
        targets: PathBuf,
        /// Result JSON to write.
        #[arg(long)]
        out: PathBuf,
        /// Scratch area for server side effects and logs.
        #[arg(long, default_value = "/tmp/knot-rag-spike")]
        spike_root: PathBuf,
        /// Hidden: run the "present but broken" scenario with this program.
        #[arg(long, hide = true)]
        program: Option<PathBuf>,
    },
    /// Gold set + metrics over LSP / knot_db / heuristic (T7).
    Gold {
        /// Language whose S1/heuristic results to compare.
        #[arg(long)]
        lang: Lang,
        /// Path to a `targets/*.toml` file.
        #[arg(long)]
        targets: PathBuf,
        /// S1 result JSON (defaults to `results/s1_<lang>_warm.json`).
        #[arg(long)]
        s1: Option<PathBuf>,
        /// Heuristic result JSON (defaults to `results/heuristic_<lang>.json`).
        #[arg(long)]
        heuristic: Option<PathBuf>,
        /// Directory holding the raw knot ground-truth JSON.
        #[arg(long, default_value = "groundtruth")]
        groundtruth_dir: PathBuf,
        /// Adjudication TOML (defaults to `results/adjudication/<lang>.toml`).
        #[arg(long)]
        adjudication: Option<PathBuf>,
        /// Result JSON (defaults to `results/gold_<lang>.json`).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print every `EntityKind` wire label (T3.2 debug helper).
    Kinds,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::ValidateTargets {
            targets,
            write,
            include_config_files,
        } => cmd_validate_targets(targets, write, include_config_files),
        Command::Smoke {
            lang,
            root,
            spike_root,
            file,
        } => block_on_async(run_smoke(lang, root, spike_root, file)),
        Command::Run {
            targets,
            run,
            out,
            max_targets,
            skip_column_check,
            spike_root,
            groundtruth_dir,
        } => {
            let config = RunConfig {
                targets,
                run,
                out,
                max_targets,
                skip_column_check,
                spike_root,
                groundtruth_dir,
            };
            block_on_async(probe::run_probe(&config))
        }
        Command::Heuristic { targets, out } => {
            knot_rag_spike_phase0::graph::heuristic::run_heuristic(&targets, &out)
        }
        Command::Nolsp {
            targets,
            out,
            spike_root,
            program,
        } => {
            let config = probe::NoLspConfig {
                targets,
                out,
                spike_root,
                program,
            };
            block_on_async(probe::run_nolsp(&config))
        }
        Command::Gold {
            lang,
            targets,
            s1,
            heuristic,
            groundtruth_dir,
            adjudication,
            out,
        } => gold::run_gold(&gold_config(GoldArgs {
            lang,
            targets,
            s1,
            heuristic,
            groundtruth_dir,
            adjudication,
            out,
        })),
        Command::Kinds => {
            for kind in knot::models::EntityKind::all_wire_kinds() {
                println!("{kind}");
            }
            Ok(())
        }
    }
}

/// Run one async probe flow on a fresh multi-thread runtime.
fn block_on_async(future: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    runtime.block_on(future)
}

fn cmd_validate_targets(targets: PathBuf, write: bool, include_config_files: bool) -> Result<()> {
    let rows = groundtruth::validate_targets_file(&targets, write, include_config_files)?;
    print!("{}", groundtruth::format_validation_table(&rows));
    let missing = rows
        .iter()
        .filter(|row| row.status == TargetStatus::Missing)
        .count();
    if missing > 0 {
        anyhow::bail!("{missing} target(s) missing (see table above)");
    }
    Ok(())
}

/// CLI inputs of the `gold` subcommand before default resolution.
struct GoldArgs {
    lang: Lang,
    targets: PathBuf,
    s1: Option<PathBuf>,
    heuristic: Option<PathBuf>,
    groundtruth_dir: PathBuf,
    adjudication: Option<PathBuf>,
    out: Option<PathBuf>,
}

fn gold_config(args: GoldArgs) -> gold::GoldConfig {
    let lang_label = args.lang.label();
    gold::GoldConfig {
        targets: args.targets,
        s1: args
            .s1
            .unwrap_or_else(|| PathBuf::from(format!("results/s1_{lang_label}_warm.json"))),
        heuristic: args
            .heuristic
            .unwrap_or_else(|| PathBuf::from(format!("results/heuristic_{lang_label}.json"))),
        groundtruth_dir: args.groundtruth_dir,
        adjudication: args
            .adjudication
            .unwrap_or_else(|| PathBuf::from(format!("results/adjudication/{lang_label}.toml"))),
        out: args
            .out
            .unwrap_or_else(|| PathBuf::from(format!("results/gold_{lang_label}.json"))),
    }
}

/// T4.5 acceptance flow: spawn → didOpen → ready → documentSymbol → shutdown.
async fn run_smoke(
    lang: Lang,
    root: PathBuf,
    spike_root: PathBuf,
    file: Option<PathBuf>,
) -> Result<()> {
    let spec = servers::spec_for(lang, &root, &spike_root);
    let file = file.unwrap_or_else(|| default_smoke_file(lang));
    let abs_file = root.join(&file);

    let mut session = client::start(&spec, &root, &spike_root.join("logs")).await?;
    let root_pid = session.child.id().context("LSP child pid")?;
    let sampler = rss::RssSampler::start(root_pid, Duration::from_millis(250));

    session.did_open(&abs_file)?;
    let ready = readiness::wait_ready(
        &mut session.events,
        spec.readiness,
        Duration::from_secs(2),
        spec.ready_timeout,
    )
    .await;
    sampler.mark("ready");

    let uri = path_to_uri(&abs_file)?;
    let symbols = session.document_symbol(&uri).await;
    sampler.mark("end");
    let report = sampler.stop().await;

    let symbol_count = match &symbols.result {
        Ok(Some(response)) => client::count_symbols(response),
        Ok(None) => 0,
        Err(err) => {
            client::shutdown(session, Duration::from_secs(10)).await?;
            anyhow::bail!("documentSymbol failed: {err}");
        }
    };

    println!(
        "smoke: lang={} ready={} reason={:?} elapsed_ms={}",
        lang.label(),
        ready.ready,
        ready.reason,
        ready.elapsed.as_millis()
    );
    println!(
        "documentSymbol {}: {symbol_count} symbols (latency_ms={} retries={})",
        file.display(),
        symbols.latency.as_millis(),
        symbols.retries
    );
    if let Ok(Some(response)) = &symbols.result {
        for name in client::symbol_names(response).iter().take(10) {
            println!("  symbol: {name}");
        }
    }
    println!(
        "rss: peak_mib={} last_mib={} samples={} peak_procs={} marks={:?}",
        mib(report.peak_bytes),
        mib(report.last_bytes),
        report.samples,
        report.peak_process_count,
        report
            .marks
            .iter()
            .map(|(label, _, _)| label.clone())
            .collect::<Vec<_>>()
    );
    println!(
        "timings: spawn_ms={} initialize_ms={}",
        session.timings.spawn_ms, session.timings.initialize_ms
    );
    println!("stderr log: {}", session.stderr_log.display());

    client::shutdown(session, Duration::from_secs(10)).await?;
    Ok(())
}

fn default_smoke_file(lang: Lang) -> PathBuf {
    match lang {
        Lang::Rust => PathBuf::from("src/lib.rs"),
        Lang::Ts => PathBuf::from("src/main.tsx"),
        Lang::Java => PathBuf::from(
            "spring-ai-client-chat/src/main/java/org/springframework/ai/chat/client/ChatClient.java",
        ),
    }
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}
