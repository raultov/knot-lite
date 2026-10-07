//! Cached query services (P2-D14).
//!
//! The MCP server keeps one [`QueryServices`] for its whole life: the loaded
//! index is reused until the manifest stamp changes, and the query embedder is
//! created once per model. The CLI builds a fresh `QueryServices` per
//! invocation. Poisoned mutexes recover the inner value; nothing here panics.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use knot::pipeline::embed::EmbedModelChoice;

use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};
use crate::config::Config;
use crate::embed::knot_embedder::KnotEmbedder;
use crate::embed::settings::EmbedSettings;
use crate::embed::{EmbedBatch, EmbeddingSpec};
use crate::error::IndexUnavailable;
use crate::index::encode::normalize;
use crate::index::layout::IndexLayout;
use crate::index::load::{LoadedIndex, load_index, read_stamp};
use crate::lsp::launch::start_pool;
use crate::lsp::pool::{LspPool, PoolSnapshot};

/// Builds a query embedder for a model name (may download the model).
pub type EmbedderFactory =
    Arc<dyn Fn(&str) -> anyhow::Result<Box<dyn EmbedBatch + Send>> + Send + Sync>;

/// The model name an embedder was created for, with the embedder.
type CachedEmbedder = (String, Box<dyn EmbedBatch + Send>);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The loaded index, reloaded only when the manifest stamp changes.
#[derive(Default)]
pub struct IndexCache {
    slot: Mutex<Option<Arc<LoadedIndex>>>,
    loads: AtomicUsize,
}

impl IndexCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current index. Reads the manifest stamp on every call (cheap) and
    /// reloads only when it differs from the cached one; an index that became
    /// unavailable is reported, never served stale.
    pub fn get(&self, layout: &IndexLayout) -> Result<Arc<LoadedIndex>, IndexUnavailable> {
        let stamp = read_stamp(layout)?;
        let mut slot = lock(&self.slot);
        if let Some(cached) = slot.as_ref()
            && cached.stamp == stamp
        {
            return Ok(Arc::clone(cached));
        }
        let loaded = Arc::new(load_index(layout)?);
        self.loads.fetch_add(1, Ordering::Relaxed);
        *slot = Some(Arc::clone(&loaded));
        Ok(loaded)
    }

    /// How many times the index was loaded from disk.
    pub fn loads(&self) -> usize {
        self.loads.load(Ordering::Relaxed)
    }
}

/// The query embedder, created once per model.
pub struct EmbedderCache {
    factory: EmbedderFactory,
    slot: Mutex<Option<CachedEmbedder>>,
    inits: AtomicUsize,
}

impl EmbedderCache {
    /// A cache creating embedders with `factory`.
    pub fn new(factory: EmbedderFactory) -> Self {
        Self {
            factory,
            slot: Mutex::new(None),
            inits: AtomicUsize::new(0),
        }
    }

    /// Embed `text` with the model recorded in `spec`, (re)creating the
    /// embedder when the model differs from the cached one. The vector is
    /// checked against `spec.dim` and returned **normalised**.
    pub fn embed_query(
        &self,
        spec: &EmbeddingSpec,
        text: &str,
    ) -> Result<Vec<f32>, IndexUnavailable> {
        let mut slot = lock(&self.slot);
        let cached = matches!(slot.as_ref(), Some((model, _)) if *model == spec.model);
        if !cached {
            let created = (self.factory)(&spec.model)
                .map_err(|error| model_unavailable(spec, &format!("{error:#}")))?;
            self.inits.fetch_add(1, Ordering::Relaxed);
            *slot = Some((spec.model.clone(), created));
        }
        let Some((_, embedder)) = slot.as_mut() else {
            return Err(model_unavailable(spec, "the embedder is not initialised"));
        };
        let mut vector = embedder
            .embed_query(text)
            .map_err(|error| model_unavailable(spec, &format!("{error:#}")))?;
        let expected = usize::try_from(spec.dim).unwrap_or(usize::MAX);
        if vector.len() != expected {
            return Err(model_unavailable(
                spec,
                &format!(
                    "the model produced a {}-component vector, the index expects {expected}",
                    vector.len()
                ),
            ));
        }
        normalize(&mut vector);
        Ok(vector)
    }

    /// How many times an embedder was created.
    pub fn inits(&self) -> usize {
        self.inits.load(Ordering::Relaxed)
    }
}

fn model_unavailable(spec: &EmbeddingSpec, error: &str) -> IndexUnavailable {
    IndexUnavailable::ModelUnavailable {
        model: spec.model.clone(),
        error: crate::index::manifest::one_line(error),
        accepted: EmbedModelChoice::accepted_names(),
    }
}

