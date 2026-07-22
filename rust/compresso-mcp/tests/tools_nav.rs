use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_atlas::{Indexer, outline};
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

    fn arguments(&self) -> Map<String, Value> {
        Map::from_iter([(
            "root".to_owned(),
            Value::String(self.root.to_string_lossy().into_owned()),
        )])
    }
}

#[tokio::test]
async fn test_repo_map_budget() {
    let files = (0..40)
        .map(|index| {
            (
                format!("src/module_{index:02}.rs"),
                format!("pub fn function_{index:02}() -> usize {{ {index} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let borrowed = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = IndexedFixture::new(&borrowed);
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("max_tokens".to_owned(), json!(100));
    arguments.insert("focus".to_owned(), json!("src/module_39.rs"));

    let result = successful_json(
        harness
            .call("compresso_repo_map", Value::Object(arguments.clone()))
            .await,
    );

    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["root"], fixture.root.to_string_lossy().as_ref());
    assert!(
        result["tokens_used"]
            .as_u64()
            .is_some_and(|used| used <= 100)
    );
    assert_eq!(result["truncated"], true);
    assert_eq!(result["stale"], false);
    assert!(
        result["map"]
            .as_str()
            .is_some_and(|map| map.contains("module_39.rs")),
        "focus should bias the map under a tight budget"
    );
    assert!(
        result["operation_id"]
            .as_str()
            .is_some_and(|id| id.len() == 26)
    );

    fs::write(
        fixture.root.join("src/module_39.rs"),
        "pub fn function_39() -> usize { 3900 }\n",
    )
    .expect("indexed source should change");
    let stale = successful_json(
        harness
            .call("compresso_repo_map", Value::Object(arguments))
            .await,
    );
    assert_eq!(stale["stale"], true);
    assert!(
        stale["reindex_hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("compresso index"))
    );
    harness.close().await;
}

#[tokio::test]
async fn test_outline_matches_atlas() {
    let fixture = IndexedFixture::new(&[(
        "src/lib.rs",
        "pub struct SessionManager;\nimpl SessionManager {\n    pub fn start(&self) -> bool { true }\n}\n",
    )]);
    let expected = outline(&fixture.store, fixture.repo, "src/lib.rs")
        .expect("direct Atlas outline should load");
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("path".to_owned(), json!("src/lib.rs"));

    let result = successful_json(
        harness
            .call("compresso_outline", Value::Object(arguments.clone()))
            .await,
    );

    assert_eq!(result["path"], expected.path);
    assert_eq!(result["language"], "rust");
    let actual = result["symbols"]
        .as_array()
        .expect("outline should contain symbols");
    assert_eq!(actual.len(), expected.symbols.len());
    for (actual, expected) in actual.iter().zip(expected.symbols) {
        assert_eq!(actual["qualified_name"], expected.qualified_name);
        assert_eq!(actual["start_line"], expected.start_line);
        assert_eq!(actual["end_line"], expected.end_line);
        assert_eq!(actual["token_estimate"], expected.token_estimate);
    }
    assert_eq!(result["stale"], false);
    assert_eq!(result["truncated"], false);

    fs::write(
        fixture.root.join("src/lib.rs"),
        "// changed without reindex\npub struct SessionManager;\n",
    )
    .expect("indexed source should change");
    let stale = successful_json(
        harness
            .call("compresso_outline", Value::Object(arguments))
            .await,
    );
    assert_eq!(stale["stale"], true);
    assert!(
        stale["reindex_hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("compresso index"))
    );
    harness.close().await;
}

#[tokio::test]
async fn test_unfold_unique() {
    let fixture = IndexedFixture::new(&[(
        "src/lib.rs",
        "pub fn unique(value: u32) -> u32 {\n    value + 1\n}\n",
    )]);
    let mut harness = Harness::start(fixture.store.clone()).await;
    let arguments = unfold_arguments(&fixture, "unique");

    let result = successful_json(
        harness
            .call("compresso_unfold", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["status"], "found");
    assert_eq!(result["symbol"]["qualified_name"], "unique");
    assert_eq!(
        result["content"],
        "pub fn unique(value: u32) -> u32 {\n    value + 1\n}\n"
    );
    assert_eq!(result["stale"], false);
    assert!(result["candidates"].as_array().is_some_and(Vec::is_empty));
    harness.close().await;
}

#[tokio::test]
async fn test_unfold_ambiguous_candidates() {
    let fixture = overload_fixture();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = unfold_arguments(&fixture, "Worker::run");
    arguments.insert("kind".to_owned(), json!("method"));

    let first = successful_json(
        harness
            .call("compresso_unfold", Value::Object(arguments.clone()))
            .await,
    );
    let second = successful_json(
        harness
            .call("compresso_unfold", Value::Object(arguments))
            .await,
    );

    assert_eq!(first["status"], "ambiguous");
    let candidates = first["candidates"]
        .as_array()
        .expect("ambiguous response should have candidates");
    assert_eq!(candidates.len(), 2);
    assert!(candidates.len() <= 20);
    assert_eq!(first["candidates"], second["candidates"]);
    assert!(candidates.iter().all(|candidate| {
        candidate["path"] == "src/lib.rs"
            && candidate["kind"] == "method"
            && candidate["signature_hash"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
    }));
    harness.close().await;
}

#[tokio::test]
async fn test_unfold_signature_disambiguates() {
    let fixture = overload_fixture();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut broad = unfold_arguments(&fixture, "Worker::run");
    broad.insert("kind".to_owned(), json!("method"));
    let ambiguous = successful_json(
        harness
            .call("compresso_unfold", Value::Object(broad.clone()))
            .await,
    );
    let desired = ambiguous["candidates"]
        .as_array()
        .and_then(|candidates| candidates.get(1))
        .expect("second overload should be a candidate");
    broad.insert(
        "signature_hash".to_owned(),
        desired["signature_hash"].clone(),
    );
    broad.insert("start_line_hint".to_owned(), desired["start_line"].clone());

    let result = successful_json(harness.call("compresso_unfold", Value::Object(broad)).await);

    assert_eq!(result["status"], "found");
    assert_eq!(result["symbol"]["start_line"], desired["start_line"]);
    assert!(
        result["content"]
            .as_str()
            .is_some_and(|content| content.contains("value: u8"))
    );
    harness.close().await;
}

#[tokio::test]
async fn test_unfold_stale_current() {
    let fixture = IndexedFixture::new(&[(
        "src/lib.rs",
        "pub fn target() -> u8 { 1 }\npub fn target(value: u8) -> u8 { value }\n",
    )]);
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut broad = unfold_arguments(&fixture, "target");
    let ambiguous = successful_json(
        harness
            .call("compresso_unfold", Value::Object(broad.clone()))
            .await,
    );
    let signature_hash = ambiguous["candidates"][0]["signature_hash"].clone();
    broad.insert("signature_hash".to_owned(), signature_hash);
    fs::write(
        fixture.root.join("src/lib.rs"),
        "// moved without reindex\n\npub fn target(value: u8) -> u8 { value + 1 }\npub fn target() -> u8 {\n    9\n}\n",
    )
    .expect("indexed file should change");

    let result = successful_json(harness.call("compresso_unfold", Value::Object(broad)).await);

    assert_eq!(result["status"], "found");
    assert_eq!(result["stale"], true);
    assert_eq!(result["content"], "pub fn target() -> u8 {\n    9\n}\n");
    assert!(
        result["reindex_hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("compresso index"))
    );
    harness.close().await;
}

#[tokio::test]
async fn test_unfold_deleted_not_found() {
    let fixture = IndexedFixture::new(&[("src/lib.rs", "pub fn removed() -> u8 { 1 }\n")]);
    fs::write(fixture.root.join("src/lib.rs"), "pub fn replacement() {}\n")
        .expect("indexed symbol should be deleted");
    let mut harness = Harness::start(fixture.store.clone()).await;

    let result = successful_json(
        harness
            .call(
                "compresso_unfold",
                Value::Object(unfold_arguments(&fixture, "removed")),
            )
            .await,
    );

    assert_eq!(result["status"], "not_found");
    assert_eq!(result["stale"], true);
    assert_eq!(result["qualified_name"], "removed");
    assert!(result["candidates"].as_array().is_some_and(Vec::is_empty));
    harness.close().await;
}

#[tokio::test]
async fn test_unfold_schema_bounds() {
    let fixture = IndexedFixture::new(&[("src/lib.rs", "pub fn target() {}\n")]);
    let mut harness = Harness::start(fixture.store.clone()).await;
    let invalid = [
        json!({ "path": "src/lib.rs", "qualified_name": "target", "max_tokens": 99 }),
        json!({ "path": "src/lib.rs", "qualified_name": "target", "start_line_hint": 0 }),
        json!({ "path": "src/lib.rs", "qualified_name": "target", "signature_hash": "ABC" }),
        json!({ "path": "src/lib.rs", "qualified_name": "target", "kind": "procedure" }),
        json!({ "path": "src/lib.rs", "qualified_name": "target", "surprise": true }),
    ];
    for arguments in invalid {
        let mut arguments = arguments
            .as_object()
            .expect("fixture arguments should be an object")
            .clone();
        arguments.insert(
            "root".to_owned(),
            Value::String(fixture.root.to_string_lossy().into_owned()),
        );
        let response = harness
            .call("compresso_unfold", Value::Object(arguments))
            .await;
        assert_tool_error(&response, "invalid_arguments");
    }

    let mut traversal = unfold_arguments(&fixture, "target");
    traversal.insert("path".to_owned(), json!("../outside.rs"));
    let response = harness
        .call("compresso_unfold", Value::Object(traversal))
        .await;
    assert_tool_error(&response, "outside_root");

    let outside = fixture._temp.path().join("outside");
    fs::create_dir(&outside).expect("outside directory should be created");
    let response = harness
        .call(
            "compresso_unfold",
            json!({
                "root": outside.to_string_lossy(),
                "path": "src/lib.rs",
                "qualified_name": "target"
            }),
        )
        .await;
    assert_tool_error(&response, "outside_root");
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_unfold_symlink_escape() {
    use std::os::unix::fs::symlink;

    let fixture = IndexedFixture::new(&[("src/lib.rs", "pub fn target() {}\n")]);
    let outside = fixture._temp.path().join("private.rs");
    fs::write(&outside, "pub fn private() {}\n").expect("outside file should be written");
    symlink(&outside, fixture.root.join("escape.rs")).expect("escape symlink should be created");
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = unfold_arguments(&fixture, "private");
    arguments.insert("path".to_owned(), json!("escape.rs"));

    let response = harness
        .call("compresso_unfold", Value::Object(arguments))
        .await;

    assert_tool_error(&response, "outside_root");
    assert!(
        !response
            .to_string()
            .contains(outside.to_string_lossy().as_ref()),
        "outside-root errors must not leak the resolved path"
    );
    harness.close().await;
}

#[tokio::test]
async fn test_session_bound_to_repo_a_cannot_access_approved_repo_b() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root_a = temp.path().join("repo-a");
    let root_b = temp.path().join("repo-b");
    write_file(&root_a, "src/lib.rs", "pub fn visible_from_a() {}\n");
    write_file(
        &root_b,
        "src/lib.rs",
        "pub fn secret_from_b_canary_9182() {}\n",
    );
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let indexer = Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k));
    indexer.index(&root_a, |_| {}).expect("repo A should index");
    indexer.index(&root_b, |_| {}).expect("repo B should index");
    let root_a = fs::canonicalize(root_a).expect("repo A should canonicalize");
    let root_b = fs::canonicalize(root_b).expect("repo B should canonicalize");
    let mut harness = Harness::start_for_root(store, root_a.clone()).await;

    let selected = successful_json(harness.call("compresso_repo_map", json!({})).await);
    assert_eq!(selected["root"], root_a.to_string_lossy().as_ref());

    let response = harness
        .call(
            "compresso_repo_map",
            json!({ "root": root_b.to_string_lossy() }),
        )
        .await;
    assert_tool_error(&response, "outside_root");
    let response_text = response.to_string();
    assert!(!response_text.contains(root_b.to_string_lossy().as_ref()));
    assert!(!response_text.contains("secret_from_b_canary_9182"));
    harness.close().await;
}

#[tokio::test]
async fn test_omitted_root_does_not_select_sole_approved_repository() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root_b = temp.path().join("repo-b");
    write_file(
        &root_b,
        "src/lib.rs",
        "pub fn sole_repo_secret_canary_2714() {}\n",
    );
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
        .index(&root_b, |_| {})
        .expect("repo B should index");
    let root_b = fs::canonicalize(root_b).expect("repo B should canonicalize");
    let mut harness = Harness::start_without_root(store).await;

    let omitted = harness.call("compresso_repo_map", json!({})).await;
    assert_tool_error(&omitted, "not_indexed");
    assert!(
        !omitted
            .to_string()
            .contains(root_b.to_string_lossy().as_ref())
    );

    let supplied = harness
        .call(
            "compresso_repo_map",
            json!({ "root": root_b.to_string_lossy() }),
        )
        .await;
    assert_tool_error(&supplied, "outside_root");
    assert!(
        !supplied
            .to_string()
            .contains(root_b.to_string_lossy().as_ref())
    );

    let compressed = successful_json(
        harness
            .call("compresso_compress", json!({ "text": "alpha beta gamma" }))
            .await,
    );
    let operation_id = compressed["operation_id"]
        .as_str()
        .expect("compression should return an operation id");
    let stats = successful_json(harness.call("compresso_stats", json!({})).await);
    assert!(stats["operations"].as_u64().is_some_and(|count| count >= 1));
    let explained = successful_json(
        harness
            .call("compresso_explain", json!({ "operation_id": operation_id }))
            .await,
    );
    assert_eq!(explained["explained_operation_id"], operation_id);
    harness.close().await;
}

fn overload_fixture() -> IndexedFixture {
    IndexedFixture::new(&[(
        "src/lib.rs",
        "pub struct Worker;\nimpl Worker {\n    pub fn run(&self) {}\n    pub fn run(&self, value: u8) -> u8 { value }\n}\n",
    )])
}

fn unfold_arguments(fixture: &IndexedFixture, qualified_name: &str) -> Map<String, Value> {
    let mut arguments = fixture.arguments();
    arguments.insert("path".to_owned(), json!("src/lib.rs"));
    arguments.insert("qualified_name".to_owned(), json!(qualified_name));
    arguments
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
