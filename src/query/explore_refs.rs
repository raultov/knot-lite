//! `explore_file`'s language-server work (P5-D14…P5-D16): reference counts
//! asked live (`references` only), and the outline of a file the index does
//! not describe (`documentSymbol`).
//!
//! The pool is used exactly as `find_callers` uses it (P5-D16): the first
//! request may wait the pool's ready wait for a starting server (one that is
//! still not ready answers nothing and keeps starting in the background); the
//! live budget starts once the server answers.

use std::path::Path;
use std::time::{Duration, Instant};

use async_lsp::lsp_types::Position;
use serde::Serialize;

use super::callers::LspContext;
use super::sources::live_container_count;
use crate::capability::languages::LspLang;
use crate::graph::containers::ContainerKey;
use crate::index::load::LoadedIndex;
use crate::lsp::ops::{OpSet, TargetShape};
use crate::lsp::outline::OutlineSymbol;
use crate::lsp::target::{DEFAULT_RETRY_PAUSE, LiveRequest, LiveStatus};
use crate::lsp::uri::join_rel;

/// Entities asked live per call, at most.
pub const EXPLORE_LIVE_MAX: usize = 40;
/// Live time per call (cut further by `KNOT_LITE_LSP_DEADLINE_SECS`).
pub const EXPLORE_LIVE_BUDGET: Duration = Duration::from_secs(4);

/// `lsp` object of explore/v2 (§10.2).
#[derive(Debug, Clone, Serialize)]
pub struct ExploreLsp {
    /// `ok`, `partial` (budget or deadline cut), `not_ready`, `unavailable`,
    /// `off` or `not_applicable` (no server for the language).
    pub status: &'static str,
    /// The server program, when the language has one.
    pub server: Option<&'static str>,
    /// Requests sent.
    pub asked: usize,
    /// Requests answered.
    pub answered: usize,
    /// Milliseconds spent on the server.
    pub ms: u64,
}

impl ExploreLsp {
    fn idle(status: &'static str, lang: Option<LspLang>) -> Self {
        Self {
            status,
            server: lang.map(LspLang::server_program),
            asked: 0,
            answered: 0,
            ms: 0,
        }
    }
}

/// Something whose references may be counted live.
#[derive(Debug, Clone)]
pub struct RefTarget {
    /// Row of the response it belongs to.
    pub row: usize,
    /// LSP position of its name in the current file.
    pub position: Position,
    /// What it is (decides grouping of the answer).
    pub shape: TargetShape,
    /// Its own container (references from inside it are recursion).
    pub own: ContainerKey,
    /// Order of asking: types and callables first, then by line.
    pub rank: (u8, usize),
}

/// The language-server side of one `explore_file` call.
pub struct ExploreLive<'a> {
    /// The loaded index (its container table groups the answers).
    pub index: &'a LoadedIndex,
    /// Settings, repository root and pool.
    pub lsp: &'a LspContext<'a>,
    /// The file's server language, if it has one.
    pub lang: Option<LspLang>,
    /// The repo-relative file.
    pub rel: &'a str,
    /// Optional overlay lookup for changed files (P6-D13).
    pub lookup: Option<&'a crate::graph::containers::ContainerLookup<'a>>,
}

/// How a run of requests ended.
#[derive(Default)]
struct Tally {
    asked: usize,
    answered: usize,
    cut: bool,
    stopped: Option<&'static str>,
}

