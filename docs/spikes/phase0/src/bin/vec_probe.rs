//! S2 probe binary: file-based vectors (T9).
//!
//! Thin CLI over the `vec` library: `index`, `measure`, `determinism`,
//! `mutate`, `parity`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use knot_rag_spike_phase0::vec::index::{self, IndexConfig};
use knot_rag_spike_phase0::vec::measure::{self, DeterminismRuns, MeasureConfig};

#[derive(Parser)]
#[command(name = "vec_probe", about = "S2 file-based vector probe")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse + embed + write every shard variant (T9.2).
    Index {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value_t = 64)]
        batch_size: usize,
        /// One embed call per source file (incremental simulation).
        #[arg(long)]
        per_file_batches: bool,
        /// Re-index one file only (churn experiment, T9.6).
        #[arg(long)]
        only_file: Option<String>,
    },
    /// Measure sizes, load times, latency, quantisation, quality (T9.5).
    Measure {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long)]
        queries: PathBuf,
        #[arg(long)]
        level1: PathBuf,
        #[arg(long)]
        result: PathBuf,
        #[arg(long, default_value = "groundtruth")]
        groundtruth_dir: PathBuf,
    },
    /// Determinism: rerun / batching / threads comparisons (T9.7).
    Determinism {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long)]
        result: PathBuf,
        /// Pre-indexed dir produced with `RAYON_NUM_THREADS=1` (optional).
        #[arg(long)]
        threads_dir: Option<PathBuf>,
    },
    /// Edit one sandbox file to simulate incremental churn (T9.6).
    Mutate {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        file: String,
        #[arg(long)]
        mode: String,
    },
    /// Compare spike f32 vectors with a Qdrant dump (T9.9).
    Parity {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long)]
        qdrant_dump: PathBuf,
        #[arg(long)]
        result: PathBuf,
        /// Write the deterministic 200-id sample and exit (dump preparation).
        #[arg(long)]
        ids_out: Option<PathBuf>,
    },
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Index {
            repo,
            root,
            out_dir,
            batch_size,
            per_file_batches,
            only_file,
        } => {
            let config = IndexConfig {
                repo,
                root,
                out_dir,
                batch_size,
                per_file_batches,
                only_file,
            };
            let stats = index::run_index(&config)?;
            println!(
                "indexed {} entities ({} chunks) into {} in {} ms",
                stats.entities,
                stats.chunks,
                config.out_dir.display(),
                stats.parse_ms + stats.embed_ms + stats.embed_chunks_ms
            );
            Ok(())
        }
        Command::Measure {
            repo,
            root,
            out_dir,
            queries,
            level1,
            result,
            groundtruth_dir,
        } => measure::run_measure(&MeasureConfig {
            repo,
            root,
            out_dir,
            queries,
            level1,
            result,
            groundtruth_dir,
        }),
        Command::Determinism {
            repo: _,
            root,
            out_dir,
            result,
            threads_dir,
        } => run_determinism(root, out_dir, result, threads_dir),
        Command::Mutate { root, file, mode } => run_mutate(root, file, mode),
        Command::Parity {
            repo: _,
            out_dir,
            qdrant_dump,
            result,
            ids_out,
        } => {
            if let Some(ids_out) = ids_out {
                return write_parity_ids(&out_dir, &ids_out);
            }
            measure::run_parity(
                &MeasureConfig {
                    repo: String::new(),
                    root: PathBuf::new(),
                    out_dir,
                    queries: PathBuf::new(),
                    level1: PathBuf::new(),
                    result,
                    groundtruth_dir: PathBuf::new(),
                },
                &qdrant_dump,
            )
        }
    }
}

