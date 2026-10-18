//! Language-server settings and the per-repository scratch directory
//! (P3-D12, P3-D14, Appendix C of the Phase 3 plan).
//!
//! Pure, like `embed::settings`: every input comes through the `lookup` closure
//! (process environment, then the knot-lite `.env`), so the process
//! environment is never read here and never mutated (P1-D10). Invalid values
//! never fail the resolution: they fall back to the default and leave a
//! warning that `status` surfaces.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::embed::settings::{first_set, user_cache_subdir};
use crate::index::hash::{sha256_hex, short16};

/// Master switch: `on|off` (default `on`).
pub const ENV_ENABLED: &str = "KNOT_LITE_LSP";
/// Build-code execution (build scripts, proc-macros, Gradle import): `on|off` (default `on`).
pub const ENV_BUILD_EXEC: &str = "KNOT_LITE_LSP_BUILD_EXEC";
/// Root of the per-repository scratch directories.
pub const ENV_CACHE_DIR: &str = "KNOT_LITE_LSP_CACHE_DIR";
/// How long a query waits for a starting server, in seconds.
pub const ENV_READY_WAIT: &str = "KNOT_LITE_LSP_READY_WAIT_SECS";
/// Idle time after which the MCP server stops a language server, in seconds (`0` = never).
pub const ENV_IDLE: &str = "KNOT_LITE_LSP_IDLE_SECS";
/// Live language-server targets per `find_callers` call.
pub const ENV_MAX_LIVE_TARGETS: &str = "KNOT_LITE_LSP_MAX_LIVE_TARGETS";
/// Live language-server time per `find_callers` call, in seconds.
pub const ENV_DEADLINE: &str = "KNOT_LITE_LSP_DEADLINE_SECS";
/// Start the servers of the indexed languages when the MCP server starts: `on|off` (default `off`).
pub const ENV_WARMUP: &str = "KNOT_LITE_LSP_WARMUP";
/// Edit-sync mode for language servers: `off|open|watched|open+watched` (default `watched`, P6-D10, P6-D11).
pub const ENV_SYNC: &str = "KNOT_LITE_LSP_SYNC";

/// Default idle shutdown, in seconds.
pub const DEFAULT_IDLE_SECS: u64 = 900;
/// Default number of live targets per call.
pub const DEFAULT_MAX_LIVE_TARGETS: usize = 10;
/// Default live deadline per call, in seconds.
pub const DEFAULT_DEADLINE_SECS: u64 = 30;

/// Upper bound of [`ENV_MAX_LIVE_TARGETS`].
pub const MAX_LIVE_TARGETS_CAP: u64 = 100;
/// Upper bound of [`ENV_DEADLINE`], in seconds.
pub const DEADLINE_CAP_SECS: u64 = 600;
/// Upper bound of [`ENV_READY_WAIT`], in seconds.
pub const READY_WAIT_CAP_SECS: u64 = 3600;
/// Upper bound of [`ENV_IDLE`], in seconds (one week).
pub const IDLE_CAP_SECS: u64 = 7 * 24 * 3600;

/// Components below `~/.cache` of the scratch root.
const CACHE_LEAF: &[&str] = &["knot-lite", "lsp"];

/// Directory name used under `<temp dir>` when no home directory is known.
const TEMP_CACHE_DIR: &str = "knot-lite-lsp";

/// Longest sanitised repository basename kept in a [`repo_key`].
const MAX_KEY_NAME_CHARS: usize = 40;

/// Basename used when a repository path has none.
const FALLBACK_KEY_NAME: &str = "repo";

/// Resolved language-server settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspSettings {
    /// Whether language servers may be started (pre-computed data is read regardless).
    pub enabled: bool,
    /// Whether the repository's build code may run (rust-analyzer build scripts and
    /// proc-macros, jdtls Gradle import).
    pub build_exec: bool,
    /// Root of the per-repository scratch directories.
    pub cache_root: PathBuf,
    /// How long a query waits for a starting server; `None` = the surface default
    /// (the server's ready timeout for the CLI, 20 s for MCP).
    pub ready_wait: Option<Duration>,
    /// Idle time after which a server is stopped; zero disables the reaper.
    pub idle: Duration,
    /// Live language-server targets per `find_callers` call.
    pub max_live_targets: usize,
    /// Live language-server time per `find_callers` call.
    pub deadline: Duration,
    /// Whether the MCP server starts the servers of the indexed languages eagerly.
    pub warmup: bool,
    /// Edit-sync mode (P6-D10, P6-D11).
    pub sync: crate::lsp::sync::SyncMode,
    /// Invalid or clamped values, one human-readable line each.
    pub warnings: Vec<String>,
}

