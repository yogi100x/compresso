use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_atlas::Indexer;
use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io};
use compresso_store::Store;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(3);
const PROTOCOL_VERSION: &str = "2024-11-05";
const GATE_CANARY: &str = "gate_content_canary_8492";

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Harness {
    async fn start(store: Store) -> Self {
        let config = Config::default();
        let deps = McpDeps {
            store,
            engine: Engine::new(
                config.compression.mode,
                counter_for(TokenProvider::Heuristic),
            ),
            config,
        };
        let (client, server) = tokio::io::duplex(256 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let server = tokio::spawn(serve_io(deps, server_read, server_write));
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
                    "clientInfo": { "name": "compresso-read-test", "version": "1.0.0" }
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
    source: String,
}

impl IndexedFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("repository root should be created");
        let mut source = concat!(
            "pub struct SampleReader {\n",
            "    values: usize,\n",
            "}\n",
            "\n",
            "impl SampleReader {\n",
            "    pub fn has_values(&self) -> bool {\n",
            "        let gate_content_canary_8492 = true;\n",
            "        gate_content_canary_8492 && self.values > 0\n",
            "    }\n",
            "}\n",
        )
        .to_owned();
        for index in 0..300 {
            source.push_str(&format!(
                "pub fn helper_{index:03}() -> usize {{ {index} }}\n"
            ));
        }
        write_file(&root, "src/sample.rs", &source);
        write_file(
            &root,
            "src/session.rs",
            concat!(
                "pub struct SessionManager {\n",
                "    attempts: usize,\n",
                "}\n",
                "impl SessionManager {\n",
                "    pub fn retry_session(&self) -> bool { self.attempts < 3 }\n",
                "}\n",
            ),
        );
        write_file(
            &root,
            "src/cache.rs",
            "pub struct CacheManager;\nimpl CacheManager { pub fn clear(&self) {} }\n",
        );

        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
            .index(&root, |_| {})
            .expect("fixture should index");
        let root = fs::canonicalize(root).expect("repository root should canonicalize");
        let repo = store
            .repo_by_root(&root)
            .expect("repository lookup should succeed")
            .expect("repository should be approved")
            .id;

        let file = store
            .indexed_file(repo, "src/session.rs")
            .expect("indexed file should load")
            .expect("session should be indexed");
        let mut symbols = store
            .symbols_in_file(repo, "src/session.rs")
            .expect("symbols should load");
        let manager = symbols
            .iter_mut()
            .find(|symbol| symbol.qualified_name == "SessionManager")
            .expect("manager symbol should exist");
        manager.signature = Some("pub struct SessionManager // session retry logic".to_owned());
        store
            .replace_indexed_file(repo, &file, &symbols, &[])
            .expect("context search signature should seed");

        Self {
            _temp: temp,
            root,
            store,
            source,
        }
    }

    fn arguments(&self) -> Map<String, Value> {
        Map::from_iter([(
            "root".to_owned(),
            Value::String(self.root.to_string_lossy().into_owned()),
        )])
    }
}

#[tokio::test]
async fn test_read_tool_bounded() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("path".to_owned(), json!("src/sample.rs"));
    arguments.insert("max_tokens".to_owned(), json!(100));

    let result = successful_json(
        harness
            .call("compresso_read", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["path"], "src/sample.rs");
    assert!(
        result["tokens_used"]
            .as_u64()
            .is_some_and(|used| used <= 100)
    );
    assert_eq!(result["truncated"], true);
    assert!(
        result["elision"]
            .as_str()
            .is_some_and(|text| text.contains("elided"))
    );
    assert!(
        result["content"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    harness.close().await;
}

#[tokio::test]
async fn test_read_gate_mode_no_content() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("path".to_owned(), json!("src/sample.rs"));
    arguments.insert("gate".to_owned(), json!(true));

    let response = harness
        .call("compresso_read", Value::Object(arguments))
        .await;
    let serialized = response.to_string();
    let result = successful_json(response);

    assert!(result.get("content").is_none());
    assert!(result.get("range").is_none());
    assert!(result.get("elision").is_none());
    assert!(result["gate"]["worth_reading"].is_boolean());
    assert!(result["gate"]["reason"].is_string());
    assert!(
        result["gate"]["file_tokens_total"]
            .as_u64()
            .is_some_and(|count| count > 100)
    );
    assert!(
        result["gate"]["recommended_budget"]
            .as_u64()
            .is_some_and(|count| count >= 100)
    );
    assert!(result["gate"]["cheaper_alternative"].is_string());
    assert!(!serialized.contains(GATE_CANARY));
    assert!(!serialized.contains("self.values > 0"));
    harness.close().await;
}

#[tokio::test]
async fn test_read_dotdot_via_mcp() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("path".to_owned(), json!("../../etc/passwd"));

    let response = harness
        .call("compresso_read", Value::Object(arguments))
        .await;

    assert_tool_error(&response, "outside_root");
    assert!(!response.to_string().contains("/etc/passwd"));
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_read_symlink_escape_via_mcp() {
    use std::os::unix::fs::symlink;

    let fixture = IndexedFixture::new();
    let outside = fixture._temp.path().join("outside.txt");
    fs::write(&outside, "outside secret").expect("outside file should be written");
    symlink(&outside, fixture.root.join("escape.txt")).expect("escape symlink should be created");
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("path".to_owned(), json!("escape.txt"));

    let response = harness
        .call("compresso_read", Value::Object(arguments))
        .await;

    assert_tool_error(&response, "outside_root");
    assert!(
        !response
            .to_string()
            .contains(outside.to_string_lossy().as_ref())
    );
    harness.close().await;
}

#[tokio::test]
async fn test_search_tool_hits() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("query".to_owned(), json!("SessionManager"));

    let result = successful_json(
        harness
            .call("compresso_search", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["query"], "SessionManager");
    assert_eq!(result["hits"][0]["qualified_name"], "SessionManager");
    assert_eq!(result["hits"][0]["path"], "src/session.rs");
    assert!(result["hits"][0]["start_line"].as_u64().is_some());
    assert!(result["hits"][0]["end_line"].as_u64().is_some());
    assert!(
        result["tokens_used"]
            .as_u64()
            .is_some_and(|used| used <= 2_000)
    );
    assert_eq!(result["stale"], false);
    harness.close().await;
}

#[tokio::test]
async fn test_search_tool_budget_is_deterministic() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("query".to_owned(), json!("helper"));
    arguments.insert("limit".to_owned(), json!(100));
    arguments.insert("max_tokens".to_owned(), json!(100));

    let first = successful_json(
        harness
            .call("compresso_search", Value::Object(arguments.clone()))
            .await,
    );
    let second = successful_json(
        harness
            .call("compresso_search", Value::Object(arguments))
            .await,
    );

    assert_eq!(first["hits"], second["hits"]);
    assert_eq!(first["hits_total"], 100);
    assert_eq!(first["truncated"], true);
    assert!(
        first["tokens_used"]
            .as_u64()
            .is_some_and(|used| used <= 100)
    );
    harness.close().await;
}