/// T9.7: index three runs and compare their shards.
fn run_determinism(
    root: PathBuf,
    out_dir: PathBuf,
    result: PathBuf,
    threads_dir: Option<PathBuf>,
) -> Result<()> {
    let base = out_dir.clone();
    let run1 = base.join("det-run1");
    let run2 = base.join("det-run2");
    let per_file = base.join("det-perfile");
    for (dir, per_file_batches) in [(&run1, false), (&run2, false), (&per_file, true)] {
        if dir.join("index_stats.json").exists() {
            continue; // reuse a previous run when re-merging
        }
        let config = IndexConfig {
            repo: String::new(),
            root: root.clone(),
            out_dir: dir.clone(),
            batch_size: 64,
            per_file_batches,
            only_file: None,
        };
        index::run_index(&config)?;
    }
    measure::run_determinism(
        &MeasureConfig {
            repo: String::new(),
            root,
            out_dir: base,
            queries: PathBuf::new(),
            level1: PathBuf::new(),
            result,
            groundtruth_dir: PathBuf::new(),
        },
        &DeterminismRuns {
            run1,
            run2,
            per_file,
            threads: threads_dir,
        },
    )
}

/// T9.6: insert a marker function into the sandbox copy (never the original).
fn run_mutate(root: PathBuf, file: String, mode: String) -> Result<()> {
    let path = root.join(&file);
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let marker = marker_for(&file);
    let lines: Vec<&str> = source.lines().collect();
    let mut out: Vec<String> = Vec::new();
    match mode.as_str() {
        "append" => {
            let split = if file.ends_with(".java") {
                lines.len().saturating_sub(1) // inside the class: before final `}`
            } else {
                lines.len()
            };
            out.extend(lines[..split].iter().map(|l| (*l).to_owned()));
            out.push(marker);
            out.extend(lines[split..].iter().map(|l| (*l).to_owned()));
        }
        "prepend" => {
            // After the header (package / imports / leading comments) so the
            // file stays parseable while every later line shifts (T9.6).
            let mut at = 0;
            for (idx, line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                let header = trimmed.is_empty()
                    || trimmed.starts_with("//")
                    || trimmed.starts_with("/*")
                    || trimmed.starts_with('*')
                    || trimmed.starts_with("package ")
                    || trimmed.starts_with("import ")
                    || trimmed.starts_with("#")
                    || trimmed.starts_with("use ")
                    || trimmed.starts_with("pub use ");
                if header {
                    at = idx + 1;
                } else {
                    break;
                }
            }
            out.extend(lines[..at].iter().map(|l| (*l).to_owned()));
            out.push(marker);
            out.extend(lines[at..].iter().map(|l| (*l).to_owned()));
        }
        other => anyhow::bail!("unknown mode {other} (append|prepend)"),
    }
    std::fs::write(&path, out.join("\n") + "\n")
        .with_context(|| format!("writing {}", path.display()))?;
    println!("mutated {} ({mode})", path.display());
    Ok(())
}

fn marker_for(file: &str) -> String {
    if file.ends_with(".java") {
        "    public static int spikeChurnMarker() { return 42; }".to_owned()
    } else if file.ends_with(".ts") || file.ends_with(".tsx") || file.ends_with(".js") {
        "export function spikeChurnMarker(): number { return 42; }".to_owned()
    } else {
        "pub fn spike_churn_marker() -> usize { 42 }".to_owned()
    }
}

/// Deterministic 200-id sample from the f32 shards (parity input, T9.9).
fn write_parity_ids(out_dir: &Path, ids_out: &Path) -> Result<()> {
    use knot_rag_spike_phase0::vec::encode::Variant;
    let matrix = measure::load_matrix(out_dir, Variant::F32Dec, 384)?;
    let ids: BTreeSet<String> = matrix.ids.iter().cloned().collect();
    let mut sorted: Vec<String> = ids.into_iter().collect();
    sorted.sort();
    let step = (sorted.len() / 200).max(1);
    let sample: Vec<&String> = sorted.iter().step_by(step).take(200).collect();
    std::fs::write(ids_out, serde_json::to_string_pretty(&sample)?)?;
    println!("wrote {} ids to {}", sample.len(), ids_out.display());
    Ok(())
}
