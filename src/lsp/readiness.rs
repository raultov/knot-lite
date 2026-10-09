//! LSP readiness state machine.
//!
//! The client router feeds [`ReadinessState`] through a `watch` channel;
//! [`wait_ready`] turns the event stream into a single ready/not-ready outcome.
//! Ported from the Phase 0 spike (T4.3); the signals each server announces were
//! measured there (G5–G7).

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

/// rust-analyzer progress tokens that mark indexing work (old and new names).
pub const RA_INDEXING_TOKENS: &[&str] = &["rustAnalyzer/Indexing", "rustAnalyzer/cachePriming"];

/// How long the caller waits after `didOpen` when a lazy server reports no
/// progress at all before assuming there is nothing to wait for.
const NO_PROGRESS_GRACE: Duration = Duration::from_secs(5);

/// How long rust-analyzer may stay silent before the progress-idle fallback
/// kicks in.
const RA_STATUS_FALLBACK: Duration = Duration::from_secs(30);

/// Poll granularity of [`wait_ready`] between state changes.
const POLL_TICK: Duration = Duration::from_millis(100);

/// How a server announces "ready".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessKind {
    /// rust-analyzer: `experimental/serverStatus` with `quiescent`.
    RustAnalyzerStatus,
    /// jdtls: `language/status` with `type == "ServiceReady"`.
    JdtlsServiceReady,
    /// typescript-language-server: `$/progress` quiet period.
    ProgressIdle,
}

/// Live readiness state, fed by the client router and watched by [`wait_ready`].
#[derive(Debug, Clone, Default)]
pub struct ReadinessState {
    /// token -> title of in-flight `$/progress` work.
    pub open_progress: BTreeMap<String, String>,
    /// Total `$/progress` notifications seen.
    pub progress_events: u64,
    /// Completed begin/end pairs.
    pub progress_pairs: u64,
    /// First observed server activity (start of the no-progress grace period).
    pub first_activity: Option<Instant>,
    /// Last observed server activity (idle detection).
    pub last_activity: Option<Instant>,
    /// Last `experimental/serverStatus` quiescent flag.
    pub ra_quiescent: Option<bool>,
    /// Last `experimental/serverStatus` health.
    pub ra_health: Option<String>,
    /// Whether any `experimental/serverStatus` arrived.
    pub ra_status_seen: bool,
    /// Whether a rust-analyzer indexing token has ended at least once.
    pub ra_indexing_ended: bool,
    /// Last `language/status` type (jdtls).
    pub jdtls_status: Option<String>,
    /// Whether jdtls has announced `ServiceReady` at least once. Sticky: jdtls
    /// keeps sending `Starting` status reports for the progress of its
    /// initialization jobs (a Gradle import goes on after `ServiceReady`), so
    /// the *last* type says nothing about readiness.
    pub jdtls_service_ready: bool,
    /// Every distinct `language/status.type` with its unix-ms timestamp.
    pub jdtls_status_timeline: Vec<(u64, String)>,
    /// Why the fed state considers itself ready (set when a signal arrives).
    pub ready_reason: Option<String>,
}

impl ReadinessState {
    /// rust-analyzer's last reported `quiescent` flag; `None` before the first
    /// `experimental/serverStatus` (and for every other server). Retry-on-empty
    /// uses it: a quiescent server's empty answer is trustworthy.
    pub fn quiescent_known(&self) -> Option<bool> {
        self.ra_quiescent
    }

    fn note_activity(&mut self) {
        let now = Instant::now();
        self.first_activity.get_or_insert(now);
        self.last_activity = Some(now);
    }

    /// A `$/progress` `begin` notification.
    pub fn on_progress_begin(&mut self, token: String, title: String) {
        self.note_activity();
        self.progress_events += 1;
        self.open_progress.insert(token, title);
    }

    /// A `$/progress` `report` notification.
    pub fn on_progress_report(&mut self, token: &str) {
        self.note_activity();
        self.progress_events += 1;
        if !self.open_progress.contains_key(token) {
            self.open_progress.insert(token.to_owned(), String::new());
        }
    }

    /// A `$/progress` `end` notification.
    pub fn on_progress_end(&mut self, token: &str) {
        self.note_activity();
        self.progress_events += 1;
        if self.open_progress.remove(token).is_some() {
            self.progress_pairs += 1;
        }
        if RA_INDEXING_TOKENS.contains(&token) {
            self.ra_indexing_ended = true;
        }
    }

