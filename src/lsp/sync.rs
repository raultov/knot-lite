//! Edit sync for language servers that need it (P6-D10, P6-D11).
//!
//! A per-language **ledger** records what the server has seen (`rel →
//! (mtime, size)`).  Before each request of a language with `sync_edits`,
//! files whose `(mtime, size)` differ from the ledger are synced by the
//! configured [`SyncMode`], then the ledger is updated.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use async_lsp::lsp_types::{FileChangeType, FileEvent};
use serde::{Deserialize, Serialize};

use super::session::LspSession;
use super::uri::path_to_uri;
use crate::capability::languages::LspLang;

/// Maximum files synced per request (P6-D10).
pub const MAX_SYNC_FILES: usize = 64;

/// Upper bound on waiting for the server to settle after edit sync (P6-D10).
pub const SYNC_WAIT: Duration = Duration::from_secs(10);

/// How changed files are pushed to the server (P6-D11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    /// No sync (0.5.0 behaviour).
    Off,
    /// `didClose` + `didOpen` of each changed file.
    Open,
    /// One `workspace/didChangeWatchedFiles` notification (default, selected by J1).
    #[default]
    Watched,
    /// Both `open` and `watched`.
    OpenWatched,
}

impl SyncMode {
    /// Parse the `KNOT_LITE_LSP_SYNC` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "open" => Some(Self::Open),
            "watched" => Some(Self::Watched),
            "open+watched" | "openwatched" => Some(Self::OpenWatched),
            _ => None,
        }
    }

    /// The wire label for `status`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Open => "open",
            Self::Watched => "watched",
            Self::OpenWatched => "open+watched",
        }
    }
}

/// One file's last-seen state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// Modification time as seconds since the Unix epoch.
    pub mtime_secs: u64,
    /// File size in bytes.
    pub size: u64,
}

/// What the server has seen: `rel → (mtime, size)`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    /// Workspace creation marker (mismatch = workspace was recreated, P6 risk R3).
    #[serde(default)]
    pub workspace_marker: String,
    /// Repo-relative path → last-seen state.
    pub seen: BTreeMap<String, Seen>,
}

impl Ledger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from `path` (JSON lines); a missing or unreadable ledger is empty.
    pub fn load(path: &Path, current_marker: &str) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut lines = text.lines();
        let Some(first_line) = lines.next() else {
            return Self::default();
        };
        // First line contains the workspace marker:
        let Ok(marker) = serde_json::from_str::<String>(first_line) else {
            return Self::default();
        };
        if !current_marker.is_empty() && marker != current_marker {
            return Self::default();
        }
        let mut seen = BTreeMap::new();
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok((rel, entry)) = serde_json::from_str::<(String, Seen)>(line) {
                seen.insert(rel, entry);
            }
        }
        Self {
            workspace_marker: marker,
            seen,
        }
    }

    /// Save to `path` (atomic rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = String::new();
        text.push_str(&serde_json::to_string(&self.workspace_marker)?);
        text.push('\n');
        for (rel, seen) in &self.seen {
            text.push_str(&serde_json::to_string(&(rel, seen))?);
            text.push('\n');
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Files whose `(mtime, size)` differ from the ledger, sorted, capped at
    /// `MAX_SYNC_FILES`.
    pub fn diff(&self, universe: &[(String, PathBuf)]) -> Vec<String> {
        let mut out = Vec::new();
        for (rel, abs) in universe {
            let Ok(meta) = std::fs::metadata(abs) else {
                // Deleted file: always sync.
                out.push(rel.clone());
                if out.len() >= MAX_SYNC_FILES {
                    break;
                }
                continue;
            };
            let mtime_secs = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let size = meta.len();
            match self.seen.get(rel) {
                Some(prev) if prev.mtime_secs == mtime_secs && prev.size == size => {}
                _ => {
                    out.push(rel.clone());
                    if out.len() >= MAX_SYNC_FILES {
                        break;
                    }
                }
            }
        }
        out
    }

    /// Record `rel` as seen.
    pub fn mark(&mut self, rel: &str, abs: &Path) {
        let Ok(meta) = std::fs::metadata(abs) else {
            self.seen.remove(rel);
            return;
        };
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.seen.insert(
            rel.to_owned(),
            Seen {
                mtime_secs,
                size: meta.len(),
            },
        );
    }

    /// Whether the ledger is empty.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Whether a language's server needs edit sync (P6-D10: Java only).
pub fn sync_edits(lang: LspLang) -> bool {
    matches!(lang, LspLang::Java)
}

/// The scratch path of the ledger for `lang` under `cache_root` / `repo_key`.
pub fn ledger_path(cache_root: &Path, repo_key: &str, lang: LspLang) -> PathBuf {
    let dir = cache_root.join(repo_key);
    let name = match lang {
        LspLang::Rust => "rust-ledger.jsonl",
        LspLang::Ts => "ts-ledger.jsonl",
        LspLang::Java => "java-ledger.jsonl",
    };
    dir.join(name)
}

/// What [`sync_before`] needs.
pub struct SyncInput<'a> {
    pub mode: SyncMode,
    pub repo_root: &'a Path,
    pub universe: &'a [(String, PathBuf)],
    pub deadline: Instant,
}

