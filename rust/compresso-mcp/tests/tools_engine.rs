use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io, serve_io_for_root};
use compresso_store::{EdgeKind, EdgeRecord, EdgeResolution, FileLanguage, FileRecord, Store};
use jsonschema::{Draft, Validator};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::{Mutex, MutexGuard};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const PROTOCOL_VERSION: &str = "2024-11-05";
const LOG_FIXTURE: &str = include_str!("../../../fixtures/logs/repeated-npm-install/input.txt");
static COMPRESSION_ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Harness {
    async fn start(store: Store) -> Self {
        Self::start_with_root(store, None).await
    }

    async fn start_for_root(store: Store, root: PathBuf) -> Self {
        Self::start_with_root(store, Some(root)).await
    }

    async fn start_with_root(store: Store, root: Option<PathBuf>) -> Self {
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
            match root {
                Some(root) => serve_io_for_root(deps, root, server_read, server_write).await,
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
                    "clientInfo": { "name": "compresso-engine-test", "version": "1.0.0" }
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

struct ImpactFixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl ImpactFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("repository root should be created");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(&root)
            .expect("repository should approve");

        for path in ["a.rs", "b.rs", "c.rs"] {
            fs::write(root.join(path), format!("// {path}\n"))
                .expect("fixture source should be written");
            store
                .upsert_file(
                    repo,
                    &FileRecord {
                        path: path.to_owned(),
                        hash: format!("hash-{path}"),
                        language: Some(FileLanguage::Rust),
                        size_bytes: 8,
                        token_estimate: 3,
                        indexed_at: 1,
                    },
                )
                .expect("fixture file should be indexed");
        }
        replace_edges(&store, repo, "a.rs", &["b.rs"]);
        replace_edges(&store, repo, "b.rs", &["c.rs"]);
        store
            .stamp_index_version(repo, "test-parser", 1)
            .expect("index version should be stamped");

        Self {
            _temp: temp,
            root: fs::canonicalize(root).expect("repository should canonicalize"),
            store,
        }
    }

    fn impact_arguments(&self) -> Value {
        json!({
            "root": self.root,
            "path": "c.rs",
            "max_depth": 2,
            "max_results": 100
        })
    }
}

#[tokio::test]
async fn test_compress_tool_log_fixture() {
    let _lock = compression_env_lock().await;
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let mut harness = Harness::start(store).await;

    let result = successful_json(
        harness
            .call(
                "compresso_compress",
                json!({
                    "text": LOG_FIXTURE,
                    "max_tokens": 100_000,
                    "max_latency_ms": 10_000
                }),
            )
            .await,
    );

    assert_eq!(result["fallback_used"], false);
    assert!(
        result["compressed"]
            .as_str()
            .is_some_and(|text| text.contains("[×500"))
    );
    assert!(
        result["after_tokens"]["openai_o200k"].as_u64()
            < result["before_tokens"]["openai_o200k"].as_u64()
    );
    harness.close().await;
}

#[tokio::test]
async fn test_compress_tool_fail_open() {
    let _lock = compression_env_lock().await;
    let _failure = StrategyFailureEnv::enable();
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let mut harness = Harness::start(store).await;
    let input = "not structured, but it must survive byte for byte\n\0edge";

    let result = successful_json(
        harness
            .call("compresso_compress", json!({ "text": input }))
            .await,
    );

    assert_eq!(result["compressed"], input);
    assert_eq!(result["fallback_used"], true);
    assert_eq!(result["fallback_reason"], "strategy_error:collapse_logs");
    harness.close().await;
}

#[tokio::test]
async fn test_impact_tool_depth_two() {
    let fixture = ImpactFixture::new();
    let mut harness = Harness::start_for_root(fixture.store.clone(), fixture.root.clone()).await;

    let result = successful_json(
        harness
            .call("compresso_impact", fixture.impact_arguments())
            .await,
    );

    assert_eq!(result["path"], "c.rs");
    assert_eq!(
        result["dependents"],
        json!([
            { "path": "b.rs", "depth": 1, "edge_kinds": ["imports"] },
            { "path": "a.rs", "depth": 2, "edge_kinds": ["imports"] }
        ])
    );
    assert_eq!(result["complete"], true);
    harness.close().await;
}

#[tokio::test]
async fn test_explain_roundtrip() {
    let _lock = compression_env_lock().await;
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let mut harness = Harness::start(store).await;
    let compressed = successful_json(
        harness
            .call(
                "compresso_compress",
                json!({ "text": LOG_FIXTURE, "max_tokens": 100_000 }),
            )
            .await,
    );
    let operation_id = compressed["operation_id"]
        .as_str()
        .expect("compression should return an operation id");

    let explained = successful_json(
        harness
            .call("compresso_explain", json!({ "operation_id": operation_id }))
            .await,
    );

    assert_eq!(explained["explained_operation_id"], operation_id);
    assert_eq!(explained["trace"]["operation_id"], operation_id);
    trace_validator()
        .validate(&explained["trace"])
        .expect("explained trace should satisfy the root schema");
    assert!(!explained.to_string().contains("registry.npmjs.org"));
    harness.close().await;
}

#[tokio::test]
async fn test_stats_reflects_session_ops() {
    let _lock = compression_env_lock().await;
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let mut harness = Harness::start(store).await;
    let inputs = ["alpha beta gamma", "delta epsilon zeta eta"];
    let mut observed = 0_u64;
    let mut applied = 0_u64;
    for input in inputs {
        let result = successful_json(
            harness
                .call("compresso_compress", json!({ "text": input }))
                .await,
        );
        observed += result["before_tokens"]["openai_o200k"]
            .as_u64()
            .expect("before count should be numeric");
        applied += result["after_tokens"]["openai_o200k"]
            .as_u64()
            .expect("after count should be numeric");
    }

    let stats = successful_json(harness.call("compresso_stats", json!({})).await);

    assert_eq!(stats["operations"], 2);
    assert_eq!(stats["observed_tokens"], observed);
    assert_eq!(stats["applied_tokens"], applied);
    assert_eq!(stats["net_tokens_saved"], observed.saturating_sub(applied));
    harness.close().await;
}

#[tokio::test]
async fn test_compress_records_root_valid_trace() {
    let _lock = compression_env_lock().await;
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let mut harness = Harness::start(store.clone()).await;
    let result = successful_json(
        harness
            .call(
                "compresso_compress",
                json!({ "text": LOG_FIXTURE, "max_tokens": 100_000 }),
            )
            .await,
    );
    let operation_id = result["operation_id"]
        .as_str()
        .expect("operation id should be present");

    let operation = store
        .operation_by_id(operation_id)
        .expect("operation lookup should succeed")
        .expect("compression operation should be retained");
    let trace: Value = serde_json::from_str(
        operation
            .trace_json
            .as_deref()
            .expect("compression operation should carry a trace"),
    )
    .expect("stored trace should parse");
    trace_validator()
        .validate(&trace)
        .expect("stored trace should satisfy the root schema");
    assert_eq!(trace["operation_id"], operation_id);
    harness.close().await;
}

#[tokio::test]
async fn test_segmented_compress_accounting_matches_payload_trace_and_ledger() {
    let _lock = compression_env_lock().await;
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let mut harness = Harness::start(store.clone()).await;
    let input = concat!(
        "First protected paragraph ends at a tokenizer boundary.\n\n",
        "A middle paragraph has repeated repeated repeated words.\n\n",
        "Final protected paragraph remains present.\n",
    );

    let result = successful_json(
        harness
            .call(
                "compresso_compress",
                json!({ "text": input, "max_tokens": 100_000 }),
            )
            .await,
    );
    let operation_id = result["operation_id"]
        .as_str()
        .expect("operation id should be present");
    let operation = store
        .operation_by_id(operation_id)
        .expect("operation lookup should succeed")
        .expect("compression operation should be retained");
    let trace: Value = serde_json::from_str(
        operation
            .trace_json
            .as_deref()
            .expect("compression trace should be retained"),
    )
    .expect("trace should parse");
    let before = result["before_tokens"]["openai_o200k"]
        .as_u64()
        .expect("before count should be numeric") as usize;
    let after = result["after_tokens"]["openai_o200k"]
        .as_u64()
        .expect("after count should be numeric") as usize;
    let compressed = result["compressed"]
        .as_str()
        .expect("compressed text should be present");

    assert_eq!(before, counter_for(TokenProvider::OpenAiO200k).count(input));
    assert_eq!(
        after,
        counter_for(TokenProvider::OpenAiO200k).count(compressed)
    );
    assert_eq!(operation.observed_tokens, before);
    assert_eq!(operation.applied_tokens, after);
    assert_eq!(trace["before_tokens"], before);
    assert_eq!(trace["after_tokens"], after);
    assert_eq!(trace["token_provider"], "openai_o200k");
    assert_eq!(result["token_counting_degraded"], false);
    assert_eq!(
        result["actual_token_providers"]["openai_o200k"],
        "openai_o200k"
    );
    assert_eq!(
        result["actual_token_providers"]["anthropic_calibrated"],
        "anthropic_calibrated"
    );
    harness.close().await;
}

#[tokio::test]
async fn test_engine_tools_reject_invalid_arguments() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let mut harness = Harness::start(store).await;

    for (name, arguments) in [
        ("compresso_compress", json!({ "text": "" })),
        (
            "compresso_compress",
            json!({ "text": "hello", "mode": "balanced" }),
        ),
        (
            "compresso_compress",
            json!({ "text": "hello", "max_tokens": 99 }),
        ),
        (
            "compresso_compress",
            json!({ "text": "hello", "max_latency_ms": 0 }),
        ),
        (
            "compresso_compress",
            json!({ "text": "hello", "unknown": true }),
        ),
        ("compresso_stats", json!({ "since": "tomorrow" })),
        ("compresso_explain", json!({ "operation_id": "" })),
    ] {
        let response = harness.call(name, arguments).await;
        assert_tool_error(&response, "invalid_arguments");
    }
    harness.close().await;
}