/// Resolve the language-server settings.
///
/// Booleans accept `on|off|true|false|1|0|yes|no` (case-insensitive, trimmed).
/// Numbers are trimmed `u64`s clamped to their cap. Anything else falls back to
/// the default and adds a line to [`LspSettings::warnings`]. Empty values count
/// as unset. The cache root is [`ENV_CACHE_DIR`] → `$HOME/.cache/knot-lite/lsp`
/// → `$USERPROFILE/.cache/knot-lite/lsp` → `<temp_dir>/knot-lite-lsp`.
pub fn resolve_lsp_settings(
    lookup: &dyn Fn(&str) -> Option<String>,
    temp_dir: &Path,
) -> LspSettings {
    let mut reader = Reader {
        lookup,
        warnings: Vec::new(),
    };
    let enabled = reader.flag(ENV_ENABLED, true);
    let build_exec = reader.flag(ENV_BUILD_EXEC, true);
    let warmup = reader.flag(ENV_WARMUP, false);
    let ready_wait = reader
        .number(ENV_READY_WAIT, READY_WAIT_CAP_SECS)
        .map(Duration::from_secs);
    let idle = Duration::from_secs(
        reader
            .number(ENV_IDLE, IDLE_CAP_SECS)
            .unwrap_or(DEFAULT_IDLE_SECS),
    );
    let max_live_targets = reader
        .number(ENV_MAX_LIVE_TARGETS, MAX_LIVE_TARGETS_CAP)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(DEFAULT_MAX_LIVE_TARGETS);
    let deadline = Duration::from_secs(
        reader
            .number(ENV_DEADLINE, DEADLINE_CAP_SECS)
            .unwrap_or(DEFAULT_DEADLINE_SECS),
    );
    let cache_root = first_set(lookup, &[ENV_CACHE_DIR])
        .map(PathBuf::from)
        .or_else(|| user_cache_subdir(lookup, CACHE_LEAF))
        .unwrap_or_else(|| temp_dir.join(TEMP_CACHE_DIR));
    let sync = match (lookup)(ENV_SYNC).as_deref().map(str::trim) {
        None | Some("") => crate::lsp::sync::SyncMode::Watched,
        Some(val) => match crate::lsp::sync::SyncMode::parse(val) {
            Some(mode) => mode,
            None => {
                reader.warnings.push(format!(
                    "{ENV_SYNC}='{val}' is invalid; must be one of off|open|watched|open+watched (using default 'watched')"
                ));
                crate::lsp::sync::SyncMode::Watched
            }
        },
    };
    LspSettings {
        enabled,
        build_exec,
        cache_root,
        ready_wait,
        idle,
        max_live_targets,
        deadline,
        warmup,
        sync,
        warnings: reader.warnings,
    }
}

/// `<sanitised basename>-<first 16 hex of sha256(canonical repo path)>`.
///
/// The basename keeps `[A-Za-z0-9._-]` (everything else becomes `_`, at most 40
/// characters, empty → `repo`); the hash keeps two checkouts that share a
/// basename apart.
pub fn repo_key(canonical_repo: &Path) -> String {
    let basename = canonical_repo
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut name: String = basename
        .chars()
        .take(MAX_KEY_NAME_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        name.push_str(FALLBACK_KEY_NAME);
    }
    let hash = short16(sha256_hex(canonical_repo.to_string_lossy().as_bytes()));
    format!("{name}-{hash}")
}

/// Scratch directory of one repository: `<cache root>/<repo key>`.
pub fn scratch_dir(settings: &LspSettings, canonical_repo: &Path) -> PathBuf {
    settings.cache_root.join(repo_key(canonical_repo))
}

