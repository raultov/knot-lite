//! Stdio integration test for the `knot-lite-mcp` server (P1-D13: the only
//! test outside `src/`, because the transport cannot be covered by lib tests).
//!
//! Cross-platform: spawns the built binary directly (no shell, no Python).

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Map, Value};

/// Wire protocol spoken by the server (2026-07-28).
const PROTOCOL_VERSION: &str = "2026-07-28";

fn meta() -> String {
    format!(
        r#""_meta":{{"io.modelcontextprotocol/protocolVersion":"{PROTOCOL_VERSION}","io.modelcontextprotocol/clientCapabilities":{{}}}}"#
    )
}

fn request(id: u64, method: &str, params: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
}

fn call(id: u64, name: &str, arguments: &str) -> String {
    request(
        id,
        "tools/call",
        &format!(r#"{{"name":"{name}","arguments":{arguments},{}}}"#, meta()),
    )
}

/// Spawn the MCP server over stdio against an isolated tempdir repository.
fn spawn_server(repo: &std::path::Path) -> (Child, Receiver<String>, tempfile::TempDir) {
    let isolation = tempfile::tempdir().expect("config isolation tempdir");
    let non_exec = isolation.path().join("not-an-lsp");
    std::fs::write(&non_exec, b"stub").expect("write non-exec stub");

    let mut child = Command::new(env!("CARGO_BIN_EXE_knot-lite-mcp"))
        .arg("--repo-path")
        .arg(repo)
        .env("RUST_LOG", "off")
        // Isolate the user's .env (P1-D7 config location).
        .env("KNOT_LITE_CONFIG_DIR", isolation.path())
        // Deterministic tiers: every override is invalid (non-executable stub).
        .env("KNOT_LITE_LSP_RUST", &non_exec)
        .env("KNOT_LITE_LSP_TS", &non_exec)
        .env("KNOT_LITE_LSP_JAVA", &non_exec)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn knot-lite-mcp");

    let stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        // Every stdout line must parse as JSON: this guards against log
        // leakage corrupting the MCP stream.
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    (child, rx, isolation)
}

fn collect_responses(rx: &Receiver<String>, wanted: &[u64]) -> Map<String, Value> {
    let mut by_id = Map::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while wanted.iter().any(|id| !by_id.contains_key(&id.to_string())) {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .expect("timed out waiting for MCP responses");
        let line = rx
            .recv_timeout(remaining)
            .expect("timed out waiting for MCP responses");
        let value: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("non-JSON line on stdout (log leakage?): {e}: {line}"));
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("response without numeric id: {line}"));
        by_id.insert(id.to_string(), value);
    }
    by_id
}

fn result_of(response: &Value) -> Value {
    if let Some(error) = response.get("error") {
        panic!("unexpected JSON-RPC error: {error}");
    }
    response
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("response without result: {response}"))
}

fn error_text(result: &Value) -> String {
    assert_eq!(result.get("isError").and_then(Value::as_bool), Some(true));
    result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .expect("error text")
        .to_string()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir src");
    std::fs::write(tmp.path().join("src/lib.rs"), "pub fn f() {}\n").expect("write lib.rs");
    std::fs::write(tmp.path().join("run.sh"), "echo hi\n").expect("write run.sh");
    tmp
}

fn stop(child: &mut Child) {
    if let Some(stdin) = child.stdin.take() {
        drop(stdin);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn assert_discover(discover: &Value) {
    // Identity lives in `_meta[".../serverInfo"]` (P1.3: DiscoverResult has no
    // top-level `serverInfo` field).
    let server_info = discover
        .get("_meta")
        .and_then(|m| m.get("io.modelcontextprotocol/serverInfo"))
        .expect("serverInfo in _meta");
    assert_eq!(
        server_info.get("name").and_then(Value::as_str),
        Some("knot-lite")
    );
}

fn assert_tools_list(list: &Value) {
    let tools = list.get("tools").and_then(Value::as_array).expect("tools");
    let mut names: Vec<String> = tools
        .iter()
        .map(|t| {
            t.get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "explore_file",
            "find_callers",
            "list_files",
            "search_hybrid_context",
            "status"
        ]
    );
    let find_callers = tools
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some("find_callers"))
        .expect("find_callers tool");
    let required = find_callers
        .get("inputSchema")
        .and_then(|s| s.get("required"))
        .and_then(Value::as_array)
        .expect("required");
    assert_eq!(required.len(), 1);
    assert_eq!(required[0].as_str(), Some("entity_name"));
}

fn assert_status(status: &Value) {
    assert_ne!(status.get("isError").and_then(Value::as_bool), Some(true));
    let structured = status.get("structuredContent").expect("structuredContent");
    assert_eq!(
        structured.get("schema").and_then(Value::as_str),
        Some("knot-lite/status/v1")
    );
    let languages = structured
        .get("languages")
        .and_then(Value::as_array)
        .expect("languages");
    let rust_row = languages
        .iter()
        .find(|row| row.get("language").and_then(Value::as_str) == Some("rust"))
        .expect("rust row");
    assert_eq!(
        rust_row.get("tier").and_then(Value::as_u64),
        Some(2),
        "invalid override -> tier 2"
    );
    let files_only = structured
        .get("files_only")
        .and_then(|v| v.get("top"))
        .and_then(Value::as_array)
        .expect("files_only.top");
    assert!(
        files_only
            .iter()
            .any(|entry| entry.get("key").and_then(Value::as_str) == Some(".sh")),
        "run.sh must be counted as files-only: {files_only:?}"
    );
}

#[test]
fn mcp_stdio_protocol_session() {
    let repo = fixture();
    let (mut child, rx, _isolation) = spawn_server(repo.path());
    let mut stdin = child.stdin.take().expect("child stdin");

    let requests = vec![
        request(1, "server/discover", &format!("{{{}}}", meta())),
        request(2, "tools/list", &format!("{{{}}}", meta())),
        call(3, "status", "{}"),
        call(4, "find_callers", r#"{"entity_name":"foo"}"#),
        call(
            5,
            "find_callers",
            r#"{"entity_name":"foo","repo_name":"other"}"#,
        ),
        call(6, "nope", "{}"),
        // Negative test (P1.3 recorded behaviour): a request without `_meta` is
        // not a typed request and falls through to the custom-request handler,
        // answered with JSON-RPC error -32601.
        request(7, "tools/list", "{}"),
    ];
    for request in &requests {
        writeln!(stdin, "{request}").expect("write request");
    }
    stdin.flush().expect("flush requests");

    let responses = collect_responses(&rx, &[1, 2, 3, 4, 5, 6, 7]);
    stop(&mut child);

    assert_discover(&result_of(&responses["1"]));
    assert_tools_list(&result_of(&responses["2"]));
    assert_status(&result_of(&responses["3"]));

    // Queries fail as tool errors (P1-D12), never as protocol errors.
    assert!(error_text(&result_of(&responses["4"])).contains("has no index"));
    assert!(error_text(&result_of(&responses["5"])).contains("not served here"));
    assert_eq!(
        error_text(&result_of(&responses["6"])),
        "Unknown tool: nope"
    );

    // Request without `_meta`: JSON-RPC error -32601 (custom-request fallback).
    let no_meta = &responses["7"];
    let error = no_meta.get("error").expect("JSON-RPC error member");
    assert_eq!(error.get("code").and_then(Value::as_i64), Some(-32601));
}
