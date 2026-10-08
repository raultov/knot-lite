//! Dispatcher loop and every operation that touches a slot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::slot::{Slot, snapshot_slot};
use super::start::{Next, await_start_result, decide_start};
use super::*;
use crate::lsp::normalize::LocationFilter;
use crate::lsp::process::stop;
use crate::lsp::target::run_target;
use crate::lsp::uri::CanonicalRoot;
use sysinfo::System;

pub(super) struct Inner {
    pub(super) slots: BTreeMap<LspLang, Slot>,
    pub(super) lang_mutexes: BTreeMap<LspLang, Arc<tokio::sync::Mutex<()>>>,
    pub(super) current_stamp: Option<IndexStamp>,
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

pub(super) async fn run_dispatcher(
    rx: std::sync::mpsc::Receiver<Command>,
    factory: Arc<dyn SessionFactory>,
    settings: PoolSettings,
    stamp: Option<IndexStamp>,
    clock: Arc<dyn Clock>,
) {
    let mut slots = BTreeMap::new();
    let mut lang_mutexes = BTreeMap::new();

    for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
        slots.insert(lang, Slot::NotStarted);
        lang_mutexes.insert(lang, Arc::new(tokio::sync::Mutex::new(())));
    }

    let inner = Arc::new(tokio::sync::Mutex::new(Inner {
        slots,
        lang_mutexes,
        current_stamp: stamp,
    }));

    // Start background idle reaper if idle > 0
    let reaper_task = if settings.idle > Duration::ZERO {
        let inner_clone = Arc::clone(&inner);
        let clock_clone = Arc::clone(&clock);
        let idle_dur = settings.idle;
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(REAPER_INTERVAL);
            loop {
                interval.tick().await;
                reap_idle_servers(&inner_clone, idle_dur, &clock_clone).await;
            }
        }))
    } else {
        None
    };

    let mut sys = System::new();

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Query { req, reply } => {
                let inner = Arc::clone(&inner);
                let factory = Arc::clone(&factory);
                let clock = Arc::clone(&clock);
                let settings = settings.clone();

                tokio::spawn(async move {
                    let lang = req.lang;
                    let mutex = {
                        let guard = inner.lock().await;
                        Arc::clone(&guard.lang_mutexes[&lang])
                    };

                    let _guard = mutex.lock().await;

                    let target_res = ensure_ready(&inner, &factory, lang, &settings, &clock).await;

                    let outcome = match target_res {
                        Ok(()) => {
                            let root_path = repo_root_from_abs_rel(&req.abs_file, &req.rel_file);
                            let root = CanonicalRoot::new(&root_path);
                            let filter = LocationFilter { root: &root, lang };
                            let (outcome, _failed) =
                                run_target_on_slot(&inner, lang, &req, &filter, &clock).await;
                            outcome
                        }
                        Err(outcome) => *outcome,
                    };

                    let _ = reply.send(outcome);
                });
            }
            Command::Warm { lang } => {
                let inner = Arc::clone(&inner);
                let factory = Arc::clone(&factory);
                let clock = Arc::clone(&clock);
                let settings = settings.clone();

                tokio::spawn(async move {
                    let mutex = {
                        let guard = inner.lock().await;
                        Arc::clone(&guard.lang_mutexes[&lang])
                    };
                    let _guard = mutex.lock().await;
                    let _ = ensure_ready(&inner, &factory, lang, &settings, &clock).await;
                });
            }
            Command::Snapshot { reply } => {
                let inner = Arc::clone(&inner);
                let clock = Arc::clone(&clock);
                let mut sys_local = std::mem::take(&mut sys);

                let snapshot = snapshot_slots(&inner, &mut sys_local, &clock).await;
                sys = sys_local;
                let _ = reply.send(snapshot);
            }
            Command::Invalidate { stamp } => {
                let mut guard = inner.lock().await;
                guard.current_stamp = Some(stamp);
            }
            Command::Shutdown { reply } => {
                if let Some(handle) = reaper_task {
                    handle.abort();
                }
                shutdown_all_slots(&inner).await;
                let _ = reply.send(());
                break;
            }
        }
    }
}

