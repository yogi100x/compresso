use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_atlas::Indexer;
use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io_for_root};
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
        let repositories = store
            .approved_repositories()
            .expect("approved repositories should load");
        let [repository] = repositories.as_slice() else {
            panic!("read harness requires exactly one repository");
        };
        let root = repository.root.clone();
        Self::start_for_root(store, root).await
    }

    async fn start_for_root(store: Store, root: PathBuf) -> Self {
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
        let server = tokio::spawn(serve_io_for_root(deps, root, server_read, server_write));
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

    fn add_authentication_files(&self, count: usize) {
        for index in 0..count {
            let concept = match index % 5 {
                0 => "login",
                1 => "oauth",
                2 => "credential",
                3 => "cookie",
                _ => "access_token",
            };
            write_file(
                &self.root,
                &format!("src/auth_{index:02}.rs"),
                &format!(
                    concat!(
                        "pub fn {concept}_provider_{index:02}() -> &'static str {{\n",
                        "    let credential = \"authentication session credential ",
                        "provider login callback token refresh storage validation\";\n",
                        "    credential\n",
                        "}}\n",
                    ),
                    concept = concept,
                    index = index,
                ),
            );
        }
        Indexer::new(&self.store, counter_for(TokenProvider::OpenAiO200k))
            .index(&self.root, |_| {})
            .expect("expanded fixture should reindex");
    }

    fn add_shell_and_documentation_auth_evidence(&self) {
        write_file(
            &self.root,
            "scripts/refresh-token.sh",
            "#!/bin/sh\n# Refresh the access_token used by the local worker.\nACCESS_TOKEN_NAME=access_token\n",
        );
        write_file(
            &self.root,
            "docs/provider.md",
            "# access_token\n\nThe access_token appears in user documentation.\n",
        );
        Indexer::new(&self.store, counter_for(TokenProvider::OpenAiO200k))
            .index(&self.root, |_| {})
            .expect("shell and text fixture should reindex");
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

#[cfg(unix)]
#[tokio::test]
async fn test_bound_root_retarget_cannot_leak_another_approved_repository() {
    use std::os::unix::fs::symlink;

    const REPO_B_CANARY: &str = "repo_b_private_canary_7319";
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root_a = temp.path().join("repo-a");
    let root_b = temp.path().join("repo-b");
    write_file(
        &root_a,
        "src/session.rs",
        "pub fn session_target() -> &'static str { \"repository a\" }\n",
    );
    write_file(
        &root_b,
        "src/session.rs",
        &format!("pub fn session_target() -> &'static str {{ \"{REPO_B_CANARY}\" }}\n"),
    );
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let indexer = Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k));
    indexer.index(&root_a, |_| {}).expect("repo A should index");
    indexer.index(&root_b, |_| {}).expect("repo B should index");
    let root_a = fs::canonicalize(root_a).expect("repo A should canonicalize");
    let root_b = fs::canonicalize(root_b).expect("repo B should canonicalize");
    let mut harness = Harness::start_for_root(store, root_a.clone()).await;
    let parked_a = root_a.with_file_name("repo-a-before-retarget");
    fs::rename(&root_a, &parked_a).expect("repo A should move away from its bound path");
    symlink(&root_b, &root_a).expect("bound path should retarget to repo B");

    for (tool, arguments) in [
        (
            "compresso_context",
            json!({ "task": "session target", "max_tokens": 800 }),
        ),
        (
            "compresso_read",
            json!({ "path": "src/session.rs", "max_tokens": 800 }),
        ),
    ] {
        let response = harness.call(tool, arguments).await;
        assert_tool_error(&response, "outside_root");
        let rendered = response.to_string();
        assert!(!rendered.contains(REPO_B_CANARY));
        assert!(!rendered.contains(root_b.to_string_lossy().as_ref()));
    }
    harness.close().await;
}

