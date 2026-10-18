//! One target, every operation: the language-server half of `find_callers`
//! (§6.4 steps 2–6 of the Phase 3 plan).
//!
//! [`run_target`] opens the target's document, asks the server the operations of
//! the target's kind (incoming calls, references, implementations), normalises
//! the locations and hands back raw in-repository locations. It never sees the
//! index (see `lsp::normalize`) and it never answers *partially*: a deadline or a
//! timeout discards everything gathered so far, because a union with a missing
//! operation would silently under-report — the caller falls back to the
//! heuristic edges instead.
//!
//! The same job asks for a document's symbols when [`OpSet::document_symbol`]
//! is set (`explore_file`'s outline, P5.11); the position is then unused.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_lsp::lsp_types::{
    CallHierarchyItem, CallHierarchyServerCapability, ImplementationProviderCapability, OneOf,
    Position, Range, ServerCapabilities, Url,
};
use serde::{Deserialize, Serialize};

use super::normalize::{LocationFilter, Normalized, RawHit, RawLocation, normalize};
use super::ops::{Op, OpSet};
use super::outline::{OutlineSymbol, flatten_outline};
use super::session::{LspSession, RequestError, RequestOutcome};
use super::uri::{join_rel, path_to_uri};
use crate::capability::languages::LspLang;

/// How long the first attempt's emptiness is given to be a lag artefact before
/// the one retry (a server that is still indexing answers empty).
pub const DEFAULT_RETRY_PAUSE: Duration = Duration::from_secs(2);

/// One target to ask the server about.
#[derive(Debug, Clone)]
pub struct LiveRequest {
    /// The server's language.
    pub lang: LspLang,
    /// Record id of the target (for logs and the pre-computed edges).
    pub target_id: String,
    /// Absolute path of the target's file.
    pub abs_file: PathBuf,
    /// Repo-relative path of the target's file.
    pub rel_file: String,
    /// LSP position of the target's name (line from `locate_name` on the
    /// current file, column from the index).
    pub position: Position,
    /// The operations to run.
    pub ops: OpSet,
    /// How many caller containers the heuristic found (decides retry-on-empty).
    pub heuristic_hint: usize,
    /// When the caller stops waiting; nothing started after it is answered.
    pub deadline: Instant,
    /// Pause before the retry-on-empty (tests shorten it).
    pub retry_pause: Duration,
    /// Repository files changed since the index (hint for edit sync).
    pub changed_files: std::sync::Arc<Vec<String>>,
}

/// How a live query ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveStatus {
    /// The union is complete.
    Ok,
    /// The target cannot be positioned (unreadable file, bad path).
    NoPosition,
    /// The server is still starting.
    NotReady,
    /// There is no server for this language.
    NoServer,
    /// The server is dead or its connection broke.
    Failed,
    /// A request outlived its own timeout.
    Timeout,
    /// The call's deadline expired.
    Deadline,
    /// The server answered an operation with an error.
    Error,
}

impl LiveStatus {
    /// The wire word used in `lsp.status` (§10.1).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NoPosition => "no_position",
            Self::NotReady => "not_ready",
            Self::NoServer => "no_server",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Deadline => "deadline",
            Self::Error => "error",
        }
    }
}

/// In-repository locations per operation; `None` when the operation did not run
/// (not applicable to the kind, or the server lacks the capability).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpCounts {
    /// `incomingCalls` locations.
    pub incoming: Option<usize>,
    /// `references` locations.
    pub references: Option<usize>,
    /// `implementation` locations.
    pub implementation: Option<usize>,
}

impl OpCounts {
    fn set(&mut self, op: Op, count: usize) {
        let slot = match op {
            Op::Incoming => &mut self.incoming,
            Op::References => &mut self.references,
            Op::Implementation => &mut self.implementation,
        };
        *slot = Some(count);
    }
}

/// What a live query produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOutcome {
    /// How it ended.
    pub status: LiveStatus,
    /// Why, when it did not end with [`LiveStatus::Ok`].
    pub detail: Option<String>,
    /// In-repository locations; empty unless the status is `Ok`.
    pub locations: Vec<RawLocation>,
    /// Locations per operation.
    pub op_counts: OpCounts,
    /// Locations outside the repository that were dropped.
    pub external: usize,
    /// Java references that only sit in a comment and were dropped.
    pub comment_filtered: usize,
    /// Whether the empty first answer was asked again.
    pub retried_empty: bool,
    /// Wall-clock milliseconds the query took.
    pub ms: u64,
    /// The document's symbols, when they were asked for and the server
    /// answers `documentSymbol`; `None` otherwise.
    pub symbols: Option<Vec<OutlineSymbol>>,
    /// Number of files synced with the server before this query.
    pub sync_synced: usize,
    /// Number of changed files left for subsequent queries.
    pub sync_left: usize,
}

impl LiveOutcome {
    /// An outcome that carries no locations.
    pub fn without_answer(status: LiveStatus, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: Some(detail.into()),
            locations: Vec::new(),
            op_counts: OpCounts::default(),
            external: 0,
            comment_filtered: 0,
            retried_empty: false,
            ms: 0,
            symbols: None,
            sync_synced: 0,
            sync_left: 0,
        }
    }
}

