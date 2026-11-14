//! A client that stops reading must end the server with a typed error.

use std::time::Duration;

use compresso_core::{Config, Engine, StreamError, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io};
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

struct Harness {
    _temp: TempDir,
    /// Client end of the server's input; held open for the whole test.
    input: DuplexStream,
    /// Client end of the server's output; dropping it closes the pipe.
    output: Option<BufReader<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    fn start() -> Self {
        let temp = TempDir::new().expect("temporary directory should be created");
        let config = Config::default();
        let deps = McpDeps {
            store: Store::open(&temp.path().join("compresso.db"))
                .expect("temporary store should open"),
            engine: Engine::new(
                config.compression.mode,
                counter_for(TokenProvider::Heuristic),
            ),
            config,
        };
        let (input, server_input) = tokio::io::duplex(64 * 1024);
        let (server_output, output) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(serve_io(deps, server_input, server_output));
        Self {
            _temp: temp,
            input,
            output: Some(BufReader::new(output)),
            server,
        }
    }

    async fn send(&mut self, message: &Value) {
        let mut frame = serde_json::to_vec(message).expect("request should serialize");
        frame.push(b'\n');
        timeout(IO_TIMEOUT, self.input.write_all(&frame))
            .await
            .expect("request write should not time out")
            .expect("request should be written");
    }

    async fn send_initialize(&mut self) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "closed-pipe-test", "version": "1.0.0" }
            }
        }))
        .await;
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

    /// Waits for the server to stop on its own; its input is still open.
    async fn closed_output_error(self) -> StreamError {
        let result = timeout(IO_TIMEOUT, self.server)
            .await
            .expect("server should stop once its output reader is gone")
            .expect("server task should not panic");
        drop(self.input);
        result
            .expect_err("a closed output must be reported")
            .downcast::<StreamError>()
            .expect("the failure should be a typed stream error")
    }
}

#[tokio::test]
async fn test_output_closed_before_initialize_response_is_typed_error() {
    let mut harness = Harness::start();
    drop(harness.output.take());

    harness.send_initialize().await;
    let error = harness.closed_output_error().await;

    assert!(error.is_closed(), "unexpected error: {error:?}");
    assert_eq!(error.code(), "output_closed");
}

#[tokio::test]
async fn test_output_closed_during_session_is_typed_error() {
    let mut harness = Harness::start();
    harness.send_initialize().await;
    let response = harness.receive().await;
    assert_eq!(response["result"]["serverInfo"]["name"], "compresso");
    harness
        .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;

    drop(harness.output.take());
    harness
        .send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .await;
    let error = harness.closed_output_error().await;

    assert!(error.is_closed(), "unexpected error: {error:?}");
}

#[tokio::test]
async fn test_malformed_frame_with_closed_output_is_typed_error() {
    let mut harness = Harness::start();
    harness.send_initialize().await;
    harness.receive().await;
    harness
        .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;

    drop(harness.output.take());
    timeout(IO_TIMEOUT, harness.input.write_all(b"{ not json\n"))
        .await
        .expect("frame write should not time out")
        .expect("frame should be written");
    let error = harness.closed_output_error().await;

    assert!(error.is_closed(), "unexpected error: {error:?}");
}
