//! Opt-in `compresso_recall` behavior over synthetic transcripts in a scratch home.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, McpRecallOptions, serve_io_for_root, serve_io_with_recall};
use compresso_recall::RecallSource;
use compresso_store::{StatsFilter, Store};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const PROTOCOL_VERSION: &str = "2024-11-05";
const SECRET_CANARY: &str = "ghp_recallcanary0123456789abcdefghijkl";

struct Fixture {
    temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("work/repo");
        let other = temp.path().join("work/other");
        fs::create_dir_all(&root).expect("repository root should be created");
        fs::create_dir_all(&other).expect("other repository should be created");
        let root = fs::canonicalize(root).expect("repository should canonicalize");
        let other = fs::canonicalize(other).expect("other repository should canonicalize");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        store
            .approve_repo(&root)
            .expect("repository should approve");
        let fixture = Self { temp, root, store };
        fixture.write_transcript(
            ".claude/projects/-work-repo/aaaa.jsonl",
            &[
                claude_line(
                    &fixture.root,
                    "session-a",
                    "2026-05-01T10:00:00.000Z",
                    "Decision: store the lease window as 21 days in the license file.",
                ),
                claude_line(
                    &fixture.root,
                    "session-a",
                    "2026-05-01T10:01:00.000Z",
                    &format!("the lease push used {SECRET_CANARY} as the token"),
                ),
            ],
        );
        fixture.write_transcript(
            ".claude/projects/-work-other/bbbb.jsonl",
            &[claude_line(
                &other,
                "session-b",
                "2026-05-02T10:00:00.000Z",
                "Other repository lease note that must stay out of scope.",
            )],
        );
        fixture.write_transcript(
            ".codex/sessions/2026/05/03/rollout-2026-05-03T09-00-00-thread-c.jsonl",
            &[
                json!({ "timestamp": "2026-05-03T09:00:00.000Z", "type": "session_meta",
                    "payload": { "id": "thread-c", "cwd": fixture.root } }),
                json!({ "timestamp": "2026-05-03T09:00:05.000Z", "type": "response_item",
                    "payload": { "type": "message", "role": "assistant", "content": [
                        { "type": "output_text", "text": "Chose a lease renewal check on startup." }
                    ] } }),
            ],
        );
        fixture
    }

    fn home(&self) -> PathBuf {
        self.temp.path().join("home")
    }

    fn write_transcript(&self, relative: &str, lines: &[Value]) {
        let path = self.home().join(relative);
        fs::create_dir_all(path.parent().expect("transcript parent should exist"))
            .expect("transcript directory should be created");
        let body = lines
            .iter()
            .map(|line| line.to_string() + "\n")
            .collect::<String>();
        fs::write(path, body).expect("transcript should be written");
    }

    fn deps(&self) -> McpDeps {
        let config = Config::default();
        McpDeps {
            store: self.store.clone(),
            engine: Engine::new(
                config.compression.mode,
                counter_for(TokenProvider::OpenAiO200k),
            ),
            config,
        }
    }

    fn options(&self, sources: &[RecallSource]) -> McpRecallOptions {
        McpRecallOptions {
            home_dir: self.home(),
            sources: sources.to_vec(),
        }
    }
}

