//! Language-server registry: server programs, overrides and install hints.
//!
//! Precedence is explicit (Phase 0 G16): an override is set → [`LspStatus::Override`]
//! or [`LspStatus::OverrideInvalid`] with **no** PATH fallback (so typos stay
//! visible); without an override the PATH is searched → [`LspStatus::Found`] /
//! [`LspStatus::Missing`].

use std::ffi::OsStr;
use std::path::PathBuf;

use serde::Serialize;

use super::languages::LspLang;
use super::which::{is_executable, path_dir_count, which_in};
use crate::config::LspOverrides;

/// Result of a language-server lookup, as reported by `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LspStatus {
    /// The default server program was found on `PATH`.
    Found {
        /// Server program name.
        server: String,
        /// Absolute path of the executable found.
        path: String,
    },
    /// An override resolved to an executable.
    Override {
        /// Server program name.
        server: String,
        /// Absolute path of the executable found.
        path: String,
    },
    /// An override was set but does not resolve to an executable.
    OverrideInvalid {
        /// Server program name.
        server: String,
        /// The override value that failed to resolve.
        value: String,
        /// Human-readable hint.
        hint: String,
    },
    /// No server binary was found.
    Missing {
        /// Server program name.
        server: String,
        /// Number of `PATH` directories searched.
        searched_dirs: usize,
        /// Human-readable install hint.
        hint: String,
    },
}

/// Human-readable install hint per language.
pub fn install_hint(lang: LspLang) -> &'static str {
    match lang {
        LspLang::Rust => "Install it with `rustup component add rust-analyzer`.",
        LspLang::Ts => "Install it with `npm install -g typescript-language-server typescript`.",
        LspLang::Java => {
            "Install Eclipse JDT Language Server (jdtls) 1.61+ from https://download.eclipse.org/jdtls/milestones/ and put its bin/ directory on PATH."
        }
    }
}

/// Hint shown when an override does not resolve to an executable.
pub const OVERRIDE_INVALID_HINT: &str = "The override does not resolve to an executable. Point it at a program name on PATH or at the executable's full path.";

/// Real-environment wrapper around [`detect_with`].
pub fn detect(lang: LspLang, overrides: &LspOverrides) -> LspStatus {
    let path_var = std::env::var_os("PATH");
    let pathext = std::env::var_os("PATHEXT");
    let windows = cfg!(windows);
    detect_with(lang, overrides, path_var.as_deref(), &|program| {
        which_in(
            program,
            path_var.as_deref(),
            pathext.as_deref(),
            windows,
            &is_executable,
        )
    })
}

/// Pure lookup: `lookup` resolves a program name to an executable path
/// (see [`which_in`]); `path_var` is only used to report `searched_dirs`.
///
/// (Phase 1 deviation from the plan's sketch: the plan's
/// `detect_with(lang, overrides, path_var, pathext, windows, is_exec)` has six
/// parameters and would trip `clippy::too_many_arguments` at the repository's
/// threshold of 5, so the lookup parameters are collapsed into the `lookup`
/// closure. See PLAN_PHASE1_SKELETON.md Appendix D.)
pub fn detect_with(
    lang: LspLang,
    overrides: &LspOverrides,
    path_var: Option<&OsStr>,
    lookup: &dyn Fn(&str) -> Option<PathBuf>,
) -> LspStatus {
    let server = lang.server_program().to_string();
    let override_value = match lang {
        LspLang::Rust => overrides.rust.clone(),
        LspLang::Ts => overrides.ts.clone(),
        LspLang::Java => overrides.java.clone(),
    };
    if let Some(value) = override_value {
        return match lookup(&value) {
            Some(path) => LspStatus::Override {
                server,
                path: path.display().to_string(),
            },
            None => LspStatus::OverrideInvalid {
                server,
                value,
                hint: OVERRIDE_INVALID_HINT.to_string(),
            },
        };
    }
    match lookup(lang.server_program()) {
        Some(path) => LspStatus::Found {
            server,
            path: path.display().to_string(),
        },
        None => LspStatus::Missing {
            server,
            searched_dirs: path_dir_count(path_var),
            hint: install_hint(lang).to_string(),
        },
    }
}