/// Why a query gave up.
enum Failure {
    NoPosition(String),
    Failed(String),
    Timeout(String),
    Deadline,
    Error(String),
}

/// A location as the server returned it, before normalisation.
struct Found {
    uri: Url,
    range: Range,
    sites: u32,
}

/// Everything one pass over the operations gathered.
#[derive(Default)]
struct Collected {
    locations: Vec<RawLocation>,
    counts: OpCounts,
    external: usize,
    comment_filtered: usize,
    symbols: Option<Vec<OutlineSymbol>>,
}

impl Collected {
    /// Normalise what `op` returned and count it.
    fn absorb(
        &mut self,
        op: Op,
        found: Vec<Found>,
        filter: &LocationFilter<'_>,
        sources: &SourceCache<'_>,
    ) {
        let mut in_repo = 0;
        for location in found {
            let hit = RawHit {
                op,
                uri: &location.uri,
                range: &location.range,
                sites: location.sites,
            };
            match normalize(&hit, filter, &|rel, line| sources.line(rel, line)) {
                Normalized::External => self.external += 1,
                Normalized::Comment => self.comment_filtered += 1,
                Normalized::InRepo(raw) => {
                    in_repo += 1;
                    self.locations.push(raw);
                }
            }
        }
        self.counts.set(op, in_repo);
    }
}

/// Current source lines, read on first use (only Java's comment filter asks).
struct SourceCache<'a> {
    root: &'a Path,
    files: Mutex<HashMap<String, Option<Vec<String>>>>,
}

impl<'a> SourceCache<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: Mutex::new(HashMap::new()),
        }
    }

    fn line(&self, rel: &str, line0: u32) -> Option<String> {
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lines = files.entry(rel.to_owned()).or_insert_with(|| {
            std::fs::read_to_string(join_rel(self.root, rel))
                .ok()
                .map(|text| text.lines().map(str::to_owned).collect())
        });
        lines.as_ref()?.get(usize::try_from(line0).ok()?).cloned()
    }
}

/// Ask the server everything about one target.
///
/// Steps: open the document; run each operation of `req.ops` the server
/// supports, in the order incoming → references → implementation, each within
/// the call's deadline; normalise; and, when everything came back empty although
/// the heuristic found callers and the server is not known to be idle, wait and
/// ask once more (a server that is still indexing answers empty).
pub async fn run_target(
    session: &mut LspSession,
    req: &LiveRequest,
    filter: &LocationFilter<'_>,
) -> LiveOutcome {
    let started = Instant::now();
    let sources = SourceCache::new(filter.root.raw());
    let mut retried_empty = false;
    let result = match open_target(session, req) {
        Ok(uri) => {
            let first = collect(session, req, &uri, filter, &sources).await;
            match first {
                Ok(collected) if wants_retry(session, req, &collected) => {
                    match pause_before_retry(req).await {
                        true => {
                            retried_empty = true;
                            collect(session, req, &uri, filter, &sources).await
                        }
                        false => Ok(collected),
                    }
                }
                other => other,
            }
        }
        Err(failure) => Err(failure),
    };
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    finish(result, retried_empty, ms)
}

fn finish(result: Result<Collected, Failure>, retried_empty: bool, ms: u64) -> LiveOutcome {
    let outcome = match result {
        Ok(collected) => LiveOutcome {
            status: LiveStatus::Ok,
            detail: None,
            locations: collected.locations,
            op_counts: collected.counts,
            external: collected.external,
            comment_filtered: collected.comment_filtered,
            retried_empty,
            ms: 0,
            symbols: collected.symbols,
            sync_synced: 0,
            sync_left: 0,
        },
        Err(Failure::NoPosition(detail)) => {
            LiveOutcome::without_answer(LiveStatus::NoPosition, detail)
        }
        Err(Failure::Failed(detail)) => LiveOutcome::without_answer(LiveStatus::Failed, detail),
        Err(Failure::Timeout(detail)) => LiveOutcome::without_answer(LiveStatus::Timeout, detail),
        Err(Failure::Deadline) => {
            LiveOutcome::without_answer(LiveStatus::Deadline, "the call's deadline expired")
        }
        Err(Failure::Error(detail)) => LiveOutcome::without_answer(LiveStatus::Error, detail),
    };
    LiveOutcome {
        retried_empty,
        ms,
        ..outcome
    }
}

/// Check the server is running, open the target's document and return its URI.
fn open_target(session: &mut LspSession, req: &LiveRequest) -> Result<Url, Failure> {
    if !session.is_alive() {
        return Err(Failure::Failed(
            "the language server is not running".to_owned(),
        ));
    }
    let uri =
        path_to_uri(&req.abs_file).map_err(|error| Failure::NoPosition(format!("{error:#}")))?;
    if let Err(error) = session.did_open(&req.abs_file) {
        if !session.is_alive() {
            return Err(Failure::Failed(format!("didOpen failed: {error:#}")));
        }
        return Err(Failure::NoPosition(format!("{error:#}")));
    }
    Ok(uri)
}

/// Whether an empty answer should be asked again: nothing came back although
/// the heuristic found callers, and the server is not known to be idle (a
/// rust-analyzer that reported `quiescent` has nothing left to index).
fn wants_retry(session: &LspSession, req: &LiveRequest, collected: &Collected) -> bool {
    let quiescent = session.events.borrow().quiescent_known() == Some(true);
    collected.locations.is_empty() && req.heuristic_hint >= 1 && !quiescent
}

