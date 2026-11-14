//! Incoming JSON-RPC messages are capped; an oversized one is rejected and
//! the session carries on.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use compresso_core::{
    Config, Engine, StreamError, TokenProvider, counter_for, initialize_tokenizers,
};
use compresso_mcp::{
    MAX_INCOMING_MESSAGE_BYTES, McpDeps, McpInputError, McpLedgerHooks, serve_io_with_hooks,
};
use compresso_store::{SessionId, Store};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(60);
const CANARY: &str = "message_cap_canary_5512";
static PADDING: [u8; 64 * 1024] = [b' '; 64 * 1024];

struct Harness {
    _temp: TempDir,
    store: Store,
    input: Option<DuplexStream>,
    output: Option<BufReader<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    warnings: Arc<Mutex<Vec<String>>>,
    sessions: Arc<Mutex<Vec<SessionId>>>,
}

impl Harness {
    async fn start() -> Self {
        let temp = TempDir::new().expect("temporary directory should be created");
        let store =
            Store::open(&temp.path().join("compresso.db")).expect("temporary store should open");
        let config = Config::default();
        let deps = McpDeps {
            store: store.clone(),
            engine: Engine::new(
                config.compression.mode,
                counter_for(TokenProvider::Heuristic),
            ),
            config,
        };
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let sessions = Arc::new(Mutex::new(Vec::new()));
        let hooks = {
            let (store, warnings, sessions) = (store.clone(), warnings.clone(), sessions.clone());
            McpLedgerHooks::new(
                move |operation| {
                    sessions
                        .lock()
                        .expect("session capture should lock")
                        .extend(operation.session_id.clone());
                    store.record_operation(operation)
                },
                move |message| {
                    warnings
                        .lock()
                        .expect("warning capture should lock")
                        .push(message.to_owned());
                },
            )
        };
        let (input, server_input) = tokio::io::duplex(64 * 1024);
        let (server_output, output) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(serve_io_with_hooks(
            deps,
            server_input,
            server_output,
            || initialize_tokenizers(&[TokenProvider::Heuristic]),
            hooks,
        ));
        let mut harness = Self {
            _temp: temp,
            store,
            input: Some(input),
            output: Some(BufReader::new(output)),
            server,
            warnings,
            sessions,
        };
        harness
            .send(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "message-cap-test", "version": "1.0.0" }
                }
            }))
            .await;
        let response = harness.receive().await;
        assert_eq!(response["result"]["serverInfo"]["name"], "compresso");
        harness
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        harness
    }

    async fn write(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().expect("input should still be open");
        timeout(IO_TIMEOUT, input.write_all(bytes))
            .await
            .expect("request write should not time out")
            .expect("request should be written");
    }

    async fn send(&mut self, message: &Value) {
        let mut frame = serde_json::to_vec(message).expect("request should serialize");
        frame.push(b'\n');
        self.write(&frame).await;
    }

    /// Streams one JSON object of exactly `total_bytes` (before any newline):
    /// `head`, insignificant whitespace, then the closing brace.
    async fn send_padded(&mut self, head: &str, total_bytes: usize, terminate: bool) {
        self.write(head.as_bytes()).await;
        let mut padding = total_bytes - head.len() - 1;
        while padding > 0 {
            let chunk = padding.min(PADDING.len());
            self.write(&PADDING[..chunk]).await;
            padding -= chunk;
        }
        self.write(if terminate { b"}\n" } else { b"}" }).await;
    }

    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        let output = self.output.as_mut().expect("output should still be open");
        timeout(IO_TIMEOUT, output.read_line(&mut line))
            .await
            .expect("response read should not time out")
            .expect("response should be readable");
        serde_json::from_str(&line).expect("response should be valid JSON-RPC")
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings
            .lock()
            .expect("warning capture should lock")
            .clone()
    }

    async fn finish(&mut self) -> anyhow::Result<()> {
        drop(self.input.take());
        timeout(IO_TIMEOUT, &mut self.server)
            .await
            .expect("server should stop once its input ends")
            .expect("server task should not panic")
    }
}

