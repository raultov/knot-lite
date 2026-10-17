//! One LSP client session over any pair of byte streams.
//!
//! The session knows nothing about processes: [`initialize`] takes the two
//! halves of a pipe (a child's stdout/stdin in production, an in-memory duplex
//! in the tests), runs the handshake and returns an [`LspSession`] that issues
//! the four requests `find_callers` needs. Ported from the Phase 0 spike client
//! (T4.2) with typed request errors, an mtime-aware `didOpen`, and an
//! `initialize` request that never touches `InitializeParams::root_uri`
//! (P3-D15).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, anyhow};
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::notification::{
    DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized,
    Notification,
};
use async_lsp::lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyPrepare, DocumentSymbolRequest, GotoImplementation,
    References, Request, Shutdown,
};
use async_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyPrepareParams, ClientCapabilities, ClientInfo, ConfigurationParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbolClientCapabilities, DocumentSymbolParams, DocumentSymbolResponse, FileEvent,
    GotoDefinitionParams, GotoDefinitionResponse, InitializeParams, InitializeResult,
    InitializedParams, Location, NumberOrString, Position, ProgressParamsValue, ReferenceContext,
    ReferenceParams, ServerCapabilities, TextDocumentClientCapabilities, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, Url, WindowClientCapabilities, WorkDoneProgress,
    WorkDoneProgressParams, WorkspaceClientCapabilities, WorkspaceFolder,
};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ErrorCode, MainLoop, ResponseError, ServerSocket};
use futures::io::{AsyncRead, AsyncWrite};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tower::ServiceBuilder;

use super::readiness::ReadinessState;
use super::spec::ServerSpec;
use super::uri::path_to_uri;

/// Loopback event that breaks the main loop.
pub struct Stop;

/// The `initialize` request with raw JSON parameters.
///
/// `InitializeParams::root_uri` is deprecated in `lsp-types`, but jdtls and
/// tsserver still read `rootUri`. Building the parameters from the
/// non-deprecated fields and inserting the key by hand keeps the deprecated
/// field out of the source (P3-D15).
pub enum RawInitialize {}

impl Request for RawInitialize {
    type Params = Value;
    type Result = InitializeResult;
    const METHOD: &'static str = "initialize";
}

/// `experimental/serverStatus` parameters (rust-analyzer).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ServerStatusParams {
    /// `ok`, `warning` or `error`.
    pub health: String,
    /// Whether the server finished its background work.
    pub quiescent: bool,
    /// Human-readable status.
    #[serde(default)]
    pub message: Option<String>,
}

/// Marker type for `experimental/serverStatus`.
pub enum ServerStatusNotification {}

impl Notification for ServerStatusNotification {
    type Params = ServerStatusParams;
    const METHOD: &'static str = "experimental/serverStatus";
}

/// `language/status` parameters (jdtls).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LanguageStatusParams {
    /// `Starting`, `ServiceReady`, …
    #[serde(rename = "type")]
    pub kind: String,
    /// Human-readable status.
    pub message: String,
}

/// Marker type for `language/status`.
pub enum LanguageStatusNotification {}

impl Notification for LanguageStatusNotification {
    type Params = LanguageStatusParams;
    const METHOD: &'static str = "language/status";
}

/// The two halves of the byte stream to a server.
pub struct SessionIo<R, W> {
    /// Server → client.
    pub read: R,
    /// Client → server.
    pub write: W,
}

/// What the handshake needs besides the byte streams.
#[derive(Debug, Clone, Copy)]
pub struct InitInputs<'a> {
    /// Launch data (initialization options, timeouts, language ids).
    pub spec: &'a ServerSpec,
    /// Repository root, absolute.
    pub repo_root: &'a Path,
}

/// Wall-clock timings recorded around the handshake.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct SessionTimings {
    /// Milliseconds the `initialize` request took.
    pub initialize_ms: u64,
}

/// How a request is retried when the server answers `ContentModified`,
/// `ServerCancelled` or `RequestCancelled` (it is still indexing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt.
    pub max_retries: u32,
    /// Pause between attempts.
    pub backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            backoff: Duration::from_millis(300),
        }
    }
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// No answer within the per-attempt timeout.
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// The server answered with an error.
    #[error("server error {code}: {message}")]
    Response {
        /// JSON-RPC error code.
        code: i32,
        /// Server message.
        message: String,
    },
    /// The connection is gone (server exited, pipe closed, …).
    #[error("{0}")]
    Transport(String),
}

impl RequestError {
    /// Whether the server asked the client to try again.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Response { code, .. }
                if [
                    ErrorCode::CONTENT_MODIFIED.0,
                    ErrorCode::SERVER_CANCELLED.0,
                    ErrorCode::REQUEST_CANCELLED.0,
                ]
                .contains(code)
        )
    }
}

impl From<async_lsp::Error> for RequestError {
    fn from(error: async_lsp::Error) -> Self {
        match error {
            async_lsp::Error::Response(response) => Self::Response {
                code: response.code.0,
                message: response.message,
            },
            other => Self::Transport(other.to_string()),
        }
    }
}

