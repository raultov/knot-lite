//! async-lsp client: spawn, router, requests and shutdown (T4.2).
//!
//! Modelled on async-lsp's `examples/client_builder.rs`, extended with the
//! server→client request answers, the custom readiness notifications and the
//! retry/latency accounting the spike needs.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ErrorCode, LanguageServer, MainLoop, ServerSocket};
use lsp_types::notification::Notification;
use lsp_types::request::Request;
use lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyItem, CallHierarchyPrepareParams,
    ClientCapabilities, ConfigurationParams, DidOpenTextDocumentParams, DocumentSymbol,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    InitializeParams, InitializeResult, InitializedParams, Location, NumberOrString, Position,
    ProgressParamsValue, ReferenceContext, ReferenceParams, TextDocumentClientCapabilities,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Url,
    WindowClientCapabilities, WorkDoneProgress, WorkDoneProgressParams,
    WorkspaceClientCapabilities, WorkspaceFolder, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;

use crate::common::Lang;
use crate::lsp::readiness::ReadinessState;
use crate::lsp::servers::ServerSpec;
use crate::uri::path_to_uri;

/// Loopback event that breaks the main loop (as in the upstream example).
pub struct Stop;

/// `experimental/serverStatus` (rust-analyzer custom notification).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ServerStatusParams {
    pub health: String,
    pub quiescent: bool,
    #[serde(default)]
    pub message: Option<String>,
}

/// Marker type for [`ServerStatusParams`].
pub enum ServerStatusNotification {}

impl Notification for ServerStatusNotification {
    type Params = ServerStatusParams;
    const METHOD: &'static str = "experimental/serverStatus";
}

/// `language/status` (jdtls / vscode-java custom notification).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LanguageStatusParams {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
}

/// Marker type for [`LanguageStatusParams`].
pub enum LanguageStatusNotification {}

impl Notification for LanguageStatusNotification {
    type Params = LanguageStatusParams;
    const METHOD: &'static str = "language/status";
}

/// Wall-clock timings recorded around the session start.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct SessionTimings {
    pub spawn_ms: u64,
    pub initialize_ms: u64,
}

/// Outcome of one request, with per-try latency and retry accounting.
#[derive(Debug)]
pub struct RequestOutcome<T> {
    pub result: Result<T, String>,
    /// Wall time of the successful try (or of the failing one).
    pub latency: Duration,
    /// Wall time including retries and backoff.
    pub total: Duration,
    pub retries: u32,
}

/// An initialised LSP session against one sandbox root.
pub struct LspSession {
    pub server: ServerSocket,
    pub child: Child,
    pub mainloop: JoinHandle<Result<()>>,
    pub events: watch::Receiver<ReadinessState>,
    pub init_result: InitializeResult,
    pub timings: SessionTimings,
    pub stderr_log: PathBuf,
    /// Server→client request methods seen (for the result JSON).
    pub seen_server_requests: Arc<Mutex<BTreeSet<String>>>,
    /// Counted (otherwise ignored) server→client notifications.
    pub notif_counts: Arc<Mutex<BTreeMap<String, u64>>>,
    language_id_for: fn(&Path) -> &'static str,
    first_request_done: bool,
    first_request_timeout: Duration,
    opened: BTreeSet<Url>,
}

/// Router state shared with the main loop task.
struct ClientState {
    ready_tx: watch::Sender<ReadinessState>,
    java_settings: serde_json::Value,
    workspace_folder: WorkspaceFolder,
    seen_requests: Arc<Mutex<BTreeSet<String>>>,
    notif_counts: Arc<Mutex<BTreeMap<String, u64>>>,
}