#[tokio::test]
async fn test_secret_pattern_content_never_reaches_mcp() {
    const SECRET_CONTENT_CANARY: &str = "mcp_secret_content_canary_4628";
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root = temp.path().join("repo");
    write_file(
        &root,
        "src/visible.rs",
        "pub fn visible_symbol() -> bool { true }\n",
    );
    write_file(
        &root,
        ".ENV.private.rs",
        &format!(
            "pub fn never_indexed_symbol() -> &'static str {{ \"{SECRET_CONTENT_CANARY}\" }}\n"
        ),
    );
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let report = Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
        .index(&root, |_| {})
        .expect("repository should index");
    assert_eq!(report.files_skipped, 1);
    let mut harness = Harness::start(store).await;

    for response in [
        harness
            .call(
                "compresso_search",
                json!({ "query": "never_indexed_symbol" }),
            )
            .await,
        harness
            .call(
                "compresso_context",
                json!({ "task": "never indexed symbol", "max_tokens": 800 }),
            )
            .await,
        harness.call("compresso_repo_map", json!({})).await,
    ] {
        let rendered = response.to_string();
        assert!(!rendered.contains(SECRET_CONTENT_CANARY));
        assert!(!rendered.contains(".ENV.private.rs"));
    }

    let response = harness
        .call(
            "compresso_read",
            json!({ "path": ".ENV.private.rs", "max_tokens": 800 }),
        )
        .await;
    assert_tool_error(&response, "not_found");
    assert!(!response.to_string().contains(SECRET_CONTENT_CANARY));
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
async fn test_search_truncation_reports_line_numbers_for_dropped_hits() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("query".to_owned(), json!("helper"));
    arguments.insert("limit".to_owned(), json!(100));
    arguments.insert("max_tokens".to_owned(), json!(200));

    let result = successful_json(
        harness
            .call("compresso_search", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["truncated"], true);
    let hits = result["hits"].as_array().expect("hits should be an array");
    let dropped = result["truncated_hits"]
        .as_array()
        .expect("truncated hits should be an array");
    assert!(!hits.is_empty());
    assert!(!dropped.is_empty());
    for location in dropped {
        assert_eq!(location["path"], "src/sample.rs");
        let start = location["start_line"]
            .as_u64()
            .expect("start line should be reported");
        let end = location["end_line"]
            .as_u64()
            .expect("end line should be reported");
        assert!(start >= 1 && end >= start);
        assert!(location.get("signature").is_none());
    }
    let omitted = result["truncated_hits_omitted"]
        .as_u64()
        .expect("omitted count should be reported");
    assert_eq!(hits.len() as u64 + dropped.len() as u64 + omitted, 100);
    assert!(
        result["tokens_used"]
            .as_u64()
            .is_some_and(|used| used <= 200)
    );
    harness.close().await;
}

#[tokio::test]
async fn test_search_untruncated_result_has_no_truncated_hits() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("query".to_owned(), json!("SessionManager"));

    let result = successful_json(
        harness
            .call("compresso_search", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["truncated"], false);
    assert_eq!(result["truncated_hits"], json!([]));
    assert_eq!(result["truncated_hits_omitted"], 0);
    assert_eq!(result["diagnostics"], json!([]));
    harness.close().await;
}

#[tokio::test]
async fn test_search_missing_path_scope_is_informational_success() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("query".to_owned(), json!("SessionManager"));
    arguments.insert("path".to_owned(), json!("src/missing"));

    let result = successful_json(
        harness
            .call("compresso_search", Value::Object(arguments.clone()))
            .await,
    );

    assert_eq!(result["hits"], json!([]));
    assert_eq!(result["path"], "src/missing");
    assert_eq!(result["diagnostics"][0]["code"], "path_not_found");
    assert_eq!(result["diagnostics"][0]["severity"], "info");
    assert!(
        result["diagnostics"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("src/missing"))
    );

    arguments.insert("path".to_owned(), json!("src/session.rs"));
    let scoped = successful_json(
        harness
            .call("compresso_search", Value::Object(arguments.clone()))
            .await,
    );
    assert_eq!(scoped["hits"][0]["path"], "src/session.rs");
    assert_eq!(scoped["diagnostics"], json!([]));

    arguments.insert("path".to_owned(), json!("../outside"));
    let escaped = harness
        .call("compresso_search", Value::Object(arguments))
        .await;
    assert_tool_error(&escaped, "invalid_arguments");
    harness.close().await;
}

