//! A scripted language server living in the test process (P3-D13).
//!
//! It speaks real LSP over a [`tokio::io::duplex`] pipe through `async-lsp`'s
//! server main loop, so the client under test exercises its actual framing,
//! router and retry code without spawning anything. Every behaviour a test can
//! script (readiness notifications, server → client requests, injected
//! `ContentModified` errors, delays) is copied from something a real server
//! did in Phase 0.

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::notification::{
    DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized, Progress,
};
use async_lsp::lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyPrepare, GotoImplementation, References,
    RegisterCapability, Request, ShowMessageRequest, Shutdown, WorkDoneProgressCreate,
    WorkspaceConfiguration, WorkspaceFoldersRequest,
};
use async_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyServerCapability, ConfigurationItem,
    ConfigurationParams, GotoDefinitionResponse, ImplementationProviderCapability,
    InitializeResult, Location, LocationLink, MessageType, NumberOrString, OneOf, Position,
    ProgressParams, ProgressParamsValue, RegistrationParams, ServerCapabilities, ServerInfo,
    ShowMessageRequestParams, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd,
};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::{ClientSocket, ErrorCode, MainLoop, ResponseError};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;

use super::session::{
    InitInputs, LanguageStatusNotification, LanguageStatusParams, LspSession, RawInitialize,
    ServerStatusNotification, ServerStatusParams, SessionIo, initialize,
};
use super::spec::{ServerSpec, SpecInputs, spec_for};
use crate::capability::languages::LspLang;

/// Bytes buffered by each direction of the in-memory pipe.
const PIPE_BYTES: usize = 1 << 16;

