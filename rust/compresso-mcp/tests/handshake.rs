use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io, server_instructions};
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(3);
const PROTOCOL_VERSION: &str = "2024-11-05";
const TOOL_NAMES: [&str; 10] = [
    "compresso_compress",
    "compresso_context",
    "compresso_explain",
    "compresso_impact",
    "compresso_outline",
    "compresso_read",
    "compresso_repo_map",
    "compresso_search",
    "compresso_stats",
    "compresso_unfold",
];

struct Harness {
    _temp: TempDir,
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    async fn start() -> Self {
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
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let server = tokio::spawn(serve_io(deps, server_read, server_write));
        let (client_read, client_write) = tokio::io::split(client);
        Self {
            _temp: temp,
            input: client_write,
            output: BufReader::new(client_read),
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

    async fn send_raw(&mut self, frame: &[u8]) {
        timeout(IO_TIMEOUT, self.input.write_all(frame))
            .await
            .expect("request write should not time out")
            .expect("request should be written");
    }

    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        let bytes = timeout(IO_TIMEOUT, self.output.read_line(&mut line))
            .await
            .expect("response read should not time out")
            .expect("response should be readable");
        assert!(bytes > 0, "server closed before sending a response");
        serde_json::from_str(&line).expect("response should be valid JSON-RPC")
    }

    async fn initialize(&mut self) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "compresso-test", "version": "1.0.0" }
            }
        }))
        .await;
        let response = self.receive().await;
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await;
        response
    }

    async fn close(mut self) {
        timeout(IO_TIMEOUT, self.input.shutdown())
            .await
            .expect("input shutdown should not time out")
            .expect("input should shut down");
        drop(self.input);
        timeout(IO_TIMEOUT, self.server)
            .await
            .expect("server should exit on EOF")
            .expect("server task should not panic")
            .expect("server should exit cleanly");
    }
}

#[tokio::test]
async fn test_initialize_handshake() {
    let mut harness = Harness::start().await;
    let response = harness.initialize().await;

    assert_eq!(response["result"]["serverInfo"]["name"], "compresso");
    assert_eq!(
        response["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);

    harness.close().await;
}

#[tokio::test]
async fn test_tools_list_exact_ten() {
    let mut harness = Harness::start().await;
    let _ = harness.initialize().await;
    harness
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .await;

    let response = harness.receive().await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools/list should return an array");
    let mut names = tools
        .iter()
        .map(|tool| {
            let schema = tool["inputSchema"]
                .as_object()
                .expect("every tool should declare an input schema");
            assert_eq!(
                schema.get("$schema").and_then(Value::as_str),
                Some("https://json-schema.org/draft/2020-12/schema")
            );
            assert_eq!(
                schema.get("additionalProperties").and_then(Value::as_bool),
                Some(false)
            );
            assert!(
                schema.get("required").is_some_and(Value::is_array),
                "every tool schema should declare required arguments"
            );
            tool["name"].as_str().expect("tool name should be a string")
        })
        .collect::<Vec<_>>();
    names.sort_unstable();

    assert_eq!(names, TOOL_NAMES);
    harness.close().await;
}

#[test]
fn test_instructions_word_budget() {
    let instructions = server_instructions();
    assert!(instructions.split_whitespace().count() <= 200);

    let normalized = instructions.to_ascii_lowercase();
    for denied in ["always use", "prefer compresso", "instead of your"] {
        assert!(
            !normalized.contains(denied),
            "instructions contain denied phrase {denied:?}"
        );
    }
}

#[tokio::test]
async fn test_malformed_frame_survives() {
    let mut harness = Harness::start().await;
    harness.send_raw(b"{\"jsonrpc\":\"2.0\",\n").await;

    let error = harness.receive().await;
    assert_eq!(error["jsonrpc"], "2.0");
    assert!(error["id"].is_null());
    assert_eq!(error["error"]["code"], -32700);

    let response = harness.initialize().await;
    assert_eq!(response["result"]["serverInfo"]["name"], "compresso");
    harness.close().await;
}

#[tokio::test]
async fn test_eof_before_initialize_exits_cleanly() {
    Harness::start().await.close().await;
}