/// Spawn the language server and complete the LSP handshake.
pub async fn start(spec: &ServerSpec, root: &Path, logs_dir: &Path) -> Result<LspSession> {
    let spawn_started = Instant::now();
    let path_value = spec
        .search_path
        .clone()
        .or_else(|| std::env::var_os("PATH"));
    let program = crate::lsp::servers::which_in(&spec.program, path_value.as_deref())
        .with_context(|| format!("language server not on PATH: {}", spec.program))?;
    std::fs::create_dir_all(logs_dir)
        .with_context(|| format!("creating log dir {}", logs_dir.display()))?;

    let stderr_log = logs_dir.join(format!("{}_{}.stderr.log", spec.lang.label(), unix_ms()));
    let stderr_file = std::fs::File::create(&stderr_log)
        .with_context(|| format!("creating {}", stderr_log.display()))?;
    let mut child = spawn_server(spec, &program, root, stderr_file)?;
    let stdin = child.stdin.take().context("child stdin piped")?;
    let stdout = child.stdout.take().context("child stdout piped")?;

    let root_url = path_to_uri(root)?;
    let folder = WorkspaceFolder {
        uri: root_url.clone(),
        name: root
            .file_name()
            .map_or_else(|| "root".to_owned(), |n| n.to_string_lossy().into_owned()),
    };
    let java_settings = spec
        .initialization_options
        .pointer("/settings/java")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let (ready_tx, ready_rx) = watch::channel(ReadinessState::default());
    let seen_requests = Arc::new(Mutex::new(BTreeSet::new()));
    let notif_counts = Arc::new(Mutex::new(BTreeMap::new()));
    let state = ClientState {
        ready_tx,
        java_settings,
        workspace_folder: folder.clone(),
        seen_requests: Arc::clone(&seen_requests),
        notif_counts: Arc::clone(&notif_counts),
    };

    let (mainloop, mut server) = MainLoop::new_client(|_| {
        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .service(build_client_router(state))
    });
    let mainloop_task = tokio::spawn(async move {
        mainloop
            .run_buffered(stdout.compat(), stdin.compat_write())
            .await
            .map_err(anyhow::Error::from)
    });

    let init_started = Instant::now();
    let init_result = match server
        .initialize(build_initialize_params(spec, &root_url, &folder))
        .await
    {
        Ok(result) => result,
        Err(err) => {
            // Diagnose dead servers: report the child's exit status instead of
            // a bare protocol error (T8 "present but broken" scenario).
            let exit = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            let detail = match exit {
                Ok(Ok(status)) => format!(
                    "exited with status {} before initialize",
                    status
                        .code()
                        .map_or_else(|| "signal".to_owned(), |c| c.to_string())
                ),
                Ok(Err(wait_err)) => format!("wait failed before initialize: {wait_err}"),
                Err(_) => "did not exit before initialize".to_owned(),
            };
            return Err(anyhow!("LSP server {detail} ({err})"));
        }
    };
    server
        .initialized(InitializedParams {})
        .map_err(|err| anyhow!("LSP initialized failed: {err}"))?;

    Ok(LspSession {
        timings: SessionTimings {
            spawn_ms: spawn_started.elapsed().as_millis() as u64,
            initialize_ms: init_started.elapsed().as_millis() as u64,
        },
        server,
        child,
        mainloop: mainloop_task,
        events: ready_rx,
        init_result,
        stderr_log,
        seen_server_requests: seen_requests,
        notif_counts,
        language_id_for: spec.language_id_for,
        first_request_done: false,
        first_request_timeout: if spec.lang == Lang::Java {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(30)
        },
        opened: BTreeSet::new(),
    })
}

fn spawn_server(
    spec: &ServerSpec,
    program: &Path,
    root: &Path,
    stderr_file: std::fs::File,
) -> Result<Child> {
    let mut command = Command::new(program);
    command
        .args(&spec.args)
        .envs(
            spec.env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(stderr_file))
        .kill_on_drop(true);
    if let Some(path) = &spec.search_path {
        // T8: the sanitized PATH reaches the child, the real env stays intact.
        command.env("PATH", path);
    }
    command
        .spawn()
        .with_context(|| format!("spawning {} {}", program.display(), spec.args.join(" ")))
}

