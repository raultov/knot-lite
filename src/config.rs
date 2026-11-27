//! Configuration resolution: CLI flags, process environment and the
//! knot-lite `.env` file, merged by a pure, injectable resolver.
//!
//! Precedence (P1-D7): CLI > process environment > `.env` > default. The
//! process environment is never mutated (P1-D10): the `.env` file is read
//! into a map and merged by [`resolve`].
//!
//! **Every** variable is read through one lookup ([`layered`]), so the rule
//! is the same for all of them: the process environment first, then the
//! `.env` file, and a value that is empty or blank counts as unset at each
//! level. clap reads no environment variable. The one exception is
//! [`CONFIG_DIR_ENV`], which says where the `.env` file is and can therefore
//! only come from the process environment. The README table of variables is
//! checked against this module by `documented_variables_are_the_ones_read`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::embed::settings::{EmbedSettings, resolve_embed_settings};
use crate::lsp::settings::{LspSettings, resolve_lsp_settings};

/// Fallback repository name when the path has no final component (knot's fallback).
pub const UNNAMED_REPO: &str = "unnamed-repo";

/// Key of the `.env` directory override.
pub const CONFIG_DIR_ENV: &str = "KNOT_LITE_CONFIG_DIR";

/// Key of the repository root.
pub const REPO_PATH_ENV: &str = "KNOT_LITE_REPO_PATH";

/// Key of the logical repository name.
pub const REPO_NAME_ENV: &str = "KNOT_LITE_REPO_NAME";

/// Key of the MCP server's dry-run switch.
pub const DRY_RUN_ENV: &str = "KNOT_LITE_DRY_RUN";

/// Key of the log filter (`tracing`'s directive syntax).
pub const LOG_ENV: &str = "RUST_LOG";

/// The lookup every setting goes through: the process environment (`env`),
/// then the knot-lite `.env` file (`dotenv`). A value that is empty or blank
/// counts as unset in either, so `KEY=` never hides the `.env` value or the
/// default.
pub fn layered<'a>(
    env: &'a dyn Fn(&str) -> Option<String>,
    dotenv: &'a BTreeMap<String, String>,
) -> impl Fn(&str) -> Option<String> + 'a {
    let set = |value: &String| !value.trim().is_empty();
    move |key: &str| {
        env(key)
            .filter(set)
            .or_else(|| dotenv.get(key).filter(|value| set(value)).cloned())
    }
}

/// The value of a boolean switch: `on|off`, `true|false`, `yes|no`, `1|0`
/// and their initials `y|n`, `t|f` (trimmed, case-insensitive); `None` for
/// anything else.
pub fn parse_switch(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" | "y" | "t" => Some(true),
        "off" | "false" | "0" | "no" | "n" | "f" => Some(false),
        _ => None,
    }
}

/// The process environment over the real knot-lite `.env` file: what the
/// binaries read the settings from that are needed before a [`Config`]
/// exists (the log filter, the dry-run switch).
#[derive(Debug, Clone, Default)]
pub struct EnvLayers {
    dotenv: BTreeMap<String, String>,
}

impl EnvLayers {
    /// Read the `.env` file of the real environment (a missing one is empty).
    pub fn load() -> Self {
        let dotenv =
            env_path_from(&process_env).map_or_else(BTreeMap::new, |path| read_dotenv(&path));
        Self { dotenv }
    }

    /// The value of `key`, by the rule of [`layered`].
    pub fn get(&self, key: &str) -> Option<String> {
        layered(&process_env, &self.dotenv)(key)
    }
}

fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Whether the MCP server runs dry: the `--dry-run` flag, else
/// [`DRY_RUN_ENV`] (a [`parse_switch`] value, default off). An invalid value
/// is ignored with the returned warning, as for every other switch.
pub fn resolve_dry_run(
    flag: bool,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> (bool, Option<String>) {
    if flag {
        return (true, None);
    }
    match lookup(DRY_RUN_ENV) {
        None => (false, None),
        Some(raw) => match parse_switch(&raw) {
            Some(value) => (value, None),
            None => (
                false,
                Some(format!(
                    "{DRY_RUN_ENV}='{}' is not a boolean (on|off|true|false|1|0|yes|no); using the default (off)",
                    raw.trim()
                )),
            ),
        },
    }
}

/// Key selecting the ranking of `search_hybrid_context` (P5-D12).
pub const SEARCH_ENV: &str = "KNOT_LITE_SEARCH";

/// How `search_hybrid_context` ranks. `vector` is the default: the hybrid
/// ranking missed gate H1 on the held-out split (P5-D18) and ships opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Recall pool, knot's kind-aware re-rank and the BM25 keyword channel.
    Hybrid,
    /// Cosine only (the 0.4.x ranking).
    #[default]
    Vector,
}