/// How long a CLI query waits for a starting language server: the longest
/// server ready timeout. The start gives up on its own at the server's timeout,
/// and every waiter is told, so this only needs to be no shorter.
pub const CLI_READY_WAIT: Duration = Duration::from_secs(900);

/// How long an MCP query waits for a starting language server (P3-D7).
pub const MCP_READY_WAIT: Duration = Duration::from_secs(20);

/// Everything a query needs besides the configuration.
pub struct QueryServices {
    /// The cached in-memory index.
    pub index: IndexCache,
    /// The cached query embedder.
    pub embedder: EmbedderCache,
    /// The process-wide language-server pool, created on first use.
    lsp: OnceLock<Option<Arc<LspPool>>>,
    /// Wait for a starting server when `KNOT_LITE_LSP_READY_WAIT_SECS` is unset.
    ready_wait: Duration,
}

impl QueryServices {
    /// Services backed by knot's real embedder (downloads the model on first
    /// use into `settings.cache_dir`).
    pub fn real(settings: &EmbedSettings) -> Self {
        let cache_dir = settings.cache_dir.clone();
        Self::with_factory(Arc::new(move |model: &str| {
            let embedder = KnotEmbedder::init(model, &cache_dir)?;
            Ok(Box::new(embedder) as Box<dyn EmbedBatch + Send>)
        }))
    }

    /// Services with a custom embedder factory (tests).
    pub fn with_factory(factory: EmbedderFactory) -> Self {
        Self {
            index: IndexCache::new(),
            embedder: EmbedderCache::new(factory),
            lsp: OnceLock::new(),
            ready_wait: CLI_READY_WAIT,
        }
    }

    /// The same services with another default wait for starting servers.
    pub fn with_ready_wait(mut self, ready_wait: Duration) -> Self {
        self.ready_wait = ready_wait;
        self
    }

    /// The current index. When it is a new version of the index, the
    /// language-server pool is told (its servers were started on the old one).
    pub fn get_index(&self, layout: &IndexLayout) -> Result<Arc<LoadedIndex>, IndexUnavailable> {
        let loaded = self.index.get(layout)?;
        if let Some(Some(pool)) = self.lsp.get() {
            pool.invalidate(loaded.stamp.clone());
        }
        Ok(loaded)
    }

    /// The language-server pool, created on first use; `None` when language
    /// servers are off (`KNOT_LITE_LSP=off`) or the pool cannot be created.
    /// Servers inside the pool start lazily.
    pub fn lsp_pool(&self, cfg: &Config, index: &LoadedIndex) -> Option<Arc<LspPool>> {
        if !cfg.lsp.enabled {
            return None;
        }
        self.lsp
            .get_or_init(|| self.create_pool(cfg, index))
            .clone()
    }

    fn create_pool(&self, cfg: &Config, index: &LoadedIndex) -> Option<Arc<LspPool>> {
        let files: Vec<String> = index.file_sha16.keys().cloned().collect();
        start_pool(cfg, &files, self.ready_wait, Some(index.stamp.clone())).map(Arc::new)
    }

    /// Start the servers of the languages the index has, ahead of the first
    /// query (`KNOT_LITE_LSP_WARMUP=on`, MCP only). Nothing happens when
    /// servers are off or there is no index yet. Returns without waiting.
    pub fn warm_up(&self, cfg: &Config) {
        if !cfg.lsp.enabled {
            return;
        }
        let layout = IndexLayout::for_repo(&cfg.repo_path);
        let Ok(index) = self.get_index(&layout) else {
            return;
        };
        let Some(pool) = self.lsp_pool(cfg, &index) else {
            return;
        };
        let languages: BTreeSet<LspLang> = index
            .by_file
            .keys()
            .filter_map(|rel| lsp_lang_for_label(label_for(Path::new(rel))))
            .collect();
        for lang in languages {
            pool.warm(lang);
        }
    }

    /// The same services over a ready-made pool (tests inject a fake one).
    #[cfg(test)]
    pub(crate) fn with_lsp_pool(self, pool: LspPool) -> Self {
        let _ = self.lsp.set(Some(Arc::new(pool)));
        self
    }