#[tokio::test]
async fn test_search_invalid_query_reports_position_and_reason() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    for (query, position, reason) in [
        ("Session\u{0}Manager", 7, "NUL"),
        ("   ", 0, "whitespace"),
        ("", 0, "empty"),
    ] {
        let mut arguments = fixture.arguments();
        arguments.insert("query".to_owned(), json!(query));
        let response = harness
            .call("compresso_search", Value::Object(arguments))
            .await;
        assert_tool_error(&response, "invalid_arguments");
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("tool error should contain text");
        assert!(text.contains(&format!("byte {position}")), "{text}");
        assert!(text.contains(reason), "{text}");
    }
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

    let response = harness
        .call("compresso_context", Value::Object(arguments))
        .await;
    let payload = response["result"]["content"][0]["text"]
        .as_str()
        .expect("context payload should be text")
        .to_owned();
    let payload_tokens = counter_for(TokenProvider::OpenAiO200k).count(&payload);
    let result = successful_json(response);

    assert_eq!(result["retrieval_status"], "matched");
    assert_eq!(result["context_sufficiency"], "task_pack_ready");
    assert_eq!(result["next_action"], "answer_from_pack");
    assert_eq!(result["broad_follow_up_required"], false);
    assert_eq!(result["targeted_follow_up_required"], false);
    assert_eq!(result["targeted_follow_up_limit"], 0);
    assert!(
        result["evidence_coverage"]["files_included"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert_eq!(result["budget_applied"], 800);
    assert_eq!(result["budget_ceiling"], 800);
    assert_eq!(result["budget_expanded"], false);
    assert!(result["budget_expansion_reason"].is_null());
    assert_eq!(result["budget_clamped"], false);
    assert!(result["retrieval_note"].is_null());
    assert!(
        result["matches_total"]
            .as_u64()
            .is_some_and(|matches| matches > 0)
    );
    assert_eq!(result["tokens_used"], payload_tokens);
    assert!(payload_tokens <= 800);
    assert!(
        result["content_pack"]
            .as_str()
            .is_some_and(|content| content.contains("SessionManager::retry_session"))
    );
    assert!(result.get("task").is_none());
    assert!(result.get("search_queries").is_none());
    assert!(result.get("plan").is_none());
    let stats = successful_json(harness.call("compresso_stats", json!({ "limit": 5 })).await);
    assert_eq!(
        stats["retrieval_savings"]["retrieval_pack_tokens"],
        payload_tokens
    );
    assert!(
        stats["retrieval_savings"]["estimated_raw_source_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );
    assert!(
        stats["retrieval_savings"]["follow_up_calls_avoided"]
            .as_u64()
            .is_some_and(|calls| calls > 0)
    );
    assert_eq!(stats["retrieval_savings"]["basis"], "modeled");
    assert_eq!(
        stats["by_repository"][0]["repository"],
        fixture.root.to_string_lossy().as_ref()
    );
    assert_eq!(stats["by_operation"][0]["tool"], "compresso_context");
    harness.close().await;
}

#[tokio::test]
async fn test_context_tool_clamps_oversized_ceiling_but_stops_at_smallest_ready_pack() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("task".to_owned(), json!("session retry logic"));
    arguments.insert("max_tokens".to_owned(), json!(10_000));

    let response = harness
        .call("compresso_context", Value::Object(arguments))
        .await;
    let payload = response["result"]["content"][0]["text"]
        .as_str()
        .expect("context payload should be text")
        .to_owned();
    let payload_tokens = counter_for(TokenProvider::OpenAiO200k).count(&payload);
    let result = successful_json(response);

    assert_eq!(result["retrieval_status"], "matched");
    assert_eq!(result["context_sufficiency"], "task_pack_ready");
    assert_eq!(result["next_action"], "answer_from_pack");
    assert_eq!(result["broad_follow_up_required"], false);
    assert_eq!(result["targeted_follow_up_required"], false);
    assert_eq!(result["targeted_follow_up_limit"], 0);
    assert_eq!(result["budget_applied"], 800);
    assert_eq!(result["budget_ceiling"], 4_000);
    assert_eq!(result["budget_expanded"], false);
    assert!(result["budget_expansion_reason"].is_null());
    assert_eq!(result["budget_clamped"], true);
    assert_eq!(result["tokens_used"], payload_tokens);
    assert!(payload_tokens <= 800);
    harness.close().await;
}

#[tokio::test]
async fn test_context_tool_adaptively_expands_to_smallest_sufficient_budget() {
    let fixture = IndexedFixture::new();
    fixture.add_authentication_files(20);
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert(
        "task".to_owned(),
        json!("authentication login credential session"),
    );
    arguments.insert("max_tokens".to_owned(), json!(4_000));

    let response = harness
        .call("compresso_context", Value::Object(arguments))
        .await;
    let payload = response["result"]["content"][0]["text"]
        .as_str()
        .expect("context payload should be text")
        .to_owned();
    let payload_tokens = counter_for(TokenProvider::OpenAiO200k).count(&payload);
    let result = successful_json(response);

    assert_eq!(result["context_sufficiency"], "task_pack_ready");
    assert_eq!(result["next_action"], "answer_from_pack");
    let budget_applied = result["budget_applied"]
        .as_u64()
        .expect("applied budget should be numeric");
    assert!(
        matches!(budget_applied, 1_600 | 3_200 | 4_000),
        "adaptive pack should expand through a declared stage: {result}"
    );
    assert_eq!(result["budget_ceiling"], 4_000);
    assert_eq!(result["budget_expanded"], true);
    assert_eq!(
        result["budget_expansion_reason"],
        "insufficient_evidence_coverage_at_lower_budget"
    );
    assert_eq!(result["budget_clamped"], false);
    assert_eq!(
        result["evidence_coverage"]["files_included"],
        result["evidence_coverage"]["files_required"]
    );
    assert_eq!(
        result["evidence_coverage"]["files_required"],
        result["evidence_coverage"]["candidate_files"]
    );
    assert_eq!(result["targeted_follow_up_limit"], 0);
    assert_eq!(result["tokens_used"], payload_tokens);
    assert!(payload_tokens <= budget_applied as usize);

    let preceding_ceiling = match budget_applied {
        1_600 => 800,
        3_200 => 1_600,
        4_000 => 3_200,
        _ => unreachable!("asserted adaptive stage"),
    };
    let mut preceding_arguments = fixture.arguments();
    preceding_arguments.insert(
        "task".to_owned(),
        json!("authentication login credential session"),
    );
    preceding_arguments.insert("max_tokens".to_owned(), json!(preceding_ceiling));
    let preceding = successful_json(
        harness
            .call("compresso_context", Value::Object(preceding_arguments))
            .await,
    );
    assert_eq!(
        preceding["context_sufficiency"], "targeted_follow_up_needed",
        "the prior stage must remain insufficient"
    );
    harness.close().await;
}

#[tokio::test]
async fn test_context_tool_prefers_shell_implementation_and_keeps_text_opt_in() {
    let fixture = IndexedFixture::new();
    fixture.add_shell_and_documentation_auth_evidence();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut implementation_arguments = fixture.arguments();
    implementation_arguments.insert("task".to_owned(), json!("access_token"));
    implementation_arguments.insert("max_tokens".to_owned(), json!(4_000));

    let implementation = successful_json(
        harness
            .call("compresso_context", Value::Object(implementation_arguments))
            .await,
    );
    let implementation_pack = implementation["content_pack"]
        .as_str()
        .expect("implementation pack should be text");
    assert_eq!(implementation["context_sufficiency"], "task_pack_ready");
    assert!(implementation_pack.contains("scripts/refresh-token.sh"));
    assert!(!implementation_pack.contains("docs/provider.md"));
    assert!(
        implementation["budget_applied"]
            .as_u64()
            .is_some_and(|budget| budget <= 1_600)
    );

    let mut documentation_arguments = fixture.arguments();
    documentation_arguments.insert("task".to_owned(), json!("documentation about access_token"));
    documentation_arguments.insert("max_tokens".to_owned(), json!(4_000));
    let documentation = successful_json(
        harness
            .call("compresso_context", Value::Object(documentation_arguments))
            .await,
    );
    let documentation_pack = documentation["content_pack"]
        .as_str()
        .expect("documentation pack should be text");
    assert!(documentation_pack.contains("scripts/refresh-token.sh"));
    assert!(documentation_pack.contains("docs/provider.md"));
    harness.close().await;
}

#[tokio::test]
async fn test_context_tool_respects_explicit_800_token_ceiling() {
    let fixture = IndexedFixture::new();
    fixture.add_authentication_files(20);
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert(
        "task".to_owned(),
        json!("authentication login credential session"),
    );
    arguments.insert("max_tokens".to_owned(), json!(800));

    let response = harness
        .call("compresso_context", Value::Object(arguments))
        .await;
    let payload = response["result"]["content"][0]["text"]
        .as_str()
        .expect("context payload should be text")
        .to_owned();
    let payload_tokens = counter_for(TokenProvider::OpenAiO200k).count(&payload);
    let result = successful_json(response);

    assert_eq!(result["context_sufficiency"], "targeted_follow_up_needed");
    assert_eq!(result["next_action"], "targeted_follow_up");
    assert_eq!(result["budget_applied"], 800);
    assert_eq!(result["budget_ceiling"], 800);
    assert_eq!(result["budget_expanded"], false);
    assert!(result["budget_expansion_reason"].is_null());
    assert_eq!(result["budget_clamped"], false);
    assert!(
        result["evidence_coverage"]["files_included"]
            .as_u64()
            .zip(result["evidence_coverage"]["candidate_files"].as_u64())
            .is_some_and(|(included, candidates)| included < candidates)
    );
    assert_eq!(result["targeted_follow_up_limit"], 2);
    assert_eq!(result["tokens_used"], payload_tokens);
    assert!(payload_tokens <= 800);
    harness.close().await;
}

#[tokio::test]
async fn test_context_tool_only_advertises_two_calls_for_targeted_follow_up() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("task".to_owned(), json!("manager reader"));
    arguments.insert("max_tokens".to_owned(), json!(300));

    let result = successful_json(
        harness
            .call("compresso_context", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["context_sufficiency"], "targeted_follow_up_needed");
    assert_eq!(result["next_action"], "targeted_follow_up");
    assert_eq!(result["broad_follow_up_required"], false);
    assert_eq!(result["targeted_follow_up_required"], true);
    assert_eq!(result["targeted_follow_up_limit"], 2);
    harness.close().await;
}

#[tokio::test]
async fn test_context_tool_stale_pack_requests_reindex_without_targeted_calls() {
    let fixture = IndexedFixture::new();
    fs::write(
        fixture.root.join("src/session.rs"),
        "pub struct SessionManager;\n// changed after indexing\n",
    )
    .expect("session should change");
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("task".to_owned(), json!("session retry logic"));
    arguments.insert("max_tokens".to_owned(), json!(800));

    let result = successful_json(
        harness
            .call("compresso_context", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["context_sufficiency"], "follow_up_needed");
    assert_eq!(result["next_action"], "reindex_then_retry");
    assert_eq!(result["broad_follow_up_required"], true);
    assert_eq!(result["targeted_follow_up_required"], false);
    assert_eq!(result["targeted_follow_up_limit"], 0);
    let stats = successful_json(harness.call("compresso_stats", json!({ "limit": 1 })).await);
    assert_eq!(
        stats["by_operation"][0]["estimated_raw_source_tokens"],
        stats["by_operation"][0]["retrieval_pack_tokens"]
    );
    assert_eq!(stats["by_operation"][0]["retrieval_tokens_avoided"], 0);
    assert_eq!(stats["by_operation"][0]["follow_up_calls_avoided"], 0);
    harness.close().await;
}

#[tokio::test]
async fn test_context_tool_no_matches_never_claims_repository_absence() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert(
        "task".to_owned(),
        json!("locate quantum cryptography lattice handshake"),
    );
    arguments.insert("max_tokens".to_owned(), json!(800));

    let response = harness
        .call("compresso_context", Value::Object(arguments))
        .await;
    let payload = response["result"]["content"][0]["text"]
        .as_str()
        .expect("context payload should be text")
        .to_owned();
    let payload_tokens = counter_for(TokenProvider::OpenAiO200k).count(&payload);
    let result = successful_json(response);

    assert_eq!(result["retrieval_status"], "no_matches");
    assert_eq!(result["context_sufficiency"], "follow_up_needed");
    assert_eq!(result["next_action"], "broaden_task");
    assert_eq!(result["broad_follow_up_required"], true);
    assert_eq!(result["targeted_follow_up_required"], false);
    assert_eq!(result["targeted_follow_up_limit"], 0);
    assert_eq!(result["matches_total"], 0);
    assert!(
        result["retrieval_note"]
            .as_str()
            .is_some_and(|note| note.contains("not proof"))
    );
    assert_eq!(result["tokens_used"], payload_tokens);
    assert!(payload_tokens <= 800);
    assert!(result["content_pack"].is_string());
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
async fn test_read_end_line_beyond_eof_clamps_and_reports_resolved_range() {
    let fixture = IndexedFixture::new();
    let mut harness = Harness::start(fixture.store.clone()).await;
    let mut arguments = fixture.arguments();
    arguments.insert("path".to_owned(), json!("src/session.rs"));
    arguments.insert("start_line".to_owned(), json!(4));
    arguments.insert("end_line".to_owned(), json!(240));
    arguments.insert("max_tokens".to_owned(), json!(1_800));

    let result = successful_json(
        harness
            .call("compresso_read", Value::Object(arguments))
            .await,
    );

    assert_eq!(result["range"]["start_line"], 4);
    assert_eq!(result["range"]["end_line"], 6);
    assert_eq!(result["resolved_range"]["start_line"], 4);
    assert_eq!(result["resolved_range"]["end_line"], 6);
    assert_eq!(result["file_lines_total"], 6);
    assert_eq!(result["range_clamped"], true);
    assert!(result["elision"].is_null());
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
