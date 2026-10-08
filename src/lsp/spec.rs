//! How to launch, initialise and detect readiness of each language server
//! (Appendix F of the Phase 3 plan).
//!
//! [`spec_for`] is pure: it reads nothing from the process environment or the
//! disk. Everything a server may write is pointed into the per-repository
//! scratch directory (P3-D12), and build-code execution follows
//! `KNOT_LITE_LSP_BUILD_EXEC` (P3-D14).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::preopen::PreopenPlan;
use super::readiness::ReadinessKind;
use super::uri::path_to_uri;
use crate::capability::languages::LspLang;
use crate::capability::lsp_registry::{LspStatus, detect};
use crate::config::LspOverrides;

/// A language server that cannot (or did not) start; the caller answers from
/// the heuristic edges and reports `diagnostic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Degraded {
    /// Whether the server binary exists.
    pub lsp_available: bool,
    /// Whether a server process was started.
    pub lsp_started: bool,
    /// One-line explanation (the pinned texts of §10.5).
    pub diagnostic: String,
}

impl Degraded {
    /// The binary is not there: nothing was spawned.
    pub fn unavailable(diagnostic: String) -> Self {
        Self {
            lsp_available: false,
            lsp_started: false,
            diagnostic,
        }
    }
}

/// Launch and tuning data of one language server.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    /// Which server.
    pub lang: LspLang,
    /// Resolved executable.
    pub program: PathBuf,
    /// Command-line arguments.
    pub args: Vec<String>,
    /// Environment of the child only (the process environment is never modified).
    pub env: Vec<(String, String)>,
    /// `initializationOptions` of the `initialize` request.
    pub initialization_options: Value,
    /// How the server announces readiness.
    pub readiness: ReadinessKind,
    /// How long a start may take before the server counts as failed.
    pub ready_timeout: Duration,
    /// Timeout of the first request after the server is ready (lazy project load).
    pub first_request_timeout: Duration,
    /// Timeout of every later request.
    pub request_timeout: Duration,
    /// LSP `languageId` of a source file.
    pub language_id_for: fn(&Path) -> &'static str,
}

/// What [`spec_for`] needs.
#[derive(Debug, Clone, Copy)]
pub struct SpecInputs<'a> {
    /// Which server.
    pub lang: LspLang,
    /// Resolved executable (see [`resolve_program`]).
    pub program: &'a Path,
    /// Repository root, absolute.
    pub repo_root: &'a Path,
    /// Per-repository scratch directory (`lsp::settings::scratch_dir`).
    pub scratch: &'a Path,
    /// Whether the repository's build code may run.
    pub build_exec: bool,
    /// Files and projects to hand to the server.
    pub preopen: &'a PreopenPlan,
}

/// The three timeouts that differ per server.
#[derive(Clone, Copy)]
struct Timeouts {
    ready: Duration,
    first_request: Duration,
    request: Duration,
}

const RUST_TIMEOUTS: Timeouts = Timeouts {
    ready: Duration::from_secs(300),
    first_request: Duration::from_secs(30),
    request: Duration::from_secs(15),
};

const TS_TIMEOUTS: Timeouts = Timeouts {
    ready: Duration::from_secs(120),
    first_request: Duration::from_secs(30),
    request: Duration::from_secs(15),
};

const JAVA_TIMEOUTS: Timeouts = Timeouts {
    ready: Duration::from_secs(900),
    first_request: Duration::from_secs(120),
    request: Duration::from_secs(30),
};

/// Build the launch spec for `inputs.lang`.
pub fn spec_for(inputs: &SpecInputs<'_>) -> ServerSpec {
    match inputs.lang {
        LspLang::Rust => rust_spec(inputs),
        LspLang::Ts => ts_spec(inputs),
        LspLang::Java => java_spec(inputs),
    }
}

fn rust_spec(inputs: &SpecInputs<'_>) -> ServerSpec {
    let mut spec = ServerSpec::new(
        inputs,
        ReadinessKind::RustAnalyzerStatus,
        RUST_TIMEOUTS,
        rust_language_id,
    );
    spec.env = vec![(
        "CARGO_TARGET_DIR".to_owned(),
        text(&inputs.scratch.join("target-ra")),
    )];
    spec.initialization_options = rust_options(inputs);
    spec
}

fn ts_spec(inputs: &SpecInputs<'_>) -> ServerSpec {
    let mut spec = ServerSpec::new(
        inputs,
        ReadinessKind::ProgressIdle,
        TS_TIMEOUTS,
        ts_language_id,
    );
    spec.args = vec!["--stdio".to_owned()];
    spec.initialization_options = ts_options();
    spec
}

fn java_spec(inputs: &SpecInputs<'_>) -> ServerSpec {
    let mut spec = ServerSpec::new(
        inputs,
        ReadinessKind::JdtlsServiceReady,
        JAVA_TIMEOUTS,
        java_language_id,
    );
    spec.args = java_args(inputs.scratch);
    spec.initialization_options = java_options(inputs);
    spec
}