    /// An `experimental/serverStatus` notification (rust-analyzer).
    pub fn on_server_status(&mut self, health: &str, quiescent: bool) {
        self.note_activity();
        self.ra_status_seen = true;
        self.ra_quiescent = Some(quiescent);
        self.ra_health = Some(health.to_owned());
        if quiescent && health != "error" {
            self.ready_reason = Some("serverStatus quiescent".to_owned());
        }
    }

    /// A `language/status` notification (jdtls).
    pub fn on_language_status(&mut self, typ: &str) {
        tracing::debug!(typ, "language/status");
        self.note_activity();
        self.jdtls_status = Some(typ.to_owned());
        let ts = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        if !self
            .jdtls_status_timeline
            .iter()
            .any(|(_, seen)| seen == typ)
        {
            self.jdtls_status_timeline.push((ts, typ.to_owned()));
        }
        if typ == "ServiceReady" {
            self.jdtls_service_ready = true;
            self.ready_reason = Some("ServiceReady".to_owned());
        }
    }
}

/// Result of [`wait_ready`].
#[derive(Debug, Clone)]
pub struct ReadyOutcome {
    /// Whether the server reported ready before the timeout.
    pub ready: bool,
    /// Why (`serverStatus quiescent`, `progress idle`, `timeout`, …).
    pub reason: String,
    /// Time spent waiting.
    pub elapsed: Duration,
}

/// Poll the readiness state until the server reports ready or `timeout` passes.
///
/// On timeout the outcome has `ready = false` and `reason = "timeout"`; the
/// caller decides what to do with a server that is still starting.
pub async fn wait_ready(
    rx: &mut watch::Receiver<ReadinessState>,
    kind: ReadinessKind,
    idle: Duration,
    timeout: Duration,
) -> ReadyOutcome {
    let started = Instant::now();
    loop {
        {
            let state = rx.borrow_and_update();
            if let Some(reason) = evaluate(&state, kind, idle, Instant::now(), started) {
                return ReadyOutcome {
                    ready: true,
                    reason: reason.to_owned(),
                    elapsed: started.elapsed(),
                };
            }
        }
        if started.elapsed() >= timeout {
            return ReadyOutcome {
                ready: false,
                reason: "timeout".to_owned(),
                elapsed: started.elapsed(),
            };
        }
        let changed = tokio::select! {
            changed = rx.changed() => changed.is_ok(),
            () = tokio::time::sleep(POLL_TICK) => true,
        };
        if !changed {
            return ReadyOutcome {
                ready: false,
                reason: "state channel closed".to_owned(),
                elapsed: started.elapsed(),
            };
        }
    }
}

/// Ready-condition evaluation, separated from the async loop for unit tests.
fn evaluate(
    state: &ReadinessState,
    kind: ReadinessKind,
    idle: Duration,
    now: Instant,
    started: Instant,
) -> Option<&'static str> {
    match kind {
        ReadinessKind::RustAnalyzerStatus => {
            if state.ra_status_seen {
                let quiescent = state.ra_quiescent.unwrap_or(false);
                let healthy = state.ra_health.as_deref().is_none_or(|h| h != "error");
                return (quiescent && healthy).then_some("serverStatus quiescent");
            }
            // Fallback when rust-analyzer never reports status: progress-idle
            // plus proof that the indexing tokens have completed once.
            let silent_long_enough = started.elapsed() >= RA_STATUS_FALLBACK;
            (silent_long_enough && state.ra_indexing_ended && progress_idle(state, idle, now))
                .then_some("fallback progress-idle after indexing")
        }
        ReadinessKind::JdtlsServiceReady => (state.jdtls_service_ready
            && progress_quiet(state, idle, now))
        .then_some("ServiceReady and progress idle"),
        ReadinessKind::ProgressIdle => progress_idle(state, idle, now).then_some("progress idle"),
    }
}

/// `$/progress` is quiet: nothing open, idle for `idle`, and either a completed
/// pair was observed or no progress at all was seen within the grace period.
fn progress_idle(state: &ReadinessState, idle: Duration, now: Instant) -> bool {
    let seen_pair = state.progress_pairs >= 1;
    let grace = state.progress_events == 0
        && state
            .first_activity
            .is_some_and(|t| now.duration_since(t) >= NO_PROGRESS_GRACE);
    (seen_pair || grace) && progress_quiet(state, idle, now)
}

