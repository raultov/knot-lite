//! Per-language LSP launch specs (T4.1).

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::common::Lang;
use crate::lsp::readiness::ReadinessKind;

/// How to launch, initialise and detect readiness of one language server.
pub struct ServerSpec {
    pub lang: Lang,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub language_id_for: fn(&Path) -> &'static str,
    pub initialization_options: serde_json::Value,
    pub ready_timeout: Duration,
    pub readiness: ReadinessKind,
    /// Sanitized `PATH` used for lookup and the child process (T8); `None`
    /// means the real environment.
    pub search_path: Option<OsString>,
}

/// Build the launch spec for `lang` over the snapshot at `root`.
///
/// `spike_root` provides the scratch areas (rust-analyzer target dir, jdtls
/// workspace dir) so language-server side effects never touch real repos.
pub fn spec_for(lang: Lang, root: &Path, spike_root: &Path) -> ServerSpec {
    match lang {
        Lang::Rust => ServerSpec {
            lang,
            program: "rust-analyzer".to_owned(),
            args: Vec::new(),
            env: vec![(
                "CARGO_TARGET_DIR".to_owned(),
                spike_root.join("target-ra").to_string_lossy().into_owned(),
            )],
            language_id_for: rust_id,
            initialization_options: json!({
                "checkOnSave": false,
                "cachePriming": { "enable": true },
                "cargo": { "buildScripts": { "enable": true } },
                "procMacro": { "enable": true }
            }),
            ready_timeout: Duration::from_secs(300),
            readiness: ReadinessKind::RustAnalyzerStatus,
            search_path: None,
        },
        Lang::Ts => ServerSpec {
            lang,
            program: "typescript-language-server".to_owned(),
            args: vec!["--stdio".to_owned()],
            env: Vec::new(),
            language_id_for: ts_id,
            initialization_options: json!({
                "tsserver": { "logVerbosity": "off" },
                "maxTsServerMemory": 4096,
                "preferences": { "includeCompletionsForModuleExports": false }
            }),
            ready_timeout: Duration::from_secs(120),
            readiness: ReadinessKind::ProgressIdle,
            search_path: None,
        },
        Lang::Java => ServerSpec {
            lang,
            program: "jdtls".to_owned(),
            args: vec![
                "--jvm-arg=-Xmx8g".to_owned(),
                "-data".to_owned(),
                jdtls_workspace(root, spike_root),
            ],
            env: Vec::new(),
            language_id_for: java_id,
            initialization_options: jdtls_initialization_options(root),
            ready_timeout: Duration::from_secs(900),
            readiness: ReadinessKind::JdtlsServiceReady,
            search_path: None,
        },
    }
}

/// `-data` workspace directory for jdtls (kept under the spike scratch area).
fn jdtls_workspace(root: &Path, spike_root: &Path) -> String {
    let name = root.file_name().map_or_else(
        || "workspace".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    spike_root
        .join("jdtls-ws")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn jdtls_initialization_options(root: &Path) -> serde_json::Value {
    let root_url =
        crate::uri::path_to_uri(root).map_or_else(|_| json!(null), |url| json!(url.as_str()));
    json!({
        "bundles": [],
        "workspaceFolders": [root_url],
        "settings": { "java": {
            "import": {
                "maven": { "enabled": true },
                "gradle": { "enabled": false },
                "exclusions": [
                    "**/node_modules/**",
                    "**/.metadata/**",
                    "**/archetype-resources/**",
                    "**/META-INF/maven/**"
                ]
            },
            "configuration": { "updateBuildConfiguration": "automatic" },
            "autobuild": { "enabled": true },
            "maxConcurrentBuilds": 4,
            "referencesCodeLens": { "enabled": false },
            "implementationsCodeLens": { "enabled": false }
        } },
        "extendedClientCapabilities": {
            "progressReportProvider": true,
            "classFileContentsSupport": false
        }
    })
}

fn rust_id(_path: &Path) -> &'static str {
    "rust"
}

fn java_id(_path: &Path) -> &'static str {
    "java"
}

/// LSP `languageId` for TS/JS files by extension.
fn ts_id(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("tsx") => "typescriptreact",
        Some("jsx") => "javascriptreact",
        Some("js") | Some("mjs") | Some("cjs") => "javascript",
        _ => "typescript",
    }
}

/// PATH lookup of an executable without shelling out.
pub fn which(program: &str) -> Option<PathBuf> {
    which_in(program, env::var_os("PATH").as_deref())
}

