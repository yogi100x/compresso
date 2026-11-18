use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_atlas::Indexer;
use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io, serve_io_for_root};
use compresso_store::Store;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(3);
const PROTOCOL_VERSION: &str = "2024-11-05";

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Harness {
    async fn start(store: Store) -> Self {
        let repositories = store
            .approved_repositories()
            .expect("approved repositories should load");
        let [repository] = repositories.as_slice() else {
            panic!("navigation harness requires exactly one repository");
        };
        Self::start_for_root(store, repository.root.clone()).await
    }

    async fn start_for_root(store: Store, root: PathBuf) -> Self {
        Self::start_scoped(store, Some(root)).await
    }

    async fn start_without_root(store: Store) -> Self {
        Self::start_scoped(store, None).await
    }

    async fn start_scoped(store: Store, root: Option<PathBuf>) -> Self {
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
                    "clientInfo": { "name": "compresso-nav-test", "version": "1.0.0" }
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
    repo: i64,
}

impl IndexedFixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("repository root should be created");
        for (path, source) in files {
            write_file(&root, path, source);
        }
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
        Self {
            _temp: temp,
            root,
            store,
            repo,
        }
    }
}

const FILES: [(&str, &str); 5] = [
    (
        "src/lib.rs",
        "mod a;\nmod b;\nmod store;\n\npub fn entry() {\n    middle();\n}\n\npub fn middle() {\n    store::save();\n}\n\npub fn recurse(n: u32) {\n    if n > 0 {\n        recurse(n - 1);\n    }\n}\n\npub fn ping() {\n    pong();\n}\n\npub fn pong() {\n    ping();\n}\n\npub fn lonely() {}\n",
    ),
    (
        "src/store.rs",
        "pub fn save() {\n    write();\n}\n\nfn write() {}\n",
    ),
    ("src/a.rs", "pub fn dup() {}\n"),
    ("src/b.rs", "pub fn dup() {}\n"),
    ("src/many.rs", "pub fn hub() {}\n"),
];

fn trace_fixture() -> IndexedFixture {
    let mut files = FILES.to_vec();
    let callers = (0..60)
        .map(|index| format!("pub fn caller_{index}() {{\n    crate::many::hub();\n}}\n"))
        .collect::<String>();
    let leaked: &'static str = Box::leak(callers.into_boxed_str());
    files.push(("src/callers.rs", leaked));
    IndexedFixture::new(&files)
}

async fn trace_call(fixture: IndexedFixture, arguments: Value) -> (IndexedFixture, Value) {
    let IndexedFixture {
        _temp,
        root,
        store,
        repo,
    } = fixture;
    let mut harness = Harness::start(store).await;
    let mut full = Map::from_iter([(
        "root".to_owned(),
        Value::String(root.to_string_lossy().into_owned()),
    )]);
    for (key, value) in arguments.as_object().expect("object").clone() {
        full.insert(key, value);
    }
    let response = harness.call("compresso_trace", Value::Object(full)).await;
    harness.close().await;
    let store = Store::open(&_temp.path().join("compresso.db")).expect("store should reopen");
    (
        IndexedFixture {
            _temp,
            root,
            store,
            repo,
        },
        response,
    )
}

