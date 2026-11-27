//! The stdio transport of `knot-lite-mcp`.
//!
//! It behaves like the SDK's `StdioTransport` (one JSON-RPC message per line
//! on stdin, replies on stdout, lines over the size cap dropped) with one
//! difference: **no line that calls for a reply goes unanswered**. Only a
//! notification (a `method` without an `id`), a response and a blank line
//! get none. Everything else the server cannot serve is answered here, so the
//! client learns why its request failed instead of waiting for ever:
//!
//! - not JSON: the parse error (`-32700`, `id: null`);
//! - over the size cap: invalid request (`-32600`, `id: null`; the line is
//!   never read, so its id is unknown);
//! - JSON that is no JSON-RPC message the server reads (a scalar, an object
//!   without `method`, a wrong `jsonrpc`, an `id` or `params` of the wrong
//!   type): `-32600`, or `-32602` when only the shape of `params` is wrong,
//!   with the request's `id` when it is a string or an integer;
//! - a batch (an array): MCP dropped JSON-RPC batching and the SDK ignores
//!   them, so every request in it gets its own `-32600`.
//!
//! The replies go through the same writer as every other message, so they
//! never interleave with one.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_mcp_sdk::schema::RequestId;
use rust_mcp_sdk::schema::schema_utils::McpMessage as _;
use rust_mcp_sdk::schema::schema_utils::{
    ClientMessage, ClientMessages, MessageFromServer, SdkError, ServerMessage, ServerMessages,
};
use rust_mcp_sdk::{
    IoStream, McpDispatch, MessageDispatcher, Transport, TransportDispatcher, TransportError,
    TransportOptions,
};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, BufReader};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

type TransportResult<T> = Result<T, TransportError>;
type Sender = Arc<RwLock<Option<MessageDispatcher<ClientMessage>>>>;
type Pending = Arc<Mutex<HashMap<RequestId, oneshot::Sender<ClientMessage>>>>;

/// The JSON-RPC 2.0 reply to a line that is not JSON.
pub const PARSE_ERROR_REPLY: &str =
    r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#;

/// The JSON-RPC 2.0 reply to a line over the size cap of `max` bytes: it is
/// never read, so its id is unknown.
pub fn too_long_reply(max: usize) -> String {
    error_reply(
        &Value::Null,
        INVALID_REQUEST,
        &format!("Invalid Request: message exceeds {max} bytes"),
    )
}

/// Server-side stdio transport that answers unparseable lines.
pub struct StdioServerTransport {
    options: TransportOptions,
    shutdown: CancellationToken,
    is_shut_down: Mutex<bool>,
    message_sender: Sender,
    error_stream: RwLock<Option<IoStream>>,
    pending_requests: Pending,
}