/// The search mode and the warning of an invalid [`SEARCH_ENV`] value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchSettings {
    /// The mode in force.
    pub mode: SearchMode,
    /// One line per invalid value (it falls back to `vector`).
    pub warnings: Vec<String>,
}

/// Resolve [`SEARCH_ENV`]: `hybrid` or `vector` (trimmed, case-insensitive);
/// unset or empty means `vector`; anything else warns and uses `vector`.
pub fn resolve_search_settings(lookup: &dyn Fn(&str) -> Option<String>) -> SearchSettings {
    let raw = lookup(SEARCH_ENV).unwrap_or_default();
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "vector" => SearchSettings::default(),
        "hybrid" => SearchSettings {
            mode: SearchMode::Hybrid,
            warnings: Vec::new(),
        },
        _ => SearchSettings {
            mode: SearchMode::Vector,
            warnings: vec![format!(
                "{SEARCH_ENV} = '{}' is not 'hybrid' or 'vector'; using vector",
                raw.trim()
            )],
        },
    }
}

/// Output rendering shared by the CLI subcommands.
#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Human-readable tables (CLI default).
    #[default]
    Table,
    /// Pretty-printed JSON.
    Json,
    /// GitHub-flavoured Markdown (used by MCP).
    Markdown,
}

/// Flags shared by both binaries (flattened into their clap structs).
///
/// clap reads no environment variable here: [`resolve`] falls back to
/// `KNOT_LITE_REPO_PATH` / `KNOT_LITE_REPO_NAME` through [`layered`], like
/// every other setting.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct CommonArgs {
    /// Repository root [env: KNOT_LITE_REPO_PATH]. Defaults to the current working directory.
    #[arg(long, global = true)]
    pub repo_path: Option<String>,
    /// Logical repository name [env: KNOT_LITE_REPO_NAME]. Defaults to the last component of the repository path.
    #[arg(long, global = true)]
    pub repo_name: Option<String>,
}

/// Language-server overrides: program name or path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LspOverrides {
    /// Override for `rust-analyzer` (`KNOT_LITE_LSP_RUST`).
    pub rust: Option<String>,
    /// Override for `typescript-language-server` (`KNOT_LITE_LSP_TS`).
    pub ts: Option<String>,
    /// Override for `jdtls` (`KNOT_LITE_LSP_JAVA`).
    pub java: Option<String>,
}

/// Fully resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Repository root (canonical and absolute, except in dry-run mode).
    pub repo_path: PathBuf,
    /// Logical repository name.
    pub repo_name: String,
    /// Language-server overrides.
    pub lsp_overrides: LspOverrides,
    /// Whether the server answers without opening a repository (MCP only).
    pub dry_run: bool,
    /// Embedding model and cache directory (P2-D11); the model name is not validated here.
    pub embed: EmbedSettings,
    /// Language-server switches, limits and scratch root (P3-D12, P3-D14).
    pub lsp: LspSettings,
    /// Ranking of `search_hybrid_context` (P5-D12).
    pub search: SearchSettings,
}

/// Everything [`resolve`] reads besides the CLI arguments, injected so the
/// resolver stays pure.
pub struct ResolveInputs<'a> {
    /// Process-environment lookup.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Key/values read from the knot-lite `.env` file.
    pub dotenv: &'a BTreeMap<String, String>,
    /// Current working directory (the default repository path).
    pub cwd: &'a Path,
    /// System temporary directory (last-resort embedding cache location).
    pub temp_dir: &'a Path,
}

impl Config {
    /// Load the configuration for the CLI binary against the real environment,
    /// the real `.env` file and the real current working directory.
    pub fn load_cli(args: &CommonArgs) -> Result<Self> {
        load(args, false)
    }

    /// Load the configuration for the MCP server binary. In dry-run mode the
    /// repository path is kept as given and never touched on disk.
    pub fn load_mcp(args: &CommonArgs, dry_run: bool) -> Result<Self> {
        load(args, dry_run)
    }
}