/// Graceful `shutdown` + `exit`, then wait for the loop and the child
/// (killing the process tree on timeout).
pub async fn shutdown(session: LspSession, timeout: Duration) -> Result<()> {
    let root_pid = session.child.id();
    let LspSession {
        mut server,
        mut child,
        mainloop,
        ..
    } = session;

    let handshake = async {
        server
            .shutdown(())
            .await
            .map_err(|err| anyhow!("LSP shutdown request failed: {err}"))?;
        server
            .exit(())
            .map_err(|err| anyhow!("LSP exit notification failed: {err}"))?;
        server
            .emit(Stop)
            .map_err(|err| anyhow!("emitting Stop failed: {err}"))?;
        Ok::<(), anyhow::Error>(())
    };
    if tokio::time::timeout(timeout, handshake).await.is_err() {
        tracing::warn!("LSP shutdown handshake timed out after {timeout:?}");
    }
    match tokio::time::timeout(timeout, mainloop).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(err))) => tracing::warn!("LSP main loop ended with error: {err:#}"),
        Ok(Err(err)) => tracing::warn!("LSP main loop join failed: {err}"),
        Err(_) => tracing::warn!("LSP main loop did not finish within {timeout:?}"),
    }
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => tracing::info!("LSP child exited: {status}"),
        Ok(Err(err)) => return Err(anyhow!("waiting for LSP child: {err}")),
        Err(_) => {
            tracing::warn!("LSP child alive after {timeout:?}; killing process tree");
            if let Some(pid) = root_pid {
                crate::lsp::rss::kill_process_tree(pid);
            }
            let _ = child.wait().await;
        }
    }
    Ok(())
}

/// Send one request, retrying on `ContentModified`/`RequestCancelled`/
/// `ServerCancelled` (T4.2 item 4).
pub async fn request_with_retry<R>(
    server: &ServerSocket,
    params: R::Params,
    max_retries: u32,
    per_try: Duration,
) -> RequestOutcome<R::Result>
where
    R: Request,
    R::Params: Clone,
{
    let started = Instant::now();
    let mut retries = 0;
    loop {
        let try_started = Instant::now();
        match tokio::time::timeout(per_try, server.request::<R>(params.clone())).await {
            Err(_) => {
                return RequestOutcome {
                    result: Err(format!("timeout after {per_try:?}")),
                    latency: try_started.elapsed(),
                    total: started.elapsed(),
                    retries,
                };
            }
            Ok(Ok(result)) => {
                return RequestOutcome {
                    result: Ok(result),
                    latency: try_started.elapsed(),
                    total: started.elapsed(),
                    retries,
                };
            }
            Ok(Err(err)) if retries < max_retries && is_retryable(&err) => {
                retries += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Ok(Err(err)) => {
                return RequestOutcome {
                    result: Err(err.to_string()),
                    latency: try_started.elapsed(),
                    total: started.elapsed(),
                    retries,
                };
            }
        }
    }
}

fn is_retryable(err: &async_lsp::Error) -> bool {
    match err {
        async_lsp::Error::Response(resp) => matches!(
            resp.code,
            ErrorCode::CONTENT_MODIFIED
                | ErrorCode::SERVER_CANCELLED
                | ErrorCode::REQUEST_CANCELLED
        ),
        _ => false,
    }
}

fn unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Build the initialize params (T4.2 item 3).
#[expect(
    deprecated,
    reason = "plan T4.2 item 3: jdtls and typescript-language-server still read the deprecated root_uri"
)]
fn build_initialize_params(
    spec: &ServerSpec,
    root_url: &Url,
    folder: &WorkspaceFolder,
) -> InitializeParams {
    InitializeParams {
        process_id: Some(std::process::id()),
        root_uri: Some(root_url.clone()),
        workspace_folders: Some(vec![folder.clone()]),
        initialization_options: Some(spec.initialization_options.clone()),
        capabilities: client_capabilities(),
        ..InitializeParams::default()
    }
}

/// Capabilities the spike needs: progress, call/type hierarchy, references,
/// implementation, definition, hierarchical document symbols, workspace
/// folders/symbols and rust-analyzer's status notification opt-in.
fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            call_hierarchy: Some(Default::default()),
            type_hierarchy: Some(Default::default()),
            references: Some(Default::default()),
            implementation: Some(Default::default()),
            definition: Some(Default::default()),
            document_symbol: Some(lsp_types::DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            workspace_folders: Some(true),
            configuration: Some(true),
            symbol: Some(Default::default()),
            ..Default::default()
        }),
        experimental: Some(serde_json::json!({ "serverStatusNotification": true })),
        ..Default::default()
    }
}