impl StdioServerTransport {
    /// A transport over the process's stdin, stdout and stderr.
    pub fn new(options: TransportOptions) -> Self {
        Self {
            options,
            shutdown: CancellationToken::new(),
            is_shut_down: Mutex::new(false),
            message_sender: Arc::new(RwLock::new(None)),
            error_stream: RwLock::new(None),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// JSON-RPC 2.0 `Invalid Request`.
const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC 2.0 `Invalid params`.
const INVALID_PARAMS: i64 = -32602;

/// A JSON-RPC 2.0 error reply for the request `id` (`null` when unknown).
fn error_reply(id: &Value, code: i64, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

/// What one input line is.
#[derive(Debug)]
enum Incoming {
    /// A JSON-RPC message for the server.
    Message(Box<ClientMessages>),
    /// Nothing the server can serve: answered with these error replies.
    Rejected(Vec<String>),
    /// A notification or a response the server cannot read: no reply is due.
    Ignored,
    /// A blank line.
    Blank,
}

fn classify(line: &[u8]) -> Incoming {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Incoming::Blank;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Incoming::Rejected(vec![PARSE_ERROR_REPLY.to_owned()]);
    };
    match value {
        Value::Array(items) => Incoming::Rejected(batch_replies(&items)),
        Value::Object(_) => match serde_json::from_value::<ClientMessage>(value.clone()) {
            // The SDK reads a request whose id it cannot use (`null`, a
            // fraction, an object) as a notification and never answers it.
            Ok(message) if message.is_request() || !is_request_shaped(&value) => {
                Incoming::Message(Box::new(ClientMessages::Single(message)))
            }
            _ => {
                rejection(&value).map_or(Incoming::Ignored, |reply| Incoming::Rejected(vec![reply]))
            }
        },
        _ => Incoming::Rejected(vec![error_reply(
            &Value::Null,
            INVALID_REQUEST,
            "Invalid Request: a JSON-RPC message is a JSON object",
        )]),
    }
}

/// The id to answer `message` with: its `id` when that is a string or an
/// integer, `null` otherwise (JSON-RPC 2.0 §5.1).
fn reply_id(message: &Value) -> Value {
    match message.get("id") {
        Some(id @ Value::String(_)) => id.clone(),
        Some(id @ Value::Number(number)) if number.is_i64() || number.is_u64() => id.clone(),
        _ => Value::Null,
    }
}

/// Whether `message` is an object with both a `method` and an `id`: a
/// request, whatever the types of the two.
fn is_request_shaped(message: &Value) -> bool {
    message
        .as_object()
        .is_some_and(|object| object.contains_key("method") && object.contains_key("id"))
}

/// Whether a reply is due for `message`: not for a notification (a `method`
/// without an `id`) nor for a response (`result` or `error` without a `method`).
fn expects_reply(message: &Value) -> bool {
    let Some(object) = message.as_object() else {
        return true;
    };
    if object.contains_key("method") {
        object.contains_key("id")
    } else {
        !(object.contains_key("result") || object.contains_key("error"))
    }
}

/// The error reply for a JSON object the server cannot read as a message, or
/// `None` when no reply is due.
fn rejection(message: &Value) -> Option<String> {
    if !expects_reply(message) {
        return None;
    }
    let id = reply_id(message);
    let well_formed_but_for_params = message.get("jsonrpc") == Some(&Value::from("2.0"))
        && message.get("method").is_some_and(Value::is_string)
        && !id.is_null()
        && message
            .get("params")
            .is_some_and(|params| params.is_object() || params.is_array());
    Some(if well_formed_but_for_params {
        error_reply(
            &id,
            INVALID_PARAMS,
            "Invalid params: the request's params do not fit its method",
        )
    } else {
        error_reply(
            &id,
            INVALID_REQUEST,
            "Invalid Request: not a JSON-RPC 2.0 request the server can read",
        )
    })
}

/// The replies to a batch: one per request in it, or one with `id: null`
/// when it holds none.
fn batch_replies(items: &[Value]) -> Vec<String> {
    const MESSAGE: &str =
        "Invalid Request: JSON-RPC batches are not supported; send one message per line";
    let replies: Vec<String> = items
        .iter()
        .filter(|item| item.is_object() && expects_reply(item))
        .map(|item| error_reply(&reply_id(item), INVALID_REQUEST, MESSAGE))
        .collect();
    if replies.is_empty() {
        vec![error_reply(&Value::Null, INVALID_REQUEST, MESSAGE)]
    } else {
        replies
    }
}

/// One line read with a size cap.
#[derive(Debug, PartialEq, Eq)]
enum LineRead {
    /// A line (newline stripped) within the cap.
    Line(Vec<u8>),
    /// A line over the cap, discarded up to the next newline.
    TooLong,
    /// End of input.
    Eof,
}

/// Read one newline-terminated line, buffering at most `max` bytes of it.
async fn read_capped_line<R>(reader: &mut R, max: usize) -> std::io::Result<LineRead>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let read = (&mut *reader)
        .take(limit)
        .read_until(b'\n', &mut line)
        .await?;
    if read == 0 {
        return Ok(LineRead::Eof);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        return Ok(LineRead::Line(line));
    }
    if line.len() <= max {
        // The last line, without a newline.
        return Ok(LineRead::Line(line));
    }
    discard_to_newline(reader).await?;
    Ok(LineRead::TooLong)
}

/// Consume input up to and including the next newline (or the end).
async fn discard_to_newline<R>(reader: &mut R) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(());
        }
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1);
            return Ok(());
        }
        let len = chunk.len();
        reader.consume(len);
    }
}

/// Write `payload` (one JSON-RPC message) through the transport's writer.
async fn reply(sender: &Sender, payload: &str) -> TransportResult<()> {
    let guard = sender.read().await;
    match guard.as_ref() {
        Some(dispatcher) => {
            McpDispatch::<ClientMessages, ServerMessages, ClientMessage, ServerMessage>::write_str(
                dispatcher, payload, true,
            )
            .await
        }
        None => Ok(()),
    }
}

