//! `compresso_report` over the MCP boundary (E17-48, D113).

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io, serve_io_for_root};
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(10);

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Harness {
    async fn start(store: Store, root: Option<PathBuf>) -> Self {
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
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "compresso-report-test", "version": "1.0.0" }
                }),
            )
            .await;
        assert_eq!(response["result"]["serverInfo"]["name"], "compresso");
        harness
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }))
            .await;
        harness
    }

    /// Calls `name` and returns whether it failed and its text.
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
                .expect("text content")
                .to_owned(),
        )
    }

    async fn report(&mut self, arguments: Value) -> Value {
        let (is_error, text) = self.call("compresso_report", arguments).await;
        assert!(!is_error, "{text}");
        serde_json::from_str(&text).expect("report payload is JSON")
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
        serde_json::from_str(&line).expect("response should be JSON-RPC")
    }

    async fn send(&mut self, message: &Value) {
        let mut frame = serde_json::to_vec(message).expect("request serializes");
        frame.push(b'\n');
        timeout(IO_TIMEOUT, self.input.write_all(&frame))
            .await
            .expect("request write should not time out")
            .expect("request is written");
    }

    async fn close(mut self) {
        timeout(IO_TIMEOUT, self.input.shutdown())
            .await
            .expect("shutdown should not time out")
            .expect("input shuts down");
        drop(self.input);
        timeout(IO_TIMEOUT, self.server)
            .await
            .expect("server should exit on EOF")
            .expect("server task should not panic")
            .expect("server exits cleanly");
    }
}

struct Fixture {
    temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("repository root");
        let root = fs::canonicalize(root).expect("root canonicalizes");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
        store.approve_repo(&root).expect("repository approves");
        Self { temp, root, store }
    }

    fn reports(&self) -> PathBuf {
        self.temp.path().join("reports")
    }
}

#[tokio::test]
async fn test_bug_report_is_saved_locally_in_full_without_absolute_paths() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(fixture.store.clone(), Some(fixture.root.clone())).await;
    // A failed call is remembered by tool and code only.
    let (failed, _) = harness
        .call("compresso_read", json!({ "path": "a.rs", "bogus": 1 }))
        .await;
    assert!(failed);
    let root = fixture.root.display().to_string();
    let payload = harness
        .report(json!({
            "kind": "bug",
            "note": format!("grep broke in {root}/src after token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 and src/lib.rs")
        }))
        .await;
    assert_eq!(payload["kind"], "bug");
    assert_eq!(payload["sent"], false);
    let text = payload["text"].as_str().expect("report text");
    assert!(text.starts_with("Compresso bug report\n"), "{text}");
    assert!(
        text.contains("- compresso_read: invalid_arguments"),
        "{text}"
    );
    assert!(text.contains("src/lib.rs"), "{text}");
    assert!(!text.contains(&root), "{text}");
    assert!(
        !text.contains(&fixture.temp.path().display().to_string()),
        "{text}"
    );
    assert!(
        !text.contains("abcdefghijklmnopqrstuvwxyz0123456789"),
        "{text}"
    );
    let saved = PathBuf::from(payload["saved_to"].as_str().expect("saved path"));
    assert_eq!(saved.parent(), Some(fixture.reports().as_path()));
    assert_eq!(fs::read_to_string(&saved).expect("report file"), text);
    assert!(
        fs::read_dir(&fixture.root).expect("root").next().is_none(),
        "nothing is written into the repository"
    );
    assert!(
        payload["how_to_send"]
            .as_str()
            .expect("advice")
            .contains("sent this report nowhere")
    );
    harness.close().await;
}

#[tokio::test]
async fn test_status_update_and_share_write_no_file() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(fixture.store.clone(), Some(fixture.root.clone())).await;
    let status = harness.report(json!({ "kind": "status" })).await;
    let text = status["text"].as_str().expect("status text");
    assert!(
        text.starts_with(&format!("Compresso {}", env!("CARGO_PKG_VERSION"))),
        "{text}"
    );
    assert!(
        text.contains("server: full profile; recall off; edits off"),
        "{text}"
    );
    assert!(text.contains("repository: bound, not indexed"), "{text}");
    assert!(
        !text.contains(&fixture.root.display().to_string()),
        "{text}"
    );

    let update = harness.report(json!({ "kind": "update" })).await;
    let text = update["text"].as_str().expect("update text");
    assert!(text.contains("does not update itself"), "{text}");
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");

    let share = harness.report(json!({ "kind": "share" })).await;
    let text = share["text"].as_str().expect("share text");
    assert!(text.starts_with("Compresso "), "{text}");
    assert!(
        !text.contains(&fixture.root.display().to_string()),
        "{text}"
    );
    assert!(
        !fixture.reports().exists(),
        "only bug and feedback save a file"
    );
    harness.close().await;
}

#[tokio::test]
async fn test_report_arguments_are_checked() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(fixture.store.clone(), None).await;
    let (failed, text) = harness
        .call("compresso_report", json!({ "kind": "status", "note": "x" }))
        .await;
    assert!(failed && text.starts_with("[invalid_arguments]"), "{text}");
    let (failed, text) = harness
        .call("compresso_report", json!({ "kind": "upload" }))
        .await;
    assert!(failed && text.starts_with("[invalid_arguments]"), "{text}");
    let (failed, text) = harness
        .call(
            "compresso_report",
            json!({ "kind": "feedback", "note": "x".repeat(2_001) }),
        )
        .await;
    assert!(failed && text.starts_with("[invalid_arguments]"), "{text}");
    let status = harness.report(json!({ "kind": "status" })).await;
    assert!(
        status["text"]
            .as_str()
            .expect("text")
            .contains("repository: none bound"),
        "{status}"
    );
    harness.close().await;
}
