//! Starting the language-server pool of a process.
//!
//! One place builds the production pool for every surface that needs it: the
//! query path (CLI and MCP), `index` (pre-computation) and `status --probe-lsp`.
//! Creating a pool only spawns its idle thread; servers start on the first query.

use std::time::Duration;

use super::pool::{LspPool, PoolSettings, ProcessFactory};
use super::settings::scratch_dir;
use crate::config::Config;
use crate::index::load::IndexStamp;

/// Start the pool for the repository of `cfg`.
///
/// `files` are the repo-relative paths of the index (they decide which TS/JS
/// files and Cargo projects are opened for the servers up front). `ready_wait`
/// is the default wait for a starting server, used unless
/// `KNOT_LITE_LSP_READY_WAIT_SECS` overrides it. `None` when the thread cannot
/// be spawned (logged); the callers then answer without language servers.
pub fn start_pool(
    cfg: &Config,
    files: &[String],
    ready_wait: Duration,
    stamp: Option<IndexStamp>,
) -> Option<LspPool> {
    let factory = ProcessFactory::new(
        cfg.repo_path.clone(),
        scratch_dir(&cfg.lsp, &cfg.repo_path),
        cfg.lsp_overrides.clone(),
        cfg.lsp.build_exec,
        files,
    );
    let settings = PoolSettings {
        ready_wait: cfg.lsp.ready_wait.unwrap_or(ready_wait),
        idle: cfg.lsp.idle,
        sync: cfg.lsp.sync,
        cache_dir: cfg.lsp.cache_root.clone(),
        inventory_files: std::sync::Arc::new(files.to_vec()),
        ..PoolSettings::default()
    };
    match LspPool::new(std::sync::Arc::new(factory), settings, stamp) {
        Ok(pool) => Some(pool),
        Err(error) => {
            tracing::warn!("cannot start the language-server pool: {error:#}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::config_for;

    #[test]
    fn the_pool_starts_idle_and_without_servers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = config_for(tmp.path().to_path_buf());
        let pool = start_pool(&cfg, &[], Duration::from_secs(1), None).expect("pool");
        // Creating the pool starts no server.
        let slots = pool.snapshot().slots;
        assert_eq!(slots.len(), 3);
        assert!(slots.values().all(|slot| slot.state == "not_started"));
    }
}