/// Outcome of one request, with per-try latency and retry accounting.
#[derive(Debug)]
pub struct RequestOutcome<T> {
    /// The answer, or why there is none.
    pub result: Result<T, RequestError>,
    /// Wall time of the last attempt.
    pub latency: Duration,
    /// Wall time including retries and back-off.
    pub total: Duration,
    /// Retries performed.
    pub retries: u32,
}

/// Server → client traffic the router answered or ignored.
#[derive(Debug, Default)]
pub struct SessionStats {
    requests: Mutex<BTreeSet<String>>,
    notifications: Mutex<BTreeMap<String, u64>>,
}

impl SessionStats {
    /// Methods of the server → client requests seen so far.
    pub fn seen_requests(&self) -> Vec<String> {
        lock(&self.requests).iter().cloned().collect()
    }

    /// How many notifications (or requests) of `label` were counted.
    pub fn count_of(&self, label: &str) -> u64 {
        lock(&self.notifications).get(label).copied().unwrap_or(0)
    }

    fn note_request(&self, label: &str) {
        lock(&self.requests).insert(label.to_owned());
    }

    fn bump(&self, label: &str) {
        *lock(&self.notifications)
            .entry(label.to_owned())
            .or_insert(0) += 1;
    }
}

/// Lock a mutex, ignoring poisoning (the data is plain counters).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What [`LspSession::shutdown`] managed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownOutcome {
    /// `shutdown` was answered and `exit` was sent within the timeout.
    pub handshake: bool,
    /// The main loop ended within the timeout (it is aborted otherwise).
    pub loop_ended: bool,
}

/// An initialised LSP session against one repository.
pub struct LspSession {
    server: ServerSocket,
    mainloop: JoinHandle<anyhow::Result<()>>,
    /// Readiness signals, fed by the router.
    pub events: watch::Receiver<ReadinessState>,
    /// What the server answered to `initialize`.
    pub init_result: InitializeResult,
    /// `"<name> <version>"` from `serverInfo`, or `""`.
    pub server_version: String,
    /// Handshake timings.
    pub timings: SessionTimings,
    /// Server → client traffic seen.
    pub stats: Arc<SessionStats>,
    /// Retry policy of every request.
    pub retry: RetryPolicy,
    first_request_timeout: Duration,
    request_timeout: Duration,
    first_request_done: bool,
    language_id_for: fn(&Path) -> &'static str,
    /// Open documents and the file's mtime when they were opened.
    opened: BTreeMap<Url, SystemTime>,
}

/// Router state shared with the main loop task.
struct ClientState {
    ready_tx: watch::Sender<ReadinessState>,
    java_settings: Value,
    workspace_folder: WorkspaceFolder,
    stats: Arc<SessionStats>,
}

/// Run the LSP handshake over `io` and return the session.
///
/// Fails when the server answers `initialize` with an error, closes the pipe
/// first, or stays silent for the spec's ready timeout.
pub async fn initialize<R, W>(
    io: SessionIo<R, W>,
    init: InitInputs<'_>,
) -> anyhow::Result<LspSession>
where
    R: AsyncRead + Send + 'static,
    W: AsyncWrite + Send + 'static,
{
    let (root_uri, folder) = root_folder(init.repo_root)?;
    let params = initialize_params(init.spec, &root_uri, folder.clone())?;
    let (ready_tx, events) = watch::channel(ReadinessState::default());
    let stats = Arc::new(SessionStats::default());
    let state = ClientState {
        ready_tx,
        java_settings: init
            .spec
            .initialization_options
            .pointer("/settings/java")
            .cloned()
            .unwrap_or(Value::Null),
        workspace_folder: folder.clone(),
        stats: Arc::clone(&stats),
    };
    let (mainloop, server) = MainLoop::new_client(|_| {
        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .service(build_client_router(state))
    });
    let mainloop = tokio::spawn(async move {
        mainloop
            .run_buffered(io.read, io.write)
            .await
            .map_err(anyhow::Error::from)
    });

    let started = Instant::now();
    let request = server.request::<RawInitialize>(params);
    let init_result = match tokio::time::timeout(init.spec.ready_timeout, request).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            mainloop.abort();
            return Err(anyhow!("initialize failed: {error}"));
        }
        Err(_) => {
            mainloop.abort();
            return Err(anyhow!(
                "initialize timed out after {:?}",
                init.spec.ready_timeout
            ));
        }
    };
    server
        .notify::<Initialized>(InitializedParams {})
        .map_err(|error| anyhow!("initialized failed: {error}"))?;

    Ok(LspSession {
        server,
        mainloop,
        events,
        server_version: server_version(&init_result),
        init_result,
        timings: SessionTimings {
            initialize_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        },
        stats,
        retry: RetryPolicy::default(),
        first_request_timeout: init.spec.first_request_timeout,
        request_timeout: init.spec.request_timeout,
        first_request_done: false,
        language_id_for: init.spec.language_id_for,
        opened: BTreeMap::new(),
    })
}