fn progress_quiet(state: &ReadinessState, idle: Duration, now: Instant) -> bool {
    let idle_ok = state
        .last_activity
        .is_none_or(|t| now.duration_since(t) >= idle);
    state.open_progress.is_empty() && idle_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_millis(2000);

    fn base() -> ReadinessState {
        ReadinessState::default()
    }

    fn old(now: Instant, ago: Duration) -> Instant {
        now.checked_sub(ago).unwrap_or(now)
    }

    #[test]
    fn rust_analyzer_ready_on_quiescent_status() {
        let mut state = base();
        state.on_server_status("ok", true);
        let now = Instant::now();
        assert_eq!(
            evaluate(&state, ReadinessKind::RustAnalyzerStatus, IDLE, now, now),
            Some("serverStatus quiescent")
        );
    }

    #[test]
    fn rust_analyzer_error_health_is_not_ready() {
        let mut state = base();
        state.on_server_status("error", true);
        let now = Instant::now();
        assert_eq!(
            evaluate(&state, ReadinessKind::RustAnalyzerStatus, IDLE, now, now),
            None
        );
    }

    #[test]
    fn rust_analyzer_not_ready_while_indexing() {
        let mut state = base();
        state.on_server_status("ok", false);
        let now = Instant::now();
        assert_eq!(
            evaluate(&state, ReadinessKind::RustAnalyzerStatus, IDLE, now, now),
            None
        );
    }

    #[test]
    fn rust_analyzer_fallback_needs_indexing_tokens_ended() {
        let now = Instant::now();
        let started = old(now, Duration::from_secs(31));
        let mut state = base();
        state.on_progress_begin("rustAnalyzer/Indexing".to_owned(), "Indexing".to_owned());
        state.last_activity = Some(old(now, IDLE * 2));
        // Still open → not ready.
        assert_eq!(
            evaluate(
                &state,
                ReadinessKind::RustAnalyzerStatus,
                IDLE,
                now,
                started
            ),
            None
        );
        // Indexing ends → ready through the fallback.
        state.on_progress_end("rustAnalyzer/Indexing");
        state.last_activity = Some(old(now, IDLE * 2));
        assert_eq!(
            evaluate(
                &state,
                ReadinessKind::RustAnalyzerStatus,
                IDLE,
                now,
                started
            ),
            Some("fallback progress-idle after indexing")
        );
    }

    #[test]
    fn rust_analyzer_fallback_needs_thirty_seconds_of_silence() {
        let now = Instant::now();
        let started = old(now, Duration::from_secs(5));
        let mut state = base();
        state.on_progress_end("rustAnalyzer/cachePriming");
        state.last_activity = Some(old(now, IDLE * 2));
        assert_eq!(
            evaluate(
                &state,
                ReadinessKind::RustAnalyzerStatus,
                IDLE,
                now,
                started
            ),
            None
        );
    }

    #[test]
    fn jdtls_needs_service_ready_and_quiet_progress() {
        let now = Instant::now();
        let mut state = base();
        state.on_language_status("Starting");
        assert_eq!(
            evaluate(&state, ReadinessKind::JdtlsServiceReady, IDLE, now, now),
            None
        );
        state.on_language_status("ServiceReady");
        state.on_progress_begin("1".to_owned(), "Importing".to_owned());
        state.last_activity = Some(old(now, IDLE * 2));
        // Progress still open → not ready.
        assert_eq!(
            evaluate(&state, ReadinessKind::JdtlsServiceReady, IDLE, now, now),
            None
        );
        state.on_progress_end("1");
        state.last_activity = Some(old(now, IDLE * 2));
        assert_eq!(
            evaluate(&state, ReadinessKind::JdtlsServiceReady, IDLE, now, now),
            Some("ServiceReady and progress idle")
        );
    }

    #[test]
    fn jdtls_stays_ready_when_starting_reports_follow_service_ready() {
        // A Gradle import: jdtls announces `ServiceReady`, then keeps reporting
        // the progress of its initialization jobs as `Starting` (measured,
        // Appendix D P4.7). Readiness waits for the reports to stop, no more.
        let now = Instant::now();
        let mut state = base();
        state.on_language_status("Starting");
        state.on_language_status("ServiceReady");
        state.on_language_status("Starting");
        assert_eq!(state.jdtls_status.as_deref(), Some("Starting"));
        // A report just arrived: still settling.
        assert_eq!(
            evaluate(&state, ReadinessKind::JdtlsServiceReady, IDLE, now, now),
            None
        );
        state.last_activity = Some(old(now, IDLE * 2));
        assert_eq!(
            evaluate(&state, ReadinessKind::JdtlsServiceReady, IDLE, now, now),
            Some("ServiceReady and progress idle")
        );
    }

    #[test]
    fn jdtls_is_not_ready_before_service_ready_however_quiet() {
        let now = Instant::now();
        let mut state = base();
        state.on_language_status("Starting");
        state.last_activity = Some(old(now, IDLE * 10));
        assert!(!state.jdtls_service_ready);
        assert_eq!(
            evaluate(&state, ReadinessKind::JdtlsServiceReady, IDLE, now, now),
            None
        );
    }

    #[test]
    fn jdtls_status_timeline_keeps_distinct_types() {
        let mut state = base();
        state.on_language_status("Starting");
        state.on_language_status("Starting");
        state.on_language_status("ServiceReady");
        let kinds: Vec<&str> = state
            .jdtls_status_timeline
            .iter()
            .map(|(_, k)| k.as_str())
            .collect();
        assert_eq!(kinds, vec!["Starting", "ServiceReady"]);
    }

    #[test]
    fn quiescent_known_tracks_last_status() {
        let mut state = base();
        assert_eq!(state.quiescent_known(), None);
        state.on_server_status("ok", false);
        assert_eq!(state.quiescent_known(), Some(false));
        state.on_server_status("ok", true);
        assert_eq!(state.quiescent_known(), Some(true));
    }

    #[tokio::test]
    async fn wait_ready_returns_when_the_server_becomes_quiescent() {
        let (tx, mut rx) = watch::channel(ReadinessState::default());
        let feeder = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send_modify(|state| state.on_server_status("ok", true));
            tx
        });
        let outcome = wait_ready(
            &mut rx,
            ReadinessKind::RustAnalyzerStatus,
            IDLE,
            Duration::from_secs(5),
        )
        .await;
        assert!(outcome.ready, "{outcome:?}");
        assert_eq!(outcome.reason, "serverStatus quiescent");
        assert!(outcome.elapsed < Duration::from_secs(4));
        drop(feeder.await.expect("feeder"));
    }

    #[tokio::test]
    async fn wait_ready_times_out_without_signals() {
        let (_tx, mut rx) = watch::channel(ReadinessState::default());
        let outcome = wait_ready(
            &mut rx,
            ReadinessKind::JdtlsServiceReady,
            IDLE,
            Duration::from_millis(250),
        )
        .await;
        assert!(!outcome.ready);
        assert_eq!(outcome.reason, "timeout");
        assert!(outcome.elapsed >= Duration::from_millis(250));
    }

    #[tokio::test]
    async fn wait_ready_reports_a_closed_state_channel() {
        let (tx, mut rx) = watch::channel(ReadinessState::default());
        drop(tx);
        let outcome = wait_ready(
            &mut rx,
            ReadinessKind::ProgressIdle,
            IDLE,
            Duration::from_secs(5),
        )
        .await;
        assert!(!outcome.ready);
        assert_eq!(outcome.reason, "state channel closed");
    }

    #[test]
    fn progress_idle_waits_for_a_completed_pair() {
        let now = Instant::now();
        let mut state = base();
        state.on_progress_begin("p".to_owned(), "t".to_owned());
        state.on_progress_end("p");
        state.last_activity = Some(old(now, IDLE * 2));
        assert_eq!(
            evaluate(&state, ReadinessKind::ProgressIdle, IDLE, now, now),
            Some("progress idle")
        );
    }

    #[test]
    fn progress_idle_blocked_by_open_token() {
        let now = Instant::now();
        let mut state = base();
        state.on_progress_begin("p".to_owned(), "t".to_owned());
        state.on_progress_begin("q".to_owned(), "t".to_owned());
        state.on_progress_end("p");
        state.last_activity = Some(old(now, IDLE * 2));
        assert_eq!(
            evaluate(&state, ReadinessKind::ProgressIdle, IDLE, now, now),
            None
        );
    }

    #[test]
    fn progress_idle_grace_when_no_progress_at_all() {
        let now = Instant::now();
        let mut state = base();
        state.note_activity();
        state.first_activity = Some(old(now, Duration::from_secs(6)));
        state.last_activity = Some(old(now, IDLE * 2));
        assert_eq!(
            evaluate(&state, ReadinessKind::ProgressIdle, IDLE, now, now),
            Some("progress idle")
        );
    }

    #[test]
    fn progress_idle_no_grace_while_recently_active() {
        let now = Instant::now();
        let mut state = base();
        state.note_activity();
        state.first_activity = Some(old(now, Duration::from_secs(6)));
        state.last_activity = Some(now);
        assert_eq!(
            evaluate(&state, ReadinessKind::ProgressIdle, IDLE, now, now),
            None
        );
    }
}
