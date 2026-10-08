//! Real language-server sessions: the [`ProcessFactory`] of the pool.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;
use crate::config::LspOverrides;
use crate::lsp::preopen::{PreopenPlan, plan};
use crate::lsp::process::{StartContext, Started, start_or_degrade, stop};
use crate::lsp::readiness::wait_ready;
use crate::lsp::spec::{Degraded, SpecInputs, resolve_program, spec_for};
use crate::lsp::uri::join_rel;

/// Production factory spawning real language-server processes.
pub struct ProcessFactory {
    /// Repository root.
    pub repo_root: PathBuf,
    /// Per-repository scratch directory.
    pub scratch: PathBuf,
    /// Executable overrides from configuration.
    pub overrides: LspOverrides,
    /// Whether build scripts and proc-macros may run.
    pub build_exec: bool,
    /// File pre-open plan.
    pub preopen: PreopenPlan,
    /// Number of stderr logs kept.
    pub logs_keep: usize,
}

impl ProcessFactory {
    /// Create a [`ProcessFactory`] with default settings.
    pub fn new(
        repo_root: PathBuf,
        scratch: PathBuf,
        overrides: LspOverrides,
        build_exec: bool,
        inventory_files: &[String],
    ) -> Self {
        let preopen = plan(&repo_root, inventory_files);
        Self {
            repo_root,
            scratch,
            overrides,
            build_exec,
            preopen,
            logs_keep: 5,
        }
    }
}

fn preopen_ts_files(started: &mut Started, repo_root: &Path, ts_files: &[String]) {
    for rel in ts_files {
        let abs = join_rel(repo_root, rel);
        let _ = started.session.did_open(&abs);
    }
}

impl SessionFactory for ProcessFactory {
    fn start(&self, lang: LspLang, _ctx: FactoryContext) -> BoxFuture<'static, StartOutcome> {
        let repo_root = self.repo_root.clone();
        let scratch = self.scratch.clone();
        let overrides = self.overrides.clone();
        let build_exec = self.build_exec;
        let preopen = self.preopen.clone();
        let logs_keep = self.logs_keep;

        Box::pin(async move {
            let program = match resolve_program(lang, &overrides) {
                Ok(prog) => prog,
                Err(degraded) => return StartOutcome::Degraded(degraded),
            };
            let spec_inputs = SpecInputs {
                lang,
                program: &program,
                repo_root: &repo_root,
                scratch: &scratch,
                build_exec,
                preopen: &preopen,
            };
            let spec = spec_for(&spec_inputs);
            let start_ctx = StartContext {
                repo_root: &repo_root,
                scratch: &scratch,
                logs_keep,
            };

            match start_or_degrade(&spec, &start_ctx).await {
                StartOutcome::Ready(mut started) => {
                    if lang == LspLang::Ts {
                        preopen_ts_files(&mut started, &repo_root, &preopen.ts_files);
                    }
                    let mut events = started.session.events.clone();
                    let ready_res = wait_ready(
                        &mut events,
                        spec.readiness,
                        Duration::from_secs(2),
                        spec.ready_timeout,
                    )
                    .await;

                    if ready_res.ready {
                        StartOutcome::Ready(started)
                    } else {
                        let diag = format!(
                            "{} readiness timed out after {:?}",
                            lang.server_program(),
                            spec.ready_timeout
                        );
                        stop(*started, STOP_TIMEOUT).await;
                        StartOutcome::Degraded(Degraded {
                            lsp_available: true,
                            lsp_started: true,
                            diagnostic: diag,
                        })
                    }
                }
                other => other,
            }
        })
    }
}