#[tokio::test]
async fn test_retrieval_records_tokens_served() {
    let fixture = ImpactFixture::new();
    let mut harness = Harness::start_for_root(fixture.store.clone(), fixture.root.clone()).await;

    let (result, rendered) = successful_json_and_text(
        harness
            .call("compresso_impact", fixture.impact_arguments())
            .await,
    );
    let operation_id = result["operation_id"]
        .as_str()
        .expect("impact operation should have an id");
    let operation = fixture
        .store
        .operation_by_id(operation_id)
        .expect("operation lookup should succeed")
        .expect("impact operation should be retained");

    assert!(operation.trace_json.is_none());
    assert_eq!(
        operation.tokens_served,
        Some(counter_for(TokenProvider::OpenAiO200k).count(&rendered))
    );
    assert_eq!(
        operation.tokens_served,
        result["tokens_used"].as_u64().map(|value| value as usize)
    );
    harness.close().await;
}

fn replace_edges(store: &Store, repo: i64, path: &str, targets: &[&str]) {
    let file = store
        .indexed_file(repo, path)
        .expect("file lookup should succeed")
        .expect("edge source should exist");
    let file_id = store
        .upsert_file(repo, &file)
        .expect("edge source should retain its id");
    let edges = targets
        .iter()
        .map(|target| EdgeRecord {
            to_path: (*target).to_owned(),
            kind: EdgeKind::Imports,
            resolution: EdgeResolution::Resolved,
        })
        .collect::<Vec<_>>();
    store
        .replace_symbols(file_id, &[], &edges)
        .expect("fixture edges should replace");
}

