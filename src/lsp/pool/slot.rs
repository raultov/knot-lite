//! Slot state machine of the pool and the mapping from slots to snapshots.

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

pub(super) fn snapshot_slot(
    slot: &Slot,
    lang: LspLang,
    sys: &mut System,
    now: Instant,
) -> SlotSnapshot {
    let server = lang.server_program().to_owned();
    match slot {
        Slot::NotStarted => SlotSnapshot {
            state: "not_started".to_owned(),
            server,
            version: String::new(),
            pid: None,
            ready_ms: None,
            uptime_s: None,
            rss_mib: None,
            attempts: 0,
            diagnostic: None,
        },
        Slot::Starting(active) => SlotSnapshot {
            state: "starting".to_owned(),
            server,
            version: String::new(),
            pid: None,
            ready_ms: None,
            uptime_s: Some(now.duration_since(active.started_at).as_secs()),
            rss_mib: None,
            attempts: active.attempts,
            diagnostic: None,
        },
        Slot::Ready {
            started,
            ready_ms,
            started_at,
            ..
        } => {
            let pid = started.process.as_ref().and_then(|p| p.pid);
            let (rss_bytes, _) =
                pid.map_or((0, 0), |p| crate::lsp::process::tree_rss_bytes(sys, p));
            let rss_mib = if pid.is_some() {
                Some(rss_bytes as f64 / (1024.0 * 1024.0))
            } else {
                None
            };
            SlotSnapshot {
                state: "ready".to_owned(),
                server,
                version: started.session.server_version.clone(),
                pid,
                ready_ms: Some(*ready_ms),
                uptime_s: Some(now.duration_since(*started_at).as_secs()),
                rss_mib,
                attempts: 0,
                diagnostic: None,
            }
        }
        Slot::Failed {
            attempts,
            diagnostic,
            ..
        } => SlotSnapshot {
            state: "failed".to_owned(),
            server,
            version: String::new(),
            pid: None,
            ready_ms: None,
            uptime_s: None,
            rss_mib: None,
            attempts: *attempts,
            diagnostic: Some(diagnostic.clone()),
        },
        Slot::Disabled {
            attempts,
            diagnostic,
        } => SlotSnapshot {
            state: "disabled".to_owned(),
            server,
            version: String::new(),
            pid: None,
            ready_ms: None,
            uptime_s: None,
            rss_mib: None,
            attempts: *attempts,
            diagnostic: Some(diagnostic.clone()),
        },
        Slot::Missing { diagnostic } => SlotSnapshot {
            state: "missing".to_owned(),
            server,
            version: String::new(),
            pid: None,
            ready_ms: None,
            uptime_s: None,
            rss_mib: None,
            attempts: 0,
            diagnostic: Some(diagnostic.clone()),
        },
    }
}
