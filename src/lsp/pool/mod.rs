//! Process-wide language-server pool (`LspPool`, P3-D8).
//!
//! One pool per process owns its own Tokio runtime on a dedicated OS thread
//! ("knot-lite-lsp") and exposes a *synchronous* public API safe to call from any
//! thread, including inside a Tokio runtime (it waits on a `std::sync::mpsc`
//! channel, never calling `block_on`).
//!
//! Slots (§6.3) transition through `NotStarted → Starting → Ready → Failed/Disabled`.
//! A crashed server restarts up to 3 times with back-off (5 s / 30 s / 120 s)
//! before being disabled for the process lifetime. Inactive `Ready` servers are
//! stopped after `idle` duration.
//!
//! Layout (P4.2): the public API lives here, [`factory`] builds real sessions,
//! [`slot`] holds the slot state machine, [`start`] the start flow of one server
//! and [`worker`] the dispatcher loop. Split mechanically in P4.2; P4.3 reworks
//! the locking.

mod factory;
mod slot;
mod start;
mod worker;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures::Future;
use serde::{Deserialize, Serialize};

use super::process::StartOutcome;
use super::target::{LiveOutcome, LiveRequest, LiveStatus};
use crate::capability::languages::LspLang;
use crate::index::load::IndexStamp;

pub use factory::ProcessFactory;
use worker::run_dispatcher;

/// How long a server dropped or shut down gets to exit gracefully.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval of the idle reaper.
const REAPER_INTERVAL: Duration = Duration::from_secs(30);

/// Abstract clock for deterministic testing of back-off and idle timers.
pub trait Clock: Send + Sync + 'static {
    /// Current instant.
    fn now(&self) -> Instant;
}

/// Production clock wrapping [`Instant::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A manually advanceable clock for tests.
#[derive(Debug, Clone)]
pub struct TestClock {
    start: Instant,
    offset_ms: Arc<AtomicU64>,
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TestClock {
    /// Create a test clock anchored at `Instant::now()`.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            offset_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Advance the test clock by `duration`.
    pub fn advance(&self, duration: Duration) {
        let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        self.offset_ms.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        let ms = self.offset_ms.load(Ordering::SeqCst);
        self.start + Duration::from_millis(ms)
    }
}

/// Inputs provided to [`SessionFactory::start`].
#[derive(Debug, Clone, Copy)]
pub struct FactoryContext {
    /// Suggested wait duration for readiness.
    pub ready_wait_hint: Duration,
}

/// Alias for an owned async future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Factory that produces initialised language servers (or diagnostics).
pub trait SessionFactory: Send + Sync + 'static {
    /// Start the language server for `lang`.
    fn start(&self, lang: LspLang, ctx: FactoryContext) -> BoxFuture<'static, StartOutcome>;
}

/// Tuning parameters for [`LspPool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSettings {
    /// How long a query waits for a starting server before returning `NotReady`.
    pub ready_wait: Duration,
    /// Idle time after which a server is stopped (`Duration::ZERO` disables).
    pub idle: Duration,
    /// Additional time given to the pool channel reply before the caller times out.
    pub reply_slack: Duration,
}

impl Default for PoolSettings {
    fn default() -> Self {
        Self {
            ready_wait: Duration::from_secs(20),
            idle: Duration::from_secs(super::settings::DEFAULT_IDLE_SECS),
            reply_slack: Duration::from_secs(5),
        }
    }
}

/// State snapshot of one language slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotSnapshot {
    /// State name (`"not_started"`, `"starting"`, `"ready"`, `"failed"`, `"disabled"`, `"missing"`).
    pub state: String,
    /// Executable / server name.
    pub server: String,
    /// Server version string from `serverInfo`.
    pub version: String,
    /// Process ID when running as a child process.
    pub pid: Option<u32>,
    /// Milliseconds taken for the server to report ready.
    pub ready_ms: Option<u64>,
    /// Seconds the server has been running.
    pub uptime_s: Option<u64>,
    /// Process tree resident memory in MiB.
    pub rss_mib: Option<f64>,
    /// Number of failed start / crash attempts.
    pub attempts: u32,
    /// Explanation when degraded or failed.
    pub diagnostic: Option<String>,
}

