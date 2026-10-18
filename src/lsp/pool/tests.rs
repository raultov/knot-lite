//! Pool tests over the scripted in-process fake servers (no real server).

use std::path::Path;

use super::*;
use crate::lsp::fake::{FakeCounters, FakeScript, FakeServers, connect, test_spec};
use crate::lsp::ops::ops_for;
use crate::lsp::process::Started;
use crate::lsp::spec::Degraded;

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
        changed_files: Arc::new(Vec::new()),
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
        ..PoolSettings::default()
    };
    let pool = LspPool::new(factory, settings, None).expect("pool");

    let outcome = pool.query(dummy_request(LspLang::Rust, repo.path()));
    assert_eq!(outcome.status, LiveStatus::NotReady);

    std::thread::sleep(Duration::from_millis(300));

    let outcome2 = pool.query(dummy_request(LspLang::Rust, repo.path()));
    assert_eq!(outcome2.status, LiveStatus::Ok);
}

#[test]
fn not_ready_server_returns_outline_request_without_blocking() {
    let repo = setup_repo();
    let factory = Arc::new(ScriptedFactory {
        script_fn: Arc::new(|lang| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
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
        ..PoolSettings::default()
    };
    let pool = LspPool::new(factory, settings, None).expect("pool");
    let mut req = dummy_request(LspLang::Rust, repo.path());
    req.ops = crate::lsp::ops::OpSet::outline();
    let started = std::time::Instant::now();
    let outcome = pool.query(req);
    assert_eq!(outcome.status, LiveStatus::NotReady);
    assert_eq!(outcome.symbols, None);
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "answered within the ready wait, not after the start: {:?}",
        started.elapsed()
    );
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
        ..PoolSettings::default()
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
        ..PoolSettings::default()
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

// --- P4.3: per-language cells (Appendix E §E.1) ----------------------------
//
// The concurrency tests use a delay on `textDocument/references` only: the
// target's first operations stay instant, so "a query is in flight" is exactly
// "the language's cell is locked". Margins are generous (no assertion tighter
// than 500 ms against a 1 500 ms delay).

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use super::slot::Shared;
use super::worker::{TaskCtx, query_task, reap_idle_servers};

fn slow_references(delay: Duration) -> FakeScript {
    FakeScript {
        delay,
        delay_only: Some("textDocument/references"),
        ..FakeScript::full()
    }
}

/// Counters of every fake server started so far.
type FakeCountersSeen = Arc<Mutex<Vec<Arc<FakeCounters>>>>;

/// Scripted servers whose counters the test keeps (request order, shutdown).
struct CountingFactory {
    script: Box<dyn Fn(LspLang) -> FakeScript + Send + Sync>,
    counters: FakeCountersSeen,
}

impl CountingFactory {
    fn new(
        script: impl Fn(LspLang) -> FakeScript + Send + Sync + 'static,
    ) -> (Arc<Self>, FakeCountersSeen) {
        let counters = Arc::new(Mutex::new(Vec::new()));
        let factory = Arc::new(Self {
            script: Box::new(script),
            counters: Arc::clone(&counters),
        });
        (factory, counters)
    }
}

impl SessionFactory for CountingFactory {
    fn start(&self, lang: LspLang, _ctx: FactoryContext) -> BoxFuture<'static, StartOutcome> {
        let script = (self.script)(lang);
        let counters = Arc::clone(&self.counters);
        Box::pin(async move {
            let spec = test_spec(lang);
            let init = crate::lsp::session::InitInputs {
                spec: &spec,
                repo_root: Path::new("/work/demo"),
            };
            let (session, handle) = connect(script, init).await;
            counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(Arc::clone(&handle.counters));
            StartOutcome::Ready(Box::new(Started {
                session,
                process: None,
            }))
        })
    }
}

/// Every recorded method of every server, in arrival order.
fn all_requests(counters: &FakeCountersSeen) -> Vec<String> {
    let guard = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.iter().flat_map(|c| c.requests()).collect::<Vec<_>>()
}

#[test]
fn a_slow_query_of_one_language_does_not_delay_another_language() {
    let repo = setup_repo();
    let (factory, _) = CountingFactory::new(|lang| match lang {
        LspLang::Java => slow_references(Duration::from_millis(1500)),
        _ => FakeScript::full(),
    });
    let pool = Arc::new(LspPool::new(factory, PoolSettings::default(), None).expect("pool"));

    // Warm both languages with one query each (the Java one already pays the
    // delay once: that is its "cold" measurement, not part of the assertion).
    assert_eq!(
        pool.query(dummy_request(LspLang::Rust, repo.path())).status,
        LiveStatus::Ok
    );
    assert_eq!(
        pool.query(dummy_request(LspLang::Java, repo.path())).status,
        LiveStatus::Ok
    );

    let java_done = Arc::new(AtomicBool::new(false));
    let java_flag = Arc::clone(&java_done);
    let pool_java = Arc::clone(&pool);
    let java_repo = repo.path().to_path_buf();
    let java = std::thread::spawn(move || {
        let outcome = pool_java.query(dummy_request(LspLang::Java, &java_repo));
        java_flag.store(true, Ordering::SeqCst);
        outcome
    });

    std::thread::sleep(Duration::from_millis(100));
    let started_at = Instant::now();
    let rust = pool.query(dummy_request(LspLang::Rust, repo.path()));
    let rust_ms = started_at.elapsed();

    assert_eq!(rust.status, LiveStatus::Ok);
    assert!(
        rust_ms < Duration::from_millis(750),
        "the Rust query waited for Java: {rust_ms:?}"
    );
    assert!(
        !java_done.load(Ordering::SeqCst),
        "the Rust query outlived the Java query: {rust_ms:?}"
    );
    assert_eq!(java.join().expect("java thread").status, LiveStatus::Ok);
}

#[test]
fn snapshot_is_answered_while_a_query_runs() {
    let repo = setup_repo();
    let (factory, _) = CountingFactory::new(|lang| match lang {
        LspLang::Java => slow_references(Duration::from_millis(1500)),
        _ => FakeScript::full(),
    });
    let pool = Arc::new(LspPool::new(factory, PoolSettings::default(), None).expect("pool"));
    assert_eq!(
        pool.query(dummy_request(LspLang::Java, repo.path())).status,
        LiveStatus::Ok
    );

    let pool_java = Arc::clone(&pool);
    let java_repo = repo.path().to_path_buf();
    let java =
        std::thread::spawn(move || pool_java.query(dummy_request(LspLang::Java, &java_repo)));

    std::thread::sleep(Duration::from_millis(100));
    let started_at = Instant::now();
    let snap = pool.snapshot();
    let snap_ms = started_at.elapsed();

    assert!(
        snap_ms < Duration::from_millis(500),
        "snapshot waited for the running query: {snap_ms:?}"
    );
    assert_eq!(snap.slots["java"].state, "ready");
    assert_eq!(snap.slots["rust"].state, "not_started");
    assert_eq!(java.join().expect("java thread").status, LiveStatus::Ok);
}

#[test]
fn invalidate_is_not_blocked_by_a_running_query() {
    let repo = setup_repo();
    let servers = FakeServers::new(|lang| match lang {
        LspLang::Java => slow_references(Duration::from_millis(1500)),
        _ => FakeScript::full(),
    });
    let starts = servers.starts();
    let pool =
        Arc::new(LspPool::new(Arc::new(servers), PoolSettings::default(), None).expect("pool"));
    assert_eq!(
        pool.query(dummy_request(LspLang::Java, repo.path())).status,
        LiveStatus::Ok
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    let pool_java = Arc::clone(&pool);
    let java_repo = repo.path().to_path_buf();
    let java =
        std::thread::spawn(move || pool_java.query(dummy_request(LspLang::Java, &java_repo)));

    std::thread::sleep(Duration::from_millis(100));
    let started_at = Instant::now();
    pool.invalidate(IndexStamp {
        updated_at: "t2".to_owned(),
        source_fingerprint: "fp2".to_owned(),
    });
    let snap = pool.snapshot();
    let blocked_ms = started_at.elapsed();

    assert!(
        blocked_ms < Duration::from_millis(500),
        "invalidate + snapshot waited for the running query: {blocked_ms:?}"
    );
    assert_eq!(snap.slots["java"].state, "ready");
    assert_eq!(java.join().expect("java thread").status, LiveStatus::Ok);

    // The index changed under the running server: the next query restarts it.
    assert_eq!(
        pool.query(dummy_request(LspLang::Java, repo.path())).status,
        LiveStatus::Ok
    );
    assert_eq!(starts.load(Ordering::SeqCst), 2);
}

#[test]
fn queries_of_the_same_language_stay_serialised() {
    let repo = setup_repo();
    let (factory, counters) = CountingFactory::new(|lang| match lang {
        LspLang::Rust => slow_references(Duration::from_millis(500)),
        _ => FakeScript::full(),
    });
    let pool = Arc::new(LspPool::new(factory, PoolSettings::default(), None).expect("pool"));

    let pool_2 = Arc::clone(&pool);
    let repo_2 = repo.path().to_path_buf();
    let second = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        pool_2.query(dummy_request(LspLang::Rust, &repo_2))
    });
    let first = pool.query(dummy_request(LspLang::Rust, repo.path()));
    let second = second.join().expect("second thread");

    assert_eq!(first.status, LiveStatus::Ok);
    assert_eq!(second.status, LiveStatus::Ok);

    // Serialised ⇒ one target's whole request sequence precedes the other's:
    // the second `prepareCallHierarchy` arrives after the first
    // `textDocument/references`. Interleaved requests would prove parallelism.
    let requests = all_requests(&counters);
    let prepare = "textDocument/prepareCallHierarchy";
    let references = "textDocument/references";
    let first_references = requests
        .iter()
        .position(|m| m == references)
        .expect("a references request");
    let prepares: Vec<usize> = requests
        .iter()
        .enumerate()
        .filter(|(_, m)| m.as_str() == prepare)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(prepares.len(), 2, "requests: {requests:?}");
    assert!(
        prepares[1] > first_references,
        "the second query overlapped the first: {requests:?}"
    );
}