#[tokio::test]
async fn test_search_stale_is_structured_success() {
    let fixture = IndexedFixture::new();
    fs::write(
        fixture.root.join("src/sample.rs"),
        format!("{}\n// changed after indexing\n", fixture.source),
    )
    .expect("sample should change");
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("query".to_owned(), json!("SessionManager"));

    let result = successful_json(
        harness
            .call("compresso_search", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["stale"], true);
    assert!(
        result["reindex_hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("compresso index"))
    );
    harness.close().await;
}

#[tokio::test]
async fn test_context_tool_budget_and_sections() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("task".to_owned(), json!("session retry logic"));
    arguments.insert("max_tokens".to_owned(), json!(800));

    let result = successful_json(
        harness
            .call("compresso_context", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["task"], "session retry logic");
    assert!(
        result["tokens_used"]
            .as_u64()
            .is_some_and(|used| used <= 800)
    );
    assert!(result["plan"]["map_excerpt"].is_string());
    assert!(result["plan"]["outlines"].is_array());
    assert!(result["plan"]["bodies"].is_array());
    assert!(
        result["plan"]["outlines"]
            .as_array()
            .is_some_and(|outlines| !outlines.is_empty())
    );
    assert!(
        result["plan"]["bodies"]
            .as_array()
            .is_some_and(|bodies| !bodies.is_empty())
    );
    assert_eq!(result["plan"]["bodies"][0]["symbol"], "SessionManager");
    harness.close().await;
}

#[tokio::test]
async fn test_read_start_line_without_end_reads_to_eof() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("path".to_owned(), json!("src/session.rs"));
    arguments.insert("start_line".to_owned(), json!(4));

    let result = successful_json(
        harness
            .call("compresso_read", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["range"]["start_line"], 4);
    assert_eq!(result["range"]["end_line"], 6);
    assert!(
        result["content"]
            .as_str()
            .is_some_and(|content| content.starts_with("impl SessionManager"))
    );
    harness.close().await;
}

#[tokio::test]
async fn test_read_missing_and_binary_errors_are_stable() {
    let fixture = IndexedFixture::new();
    fs::write(fixture.root.join("binary.dat"), [0xff, 0xfe, 0xfd])
        .expect("binary fixture should be written");
    let mut harness = Harness::start(fixture.store.clone()).await;

    for (path, code) in [
        ("src/missing.rs", "not_found"),
        ("binary.dat", "invalid_arguments"),
    ] {
        let mut arguments = fixture.arguments();
        arguments.insert("path".to_owned(), json!(path));
        let response = harness
            .call("compresso_read", Value::Object(arguments))
            .await;
        assert_tool_error(&response, code);
    }
    harness.close().await;
}

#[tokio::test]
async fn test_read_tools_reject_invalid_arguments_and_unknown_fields() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;

    let cases = [
        (
            "compresso_search",
            json!({ "root": fixture.root, "query": "", "limit": 20 }),
        ),
        (
            "compresso_search",
            json!({ "root": fixture.root, "query": "SessionManager", "limit": 101 }),
        ),
        (
            "compresso_read",
            json!({ "root": fixture.root, "path": "src/sample.rs", "end_line": 2 }),
        ),
        (
            "compresso_read",
            json!({ "root": fixture.root, "path": "src/sample.rs", "gate": "yes" }),
        ),
        (
            "compresso_context",
            json!({ "root": fixture.root, "task": "ab" }),
        ),
        (
            "compresso_context",
            json!({ "root": fixture.root, "task": "session retry logic", "max_tokens": 99 }),
        ),
        (
            "compresso_context",
            json!({ "root": fixture.root, "task": "session retry logic", "extra": true }),
        ),
    ];
    for (name, arguments) in cases {
        let response = harness.call(name, arguments).await;
        assert_tool_error(&response, "invalid_arguments");
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

fn assert_tool_error(response: &Value, code: &str) {
    assert!(
        response.get("error").is_none(),
        "tool error must not be protocol error"
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

fn write_file(root: &Path, path: &str, source: &str) {
    let destination = root.join(path);
    fs::create_dir_all(
        destination
            .parent()
            .expect("fixture path should have a parent"),
    )
    .expect("fixture parent should be created");
    fs::write(destination, source).expect("fixture should be written");
}