/// `rootUri` and the workspace folder of the repository.
fn root_folder(root: &Path) -> anyhow::Result<(Url, WorkspaceFolder)> {
    let uri = path_to_uri(root)?;
    let name = root.file_name().map_or_else(
        || "root".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let folder = WorkspaceFolder {
        uri: uri.clone(),
        name,
    };
    Ok((uri, folder))
}

/// `initialize` parameters as JSON: the typed parameters without the
/// deprecated fields, plus `rootUri` inserted by hand (P3-D15).
fn initialize_params(
    spec: &ServerSpec,
    root_uri: &Url,
    folder: WorkspaceFolder,
) -> anyhow::Result<Value> {
    let params = InitializeParams {
        process_id: Some(std::process::id()),
        workspace_folders: Some(vec![folder]),
        initialization_options: Some(spec.initialization_options.clone()),
        capabilities: client_capabilities(),
        client_info: Some(ClientInfo {
            name: "knot-lite".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
        ..InitializeParams::default()
    };
    let mut value = serde_json::to_value(params).context("serialising initialize parameters")?;
    if let Some(object) = value.as_object_mut() {
        object.insert("rootUri".to_owned(), json!(root_uri.as_str()));
    }
    Ok(value)
}

/// `"<name> <version>"` of the server, `""` when it does not say.
fn server_version(result: &InitializeResult) -> String {
    result
        .server_info
        .as_ref()
        .map_or_else(String::new, |info| {
            match info
                .version
                .as_deref()
                .filter(|version| !version.is_empty())
            {
                Some(version) => format!("{} {version}", info.name),
                None => info.name.clone(),
            }
        })
}

/// Capabilities the client needs: progress, call hierarchy, references,
/// implementation, hierarchical document symbols, workspace folders and
/// configuration, and rust-analyzer's status notification. It does **not**
/// advertise `didChangeWatchedFiles` (P3-D18).
fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..WindowClientCapabilities::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            call_hierarchy: Some(Default::default()),
            references: Some(Default::default()),
            implementation: Some(Default::default()),
            definition: Some(Default::default()),
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..DocumentSymbolClientCapabilities::default()
            }),
            ..TextDocumentClientCapabilities::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            workspace_folders: Some(true),
            configuration: Some(true),
            symbol: Some(Default::default()),
            ..WorkspaceClientCapabilities::default()
        }),
        experimental: Some(json!({ "serverStatusNotification": true })),
        ..ClientCapabilities::default()
    }
}

/// Build the server → client router.
fn build_client_router(state: ClientState) -> Router<ClientState> {
    let mut router = Router::new(state);
    register_server_requests(&mut router);
    register_server_notifications(&mut router);
    router
        .event(|_, _: Stop| ControlFlow::Break(Ok(())))
        .unhandled_request(|state, request| {
            state.stats.note_request(request.method.as_str());
            tracing::debug!("unhandled server request: {}", request.method);
            async move { Ok(Value::Null) }
        })
        .unhandled_notification(|state, notification| {
            state.stats.bump(notification.method.as_str());
            ControlFlow::Continue(())
        });
    router
}

/// Requests a server may send; each must be answered or some servers hang.
fn register_server_requests(router: &mut Router<ClientState>) {
    router.request::<async_lsp::lsp_types::request::WorkspaceConfiguration, _>(|state, params| {
        state.stats.note_request("workspace/configuration");
        let replies = configuration_replies(&state.java_settings, &params);
        async move { Ok(replies) }
    });
    router.request::<async_lsp::lsp_types::request::WorkspaceFoldersRequest, _>(|state, _| {
        state.stats.note_request("workspace/workspaceFolders");
        let folder = state.workspace_folder.clone();
        async move { Ok(Some(vec![folder])) }
    });
    router.request::<async_lsp::lsp_types::request::ShowMessageRequest, _>(|state, params| {
        state.stats.note_request("window/showMessageRequest");
        state.stats.bump("window/showMessageRequest");
        tracing::info!("showMessageRequest: {:?} {}", params.typ, params.message);
        async move { Ok(None) }
    });
    register_unit_request::<async_lsp::lsp_types::request::WorkDoneProgressCreate>(
        router,
        "window/workDoneProgress/create",
    );
    register_unit_request::<async_lsp::lsp_types::request::RegisterCapability>(
        router,
        "client/registerCapability",
    );
    register_unit_request::<async_lsp::lsp_types::request::UnregisterCapability>(
        router,
        "client/unregisterCapability",
    );
}

/// `workspace/configuration`: `java*` sections get the jdtls settings subtree
/// of the initialization options, everything else `null`.
fn configuration_replies(java_settings: &Value, params: &ConfigurationParams) -> Vec<Value> {
    params
        .items
        .iter()
        .map(|item| {
            let wants_java = item
                .section
                .as_deref()
                .is_some_and(|section| section.starts_with("java"));
            if wants_java {
                java_settings.clone()
            } else {
                Value::Null
            }
        })
        .collect()
}