fn successful_json(response: Value) -> Value {
    successful_json_and_text(response).0
}

fn successful_json_and_text(response: Value) -> (Value, String) {
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
    let rendered = content[0]["text"]
        .as_str()
        .expect("tool content should be text")
        .to_owned();
    let value = serde_json::from_str(&rendered).expect("tool content should be structured JSON");
    (value, rendered)
}

fn assert_tool_error(response: &Value, code: &str) {
    assert!(
        response.get("error").is_none(),
        "tool error must not be a protocol error"
    );
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error should contain text");
    assert!(
        text.starts_with(&format!("[{code}]")),
        "unexpected tool error: {response}"
    );
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

async fn compression_env_lock() -> MutexGuard<'static, ()> {
    COMPRESSION_ENV_LOCK.lock().await
}

struct StrategyFailureEnv;

impl StrategyFailureEnv {
    fn enable() -> Self {
        // SAFETY: every test in this process that invokes compression holds
        // COMPRESSION_ENV_LOCK, so no peer reads or mutates this variable concurrently.
        unsafe { std::env::set_var("COMPRESSO_TEST_FAIL_STRATEGY", "collapse_logs") };
        Self
    }
}

impl Drop for StrategyFailureEnv {
    fn drop(&mut self) {
        // SAFETY: the guard is dropped before the associated mutex guard.
        unsafe { std::env::remove_var("COMPRESSO_TEST_FAIL_STRATEGY") };
    }
}