/// Helper for callers that want the default [`which_in`] binding without
/// spelling out every parameter.
pub fn default_lookup<'a>(
    path_var: Option<&'a OsStr>,
    pathext: Option<&'a OsStr>,
    windows: bool,
) -> impl Fn(&str) -> Option<PathBuf> + 'a {
    move |program| which_in(program, path_var, pathext, windows, &is_executable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, b"stub").expect("write");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    fn no_overrides() -> LspOverrides {
        LspOverrides::default()
    }

    #[test]
    fn found_on_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let found = touch(tmp.path(), "rust-analyzer");
        let path_var = std::env::join_paths([tmp.path()]).expect("join");
        let status = detect_with(
            LspLang::Rust,
            &no_overrides(),
            Some(&path_var),
            &default_lookup(Some(&path_var), None, false),
        );
        assert_eq!(
            status,
            LspStatus::Found {
                server: "rust-analyzer".to_string(),
                path: found.display().to_string(),
            }
        );
    }

    #[test]
    fn missing_reports_searched_dirs_and_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path_var = std::env::join_paths([tmp.path(), tmp.path(), tmp.path()]).expect("join");
        let status = detect_with(
            LspLang::Java,
            &no_overrides(),
            Some(&path_var),
            &default_lookup(Some(&path_var), None, false),
        );
        assert_eq!(
            status,
            LspStatus::Missing {
                server: "jdtls".to_string(),
                searched_dirs: 3,
                hint: install_hint(LspLang::Java).to_string(),
            }
        );
    }

    #[test]
    fn override_valid_wins() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let found = touch(tmp.path(), "my-ts-server");
        let overrides = LspOverrides {
            ts: Some("my-ts-server".to_string()),
            ..LspOverrides::default()
        };
        let path_var = std::env::join_paths([tmp.path()]).expect("join");
        let status = detect_with(
            LspLang::Ts,
            &overrides,
            Some(&path_var),
            &default_lookup(Some(&path_var), None, false),
        );
        assert_eq!(
            status,
            LspStatus::Override {
                server: "typescript-language-server".to_string(),
                path: found.display().to_string(),
            }
        );
    }

    #[test]
    fn override_invalid_has_no_path_fallback() {
        // The default server exists on PATH, but the override is a typo: the
        // typo must be reported instead of silently falling back.
        let tmp = tempfile::tempdir().expect("tempdir");
        touch(tmp.path(), "typescript-language-server");
        let overrides = LspOverrides {
            ts: Some("typo-server".to_string()),
            ..LspOverrides::default()
        };
        let path_var = std::env::join_paths([tmp.path()]).expect("join");
        let status = detect_with(
            LspLang::Ts,
            &overrides,
            Some(&path_var),
            &default_lookup(Some(&path_var), None, false),
        );
        assert_eq!(
            status,
            LspStatus::OverrideInvalid {
                server: "typescript-language-server".to_string(),
                value: "typo-server".to_string(),
                hint: OVERRIDE_INVALID_HINT.to_string(),
            }
        );
    }

    #[test]
    fn windows_cmd_override_resolves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let found = touch(tmp.path(), "tsls.cmd");
        let overrides = LspOverrides {
            ts: Some("tsls".to_string()),
            ..LspOverrides::default()
        };
        let path_var = std::env::join_paths([tmp.path()]).expect("join");
        let status = detect_with(
            LspLang::Ts,
            &overrides,
            Some(&path_var),
            &default_lookup(Some(&path_var), Some(OsStr::new(".cmd;.exe")), true),
        );
        assert_eq!(
            status,
            LspStatus::Override {
                server: "typescript-language-server".to_string(),
                path: found.display().to_string(),
            }
        );
    }

    #[test]
    fn hints_are_stable() {
        assert_eq!(
            install_hint(LspLang::Rust),
            "Install it with `rustup component add rust-analyzer`."
        );
        assert_eq!(
            install_hint(LspLang::Ts),
            "Install it with `npm install -g typescript-language-server typescript`."
        );
        assert!(install_hint(LspLang::Java).contains("1.61+"));
    }
}