/// Build the server→client router (T4.2 item 2).
fn build_client_router(state: ClientState) -> Router<ClientState> {
    let mut router = Router::new(state);
    register_server_requests(&mut router);
    register_server_notifications(&mut router);
    router
        .event(|_, _: Stop| ControlFlow::Break(Ok(())))
        .unhandled_request(|state, req| {
            note_request(&state.seen_requests, req.method.as_str());
            tracing::debug!("unhandled server request: {}", req.method);
            async move { Ok(serde_json::Value::Null) }
        })
        .unhandled_notification(|state, notif| {
            bump_count(&state.notif_counts, notif.method.as_str());
            ControlFlow::Continue(())
        });
    router
}

/// Requests a server may send; each must be answered or some servers hang.
fn register_server_requests(router: &mut Router<ClientState>) {
    router.request::<lsp_types::request::WorkspaceConfiguration, _>(|state, params| {
        note_request(&state.seen_requests, "workspace/configuration");
        let replies = configuration_replies(state, &params);
        async move { Ok(replies) }
    });
    router.request::<lsp_types::request::WorkspaceFoldersRequest, _>(|state, _params| {
        note_request(&state.seen_requests, "workspace/workspaceFolders");
        let folder = state.workspace_folder.clone();
        async move { Ok(Some(vec![folder])) }
    });
    router.request::<lsp_types::request::ShowMessageRequest, _>(|state, params| {
        note_request(&state.seen_requests, "window/showMessageRequest");
        bump_count(&state.notif_counts, "window/showMessageRequest");
        tracing::info!("showMessageRequest: {:?} {}", params.typ, params.message);
        async move { Ok(None) }
    });
    register_unit_request::<lsp_types::request::WorkDoneProgressCreate>(
        router,
        "window/workDoneProgress/create",
    );
    register_unit_request::<lsp_types::request::RegisterCapability>(
        router,
        "client/registerCapability",
    );
    register_unit_request::<lsp_types::request::UnregisterCapability>(
        router,
        "client/unregisterCapability",
    );
}

/// `workspace/configuration`: answer `java*` sections with the jdtls settings
/// subtree from the init options, everything else with `null` (T4.2 item 2).
fn configuration_replies(
    state: &ClientState,
    params: &ConfigurationParams,
) -> Vec<serde_json::Value> {
    params
        .items
        .iter()
        .map(|item| {
            let wants_java = item
                .section
                .as_deref()
                .is_some_and(|section| section.starts_with("java"));
            if wants_java {
                state.java_settings.clone()
            } else {
                serde_json::Value::Null
            }
        })
        .collect()
}