/// Read messages from `readable` until it ends or `token` is cancelled.
async fn read_loop<R>(
    readable: R,
    tx: mpsc::Sender<ClientMessages>,
    sender: Sender,
    max_line_length: usize,
    token: CancellationToken,
) -> TransportResult<()>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(readable);
    loop {
        let read = tokio::select! {
            () = token.cancelled() => return Ok(()),
            read = read_capped_line(&mut reader, max_line_length) => read,
        };
        let line = match read {
            Ok(LineRead::Line(line)) => line,
            Ok(LineRead::Eof) => return Ok(()),
            Ok(LineRead::TooLong) => {
                tracing::error!(
                    "incoming message exceeds {max_line_length} bytes; replied with an invalid-request error"
                );
                reply(&sender, &too_long_reply(max_line_length)).await?;
                continue;
            }
            Err(error) => {
                return Err(TransportError::ProcessError(format!(
                    "Error reading from readable stream: {error}"
                )));
            }
        };
        match classify(&line) {
            Incoming::Message(message) => {
                if tx.send(*message).await.is_err() {
                    return Ok(());
                }
            }
            Incoming::Rejected(replies) => {
                tracing::warn!(
                    "incoming line is not a JSON-RPC message the server can serve; replied with {} error(s)",
                    replies.len()
                );
                for payload in &replies {
                    reply(&sender, payload).await?;
                }
            }
            Incoming::Ignored => {
                tracing::warn!(
                    "incoming notification or response is not one the server reads; ignored"
                );
            }
            Incoming::Blank => {}
        }
    }
}

#[async_trait]
impl Transport<ClientMessages, MessageFromServer, ClientMessage, ServerMessages, ServerMessage>
    for StdioServerTransport
{
    async fn start(&self) -> TransportResult<ReceiverStream<ClientMessages>>
    where
        MessageDispatcher<ClientMessage>:
            McpDispatch<ClientMessages, ServerMessages, ClientMessage, ServerMessage>,
    {
        let dispatcher = MessageDispatcher::new(
            self.pending_requests.clone(),
            Mutex::new(Box::pin(tokio::io::stdout())),
            self.options.timeout,
        );
        *self.message_sender.write().await = Some(dispatcher);
        *self.error_stream.write().await = Some(IoStream::Writable(Box::pin(tokio::io::stderr())));
        let (tx, rx) = mpsc::channel(self.options.channel_capacity.max(1));
        let reader = read_loop(
            tokio::io::stdin(),
            tx,
            self.message_sender.clone(),
            self.options.max_line_length,
            self.shutdown.clone(),
        );
        tokio::spawn(async move {
            if let Err(error) = reader.await {
                tracing::error!("{error}");
            }
        });
        Ok(ReceiverStream::new(rx))
    }

    fn message_sender(&self) -> Sender {
        self.message_sender.clone()
    }

    fn error_stream(&self) -> &RwLock<Option<IoStream>> {
        &self.error_stream
    }

    async fn shut_down(&self) -> TransportResult<()> {
        self.shutdown.cancel();
        *self.is_shut_down.lock().await = true;
        Ok(())
    }

    async fn is_shut_down(&self) -> bool {
        *self.is_shut_down.lock().await
    }

    async fn consume_string_payload(&self, _payload: &str) -> TransportResult<()> {
        Err(TransportError::Internal(
            "consume_string_payload() is not supported by the stdio transport".to_owned(),
        ))
    }

    async fn pending_request_tx(
        &self,
        request_id: &RequestId,
    ) -> Option<oneshot::Sender<ClientMessage>> {
        self.pending_requests.lock().await.remove(request_id)
    }

    async fn keep_alive(
        &self,
        _interval: Duration,
        _disconnect_tx: oneshot::Sender<()>,
    ) -> TransportResult<JoinHandle<()>> {
        Err(TransportError::Internal(
            "keep_alive() is not supported by the stdio transport".to_owned(),
        ))
    }
}

#[async_trait]
impl McpDispatch<ClientMessages, ServerMessages, ClientMessage, ServerMessage>
    for StdioServerTransport
{
    async fn send_message(
        &self,
        message: ServerMessages,
        request_timeout: Option<Duration>,
    ) -> TransportResult<Option<ClientMessages>> {
        let guard = self.message_sender.read().await;
        let dispatcher = guard.as_ref().ok_or(SdkError::connection_closed())?;
        dispatcher.send_message(message, request_timeout).await
    }

    async fn send(
        &self,
        message: ServerMessage,
        request_timeout: Option<Duration>,
    ) -> TransportResult<Option<ClientMessage>> {
        let guard = self.message_sender.read().await;
        let dispatcher = guard.as_ref().ok_or(SdkError::connection_closed())?;
        dispatcher.send(message, request_timeout).await
    }

    async fn write_str(&self, payload: &str, skip_store: bool) -> TransportResult<()> {
        let guard = self.message_sender.read().await;
        let dispatcher = guard.as_ref().ok_or(SdkError::connection_closed())?;
        McpDispatch::<ClientMessages, ServerMessages, ClientMessage, ServerMessage>::write_str(
            dispatcher, payload, skip_store,
        )
        .await
    }
}

