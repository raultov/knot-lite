//! Logging setup (stderr) and shared runtime helpers.
//!
//! All logging goes to **stderr**: stdout carries the CLI result or the MCP
//! stdio stream and must never be polluted (P1-D13 makes this a hard gate).

use anyhow::Result;
use tracing_subscriber::EnvFilter;

/// Default log level for the CLI binary.
pub const CLI_LOG_LEVEL: &str = "error";

/// Default log level for the MCP server binary.
pub const MCP_LOG_LEVEL: &str = "info";

/// Initialise `tracing` with `RUST_LOG` (falling back to `default_level`),
/// writing to stderr without ANSI colours.
pub fn init_logging(default_level: &str) -> Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to initialize logging: {e}"))
}

/// The `k` highest-scored entries of `scored`: score descending, ties by the
/// lower key (record position or matrix row), so every ranking is deterministic.
pub fn top_scored(mut scored: Vec<(usize, f32)>, k: usize) -> Vec<(usize, f32)> {
    let keep = k.min(scored.len());
    if keep == 0 {
        return Vec::new();
    }
    let by_rank = |a: &(usize, f32), b: &(usize, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
    scored.select_nth_unstable_by(keep - 1, by_rank);
    scored.truncate(keep);
    scored.sort_by(by_rank);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_scored_orders_by_score_then_key() {
        let scored = vec![(4, 0.5), (1, 0.9), (3, 0.5), (2, 0.1)];
        assert_eq!(
            top_scored(scored.clone(), 3),
            vec![(1, 0.9), (3, 0.5), (4, 0.5)]
        );
        assert_eq!(top_scored(scored.clone(), 10).len(), 4);
        assert!(top_scored(scored, 0).is_empty());
    }
}
