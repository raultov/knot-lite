//! `status --probe-lsp`: start each language server once, report how it did.
//!
//! A probe is data, never an error: a server that is missing, refuses to start
//! or never becomes ready is reported with its diagnostic and `ok: false`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::pool::{LspPool, PoolSnapshot, SlotSnapshot};
use crate::capability::languages::LspLang;

/// How often the pool is asked whether the servers are done starting.
const POLL: Duration = Duration::from_millis(100);

/// Slot states that will not change without another query.
const SETTLED: [&str; 4] = ["ready", "failed", "disabled", "missing"];

/// What one probe found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbeResult {
    /// The server became ready.
    pub ok: bool,
    /// Milliseconds it took to become ready.
    pub ready_ms: Option<u64>,
    /// Version the server reported.
    pub version: String,
    /// Process-tree memory when probed, in MiB.
    pub rss_mib: Option<f64>,
    /// Why it did not become ready (or what is odd about it).
    pub diagnostic: Option<String>,
}

impl ProbeResult {
    fn of(slot: Option<&SlotSnapshot>, waited_for: Duration) -> Self {
        let Some(slot) = slot else {
            return Self::unavailable("the pool has no slot for this language".to_owned());
        };
        if slot.state == "ready" {
            return Self {
                ok: true,
                ready_ms: slot.ready_ms,
                version: slot.version.clone(),
                rss_mib: slot.rss_mib,
                diagnostic: slot.diagnostic.clone(),
            };
        }
        let why = slot.diagnostic.clone().unwrap_or_else(|| {
            format!(
                "{} was {} after {} s",
                slot.server,
                slot.state.replace('_', " "),
                waited_for.as_secs()
            )
        });
        Self::unavailable(why)
    }

    /// A probe that could not even try, with the reason.
    pub fn unavailable(diagnostic: String) -> Self {
        Self {
            ok: false,
            ready_ms: None,
            version: String::new(),
            rss_mib: None,
            diagnostic: Some(diagnostic),
        }
    }
}

/// Start the servers of `languages` through `pool` and wait up to `wait` for
/// each to settle. The result is keyed by language directory name.
pub fn probe_languages(
    pool: &LspPool,
    languages: &[LspLang],
    wait: Duration,
) -> BTreeMap<String, ProbeResult> {
    for lang in languages {
        pool.warm(*lang);
    }
    let started = Instant::now();
    let snapshot = loop {
        let snapshot = pool.snapshot();
        if all_settled(&snapshot, languages) || started.elapsed() >= wait {
            break snapshot;
        }
        std::thread::sleep(POLL);
    };
    let waited_for = started.elapsed();
    languages
        .iter()
        .map(|lang| {
            let slot = snapshot.slots.get(lang.dir_name());
            (
                lang.dir_name().to_owned(),
                ProbeResult::of(slot, waited_for),
            )
        })
        .collect()
}

fn all_settled(snapshot: &PoolSnapshot, languages: &[LspLang]) -> bool {
    languages.iter().all(|lang| {
        snapshot
            .slots
            .get(lang.dir_name())
            .is_some_and(|slot| SETTLED.contains(&slot.state.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::fake::{FakeScript, FakeServers};

    #[test]
    fn a_server_that_starts_is_reported_ready() {
        let pool = FakeServers::new(|_| FakeScript::full()).pool();
        let result = probe_languages(&pool, &[LspLang::Rust], Duration::from_secs(10));
        let rust = &result["rust"];
        assert!(rust.ok, "{rust:?}");
        assert!(rust.ready_ms.is_some());
        assert_eq!(rust.diagnostic, None);
    }

    #[test]
    fn a_missing_server_is_data_not_an_error() {
        let pool = FakeServers::missing().pool();
        let result = probe_languages(
            &pool,
            &[LspLang::Rust, LspLang::Java],
            Duration::from_secs(10),
        );
        for language in ["rust", "java"] {
            let probe = &result[language];
            assert!(!probe.ok);
            assert!(
                probe
                    .diagnostic
                    .as_deref()
                    .is_some_and(|d| d.contains("not found in PATH")),
                "{probe:?}"
            );
        }
    }

    #[test]
    fn a_server_that_never_settles_times_out_with_a_reason() {
        let pool = FakeServers::new(|_| FakeScript::full())
            .starting_for(Duration::from_secs(30))
            .pool();
        let result = probe_languages(&pool, &[LspLang::Rust], Duration::from_millis(300));
        let rust = &result["rust"];
        assert!(!rust.ok);
        assert!(
            rust.diagnostic
                .as_deref()
                .is_some_and(|d| d.contains("starting")),
            "{rust:?}"
        );
    }

    #[test]
    fn only_the_asked_languages_are_probed() {
        let pool = FakeServers::new(|_| FakeScript::full()).pool();
        let result = probe_languages(&pool, &[LspLang::Ts], Duration::from_secs(10));
        assert_eq!(result.keys().collect::<Vec<_>>(), vec!["ts"]);
    }
}