fn load(args: &CommonArgs, dry_run: bool) -> Result<Config> {
    let env = process_env;
    let EnvLayers { dotenv } = EnvLayers::load();
    let cwd = std::env::current_dir().context("failed to read the current working directory")?;
    let temp_dir = std::env::temp_dir();
    resolve(
        args,
        dry_run,
        &ResolveInputs {
            env: &env,
            dotenv: &dotenv,
            cwd: &cwd,
            temp_dir: &temp_dir,
        },
    )
}

/// Resolve the effective configuration.
///
/// `inputs.env` is the process-environment lookup; `inputs.dotenv` holds the
/// key/values read from the knot-lite `.env` file. `args` holds the CLI flags
/// only; whatever they leave unset comes from [`layered`]: process
/// environment, then `.env`, blank values skipped.
pub fn resolve(args: &CommonArgs, dry_run: bool, inputs: &ResolveInputs<'_>) -> Result<Config> {
    let ResolveInputs {
        env,
        dotenv,
        cwd,
        temp_dir,
    } = *inputs;
    let lookup = layered(env, dotenv);

    let path_choice = args.repo_path.clone().or_else(|| lookup(REPO_PATH_ENV));
    let repo_path = resolve_repo_path(path_choice.as_deref(), dry_run, cwd)?;

    let name_choice = args.repo_name.clone().or_else(|| lookup(REPO_NAME_ENV));
    let repo_name = resolve_repo_name(name_choice.as_deref(), &repo_path);

    Ok(Config {
        repo_path,
        repo_name,
        lsp_overrides: LspOverrides {
            rust: lookup("KNOT_LITE_LSP_RUST"),
            ts: lookup("KNOT_LITE_LSP_TS"),
            java: lookup("KNOT_LITE_LSP_JAVA"),
        },
        dry_run,
        embed: resolve_embed_settings(&lookup, temp_dir),
        lsp: resolve_lsp_settings(&lookup, temp_dir),
        search: resolve_search_settings(&lookup),
    })
}

fn resolve_repo_path(choice: Option<&str>, dry_run: bool, cwd: &Path) -> Result<PathBuf> {
    let path = choice.map_or_else(|| cwd.to_path_buf(), PathBuf::from);
    if dry_run {
        return Ok(path);
    }
    let canonical = std::fs::canonicalize(&path).map_err(|_| {
        anyhow::anyhow!(
            "repository path '{}' does not exist or is not a directory",
            path.display()
        )
    })?;
    if !canonical.is_dir() {
        anyhow::bail!(
            "repository path '{}' does not exist or is not a directory",
            path.display()
        );
    }
    Ok(canonical)
}

fn resolve_repo_name(choice: Option<&str>, repo_path: &Path) -> String {
    choice.map_or_else(
        || {
            repo_path.file_name().map_or_else(
                || UNNAMED_REPO.to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        },
        str::to_string,
    )
}

/// Return the path of the knot-lite `.env` file: `KNOT_LITE_CONFIG_DIR/.env`,
/// then `$HOME/.config/knot-lite/.env`, then `$USERPROFILE/.config/knot-lite/.env`.
/// Never the current working directory: an empty or blank value counts as
/// unset (an empty [`CONFIG_DIR_ENV`] would otherwise name `./.env`).
pub fn env_path_from(get: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |key: &str| get(key).filter(|value| !value.trim().is_empty());
    if let Some(dir) = set(CONFIG_DIR_ENV) {
        return Some(PathBuf::from(dir).join(".env"));
    }
    ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(set)
        .map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("knot-lite")
                .join(".env")
        })
}

/// Read a `.env` file into a map. A missing file is not an error; any other
/// failure yields an empty map (knot's behaviour). The process environment is
/// never modified (P1-D10).
pub fn read_dotenv(path: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let iter = match dotenvy::from_path_iter(path) {
        Ok(iter) => iter,
        Err(e) if e.not_found() => {
            tracing::debug!(path = %path.display(), "no knot-lite .env file");
            return map;
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read knot-lite .env file");
            return map;
        }
    };
    for entry in iter {
        match entry {
            Ok((key, value)) => {
                map.insert(key, value);
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "ignoring malformed .env entry");
            }
        }
    }
    map
}