#[tokio::test]
async fn test_trace_tool_returns_a_call_path_with_sites_and_signatures() {
    let (_fixture, response) =
        trace_call(trace_fixture(), json!({ "from": "entry", "to": "write" })).await;
    let payload = successful_json(response);
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["status"], "paths");
    let hops = payload["paths"][0].as_array().expect("one path");
    let rows = hops
        .iter()
        .map(|hop| {
            (
                hop["symbol"].as_str().unwrap_or_default().to_owned(),
                hop["defined_at"].as_str().unwrap_or_default().to_owned(),
                hop["call_site"].as_str().map(str::to_owned),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            ("entry".to_owned(), "src/lib.rs:5".to_owned(), None),
            (
                "middle".to_owned(),
                "src/lib.rs:9".to_owned(),
                Some("src/lib.rs:6".to_owned())
            ),
            (
                "save".to_owned(),
                "src/store.rs:1".to_owned(),
                Some("src/lib.rs:10".to_owned())
            ),
            (
                "write".to_owned(),
                "src/store.rs:5".to_owned(),
                Some("src/store.rs:2".to_owned())
            ),
        ]
    );
    assert_eq!(hops[3]["signature"], "fn write()");
    assert_eq!(hops[3]["kind"], "function");
    assert_eq!(hops[1]["resolution"], "same_file");
    assert_eq!(payload["complete"], true);
    assert_eq!(payload["truncated"], false);
    assert_eq!(payload["stale"], false);
    assert!(
        payload["tokens_used"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );
    assert!(
        payload["operation_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
}

#[tokio::test]
async fn test_trace_tool_reports_no_path() {
    let (_fixture, response) =
        trace_call(trace_fixture(), json!({ "from": "lonely", "to": "write" })).await;
    let payload = successful_json(response);
    assert_eq!(payload["status"], "no_path");
    assert_eq!(payload["paths"], json!([]));
    assert_eq!(payload["complete"], true);
}

#[tokio::test]
async fn test_trace_tool_returns_candidates_for_an_ambiguous_name() {
    let (_fixture, response) = trace_call(trace_fixture(), json!({ "to": "dup" })).await;
    let payload = successful_json(response);
    assert_eq!(payload["status"], "ambiguous");
    assert_eq!(payload["end"], "to");
    assert_eq!(payload["candidates_total"], 2);
    assert_eq!(payload["candidates"][0]["defined_at"], "src/a.rs:1");
    assert_eq!(payload["candidates"][1]["defined_at"], "src/b.rs:1");
}

#[tokio::test]
async fn test_trace_tool_depth_cap_is_reported() {
    let (_fixture, response) = trace_call(
        trace_fixture(),
        json!({ "from": "entry", "to": "write", "max_depth": 2 }),
    )
    .await;
    let payload = successful_json(response);
    assert_eq!(payload["status"], "no_path");
    assert_eq!(payload["depth_limited"], true);
    assert_eq!(payload["complete"], false);
    assert_eq!(payload["max_depth"], 2);
}

#[tokio::test]
async fn test_trace_tool_is_cycle_safe_and_marks_recursion() {
    let (fixture, response) =
        trace_call(trace_fixture(), json!({ "from": "ping", "max_depth": 8 })).await;
    let payload = successful_json(response);
    assert_eq!(payload["status"], "callees");
    let callees = payload["callees"].as_array().expect("callees");
    assert_eq!(callees.len(), 2);
    assert_eq!(callees[0]["symbol"], "pong");
    assert_eq!(callees[1]["symbol"], "ping");
    assert_eq!(callees[1]["cycle"], true);

    let (_fixture, response) = trace_call(fixture, json!({ "from": "recurse" })).await;
    let payload = successful_json(response);
    let callees = payload["callees"].as_array().expect("callees");
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0]["symbol"], "recurse");
    assert_eq!(callees[0]["call_site"], "src/lib.rs:15");
    assert_eq!(callees[0]["cycle"], true);
}

#[tokio::test]
async fn test_trace_tool_callers_report_total_and_truncation_in_one_call() {
    let (fixture, response) = trace_call(trace_fixture(), json!({ "to": "hub" })).await;
    let payload = successful_json(response);
    assert_eq!(payload["status"], "callers");
    assert_eq!(payload["total"], 50 + 10);
    assert_eq!(payload["shown"], 50);
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["complete"], false);
    let first = &payload["callers"][0];
    assert_eq!(first["symbol"], "caller_0");
    assert_eq!(first["call_site"], "src/callers.rs:2");
    // `via` is implied at depth 1 and omitted to save tokens.
    assert!(first.get("via").is_none());
    assert_eq!(first["depth"], 1);

    let (fixture, response) = trace_call(fixture, json!({ "to": "hub", "max_results": 500 })).await;
    let payload = successful_json(response);
    assert_eq!(payload["total"], 60);
    assert_eq!(payload["shown"], 60);
    assert_eq!(payload["truncated"], false);
    assert_eq!(payload["complete"], true);

    // The token cap trims the list and says so.
    let (_fixture, response) = trace_call(
        fixture,
        json!({ "to": "hub", "max_results": 500, "max_tokens": 200 }),
    )
    .await;
    let payload = successful_json(response);
    assert!(
        payload["tokens_used"]
            .as_u64()
            .is_some_and(|tokens| tokens <= 200)
    );
    assert!(payload["shown"].as_u64().is_some_and(|shown| shown < 60));
    assert_eq!(payload["total"], 60);
    assert_eq!(payload["truncated"], true);
}

