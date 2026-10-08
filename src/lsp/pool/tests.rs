//! Pool tests over the scripted in-process fake servers (no real server).

use std::path::Path;

use super::*;
use crate::lsp::fake::{FakeScript, connect, test_spec};
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