/// Resolve the server executable of `lang` against the real `PATH` and the
/// overrides. An override that does not resolve never falls back to `PATH`
/// (a typo stays visible).
pub fn resolve_program(lang: LspLang, overrides: &LspOverrides) -> Result<PathBuf, Degraded> {
    program_from_status(lang, detect(lang, overrides))
}

/// [`resolve_program`] over an already computed [`LspStatus`] (pure).
pub fn program_from_status(lang: LspLang, status: LspStatus) -> Result<PathBuf, Degraded> {
    match status {
        LspStatus::Found { path, .. } | LspStatus::Override { path, .. } => Ok(PathBuf::from(path)),
        LspStatus::Missing {
            server,
            searched_dirs,
            hint,
        } => Err(Degraded::unavailable(format!(
            "{server} not found in PATH (searched {searched_dirs} dirs). {hint}"
        ))),
        LspStatus::OverrideInvalid { value, hint, .. } => Err(Degraded::unavailable(format!(
            "{} = '{value}' does not resolve to an executable. {hint}",
            lang.env_override_key()
        ))),
    }
}

impl ServerSpec {
    /// A spec with the common fields filled in; the caller sets the rest
    /// (`args`, `env`, `initialization_options`).
    fn new(
        inputs: &SpecInputs<'_>,
        readiness: ReadinessKind,
        timeouts: Timeouts,
        language_id_for: fn(&Path) -> &'static str,
    ) -> Self {
        Self {
            lang: inputs.lang,
            program: inputs.program.to_path_buf(),
            args: Vec::new(),
            env: Vec::new(),
            initialization_options: Value::Null,
            readiness,
            ready_timeout: timeouts.ready,
            first_request_timeout: timeouts.first_request,
            request_timeout: timeouts.request,
            language_id_for,
        }
    }
}

/// rust-analyzer: no on-save checks, cache priming on, watcher in the server
/// (the client does not implement `didChangeWatchedFiles`, P3-D18).
fn rust_options(inputs: &SpecInputs<'_>) -> Value {
    let mut options = json!({
        "checkOnSave": false,
        "cachePriming": { "enable": true },
        "cargo": { "buildScripts": { "enable": inputs.build_exec } },
        "procMacro": { "enable": inputs.build_exec },
        "files": { "watcher": "server" }
    });
    if let Some(projects) = &inputs.preopen.rust_linked_projects {
        options["linkedProjects"] = json!(projects);
    }
    options
}

/// typescript-language-server. `disableAutomaticTypingAcquisition` is new
/// relative to Phase 0 (risk R6): L2 catches a recall change.
fn ts_options() -> Value {
    json!({
        "tsserver": { "logVerbosity": "off" },
        "maxTsServerMemory": 4096,
        "disableAutomaticTypingAcquisition": true,
        "preferences": { "includeCompletionsForModuleExports": false }
    })
}

/// jdtls arguments: heap, no Eclipse metadata files inside the repository
/// (P3-D12) and the workspace in the scratch directory.
fn java_args(scratch: &Path) -> Vec<String> {
    vec![
        "--jvm-arg=-Xmx8g".to_owned(),
        "--jvm-arg=-Djava.import.generatesMetadataFilesAtProjectRoot=false".to_owned(),
        "-data".to_owned(),
        text(&scratch.join("jdtls-ws")),
    ]
}