fn register_unit_request<R>(router: &mut Router<ClientState>, label: &'static str)
where
    R: Request<Result = ()>,
{
    router.request::<R, _>(move |state, _| {
        state.stats.note_request(label);
        async move { Ok::<(), ResponseError>(()) }
    });
}

/// Notifications from the server: feed readiness, count the rest.
fn register_server_notifications(router: &mut Router<ClientState>) {
    router.notification::<async_lsp::lsp_types::notification::Progress>(|state, progress| {
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
    register_ignored_notification::<async_lsp::lsp_types::notification::PublishDiagnostics>(
        router,
        "textDocument/publishDiagnostics",
    );
    register_ignored_notification::<async_lsp::lsp_types::notification::LogMessage>(
        router,
        "window/logMessage",
    );
    register_ignored_notification::<async_lsp::lsp_types::notification::ShowMessage>(
        router,
        "window/showMessage",
    );
    register_ignored_notification::<async_lsp::lsp_types::notification::TelemetryEvent>(
        router,
        "telemetry/event",
    );
}

fn register_ignored_notification<N>(router: &mut Router<ClientState>, label: &'static str)
where
    N: Notification,
{
    router.notification::<N>(move |state, _| {
        state.stats.bump(label);
        ControlFlow::Continue(())
    });
}

fn handle_progress(state: &ClientState, token: NumberOrString, value: ProgressParamsValue) {
    let token = match token {
        NumberOrString::String(text) => text,
        NumberOrString::Number(number) => number.to_string(),
    };
    state.ready_tx.send_modify(|ready| match value {
        ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(begin)) => {
            ready.on_progress_begin(token, begin.title);
        }
        ProgressParamsValue::WorkDone(WorkDoneProgress::Report(_)) => {
            ready.on_progress_report(&token);
        }
        ProgressParamsValue::WorkDone(WorkDoneProgress::End(_)) => ready.on_progress_end(&token),
    });
}

/// One attempt of a request, bounded by `per_try`.
async fn attempt<R>(
    server: &ServerSocket,
    params: R::Params,
    per_try: Duration,
) -> Result<R::Result, RequestError>
where
    R: Request,
{
    match tokio::time::timeout(per_try, server.request::<R>(params)).await {
        Err(_) => Err(RequestError::Timeout(per_try)),
        Ok(result) => result.map_err(RequestError::from),
    }
}

/// Send one request, retrying while the server asks for it (see [`RetryPolicy`]).
pub async fn request_with_retry<R>(
    server: &ServerSocket,
    params: R::Params,
    policy: RetryPolicy,
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
        match attempt::<R>(server, params.clone(), per_try).await {
            Err(error) if retries < policy.max_retries && error.is_retryable() => {
                retries += 1;
                tokio::time::sleep(policy.backoff).await;
            }
            result => {
                return RequestOutcome {
                    result,
                    latency: try_started.elapsed(),
                    total: started.elapsed(),
                    retries,
                };
            }
        }
    }
}

fn position_params(uri: &Url, position: Position) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position,
    }
}

/// `Location`s of a `textDocument/implementation` answer, whatever its shape.
fn implementation_locations(response: Option<GotoDefinitionResponse>) -> Vec<Location> {
    match response {
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
    }
}