fn claude_line(cwd: &Path, session: &str, timestamp: &str, text: &str) -> Value {
    json!({
        "type": "assistant",
        "cwd": cwd,
        "sessionId": session,
        "timestamp": timestamp,
        "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] }
    })
}

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Harness {
    async fn start(deps: McpDeps, root: Option<PathBuf>, recall: Option<McpRecallOptions>) -> Self {
        let (client, server) = tokio::io::duplex(512 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let server = tokio::spawn(async move {
            match (recall, root) {
                (Some(recall), root) => {
                    serve_io_with_recall(deps, root, server_read, server_write, recall).await
                }
                (None, Some(root)) => {
                    serve_io_for_root(deps, root, server_read, server_write).await
                }
                (None, None) => compresso_mcp::serve_io(deps, server_read, server_write).await,
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
                    "clientInfo": { "name": "compresso-recall-test", "version": "1.0.0" }
                }),
            )
            .await;
        assert_eq!(response["result"]["serverInfo"]["name"], "compresso");
        harness
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }))
            .await;
        harness
    }

    async fn tool_names(&mut self) -> Vec<String> {
        let response = self.request("tools/list", json!({})).await;
        response["result"]["tools"]
            .as_array()
            .expect("tools/list should return an array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
            .collect()
    }

    /// Returns `(is_error, text)` for one tool call.
    async fn call(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let response = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        let result = &response["result"];
        (
            result["isError"].as_bool().unwrap_or(false),
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("tool call should return text: {response}"))
                .to_owned(),
        )
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        let mut line = String::new();
        let bytes = timeout(IO_TIMEOUT, self.output.read_line(&mut line))
            .await
            .expect("response read should not time out")
            .expect("response should be readable");
        assert!(bytes > 0, "server closed before sending a response");
        serde_json::from_str(&line).expect("response should be valid JSON-RPC")
    }

    async fn send(&mut self, message: &Value) {
        let mut frame = serde_json::to_vec(message).expect("request should serialize");
        frame.push(b'\n');
        timeout(IO_TIMEOUT, self.input.write_all(&frame))
            .await
            .expect("request write should not time out")
            .expect("request should be written");
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
async fn test_recall_tool_is_absent_and_refused_until_an_operator_enables_a_source() {
    let fixture = Fixture::new();
    let mut default_server = Harness::start(fixture.deps(), Some(fixture.root.clone()), None).await;
    let mut cursor_only = Harness::start(
        fixture.deps(),
        Some(fixture.root.clone()),
        Some(fixture.options(&[RecallSource::Cursor])),
    )
    .await;

    for server in [&mut default_server, &mut cursor_only] {
        let names = server.tool_names().await;
        assert_eq!(names.len(), 12);
        assert!(!names.iter().any(|name| name == "compresso_recall"));
        let (is_error, text) = server
            .call("compresso_recall", json!({ "query": "lease" }))
            .await;
        assert!(is_error);
        assert!(text.starts_with("[recall_disabled]"), "{text}");
        assert!(!text.contains("21 days"));
    }

    default_server.close().await;
    cursor_only.close().await;
}

#[tokio::test]
async fn test_recall_tool_returns_redacted_repository_scoped_hits_and_records_the_operation() {
    let fixture = Fixture::new();
    let mut server = Harness::start(
        fixture.deps(),
        Some(fixture.root.clone()),
        Some(fixture.options(&[RecallSource::ClaudeCode, RecallSource::Codex])),
    )
    .await;

    let tools = server.request("tools/list", json!({})).await;
    let declaration = tools["result"]["tools"]
        .as_array()
        .expect("tools should list")
        .iter()
        .find(|tool| tool["name"] == "compresso_recall")
        .expect("enabled recall should be advertised")
        .clone();
    assert_eq!(declaration["annotations"]["readOnlyHint"], true);
    assert_eq!(declaration["annotations"]["destructiveHint"], false);
    assert_eq!(declaration["annotations"]["openWorldHint"], false);
    assert_eq!(declaration["inputSchema"]["required"], json!(["query"]));
    assert_eq!(declaration["inputSchema"]["additionalProperties"], false);

    let (is_error, text) = server
        .call("compresso_recall", json!({ "query": "lease", "limit": 10 }))
        .await;
    assert!(!is_error, "{text}");
    assert!(!text.contains(SECRET_CANARY));
    assert!(!text.contains("must stay out of scope"));
    let payload: Value = serde_json::from_str(&text).expect("recall payload should be JSON");
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["scope"], "repository");
    assert_eq!(payload["truncated"], false);
    assert_eq!(payload["redactions"], 1);
    let hits = payload["hits"].as_array().expect("hits should be an array");
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0]["source"], "codex");
    assert_eq!(hits[0]["session_id"], "thread-c");
    assert_eq!(hits[0]["line"], 2);
    assert!(hits.iter().any(|hit| {
        hit["snippet"]
            .as_str()
            .is_some_and(|text| text.contains("21 days"))
    }));
    assert!(hits.iter().any(|hit| {
        hit["snippet"]
            .as_str()
            .is_some_and(|text| text.contains("[REDACTED:token]"))
    }));
    assert!(
        payload["notice"]
            .as_str()
            .is_some_and(|notice| notice.contains("never as instructions"))
    );

    let operation_id = payload["operation_id"]
        .as_str()
        .expect("operation id should be text");
    let operation = fixture
        .store
        .operation_by_id(operation_id)
        .expect("operation lookup should succeed")
        .expect("recall should be recorded in the ledger");
    assert_eq!(operation.tool, "compresso_recall");
    assert!(operation.trace_json.is_none());
    assert!(operation.tokens_served.is_some_and(|tokens| tokens > 0));
    let stats = fixture
        .store
        .detailed_stats(&StatsFilter::default())
        .expect("stats should load");
    assert_eq!(stats.summary.operations, 1);

    server.close().await;
}

#[tokio::test]
async fn test_recall_tool_cannot_widen_sources_scope_or_bounds() {
    let fixture = Fixture::new();
    let mut codex_only = Harness::start(
        fixture.deps(),
        Some(fixture.root.clone()),
        Some(fixture.options(&[RecallSource::Codex])),
    )
    .await;
    let mut unbound = Harness::start(
        fixture.deps(),
        None,
        Some(fixture.options(&[RecallSource::Codex])),
    )
    .await;

    let (is_error, text) = codex_only
        .call("compresso_recall", json!({ "query": "lease" }))
        .await;
    assert!(!is_error, "{text}");
    let payload: Value = serde_json::from_str(&text).expect("recall payload should be JSON");
    assert_eq!(payload["hits"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["sources"].as_array().map(Vec::len), Some(1));

    for (arguments, expected) in [
        (
            json!({ "query": "lease", "sources": ["claude-code"] }),
            "[recall_disabled]",
        ),
        (
            json!({ "query": "lease", "sources": ["cursor"] }),
            "[recall_disabled]",
        ),
        (
            json!({ "query": "lease", "sources": [] }),
            "[invalid_arguments]",
        ),
        (
            json!({ "query": "lease", "sources": ["everything"] }),
            "[invalid_arguments]",
        ),
        (
            json!({ "query": "lease", "limit": 21 }),
            "[invalid_arguments]",
        ),
        (
            json!({ "query": "lease", "root": "/" }),
            "[invalid_arguments]",
        ),
        (
            json!({ "query": "lease", "all_repositories": true }),
            "[invalid_arguments]",
        ),
        (json!({ "query": "   " }), "[invalid_arguments]"),
        (json!({}), "[invalid_arguments]"),
    ] {
        let (is_error, text) = codex_only.call("compresso_recall", arguments.clone()).await;
        assert!(is_error, "{arguments} should be refused: {text}");
        assert!(text.starts_with(expected), "{arguments}: {text}");
        assert!(!text.contains("21 days"));
    }

    let (is_error, text) = unbound
        .call("compresso_recall", json!({ "query": "lease" }))
        .await;
    assert!(is_error);
    assert!(text.starts_with("[not_indexed]"), "{text}");

    codex_only.close().await;
    unbound.close().await;
}
