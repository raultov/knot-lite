//! Slot state machine of the pool, its lock-free view and the per-language
//! cells (P4.3).
//!
//! One [`LangCell`] owns a language's [`Slot`] behind a `tokio` mutex (held for
//! a whole operation on that language) plus a [`SlotView`] behind a `std` mutex
//! (published on every transition, read by `snapshot` without ever waiting for a
//! query).

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::time::Instant;

use super::*;
use crate::lsp::process::Started;
use sysinfo::System;

/// Calculate back-off duration for a failed attempt.
pub(super) fn backoff_duration(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_secs(5),
        2 => Duration::from_secs(30),
        _ => Duration::from_secs(120),
    }
}

pub(super) type StartCell = Arc<std::sync::Mutex<Option<StartOutcome>>>;

/// A server start in flight: its task publishes the outcome through `rx`.
#[derive(Clone)]
pub(super) struct SharedStart {
    pub(super) started_at: Instant,
    pub(super) attempts: u32,
    pub(super) rx: tokio::sync::watch::Receiver<Option<StartCell>>,
}

pub(super) enum Slot {
    NotStarted,
    Starting(SharedStart),
    Ready {
        started: Box<Started>,
        ready_ms: u64,
        started_at: Instant,
        last_used: Instant,
        stamp: Option<IndexStamp>,
        attempts: u32,
    },
    Failed {
        attempts: u32,
        at: Instant,
        diagnostic: String,
    },
    Disabled {
        attempts: u32,
        diagnostic: String,
    },
    Missing {
        diagnostic: String,
    },
}

/// What `snapshot` reports for one slot (P4-D6).
///
/// Every transition publishes one of these, so reading it never waits for a
/// running query. `attempts` is reported as 0 while ready, as in 0.3.0 (pinned
/// by `snapshot_reports_states_and_versions`).
#[derive(Debug, Clone)]
pub(super) struct SlotView {
    pub(super) state: &'static str,
    pub(super) version: String,
    pub(super) pid: Option<u32>,
    pub(super) ready_ms: Option<u64>,
    pub(super) since: Option<Instant>,
    pub(super) attempts: u32,
    pub(super) diagnostic: Option<String>,
}

impl SlotView {
    fn plain(state: &'static str) -> Self {
        Self {
            state,
            version: String::new(),
            pid: None,
            ready_ms: None,
            since: None,
            attempts: 0,
            diagnostic: None,
        }
    }

    /// The view of `slot`.
    fn of(slot: &Slot) -> Self {
        match slot {
            Slot::NotStarted => Self::plain("not_started"),
            Slot::Starting(active) => Self {
                state: "starting",
                since: Some(active.started_at),
                attempts: active.attempts,
                ..Self::plain("starting")
            },
            Slot::Ready {
                started,
                ready_ms,
                started_at,
                ..
            } => Self {
                state: "ready",
                version: started.session.server_version.clone(),
                pid: started.process.as_ref().and_then(|p| p.pid),
                ready_ms: Some(*ready_ms),
                since: Some(*started_at),
                attempts: 0,
                diagnostic: None,
            },
            Slot::Failed {
                attempts,
                diagnostic,
                ..
            } => Self {
                state: "failed",
                attempts: *attempts,
                diagnostic: Some(diagnostic.clone()),
                ..Self::plain("failed")
            },
            Slot::Disabled {
                attempts,
                diagnostic,
            } => Self {
                state: "disabled",
                attempts: *attempts,
                diagnostic: Some(diagnostic.clone()),
                ..Self::plain("disabled")
            },
            Slot::Missing { diagnostic } => Self {
                state: "missing",
                diagnostic: Some(diagnostic.clone()),
                ..Self::plain("missing")
            },
        }
    }
}

/// One language of the pool: its slot and the view `snapshot` reads (P4-D4).
pub(super) struct LangCell {
    /// Which language this cell owns.
    pub(super) lang: LspLang,
    slot: tokio::sync::Mutex<Slot>,
    view: std::sync::Mutex<SlotView>,
}