impl ExploreLive<'_> {
    /// The status when nothing can be asked: `not_applicable` without a
    /// server language, `off` without a pool; `None` when asking is possible.
    pub fn unavailable(&self) -> Option<ExploreLsp> {
        match (self.lang, self.lsp.pool) {
            (None, _) => Some(ExploreLsp::idle("not_applicable", None)),
            (Some(lang), None) => Some(ExploreLsp::idle("off", Some(lang))),
            (Some(_), Some(_)) => None,
        }
    }

    fn budget(&self) -> Duration {
        EXPLORE_LIVE_BUDGET.min(self.lsp.settings.deadline)
    }

    /// The deadline of a first request: the budget plus the time a starting
    /// server may take to become ready.
    fn first_deadline(&self, pool: &crate::lsp::pool::LspPool) -> Instant {
        Instant::now() + pool.ready_wait() + self.budget()
    }

    fn request(
        &self,
        lang: LspLang,
        position: Position,
        ops: OpSet,
        deadline: Instant,
    ) -> LiveRequest {
        LiveRequest {
            lang,
            target_id: format!("explore:{}", self.rel),
            abs_file: join_rel(self.lsp.repo_root, self.rel),
            rel_file: self.rel.to_owned(),
            position,
            ops,
            heuristic_hint: 0,
            deadline,
            retry_pause: DEFAULT_RETRY_PAUSE,
        }
    }

    /// The file's current symbols from the server, or the status that explains
    /// why there are none.
    pub fn outline(&self) -> Result<Vec<OutlineSymbol>, ExploreLsp> {
        if let Some(idle) = self.unavailable() {
            return Err(idle);
        }
        let (Some(lang), Some(pool)) = (self.lang, self.lsp.pool) else {
            return Err(ExploreLsp::idle("off", self.lang));
        };
        let started = Instant::now();
        let outcome = pool.query(self.request(
            lang,
            Position::default(),
            OpSet::outline(),
            self.first_deadline(pool),
        ));
        let mut lsp = ExploreLsp::idle(status_word(outcome.status), Some(lang));
        lsp.asked = 1;
        lsp.ms = elapsed_ms(started);
        match (outcome.status, outcome.symbols) {
            (LiveStatus::Ok, Some(symbols)) => Ok(symbols),
            (LiveStatus::Ok, None) => {
                lsp.status = "unavailable";
                Err(lsp)
            }
            _ => Err(lsp),
        }
    }

    /// Count the references of `targets` live, best first, within the budget:
    /// `(row, distinct caller containers)` for every answered target.
    pub fn count(&self, mut targets: Vec<RefTarget>) -> (Vec<(usize, usize)>, ExploreLsp) {
        if let Some(idle) = self.unavailable() {
            return (Vec::new(), idle);
        }
        let (Some(lang), Some(pool)) = (self.lang, self.lsp.pool) else {
            return (Vec::new(), ExploreLsp::idle("off", self.lang));
        };
        targets.sort_by_key(|target| target.rank);
        let mut tally = Tally {
            cut: targets.len() > EXPLORE_LIVE_MAX,
            ..Tally::default()
        };
        targets.truncate(EXPLORE_LIVE_MAX);
        let started = Instant::now();
        let mut deadline = self.first_deadline(pool);
        let mut counts = Vec::new();
        for target in targets {
            if Instant::now() >= deadline {
                tally.cut = true;
                break;
            }
            tally.asked += 1;
            let outcome =
                pool.query(self.request(lang, target.position, OpSet::references_only(), deadline));
            if tally.asked == 1 {
                // The server is up (or will not be): the budget starts now.
                deadline = deadline.min(Instant::now() + self.budget());
            }
            match outcome.status {
                LiveStatus::Ok => {
                    tally.answered += 1;
                    let default_lookup = crate::graph::containers::ContainerLookup::index_only(
                        &self.index.containers,
                    );
                    let lookup = self.lookup.unwrap_or(&default_lookup);
                    let count = live_container_count(
                        self.index,
                        &outcome,
                        target.shape,
                        target.own,
                        lookup,
                    );
                    counts.push((target.row, count));
                }
                LiveStatus::NoPosition => {}
                LiveStatus::Deadline | LiveStatus::Timeout => {
                    tally.cut = true;
                    break;
                }
                other => {
                    tally.stopped = Some(status_word(other));
                    break;
                }
            }
        }
        let status = tally
            .stopped
            .unwrap_or(if tally.cut { "partial" } else { "ok" });
        let lsp = ExploreLsp {
            status,
            server: Some(lang.server_program()),
            asked: tally.asked,
            answered: tally.answered,
            ms: elapsed_ms(started),
        };
        (counts, lsp)
    }
}

/// The explore status for a request that did not end with `ok`.
fn status_word(status: LiveStatus) -> &'static str {
    match status {
        LiveStatus::Ok => "ok",
        LiveStatus::NotReady => "not_ready",
        LiveStatus::Deadline | LiveStatus::Timeout => "partial",
        LiveStatus::NoPosition | LiveStatus::NoServer | LiveStatus::Failed | LiveStatus::Error => {
            "unavailable"
        }
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The language-server rank of an entity: types 0, callables 1, the rest 2.
pub fn family_rank(family: super::explore::KindFamily) -> u8 {
    match family {
        super::explore::KindFamily::Types => 0,
        super::explore::KindFamily::Callables => 1,
        _ => 2,
    }
}

/// Whether `rel` is a file on disk that the index could describe: inside the
/// repository, under no excluded directory.
pub fn on_disk_candidate(root: &Path, rel: &str) -> bool {
    let path = Path::new(rel);
    let inside = !rel.is_empty()
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)));
    let excluded = path.parent().is_some_and(|parent| {
        parent.components().any(|part| {
            crate::capability::scan::is_excluded_dir_name(&part.as_os_str().to_string_lossy())
        })
    });
    inside && !excluded && join_rel(root, rel).is_file()
}