    /// The state of every language server, when a pool exists.
    pub fn lsp_runtime(&self) -> Option<PoolSnapshot> {
        self.lsp.get()?.as_ref().map(|pool| pool.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::fake::{FAKE_MODEL, FakeEmbedder};
    use crate::index::build::test_support::{DIM, fixture, opts, run, run_with};

    fn spec(model: &str, dim: u32) -> EmbeddingSpec {
        EmbeddingSpec {
            model: model.to_owned(),
            dim,
            encoding: "i8pv_b64".to_owned(),
            query_prefix: String::new(),
            passage_prefix: String::new(),
        }
    }

    /// A factory creating fake embedders of `dim` components, counting calls.
    fn counting_factory(dim: usize) -> (EmbedderFactory, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let factory: EmbedderFactory = Arc::new(move |_model: &str| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(FakeEmbedder::new(dim)) as Box<dyn EmbedBatch + Send>)
        });
        (factory, calls)
    }

    #[test]
    fn index_cache_reuses_until_stamp_changes() {
        let (tmp, cfg) = fixture();
        let layout = IndexLayout::for_repo(tmp.path());
        let cache = IndexCache::new();
        assert!(matches!(
            cache.get(&layout),
            Err(IndexUnavailable::Absent { .. })
        ));
        assert_eq!(cache.loads(), 0);

        run(&cfg);
        let first = cache.get(&layout).expect("load");
        let second = cache.get(&layout).expect("cached");
        assert_eq!(cache.loads(), 1);
        assert!(Arc::ptr_eq(&first, &second));

        // A source change alters the fingerprint, hence the stamp.
        std::fs::write(tmp.path().join("src/extra.rs"), "pub fn extra() {}\n").expect("write");
        run_with(&cfg, &opts());
        let third = cache.get(&layout).expect("reloaded");
        assert_eq!(cache.loads(), 2);
        assert!(!Arc::ptr_eq(&first, &third));
        assert!(third.by_name.contains_key("extra"));
    }

    #[test]
    fn index_cache_reports_an_index_that_disappears() {
        let (tmp, cfg) = fixture();
        let layout = IndexLayout::for_repo(tmp.path());
        run(&cfg);
        let cache = IndexCache::new();
        cache.get(&layout).expect("load");
        std::fs::remove_dir_all(&layout.dir).expect("remove index");
        assert!(matches!(
            cache.get(&layout),
            Err(IndexUnavailable::Absent { .. })
        ));
    }

    #[test]
    fn embedder_cache_creates_once_per_model() {
        let (factory, calls) = counting_factory(DIM);
        let cache = EmbedderCache::new(factory);
        let dim = u32::try_from(DIM).expect("fits");
        cache
            .embed_query(&spec(FAKE_MODEL, dim), "a")
            .expect("first");
        cache
            .embed_query(&spec(FAKE_MODEL, dim), "b")
            .expect("second");
        assert_eq!((cache.inits(), calls.load(Ordering::Relaxed)), (1, 1));

        cache
            .embed_query(&spec("another-model", dim), "c")
            .expect("other model");
        assert_eq!((cache.inits(), calls.load(Ordering::Relaxed)), (2, 2));
    }

    #[test]
    fn embedder_cache_maps_factory_error_to_model_unavailable() {
        let factory: EmbedderFactory = Arc::new(|_model: &str| anyhow::bail!("no network"));
        let cache = EmbedderCache::new(factory);
        match cache.embed_query(&spec("AllMiniLML6V2", 16), "q") {
            Err(IndexUnavailable::ModelUnavailable {
                model,
                error,
                accepted,
            }) => {
                assert_eq!(model, "AllMiniLML6V2");
                assert!(error.contains("no network"), "{error}");
                assert!(accepted.contains("BGEBaseENV15"), "{accepted}");
            }
            other => panic!("expected ModelUnavailable, got {other:?}"),
        }
        assert_eq!(cache.inits(), 0, "a failed creation is not counted");
    }

    #[test]
    fn embedder_cache_rejects_dim_mismatch() {
        let (factory, _) = counting_factory(8);
        let cache = EmbedderCache::new(factory);
        match cache.embed_query(&spec(FAKE_MODEL, 16), "q") {
            Err(IndexUnavailable::ModelUnavailable { error, .. }) => {
                assert!(
                    error.contains("8-component") && error.contains("expects 16"),
                    "{error}"
                );
            }
            other => panic!("expected ModelUnavailable, got {other:?}"),
        }
    }

    /// Returns an unnormalised 2-component vector.
    struct Unnormalised;