#[tokio::test]
async fn test_trace_tool_reports_a_stale_index() {
    let fixture = trace_fixture();
    write_file(
        &fixture.root,
        "src/store.rs",
        "pub fn save() {\n    write();\n}\n\nfn write() {}\n// edited\n",
    );
    let (_fixture, response) = trace_call(fixture, json!({ "from": "entry", "to": "write" })).await;
    let payload = successful_json(response);
    assert_eq!(payload["status"], "paths");
    assert_eq!(payload["stale"], true);
    assert!(
        payload["reindex_hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("compresso index"))
    );
}

#[tokio::test]
async fn test_trace_tool_requires_reindex_when_the_index_predates_call_sites() {
    let fixture = trace_fixture();
    fixture
        .store
        .stamp_index_version(fixture.repo, "0.1.0+atlas-shell-text-v1+rust-imports-v2", 1)
        .expect("old version should be stamped");
    let (_fixture, response) = trace_call(fixture, json!({ "from": "entry", "to": "write" })).await;
    let payload = successful_json(response);
    assert_eq!(payload["status"], "reindex_required");
    assert_eq!(payload["complete"], false);
    assert!(payload["reindex_hint"].is_string());
}

#[tokio::test]
async fn test_trace_tool_rejects_invalid_arguments_and_unbound_sessions() {
    let (fixture, response) = trace_call(trace_fixture(), json!({})).await;
    assert_tool_error(&response, "invalid_arguments");
    let (fixture, response) = trace_call(fixture, json!({ "from": "entry", "max_depth": 9 })).await;
    assert_tool_error(&response, "invalid_arguments");
    let (fixture, response) = trace_call(fixture, json!({ "from": "entry", "cmd": "ls" })).await;
    assert_tool_error(&response, "invalid_arguments");

    let IndexedFixture { _temp, store, .. } = fixture;
    let mut harness = Harness::start_without_root(store).await;
    let response = harness
        .call("compresso_trace", json!({ "from": "entry" }))
        .await;
    assert_tool_error(&response, "not_indexed");
    let response = harness
        .call(
            "compresso_trace",
            json!({ "from": "entry", "root": _temp.path().to_string_lossy() }),
        )
        .await;
    assert_tool_error(&response, "outside_root");
    harness.close().await;
}

#[tokio::test]
async fn test_trace_tool_is_listed_and_records_a_retrieval_operation() {
    let fixture = trace_fixture();
    let IndexedFixture {
        _temp, root, store, ..
    } = fixture;
    let mut harness = Harness::start(store).await;
    let tools = harness.request("tools/list", json!({})).await;
    let declared = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "compresso_trace")
        .expect("compresso_trace should be declared")
        .clone();
    let description = declared["description"].as_str().expect("description");
    assert!(description.contains("who calls X"));
    assert!(description.contains("how A reaches B"));
    let response = harness
        .call(
            "compresso_trace",
            json!({ "root": root.to_string_lossy(), "to": "save" }),
        )
        .await;
    successful_json(response);
    let stats = successful_json(harness.call("compresso_stats", json!({})).await);
    harness.close().await;
    assert!(
        stats["by_tool"]
            .as_array()
            .expect("by_tool")
            .iter()
            .any(|row| row["tool"] == "compresso_trace" && row["operations"] == 1)
    );
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
    let content = response["result"]["content"]
        .as_array()
        .expect("tool error should contain content");
    assert_eq!(content.len(), 1);
    assert!(
        content[0]["text"]
            .as_str()
            .is_some_and(|text| text.starts_with(&format!("[{code}]"))),
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
    fs::write(destination, source).expect("fixture source should be written");
}
