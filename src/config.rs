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

/// Fallback repository name when the path has no final component (knot's fallback).
pub const UNNAMED_REPO: &str = "unnamed-repo";

/// Key of the `.env` directory override.
pub const CONFIG_DIR_ENV: &str = "KNOT_LITE_CONFIG_DIR";

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
    resolve(args, dry_run, &env, &dotenv, &cwd)
}

/// Resolve the effective configuration.
///
/// `env` is the process-environment lookup; `dotenv` holds the key/values read
/// from the knot-lite `.env` file. `args` already contains the CLI flags merged
/// with the process environment (clap's `env` attribute), so `repo_path` and
/// `repo_name` fall back to `dotenv` only when `args` is `None`. The keys clap
/// does not see (`KNOT_LITE_LSP_*`) use [`lookup`]: process env, then `.env`.
pub fn resolve(
    args: &CommonArgs,
    dry_run: bool,
    env: &dyn Fn(&str) -> Option<String>,
    dotenv: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Config> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let cfg = resolve(
            &empty_args(),
            false,
            &env_of(&[]),
            &BTreeMap::new(),
            tmp.path(),
        )
        .expect("resolve");
        assert_eq!(
            cfg.repo_path,
            std::fs::canonicalize(tmp.path()).expect("canon")
        );
    }

    #[test]
    fn resolve_dotenv_used_when_args_and_env_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dotenv = dotenv_of(&[("KNOT_LITE_REPO_NAME", "from-dotenv")]);
        let cfg =
            resolve(&empty_args(), false, &env_of(&[]), &dotenv, tmp.path()).expect("resolve");
        assert_eq!(cfg.repo_name, "from-dotenv");
    }

    #[test]
    fn resolve_process_env_beats_dotenv_for_lsp_overrides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dotenv = dotenv_of(&[
            ("KNOT_LITE_LSP_RUST", "dotenv-rust"),
            ("KNOT_LITE_LSP_TS", "dotenv-ts"),
        ]);
        let env = env_of(&[("KNOT_LITE_LSP_RUST", "env-rust")]);
        let cfg = resolve(&empty_args(), false, &env, &dotenv, tmp.path()).expect("resolve");
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
        let cfg = resolve(&args, false, &env_of(&[]), &dotenv, tmp.path()).expect("resolve");
        assert_eq!(
            cfg.repo_path,
            std::fs::canonicalize(wanted.path()).expect("canon")
        );
    }

    #[test]
    fn resolve_errors_on_missing_path() {
        let err = resolve(
            &CommonArgs {
                repo_path: Some("/definitely/not/a/real/path".to_string()),
                repo_name: None,
            },
            false,
            &env_of(&[]),
            &BTreeMap::new(),
            Path::new("/"),
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("/definitely/not/a/real/path"));
    }

    #[test]
    fn resolve_dry_run_tolerates_missing_path() {
        let cfg = resolve(
            &CommonArgs {
                repo_path: Some("/definitely/not/a/real/path".to_string()),
                repo_name: None,
            },
            true,
            &env_of(&[]),
            &BTreeMap::new(),
            Path::new("/"),
        )
        .expect("dry-run must not touch the filesystem");
        assert_eq!(cfg.repo_path, PathBuf::from("/definitely/not/a/real/path"));
    }

    #[test]
    fn repo_name_defaults_to_dir_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = resolve(
            &empty_args(),
            false,
            &env_of(&[]),
            &BTreeMap::new(),
            tmp.path(),
        )
        .expect("resolve");
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
        let cfg =
            resolve(&args, false, &env_of(&[]), &BTreeMap::new(), tmp.path()).expect("resolve");
        assert_eq!(cfg.repo_name, "explicit");
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
}
