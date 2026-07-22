use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use compresso_atlas::Indexer;
use compresso_core::{Config, Engine, TokenProvider, counter_for, initialize_tokenizers};
use compresso_mcp::{McpDeps, McpLedgerHooks, serve_io, serve_io_with_hooks};
use compresso_store::{StatsFilter, Store, StoreError};
use jsonschema::{Draft, Validator};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const PROTOCOL_VERSION: &str = "2024-11-05";
const SOURCE_CANARY: &str = "session_ledger_source_canary_1947";

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Harness {
    async fn start(store: Store) -> Self {
        Self::start_with_optional_hooks(store, None).await
    }

    async fn start_with_hooks(store: Store, hooks: McpLedgerHooks) -> Self {
        Self::start_with_optional_hooks(store, Some(hooks)).await
    }

    async fn start_with_optional_hooks(store: Store, hooks: Option<McpLedgerHooks>) -> Self {
        let config = Config::default();
        let deps = McpDeps {
            store,
            engine: Engine::new(
                config.compression.mode,
                counter_for(TokenProvider::OpenAiO200k),
            ),
            config,
        };
        let (client, server) = tokio::io::duplex(512 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let server = tokio::spawn(async move {
            match hooks {
                Some(hooks) => {
                    serve_io_with_hooks(
                        deps,
                        server_read,
                        server_write,
                        || {
                            initialize_tokenizers(&[
                                TokenProvider::OpenAiO200k,
                                TokenProvider::AnthropicCalibrated,
                            ])
                        },
                        hooks,
                    )
                    .await
                }
                None => serve_io(deps, server_read, server_write).await,
            }
        });
        let (client_read, client_write) = tokio::io::split(client);
        let mut harness = Self {
            input: client_write,
            output: BufReader::new(client_read),
            server,
            next_id: 1,
        };
        let response = harness
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "test-harness", "version": "1.0.0" }
                }),
            )
            .await;
        assert_eq!(response["result"]["serverInfo"]["name"], "compresso");
        harness.notify("notifications/initialized", json!({})).await;
        harness
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await;
        self.receive().await
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await;
    }

    async fn send(&mut self, message: &Value) {
        let mut frame = serde_json::to_vec(message).expect("request should serialize");
        frame.push(b'\n');
        timeout(IO_TIMEOUT, self.input.write_all(&frame))
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

struct IndexedFixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl IndexedFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).expect("source directory should be created");
        fs::write(
            root.join("src/session.rs"),
            format!(
                "pub struct SessionManager;\nimpl SessionManager {{ pub fn retry(&self) {{ let _ = \"{SOURCE_CANARY}\"; }} }}\n"
            ),
        )
        .expect("source fixture should be written");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
            .index(&root, |_| {})
            .expect("fixture should index");
        Self {
            _temp: temp,
            root: fs::canonicalize(root).expect("repository should canonicalize"),
            store,
        }
    }

    fn root_argument(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }
}

#[tokio::test]
async fn test_session_row_lifecycle() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let mut harness = Harness::start(store.clone()).await;

    let result = successful_json(
        harness
            .call("compresso_compress", json!({ "text": "alpha beta gamma" }))
            .await,
    );
    let operation = store
        .operation_by_id(
            result["operation_id"]
                .as_str()
                .expect("operation id should exist"),
        )
        .expect("operation lookup should succeed")
        .expect("operation should be recorded");
    let session_id = operation
        .session_id
        .expect("MCP operation should link to a session");
    let active = store
        .session_by_id(&session_id)
        .expect("session lookup should succeed")
        .expect("session should exist");
    assert_eq!(active.client, "test-harness");
    assert_eq!(active.transport, "stdio");
    assert_eq!(active.ended_at, None);

    harness.close().await;

    let closed = store
        .session_by_id(&session_id)
        .expect("session lookup should succeed")
        .expect("session should exist");
    assert!(closed.ended_at.is_some());
}