async fn run_target_on_slot(
    inner: &Arc<tokio::sync::Mutex<Inner>>,
    lang: LspLang,
    req: &LiveRequest,
    filter: &LocationFilter<'_>,
    clock: &Arc<dyn Clock>,
) -> (LiveOutcome, bool) {
    let mut guard = inner.lock().await;
    let slot = guard.slots.get_mut(&lang).expect("slot");

    let Slot::Ready {
        started,
        last_used,
        attempts,
        ..
    } = slot
    else {
        return (
            LiveOutcome::without_answer(LiveStatus::NotReady, "slot is not ready"),
            false,
        );
    };

    let prev_attempts = *attempts;
    let outcome = run_target(&mut started.session, req, filter).await;
    *last_used = clock.now();

    if outcome.status == LiveStatus::Failed {
        let old_slot = std::mem::replace(slot, Slot::NotStarted);
        if let Slot::Ready { started, .. } = old_slot {
            stop(*started, STOP_TIMEOUT).await;
        }
        *slot = Slot::Failed {
            attempts: prev_attempts + 1,
            at: clock.now(),
            diagnostic: outcome
                .detail
                .clone()
                .unwrap_or_else(|| "server process died".to_owned()),
        };
        (outcome, true)
    } else {
        (outcome, false)
    }
}

async fn check_stale_or_dead(
    guard: &mut Inner,
    lang: LspLang,
    settings: &PoolSettings,
    clock: &dyn Clock,
) {
    let Some(slot) = guard.slots.get(&lang) else {
        return;
    };

    let Slot::Ready {
        started,
        stamp,
        last_used,
        ..
    } = slot
    else {
        return;
    };

    if !started.session.is_alive() {
        let old = guard.slots.insert(
            lang,
            Slot::Failed {
                attempts: 1,
                at: clock.now(),
                diagnostic: format!("{} process died", lang.server_program()),
            },
        );
        if let Some(Slot::Ready { started, .. }) = old {
            stop(*started, STOP_TIMEOUT).await;
        }
    } else if settings.idle > Duration::ZERO
        && clock.now().duration_since(*last_used) >= settings.idle
    {
        tracing::info!("stopping idle {} server", lang.server_program());
        let old = guard.slots.insert(lang, Slot::NotStarted);
        if let Some(Slot::Ready { started, .. }) = old {
            stop(*started, STOP_TIMEOUT).await;
        }
    } else if guard.current_stamp.is_some() && stamp.as_ref() != guard.current_stamp.as_ref() {
        tracing::info!(
            "restarting {} server due to index update",
            lang.server_program()
        );
        let old = guard.slots.insert(lang, Slot::NotStarted);
        if let Some(Slot::Ready { started, .. }) = old {
            stop(*started, STOP_TIMEOUT).await;
        }
    }
}

async fn ensure_ready(
    inner: &Arc<tokio::sync::Mutex<Inner>>,
    factory: &Arc<dyn SessionFactory>,
    lang: LspLang,
    settings: &PoolSettings,
    clock: &Arc<dyn Clock>,
) -> Result<(), Box<LiveOutcome>> {
    let next = {
        let mut guard = inner.lock().await;
        check_stale_or_dead(&mut guard, lang, settings, clock.as_ref()).await;
        decide_start(&mut guard, factory, lang, settings, clock)
    };
    match next {
        Next::Done(result) => result,
        Next::Wait(shared) => {
            await_start_result(inner, lang, shared, settings.ready_wait, clock).await
        }
    }
}

async fn reap_idle_servers(
    inner: &Arc<tokio::sync::Mutex<Inner>>,
    idle_dur: Duration,
    clock: &Arc<dyn Clock>,
) {
    let mut guard = inner.lock().await;
    let now = clock.now();

    for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
        let should_stop = match guard.slots.get(&lang) {
            Some(Slot::Ready { last_used, .. }) => now.duration_since(*last_used) >= idle_dur,
            _ => false,
        };
        if should_stop {
            tracing::info!("stopping idle {} server", lang.server_program());
            let old_slot = std::mem::replace(guard.slots.get_mut(&lang).unwrap(), Slot::NotStarted);
            if let Slot::Ready { started, .. } = old_slot {
                stop(*started, STOP_TIMEOUT).await;
            }
        }
    }
}

async fn snapshot_slots(
    inner: &Arc<tokio::sync::Mutex<Inner>>,
    sys: &mut System,
    clock: &Arc<dyn Clock>,
) -> PoolSnapshot {
    let guard = inner.lock().await;
    let mut map = BTreeMap::new();
    let now = clock.now();

    for (lang, slot) in &guard.slots {
        let key = lang.dir_name().to_owned();
        let snap = snapshot_slot(slot, *lang, sys, now);
        map.insert(key, snap);
    }

    PoolSnapshot { slots: map }
}

async fn shutdown_all_slots(inner: &Arc<tokio::sync::Mutex<Inner>>) {
    let mut guard = inner.lock().await;
    for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
        let old_slot = std::mem::replace(guard.slots.get_mut(&lang).unwrap(), Slot::NotStarted);
        if let Slot::Ready { started, .. } = old_slot {
            stop(*started, STOP_TIMEOUT).await;
        }
    }
}