/// Sleep before the retry, but only when the deadline leaves room to ask again
/// afterwards: a pause that eats the whole budget would turn a valid (empty)
/// answer into a deadline failure. `false` means "do not retry".
async fn pause_before_retry(req: &LiveRequest) -> bool {
    let remaining = req.deadline.saturating_duration_since(Instant::now());
    if remaining <= req.retry_pause {
        return false;
    }
    tokio::time::sleep(req.retry_pause).await;
    true
}

/// One pass over the operations the target's kind and the server allow.
async fn collect(
    session: &mut LspSession,
    req: &LiveRequest,
    uri: &Url,
    filter: &LocationFilter<'_>,
    sources: &SourceCache<'_>,
) -> Result<Collected, Failure> {
    let mut collected = Collected::default();
    for op in [Op::Incoming, Op::References, Op::Implementation] {
        if !req.ops.contains(op) || !supports(session.capabilities(), op) {
            continue;
        }
        let found = match op {
            Op::Incoming => incoming(session, req, uri).await?,
            Op::References => references(session, req, uri).await?,
            Op::Implementation => implementation(session, req, uri).await?,
        };
        collected.absorb(op, found, filter, sources);
    }
    if req.ops.document_symbol && supports_document_symbol(session.capabilities()) {
        collected.symbols = Some(document_symbols(session, req, uri).await?);
    }
    Ok(collected)
}

/// Whether the server advertises `documentSymbol`.
fn supports_document_symbol(capabilities: &ServerCapabilities) -> bool {
    matches!(
        capabilities.document_symbol_provider,
        Some(OneOf::Left(true) | OneOf::Right(_))
    )
}

/// `textDocument/documentSymbol` of the target's document, flattened.
async fn document_symbols(
    session: &mut LspSession,
    req: &LiveRequest,
    uri: &Url,
) -> Result<Vec<OutlineSymbol>, Failure> {
    let per_try = budget(session, req.deadline)?;
    let answer = settle(session.document_symbol(uri, per_try).await, req.deadline)?;
    Ok(answer.map(flatten_outline).unwrap_or_default())
}

/// Whether the server advertises `op` (absent or `false` means no).
fn supports(capabilities: &ServerCapabilities, op: Op) -> bool {
    match op {
        Op::Incoming => matches!(
            capabilities.call_hierarchy_provider,
            Some(
                CallHierarchyServerCapability::Simple(true)
                    | CallHierarchyServerCapability::Options(_)
            )
        ),
        Op::References => matches!(
            capabilities.references_provider,
            Some(OneOf::Left(true) | OneOf::Right(_))
        ),
        Op::Implementation => matches!(
            capabilities.implementation_provider,
            Some(
                ImplementationProviderCapability::Simple(true)
                    | ImplementationProviderCapability::Options(_)
            )
        ),
    }
}

/// `prepareCallHierarchy` at the target, then `incomingCalls` of the item that
/// contains the position (else the first). Each caller is located at its own
/// declaration, as in Phase 0.
async fn incoming(
    session: &mut LspSession,
    req: &LiveRequest,
    uri: &Url,
) -> Result<Vec<Found>, Failure> {
    let per_try = budget(session, req.deadline)?;
    let prepared = session
        .prepare_call_hierarchy(uri, req.position, per_try)
        .await;
    let items = settle(prepared, req.deadline)?.unwrap_or_default();
    let Some(item) = pick_item(items, req.position) else {
        return Ok(Vec::new());
    };
    let per_try = budget(session, req.deadline)?;
    let calls = settle(session.incoming_calls(item, per_try).await, req.deadline)?;
    Ok(calls
        .unwrap_or_default()
        .into_iter()
        .map(|call| Found {
            uri: call.from.uri,
            range: call.from.selection_range,
            sites: u32::try_from(call.from_ranges.len()).map_or(u32::MAX, |n| n.max(1)),
        })
        .collect())
}

async fn references(
    session: &mut LspSession,
    req: &LiveRequest,
    uri: &Url,
) -> Result<Vec<Found>, Failure> {
    let per_try = budget(session, req.deadline)?;
    let outcome = session.references(uri, req.position, false, per_try).await;
    let locations = settle(outcome, req.deadline)?.unwrap_or_default();
    Ok(locations
        .into_iter()
        .map(|location| Found {
            uri: location.uri,
            range: location.range,
            sites: 1,
        })
        .collect())
}

async fn implementation(
    session: &mut LspSession,
    req: &LiveRequest,
    uri: &Url,
) -> Result<Vec<Found>, Failure> {
    let per_try = budget(session, req.deadline)?;
    let outcome = session.implementation(uri, req.position, per_try).await;
    let locations = settle(outcome, req.deadline)?;
    Ok(locations
        .into_iter()
        .map(|location| Found {
            uri: location.uri,
            range: location.range,
            sites: 1,
        })
        .collect())
}