/// Shared fixtures for tests that need a [`Config`] without resolving one.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use super::{Config, LspOverrides, SearchSettings};
    use crate::embed::settings::EmbedSettings;
    use crate::lsp::settings::{LspSettings, resolve_lsp_settings};

    /// A non-dry-run config named `demo` rooted at `root`. Language servers are
    /// **off**: nothing reachable by the test suite may start a real one (P3-D13).
    /// Tests that need a pool inject a fake one.
    pub(crate) fn config_for(root: PathBuf) -> Config {
        let temp_dir = std::env::temp_dir();
        Config {
            repo_name: "demo".to_string(),
            repo_path: root,
            lsp_overrides: LspOverrides::default(),
            dry_run: false,
            embed: EmbedSettings {
                model: "AllMiniLML6V2".to_string(),
                cache_dir: temp_dir.join("knot-lite-test-fastembed"),
            },
            lsp: LspSettings {
                enabled: false,
                ..resolve_lsp_settings(&|_| None, &temp_dir)
            },
            search: SearchSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve with a map-backed environment, a fixed temp dir and no `.env`.
    fn resolve_with(
        args: &CommonArgs,
        dry_run: bool,
        env: &[(&str, &str)],
        dotenv: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<Config> {
        let env = env_of(env);
        resolve(
            args,
            dry_run,
            &ResolveInputs {
                env: &env,
                dotenv,
                cwd,
                temp_dir: Path::new("/tmp"),
            },
        )
    }

    /// Environment lookup backed by a map, never by the process environment.
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| owned.get(key).cloned()
    }

    fn dotenv_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn empty_args() -> CommonArgs {
        CommonArgs::default()
    }

    #[test]
    fn env_path_prefers_config_dir() {
        let get = env_of(&[
            ("KNOT_LITE_CONFIG_DIR", "/custom/cfg"),
            ("HOME", "/home/u"),
            ("USERPROFILE", "/win/u"),
        ]);
        assert_eq!(
            env_path_from(&get),
            Some(PathBuf::from("/custom/cfg").join(".env"))
        );
    }

    #[test]
    fn env_path_falls_back_to_home() {
        let get = env_of(&[("HOME", "/home/u"), ("USERPROFILE", "/win/u")]);
        assert_eq!(
            env_path_from(&get),
            Some(PathBuf::from("/home/u/.config/knot-lite/.env"))
        );
    }

    #[test]
    fn env_path_falls_back_to_userprofile() {
        let get = env_of(&[("USERPROFILE", "/win/u")]);
        assert_eq!(
            env_path_from(&get),
            Some(PathBuf::from("/win/u/.config/knot-lite/.env"))
        );
    }

    #[test]
    fn env_path_none_when_nothing_set() {
        let get = env_of(&[]);
        assert_eq!(env_path_from(&get), None);
    }

    #[test]
    fn env_path_never_cwd() {
        // No environment keys at all: the lookup must not invent a cwd-relative path.
        let get = env_of(&[]);
        assert!(env_path_from(&get).is_none());
    }

    #[test]
    fn resolve_uses_cwd_when_nothing_given() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg =
            resolve_with(&empty_args(), false, &[], &BTreeMap::new(), tmp.path()).expect("resolve");
        assert_eq!(
            cfg.repo_path,
            std::fs::canonicalize(tmp.path()).expect("canon")
        );
    }

    #[test]
    fn resolve_dotenv_used_when_args_and_env_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dotenv = dotenv_of(&[("KNOT_LITE_REPO_NAME", "from-dotenv")]);
        let cfg = resolve_with(&empty_args(), false, &[], &dotenv, tmp.path()).expect("resolve");
        assert_eq!(cfg.repo_name, "from-dotenv");
    }

    #[test]
    fn resolve_process_env_beats_dotenv_for_lsp_overrides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dotenv = dotenv_of(&[
            ("KNOT_LITE_LSP_RUST", "dotenv-rust"),
            ("KNOT_LITE_LSP_TS", "dotenv-ts"),
        ]);
        let env = [("KNOT_LITE_LSP_RUST", "env-rust")];
        let cfg = resolve_with(&empty_args(), false, &env, &dotenv, tmp.path()).expect("resolve");
        assert_eq!(cfg.lsp_overrides.rust.as_deref(), Some("env-rust"));
        assert_eq!(cfg.lsp_overrides.ts.as_deref(), Some("dotenv-ts"));
        assert_eq!(cfg.lsp_overrides.java, None);
    }

    #[test]
    fn resolve_args_beat_dotenv_for_repo_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let wanted = tempfile::tempdir().expect("tempdir");
        let args = CommonArgs {
            repo_path: Some(wanted.path().to_string_lossy().into_owned()),
            repo_name: None,
        };
        let dotenv = dotenv_of(&[("KNOT_LITE_REPO_PATH", tmp.path().to_string_lossy().as_ref())]);
        let cfg = resolve_with(&args, false, &[], &dotenv, tmp.path()).expect("resolve");
        assert_eq!(
            cfg.repo_path,
            std::fs::canonicalize(wanted.path()).expect("canon")
        );
    }

    #[test]
    fn resolve_errors_on_missing_path() {
        let err = resolve_with(
            &CommonArgs {
                repo_path: Some("/definitely/not/a/real/path".to_string()),
                repo_name: None,
            },
            false,
            &[],
            &BTreeMap::new(),
            Path::new("/"),
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("/definitely/not/a/real/path"));
    }

    #[test]
    fn resolve_dry_run_tolerates_missing_path() {
        let cfg = resolve_with(
            &CommonArgs {
                repo_path: Some("/definitely/not/a/real/path".to_string()),
                repo_name: None,
            },
            true,
            &[],
            &BTreeMap::new(),
            Path::new("/"),
        )
        .expect("dry-run must not touch the filesystem");
        assert_eq!(cfg.repo_path, PathBuf::from("/definitely/not/a/real/path"));
    }

    #[test]
    fn repo_name_defaults_to_dir_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg =
            resolve_with(&empty_args(), false, &[], &BTreeMap::new(), tmp.path()).expect("resolve");
        let dir_name = tmp
            .path()
            .file_name()
            .expect("tempdir has a final component")
            .to_string_lossy()
            .into_owned();
        assert_eq!(cfg.repo_name, dir_name);
    }

    #[test]
    fn repo_name_explicit_wins() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let args = CommonArgs {
            repo_path: None,
            repo_name: Some("explicit".to_string()),
        };
        let cfg = resolve_with(&args, false, &[], &BTreeMap::new(), tmp.path()).expect("resolve");
        assert_eq!(cfg.repo_name, "explicit");
    }

    #[test]
    fn resolve_fills_embed_settings_from_dotenv() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dotenv = dotenv_of(&[
            ("KNOT_LITE_EMBED_MODEL", "BGEBaseENV15"),
            ("KNOT_LITE_FASTEMBED_CACHE_DIR", "/models"),
        ]);
        let cfg = resolve_with(&empty_args(), false, &[], &dotenv, tmp.path()).expect("resolve");
        assert_eq!(cfg.embed.model, "BGEBaseENV15");
        assert_eq!(cfg.embed.cache_dir, PathBuf::from("/models"));
    }

    #[test]
    fn resolve_embed_defaults_use_knots_model_and_the_injected_temp_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg =
            resolve_with(&empty_args(), false, &[], &BTreeMap::new(), tmp.path()).expect("resolve");
        assert_eq!(cfg.embed.model, "AllMiniLML6V2");
        assert_eq!(cfg.embed.cache_dir, Path::new("/tmp/knot-lite-fastembed"));
    }

    #[test]
    fn resolve_fills_lsp_settings_from_dotenv() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dotenv = dotenv_of(&[
            ("KNOT_LITE_LSP", "off"),
            ("KNOT_LITE_LSP_BUILD_EXEC", "no"),
            ("KNOT_LITE_LSP_CACHE_DIR", "/scratch"),
            ("KNOT_LITE_LSP_DEADLINE_SECS", "45"),
        ]);
        let cfg = resolve_with(&empty_args(), false, &[], &dotenv, tmp.path()).expect("resolve");
        assert!(!cfg.lsp.enabled);
        assert!(!cfg.lsp.build_exec);
        assert_eq!(cfg.lsp.cache_root, PathBuf::from("/scratch"));
        assert_eq!(cfg.lsp.deadline, std::time::Duration::from_secs(45));
        assert!(cfg.lsp.warnings.is_empty());
    }

    #[test]
    fn resolve_process_env_beats_dotenv_for_lsp_settings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dotenv = dotenv_of(&[("KNOT_LITE_LSP", "off")]);
        let env = [("KNOT_LITE_LSP", "on")];
        let cfg = resolve_with(&empty_args(), false, &env, &dotenv, tmp.path()).expect("resolve");
        assert!(cfg.lsp.enabled);
    }

    #[test]
    fn layered_skips_blank_values_at_each_level() {
        let dotenv = dotenv_of(&[("A", "dotenv-a"), ("B", " "), ("C", "dotenv-c")]);
        let env = env_of(&[("A", ""), ("B", "  "), ("C", "env-c")]);
        let lookup = layered(&env, &dotenv);
        assert_eq!(lookup("A").as_deref(), Some("dotenv-a"));
        assert_eq!(lookup("B"), None);
        assert_eq!(lookup("C").as_deref(), Some("env-c"));
        assert_eq!(lookup("D"), None);
    }

    /// An empty variable is an unset one, for the keys clap used to read too
    /// (#37 and its siblings: an empty repository path, name or override).
    #[test]
    fn blank_process_values_fall_back_to_dotenv_then_to_the_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blank = [
            (REPO_PATH_ENV, ""),
            (REPO_NAME_ENV, " "),
            ("KNOT_LITE_LSP_RUST", ""),
            ("KNOT_LITE_LSP", ""),
        ];
        let dotenv = dotenv_of(&[
            (REPO_NAME_ENV, "from-dotenv"),
            ("KNOT_LITE_LSP_RUST", "dotenv-rust"),
            ("KNOT_LITE_LSP", "off"),
        ]);
        let cfg = resolve_with(&empty_args(), false, &blank, &dotenv, tmp.path()).expect("resolve");
        assert_eq!(
            cfg.repo_path,
            std::fs::canonicalize(tmp.path()).expect("canon")
        );
        assert_eq!(cfg.repo_name, "from-dotenv");
        assert_eq!(cfg.lsp_overrides.rust.as_deref(), Some("dotenv-rust"));
        assert!(!cfg.lsp.enabled);

        let cfg = resolve_with(&empty_args(), false, &blank, &BTreeMap::new(), tmp.path())
            .expect("resolve");
        assert_ne!(cfg.repo_name, "");
        assert_eq!(cfg.lsp_overrides.rust, None);
        assert!(cfg.lsp.enabled);
    }

    #[test]
    fn env_path_ignores_blank_directories() {
        let get = env_of(&[
            ("KNOT_LITE_CONFIG_DIR", ""),
            ("HOME", " "),
            ("USERPROFILE", "/win/u"),
        ]);
        assert_eq!(
            env_path_from(&get),
            Some(PathBuf::from("/win/u/.config/knot-lite/.env"))
        );
        let get = env_of(&[("KNOT_LITE_CONFIG_DIR", ""), ("HOME", "")]);
        assert_eq!(env_path_from(&get), None);
    }

    #[test]
    fn switch_spellings() {
        for raw in ["on", "ON", " true ", "1", "yes", "Y", "t"] {
            assert_eq!(parse_switch(raw), Some(true), "{raw}");
        }
        for raw in ["off", "False", "0", "no", "n", "F"] {
            assert_eq!(parse_switch(raw), Some(false), "{raw}");
        }
        for raw in ["", " ", "2", "enable", "onn"] {
            assert_eq!(parse_switch(raw), None, "{raw}");
        }
    }

    /// The dry-run switch follows the precedence of every other variable
    /// (#31, #35, #37): flag, process environment, `.env`, default.
    #[test]
    fn dry_run_precedence_and_spellings() {
        let run = |flag: bool, env: &[(&str, &str)], dotenv: &[(&str, &str)]| {
            let env = env_of(env);
            let dotenv = dotenv_of(dotenv);
            resolve_dry_run(flag, &layered(&env, &dotenv))
        };
        assert_eq!(run(false, &[], &[]), (false, None));
        assert_eq!(run(true, &[(DRY_RUN_ENV, "off")], &[]), (true, None));
        assert_eq!(run(false, &[(DRY_RUN_ENV, "on")], &[]), (true, None));
        assert_eq!(run(false, &[], &[(DRY_RUN_ENV, "1")]), (true, None));
        assert_eq!(
            run(false, &[(DRY_RUN_ENV, "off")], &[(DRY_RUN_ENV, "on")]),
            (false, None)
        );
        // Empty in the process environment: unset, so the `.env` value or
        // the default applies.
        assert_eq!(run(false, &[(DRY_RUN_ENV, "")], &[]), (false, None));
        assert_eq!(
            run(false, &[(DRY_RUN_ENV, "")], &[(DRY_RUN_ENV, "yes")]),
            (true, None)
        );
        let (value, warning) = run(false, &[(DRY_RUN_ENV, "maybe")], &[]);
        assert!(!value);
        assert!(
            warning
                .expect("a warning")
                .contains("KNOT_LITE_DRY_RUN='maybe'"),
            "names the value"
        );
    }

    /// The variables of the README's Configuration table.
    fn documented_variables() -> Vec<String> {
        let readme = include_str!("../README.md");
        let section = readme
            .split("\n## Configuration\n")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .expect("the README has a Configuration section");
        section
            .lines()
            .filter_map(|line| line.strip_prefix("| `"))
            .filter_map(|rest| rest.split('`').next())
            .map(str::to_owned)
            .collect()
    }

    /// Drift guard for the whole class of #35: the README's table and the
    /// keys read through the layered lookup are the same set, so a variable
    /// cannot be documented yet read some other way (clap's `env`, the
    /// process environment alone), nor read without being documented.
    #[test]
    fn documented_variables_are_the_ones_read() {
        use std::cell::RefCell;
        use std::collections::BTreeSet;

        let tmp = tempfile::tempdir().expect("tempdir");
        let asked: RefCell<BTreeSet<String>> = RefCell::new(BTreeSet::new());
        let env = |key: &str| -> Option<String> {
            asked.borrow_mut().insert(key.to_owned());
            None
        };
        let dotenv = BTreeMap::new();
        resolve(
            &empty_args(),
            false,
            &ResolveInputs {
                env: &env,
                dotenv: &dotenv,
                cwd: tmp.path(),
                temp_dir: Path::new("/tmp"),
            },
        )
        .expect("resolve");
        let lookup = layered(&env, &dotenv);
        resolve_dry_run(false, &lookup);
        lookup(LOG_ENV);
        env_path_from(&env);

        // Not settings of their own: the home directory, and knot's variables
        // the table names as fallbacks.
        let fallbacks = [
            "HOME",
            "USERPROFILE",
            crate::embed::settings::ENV_MODEL_FALLBACK,
            crate::embed::settings::ENV_CACHE_FALLBACK,
        ];
        let read: BTreeSet<String> = asked
            .borrow()
            .iter()
            .filter(|key| !fallbacks.contains(&key.as_str()))
            .cloned()
            .collect();
        let documented: BTreeSet<String> = documented_variables().into_iter().collect();
        assert!(
            documented.len() >= 20,
            "the table was parsed: {documented:?}"
        );
        assert_eq!(read, documented);
    }

    /// A `.env` file that sets every documented variable but the one that
    /// locates it, for the repository at `repo`.
    fn full_dotenv(repo: &str) -> BTreeMap<String, String> {
        let dotenv = dotenv_of(&[
            (REPO_PATH_ENV, repo),
            (REPO_NAME_ENV, "named"),
            ("KNOT_LITE_EMBED_MODEL", "BGEBaseENV15"),
            ("KNOT_LITE_FASTEMBED_CACHE_DIR", "/models"),
            ("KNOT_LITE_SEARCH", "hybrid"),
            ("KNOT_LITE_LSP", "off"),
            ("KNOT_LITE_LSP_BUILD_EXEC", "off"),
            ("KNOT_LITE_LSP_SYNC", "open"),
            ("KNOT_LITE_LSP_CACHE_DIR", "/scratch"),
            ("KNOT_LITE_LSP_READY_WAIT_SECS", "7"),
            ("KNOT_LITE_LSP_IDLE_SECS", "8"),
            ("KNOT_LITE_LSP_MAX_LIVE_TARGETS", "9"),
            ("KNOT_LITE_LSP_DEADLINE_SECS", "11"),
            ("KNOT_LITE_LSP_WARMUP", "on"),
            ("KNOT_LITE_LSP_RUST", "ra"),
            ("KNOT_LITE_LSP_TS", "tsls"),
            ("KNOT_LITE_LSP_JAVA", "jdt"),
            (DRY_RUN_ENV, "on"),
            (LOG_ENV, "debug"),
        ]);
        let mut expected: Vec<&str> = dotenv.keys().map(String::as_str).collect();
        expected.push(CONFIG_DIR_ENV);
        expected.sort_unstable();
        let mut documented = documented_variables();
        documented.sort_unstable();
        assert_eq!(documented, expected, "a new variable needs a row here");
        dotenv
    }

    /// The language-server settings [`full_dotenv`] asks for.
    fn assert_full_dotenv_lsp(cfg: &Config) {
        let secs = std::time::Duration::from_secs;
        assert!(!cfg.lsp.enabled);
        assert!(!cfg.lsp.build_exec);
        assert_eq!(cfg.lsp.sync, crate::lsp::sync::SyncMode::Open);
        assert_eq!(cfg.lsp.cache_root, PathBuf::from("/scratch"));
        assert_eq!(cfg.lsp.ready_wait, Some(secs(7)));
        assert_eq!(cfg.lsp.idle, secs(8));
        assert_eq!(cfg.lsp.max_live_targets, 9);
        assert_eq!(cfg.lsp.deadline, secs(11));
        assert!(cfg.lsp.warmup);
        assert_eq!(cfg.lsp_overrides.rust.as_deref(), Some("ra"));
        assert_eq!(cfg.lsp_overrides.ts.as_deref(), Some("tsls"));
        assert_eq!(cfg.lsp_overrides.java.as_deref(), Some("jdt"));
        assert!(cfg.lsp.warnings.is_empty(), "{:?}", cfg.lsp.warnings);
    }

    /// Every documented variable is honoured from the `.env` file alone, and
    /// an empty process value does not hide it (#35, #37).
    #[test]
    fn every_documented_variable_is_read_from_dotenv_under_a_blank_process_value() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dotenv = full_dotenv(&tmp.path().to_string_lossy());
        let env = |_: &str| Some(String::new());
        let cfg = resolve(
            &empty_args(),
            false,
            &ResolveInputs {
                env: &env,
                dotenv: &dotenv,
                cwd: Path::new("/"),
                temp_dir: Path::new("/tmp"),
            },
        )
        .expect("resolve");
        assert_eq!(
            cfg.repo_path,
            std::fs::canonicalize(tmp.path()).expect("canon")
        );
        assert_eq!(cfg.repo_name, "named");
        assert_eq!(cfg.embed.model, "BGEBaseENV15");
        assert_eq!(cfg.embed.cache_dir, PathBuf::from("/models"));
        assert_eq!(cfg.search.mode, SearchMode::Hybrid);
        assert_full_dotenv_lsp(&cfg);
        let lookup = layered(&env, &dotenv);
        assert_eq!(resolve_dry_run(false, &lookup), (true, None));
        assert_eq!(lookup(LOG_ENV).as_deref(), Some("debug"));
    }

    #[test]
    fn read_dotenv_missing_file_is_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let map = read_dotenv(&tmp.path().join("nope.env"));
        assert!(map.is_empty());
    }

    #[test]
    fn read_dotenv_parses_pairs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join(".env");
        std::fs::write(&file, "KNOT_LITE_REPO_NAME=x\nKNOT_LITE_LSP_JAVA=jdtls\n").expect("write");
        let map = read_dotenv(&file);
        assert_eq!(
            map.get("KNOT_LITE_REPO_NAME").map(String::as_str),
            Some("x")
        );
        assert_eq!(
            map.get("KNOT_LITE_LSP_JAVA").map(String::as_str),
            Some("jdtls")
        );
    }

    #[test]
    fn search_mode_env_resolution() {
        let with = |value: Option<&str>| {
            let value = value.map(str::to_owned);
            resolve_search_settings(&move |key: &str| {
                (key == SEARCH_ENV).then(|| value.clone()).flatten()
            })
        };
        assert_eq!(with(None), SearchSettings::default());
        assert_eq!(with(None).mode, SearchMode::Vector);
        assert_eq!(with(Some("  ")).mode, SearchMode::Vector);
        assert_eq!(with(Some("Vector")).mode, SearchMode::Vector);
        let hybrid = with(Some(" HYBRID "));
        assert_eq!(hybrid.mode, SearchMode::Hybrid);
        assert!(hybrid.warnings.is_empty());
        let bad = with(Some("bm25"));
        assert_eq!(bad.mode, SearchMode::Vector);
        assert_eq!(
            bad.warnings,
            vec!["KNOT_LITE_SEARCH = 'bm25' is not 'hybrid' or 'vector'; using vector"]
        );
    }
}