#[tokio::test]
async fn test_operation_per_tool_call() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let root = fixture.root_argument();
    let search = successful_json(
        harness
            .call(
                "compresso_search",
                json!({ "root": root, "query": "SessionManager" }),
            )
            .await,
    );
    let read = successful_json(
        harness
            .call(
                "compresso_read",
                json!({ "root": root, "path": "src/session.rs", "max_tokens": 1_000 }),
            )
            .await,
    );
    let compress = successful_json(
        harness
            .call(
                "compresso_compress",
                json!({ "text": "one two three four" }),
            )
            .await,
    );

    let operations = [
        ("compresso_search", &search),
        ("compresso_read", &read),
        ("compresso_compress", &compress),
    ]
    .map(|(tool, result)| {
        let operation = fixture
            .store
            .operation_by_id(
                result["operation_id"]
                    .as_str()
                    .expect("successful tool should return an operation id"),
            )
            .expect("operation lookup should succeed")
            .expect("one operation should be recorded");
        assert_eq!(operation.tool, tool);
        operation
    });
    let session_id = operations[0]
        .session_id
        .clone()
        .expect("operation should link to a session");
    assert!(
        operations
            .iter()
            .all(|operation| operation.session_id.as_ref() == Some(&session_id))
    );
    for operation in &operations[..2] {
        assert!(operation.trace_json.is_none());
        assert!(operation.tokens_served.is_some_and(|tokens| tokens > 0));
        assert_eq!(operation.observed_tokens, operation.tokens_served.unwrap());
        assert_eq!(operation.applied_tokens, operation.tokens_served.unwrap());
    }
    let compression_trace: Value = serde_json::from_str(
        operations[2]
            .trace_json
            .as_deref()
            .expect("compression operation should carry its trace"),
    )
    .expect("compression trace should parse");
    trace_validator()
        .validate(&compression_trace)
        .expect("compression trace should satisfy the root schema");
    assert_eq!(compression_trace["session_id"], session_id.as_str());
    assert!(operations[2].tokens_served.is_none());

    let summary = fixture
        .store
        .detailed_stats(&StatsFilter {
            since: None,
            session_id: Some(session_id),
        })
        .expect("session stats should load");
    assert_eq!(summary.summary.operations, 3);
    harness.close().await;
}

#[tokio::test]
async fn test_ledger_failure_fail_open() {
    let fixture = IndexedFixture::new();
    let writes = Arc::new(AtomicUsize::new(0));
    let warnings = Arc::new(Mutex::new(Vec::<String>::new()));
    let writer_calls = Arc::clone(&writes);
    let captured_warnings = Arc::clone(&warnings);
    let hooks = McpLedgerHooks::new(
        move |_| {
            writer_calls.fetch_add(1, Ordering::SeqCst);
            Err(StoreError::InvalidField {
                field: "injected MCP ledger",
                reason: "deterministic write failure",
            })
        },
        move |warning| {
            captured_warnings
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(warning.to_owned());
        },
    );
    let mut harness = Harness::start_with_hooks(fixture.store.clone(), hooks).await;

    let result = successful_json(
        harness
            .call(
                "compresso_search",
                json!({
                    "root": fixture.root_argument(),
                    "query": "SessionManager"
                }),
            )
            .await,
    );

    assert!(
        result["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty())
    );
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .store
            .operation_by_id(
                result["operation_id"]
                    .as_str()
                    .expect("operation id should exist")
            )
            .expect("operation lookup should succeed")
            .is_none()
    );
    {
        let warnings = warnings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ledger write failed"));
        assert!(warnings[0].contains("compresso_search"));
        assert!(!warnings[0].contains(SOURCE_CANARY));
        assert!(!warnings[0].contains("SessionManager"));
    }
    harness.close().await;
}

fn successful_json(response: Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "unexpected protocol error: {response}"
    );
    assert_ne!(
        response["result"]["isError"], true,
        "unexpected tool error: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .expect("tool result should contain content");
    assert_eq!(content.len(), 1);
    serde_json::from_str(
        content[0]["text"]
            .as_str()
            .expect("tool content should be text"),
    )
    .expect("tool content should be structured JSON")
}

fn trace_validator() -> Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/compression-trace/v1.json");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let schema: Value = serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .expect("compression trace root schema should compile")
}