impl LangCell {
    pub(super) fn new(lang: LspLang) -> Self {
        Self {
            lang,
            slot: tokio::sync::Mutex::new(Slot::NotStarted),
            view: std::sync::Mutex::new(SlotView::of(&Slot::NotStarted)),
        }
    }

    /// The last published state; never waits for the slot lock.
    pub(super) fn view(&self) -> SlotView {
        self.view
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Publish `slot` as the current state.
    fn publish(&self, slot: &Slot) {
        *self
            .view
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = SlotView::of(slot);
    }

    /// Lock this language for a whole operation on it (P4-D5).
    pub(super) async fn lock(&self) -> SlotGuard<'_> {
        SlotGuard {
            cell: self,
            slot: self.slot.lock().await,
        }
    }

    /// Lock this language only if no operation is running on it (P4-D8).
    pub(super) fn try_lock(&self) -> Option<SlotGuard<'_>> {
        self.slot
            .try_lock()
            .ok()
            .map(|slot| SlotGuard { cell: self, slot })
    }
}

/// The locked slot of one language. Dropping the guard publishes the view, so
/// the view can never lag behind a released slot.
pub(super) struct SlotGuard<'a> {
    cell: &'a LangCell,
    slot: tokio::sync::MutexGuard<'a, Slot>,
}

impl SlotGuard<'_> {
    pub(super) fn lang(&self) -> LspLang {
        self.cell.lang
    }

    /// Publish the current state now: call it right after a mutation that is
    /// followed by an `.await` (a start wait, a stop).
    pub(super) fn publish(&self) {
        self.cell.publish(&self.slot);
    }
}

impl Deref for SlotGuard<'_> {
    type Target = Slot;

    fn deref(&self) -> &Slot {
        &self.slot
    }
}

impl DerefMut for SlotGuard<'_> {
    fn deref_mut(&mut self) -> &mut Slot {
        &mut self.slot
    }
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.publish();
    }
}

/// Everything the pool's tasks share (P4-D4).
///
/// No lock covering more than one language is ever held across an `.await`:
/// the cells are independent and the index stamp lives in its own `std` mutex.
pub(super) struct Shared {
    pub(super) cells: BTreeMap<LspLang, Arc<LangCell>>,
    stamp: std::sync::Mutex<Option<IndexStamp>>,
}

impl Shared {
    pub(super) fn new(stamp: Option<IndexStamp>) -> Self {
        let mut cells = BTreeMap::new();
        for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
            cells.insert(lang, Arc::new(LangCell::new(lang)));
        }
        Self {
            cells,
            stamp: std::sync::Mutex::new(stamp),
        }
    }

    pub(super) fn cell(&self, lang: LspLang) -> &Arc<LangCell> {
        self.cells.get(&lang).expect("cell")
    }

    /// The index stamp the servers were started against.
    pub(super) fn stamp(&self) -> Option<IndexStamp> {
        self.stamp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) fn set_stamp(&self, stamp: IndexStamp) {
        *self
            .stamp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stamp);
    }
}

/// Map a published view to the wire snapshot. The resident memory is looked up
/// here, from the view's pid, so no slot is ever locked for a snapshot.
pub(super) fn snapshot_slot(
    view: &SlotView,
    lang: LspLang,
    sys: &mut System,
    now: Instant,
) -> SlotSnapshot {
    let rss_mib = view.pid.map(|pid| {
        let (bytes, _) = crate::lsp::process::tree_rss_bytes(sys, pid);
        bytes as f64 / (1024.0 * 1024.0)
    });
    SlotSnapshot {
        state: view.state.to_owned(),
        server: lang.server_program().to_owned(),
        version: view.version.clone(),
        pid: view.pid,
        ready_ms: view.ready_ms,
        uptime_s: view.since.map(|since| now.duration_since(since).as_secs()),
        rss_mib,
        attempts: view.attempts,
        diagnostic: view.diagnostic.clone(),
    }
}
