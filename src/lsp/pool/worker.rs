//! Dispatcher loop and every operation that touches a slot (P4.3).
//!
//! The dispatcher never awaits a lock: `Query` and `Warm` run in spawned tasks
//! (one per language, each holding only its own cell), `Snapshot` reads the
//! published views and `Invalidate` only writes the index stamp.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::slot::{Shared, Slot, SlotGuard, snapshot_slot};
use super::start::{Next, await_start, decide_start, take_start_outcome};
use super::*;
use crate::lsp::normalize::LocationFilter;
use crate::lsp::process::stop;
use crate::lsp::target::run_target;
use crate::lsp::uri::CanonicalRoot;
use std::collections::BTreeMap;
use sysinfo::System;

/// Everything the pool's tasks share (P4-D4).
#[derive(Clone)]
pub(super) struct TaskCtx {
    /// One cell per language plus the index stamp.
    pub(super) shared: Arc<Shared>,
    /// Produces the sessions.
    pub(super) factory: Arc<dyn SessionFactory>,
    /// Tuning of this pool.
    pub(super) settings: PoolSettings,
    /// Time source (a fake clock in tests).
    pub(super) clock: Arc<dyn Clock>,
}

fn repo_root_from_abs_rel(abs: &Path, rel: &str) -> PathBuf {
    let mut root = abs.to_path_buf();
    for _ in rel.split('/') {
        if let Some(parent) = root.parent() {
            root = parent.to_path_buf();
        }
    }
    root
}

/// The pool's worker loop. `rx.recv()` blocks this OS thread only; every
/// operation runs in `tasks` or in the reaper.
pub(super) async fn run_dispatcher(rx: std::sync::mpsc::Receiver<Command>, ctx: TaskCtx) {
    let shared = Arc::clone(&ctx.shared);

    let reaper_task = if ctx.settings.idle > Duration::ZERO {
        let ctx = ctx.clone();
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(REAPER_INTERVAL);
            loop {
                interval.tick().await;
                reap_idle_servers(&ctx).await;
            }
        }))
    } else {
        None
    };

    let mut sys = System::new();
    let mut tasks = tokio::task::JoinSet::new();

    while let Ok(cmd) = rx.recv() {
        while tasks.try_join_next().is_some() {}
        match cmd {
            Command::Query { req, reply } => {
                let ctx = ctx.clone();
                tasks.spawn(async move {
                    let _ = reply.send(query_task(ctx, req).await);
                });
            }
            Command::Warm { lang } => {
                let ctx = ctx.clone();
                tasks.spawn(async move {
                    warm_task(ctx, lang).await;
                });
            }
            Command::Snapshot { reply } => {
                // Views only: never waits for a running query (P4-D6).
                let _ = reply.send(snapshot_all(&shared, &mut sys, ctx.clock.now()));
            }
            Command::Invalidate { stamp } => shared.set_stamp(stamp),
            Command::Shutdown { reply } => {
                if let Some(handle) = reaper_task {
                    handle.abort();
                }
                // Abort the work in flight first: its slot guards are dropped
                // and the cells become free to stop (P4-D7).
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                shutdown_all(&shared).await;
                let _ = reply.send(());
                break;
            }
        }
    }
}

/// One target query, holding this language's lock only (P4-D5).
pub(super) async fn query_task(ctx: TaskCtx, req: LiveRequest) -> LiveOutcome {
    let lang = req.lang;
    let mut slot = ctx.shared.cell(lang).lock().await;
    match ensure_ready(&mut slot, &ctx).await {
        Ok(()) => {
            let root_path = repo_root_from_abs_rel(&req.abs_file, &req.rel_file);
            let root = CanonicalRoot::new(&root_path);
            let filter = LocationFilter { root: &root, lang };
            run_target_on_slot(&mut slot, &req, &filter, ctx.clock.as_ref())
                .await
                .0
        }
        Err(outcome) => *outcome,
    }
}

/// Pre-warm: the ensure path without a target.
async fn warm_task(ctx: TaskCtx, lang: LspLang) {
    let mut slot = ctx.shared.cell(lang).lock().await;
    let _ = ensure_ready(&mut slot, &ctx).await;
}

/// Make sure the language has a usable server, starting one if needed.
async fn ensure_ready(slot: &mut SlotGuard<'_>, ctx: &TaskCtx) -> Result<(), Box<LiveOutcome>> {
    check_stale_or_dead(slot, ctx.shared.stamp(), &ctx.settings, ctx.clock.as_ref()).await;
    match decide_start(slot, ctx) {
        Next::Done(result) => result,
        Next::Wait(shared) => await_start(slot, shared, ctx).await,
    }
}