impl LspSession {
    /// What the server said it can do.
    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.init_result.capabilities
    }

    /// Whether the main loop is still running (it ends when the server exits).
    pub fn is_alive(&self) -> bool {
        !self.mainloop.is_finished()
    }

    /// Timeout of the next request: the first one after the handshake gets the
    /// longer, lazy-project-load allowance.
    pub fn next_timeout(&mut self) -> Duration {
        if self.first_request_done {
            self.request_timeout
        } else {
            self.first_request_done = true;
            self.first_request_timeout
        }
    }

    /// Number of documents currently open.
    pub fn opened_count(&self) -> usize {
        self.opened.len()
    }

    /// Open a document, or re-open it when the file changed since it was opened
    /// (`didClose` + `didOpen`, P3-D18). Idempotent for an unchanged file; the
    /// client never sends `didChange`.
    pub fn did_open(&mut self, abs: &Path) -> anyhow::Result<()> {
        let uri = path_to_uri(abs)?;
        let mtime = std::fs::metadata(abs)
            .and_then(|meta| meta.modified())
            .with_context(|| format!("reading the modification time of {}", abs.display()))?;
        match self.opened.get(&uri) {
            Some(opened_at) if *opened_at == mtime => return Ok(()),
            Some(_) => self
                .server
                .notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                })
                .map_err(|error| anyhow!("didClose failed: {error}"))?,
            None => {}
        }
        let text =
            std::fs::read_to_string(abs).with_context(|| format!("reading {}", abs.display()))?;
        self.server
            .notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: (self.language_id_for)(abs).to_owned(),
                    version: 0,
                    text,
                },
            })
            .map_err(|error| anyhow!("didOpen failed: {error}"))?;
        self.opened.insert(uri, mtime);
        Ok(())
    }

    /// `textDocument/didClose` for `abs`, if it was previously opened.
    pub fn did_close(&mut self, abs: &Path) -> anyhow::Result<()> {
        let uri = path_to_uri(abs)?;
        if self.opened.remove(&uri).is_some() {
            self.server
                .notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri },
                })
                .map_err(|error| anyhow!("didClose failed: {error}"))?;
        }
        Ok(())
    }

    /// `workspace/didChangeWatchedFiles` notification with `events` (P6-D11).
    pub fn did_change_watched_files(&mut self, events: Vec<FileEvent>) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        self.server
            .notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams { changes: events })
            .map_err(|error| anyhow!("didChangeWatchedFiles failed: {error}"))
    }

    /// Wait for the server to settle, bounded by `timeout`.
    pub async fn wait_settle(&mut self, timeout: Duration) {
        let _ = crate::lsp::readiness::wait_ready(
            &mut self.events,
            crate::lsp::readiness::ReadinessKind::ProgressIdle,
            Duration::from_millis(500),
            timeout,
        )
        .await;
    }

    /// `textDocument/prepareCallHierarchy` at `position`.
    pub async fn prepare_call_hierarchy(
        &self,
        uri: &Url,
        position: Position,
        per_try: Duration,
    ) -> RequestOutcome<Option<Vec<CallHierarchyItem>>> {
        let params = CallHierarchyPrepareParams {
            text_document_position_params: position_params(uri, position),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        request_with_retry::<CallHierarchyPrepare>(&self.server, params, self.retry, per_try).await
    }

    /// `callHierarchy/incomingCalls` for one item.
    pub async fn incoming_calls(
        &self,
        item: CallHierarchyItem,
        per_try: Duration,
    ) -> RequestOutcome<Option<Vec<CallHierarchyIncomingCall>>> {
        let params = CallHierarchyIncomingCallsParams {
            item,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        request_with_retry::<CallHierarchyIncomingCalls>(&self.server, params, self.retry, per_try)
            .await
    }

    /// `textDocument/references` at `position`.
    pub async fn references(
        &self,
        uri: &Url,
        position: Position,
        include_declaration: bool,
        per_try: Duration,
    ) -> RequestOutcome<Option<Vec<Location>>> {
        let params = ReferenceParams {
            text_document_position: position_params(uri, position),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration,
            },
        };
        request_with_retry::<References>(&self.server, params, self.retry, per_try).await
    }

    /// `textDocument/implementation` at `position`, as a list of locations.
    pub async fn implementation(
        &self,
        uri: &Url,
        position: Position,
        per_try: Duration,
    ) -> RequestOutcome<Vec<Location>> {
        let params = GotoDefinitionParams {
            text_document_position_params: position_params(uri, position),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        let outcome =
            request_with_retry::<GotoImplementation>(&self.server, params, self.retry, per_try)
                .await;
        RequestOutcome {
            result: outcome.result.map(implementation_locations),
            latency: outcome.latency,
            total: outcome.total,
            retries: outcome.retries,
        }
    }

    /// `textDocument/documentSymbol` of one document.
    pub async fn document_symbol(
        &self,
        uri: &Url,
        per_try: Duration,
    ) -> RequestOutcome<Option<DocumentSymbolResponse>> {
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        request_with_retry::<DocumentSymbolRequest>(&self.server, params, self.retry, per_try).await
    }

    /// Graceful `shutdown` + `exit`, then stop the main loop; every step is
    /// bounded by `timeout`. Reaping the server process is the caller's job.
    pub async fn shutdown(self, timeout: Duration) -> ShutdownOutcome {
        let Self {
            server,
            mut mainloop,
            ..
        } = self;
        let handshake = tokio::time::timeout(timeout, async {
            server.request::<Shutdown>(()).await?;
            server.notify::<Exit>(())
        })
        .await;
        let handshake = matches!(handshake, Ok(Ok(())));
        if !handshake {
            tracing::warn!("LSP shutdown handshake failed or timed out after {timeout:?}");
        }
        // Break the loop even when the handshake did not complete.
        let _ = server.emit(Stop);
        let loop_ended = match tokio::time::timeout(timeout, &mut mainloop).await {
            Ok(Ok(Ok(()))) => true,
            Ok(Ok(Err(error))) => {
                tracing::debug!("LSP main loop ended with: {error:#}");
                true
            }
            Ok(Err(error)) => {
                tracing::warn!("LSP main loop join failed: {error}");
                true
            }
            Err(_) => {
                tracing::warn!("LSP main loop did not finish within {timeout:?}; aborting it");
                mainloop.abort();
                false
            }
        };
        ShutdownOutcome {
            handshake,
            loop_ended,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use async_lsp::lsp_types::{Range, ServerInfo, SymbolKind};

    use super::*;
    use crate::capability::languages::LspLang;
    use crate::lsp::fake::{
        FakeHandle, FakeNotification, FakeScript, FakeServerRequest, ImplementationShape, connect,
        eventually, test_spec,
    };

    const PER_TRY: Duration = Duration::from_secs(5);

    /// An absolute path that need not exist (the fake never reads it).
    fn demo_root() -> PathBuf {
        std::env::temp_dir().join("knot-lite-demo")
    }

    /// Connect to a fake server for `lang` with a 1 ms retry back-off.
    async fn connect_as(lang: LspLang, script: FakeScript) -> (LspSession, FakeHandle) {
        let spec = test_spec(lang);
        let root = demo_root();
        let (mut session, handle) = connect(
            script,
            InitInputs {
                spec: &spec,
                repo_root: &root,
            },
        )
        .await;
        session.retry.backoff = Duration::from_millis(1);
        (session, handle)
    }

    async fn connect_rust(script: FakeScript) -> (LspSession, FakeHandle) {
        connect_as(LspLang::Rust, script).await
    }

    fn position(line: u32, character: u32) -> Position {
        Position::new(line, character)
    }

    fn file_uri(path: &str) -> Url {
        Url::parse(&format!("file:///work/demo/{path}")).expect("uri")
    }

    fn location(path: &str, line: u32) -> Location {
        Location {
            uri: file_uri(path),
            range: Range::new(position(line, 4), position(line, 9)),
        }
    }

    fn item(name: &str, path: &str, line: u32) -> CallHierarchyItem {
        let range = Range::new(position(line, 0), position(line + 2, 1));
        CallHierarchyItem {
            name: name.to_owned(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            detail: None,
            uri: file_uri(path),
            range,
            selection_range: Range::new(position(line, 7), position(line, 7 + 6)),
            data: None,
        }
    }

    /// A script that answers `references` at (3, 7) with two locations.
    fn references_script() -> FakeScript {
        FakeScript {
            references: vec![(
                position(3, 7),
                vec![location("src/a.rs", 10), location("src/b.rs", 20)],
            )],
            ..FakeScript::full()
        }
    }

    async fn references_at_3_7(session: &LspSession) -> RequestOutcome<Option<Vec<Location>>> {
        session
            .references(&file_uri("src/lib.rs"), position(3, 7), false, PER_TRY)
            .await
    }

    #[tokio::test]
    async fn initialize_sends_root_uri_and_workspace_folder() {
        let (_session, handle) = connect_rust(FakeScript::full()).await;
        let json = handle.counters.initialize_json().expect("initialize seen");
        let root_uri = path_to_uri(&demo_root()).expect("uri");
        assert_eq!(json["rootUri"], json!(root_uri.as_str()));
        assert_eq!(json["workspaceFolders"][0]["uri"], json["rootUri"]);
        assert_eq!(json["workspaceFolders"][0]["name"], json!("knot-lite-demo"));
        assert_eq!(json["clientInfo"]["name"], json!("knot-lite"));
        assert_eq!(json["processId"], json!(std::process::id()));
        assert!(json.get("rootPath").is_none(), "{json}");
        assert_eq!(
            json["initializationOptions"],
            test_spec(LspLang::Rust).initialization_options
        );
    }

    #[tokio::test]
    async fn client_capabilities_do_not_advertise_file_watching() {
        let (_session, handle) = connect_rust(FakeScript::full()).await;
        let json = handle.counters.initialize_json().expect("initialize seen");
        let capabilities = &json["capabilities"];
        assert_eq!(capabilities["window"]["workDoneProgress"], json!(true));
        assert_eq!(
            capabilities["experimental"]["serverStatusNotification"],
            json!(true)
        );
        assert!(
            capabilities["workspace"]
                .get("didChangeWatchedFiles")
                .is_none(),
            "P3-D18: the client never reports file changes"
        );
    }

    #[tokio::test]
    async fn server_version_comes_from_server_info() {
        let info = |name: &str, version: Option<&str>| ServerInfo {
            name: name.to_owned(),
            version: version.map(str::to_owned),
        };
        let cases = [
            (
                Some(info("rust-analyzer", Some("1.98.1"))),
                "rust-analyzer 1.98.1",
            ),
            (Some(info("jdtls", None)), "jdtls"),
            (Some(info("tsserver", Some(""))), "tsserver"),
            (None, ""),
        ];
        for (server_info, expected) in cases {
            let script = FakeScript {
                server_info,
                ..FakeScript::full()
            };
            let (session, _handle) = connect_rust(script).await;
            assert_eq!(session.server_version, expected);
        }
    }

    #[tokio::test]
    async fn configuration_request_gets_java_section_or_null() {
        let script = FakeScript {
            server_requests: vec![FakeServerRequest::Configuration(vec![
                "java.import",
                "editor",
            ])],
            ..FakeScript::full()
        };
        let (session, handle) = connect_as(LspLang::Java, script).await;
        eventually("the configuration reply", || {
            !handle.counters.replies().is_empty()
        })
        .await;
        let java_settings =
            test_spec(LspLang::Java).initialization_options["settings"]["java"].clone();
        assert!(!java_settings.is_null());
        assert_eq!(
            handle.counters.replies(),
            vec![(
                "workspace/configuration".to_owned(),
                json!([java_settings, null])
            )]
        );
        assert_eq!(
            session.stats.seen_requests(),
            vec!["workspace/configuration"]
        );
    }

    #[tokio::test]
    async fn unhandled_server_request_gets_null() {
        let script = FakeScript {
            server_requests: vec![FakeServerRequest::Unknown],
            ..FakeScript::full()
        };
        let (session, handle) = connect_rust(script).await;
        eventually("the reply", || !handle.counters.replies().is_empty()).await;
        assert_eq!(
            handle.counters.replies(),
            vec![("custom/unknown".to_owned(), Value::Null)]
        );
        assert_eq!(session.stats.seen_requests(), vec!["custom/unknown"]);
    }

    #[tokio::test]
    async fn known_server_requests_are_answered() {
        let script = FakeScript {
            server_requests: vec![
                FakeServerRequest::WorkspaceFolders,
                FakeServerRequest::ShowMessage,
                FakeServerRequest::WorkDoneProgressCreate,
                FakeServerRequest::RegisterCapability,
            ],
            ..FakeScript::full()
        };
        let (session, handle) = connect_rust(script).await;
        eventually("four replies", || handle.counters.replies().len() == 4).await;
        let folder = json!([{
            "uri": path_to_uri(&demo_root()).expect("uri").as_str(),
            "name": "knot-lite-demo",
        }]);
        assert_eq!(
            handle.counters.replies(),
            vec![
                ("workspace/workspaceFolders".to_owned(), folder),
                ("window/showMessageRequest".to_owned(), Value::Null),
                ("window/workDoneProgress/create".to_owned(), Value::Null),
                ("client/registerCapability".to_owned(), Value::Null),
            ]
        );
        assert_eq!(session.stats.count_of("window/showMessageRequest"), 1);
    }

    #[tokio::test]
    async fn progress_and_status_feed_readiness() {
        let script = FakeScript {
            on_initialized: vec![
                FakeNotification::ProgressBegin {
                    token: "rustAnalyzer/Indexing",
                    title: "Indexing",
                },
                FakeNotification::ServerStatus {
                    health: "ok",
                    quiescent: false,
                },
                FakeNotification::ProgressEnd {
                    token: "rustAnalyzer/Indexing",
                },
                FakeNotification::LanguageStatus {
                    kind: "ServiceReady",
                },
                FakeNotification::ServerStatus {
                    health: "ok",
                    quiescent: true,
                },
            ],
            ..FakeScript::full()
        };
        let (session, _handle) = connect_rust(script).await;
        eventually("a quiescent status", || {
            session.events.borrow().quiescent_known() == Some(true)
        })
        .await;
        let state = session.events.borrow().clone();
        assert_eq!(state.progress_pairs, 1);
        assert!(state.open_progress.is_empty());
        assert!(state.ra_indexing_ended);
        assert_eq!(state.ra_health.as_deref(), Some("ok"));
        assert_eq!(state.jdtls_status.as_deref(), Some("ServiceReady"));
    }

    #[tokio::test]
    async fn references_round_trip() {
        let (session, handle) = connect_rust(references_script()).await;
        let outcome = references_at_3_7(&session).await;
        assert_eq!(
            outcome.result,
            Ok(Some(vec![
                location("src/a.rs", 10),
                location("src/b.rs", 20)
            ]))
        );
        assert_eq!(outcome.retries, 0);
        assert_eq!(handle.counters.requests(), vec!["textDocument/references"]);
        let unknown_position = session
            .references(&file_uri("src/lib.rs"), position(9, 9), false, PER_TRY)
            .await;
        assert_eq!(unknown_position.result, Ok(Some(Vec::new())));
    }

    #[tokio::test]
    async fn call_hierarchy_round_trip() {
        let target = item("target", "src/lib.rs", 2);
        let caller = CallHierarchyIncomingCall {
            from: item("caller", "src/a.rs", 6),
            from_ranges: vec![
                Range::new(position(7, 4), position(7, 10)),
                Range::new(position(8, 4), position(8, 10)),
            ],
        };
        let script = FakeScript {
            prepare: vec![(position(2, 7), vec![target.clone()])],
            incoming: vec![("target".to_owned(), vec![caller.clone()])],
            ..FakeScript::full()
        };
        let (session, handle) = connect_rust(script).await;
        let prepared = session
            .prepare_call_hierarchy(&file_uri("src/lib.rs"), position(2, 7), PER_TRY)
            .await;
        assert_eq!(prepared.result, Ok(Some(vec![target.clone()])));
        let incoming = session.incoming_calls(target, PER_TRY).await;
        assert_eq!(incoming.result, Ok(Some(vec![caller])));
        assert_eq!(
            handle.counters.requests(),
            vec![
                "textDocument/prepareCallHierarchy",
                "callHierarchy/incomingCalls"
            ]
        );
    }

    #[tokio::test]
    async fn implementation_link_is_normalised_to_location() {
        let locations = vec![location("src/a.rs", 1), location("src/b.rs", 2)];
        let cases = [
            (ImplementationShape::Array, locations.clone()),
            (ImplementationShape::Scalar, vec![locations[0].clone()]),
            (ImplementationShape::Link, locations.clone()),
        ];
        for (shape, expected) in cases {
            let script = FakeScript {
                implementation: vec![(position(4, 4), locations.clone())],
                implementation_shape: shape,
                ..FakeScript::full()
            };
            let (session, _handle) = connect_rust(script).await;
            let outcome = session
                .implementation(&file_uri("src/lib.rs"), position(4, 4), PER_TRY)
                .await;
            assert_eq!(outcome.result, Ok(expected));
            let none = session
                .implementation(&file_uri("src/lib.rs"), position(0, 0), PER_TRY)
                .await;
            assert_eq!(none.result, Ok(Vec::new()), "no implementation");
        }
    }

    #[tokio::test]
    async fn content_modified_is_retried_then_succeeds() {
        let script = FakeScript {
            content_modified_first: 2,
            ..references_script()
        };
        let (session, handle) = connect_rust(script).await;
        let outcome = references_at_3_7(&session).await;
        assert_eq!(
            outcome.result.map(|found| found.map(|v| v.len())),
            Ok(Some(2))
        );
        assert_eq!(outcome.retries, 2);
        assert_eq!(handle.counters.requests_of("textDocument/references"), 3);
    }

    #[tokio::test]
    async fn retries_are_bounded() {
        let script = FakeScript {
            content_modified_first: 100,
            ..references_script()
        };
        let (session, handle) = connect_rust(script).await;
        let outcome = references_at_3_7(&session).await;
        assert_eq!(
            outcome.result,
            Err(RequestError::Response {
                code: ErrorCode::CONTENT_MODIFIED.0,
                message: "content modified".to_owned(),
            })
        );
        assert_eq!(outcome.retries, 5);
        assert_eq!(handle.counters.requests_of("textDocument/references"), 6);
    }

    #[tokio::test]
    async fn timeout_is_reported_as_timeout() {
        let script = FakeScript {
            delay: Duration::from_millis(400),
            ..references_script()
        };
        let (session, _handle) = connect_rust(script).await;
        let per_try = Duration::from_millis(50);
        let outcome = session
            .references(&file_uri("src/lib.rs"), position(3, 7), false, per_try)
            .await;
        assert_eq!(outcome.result, Err(RequestError::Timeout(per_try)));
        assert_eq!(outcome.retries, 0, "a timeout is not retried");
    }

    #[test]
    fn only_server_cancellations_are_retryable() {
        let response = |code: ErrorCode| RequestError::Response {
            code: code.0,
            message: String::new(),
        };
        assert!(response(ErrorCode::CONTENT_MODIFIED).is_retryable());
        assert!(response(ErrorCode::SERVER_CANCELLED).is_retryable());
        assert!(response(ErrorCode::REQUEST_CANCELLED).is_retryable());
        assert!(!response(ErrorCode::REQUEST_FAILED).is_retryable());
        assert!(!RequestError::Timeout(Duration::ZERO).is_retryable());
        assert!(!RequestError::Transport("gone".to_owned()).is_retryable());
    }

    #[tokio::test]
    async fn first_request_gets_the_longer_timeout() {
        let (mut session, _handle) = connect_rust(FakeScript::full()).await;
        let spec = test_spec(LspLang::Rust);
        assert_eq!(session.next_timeout(), spec.first_request_timeout);
        assert_eq!(session.next_timeout(), spec.request_timeout);
        assert_eq!(session.next_timeout(), spec.request_timeout);
    }

    /// A source file in a temp dir.
    fn source_file(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, "pub fn a() {}\n").expect("write");
        path
    }

    /// Round-trip a request so every earlier notification has reached the fake.
    async fn barrier(session: &LspSession) {
        let _ = references_at_3_7(session).await;
    }

    #[tokio::test]
    async fn did_open_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = source_file(&dir, "a.rs");
        let (mut session, handle) = connect_rust(references_script()).await;
        session.did_open(&path).expect("first open");
        session.did_open(&path).expect("second open");
        barrier(&session).await;
        assert_eq!(handle.counters.did_open_count(), 1);
        assert_eq!(handle.counters.did_close_count(), 0);
        assert_eq!(session.opened_count(), 1);
    }

    #[tokio::test]
    async fn did_open_reopens_after_mtime_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = source_file(&dir, "a.rs");
        let (mut session, handle) = connect_rust(references_script()).await;
        session.did_open(&path).expect("open");
        let later = SystemTime::now() + Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open for write")
            .set_modified(later)
            .expect("set mtime");
        session.did_open(&path).expect("reopen");
        session.did_open(&path).expect("unchanged since the reopen");
        barrier(&session).await;
        assert_eq!(handle.counters.did_close_count(), 1);
        assert_eq!(handle.counters.did_open_count(), 2);
        assert_eq!(session.opened_count(), 1, "still one document");
    }

    #[tokio::test]
    async fn did_open_of_a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut session, handle) = connect_rust(references_script()).await;
        let error = session
            .did_open(&dir.path().join("missing.rs"))
            .expect_err("missing file");
        assert!(error.to_string().contains("missing.rs"), "{error:#}");
        barrier(&session).await;
        assert_eq!(handle.counters.did_open_count(), 0);
    }

    #[tokio::test]
    async fn dead_server_is_not_alive() {
        let (session, handle) = connect_rust(references_script()).await;
        assert!(session.is_alive());
        handle.kill();
        eventually("the main loop to end", || !session.is_alive()).await;
        let outcome = references_at_3_7(&session).await;
        assert!(
            matches!(outcome.result, Err(RequestError::Transport(_))),
            "{:?}",
            outcome.result
        );
    }

    #[tokio::test]
    async fn shutdown_completes_against_fake() {
        let (session, handle) = connect_rust(references_script()).await;
        let outcome = session.shutdown(Duration::from_secs(5)).await;
        assert_eq!(
            outcome,
            ShutdownOutcome {
                handshake: true,
                loop_ended: true
            }
        );
        eventually("exit", || handle.counters.exit_seen()).await;
        assert!(handle.counters.shutdown_seen());
    }

    #[tokio::test]
    async fn shutdown_of_a_dead_server_still_ends_the_loop() {
        let (session, handle) = connect_rust(references_script()).await;
        handle.kill();
        eventually("the main loop to end", || !session.is_alive()).await;
        let outcome = session.shutdown(Duration::from_millis(500)).await;
        assert!(!outcome.handshake);
        assert!(outcome.loop_ended);
    }
}