/// jdtls: Maven import always on (no build plugin runs during model import),
/// Gradle import only when build-code execution is allowed (P3-D14).
fn java_options(inputs: &SpecInputs<'_>) -> Value {
    let root_uri = path_to_uri(inputs.repo_root).map_or(Value::Null, |url| json!(url.as_str()));
    json!({
        "bundles": [],
        "workspaceFolders": [root_uri],
        "settings": { "java": {
            "import": {
                "maven": { "enabled": true },
                "gradle": { "enabled": inputs.build_exec },
                "generatesMetadataFilesAtProjectRoot": false,
                "exclusions": [
                    "**/node_modules/**",
                    "**/.metadata/**",
                    "**/archetype-resources/**",
                    "**/META-INF/maven/**"
                ]
            },
            "configuration": { "updateBuildConfiguration": "automatic" },
            // JDT's builder compiles into `target/` of every Maven module: a write into
            // the repository (L5). References and call hierarchy do not need a build.
            "autobuild": { "enabled": false },
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

fn rust_language_id(_path: &Path) -> &'static str {
    "rust"
}

fn java_language_id(_path: &Path) -> &'static str {
    "java"
}

/// LSP `languageId` of a TypeScript or JavaScript file, by extension.
fn ts_language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("tsx") => "typescriptreact",
        Some("jsx") => "javascriptreact",
        Some("js" | "mjs" | "cjs") => "javascript",
        _ => "typescript",
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::lsp_registry::{default_lookup, detect_with, install_hint};

    const ROOT: &str = "/work/demo";
    const SCRATCH: &str = "/cache/demo-0123456789abcdef";

    /// Spec of `lang` over the demo repository, with a configurable build-exec switch.
    fn spec(lang: LspLang, build_exec: bool, preopen: &PreopenPlan) -> ServerSpec {
        spec_for(&SpecInputs {
            lang,
            program: Path::new("/bin/server"),
            repo_root: Path::new(ROOT),
            scratch: Path::new(SCRATCH),
            build_exec,
            preopen,
        })
    }

    fn default_spec(lang: LspLang) -> ServerSpec {
        spec(lang, true, &PreopenPlan::default())
    }

    #[test]
    fn rust_spec_points_target_dir_into_scratch() {
        let spec = default_spec(LspLang::Rust);
        assert_eq!(spec.program, Path::new("/bin/server"));
        assert!(spec.args.is_empty());
        assert_eq!(
            spec.env,
            vec![(
                "CARGO_TARGET_DIR".to_owned(),
                Path::new(SCRATCH)
                    .join("target-ra")
                    .to_string_lossy()
                    .into_owned()
            )]
        );
        assert_eq!(spec.readiness, ReadinessKind::RustAnalyzerStatus);
        assert_eq!(spec.ready_timeout, Duration::from_secs(300));
        assert_eq!(spec.first_request_timeout, Duration::from_secs(30));
        assert_eq!(spec.request_timeout, Duration::from_secs(15));
    }

    #[test]
    fn rust_spec_honours_build_exec_off() {
        let on = default_spec(LspLang::Rust).initialization_options;
        assert_eq!(on["cargo"]["buildScripts"]["enable"], json!(true));
        assert_eq!(on["procMacro"]["enable"], json!(true));
        let off = spec(LspLang::Rust, false, &PreopenPlan::default()).initialization_options;
        assert_eq!(off["cargo"]["buildScripts"]["enable"], json!(false));
        assert_eq!(off["procMacro"]["enable"], json!(false));
    }

    #[test]
    fn rust_spec_sets_server_watcher() {
        let options = default_spec(LspLang::Rust).initialization_options;
        assert_eq!(options["files"]["watcher"], json!("server"));
        assert_eq!(options["checkOnSave"], json!(false));
        assert_eq!(options["cachePriming"]["enable"], json!(true));
    }

    #[test]
    fn rust_spec_passes_linked_projects_when_no_root_manifest() {
        assert!(
            default_spec(LspLang::Rust)
                .initialization_options
                .get("linkedProjects")
                .is_none(),
            "auto-discovery stays on without a plan"
        );
        let plan = PreopenPlan {
            rust_linked_projects: Some(vec!["/work/demo/svc/Cargo.toml".to_owned()]),
            ..PreopenPlan::default()
        };
        let options = spec(LspLang::Rust, true, &plan).initialization_options;
        assert_eq!(
            options["linkedProjects"],
            json!(["/work/demo/svc/Cargo.toml"])
        );
    }

    #[test]
    fn ts_spec_is_stdio_and_disables_typings_acquisition() {
        let spec = default_spec(LspLang::Ts);
        assert_eq!(spec.args, vec!["--stdio".to_owned()]);
        assert!(spec.env.is_empty());
        assert_eq!(spec.readiness, ReadinessKind::ProgressIdle);
        assert_eq!(spec.ready_timeout, Duration::from_secs(120));
        assert_eq!(
            spec.initialization_options["disableAutomaticTypingAcquisition"],
            json!(true)
        );
        assert_eq!(
            spec.initialization_options["tsserver"]["logVerbosity"],
            json!("off")
        );
        assert_eq!(
            spec.initialization_options["maxTsServerMemory"],
            json!(4096)
        );
    }

    #[test]
    fn java_spec_has_data_dir_xmx_and_metadata_flag() {
        let spec = default_spec(LspLang::Java);
        let workspace = Path::new(SCRATCH)
            .join("jdtls-ws")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            spec.args,
            vec![
                "--jvm-arg=-Xmx8g".to_owned(),
                "--jvm-arg=-Djava.import.generatesMetadataFilesAtProjectRoot=false".to_owned(),
                "-data".to_owned(),
                workspace,
            ]
        );
        assert_eq!(spec.readiness, ReadinessKind::JdtlsServiceReady);
        assert_eq!(spec.ready_timeout, Duration::from_secs(900));
        assert_eq!(spec.first_request_timeout, Duration::from_secs(120));
        assert_eq!(spec.request_timeout, Duration::from_secs(30));
        let import = &spec.initialization_options["settings"]["java"]["import"];
        assert_eq!(import["generatesMetadataFilesAtProjectRoot"], json!(false));
        assert_eq!(import["maven"]["enabled"], json!(true));
    }

    #[cfg(unix)]
    #[test]
    fn java_spec_announces_the_repository_as_workspace_folder() {
        let options = default_spec(LspLang::Java).initialization_options;
        assert_eq!(options["workspaceFolders"], json!(["file:///work/demo"]));
    }

    #[test]
    fn java_spec_honours_build_exec_off_for_gradle() {
        let on = default_spec(LspLang::Java).initialization_options;
        assert_eq!(
            on["settings"]["java"]["import"]["gradle"]["enabled"],
            json!(true)
        );
        let off = spec(LspLang::Java, false, &PreopenPlan::default()).initialization_options;
        assert_eq!(
            off["settings"]["java"]["import"]["gradle"]["enabled"],
            json!(false)
        );
        assert_eq!(
            off["settings"]["java"]["import"]["maven"]["enabled"],
            json!(true),
            "Maven import runs no build plugins and stays on"
        );
    }

    #[test]
    fn language_ids_cover_extensions() {
        let rust = default_spec(LspLang::Rust);
        assert_eq!((rust.language_id_for)(Path::new("src/lib.rs")), "rust");
        let java = default_spec(LspLang::Java);
        assert_eq!((java.language_id_for)(Path::new("A.java")), "java");
        let ts = default_spec(LspLang::Ts).language_id_for;
        for (file, id) in [
            ("src/main.tsx", "typescriptreact"),
            ("web/App.jsx", "javascriptreact"),
            ("scripts/run.mjs", "javascript"),
            ("scripts/run.cjs", "javascript"),
            ("lib/a.js", "javascript"),
            ("src/format.ts", "typescript"),
            ("src/format.cts", "typescript"),
        ] {
            assert_eq!(ts(Path::new(file)), id, "{file}");
        }
    }

    /// `PATH` made of one temp dir that holds (or not) a stub executable.
    fn path_with(stub: Option<&str>) -> (tempfile::TempDir, std::ffi::OsString) {
        let tmp = tempfile::tempdir().expect("tempdir");
        if let Some(name) = stub {
            let path = tmp.path().join(name);
            std::fs::write(&path, b"stub").expect("write");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
        }
        let path_var = std::env::join_paths([tmp.path()]).expect("join");
        (tmp, path_var)
    }

    fn status_in(lang: LspLang, overrides: &LspOverrides, path_var: &std::ffi::OsStr) -> LspStatus {
        detect_with(
            lang,
            overrides,
            Some(path_var),
            &default_lookup(Some(path_var), None, false),
        )
    }

    #[test]
    fn resolve_program_missing_has_install_hint() {
        let (_tmp, path_var) = path_with(None);
        let status = status_in(LspLang::Java, &LspOverrides::default(), &path_var);
        let degraded = program_from_status(LspLang::Java, status).expect_err("missing");
        assert!(!degraded.lsp_available);
        assert!(!degraded.lsp_started);
        assert_eq!(
            degraded.diagnostic,
            format!(
                "jdtls not found in PATH (searched 1 dirs). {}",
                install_hint(LspLang::Java)
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_program_finds_the_default_binary() {
        let (tmp, path_var) = path_with(Some("rust-analyzer"));
        let status = status_in(LspLang::Rust, &LspOverrides::default(), &path_var);
        assert_eq!(
            program_from_status(LspLang::Rust, status),
            Ok(tmp.path().join("rust-analyzer"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_program_invalid_override_never_falls_back_to_path() {
        // The real binary IS on PATH; the override must still win and fail.
        let (_tmp, path_var) = path_with(Some("rust-analyzer"));
        let overrides = LspOverrides {
            rust: Some("/nonexistent/ra".to_owned()),
            ..LspOverrides::default()
        };
        let status = status_in(LspLang::Rust, &overrides, &path_var);
        let degraded = program_from_status(LspLang::Rust, status).expect_err("invalid override");
        assert!(!degraded.lsp_available);
        assert!(
            degraded.diagnostic.starts_with(
                "KNOT_LITE_LSP_RUST = '/nonexistent/ra' does not resolve to an executable."
            ),
            "{}",
            degraded.diagnostic
        );
        assert!(
            degraded
                .diagnostic
                .ends_with(crate::capability::lsp_registry::OVERRIDE_INVALID_HINT)
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_program_accepts_a_valid_override() {
        let (tmp, path_var) = path_with(Some("custom-ra"));
        let overrides = LspOverrides {
            rust: Some("custom-ra".to_owned()),
            ..LspOverrides::default()
        };
        let status = status_in(LspLang::Rust, &overrides, &path_var);
        assert_eq!(
            program_from_status(LspLang::Rust, status),
            Ok(tmp.path().join("custom-ra"))
        );
    }
}