/// Run one target against the ready server of the slot's language.
async fn run_target_on_slot(
    slot: &mut SlotGuard<'_>,
    req: &LiveRequest,
    filter: &LocationFilter<'_>,
    clock: &dyn Clock,
) -> (LiveOutcome, bool) {
    let (outcome, prev_attempts) = {
        let Slot::Ready {
            started,
            last_used,
            attempts,
            ..
        } = &mut **slot
        else {
            return (
                LiveOutcome::without_answer(LiveStatus::NotReady, "slot is not ready"),
                false,
            );
        };
        let prev_attempts = *attempts;
        let outcome = run_target(&mut started.session, req, filter).await;
        *last_used = clock.now();
        (outcome, prev_attempts)
    };

    if outcome.status == LiveStatus::Failed {
        stop_slot(slot).await;
        **slot = Slot::Failed {
            attempts: prev_attempts + 1,
            at: clock.now(),
            diagnostic: outcome
                .detail
                .clone()
                .unwrap_or_else(|| "server process died".to_owned()),
        };
        slot.publish();
        (outcome, true)
    } else {
        (outcome, false)
    }
}

/// Why the ready server has to go (or not).
enum Stale {
    None,
    Died,
    Idle,
    IndexUpdate,
}

/// Drop a ready server that died, sat idle, or is older than the index.
async fn check_stale_or_dead(
    slot: &mut SlotGuard<'_>,
    stamp: Option<IndexStamp>,
    settings: &PoolSettings,
    clock: &dyn Clock,
) {
    let lang = slot.lang();
    let stale = {
        let Slot::Ready {
            started,
            stamp: own,
            last_used,
            ..
        } = &**slot
        else {
            return;
        };
        if !started.session.is_alive() {
            Stale::Died
        } else if settings.idle > Duration::ZERO
            && clock.now().duration_since(*last_used) >= settings.idle
        {
            Stale::Idle
        } else if stamp.is_some() && own.as_ref() != stamp.as_ref() {
            Stale::IndexUpdate
        } else {
            Stale::None
        }
    };

    let next = match stale {
        Stale::None => return,
        Stale::Died => Slot::Failed {
            attempts: 1,
            at: clock.now(),
            diagnostic: format!("{} process died", lang.server_program()),
        },
        Stale::Idle => {
            tracing::info!("stopping idle {} server", lang.server_program());
            Slot::NotStarted
        }
        Stale::IndexUpdate => {
            tracing::info!(
                "restarting {} server due to index update",
                lang.server_program()
            );
            Slot::NotStarted
        }
    };
    let old = std::mem::replace(&mut **slot, next);
    slot.publish();
    if let Slot::Ready { started, .. } = old {
        stop(*started, STOP_TIMEOUT).await;
    }
}

/// Take the slot's server (if any) and stop it gracefully.
async fn stop_slot(slot: &mut SlotGuard<'_>) {
    let old = std::mem::replace(&mut **slot, Slot::NotStarted);
    slot.publish();
    match old {
        Slot::Ready { started, .. } => stop(*started, STOP_TIMEOUT).await,
        Slot::Starting(shared) => {
            // A start that finished but whose outcome nobody took. A start
            // still in flight dies with its `StartOutcome`: dropping it takes
            // the `ServerProcess` with it (its `Drop` kills the tree).
            if let Some(StartOutcome::Ready(started)) = take_start_outcome(&shared.rx) {
                stop(*started, STOP_TIMEOUT).await;
            }
        }
        _ => {}
    }
}

/// Stop every server unused for `idle`. A language whose slot is locked is in
/// use and therefore not idle: it is skipped this round (P4-D8).
pub(super) async fn reap_idle_servers(ctx: &TaskCtx) {
    let idle = ctx.settings.idle;
    if idle == Duration::ZERO {
        return;
    }
    let now = ctx.clock.now();
    for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
        let Some(mut slot) = ctx.shared.cell(lang).try_lock() else {
            continue;
        };
        let idle_now = matches!(
            &*slot,
            Slot::Ready { last_used, .. } if now.duration_since(*last_used) >= idle
        );
        if idle_now {
            tracing::info!("stopping idle {} server", lang.server_program());
            stop_slot(&mut slot).await;
        }
    }
}

/// Snapshot of every slot from the published views; no slot is locked.
fn snapshot_all(shared: &Shared, sys: &mut System, now: Instant) -> PoolSnapshot {
    let mut map = BTreeMap::new();
    for (lang, cell) in &shared.cells {
        map.insert(
            lang.dir_name().to_owned(),
            snapshot_slot(&cell.view(), *lang, sys, now),
        );
    }
    PoolSnapshot { slots: map }
}

/// Stop every language's server concurrently (P4-D7).
async fn shutdown_all(shared: &Shared) {
    let mut stops = Vec::new();
    for cell in shared.cells.values() {
        let cell = Arc::clone(cell);
        stops.push(async move {
            let mut slot = cell.lock().await;
            stop_slot(&mut slot).await;
        });
    }
    futures::future::join_all(stops).await;
}
