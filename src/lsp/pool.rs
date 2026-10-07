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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures::Future;
use serde::{Deserialize, Serialize};
use sysinfo::System;

use super::normalize::LocationFilter;
use super::preopen::{PreopenPlan, plan};
use super::process::{StartContext, StartOutcome, Started, start_or_degrade, stop};
use super::readiness::wait_ready;
use super::spec::{Degraded, SpecInputs, resolve_program, spec_for};
use super::target::{LiveOutcome, LiveRequest, LiveStatus, run_target};
use super::uri::{CanonicalRoot, join_rel};
use crate::capability::languages::LspLang;
use crate::config::LspOverrides;
use crate::index::load::IndexStamp;

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

/// Calculate back-off duration for a failed attempt.
fn backoff_duration(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_secs(5),
        2 => Duration::from_secs(30),
        _ => Duration::from_secs(120),
    }
}

type StartCell = Arc<std::sync::Mutex<Option<StartOutcome>>>;

#[derive(Clone)]
struct SharedStart {
    started_at: Instant,
    attempts: u32,
    rx: tokio::sync::watch::Receiver<Option<StartCell>>,
}

enum Slot {
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

struct Inner {
    slots: BTreeMap<LspLang, Slot>,
    lang_mutexes: BTreeMap<LspLang, Arc<tokio::sync::Mutex<()>>>,
    current_stamp: Option<IndexStamp>,
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

fn repo_root_from_abs_rel(abs: &Path, rel: &str) -> PathBuf {
    let mut root = abs.to_path_buf();
    for _ in rel.split('/') {
        if let Some(parent) = root.parent() {
            root = parent.to_path_buf();
        }
    }
    root
}

async fn run_dispatcher(
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

/// What `ensure_ready` decided while it held the pool lock.
enum Next {
    /// Nothing to wait for: ready, or a failure to report.
    Done(Result<(), Box<LiveOutcome>>),
    /// A start is in flight: wait for it **without** the pool lock, so a
    /// snapshot (or another language) is not stuck behind a slow start.
    Wait(SharedStart),
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

/// Look at the slot of `lang` and start a server when there is none.
fn decide_start(
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

fn spawn_start_task(
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

struct StartTaskCtx<'a> {
    lang: LspLang,
    attempts: u32,
    started_at: Instant,
    stamp: Option<IndexStamp>,
    clock: &'a dyn Clock,
}

/// The start outcome, if the start task has produced one nobody took yet.
fn take_start_outcome(
    rx: &tokio::sync::watch::Receiver<Option<StartCell>>,
) -> Option<StartOutcome> {
    let cell = rx.borrow().clone()?;
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// Wait up to `ready_wait` for the start of `lang`, then record its outcome.
/// The pool lock is held only to read the stamp and to record the result.
async fn await_start_result(
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

fn handle_start_outcome(
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

fn snapshot_slot(slot: &Slot, lang: LspLang, sys: &mut System, now: Instant) -> SlotSnapshot {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::lsp::fake::{FakeScript, connect, test_spec};
    use crate::lsp::ops::ops_for;

    pub type ScriptFn = dyn Fn(
            LspLang,
        ) -> BoxFuture<
            'static,
            (
                super::super::session::LspSession,
                crate::lsp::fake::FakeHandle,
            ),
        > + Send
        + Sync;

    pub struct ScriptedFactory {
        pub script_fn: Arc<ScriptFn>,
        pub missing_lang: Option<LspLang>,
    }

    impl SessionFactory for ScriptedFactory {
        fn start(&self, lang: LspLang, _ctx: FactoryContext) -> BoxFuture<'static, StartOutcome> {
            if self.missing_lang == Some(lang) {
                return Box::pin(async move {
                    StartOutcome::Degraded(Degraded::unavailable(format!(
                        "{} not found in PATH",
                        lang.server_program()
                    )))
                });
            }
            let fut = (self.script_fn)(lang);
            Box::pin(async move {
                let (session, _handle) = fut.await;
                StartOutcome::Ready(Box::new(Started {
                    session,
                    process: None,
                }))
            })
        }
    }

    fn dummy_request(lang: LspLang, repo_dir: &Path) -> LiveRequest {
        let abs_file = repo_dir.join("src").join("lib.rs");
        LiveRequest {
            lang,
            target_id: "t1".to_owned(),
            abs_file: abs_file.clone(),
            rel_file: "src/lib.rs".to_owned(),
            position: async_lsp::lsp_types::Position {
                line: 0,
                character: 0,
            },
            ops: ops_for(crate::lsp::ops::TargetShape::Callable),
            heuristic_hint: 0,
            deadline: Instant::now() + Duration::from_secs(30),
            retry_pause: Duration::from_millis(10),
        }
    }

    fn setup_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("lib.rs"), "fn foo() {}\n").expect("write");
        tmp
    }

    fn default_factory() -> Arc<ScriptedFactory> {
        Arc::new(ScriptedFactory {
            script_fn: Arc::new(|lang| {
                Box::pin(async move {
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: None,
        })
    }

    #[test]
    fn query_starts_server_lazily_once() {
        let repo = setup_repo();
        let starts = Arc::new(AtomicU64::new(0));
        let starts_clone = Arc::clone(&starts);

        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(move |lang| {
                let starts_clone = Arc::clone(&starts_clone);
                Box::pin(async move {
                    starts_clone.fetch_add(1, Ordering::SeqCst);
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: None,
        });

        let pool = LspPool::new(factory, PoolSettings::default(), None).expect("pool");
        let req = dummy_request(LspLang::Rust, repo.path());
        let outcome = pool.query(req);

        assert_eq!(outcome.status, LiveStatus::Ok);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn second_query_reuses_ready_server() {
        let repo = setup_repo();
        let starts = Arc::new(AtomicU64::new(0));
        let starts_clone = Arc::clone(&starts);

        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(move |lang| {
                let starts_clone = Arc::clone(&starts_clone);
                Box::pin(async move {
                    starts_clone.fetch_add(1, Ordering::SeqCst);
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: None,
        });

        let pool = LspPool::new(factory, PoolSettings::default(), None).expect("pool");
        let outcome1 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        let outcome2 = pool.query(dummy_request(LspLang::Rust, repo.path()));

        assert_eq!(outcome1.status, LiveStatus::Ok);
        assert_eq!(outcome2.status, LiveStatus::Ok);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn not_ready_within_wait_returns_not_ready_and_keeps_starting() {
        let repo = setup_repo();
        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(|lang| {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: None,
        });

        let settings = PoolSettings {
            ready_wait: Duration::from_millis(20),
            idle: Duration::from_secs(900),
            reply_slack: Duration::from_secs(5),
        };
        let pool = LspPool::new(factory, settings, None).expect("pool");

        let outcome = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome.status, LiveStatus::NotReady);

        std::thread::sleep(Duration::from_millis(300));

        let outcome2 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome2.status, LiveStatus::Ok);
    }

    #[test]
    fn missing_server_is_terminal_without_restart() {
        let repo = setup_repo();
        let starts = Arc::new(AtomicU64::new(0));
        let starts_clone = Arc::clone(&starts);

        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(move |lang| {
                let starts_clone = Arc::clone(&starts_clone);
                Box::pin(async move {
                    starts_clone.fetch_add(1, Ordering::SeqCst);
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: Some(LspLang::Rust),
        });

        let pool = LspPool::new(factory, PoolSettings::default(), None).expect("pool");
        let outcome1 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        let outcome2 = pool.query(dummy_request(LspLang::Rust, repo.path()));

        assert_eq!(outcome1.status, LiveStatus::NoServer);
        assert_eq!(outcome2.status, LiveStatus::NoServer);
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn crash_restarts_with_backoff_then_disables() {
        let repo = setup_repo();
        let clock = Arc::new(TestClock::new());
        let attempts = Arc::new(AtomicU64::new(0));

        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(move |lang| {
                let attempts = Arc::clone(&attempts);
                Box::pin(async move {
                    let n = attempts.fetch_add(1, Ordering::SeqCst);
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    let (session, handle) = connect(FakeScript::full(), init).await;
                    if n < 3 {
                        handle.kill();
                    }
                    (session, handle)
                })
            }),
            missing_lang: None,
        });

        let pool = LspPool::new_with_clock(
            factory,
            PoolSettings::default(),
            None,
            Arc::clone(&clock) as Arc<dyn Clock>,
        )
        .expect("pool");

        // Attempt 1 fails -> attempts = 1
        let outcome1 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome1.status, LiveStatus::Failed);

        // Immediate query before 5s backoff fails due to backoff
        let outcome_backoff = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome_backoff.status, LiveStatus::Failed);

        // Attempt 2 fails -> attempts = 2
        clock.advance(Duration::from_secs(6));
        let outcome2 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome2.status, LiveStatus::Failed);

        // Attempt 3 fails -> attempts = 3
        clock.advance(Duration::from_secs(31));
        let outcome3 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome3.status, LiveStatus::Failed);

        // After 3 attempts failed, next query is disabled
        clock.advance(Duration::from_secs(121));
        let outcome4 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome4.status, LiveStatus::Failed);
        assert!(outcome4.detail.unwrap_or_default().contains("disabled"));
    }

    #[test]
    fn idle_server_is_stopped_and_restarted_on_demand() {
        let repo = setup_repo();
        let clock = Arc::new(TestClock::new());
        let starts = Arc::new(AtomicU64::new(0));
        let starts_clone = Arc::clone(&starts);

        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(move |lang| {
                let starts_clone = Arc::clone(&starts_clone);
                Box::pin(async move {
                    starts_clone.fetch_add(1, Ordering::SeqCst);
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: None,
        });

        let settings = PoolSettings {
            ready_wait: Duration::from_secs(5),
            idle: Duration::from_secs(10),
            reply_slack: Duration::from_secs(5),
        };
        let pool = LspPool::new_with_clock(
            factory,
            settings,
            None,
            Arc::clone(&clock) as Arc<dyn Clock>,
        )
        .expect("pool");

        let outcome1 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome1.status, LiveStatus::Ok);
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        clock.advance(Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(50));

        let outcome2 = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome2.status, LiveStatus::Ok);
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn invalidate_restarts_on_next_query() {
        let repo = setup_repo();
        let starts = Arc::new(AtomicU64::new(0));
        let starts_clone = Arc::clone(&starts);

        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(move |lang| {
                let starts_clone = Arc::clone(&starts_clone);
                Box::pin(async move {
                    starts_clone.fetch_add(1, Ordering::SeqCst);
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: None,
        });

        let stamp1 = IndexStamp {
            updated_at: "t1".to_owned(),
            source_fingerprint: "fp1".to_owned(),
        };
        let pool = LspPool::new(factory, PoolSettings::default(), Some(stamp1)).expect("pool");

        let _ = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        let stamp2 = IndexStamp {
            updated_at: "t2".to_owned(),
            source_fingerprint: "fp2".to_owned(),
        };
        pool.invalidate(stamp2);

        let _ = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn each_language_gets_its_own_server() {
        let repo = setup_repo();
        let pool = LspPool::new(default_factory(), PoolSettings::default(), None).expect("pool");

        let res_rust = pool.query(dummy_request(LspLang::Rust, repo.path()));
        let res_ts = pool.query(dummy_request(LspLang::Ts, repo.path()));

        assert_eq!(res_rust.status, LiveStatus::Ok);
        assert_eq!(res_ts.status, LiveStatus::Ok);
    }

    /// A factory whose Rust server takes `rust_start` to start; the others are instant.
    fn slow_rust_pool(rust_start: Duration) -> Arc<LspPool> {
        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(move |lang| {
                Box::pin(async move {
                    if lang == LspLang::Rust {
                        tokio::time::sleep(rust_start).await;
                    }
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: None,
        });
        Arc::new(LspPool::new(factory, PoolSettings::default(), None).expect("pool"))
    }

    /// Ask for the Rust target on another thread and give the start time to begin.
    fn ask_rust_in_background(
        pool: &Arc<LspPool>,
        repo: &Path,
    ) -> std::thread::JoinHandle<LiveOutcome> {
        let pool = Arc::clone(pool);
        let request = dummy_request(LspLang::Rust, repo);
        let asker = std::thread::spawn(move || pool.query(request));
        std::thread::sleep(Duration::from_millis(300));
        asker
    }

    #[test]
    fn snapshot_is_answered_while_a_server_is_starting() {
        let repo = setup_repo();
        let pool = slow_rust_pool(Duration::from_secs(2));
        let asker = ask_rust_in_background(&pool, repo.path());

        let started = Instant::now();
        let snapshot = pool.snapshot();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the snapshot waited {:?} for the start",
            started.elapsed()
        );
        assert_eq!(snapshot.slots["rust"].state, "starting", "{snapshot:?}");
        assert_eq!(asker.join().expect("join").status, LiveStatus::Ok);
    }

    #[test]
    fn another_language_is_answered_while_one_is_starting() {
        let repo = setup_repo();
        let pool = slow_rust_pool(Duration::from_secs(2));
        let asker = ask_rust_in_background(&pool, repo.path());

        let started = Instant::now();
        let ts = pool.query(dummy_request(LspLang::Ts, repo.path()));
        assert_eq!(ts.status, LiveStatus::Ok);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "TypeScript waited {:?} for the Rust start",
            started.elapsed()
        );
        assert_eq!(asker.join().expect("join").status, LiveStatus::Ok);
    }

    #[tokio::test]
    async fn query_from_inside_tokio_runtime_does_not_panic() {
        let repo = setup_repo();
        let pool = LspPool::new(default_factory(), PoolSettings::default(), None).expect("pool");

        let outcome = pool.query(dummy_request(LspLang::Rust, repo.path()));
        assert_eq!(outcome.status, LiveStatus::Ok);
    }

    #[test]
    fn wedged_pool_reply_times_out_at_deadline_plus_slack() {
        let repo = setup_repo();
        let factory = Arc::new(ScriptedFactory {
            script_fn: Arc::new(|lang| {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    let spec = test_spec(lang);
                    let init = crate::lsp::session::InitInputs {
                        spec: &spec,
                        repo_root: Path::new("/work/demo"),
                    };
                    connect(FakeScript::full(), init).await
                })
            }),
            missing_lang: None,
        });

        let settings = PoolSettings {
            ready_wait: Duration::from_millis(50),
            idle: Duration::from_secs(900),
            reply_slack: Duration::from_millis(100),
        };
        let pool = LspPool::new(factory, settings, None).expect("pool");

        let mut req = dummy_request(LspLang::Rust, repo.path());
        req.deadline = Instant::now() + Duration::from_millis(50);

        let outcome = pool.query(req);
        assert_eq!(outcome.status, LiveStatus::NotReady);
    }

    #[test]
    fn drop_shuts_down_all_servers() {
        let repo = setup_repo();
        let pool = LspPool::new(default_factory(), PoolSettings::default(), None).expect("pool");
        let _ = pool.query(dummy_request(LspLang::Rust, repo.path()));

        drop(pool);
    }

    #[test]
    fn snapshot_reports_states_and_versions() {
        let repo = setup_repo();
        let pool = LspPool::new(default_factory(), PoolSettings::default(), None).expect("pool");
        let _ = pool.query(dummy_request(LspLang::Rust, repo.path()));

        let snap = pool.snapshot();
        assert!(snap.slots.contains_key("rust"));
        let rust_slot = &snap.slots["rust"];
        assert_eq!(rust_slot.state, "ready");
        assert_eq!(rust_slot.server, "rust-analyzer");
    }
}