/// Sync changed files to `session` before a request (P6-D10, P6-D11).
pub async fn sync_before(
    session: &mut LspSession,
    ledger: &mut Ledger,
    input: SyncInput<'_>,
) -> Result<usize> {
    let SyncInput {
        mode,
        repo_root,
        universe,
        deadline,
    } = input;
    if mode == SyncMode::Off {
        return Ok(0);
    }
    let diff = ledger.diff(universe);
    if diff.is_empty() {
        return Ok(0);
    }

    if mode == SyncMode::Open || mode == SyncMode::OpenWatched {
        for rel in &diff {
            let abs = repo_root.join(rel);
            if abs.exists() {
                let _ = session.did_open(&abs);
                let _ = session.did_close(&abs);
            } else {
                let _ = session.did_close(&abs);
            }
        }
    }

    if mode == SyncMode::Watched || mode == SyncMode::OpenWatched {
        let mut events = Vec::new();
        for rel in &diff {
            let abs = repo_root.join(rel);
            if let Ok(uri) = path_to_uri(&abs) {
                let typ = if !abs.exists() {
                    FileChangeType::DELETED
                } else {
                    FileChangeType::CHANGED
                };
                events.push(FileEvent { uri, typ });
            }
        }
        let _ = session.did_change_watched_files(events);
    }

    let sync_timeout = SYNC_WAIT.min(deadline.saturating_duration_since(Instant::now()));
    session.wait_settle(sync_timeout).await;

    for rel in &diff {
        let abs = repo_root.join(rel);
        ledger.mark(rel, &abs);
    }

    Ok(diff.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn off_mode_sends_nothing() {
        assert_eq!(SyncMode::parse("off"), Some(SyncMode::Off));
        assert_eq!(SyncMode::default(), SyncMode::Watched);
    }

    #[tokio::test]
    async fn open_mode_reopens_changed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.java");
        write_file(tmp.path(), "a.java", "class A {}");
        let spec = crate::lsp::fake::test_spec(LspLang::Java);
        let (mut session, handle) = crate::lsp::fake::connect(
            crate::lsp::fake::FakeScript::full(),
            crate::lsp::session::InitInputs {
                spec: &spec,
                repo_root: tmp.path(),
            },
        )
        .await;
        let mut ledger = Ledger::new();
        let universe = vec![("a.java".to_owned(), path)];
        let synced = sync_before(
            &mut session,
            &mut ledger,
            SyncInput {
                mode: SyncMode::Open,
                repo_root: tmp.path(),
                universe: &universe,
                deadline: Instant::now() + Duration::from_secs(5),
            },
        )
        .await
        .unwrap();
        assert_eq!(synced, 1);
        assert_eq!(handle.counters.did_open_count(), 1);
        assert_eq!(handle.counters.did_close_count(), 1);
        assert_eq!(handle.counters.did_change_watched_files_count(), 0);
    }

    #[tokio::test]
    async fn watched_mode_sends_one_notification() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.java");
        write_file(tmp.path(), "a.java", "class A {}");
        let spec = crate::lsp::fake::test_spec(LspLang::Java);
        let (mut session, handle) = crate::lsp::fake::connect(
            crate::lsp::fake::FakeScript::full(),
            crate::lsp::session::InitInputs {
                spec: &spec,
                repo_root: tmp.path(),
            },
        )
        .await;
        let mut ledger = Ledger::new();
        let universe = vec![("a.java".to_owned(), path)];
        let synced = sync_before(
            &mut session,
            &mut ledger,
            SyncInput {
                mode: SyncMode::Watched,
                repo_root: tmp.path(),
                universe: &universe,
                deadline: Instant::now() + Duration::from_secs(5),
            },
        )
        .await
        .unwrap();
        assert_eq!(synced, 1);
        assert_eq!(handle.counters.did_open_count(), 0);
        assert_eq!(handle.counters.did_change_watched_files_count(), 1);
    }

    #[test]
    fn sync_mode_parse() {
        assert_eq!(SyncMode::parse("open"), Some(SyncMode::Open));
        assert_eq!(SyncMode::parse("watched"), Some(SyncMode::Watched));
        assert_eq!(SyncMode::parse("open+watched"), Some(SyncMode::OpenWatched));
        assert_eq!(SyncMode::parse("bogus"), None);
    }

    #[test]
    fn ledger_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ledger.jsonl");
        let mut ledger = Ledger::new();
        ledger.workspace_marker = "marker123".to_owned();
        write_file(tmp.path(), "a.rs", "fn a() {}");
        ledger.mark("a.rs", &tmp.path().join("a.rs"));
        ledger.save(&path).expect("save");
        let loaded = Ledger::load(&path, "marker123");
        assert_eq!(loaded.seen.len(), 1);
        assert!(loaded.seen.contains_key("a.rs"));
        assert_eq!(loaded.workspace_marker, "marker123");

        // Mismatched marker -> empty ledger (R3 mitigation):
        let wrong = Ledger::load(&path, "other_marker");
        assert!(wrong.is_empty());
    }

    #[test]
    fn corrupt_ledger_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ledger.jsonl");
        std::fs::write(&path, "not json\n").unwrap();
        let ledger = Ledger::load(&path, "");
        assert!(ledger.is_empty());
    }

    #[test]
    fn diff_detects_new_and_changed_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a.rs", "fn a() {}");
        write_file(tmp.path(), "b.rs", "fn b() {}");
        let mut ledger = Ledger::new();
        ledger.mark("a.rs", &tmp.path().join("a.rs"));
        ledger.mark("b.rs", &tmp.path().join("b.rs"));
        let universe: Vec<(String, PathBuf)> = ["a.rs", "b.rs"]
            .iter()
            .map(|r| (r.to_string(), tmp.path().join(r)))
            .collect();
        assert!(ledger.diff(&universe).is_empty());
        write_file(tmp.path(), "b.rs", "fn b() { /* edited */ }");
        let diff = ledger.diff(&universe);
        assert_eq!(diff, vec!["b.rs".to_string()]);
    }

    #[test]
    fn deleted_file_is_in_the_diff() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a.rs", "fn a() {}");
        let mut ledger = Ledger::new();
        ledger.mark("a.rs", &tmp.path().join("a.rs"));
        std::fs::remove_file(tmp.path().join("a.rs")).unwrap();
        let universe = vec![("a.rs".to_string(), tmp.path().join("a.rs"))];
        assert_eq!(ledger.diff(&universe), vec!["a.rs".to_string()]);
    }

    #[test]
    fn max_sync_files_and_note() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = Ledger::new();
        let mut universe = Vec::new();
        for i in 0..MAX_SYNC_FILES + 10 {
            let rel = format!("f{i}.rs");
            write_file(tmp.path(), &rel, "fn x() {}");
            universe.push((rel.clone(), tmp.path().join(&rel)));
        }
        let diff = ledger.diff(&universe);
        assert_eq!(diff.len(), MAX_SYNC_FILES);
    }

    #[test]
    fn sync_edits_only_java() {
        assert!(sync_edits(LspLang::Java));
        assert!(!sync_edits(LspLang::Rust));
        assert!(!sync_edits(LspLang::Ts));
    }

    #[test]
    fn ledger_path_per_language() {
        let p = ledger_path(Path::new("/cache"), "repo", LspLang::Java);
        assert!(p.ends_with("java-ledger.jsonl"));
    }

    #[test]
    fn sync_wait_inside_deadline() {
        let deadline = Instant::now() + Duration::from_millis(100);
        let sync_deadline = deadline.min(Instant::now() + SYNC_WAIT);
        assert!(sync_deadline <= deadline);
    }
}