/// The call-hierarchy item whose selection range contains `position`, else the first.
fn pick_item(items: Vec<CallHierarchyItem>, position: Position) -> Option<CallHierarchyItem> {
    let index = items
        .iter()
        .position(|item| {
            item.selection_range.start <= position && position <= item.selection_range.end
        })
        .unwrap_or(0);
    items.into_iter().nth(index)
}

/// Timeout of the next request: the session's (longer for the very first one),
/// cut to what the deadline leaves.
fn budget(session: &mut LspSession, deadline: Instant) -> Result<Duration, Failure> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Failure::Deadline);
    }
    Ok(remaining.min(session.next_timeout()))
}

/// Turn a request outcome into its answer or the reason there is none.
fn settle<T>(outcome: RequestOutcome<T>, deadline: Instant) -> Result<T, Failure> {
    outcome.result.map_err(|error| match error {
        RequestError::Timeout(after) if Instant::now() >= deadline => {
            tracing::debug!("deadline reached while waiting {after:?}");
            Failure::Deadline
        }
        RequestError::Timeout(after) => Failure::Timeout(format!("no answer within {after:?}")),
        RequestError::Response { code, message } => {
            Failure::Error(format!("the server answered error {code}: {message}"))
        }
        RequestError::Transport(detail) => Failure::Failed(detail),
    })
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::{CallHierarchyIncomingCall, Location, SymbolKind};

    use super::*;
    use crate::lsp::fake::{
        FakeHandle, FakeNotification, FakeScript, connect, eventually, test_spec,
    };
    use crate::lsp::ops::{TargetShape, ops_for};
    use crate::lsp::session::InitInputs;
    use crate::lsp::spec::ServerSpec;
    use crate::lsp::uri::CanonicalRoot;

    /// The target is at (2, 7) of `src/lib.rs`.
    const TARGET: Position = Position {
        line: 2,
        character: 7,
    };

    /// A repository on disk: the scripts of the fake server point into it, so it
    /// exists before the server does.
    struct Repo {
        dir: tempfile::TempDir,
    }

    impl Repo {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
            std::fs::write(
                dir.path().join("src").join("lib.rs"),
                "// header\n\npub fn target() {}\n",
            )
            .expect("write");
            Self { dir }
        }

        fn write(&self, rel: &str, text: &str) {
            std::fs::write(join_rel(self.dir.path(), rel), text).expect("write");
        }

        fn uri(&self, rel: &str) -> Url {
            path_to_uri(&join_rel(self.dir.path(), rel)).expect("uri")
        }

        fn location(&self, rel: &str, line: u32) -> Location {
            Location {
                uri: self.uri(rel),
                range: Range::new(Position::new(line, 4), Position::new(line, 10)),
            }
        }

        fn item(&self, name: &str, rel: &str, line: u32) -> CallHierarchyItem {
            CallHierarchyItem {
                name: name.to_owned(),
                kind: SymbolKind::FUNCTION,
                tags: None,
                detail: None,
                uri: self.uri(rel),
                range: Range::new(Position::new(line, 0), Position::new(line + 3, 1)),
                selection_range: Range::new(Position::new(line, 7), Position::new(line, 13)),
                data: None,
            }
        }

        /// The call-hierarchy item at [`TARGET`].
        fn target_item(&self) -> CallHierarchyItem {
            self.item("target", "src/lib.rs", 2)
        }

        fn call(&self, name: &str, rel: &str, ranges: usize) -> CallHierarchyIncomingCall {
            CallHierarchyIncomingCall {
                from: self.item(name, rel, 5),
                from_ranges: vec![Range::default(); ranges],
            }
        }

        /// A server that answers `prepareCallHierarchy` at the target with its item
        /// and `incomingCalls` with `calls`.
        fn with_incoming(&self, calls: Vec<CallHierarchyIncomingCall>) -> FakeScript {
            FakeScript {
                prepare: vec![(TARGET, vec![self.target_item()])],
                incoming: vec![("target".to_owned(), calls)],
                ..FakeScript::full()
            }
        }

        /// A server whose `references` answer at the target is `locations`.
        fn with_references(&self, locations: Vec<Location>) -> FakeScript {
            FakeScript {
                references: vec![(TARGET, locations)],
                ..FakeScript::full()
            }
        }
    }

    /// A launch spec with generous request timeouts.
    fn spec(lang: LspLang) -> ServerSpec {
        let mut spec = test_spec(lang);
        spec.first_request_timeout = Duration::from_secs(5);
        spec.request_timeout = Duration::from_secs(5);
        spec
    }

    /// A launch spec whose requests give up after 80 ms.
    fn slow_spec(lang: LspLang) -> ServerSpec {
        let mut spec = test_spec(lang);
        spec.first_request_timeout = Duration::from_millis(80);
        spec.request_timeout = Duration::from_millis(80);
        spec
    }

    /// A repository, a connected fake server and everything `run_target` needs.
    struct Rig {
        repo: Repo,
        root: CanonicalRoot,
        session: LspSession,
        handle: FakeHandle,
        lang: LspLang,
    }

    impl Rig {
        async fn connect(repo: Repo, spec: ServerSpec, script: FakeScript) -> Self {
            let lang = spec.lang;
            let (mut session, handle) = connect(
                script,
                InitInputs {
                    spec: &spec,
                    repo_root: repo.dir.path(),
                },
            )
            .await;
            session.retry.backoff = Duration::from_millis(1);
            let root = CanonicalRoot::new(repo.dir.path());
            Self {
                repo,
                root,
                session,
                handle,
                lang,
            }
        }

        /// A Rust server over a fresh repository.
        async fn rust(script: impl FnOnce(&Repo) -> FakeScript) -> Self {
            Self::lang(LspLang::Rust, script).await
        }

        async fn lang(lang: LspLang, script: impl FnOnce(&Repo) -> FakeScript) -> Self {
            let repo = Repo::new();
            let script = script(&repo);
            Self::connect(repo, spec(lang), script).await
        }

        fn request(&self, shape: TargetShape) -> LiveRequest {
            LiveRequest {
                lang: self.lang,
                target_id: "target-id".to_owned(),
                abs_file: self.repo.dir.path().join("src").join("lib.rs"),
                rel_file: "src/lib.rs".to_owned(),
                position: TARGET,
                ops: ops_for(shape),
                heuristic_hint: 0,
                deadline: Instant::now() + Duration::from_secs(30),
                retry_pause: Duration::from_millis(10),
                changed_files: std::sync::Arc::new(Vec::new()),
            }
        }

        async fn run(&mut self, req: &LiveRequest) -> LiveOutcome {
            let filter = LocationFilter {
                root: &self.root,
                lang: self.lang,
            };
            run_target(&mut self.session, req, &filter).await
        }

        fn methods(&self) -> Vec<String> {
            self.handle.counters.requests()
        }

        fn requests_of(&self, method: &str) -> usize {
            self.handle.counters.requests_of(method)
        }
    }

    fn counts(
        incoming: Option<usize>,
        references: Option<usize>,
        implementation: Option<usize>,
    ) -> OpCounts {
        OpCounts {
            incoming,
            references,
            implementation,
        }
    }

    #[tokio::test]
    async fn free_function_runs_incoming_and_references() {
        let mut rig = Rig::rust(|repo| FakeScript {
            references: vec![(
                TARGET,
                vec![repo.location("src/a.rs", 11), repo.location("src/b.rs", 4)],
            )],
            ..repo.with_incoming(vec![repo.call("caller", "src/a.rs", 2)])
        })
        .await;
        let outcome = rig.run(&rig.request(TargetShape::Callable)).await;
        assert_eq!(outcome.status, LiveStatus::Ok, "{:?}", outcome.detail);
        assert_eq!(
            rig.methods(),
            vec![
                "textDocument/prepareCallHierarchy",
                "callHierarchy/incomingCalls",
                "textDocument/references"
            ]
        );
        assert_eq!(outcome.op_counts, counts(Some(1), Some(2), None));
        assert_eq!(outcome.locations.len(), 3);
        assert!(!outcome.retried_empty);
    }

    #[tokio::test]
    async fn references_only_sends_one_request() {
        let mut rig = Rig::rust(|repo| {
            repo.with_references(vec![
                repo.location("src/a.rs", 3),
                repo.location("src/b.rs", 9),
            ])
        })
        .await;
        let mut req = rig.request(TargetShape::Method);
        req.ops = OpSet::references_only();
        let outcome = rig.run(&req).await;
        assert_eq!(outcome.status, LiveStatus::Ok);
        assert_eq!(rig.methods(), vec!["textDocument/references"]);
        assert_eq!(outcome.op_counts, counts(None, Some(2), None));
        assert_eq!(outcome.locations.len(), 2);
        assert_eq!(outcome.symbols, None);
    }

    #[tokio::test]
    async fn document_symbol_ignores_position() {
        use crate::lsp::outline::test_support::symbol;
        use async_lsp::lsp_types::{DocumentSymbolResponse, SymbolKind};

        let mut rig = Rig::rust(|_| FakeScript {
            symbols: vec![(
                "src/lib.rs".to_owned(),
                DocumentSymbolResponse::Nested(vec![symbol(
                    "target",
                    SymbolKind::FUNCTION,
                    (2, 2),
                    vec![],
                )]),
            )],
            ..FakeScript::full()
        })
        .await;
        let mut req = rig.request(TargetShape::None);
        req.ops = OpSet::outline();
        req.position = Position::new(999, 999);
        let outcome = rig.run(&req).await;
        assert_eq!(outcome.status, LiveStatus::Ok, "{:?}", outcome.detail);
        assert_eq!(rig.methods(), vec!["textDocument/documentSymbol"]);
        let symbols = outcome.symbols.expect("symbols");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "target");
        assert!(outcome.locations.is_empty());
    }

    #[tokio::test]
    async fn document_symbol_without_capability_is_none() {
        let mut rig = Rig::rust(|_| FakeScript {
            capabilities: ServerCapabilities::default(),
            ..FakeScript::default()
        })
        .await;
        let mut req = rig.request(TargetShape::None);
        req.ops = OpSet::outline();
        let outcome = rig.run(&req).await;
        assert_eq!(outcome.status, LiveStatus::Ok);
        assert!(rig.methods().is_empty());
        assert_eq!(outcome.symbols, None);
    }

    #[tokio::test]
    async fn type_runs_references_only() {
        let mut rig = Rig::rust(|_| FakeScript::full()).await;
        let outcome = rig.run(&rig.request(TargetShape::TypeLike)).await;
        assert_eq!(outcome.status, LiveStatus::Ok);
        assert_eq!(rig.methods(), vec!["textDocument/references"]);
        assert_eq!(outcome.op_counts, counts(None, Some(0), None));
    }

    #[tokio::test]
    async fn trait_runs_references_and_implementation() {
        let mut rig = Rig::rust(|_| FakeScript::full()).await;
        let outcome = rig.run(&rig.request(TargetShape::InterfaceLike)).await;
        assert_eq!(outcome.status, LiveStatus::Ok);
        assert_eq!(
            rig.methods(),
            vec!["textDocument/references", "textDocument/implementation"]
        );
        assert_eq!(outcome.op_counts, counts(None, Some(0), Some(0)));
    }

    #[tokio::test]
    async fn missing_capability_skips_op() {
        let mut rig = Rig::rust(|_| FakeScript {
            capabilities: ServerCapabilities {
                references_provider: Some(OneOf::Left(true)),
                // Advertised but switched off: counts as absent.
                implementation_provider: Some(ImplementationProviderCapability::Simple(false)),
                ..ServerCapabilities::default()
            },
            ..FakeScript::default()
        })
        .await;
        let outcome = rig.run(&rig.request(TargetShape::Method)).await;
        assert_eq!(outcome.status, LiveStatus::Ok);
        assert_eq!(rig.methods(), vec!["textDocument/references"]);
        assert_eq!(outcome.op_counts, counts(None, Some(0), None));
    }

    #[tokio::test]
    async fn prepare_picks_item_containing_position() {
        let mut rig = Rig::rust(|repo| FakeScript {
            // The item that does not contain the position comes first.
            prepare: vec![(
                TARGET,
                vec![repo.item("wrong", "src/lib.rs", 40), repo.target_item()],
            )],
            incoming: vec![
                (
                    "wrong".to_owned(),
                    vec![repo.call("from_wrong", "src/b.rs", 1)],
                ),
                (
                    "target".to_owned(),
                    vec![repo.call("from_target", "src/a.rs", 1)],
                ),
            ],
            ..FakeScript::full()
        })
        .await;
        let outcome = rig.run(&rig.request(TargetShape::Callable)).await;
        assert_eq!(outcome.op_counts.incoming, Some(1));
        let incoming = outcome
            .locations
            .iter()
            .find(|location| location.op == Op::Incoming)
            .expect("an incoming location");
        assert_eq!((incoming.rel.as_str(), incoming.line0), ("src/a.rs", 5));
    }

    #[tokio::test]
    async fn prepare_falls_back_to_first_item() {
        let mut rig = Rig::rust(|repo| FakeScript {
            prepare: vec![(
                TARGET,
                vec![
                    repo.item("first", "src/lib.rs", 40),
                    repo.item("second", "src/lib.rs", 50),
                ],
            )],
            incoming: vec![(
                "first".to_owned(),
                vec![repo.call("from_first", "src/a.rs", 1)],
            )],
            ..FakeScript::full()
        })
        .await;
        let outcome = rig.run(&rig.request(TargetShape::Callable)).await;
        assert_eq!(
            outcome.op_counts.incoming,
            Some(1),
            "the first item was used"
        );
    }

    #[tokio::test]
    async fn prepare_without_items_means_no_incoming_calls() {
        let mut rig = Rig::rust(|_| FakeScript::full()).await;
        let outcome = rig.run(&rig.request(TargetShape::Callable)).await;
        assert_eq!(outcome.status, LiveStatus::Ok);
        assert_eq!(outcome.op_counts.incoming, Some(0));
        assert_eq!(
            rig.methods(),
            vec![
                "textDocument/prepareCallHierarchy",
                "textDocument/references"
            ],
            "no item, no incomingCalls request"
        );
    }

    #[tokio::test]
    async fn incoming_sites_use_from_ranges() {
        let mut rig = Rig::rust(|repo| {
            repo.with_incoming(vec![
                repo.call("caller", "src/a.rs", 3),
                repo.call("silent", "src/b.rs", 0),
            ])
        })
        .await;
        let outcome = rig.run(&rig.request(TargetShape::Callable)).await;
        let sites: Vec<(String, u32)> = outcome
            .locations
            .iter()
            .filter(|location| location.op == Op::Incoming)
            .map(|location| (location.rel.clone(), location.sites))
            .collect();
        assert_eq!(
            sites,
            vec![("src/a.rs".to_owned(), 3), ("src/b.rs".to_owned(), 1)],
            "no call range still counts one site"
        );
    }

    /// A server whose first `lag` query answers are empty and whose `references`
    /// answer is then one caller.
    async fn lagging(lag: usize) -> Rig {
        Rig::rust(|repo| FakeScript {
            empty_first: lag,
            ..repo.with_references(vec![repo.location("src/a.rs", 3)])
        })
        .await
    }

    #[tokio::test]
    async fn retry_on_empty_when_heuristic_has_callers() {
        // TypeLike asks references only, so one attempt consumes one empty answer.
        let mut rig = lagging(1).await;
        let mut req = rig.request(TargetShape::TypeLike);
        req.heuristic_hint = 2;
        let outcome = rig.run(&req).await;
        assert_eq!(outcome.status, LiveStatus::Ok);
        assert!(outcome.retried_empty);
        assert_eq!(outcome.locations.len(), 1, "the retry found the caller");
        assert_eq!(rig.requests_of("textDocument/references"), 2);
    }

    #[tokio::test]
    async fn no_retry_when_heuristic_is_empty() {
        let mut rig = lagging(1).await;
        let req = rig.request(TargetShape::TypeLike);
        let outcome = rig.run(&req).await;
        assert!(!outcome.retried_empty);
        assert!(outcome.locations.is_empty());
        assert_eq!(rig.requests_of("textDocument/references"), 1);
    }

    #[tokio::test]
    async fn retry_happens_at_most_once() {
        let mut rig = lagging(5).await;
        let mut req = rig.request(TargetShape::TypeLike);
        req.heuristic_hint = 2;
        let outcome = rig.run(&req).await;
        assert!(outcome.retried_empty);
        assert!(
            outcome.locations.is_empty(),
            "still empty after the one retry"
        );
        assert_eq!(
            outcome.status,
            LiveStatus::Ok,
            "an empty union is an answer"
        );
        assert_eq!(rig.requests_of("textDocument/references"), 2);
    }

    #[tokio::test]
    async fn no_retry_when_rust_analyzer_is_quiescent() {
        let mut rig = Rig::rust(|_| FakeScript {
            on_initialized: vec![FakeNotification::ServerStatus {
                health: "ok",
                quiescent: true,
            }],
            ..FakeScript::full()
        })
        .await;
        eventually("quiescent", || {
            rig.session.events.borrow().quiescent_known() == Some(true)
        })
        .await;
        let mut req = rig.request(TargetShape::TypeLike);
        req.heuristic_hint = 3;
        let outcome = rig.run(&req).await;
        assert!(
            !outcome.retried_empty,
            "an idle server's empty answer is trusted"
        );
        assert_eq!(rig.requests_of("textDocument/references"), 1);
    }

    #[tokio::test]
    async fn no_retry_when_the_deadline_leaves_no_room_after_the_pause() {
        let mut rig = lagging(1).await;
        let mut req = rig.request(TargetShape::TypeLike);
        req.heuristic_hint = 2;
        req.retry_pause = Duration::from_secs(5);
        req.deadline = Instant::now() + Duration::from_millis(150);
        let started = Instant::now();
        let outcome = rig.run(&req).await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the 5 s pause must not outlive the deadline"
        );
        assert_eq!(outcome.status, LiveStatus::Ok, "the empty answer stands");
        assert!(!outcome.retried_empty);
        assert_eq!(rig.requests_of("textDocument/references"), 1);
    }

    #[tokio::test]
    async fn deadline_before_first_request_is_deadline() {
        let mut rig = Rig::rust(|_| FakeScript::full()).await;
        let mut req = rig.request(TargetShape::Callable);
        req.deadline = Instant::now();
        let outcome = rig.run(&req).await;
        assert_eq!(outcome.status, LiveStatus::Deadline);
        assert!(outcome.locations.is_empty());
        assert!(rig.methods().is_empty(), "nothing was sent");
    }

    #[tokio::test]
    async fn request_timeout_is_timeout_not_partial() {
        let repo = Repo::new();
        let script = FakeScript {
            // `incomingCalls` is fast, `references` outlives the request timeout.
            delay: Duration::from_millis(500),
            delay_only: Some("textDocument/references"),
            ..repo.with_incoming(vec![repo.call("caller", "src/a.rs", 1)])
        };
        let mut rig = Rig::connect(repo, slow_spec(LspLang::Rust), script).await;
        let outcome = rig.run(&rig.request(TargetShape::Callable)).await;
        assert_eq!(outcome.status, LiveStatus::Timeout, "{:?}", outcome.detail);
        assert!(
            outcome.locations.is_empty(),
            "the incoming calls are not reported alone"
        );
        assert_eq!(outcome.op_counts, OpCounts::default());
        assert!(
            outcome
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("no answer within"))
        );
    }

    #[tokio::test]
    async fn request_cut_by_the_deadline_is_deadline() {
        let mut rig = Rig::rust(|_| FakeScript {
            delay: Duration::from_millis(500),
            ..FakeScript::full()
        })
        .await;
        let mut req = rig.request(TargetShape::TypeLike);
        req.deadline = Instant::now() + Duration::from_millis(100);
        let outcome = rig.run(&req).await;
        assert_eq!(outcome.status, LiveStatus::Deadline);
        assert!(outcome.ms < 1_000);
    }

    #[tokio::test]
    async fn external_locations_are_counted_and_dropped() {
        let outside = tempfile::tempdir().expect("tempdir");
        let mut rig = Rig::rust(|repo| {
            let external = Location {
                uri: path_to_uri(&outside.path().join("dep.rs")).expect("uri"),
                range: Range::default(),
            };
            repo.with_references(vec![repo.location("src/a.rs", 3), external])
        })
        .await;
        let outcome = rig.run(&rig.request(TargetShape::TypeLike)).await;
        assert_eq!(outcome.external, 1);
        assert_eq!(
            outcome.op_counts.references,
            Some(1),
            "only the in-repo location counts"
        );
        assert_eq!(outcome.locations.len(), 1);
    }

    #[tokio::test]
    async fn java_comment_references_are_counted_as_filtered() {
        let repo = Repo::new();
        repo.write(
            "src/Doc.java",
            "/**\n * See {@link Target#run()}.\n */\nclass Doc {\n    void go() { t.run(); }\n}\n",
        );
        let script = repo.with_references(vec![
            repo.location("src/Doc.java", 1),
            repo.location("src/Doc.java", 4),
        ]);
        let mut rig = Rig::connect(repo, spec(LspLang::Java), script).await;
        let outcome = rig.run(&rig.request(TargetShape::TypeLike)).await;
        assert_eq!(outcome.comment_filtered, 1);
        assert_eq!(outcome.op_counts.references, Some(1));
        assert_eq!(outcome.locations[0].line0, 4);
    }

    #[tokio::test]
    async fn rust_references_in_doc_comments_are_kept() {
        let repo = Repo::new();
        repo.write("src/a.rs", "/// See [`target`].\nfn user() { target(); }\n");
        let script = repo.with_references(vec![
            repo.location("src/a.rs", 0),
            repo.location("src/a.rs", 1),
        ]);
        let mut rig = Rig::connect(repo, spec(LspLang::Rust), script).await;
        let outcome = rig.run(&rig.request(TargetShape::TypeLike)).await;
        assert_eq!(outcome.comment_filtered, 0);
        assert_eq!(outcome.locations.len(), 2);
    }

    #[tokio::test]
    async fn unreadable_target_file_is_no_position() {
        let mut rig = Rig::rust(|_| FakeScript::full()).await;
        let mut req = rig.request(TargetShape::Callable);
        req.abs_file = rig.repo.dir.path().join("src").join("gone.rs");
        let outcome = rig.run(&req).await;
        assert_eq!(outcome.status, LiveStatus::NoPosition);
        assert!(rig.methods().is_empty());
    }

    #[tokio::test]
    async fn dead_server_is_failed() {
        let mut rig = Rig::rust(|_| FakeScript::full()).await;
        rig.handle.kill();
        eventually("the main loop to end", || !rig.session.is_alive()).await;
        let outcome = rig.run(&rig.request(TargetShape::Callable)).await;
        assert_eq!(outcome.status, LiveStatus::Failed);
        assert_eq!(
            outcome.detail.as_deref(),
            Some("the language server is not running")
        );
    }

    #[tokio::test]
    async fn server_error_is_error_not_partial() {
        let mut rig = Rig::rust(|_| FakeScript {
            // More injected failures than the retry budget (5) allows.
            content_modified_first: 100,
            ..FakeScript::full()
        })
        .await;
        let outcome = rig.run(&rig.request(TargetShape::TypeLike)).await;
        assert_eq!(outcome.status, LiveStatus::Error);
        assert!(
            outcome
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("-32801"))
        );
        assert!(outcome.locations.is_empty());
    }

    #[tokio::test]
    async fn the_second_request_of_an_operation_uses_the_regular_timeout() {
        // First request after the handshake: long allowance (80 ms here is the
        // regular one; the first is raised to 5 s and must not stay raised).
        let repo = Repo::new();
        let script = FakeScript {
            delay: Duration::from_millis(400),
            delay_only: Some("callHierarchy/incomingCalls"),
            ..repo.with_incoming(vec![repo.call("caller", "src/a.rs", 1)])
        };
        let mut slow = slow_spec(LspLang::Rust);
        slow.first_request_timeout = Duration::from_secs(5);
        let mut rig = Rig::connect(repo, slow, script).await;
        let outcome = rig.run(&rig.request(TargetShape::Callable)).await;
        assert_eq!(
            outcome.status,
            LiveStatus::Timeout,
            "prepare used the long first timeout, incomingCalls the 80 ms one"
        );
    }

    #[test]
    fn status_words_are_the_wire_words() {
        let words = [
            (LiveStatus::Ok, "ok"),
            (LiveStatus::NoPosition, "no_position"),
            (LiveStatus::NotReady, "not_ready"),
            (LiveStatus::NoServer, "no_server"),
            (LiveStatus::Failed, "failed"),
            (LiveStatus::Timeout, "timeout"),
            (LiveStatus::Deadline, "deadline"),
            (LiveStatus::Error, "error"),
        ];
        for (status, word) in words {
            assert_eq!(status.as_str(), word);
        }
    }

    #[test]
    fn op_counts_serialise_in_wire_order_with_nulls() {
        assert_eq!(
            serde_json::to_string(&counts(Some(12), Some(40), None)).expect("json"),
            r#"{"incoming":12,"references":40,"implementation":null}"#
        );
        assert_eq!(
            serde_json::from_str::<OpCounts>(
                r#"{"incoming":null,"references":1,"implementation":2}"#
            )
            .expect("parse"),
            counts(None, Some(1), Some(2))
        );
    }

    #[test]
    fn without_answer_carries_a_detail_and_no_locations() {
        let outcome = LiveOutcome::without_answer(LiveStatus::NotReady, "still starting");
        assert_eq!(outcome.status, LiveStatus::NotReady);
        assert_eq!(outcome.detail.as_deref(), Some("still starting"));
        assert!(outcome.locations.is_empty());
        assert_eq!(outcome.op_counts, OpCounts::default());
    }
}