fn ping_head(id: u64) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping""#)
}

fn oversized_head(id: u64) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping","params":{{"note":"{CANARY}"}}"#)
}

fn assert_too_large(response: &Value, id: u64) {
    assert_eq!(response["id"], id);
    assert_eq!(response["error"]["code"], -32600);
    assert_eq!(
        response["error"]["data"]["max_message_bytes"],
        MAX_INCOMING_MESSAGE_BYTES
    );
    assert!(!response.to_string().contains(CANARY));
}

#[tokio::test]
async fn test_message_at_the_cap_is_accepted() {
    let mut harness = Harness::start().await;

    harness
        .send_padded(&ping_head(2), MAX_INCOMING_MESSAGE_BYTES, true)
        .await;
    let response = harness.receive().await;

    assert_eq!(response["id"], 2);
    assert!(response.get("error").is_none(), "unexpected: {response}");
    assert!(response["result"].is_object());
    assert!(harness.warnings().is_empty());
    harness.finish().await.expect("session should end cleanly");
}

#[tokio::test]
async fn test_message_over_the_cap_is_rejected_and_the_session_continues() {
    let mut harness = Harness::start().await;

    harness
        .send_padded(&oversized_head(2), MAX_INCOMING_MESSAGE_BYTES + 1, true)
        .await;
    assert_too_large(&harness.receive().await, 2);

    harness
        .send(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }))
        .await;
    let response = harness.receive().await;
    assert_eq!(response["id"], 3);
    assert!(response["result"]["tools"].is_array());

    let warnings = harness.warnings();
    assert_eq!(warnings.len(), 1, "unexpected warnings: {warnings:?}");
    assert!(warnings[0].contains("over the 67108864-byte limit"));
    assert!(!warnings[0].contains(CANARY));
    harness.finish().await.expect("session should end cleanly");
}

#[tokio::test]
async fn test_oversized_message_at_end_of_input_closes_the_session_with_typed_error() {
    let mut harness = Harness::start().await;
    harness
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "compresso_stats", "arguments": {} }
        }))
        .await;
    assert_eq!(harness.receive().await["id"], 2);

    harness
        .send_padded(&oversized_head(3), MAX_INCOMING_MESSAGE_BYTES + 1, false)
        .await;
    let result = harness.finish().await;
    assert_too_large(&harness.receive().await, 3);

    let error = result.expect_err("a cut-off oversized message must be reported");
    assert_eq!(
        error.downcast_ref::<McpInputError>(),
        Some(&McpInputError::UnterminatedOversizedMessage)
    );
    let session = harness
        .sessions
        .lock()
        .expect("session capture should lock")
        .first()
        .cloned()
        .expect("the stats call should have been recorded");
    assert!(
        harness
            .store
            .session_by_id(&session)
            .expect("session lookup should succeed")
            .expect("session should exist")
            .ended_at
            .is_some(),
        "the session should still be closed in the ledger"
    );
}

#[tokio::test]
async fn test_oversized_message_with_closed_output_is_typed_stream_error() {
    let mut harness = Harness::start().await;
    drop(harness.output.take());

    harness
        .send_padded(&oversized_head(2), MAX_INCOMING_MESSAGE_BYTES + 1, true)
        .await;
    // The input stays open: the server must stop because its output is gone.
    let error = timeout(IO_TIMEOUT, &mut harness.server)
        .await
        .expect("server should stop once its output reader is gone")
        .expect("server task should not panic")
        .expect_err("a closed output must be reported")
        .downcast::<StreamError>()
        .expect("the failure should be a typed stream error");

    assert!(error.is_closed(), "unexpected error: {error:?}");
    assert_eq!(error.code(), "output_closed");
}