fn register_unit_request<R>(router: &mut Router<ClientState>, label: &'static str)
where
    R: Request<Result = ()>,
{
    router.request::<R, _>(move |state, _params| {
        note_request(&state.seen_requests, label);
        async move { Ok::<(), async_lsp::ResponseError>(()) }
    });
}

/// Notifications from the server: feed readiness, count the rest.
fn register_server_notifications(router: &mut Router<ClientState>) {
    router.notification::<lsp_types::notification::Progress>(|state, progress| {
        handle_progress(state, progress.token, progress.value);
        ControlFlow::Continue(())
    });
    router.notification::<ServerStatusNotification>(|state, status| {
        state
            .ready_tx
            .send_modify(|ready| ready.on_server_status(&status.health, status.quiescent));
        ControlFlow::Continue(())
    });
    router.notification::<LanguageStatusNotification>(|state, status| {
        state
            .ready_tx
            .send_modify(|ready| ready.on_language_status(&status.kind));
        ControlFlow::Continue(())
    });
    register_ignored_notification::<lsp_types::notification::PublishDiagnostics>(
        router,
        "textDocument/publishDiagnostics",
    );
    register_ignored_notification::<lsp_types::notification::LogMessage>(
        router,
        "window/logMessage",
    );
    register_ignored_notification::<lsp_types::notification::ShowMessage>(
        router,
        "window/showMessage",
    );
    register_ignored_notification::<lsp_types::notification::TelemetryEvent>(
        router,
        "telemetry/event",
    );
}

fn register_ignored_notification<N>(router: &mut Router<ClientState>, label: &'static str)
where
    N: Notification,
{
    router.notification::<N>(move |state, _params| {
        bump_count(&state.notif_counts, label);
        ControlFlow::Continue(())
    });
}

fn handle_progress(state: &mut ClientState, token: NumberOrString, value: ProgressParamsValue) {
    let token = match token {
        NumberOrString::String(text) => text,
        NumberOrString::Number(number) => number.to_string(),
    };
    match value {
        ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(begin)) => state
            .ready_tx
            .send_modify(|ready| ready.on_progress_begin(token, begin.title)),
        ProgressParamsValue::WorkDone(WorkDoneProgress::Report(_)) => state
            .ready_tx
            .send_modify(|ready| ready.on_progress_report(&token)),
        ProgressParamsValue::WorkDone(WorkDoneProgress::End(_)) => {
            state
                .ready_tx
                .send_modify(|ready| ready.on_progress_end(&token));
        }
    }
}

fn bump_count(counts: &Arc<Mutex<BTreeMap<String, u64>>>, label: &str) {
    if let Ok(mut map) = counts.lock() {
        *map.entry(label.to_owned()).or_insert(0) += 1;
    }
}

fn note_request(seen: &Arc<Mutex<BTreeSet<String>>>, label: &str) {
    if let Ok(mut set) = seen.lock() {
        set.insert(label.to_owned());
    }
}

impl LspSession {
    /// Open a document (idempotent); never sends `didChange` (T4.2 item 5).
    pub fn did_open(&mut self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path)?;
        if self.opened.contains(&uri) {
            return Ok(());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        self.server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: (self.language_id_for)(path).to_owned(),
                    version: 0,
                    text,
                },
            })
            .map_err(|err| anyhow!("didOpen failed: {err}"))?;
        self.opened.insert(uri);
        Ok(())
    }

    /// Number of documents opened through this session.
    pub fn opened_count(&self) -> usize {
        self.opened.len()
    }

    fn next_timeout(&mut self) -> Duration {
        if self.first_request_done {
            Duration::from_secs(30)
        } else {
            self.first_request_done = true;
            self.first_request_timeout
        }
    }

    /// `textDocument/prepareCallHierarchy` at `pos`.
    pub async fn prepare_call_hierarchy(
        &mut self,
        uri: &Url,
        pos: Position,
    ) -> RequestOutcome<Option<Vec<CallHierarchyItem>>> {
        let params = CallHierarchyPrepareParams {
            text_document_position_params: position_params(uri, pos),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let timeout = self.next_timeout();
        request_with_retry::<lsp_types::request::CallHierarchyPrepare>(
            &self.server,
            params,
            10,
            timeout,
        )
        .await
    }

    /// `callHierarchy/incomingCalls` for one item.
    pub async fn incoming_calls(
        &mut self,
        item: CallHierarchyItem,
    ) -> RequestOutcome<Option<Vec<lsp_types::CallHierarchyIncomingCall>>> {
        let params = CallHierarchyIncomingCallsParams {
            item,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        let timeout = self.next_timeout();
        request_with_retry::<lsp_types::request::CallHierarchyIncomingCalls>(
            &self.server,
            params,
            10,
            timeout,
        )
        .await
    }

    /// `textDocument/references` at `pos`.
    pub async fn references(
        &mut self,
        uri: &Url,
        pos: Position,
        include_declaration: bool,
    ) -> RequestOutcome<Option<Vec<Location>>> {
        let params = ReferenceParams {
            text_document_position: position_params(uri, pos),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration,
            },
        };
        let timeout = self.next_timeout();
        request_with_retry::<lsp_types::request::References>(&self.server, params, 10, timeout)
            .await
    }

    /// `textDocument/implementation` at `pos`, normalised to `Vec<Location>`.
    pub async fn implementation(
        &mut self,
        uri: &Url,
        pos: Position,
    ) -> RequestOutcome<Vec<Location>> {
        let params = GotoDefinitionParams {
            text_document_position_params: position_params(uri, pos),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        let timeout = self.next_timeout();
        let outcome = request_with_retry::<lsp_types::request::GotoImplementation>(
            &self.server,
            params,
            10,
            timeout,
        )
        .await;
        RequestOutcome {
            result: outcome.result.map(|response| match response {
                Some(GotoDefinitionResponse::Scalar(location)) => vec![location],
                Some(GotoDefinitionResponse::Array(locations)) => locations,
                Some(GotoDefinitionResponse::Link(links)) => links
                    .into_iter()
                    .map(|link| Location {
                        uri: link.target_uri,
                        range: link.target_selection_range,
                    })
                    .collect(),
                None => Vec::new(),
            }),
            latency: outcome.latency,
            total: outcome.total,
            retries: outcome.retries,
        }
    }

    /// `textDocument/documentSymbol` for one document.
    pub async fn document_symbol(
        &mut self,
        uri: &Url,
    ) -> RequestOutcome<Option<DocumentSymbolResponse>> {
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        let timeout = self.next_timeout();
        request_with_retry::<lsp_types::request::DocumentSymbolRequest>(
            &self.server,
            params,
            10,
            timeout,
        )
        .await
    }

    /// `workspace/symbol` for one query text.
    pub async fn workspace_symbol(
        &mut self,
        query: &str,
    ) -> RequestOutcome<Option<WorkspaceSymbolResponse>> {
        let params = WorkspaceSymbolParams {
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
            query: query.to_owned(),
        };
        let timeout = self.next_timeout();
        request_with_retry::<lsp_types::request::WorkspaceSymbolRequest>(
            &self.server,
            params,
            10,
            timeout,
        )
        .await
    }
}

fn position_params(uri: &Url, pos: Position) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: pos,
    }
}