/// Snapshot of every language slot in the pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolSnapshot {
    /// Per-language slot state summaries.
    pub slots: BTreeMap<String, SlotSnapshot>,
}

/// Commands sent to the pool worker thread.
enum Command {
    Query {
        req: LiveRequest,
        reply: std::sync::mpsc::Sender<LiveOutcome>,
    },
    Warm {
        lang: LspLang,
    },
    Snapshot {
        reply: std::sync::mpsc::Sender<PoolSnapshot>,
    },
    Invalidate {
        stamp: IndexStamp,
    },
    Shutdown {
        reply: std::sync::mpsc::Sender<()>,
    },
}

/// Synchronous thread-safe wrapper around a background Tokio LSP pool.
pub struct LspPool {
    tx: std::sync::mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    settings: PoolSettings,
}

impl LspPool {
    /// Create a new [`LspPool`] with [`SystemClock`].
    pub fn new(
        factory: Arc<dyn SessionFactory>,
        settings: PoolSettings,
        stamp: Option<IndexStamp>,
    ) -> anyhow::Result<Self> {
        Self::new_with_clock(factory, settings, stamp, Arc::new(SystemClock))
    }

    /// Create a new [`LspPool`] with a custom [`Clock`].
    pub fn new_with_clock(
        factory: Arc<dyn SessionFactory>,
        settings: PoolSettings,
        stamp: Option<IndexStamp>,
        clock: Arc<dyn Clock>,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let pool_settings = settings.clone();

        let thread = std::thread::Builder::new()
            .name("knot-lite-lsp".to_owned())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                let rt = match rt {
                    Ok(rt) => rt,
                    Err(error) => {
                        tracing::error!("failed to create LSP Tokio runtime: {error}");
                        return;
                    }
                };
                rt.block_on(run_dispatcher(rx, factory, pool_settings, stamp, clock));
            })?;

        Ok(Self {
            tx,
            thread: Some(thread),
            settings,
        })
    }

    /// Query the language server for `req`. Never panics, never blocks past
    /// `req.deadline + reply_slack`.
    pub fn query(&self, req: LiveRequest) -> LiveOutcome {
        let (reply, reply_rx) = std::sync::mpsc::channel();
        let deadline = req.deadline;
        let slack = self.settings.reply_slack;
        let lang = req.lang;

        if self.tx.send(Command::Query { req, reply }).is_err() {
            return LiveOutcome::without_answer(
                LiveStatus::Failed,
                "LSP pool worker thread has stopped",
            );
        }

        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .saturating_add(slack);

        match reply_rx.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => LiveOutcome::without_answer(
                LiveStatus::Deadline,
                format!(
                    "LSP pool query for {lang:?} timed out after deadline + {} s slack",
                    slack.as_secs()
                ),
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => LiveOutcome::without_answer(
                LiveStatus::Failed,
                "LSP pool channel disconnected during query",
            ),
        }
    }

    /// Pre-warm the language server of `lang` asynchronously.
    pub fn warm(&self, lang: LspLang) {
        let _ = self.tx.send(Command::Warm { lang });
    }

    /// Produce a snapshot of slot states.
    pub fn snapshot(&self) -> PoolSnapshot {
        let (reply, reply_rx) = std::sync::mpsc::channel();
        if self.tx.send(Command::Snapshot { reply }).is_err() {
            return PoolSnapshot {
                slots: BTreeMap::new(),
            };
        }
        reply_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|_| PoolSnapshot {
                slots: BTreeMap::new(),
            })
    }

    /// Invalidate existing server sessions when the repository index updates.
    pub fn invalidate(&self, stamp: IndexStamp) {
        let _ = self.tx.send(Command::Invalidate { stamp });
    }
}

impl Drop for LspPool {
    fn drop(&mut self) {
        let (reply, reply_rx) = std::sync::mpsc::channel();
        if self.tx.send(Command::Shutdown { reply }).is_ok() {
            let _ = reply_rx.recv_timeout(Duration::from_secs(8));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