/// Reads typed values through the lookup, collecting warnings.
struct Reader<'a> {
    lookup: &'a dyn Fn(&str) -> Option<String>,
    warnings: Vec<String>,
}

impl Reader<'_> {
    /// Boolean value of `key`, or `default` (with a warning when it is not a boolean).
    fn flag(&mut self, key: &str, default: bool) -> bool {
        let Some(raw) = first_set(self.lookup, &[key]) else {
            return default;
        };
        match raw.to_ascii_lowercase().as_str() {
            "on" | "true" | "1" | "yes" => true,
            "off" | "false" | "0" | "no" => false,
            _ => {
                self.warnings.push(format!(
                    "{key}='{raw}' is not a boolean (on|off|true|false|1|0|yes|no); using the default ({})",
                    if default { "on" } else { "off" }
                ));
                default
            }
        }
    }

    /// Non-negative integer value of `key` clamped to `cap`; `None` when unset or invalid.
    fn number(&mut self, key: &str, cap: u64) -> Option<u64> {
        let raw = first_set(self.lookup, &[key])?;
        match raw.parse::<u64>() {
            Ok(n) if n > cap => {
                self.warnings
                    .push(format!("{key}={n} exceeds the maximum {cap}; using {cap}"));
                Some(cap)
            }
            Ok(n) => Some(n),
            Err(_) => {
                self.warnings.push(format!(
                    "{key}='{raw}' is not a non-negative integer; using the default"
                ));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key/value pairs of one environment layer.
    type Pairs<'a> = &'a [(&'a str, &'a str)];

    /// Resolve with a map-backed environment and `/tmp` as the temp dir.
    fn resolve(pairs: Pairs<'_>) -> LspSettings {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let lookup = move |key: &str| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        resolve_lsp_settings(&lookup, Path::new("/tmp"))
    }

    #[test]
    fn defaults_when_nothing_set() {
        let s = resolve(&[]);
        assert!(s.enabled);
        assert!(s.build_exec);
        assert!(!s.warmup);
        assert_eq!(s.ready_wait, None);
        assert_eq!(s.idle, Duration::from_secs(900));
        assert_eq!(s.max_live_targets, 10);
        assert_eq!(s.deadline, Duration::from_secs(30));
        assert_eq!(s.cache_root, PathBuf::from("/tmp/knot-lite-lsp"));
        assert_eq!(s.sync, crate::lsp::sync::SyncMode::Watched);
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn booleans_accept_documented_spellings() {
        let truthy = ["on", "true", "1", "yes", " ON ", "True", "YES"];
        let falsy = ["off", "false", "0", "no", " OFF ", "False", "NO"];
        for value in truthy {
            let s = resolve(&[(ENV_ENABLED, value), (ENV_WARMUP, value)]);
            assert!(s.enabled && s.warmup, "{value:?}");
            assert!(s.warnings.is_empty(), "{value:?}");
        }
        for value in falsy {
            let s = resolve(&[(ENV_ENABLED, value), (ENV_BUILD_EXEC, value)]);
            assert!(!s.enabled && !s.build_exec, "{value:?}");
            assert!(s.warnings.is_empty(), "{value:?}");
        }
    }

    #[test]
    fn invalid_boolean_falls_back_with_warning() {
        let s = resolve(&[(ENV_ENABLED, "maybe"), (ENV_WARMUP, "2")]);
        assert!(s.enabled, "default of KNOT_LITE_LSP is on");
        assert!(!s.warmup, "default of KNOT_LITE_LSP_WARMUP is off");
        assert_eq!(s.warnings.len(), 2);
        assert!(s.warnings[0].contains("KNOT_LITE_LSP='maybe'"));
        assert!(s.warnings[1].contains("KNOT_LITE_LSP_WARMUP='2'"));
    }

    #[test]
    fn empty_values_count_as_unset_without_warning() {
        let s = resolve(&[(ENV_ENABLED, " "), (ENV_IDLE, ""), (ENV_DEADLINE, "  ")]);
        assert!(s.enabled);
        assert_eq!(s.idle, Duration::from_secs(900));
        assert_eq!(s.deadline, Duration::from_secs(30));
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn numbers_are_parsed_trimmed() {
        let s = resolve(&[
            (ENV_READY_WAIT, " 45 "),
            (ENV_IDLE, "0"),
            (ENV_MAX_LIVE_TARGETS, "3"),
            (ENV_DEADLINE, "120"),
        ]);
        assert_eq!(s.ready_wait, Some(Duration::from_secs(45)));
        assert_eq!(s.idle, Duration::ZERO);
        assert_eq!(s.max_live_targets, 3);
        assert_eq!(s.deadline, Duration::from_secs(120));
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn numbers_are_clamped_to_caps() {
        let s = resolve(&[
            (ENV_MAX_LIVE_TARGETS, "1000"),
            (ENV_DEADLINE, "99999"),
            (ENV_READY_WAIT, "86400"),
            (ENV_IDLE, "99999999"),
        ]);
        assert_eq!(s.max_live_targets, 100);
        assert_eq!(s.deadline, Duration::from_secs(600));
        assert_eq!(s.ready_wait, Some(Duration::from_secs(3600)));
        assert_eq!(s.idle, Duration::from_secs(IDLE_CAP_SECS));
        assert_eq!(s.warnings.len(), 4, "every clamp is reported");
    }

    #[test]
    fn invalid_numbers_fall_back_with_warning() {
        let s = resolve(&[
            (ENV_DEADLINE, "soon"),
            (ENV_MAX_LIVE_TARGETS, "-1"),
            (ENV_READY_WAIT, "1.5"),
        ]);
        assert_eq!(s.deadline, Duration::from_secs(30));
        assert_eq!(s.max_live_targets, 10);
        assert_eq!(s.ready_wait, None);
        assert_eq!(s.warnings.len(), 3);
        assert!(s.warnings[0].contains("KNOT_LITE_LSP_READY_WAIT_SECS='1.5'"));
    }

    #[test]
    fn cache_root_precedence_env_home_userprofile_temp() {
        let cases: &[(Pairs<'_>, &str)] = &[
            (
                &[
                    (ENV_CACHE_DIR, "/explicit"),
                    ("HOME", "/home/u"),
                    ("USERPROFILE", "/win/u"),
                ],
                "/explicit",
            ),
            (
                &[("HOME", "/home/u"), ("USERPROFILE", "/win/u")],
                "/home/u/.cache/knot-lite/lsp",
            ),
            (&[("USERPROFILE", "/win/u")], "/win/u/.cache/knot-lite/lsp"),
            (&[], "/tmp/knot-lite-lsp"),
            (&[(ENV_CACHE_DIR, " "), ("HOME", "")], "/tmp/knot-lite-lsp"),
        ];
        for (env, expected) in cases {
            assert_eq!(resolve(env).cache_root, PathBuf::from(expected), "{env:?}");
        }
    }

    /// Split a key into its sanitised name and its hash.
    fn split_key(key: &str) -> (&str, &str) {
        key.rsplit_once('-').expect("<name>-<hash>")
    }

    #[test]
    fn repo_key_is_stable_and_sanitised() {
        let key = repo_key(Path::new("/a/b/My Repo!"));
        let (name, hash) = split_key(&key);
        assert_eq!(name, "My_Repo_");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, repo_key(Path::new("/a/b/My Repo!")), "stable");
        assert_ne!(key, repo_key(Path::new("/c/b/My Repo!")), "path-dependent");
    }

    #[test]
    fn repo_key_truncates_long_names_and_names_the_empty_one() {
        let long = format!("/x/{}", "a".repeat(100));
        assert_eq!(split_key(&repo_key(Path::new(&long))).0.len(), 40);
        assert_eq!(split_key(&repo_key(Path::new("/"))).0, "repo");
    }

    #[test]
    fn repo_key_keeps_dots_dashes_and_replaces_non_ascii_per_char() {
        let key = repo_key(Path::new("/x/my-repo.v2-ñandú"));
        assert!(key.starts_with("my-repo.v2-_and_-"), "{key}");
    }

    #[test]
    fn scratch_dir_joins_root_and_key() {
        let settings = resolve(&[(ENV_CACHE_DIR, "/cache")]);
        let repo = Path::new("/work/demo");
        assert_eq!(
            scratch_dir(&settings, repo),
            Path::new("/cache").join(repo_key(repo))
        );
    }
}