    impl EmbedBatch for Unnormalised {
        fn spec(&self) -> EmbeddingSpec {
            spec("unnormalised", 2)
        }
        fn embed_passages(
            &mut self,
            texts: &[String],
            _batch: usize,
        ) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![3.0, 4.0]).collect())
        }
        fn embed_query(&mut self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![3.0, 4.0])
        }
    }

    #[test]
    fn embed_query_returns_a_normalised_vector() {
        let factory: EmbedderFactory =
            Arc::new(|_model: &str| Ok(Box::new(Unnormalised) as Box<dyn EmbedBatch + Send>));
        let vector = EmbedderCache::new(factory)
            .embed_query(&spec("unnormalised", 2), "q")
            .expect("embed");
        assert!(
            (vector[0] - 0.6).abs() < 1e-6 && (vector[1] - 0.8).abs() < 1e-6,
            "{vector:?}"
        );
    }

    #[test]
    fn poisoned_mutexes_recover_instead_of_panicking() {
        let (factory, _) = counting_factory(DIM);
        let services = Arc::new(QueryServices::with_factory(factory));
        let poisoner = Arc::clone(&services);
        let outcome = std::thread::spawn(move || {
            let _index = poisoner.index.slot.lock().expect("lock");
            let _embedder = poisoner.embedder.slot.lock().expect("lock");
            panic!("poison both mutexes");
        })
        .join();
        assert!(outcome.is_err());

        let dim = u32::try_from(DIM).expect("fits");
        assert!(
            services
                .embedder
                .embed_query(&spec(FAKE_MODEL, dim), "q")
                .is_ok()
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        assert!(matches!(
            services.index.get(&layout),
            Err(IndexUnavailable::Absent { .. })
        ));
    }

    #[test]
    fn services_constructors_start_empty() {
        let (factory, _) = counting_factory(DIM);
        let services = QueryServices::with_factory(factory);
        assert_eq!((services.index.loads(), services.embedder.inits()), (0, 0));
    }
    #[test]
    fn lsp_pool_is_absent_when_servers_are_off() {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let services = QueryServices::with_factory(counting_factory(DIM).0);
        let index = services
            .get_index(&IndexLayout::for_repo(tmp.path()))
            .expect("index");
        assert!(!cfg.lsp.enabled, "test configs never start servers");
        assert!(services.lsp_pool(&cfg, &index).is_none());
    }

    #[test]
    fn lsp_pool_is_created_once_and_shared() {
        let (tmp, mut cfg) = fixture();
        run(&cfg);
        // Creating the pool starts no server: servers start on the first query.
        cfg.lsp.enabled = true;
        let services = QueryServices::with_factory(counting_factory(DIM).0);
        let layout = IndexLayout::for_repo(tmp.path());
        let index = services.get_index(&layout).expect("index");

        let first = services.lsp_pool(&cfg, &index).expect("pool");
        let second = services.lsp_pool(&cfg, &index).expect("pool");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(
            first
                .snapshot()
                .slots
                .values()
                .all(|slot| slot.state == "not_started")
        );
        // Asking for the index again tells the pool about its stamp and still works.
        assert!(services.get_index(&layout).is_ok());
    }
    #[test]
    fn warm_up_starts_the_servers_of_the_indexed_languages_only() {
        let (tmp, mut cfg) = fixture();
        run(&cfg);
        cfg.lsp.enabled = true;
        let servers = crate::lsp::fake::FakeServers::new(|_| crate::lsp::fake::FakeScript::full());
        let starts = servers.starts();
        let services =
            QueryServices::with_factory(counting_factory(DIM).0).with_lsp_pool(servers.pool());

        services.warm_up(&cfg);

        // The fixture has Rust and TypeScript sources and no Java: warming is
        // asynchronous, so wait for both starts.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while starts.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        let states: Vec<(String, String)> = services
            .lsp_runtime()
            .expect("pool")
            .slots
            .into_iter()
            .map(|(language, slot)| (language, slot.state))
            .collect();
        assert_eq!(
            states,
            vec![
                ("java".to_owned(), "not_started".to_owned()),
                ("rust".to_owned(), "ready".to_owned()),
                ("ts".to_owned(), "ready".to_owned()),
            ]
        );
        let _ = tmp;
    }

    #[test]
    fn warm_up_does_nothing_without_servers_or_without_an_index() {
        let (tmp, cfg) = fixture();
        run(&cfg);
        // Servers off: nothing starts and no pool is created.
        let services = QueryServices::with_factory(counting_factory(DIM).0);
        services.warm_up(&cfg);
        assert!(services.lsp_runtime().is_none());

        // Servers on but no index: nothing to warm either.
        let empty = tempfile::tempdir().expect("tempdir");
        let mut cfg = crate::config::test_support::config_for(empty.path().to_path_buf());
        cfg.lsp.enabled = true;
        let services = QueryServices::with_factory(counting_factory(DIM).0);
        services.warm_up(&cfg);
        assert!(services.lsp_runtime().is_none());
        let _ = tmp;
    }
}