impl
    TransportDispatcher<
        ClientMessages,
        MessageFromServer,
        ClientMessage,
        ServerMessages,
        ServerMessage,
    > for StdioServerTransport
{
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn lines(input: &[u8], max: usize) -> Vec<LineRead> {
        let mut reader = BufReader::new(input);
        let mut out = Vec::new();
        loop {
            let read = read_capped_line(&mut reader, max).await.expect("read");
            let eof = read == LineRead::Eof;
            out.push(read);
            if eof {
                return out;
            }
        }
    }

    #[tokio::test]
    async fn capped_lines_drop_only_the_long_one() {
        let read = lines(b"abc\n0123456789\nxy\nlast", 5).await;
        assert_eq!(
            read,
            vec![
                LineRead::Line(b"abc".to_vec()),
                LineRead::TooLong,
                LineRead::Line(b"xy".to_vec()),
                LineRead::Line(b"last".to_vec()),
                LineRead::Eof,
            ]
        );
    }

    #[tokio::test]
    async fn a_line_of_exactly_the_cap_is_kept() {
        let read = lines(b"12345\n123456\n", 5).await;
        assert_eq!(
            read,
            vec![
                LineRead::Line(b"12345".to_vec()),
                LineRead::TooLong,
                LineRead::Eof
            ]
        );
    }

    /// The `(id, code)` of the replies to one line.
    type Replies = Vec<(Value, i64)>;

    /// The `(id, code)` of every reply `line` is rejected with; `None` when
    /// the line is not rejected.
    fn rejected(line: &[u8]) -> Option<Replies> {
        let Incoming::Rejected(replies) = classify(line) else {
            return None;
        };
        Some(
            replies
                .iter()
                .map(|reply| {
                    let reply: Value = serde_json::from_str(reply).expect("a JSON reply");
                    assert_eq!(reply["jsonrpc"], "2.0", "{reply}");
                    assert!(reply["error"]["message"].is_string(), "{reply}");
                    (
                        reply["id"].clone(),
                        reply["error"]["code"].as_i64().expect("an error code"),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn non_json_lines_are_parse_errors() {
        for line in [
            &b"this is not json"[..],
            br#"{"jsonrpc":"2.0","id":1,"#,
            b"\xff\xfe",
        ] {
            assert_eq!(
                rejected(line),
                Some(vec![(Value::Null, -32700)]),
                "{line:?}"
            );
        }
    }

    #[test]
    fn requests_and_blank_lines_are_told_apart() {
        let request = br#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#;
        assert!(matches!(classify(request), Incoming::Message(_)));
        assert!(matches!(classify(b"  \r"), Incoming::Blank));
    }

    /// The rule of the module: whatever calls for a reply gets one (#32).
    /// Every line here is JSON the server cannot serve.
    #[test]
    fn every_unreadable_line_that_calls_for_a_reply_gets_one() {
        let id = |id: i64| Value::from(id);
        let cases: &[(&str, Replies)] = &[
            // A request whose params is no structured value (#32).
            (
                r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":"x"}"#,
                vec![(id(14), -32600)],
            ),
            (
                r#"{"jsonrpc":"2.0","id":"s20","method":"tools/list","params":5}"#,
                vec![(Value::from("s20"), -32600)],
            ),
            // Broken envelopes: the id is echoed when it is usable.
            (
                r#"{"jsonrpc":"2.0","id":16,"method":42}"#,
                vec![(id(16), -32600)],
            ),
            (r#"{"jsonrpc":"2.0","id":17}"#, vec![(id(17), -32600)]),
            (
                r#"{"jsonrpc":"1.0","id":18,"method":"tools/list"}"#,
                vec![(id(18), -32600)],
            ),
            (r#"{"id":19,"method":"tools/list"}"#, vec![(id(19), -32600)]),
            (
                r#"{"jsonrpc":"2.0","id":1.5,"method":"tools/list"}"#,
                vec![(Value::Null, -32600)],
            ),
            (
                r#"{"jsonrpc":"2.0","id":{"a":1},"method":"tools/list"}"#,
                vec![(Value::Null, -32600)],
            ),
            // Not a JSON-RPC message at all.
            ("42", vec![(Value::Null, -32600)]),
            (r#""text""#, vec![(Value::Null, -32600)]),
            ("null", vec![(Value::Null, -32600)]),
            ("true", vec![(Value::Null, -32600)]),
            ("{}", vec![(Value::Null, -32600)]),
            (r#"{"foo":1}"#, vec![(Value::Null, -32600)]),
            // Batches: one reply per request, or one when there is none.
            ("[]", vec![(Value::Null, -32600)]),
            ("[1,2]", vec![(Value::Null, -32600)]),
            (
                r#"[{"jsonrpc":"2.0","id":21,"method":"tools/list","params":{}},{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":"b","method":"tools/call","params":"x"}]"#,
                vec![(id(21), -32600), (Value::from("b"), -32600)],
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(rejected(line.as_bytes()).as_ref(), Some(expected), "{line}");
        }
    }

    /// Notifications and responses never get a reply, readable or not.
    #[test]
    fn unreadable_notifications_and_responses_get_no_reply() {
        for line in [
            r#"{"jsonrpc":"2.0","method":"notifications/x","params":"x"}"#,
            r#"{"method":42}"#,
            r#"{"jsonrpc":"2.0","id":24,"result":"x","extra":{"a":[1]}}"#,
            r#"{"jsonrpc":"1.0","id":25,"error":"x"}"#,
        ] {
            assert!(
                matches!(
                    classify(line.as_bytes()),
                    Incoming::Ignored | Incoming::Message(_)
                ),
                "{line}"
            );
        }
    }

    #[test]
    fn a_rejected_request_is_answered_with_its_id_and_the_next_one_served() {
        let bad = r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":"x"}"#;
        let good = r#"{"jsonrpc":"2.0","id":99,"method":"tools/list","params":{}}"#;
        let batch = format!("[{good}]");
        let input = format!("{bad}\n{batch}\n{good}\n");
        let (replies, forwarded) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(run_read_loop(input.as_bytes(), 64 * 1024));
        assert_eq!(forwarded, 1, "only the readable request is served");
        let answered: Vec<(Value, i64)> = replies
            .iter()
            .map(|reply| {
                (
                    reply["id"].clone(),
                    reply["error"]["code"].as_i64().expect("code"),
                )
            })
            .collect();
        assert_eq!(
            answered,
            vec![(Value::from(14), -32600), (Value::from(99), -32600)]
        );
    }

    /// The replies `read_loop` writes for `input` (size cap `max`) and how
    /// many messages it forwards to the server.
    async fn run_read_loop(input: &[u8], max: usize) -> (Vec<serde_json::Value>, usize) {
        let (writer, mut written) = tokio::io::duplex(64 * 1024);
        let sender: Sender = Arc::new(RwLock::new(Some(MessageDispatcher::new(
            Arc::new(Mutex::new(HashMap::new())),
            Mutex::new(Box::pin(writer)),
            Duration::from_secs(1),
        ))));
        let (tx, mut rx) = mpsc::channel(16);
        read_loop(
            input,
            tx,
            Arc::clone(&sender),
            max,
            CancellationToken::new(),
        )
        .await
        .expect("read loop");
        // The last owner of the writer: dropping it ends `written`.
        drop(sender);
        let mut out = String::new();
        written.read_to_string(&mut out).await.expect("replies");
        let replies = out
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON reply"))
            .collect();
        let mut forwarded = 0;
        while rx.try_recv().is_ok() {
            forwarded += 1;
        }
        (replies, forwarded)
    }

    #[tokio::test]
    async fn an_over_cap_line_is_answered_and_the_next_one_served() {
        let request = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let input = format!("{}\n{request}\nnot json\n", "x".repeat(200));
        let (replies, forwarded) = run_read_loop(input.as_bytes(), 100).await;
        assert_eq!(forwarded, 1, "the request after the long line is served");
        assert_eq!(
            replies,
            vec![
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32600,
                        "message": "Invalid Request: message exceeds 100 bytes"
                    }
                }),
                serde_json::from_str::<serde_json::Value>(PARSE_ERROR_REPLY).expect("json"),
            ]
        );
    }

    #[test]
    fn parse_error_reply_is_the_json_rpc_error() {
        let reply: serde_json::Value = serde_json::from_str(PARSE_ERROR_REPLY).expect("json");
        assert_eq!(
            reply,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": "Parse error" }
            })
        );
    }
}