/// A notification the fake sends right after `initialized`.
pub(crate) enum FakeNotification {
    /// `experimental/serverStatus` (rust-analyzer).
    ServerStatus {
        health: &'static str,
        quiescent: bool,
    },
    /// `$/progress` begin.
    ProgressBegin {
        token: &'static str,
        title: &'static str,
    },
    /// `$/progress` end.
    ProgressEnd { token: &'static str },
    /// `language/status` (jdtls).
    LanguageStatus { kind: &'static str },
}

/// A server → client request the fake sends right after `initialized`.
pub(crate) enum FakeServerRequest {
    /// `workspace/configuration` with these sections.
    Configuration(Vec<&'static str>),
    /// `workspace/workspaceFolders`.
    WorkspaceFolders,
    /// `window/showMessageRequest` (jdtls asks the user about builds).
    ShowMessage,
    /// `window/workDoneProgress/create`.
    WorkDoneProgressCreate,
    /// `client/registerCapability`.
    RegisterCapability,
    /// A method the client has never heard of.
    Unknown,
}

/// The shape of the `textDocument/implementation` answer (servers differ).
#[derive(Clone, Copy, Default)]
pub(crate) enum ImplementationShape {
    /// `Location[]`.
    #[default]
    Array,
    /// A single `Location` (the first one).
    Scalar,
    /// `LocationLink[]`.
    Link,
}

/// What the fake server knows and does.
#[derive(Default)]
pub(crate) struct FakeScript {
    /// Answered to `initialize`.
    pub capabilities: ServerCapabilities,
    /// Answered to `initialize`.
    pub server_info: Option<ServerInfo>,
    /// Sent after `initialized`, in order.
    pub on_initialized: Vec<FakeNotification>,
    /// Sent after the notifications, in order, each awaited.
    pub server_requests: Vec<FakeServerRequest>,
    /// `prepareCallHierarchy`, by position.
    pub prepare: Vec<(Position, Vec<CallHierarchyItem>)>,
    /// `incomingCalls`, by the item's name.
    pub incoming: Vec<(String, Vec<CallHierarchyIncomingCall>)>,
    /// `references`, by position.
    pub references: Vec<(Position, Vec<Location>)>,
    /// `implementation`, by position.
    pub implementation: Vec<(Position, Vec<Location>)>,
    /// How the implementation answer is shaped.
    pub implementation_shape: ImplementationShape,
    /// The first this many requests fail with `ContentModified`.
    pub content_modified_first: usize,
    /// Every request waits this long before it is answered.
    pub delay: Duration,
}

impl FakeScript {
    /// A server that advertises call hierarchy, references and implementation.
    pub(crate) fn full() -> Self {
        Self {
            capabilities: ServerCapabilities {
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                ..ServerCapabilities::default()
            },
            ..Self::default()
        }
    }
}

/// What the fake observed, for assertions.
#[derive(Default)]
pub(crate) struct FakeCounters {
    requests: Mutex<Vec<String>>,
    initialize_json: Mutex<Option<Value>>,
    replies: Mutex<Vec<(String, Value)>>,
    did_open: AtomicUsize,
    did_close: AtomicUsize,
    shutdown_seen: AtomicBool,
    exit_seen: AtomicBool,
    content_modified_left: AtomicUsize,
}

impl FakeCounters {
    /// Methods of every client request after `initialize`, in arrival order.
    pub(crate) fn requests(&self) -> Vec<String> {
        lock(&self.requests).clone()
    }

    /// How many requests of `method` arrived.
    pub(crate) fn requests_of(&self, method: &str) -> usize {
        lock(&self.requests).iter().filter(|m| *m == method).count()
    }

    /// The raw `initialize` parameters.
    pub(crate) fn initialize_json(&self) -> Option<Value> {
        lock(&self.initialize_json).clone()
    }

    /// `(label, reply)` of every server → client request the client answered.
    pub(crate) fn replies(&self) -> Vec<(String, Value)> {
        lock(&self.replies).clone()
    }

    /// `didOpen` notifications received.
    pub(crate) fn did_open_count(&self) -> usize {
        self.did_open.load(Ordering::SeqCst)
    }

    /// `didClose` notifications received.
    pub(crate) fn did_close_count(&self) -> usize {
        self.did_close.load(Ordering::SeqCst)
    }

    /// Whether `shutdown` was requested.
    pub(crate) fn shutdown_seen(&self) -> bool {
        self.shutdown_seen.load(Ordering::SeqCst)
    }

    /// Whether `exit` was notified.
    pub(crate) fn exit_seen(&self) -> bool {
        self.exit_seen.load(Ordering::SeqCst)
    }

    /// Consume one injected `ContentModified` failure, if any is left.
    fn take_content_modified(&self) -> bool {
        self.content_modified_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
    }
}

/// The test's grip on the fake server.
pub(crate) struct FakeHandle {
    /// What the fake observed.
    pub(crate) counters: Arc<FakeCounters>,
    task: JoinHandle<()>,
}

impl FakeHandle {
    /// Kill the server: its end of the pipe closes, the client sees EOF.
    pub(crate) fn kill(&self) {
        self.task.abort();
    }
}

struct FakeState {
    script: Arc<FakeScript>,
    counters: Arc<FakeCounters>,
    client: ClientSocket,
}

/// A launch spec for `lang` over a made-up repository (nothing is spawned).
pub(crate) fn test_spec(lang: LspLang) -> ServerSpec {
    spec_for(&SpecInputs {
        lang,
        program: Path::new("fake-server"),
        repo_root: Path::new("/work/demo"),
        scratch: Path::new("/cache/demo-0123456789abcdef"),
        build_exec: true,
        preopen: &super::preopen::PreopenPlan::default(),
    })
}

/// Start the fake server and run the real handshake against it.
pub(crate) async fn connect(script: FakeScript, init: InitInputs<'_>) -> (LspSession, FakeHandle) {
    let (session, handle) = connect_result(script, init).await;
    (session.expect("handshake with the fake server"), handle)
}

/// [`connect`] that hands the handshake result to the test.
pub(crate) async fn connect_result(
    script: FakeScript,
    init: InitInputs<'_>,
) -> (anyhow::Result<LspSession>, FakeHandle) {
    let script = Arc::new(script);
    let counters = Arc::new(FakeCounters::default());
    counters
        .content_modified_left
        .store(script.content_modified_first, Ordering::SeqCst);

    let (client_write, server_read) = tokio::io::duplex(PIPE_BYTES);
    let (server_write, client_read) = tokio::io::duplex(PIPE_BYTES);
    let (mainloop, _client) = MainLoop::new_server(|client| {
        ServiceBuilder::new()
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .service(build_router(FakeState {
                script: Arc::clone(&script),
                counters: Arc::clone(&counters),
                client,
            }))
    });
    let task = tokio::spawn(async move {
        let _ = mainloop
            .run_buffered(server_read.compat(), server_write.compat_write())
            .await;
    });
    let session = initialize(
        SessionIo {
            read: client_read.compat(),
            write: client_write.compat_write(),
        },
        init,
    )
    .await;
    (session, FakeHandle { counters, task })
}

/// Poll `condition` every 10 ms for up to two seconds.
pub(crate) async fn eventually(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Record the request, then wait the scripted delay and fail the first N
/// requests with `ContentModified`.
fn gate(
    state: &FakeState,
    method: &str,
) -> impl Future<Output = Result<(), ResponseError>> + Send + use<> {
    lock(&state.counters.requests).push(method.to_owned());
    let delay = state.script.delay;
    let fail = state.counters.take_content_modified();
    async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if fail {
            Err(ResponseError::new(
                ErrorCode::CONTENT_MODIFIED,
                "content modified",
            ))
        } else {
            Ok(())
        }
    }
}

/// The value stored for `position`, or nothing.
fn by_position<T: Clone>(entries: &[(Position, Vec<T>)], position: Position) -> Vec<T> {
    entries
        .iter()
        .find(|(candidate, _)| *candidate == position)
        .map(|(_, values)| values.clone())
        .unwrap_or_default()
}

fn build_router(state: FakeState) -> Router<FakeState> {
    let mut router = Router::new(state);
    register_lifecycle(&mut router);
    register_queries(&mut router);
    router
        .notification::<DidOpenTextDocument>(|state, _| {
            state.counters.did_open.fetch_add(1, Ordering::SeqCst);
            ControlFlow::Continue(())
        })
        .notification::<DidCloseTextDocument>(|state, _| {
            state.counters.did_close.fetch_add(1, Ordering::SeqCst);
            ControlFlow::Continue(())
        })
        .unhandled_notification(|_, _| ControlFlow::Continue(()));
    router
}

fn register_lifecycle(router: &mut Router<FakeState>) {
    router
        .request::<RawInitialize, _>(|state, params| {
            *lock(&state.counters.initialize_json) = Some(params);
            let result = InitializeResult {
                capabilities: state.script.capabilities.clone(),
                server_info: state.script.server_info.clone(),
            };
            async move { Ok::<_, ResponseError>(result) }
        })
        .notification::<Initialized>(|state, _| {
            tokio::spawn(run_post_init(
                state.client.clone(),
                Arc::clone(&state.script),
                Arc::clone(&state.counters),
            ));
            ControlFlow::Continue(())
        })
        .request::<Shutdown, _>(|state, ()| {
            state.counters.shutdown_seen.store(true, Ordering::SeqCst);
            async move { Ok::<_, ResponseError>(()) }
        })
        .notification::<Exit>(|state, ()| {
            state.counters.exit_seen.store(true, Ordering::SeqCst);
            ControlFlow::Break(Ok(()))
        });
}

fn register_queries(router: &mut Router<FakeState>) {
    router.request::<CallHierarchyPrepare, _>(|state, params| {
        let gate = gate(state, CallHierarchyPrepare::METHOD);
        let position = params.text_document_position_params.position;
        let items = by_position(&state.script.prepare, position);
        async move {
            gate.await?;
            Ok::<_, ResponseError>(Some(items))
        }
    });
    router.request::<CallHierarchyIncomingCalls, _>(|state, params| {
        let gate = gate(state, CallHierarchyIncomingCalls::METHOD);
        let calls = state
            .script
            .incoming
            .iter()
            .find(|(name, _)| *name == params.item.name)
            .map(|(_, calls)| calls.clone())
            .unwrap_or_default();
        async move {
            gate.await?;
            Ok::<_, ResponseError>(Some(calls))
        }
    });
    router.request::<References, _>(|state, params| {
        let gate = gate(state, References::METHOD);
        let position = params.text_document_position.position;
        let locations = by_position(&state.script.references, position);
        async move {
            gate.await?;
            Ok::<_, ResponseError>(Some(locations))
        }
    });
    router.request::<GotoImplementation, _>(|state, params| {
        let gate = gate(state, GotoImplementation::METHOD);
        let position = params.text_document_position_params.position;
        let locations = by_position(&state.script.implementation, position);
        let shape = state.script.implementation_shape;
        async move {
            gate.await?;
            Ok::<_, ResponseError>(implementation_answer(shape, locations))
        }
    });
}

fn implementation_answer(
    shape: ImplementationShape,
    locations: Vec<Location>,
) -> Option<GotoDefinitionResponse> {
    if locations.is_empty() {
        return None;
    }
    Some(match shape {
        ImplementationShape::Array => GotoDefinitionResponse::Array(locations),
        ImplementationShape::Scalar => GotoDefinitionResponse::Scalar(
            locations
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!("the list is not empty")),
        ),
        ImplementationShape::Link => GotoDefinitionResponse::Link(
            locations
                .into_iter()
                .map(|location| LocationLink {
                    origin_selection_range: None,
                    target_uri: location.uri,
                    target_range: location.range,
                    target_selection_range: location.range,
                })
                .collect(),
        ),
    })
}

/// What a real server does right after `initialized`.
async fn run_post_init(client: ClientSocket, script: Arc<FakeScript>, counters: Arc<FakeCounters>) {
    for notification in &script.on_initialized {
        send_notification(&client, notification);
    }
    for request in &script.server_requests {
        send_server_request(&client, request, &counters).await;
    }
}

fn send_notification(client: &ClientSocket, notification: &FakeNotification) {
    let sent = match notification {
        FakeNotification::ServerStatus { health, quiescent } => client
            .notify::<ServerStatusNotification>(ServerStatusParams {
                health: (*health).to_owned(),
                quiescent: *quiescent,
                message: None,
            }),
        FakeNotification::ProgressBegin { token, title } => {
            client.notify::<Progress>(progress(token, begin(title)))
        }
        FakeNotification::ProgressEnd { token } => client.notify::<Progress>(progress(
            token,
            WorkDoneProgress::End(WorkDoneProgressEnd { message: None }),
        )),
        FakeNotification::LanguageStatus { kind } => {
            client.notify::<LanguageStatusNotification>(LanguageStatusParams {
                kind: (*kind).to_owned(),
                message: String::new(),
            })
        }
    };
    assert!(sent.is_ok(), "the client is gone");
}

fn begin(title: &str) -> WorkDoneProgress {
    WorkDoneProgress::Begin(WorkDoneProgressBegin {
        title: title.to_owned(),
        cancellable: None,
        message: None,
        percentage: None,
    })
}

fn progress(token: &str, value: WorkDoneProgress) -> ProgressParams {
    ProgressParams {
        token: NumberOrString::String(token.to_owned()),
        value: ProgressParamsValue::WorkDone(value),
    }
}

/// Method of a request the client has no handler for.
const UNKNOWN_METHOD: &str = "custom/unknown";

/// A request type whose method no client handles.
enum UnknownRequest {}

impl Request for UnknownRequest {
    type Params = Value;
    type Result = Value;
    const METHOD: &'static str = UNKNOWN_METHOD;
}

/// Send one server → client request and record what the client answered.
async fn send_server_request(
    client: &ClientSocket,
    request: &FakeServerRequest,
    counters: &FakeCounters,
) {
    let (label, reply) = match request {
        FakeServerRequest::Configuration(sections) => {
            let items = sections
                .iter()
                .map(|section| ConfigurationItem {
                    scope_uri: None,
                    section: Some((*section).to_owned()),
                })
                .collect();
            let reply = client
                .request::<WorkspaceConfiguration>(ConfigurationParams { items })
                .await;
            ("workspace/configuration", to_json(reply))
        }
        FakeServerRequest::WorkspaceFolders => (
            "workspace/workspaceFolders",
            to_json(client.request::<WorkspaceFoldersRequest>(()).await),
        ),
        FakeServerRequest::ShowMessage => {
            let params = ShowMessageRequestParams {
                typ: MessageType::INFO,
                message: "Build the workspace?".to_owned(),
                actions: None,
            };
            (
                "window/showMessageRequest",
                to_json(client.request::<ShowMessageRequest>(params).await),
            )
        }
        FakeServerRequest::WorkDoneProgressCreate => {
            let params = WorkDoneProgressCreateParams {
                token: NumberOrString::String("fake".to_owned()),
            };
            (
                "window/workDoneProgress/create",
                to_json(client.request::<WorkDoneProgressCreate>(params).await),
            )
        }
        FakeServerRequest::RegisterCapability => {
            let params = RegistrationParams {
                registrations: Vec::new(),
            };
            (
                "client/registerCapability",
                to_json(client.request::<RegisterCapability>(params).await),
            )
        }
        FakeServerRequest::Unknown => (
            UNKNOWN_METHOD,
            to_json(client.request::<UnknownRequest>(json!({})).await),
        ),
    };
    lock(&counters.replies).push((label.to_owned(), reply));
}

/// A reply as JSON; an error becomes `{"error": "<message>"}`.
fn to_json<T: serde::Serialize>(reply: Result<T, async_lsp::Error>) -> Value {
    match reply {
        Ok(value) => serde_json::to_value(value).unwrap_or(Value::Null),
        Err(error) => json!({ "error": error.to_string() }),
    }
}