/// PATH lookup against an explicit `PATH` value (T8 passes a sanitized one).
pub fn which_in(program: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let paths = path_var?;
    env::split_paths(paths)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// Count of directories a [`which`] lookup would search.
pub fn path_dir_count(path_var: Option<&OsStr>) -> usize {
    path_var.map_or(0, |paths| env::split_paths(paths).count())
}

/// Human-readable install hint per server (T8 diagnostics).
pub fn install_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => "Install it with `rustup component add rust-analyzer`.",
        Lang::Ts => "Install it with `npm install -g typescript-language-server typescript`.",
        Lang::Java => {
            "Install it with `docs/spikes/phase0/scripts/install_jdtls.sh` \
             (https://download.eclipse.org/jdtls/milestones/)."
        }
    }
}

/// `PATH` with every directory that resolves `program` removed (T8). The real
/// environment is never modified: the result is passed explicitly to the
/// lookup and the child command only.
pub fn sanitized_path_without(program: &str) -> OsString {
    let real = env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = env::split_paths(&real).collect();
    // Bounded loop: each pass removes at least one directory.
    for _ in 0..dirs.len().saturating_add(1) {
        let current = join_paths(&dirs);
        let Some(found) = which_in(program, Some(&current)) else {
            break;
        };
        let Some(parent) = found.parent() else {
            break;
        };
        let before = dirs.len();
        dirs.retain(|dir| dir != parent);
        if dirs.len() == before {
            break;
        }
    }
    join_paths(&dirs)
}

fn join_paths(dirs: &[PathBuf]) -> OsString {
    let joined = env::join_paths(dirs);
    joined.unwrap_or_default()
}

fn is_executable(path: &Path) -> bool {
    path.is_file() && has_exec_bit(path)
}

#[cfg(unix)]
fn has_exec_bit(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn has_exec_bit(_path: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_sh_and_rejects_nonsense() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-program-xyz").is_none());
    }

    #[test]
    fn language_ids_cover_the_spiked_languages() {
        assert_eq!(rust_id(Path::new("src/lib.rs")), "rust");
        assert_eq!(java_id(Path::new("A.java")), "java");
        assert_eq!(ts_id(Path::new("src/main.tsx")), "typescriptreact");
        assert_eq!(ts_id(Path::new("src/App.tsx")), "typescriptreact");
        assert_eq!(ts_id(Path::new("src/toolcore/format.ts")), "typescript");
        assert_eq!(ts_id(Path::new("scripts/fetch-updates.mjs")), "javascript");
        assert_eq!(ts_id(Path::new("web/App.jsx")), "javascriptreact");
    }

    #[test]
    fn rust_spec_puts_build_artifacts_in_the_spike_area() {
        let spec = spec_for(Lang::Rust, Path::new("/snap/knot"), Path::new("/spike"));
        assert_eq!(spec.program, "rust-analyzer");
        assert_eq!(spec.readiness, ReadinessKind::RustAnalyzerStatus);
        assert_eq!(
            spec.env,
            vec![("CARGO_TARGET_DIR".to_owned(), "/spike/target-ra".to_owned())]
        );
        assert!((spec.language_id_for)(Path::new("x.rs")) == "rust");
    }

    #[test]
    fn java_spec_uses_the_spike_workspace_and_root_uri() {
        let spec = spec_for(
            Lang::Java,
            Path::new("/snap/work/spring-ai"),
            Path::new("/spike"),
        );
        assert_eq!(spec.program, "jdtls");
        assert!(spec.args.contains(&"--jvm-arg=-Xmx8g".to_owned()));
        assert!(spec.args.contains(&"/spike/jdtls-ws/spring-ai".to_owned()));
        assert_eq!(
            spec.initialization_options["workspaceFolders"][0],
            json!("file:///snap/work/spring-ai")
        );
        assert_eq!(spec.readiness, ReadinessKind::JdtlsServiceReady);
    }

    #[test]
    fn ts_spec_is_stdio_and_lazy() {
        let spec = spec_for(Lang::Ts, Path::new("/snap/knot-site"), Path::new("/spike"));
        assert_eq!(spec.program, "typescript-language-server");
        assert_eq!(spec.args, vec!["--stdio".to_owned()]);
        assert_eq!(spec.readiness, ReadinessKind::ProgressIdle);
    }
}
