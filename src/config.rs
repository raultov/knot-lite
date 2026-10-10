//! Configuration resolution: CLI flags, process environment and the
//! knot-lite `.env` file, merged by a pure, injectable resolver.
//!
//! Precedence (P1-D7): CLI (already merged into `args` by clap, which also
//! sees the process environment) > process environment > `.env` > default.
//! The process environment is never mutated (P1-D10): the `.env` file is read
//! into a map and merged by [`resolve`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::embed::settings::{EmbedSettings, resolve_embed_settings};
use crate::lsp::settings::{LspSettings, resolve_lsp_settings};

/// Fallback repository name when the path has no final component (knot's fallback).
pub const UNNAMED_REPO: &str = "unnamed-repo";

/// Key of the `.env` directory override.
pub const CONFIG_DIR_ENV: &str = "KNOT_LITE_CONFIG_DIR";

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
#[derive(Debug, Clone, Default, clap::Args)]
pub struct CommonArgs {
    /// Repository root. Defaults to the current working directory.
    #[arg(long, global = true, env = "KNOT_LITE_REPO_PATH")]
    pub repo_path: Option<String>,
    /// Logical repository name. Defaults to the last component of the repository path.
    #[arg(long, global = true, env = "KNOT_LITE_REPO_NAME")]
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
    let env = |key: &str| std::env::var(key).ok();
    let dotenv_path = env_path_from(&env);
    let dotenv = dotenv_path
        .as_deref()
        .map_or_else(BTreeMap::new, read_dotenv);
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
/// key/values read from the knot-lite `.env` file. `args` already contains the
/// CLI flags merged with the process environment (clap's `env` attribute), so
/// `repo_path` and `repo_name` fall back to `dotenv` only when `args` is
/// `None`. The keys clap does not see (`KNOT_LITE_LSP_*`, the embedding keys)
/// use `lookup`: process env, then `.env`.
pub fn resolve(args: &CommonArgs, dry_run: bool, inputs: &ResolveInputs<'_>) -> Result<Config> {
    let ResolveInputs {
        env,
        dotenv,
        cwd,
        temp_dir,
    } = *inputs;
    let lookup = |key: &str| -> Option<String> { env(key).or_else(|| dotenv.get(key).cloned()) };

    let path_choice = args
        .repo_path
        .clone()
        .or_else(|| dotenv.get("KNOT_LITE_REPO_PATH").cloned());
    let repo_path = resolve_repo_path(path_choice.as_deref(), dry_run, cwd)?;

    let name_choice = args
        .repo_name
        .clone()
        .or_else(|| dotenv.get("KNOT_LITE_REPO_NAME").cloned());
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
/// Never the current working directory.
pub fn env_path_from(get: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(dir) = get(CONFIG_DIR_ENV) {
        return Some(PathBuf::from(dir).join(".env"));
    }
    if let Some(home) = get("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("knot-lite")
                .join(".env"),
        );
    }
    if let Some(profile) = get("USERPROFILE") {
        return Some(
            PathBuf::from(profile)
                .join(".config")
                .join("knot-lite")
                .join(".env"),
        );
    }
    None
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