/// Count symbols in a `documentSymbol` response (flattens nested symbols).
pub fn count_symbols(response: &DocumentSymbolResponse) -> usize {
    match response {
        DocumentSymbolResponse::Flat(symbols) => symbols.len(),
        DocumentSymbolResponse::Nested(symbols) => nested_len(symbols),
    }
}

/// Flattened symbol names of a `documentSymbol` response (debug aid).
pub fn symbol_names(response: &DocumentSymbolResponse) -> Vec<String> {
    match response {
        DocumentSymbolResponse::Flat(symbols) => {
            symbols.iter().map(|info| info.name.clone()).collect()
        }
        DocumentSymbolResponse::Nested(symbols) => {
            let mut out = Vec::new();
            collect_names(&mut out, symbols);
            out
        }
    }
}

fn collect_names(out: &mut Vec<String>, symbols: &[DocumentSymbol]) {
    for symbol in symbols {
        out.push(symbol.name.clone());
        if let Some(children) = &symbol.children {
            collect_names(out, children);
        }
    }
}

fn nested_len(symbols: &[lsp_types::DocumentSymbol]) -> usize {
    symbols
        .iter()
        .map(|symbol| 1 + symbol.children.as_ref().map_or(0, |kids| nested_len(kids)))
        .sum()
}

/// Tier-2 degradation record produced when no usable LSP exists (T8).
#[derive(Debug, Clone)]
pub struct Degraded {
    /// The server binary was found on the search path.
    pub lsp_available: bool,
    /// The server completed the LSP handshake.
    pub lsp_started: bool,
    /// Human-readable explanation, including the install hint.
    pub diagnostic: String,
    /// Capability tier the probe falls back to (D9: 2 = heuristic graph).
    pub effective_tier: u8,
}

/// Outcome of the production start path (T8).
pub enum ServerStart {
    Ready(Box<LspSession>),
    Degraded(Degraded),
}

/// Start the server or degrade gracefully to tier 2 — the code path every
/// probe uses (T8). A missing binary produces **no spawn attempt**.
pub async fn start_or_degrade(
    spec: &ServerSpec,
    root: &Path,
    logs_dir: &Path,
) -> Result<ServerStart> {
    let path_value = spec
        .search_path
        .clone()
        .or_else(|| std::env::var_os("PATH"));
    if crate::lsp::servers::which_in(&spec.program, path_value.as_deref()).is_none() {
        let dirs = crate::lsp::servers::path_dir_count(path_value.as_deref());
        return Ok(ServerStart::Degraded(Degraded {
            lsp_available: false,
            lsp_started: false,
            diagnostic: format!(
                "{} not found in PATH (searched {dirs} dirs). {}",
                spec.program,
                crate::lsp::servers::install_hint(spec.lang)
            ),
            effective_tier: 2,
        }));
    }
    match start(spec, root, logs_dir).await {
        Ok(session) => Ok(ServerStart::Ready(Box::new(session))),
        Err(err) => Ok(ServerStart::Degraded(Degraded {
            lsp_available: true,
            lsp_started: false,
            diagnostic: format!("{err:#}"),
            effective_tier: 2,
        })),
    }
}