#[tokio::test]
async fn idle_reaper_skips_a_language_with_a_query_in_flight() {
    let repo = setup_repo();
    let clock = Arc::new(TestClock::new());
    let servers = FakeServers::new(|lang| match lang {
        LspLang::Java => slow_references(Duration::from_millis(400)),
        _ => FakeScript::full(),
    });
    let starts = servers.starts();
    let settings = PoolSettings {
        ready_wait: Duration::from_secs(5),
        idle: Duration::from_secs(10),
        reply_slack: Duration::from_secs(5),
        ..PoolSettings::default()
    };
    let ctx = TaskCtx {
        shared: Arc::new(Shared::new(None)),
        factory: Arc::new(servers),
        settings,
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
    };

    // Start the Java server with one query (400 ms of scripted references).
    let warm = query_task(ctx.clone(), dummy_request(LspLang::Java, repo.path()));
    assert_eq!(warm.await.status, LiveStatus::Ok);
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    // A second query holds the language's cell while it runs.
    let slow = tokio::spawn(query_task(
        ctx.clone(),
        dummy_request(LspLang::Java, repo.path()),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Past `idle`, but the language is in use: the reaper must skip it.
    clock.advance(Duration::from_secs(15));
    reap_idle_servers(&ctx).await;
    assert_eq!(ctx.shared.cell(LspLang::Java).view().state, "ready");
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(slow.await.expect("slow task").status, LiveStatus::Ok);

    // Now it is idle: the next round stops it.
    clock.advance(Duration::from_secs(15));
    reap_idle_servers(&ctx).await;
    assert_eq!(ctx.shared.cell(LspLang::Java).view().state, "not_started");
}

#[test]
fn shutdown_aborts_in_flight_queries_and_stops_servers_in_parallel() {
    let repo = setup_repo();
    let (factory, counters) = CountingFactory::new(|lang| match lang {
        LspLang::Java => slow_references(Duration::from_secs(30)),
        _ => FakeScript::full(),
    });
    let settings = PoolSettings {
        reply_slack: Duration::from_millis(100),
        ..PoolSettings::default()
    };
    let pool = LspPool::new(factory, settings, None).expect("pool");

    assert_eq!(
        pool.query(dummy_request(LspLang::Rust, repo.path())).status,
        LiveStatus::Ok
    );
    assert_eq!(
        pool.query(dummy_request(LspLang::Ts, repo.path())).status,
        LiveStatus::Ok
    );

    // A Java query whose caller gives up at its deadline while the operation
    // is still running inside the pool (its 30 s delay never finishes).
    let mut wedged = dummy_request(LspLang::Java, repo.path());
    wedged.deadline = Instant::now() + Duration::from_millis(200);
    assert_eq!(pool.query(wedged).status, LiveStatus::Deadline);

    let started_at = Instant::now();
    drop(pool);
    let drop_ms = started_at.elapsed();

    assert!(
        drop_ms < Duration::from_secs(12),
        "shutdown waited for the wedged query: {drop_ms:?}"
    );
    let guard = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(guard.len(), 3, "one server per language");
    for (index, fake) in guard.iter().enumerate() {
        assert!(
            fake.shutdown_seen() || fake.exit_seen(),
            "server {index} was not shut down"
        );
    }
}

#[test]
fn views_follow_every_transition() {
    let repo = setup_repo();

    // not_started → starting → ready (a slow start makes `starting` visible).
    let pool = FakeServers::new(|_| FakeScript::full())
        .starting_for(Duration::from_millis(300))
        .pool();
    pool.warm(LspLang::Rust);
    let mut seen: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let state = pool.snapshot().slots["rust"].state.clone();
        if seen.last().map(String::as_str) != Some(state.as_str()) {
            seen.push(state);
        }
        if seen.last().map(String::as_str) == Some("ready") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(pool);
    assert_eq!(
        seen.last().map(String::as_str),
        Some("ready"),
        "states {seen:?}"
    );
    // `seen` must be a subsequence of the state machine's order.
    let order = ["not_started", "starting", "ready"];
    let mut at = 0;
    for state in &seen {
        while at < order.len() && order[at] != state.as_str() {
            at += 1;
        }
        assert!(at < order.len(), "out of order: {seen:?}");
        at += 1;
    }

    // missing (no server binary).
    let pool = FakeServers::missing().pool();
    assert_eq!(
        pool.query(dummy_request(LspLang::Rust, repo.path())).status,
        LiveStatus::NoServer
    );
    assert_eq!(pool.snapshot().slots["rust"].state, "missing");

    // failed → backoff → failed → … → disabled (the binary exists, the server
    // does not come up).
    let clock = Arc::new(TestClock::new());
    let factory = Arc::new(CountingAlwaysDegraded);
    let pool = LspPool::new_with_clock(
        factory,
        PoolSettings::default(),
        None,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .expect("pool");
    let req = || dummy_request(LspLang::Rust, repo.path());
    assert_eq!(pool.query(req()).status, LiveStatus::Failed);
    assert_eq!(pool.snapshot().slots["rust"].state, "failed");
    clock.advance(Duration::from_secs(6));
    assert_eq!(pool.query(req()).status, LiveStatus::Failed);
    clock.advance(Duration::from_secs(31));
    assert_eq!(pool.query(req()).status, LiveStatus::Failed);
    clock.advance(Duration::from_secs(121));
    assert_eq!(pool.query(req()).status, LiveStatus::Failed);
    assert_eq!(pool.snapshot().slots["rust"].state, "disabled");
}

/// A server that is installed but never comes up (3 attempts → disabled).
struct CountingAlwaysDegraded;

impl SessionFactory for CountingAlwaysDegraded {
    fn start(&self, lang: LspLang, _ctx: FactoryContext) -> BoxFuture<'static, StartOutcome> {
        Box::pin(async move {
            StartOutcome::Degraded(Degraded {
                lsp_available: true,
                lsp_started: false,
                diagnostic: format!("{} exited before initialize", lang.server_program()),
            })
        })
    }
}
