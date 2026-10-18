//! Start flow of one language's server: decide, spawn, await, record.

use std::time::Instant;

use super::slot::{SharedStart, Slot, SlotGuard, StartCell, backoff_duration};
use super::worker::TaskCtx;
use super::*;

/// What `decide_start` decided while it held the language's lock.
pub(super) enum Next {
    /// Nothing to wait for: ready, or a failure to report.
    Done(Result<(), Box<LiveOutcome>>),
    /// A start is in flight: wait for it on the caller's task.
    Wait(SharedStart),
}

/// Look at the slot of one language and start a server when there is none.
/// Publishes the view whenever it mutates, so the wait that follows already
/// shows `starting`.
pub(super) fn decide_start(slot: &mut SlotGuard<'_>, ctx: &TaskCtx) -> Next {
    let lang = slot.lang();
    let now = ctx.clock.now();

    let next = match &mut **slot {
        Slot::Ready { .. } => Next::Done(Ok(())),
        Slot::Missing { diagnostic } => Next::Done(Err(Box::new(LiveOutcome::without_answer(
            LiveStatus::NoServer,
            diagnostic.clone(),
        )))),
        Slot::Disabled { diagnostic, .. } => {
            Next::Done(Err(Box::new(LiveOutcome::without_answer(
                LiveStatus::Failed,
                format!("{} is disabled: {diagnostic}", lang.server_program()),
            ))))
        }
        Slot::Failed {
            attempts,
            at,
            diagnostic,
        } => {
            let attempts_val = *attempts;
            let backoff = backoff_duration(attempts_val);
            if attempts_val >= 3 {
                let diag = diagnostic.clone();
                **slot = Slot::Disabled {
                    attempts: attempts_val,
                    diagnostic: diag.clone(),
                };
                Next::Done(Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::Failed,
                    format!("{} is disabled: {diag}", lang.server_program()),
                ))))
            } else if now.duration_since(*at) >= backoff {
                let shared = spawn_start_task(ctx, lang, attempts_val);
                **slot = Slot::Starting(shared.clone());
                Next::Wait(shared)
            } else {
                Next::Done(Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::Failed,
                    format!(
                        "{} failed previously ({diagnostic}); backoff active",
                        lang.server_program()
                    ),
                ))))
            }
        }
        Slot::NotStarted => {
            let shared = spawn_start_task(ctx, lang, 0);
            **slot = Slot::Starting(shared.clone());
            Next::Wait(shared)
        }
        Slot::Starting(shared) => Next::Wait(shared.clone()),
    };
    slot.publish();
    next
}

/// Spawn the task that starts the server of `lang`; its outcome is published
/// through the returned handle.
pub(super) fn spawn_start_task(ctx: &TaskCtx, lang: LspLang, attempts: u32) -> SharedStart {
    let factory = Arc::clone(&ctx.factory);
    let ready_wait = ctx.settings.ready_wait;
    let (tx, rx) = tokio::sync::watch::channel(None);
    let started_at = ctx.clock.now();

    tokio::spawn(async move {
        let outcome = factory
            .start(
                lang,
                FactoryContext {
                    ready_wait_hint: ready_wait,
                },
            )
            .await;
        let cell = Arc::new(std::sync::Mutex::new(Some(outcome)));
        let _ = tx.send(Some(cell));
    });

    SharedStart {
        started_at,
        attempts,
        rx,
    }
}

/// What a start needs to be recorded in the slot.
pub(super) struct StartTaskCtx<'a> {
    attempts: u32,
    started_at: Instant,
    stamp: Option<IndexStamp>,
    clock: &'a dyn Clock,
}

/// The start outcome, if the start task has produced one nobody took yet.
pub(super) fn take_start_outcome(
    rx: &tokio::sync::watch::Receiver<Option<StartCell>>,
) -> Option<StartOutcome> {
    let cell = rx.borrow().clone()?;
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// Wait up to `settings.ready_wait` for the start of this language and record
/// its outcome in the held slot. The wait holds only this language's lock.
pub(super) async fn await_start(
    slot: &mut SlotGuard<'_>,
    shared: SharedStart,
    ctx: &TaskCtx,
) -> Result<(), Box<LiveOutcome>> {
    let lang = slot.lang();
    let mut rx = shared.rx.clone();
    let attempts = shared.attempts;
    let started_at = shared.started_at;
    let start_ctx = StartTaskCtx {
        attempts,
        started_at,
        stamp: ctx.shared.stamp(),
        clock: ctx.clock.as_ref(),
    };

    let outcome = match take_start_outcome(&rx) {
        Some(outcome) => Some(Some(outcome)),
        None => tokio::time::timeout(ctx.settings.ready_wait, async {
            loop {
                if rx.changed().await.is_err() {
                    return None;
                }
                if let Some(outcome) = take_start_outcome(&rx) {
                    return Some(outcome);
                }
            }
        })
        .await
        .ok(),
    };

    match outcome {
        Some(Some(outcome)) => handle_start_outcome(slot, &start_ctx, outcome),
        Some(None) => {
            **slot = Slot::Failed {
                attempts: attempts + 1,
                at: ctx.clock.now(),
                diagnostic: format!("{} start task closed channel", lang.server_program()),
            };
            Err(Box::new(LiveOutcome::without_answer(
                LiveStatus::Failed,
                format!("{} start task ended unexpectedly", lang.server_program()),
            )))
        }
        None => {
            let elapsed_s = ctx.clock.now().duration_since(started_at).as_secs();
            Err(Box::new(LiveOutcome::without_answer(
                LiveStatus::NotReady,
                format!(
                    "{} is still starting (starting, {} s)",
                    lang.server_program(),
                    elapsed_s
                ),
            )))
        }
    }
}

/// Record a finished start in the slot (same rules as 0.3.0).
pub(super) fn handle_start_outcome(
    slot: &mut Slot,
    ctx: &StartTaskCtx<'_>,
    outcome: StartOutcome,
) -> Result<(), Box<LiveOutcome>> {
    match outcome {
        StartOutcome::Ready(started) => {
            let ready_ms =
                u64::try_from(ctx.clock.now().duration_since(ctx.started_at).as_millis())
                    .unwrap_or(u64::MAX);
            let now = ctx.clock.now();
            *slot = Slot::Ready {
                started,
                ready_ms,
                started_at: now,
                last_used: now,
                stamp: ctx.stamp.clone(),
                attempts: ctx.attempts,
                ledger: None,
            };
            Ok(())
        }
        StartOutcome::Degraded(degraded) => {
            let next_attempts = ctx.attempts + 1;
            let diag = degraded.diagnostic.clone();
            if !degraded.lsp_available {
                *slot = Slot::Missing {
                    diagnostic: diag.clone(),
                };
                Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::NoServer,
                    diag,
                )))
            } else if next_attempts >= 3 {
                *slot = Slot::Disabled {
                    attempts: next_attempts,
                    diagnostic: diag.clone(),
                };
                Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::Failed,
                    diag,
                )))
            } else {
                *slot = Slot::Failed {
                    attempts: next_attempts,
                    at: ctx.clock.now(),
                    diagnostic: diag.clone(),
                };
                Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::Failed,
                    diag,
                )))
            }
        }
    }
}
