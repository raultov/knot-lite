//! The stdio transport of `knot-lite-mcp`.
//!
//! It behaves like the SDK's `StdioTransport` (one JSON-RPC message per line
//! on stdin, replies on stdout, lines over the size cap dropped) with one
//! difference: a line that is not JSON gets the JSON-RPC 2.0 parse error
//! (`-32700`, `id: null`) instead of being dropped without a word, so the
//! client learns why its request went unanswered. The reply goes through the
//! same writer as every other message, so it never interleaves with one.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_mcp_sdk::schema::RequestId;
use rust_mcp_sdk::schema::schema_utils::{
    ClientMessage, ClientMessages, MessageFromServer, SdkError, ServerMessage, ServerMessages,
};
use rust_mcp_sdk::{
    IoStream, McpDispatch, MessageDispatcher, Transport, TransportDispatcher, TransportError,
    TransportOptions,
};
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

/// What one input line is.
#[derive(Debug)]
enum Incoming {
    /// A JSON-RPC message for the server.
    Message(Box<ClientMessages>),
    /// Not JSON: answered with [`PARSE_ERROR_REPLY`].
    ParseError,
    /// JSON that is no client message (no reply: it may be a notification).
    Unrecognised,
    /// A blank line.
    Blank,
}

fn classify(line: &[u8]) -> Incoming {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Incoming::Blank;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return Incoming::ParseError;
    };
    serde_json::from_value(value).map_or(Incoming::Unrecognised, |message| {
        Incoming::Message(Box::new(message))
    })
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
                tracing::error!("dropping incoming message exceeding {max_line_length} bytes");
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
            Incoming::ParseError => {
                tracing::warn!("incoming line is not JSON; replied with a parse error");
                let guard = sender.read().await;
                if let Some(dispatcher) = guard.as_ref() {
                    McpDispatch::<ClientMessages, ServerMessages, ClientMessage, ServerMessage>::write_str(
                        dispatcher,
                        PARSE_ERROR_REPLY,
                        true,
                    )
                    .await?;
                }
            }
            Incoming::Unrecognised => {
                tracing::warn!("incoming JSON is not a JSON-RPC client message; ignored");
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

    #[test]
    fn non_json_lines_are_parse_errors() {
        assert!(matches!(
            classify(b"this is not json"),
            Incoming::ParseError
        ));
        assert!(matches!(
            classify(br#"{"jsonrpc":"2.0","id":1,"#),
            Incoming::ParseError
        ));
        assert!(matches!(classify(b"\xff\xfe"), Incoming::ParseError));
    }

    #[test]
    fn requests_blank_and_foreign_json_are_told_apart() {
        let request = br#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#;
        assert!(matches!(classify(request), Incoming::Message(_)));
        assert!(matches!(classify(b"  \r"), Incoming::Blank));
        assert!(matches!(classify(b"42"), Incoming::Unrecognised));
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
