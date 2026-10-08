//! Start flow of one language's server: decide, spawn, await, record.

use std::time::{Duration, Instant};

use super::slot::{SharedStart, Slot, StartCell, backoff_duration};
use super::worker::Inner;
use super::*;

/// What `ensure_ready` decided while it held the pool lock.
pub(super) enum Next {
    /// Nothing to wait for: ready, or a failure to report.
    Done(Result<(), Box<LiveOutcome>>),
    /// A start is in flight: wait for it **without** the pool lock, so a
    /// snapshot (or another language) is not stuck behind a slow start.
    Wait(SharedStart),
}

/// Look at the slot of `lang` and start a server when there is none.
pub(super) fn decide_start(
    guard: &mut Inner,
    factory: &Arc<dyn SessionFactory>,
    lang: LspLang,
    settings: &PoolSettings,
    clock: &Arc<dyn Clock>,
) -> Next {
    let ready_wait = settings.ready_wait;
    let now = clock.now();
    let slot = guard.slots.get_mut(&lang).expect("slot");

    match slot {
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
                *slot = Slot::Disabled {
                    attempts: attempts_val,
                    diagnostic: diag.clone(),
                };
                Next::Done(Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::Failed,
                    format!("{} is disabled: {diag}", lang.server_program()),
                ))))
            } else if now.duration_since(*at) >= backoff {
                let shared = spawn_start_task(factory, lang, ready_wait, attempts_val, clock);
                *slot = Slot::Starting(shared.clone());
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
            let shared = spawn_start_task(factory, lang, ready_wait, 0, clock);
            *slot = Slot::Starting(shared.clone());
            Next::Wait(shared)
        }
        Slot::Starting(shared) => Next::Wait(shared.clone()),
    }
}

pub(super) fn spawn_start_task(
    factory: &Arc<dyn SessionFactory>,
    lang: LspLang,
    ready_wait: Duration,
    attempts: u32,
    clock: &Arc<dyn Clock>,
) -> SharedStart {
    let factory = Arc::clone(factory);
    let (tx, rx) = tokio::sync::watch::channel(None);
    let started_at = clock.now();

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

pub(super) struct StartTaskCtx<'a> {
    lang: LspLang,
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

/// Wait up to `ready_wait` for the start of `lang`, then record its outcome.
/// The pool lock is held only to read the stamp and to record the result.
pub(super) async fn await_start_result(
    inner: &Arc<tokio::sync::Mutex<Inner>>,
    lang: LspLang,
    shared: SharedStart,
    ready_wait: Duration,
    clock: &Arc<dyn Clock>,
) -> Result<(), Box<LiveOutcome>> {
    let mut rx = shared.rx.clone();
    let attempts = shared.attempts;
    let started_at = shared.started_at;
    let stamp = inner.lock().await.current_stamp.clone();
    let ctx = StartTaskCtx {
        lang,
        attempts,
        started_at,
        stamp,
        clock: clock.as_ref(),
    };

    let outcome = match take_start_outcome(&rx) {
        Some(outcome) => Some(Some(outcome)),
        None => tokio::time::timeout(ready_wait, async {
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
        Some(Some(outcome)) => {
            let mut guard = inner.lock().await;
            handle_start_outcome(&mut guard, &ctx, outcome)
        }
        Some(None) => {
            inner.lock().await.slots.insert(
                lang,
                Slot::Failed {
                    attempts: attempts + 1,
                    at: clock.now(),
                    diagnostic: format!("{} start task closed channel", lang.server_program()),
                },
            );
            Err(Box::new(LiveOutcome::without_answer(
                LiveStatus::Failed,
                format!("{} start task ended unexpectedly", lang.server_program()),
            )))
        }
        None => {
            let elapsed_s = clock.now().duration_since(started_at).as_secs();
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

pub(super) fn handle_start_outcome(
    guard: &mut Inner,
    ctx: &StartTaskCtx<'_>,
    outcome: StartOutcome,
) -> Result<(), Box<LiveOutcome>> {
    let lang = ctx.lang;
    let clock = ctx.clock;
    match outcome {
        StartOutcome::Ready(started) => {
            let ready_ms = u64::try_from(clock.now().duration_since(ctx.started_at).as_millis())
                .unwrap_or(u64::MAX);
            guard.slots.insert(
                lang,
                Slot::Ready {
                    started,
                    ready_ms,
                    started_at: clock.now(),
                    last_used: clock.now(),
                    stamp: ctx.stamp.clone(),
                    attempts: ctx.attempts,
                },
            );
            Ok(())
        }
        StartOutcome::Degraded(degraded) => {
            let next_attempts = ctx.attempts + 1;
            let diag = degraded.diagnostic.clone();
            if !degraded.lsp_available {
                guard.slots.insert(
                    lang,
                    Slot::Missing {
                        diagnostic: diag.clone(),
                    },
                );
                Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::NoServer,
                    diag,
                )))
            } else if next_attempts >= 3 {
                guard.slots.insert(
                    lang,
                    Slot::Disabled {
                        attempts: next_attempts,
                        diagnostic: diag.clone(),
                    },
                );
                Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::Failed,
                    diag,
                )))
            } else {
                guard.slots.insert(
                    lang,
                    Slot::Failed {
                        attempts: next_attempts,
                        at: clock.now(),
                        diagnostic: diag.clone(),
                    },
                );
                Err(Box::new(LiveOutcome::without_answer(
                    LiveStatus::Failed,
                    diag,
                )))
            }
        }
    }
}
