//! `compresso_report` over the MCP boundary (E17-48, D113).

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io, serve_io_for_root};
use compresso_store::{StatsFilter, Store};
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
    let shown = payload["saved_to"].as_str().expect("saved location");
    let saved = fs::read_dir(fixture.reports())
        .expect("reports beside the database")
        .map(|entry| entry.expect("entry").path())
        .collect::<Vec<_>>();
    assert_eq!(saved.len(), 1, "{saved:?}");
    let saved = saved.into_iter().next().expect("one report");
    assert!(
        shown.starts_with(
            saved
                .file_name()
                .and_then(|name| name.to_str())
                .expect("name")
        ),
        "{shown}"
    );
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
    let operations = fixture
        .store
        .detailed_stats(&StatsFilter {
            since: None,
            session_id: None,
            operation_limit: Some(10),
        })
        .expect("stats")
        .summary
        .operations;
    assert_eq!(operations, 0, "report calls are not written to the ledger");
}

/// A saved-report file name the cap counts.
fn report_name(index: usize) -> String {
    format!("bug-{index}-01ARZ3NDEKTSV4RRFFQ69G5FAV.txt")
}

/// The call's error text, which must start with `[code]`.
async fn failed_report(harness: &mut Harness, code: &str) -> String {
    let (failed, text) = harness
        .call("compresso_report", json!({ "kind": "bug", "note": "x" }))
        .await;
    assert!(failed, "{text}");
    assert!(text.starts_with(&format!("[{code}]")), "{text}");
    text
}

#[tokio::test]
async fn test_the_cap_is_exactly_one_hundred_matching_reports() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.reports()).expect("reports directory");
    for index in 0..99 {
        fs::write(fixture.reports().join(report_name(index)), "x").expect("old report");
    }
    // Files that are not reports do not count toward the cap.
    for index in 0..10 {
        fs::write(fixture.reports().join(format!("notes-{index}.txt")), "x").expect("other file");
    }
    let mut harness = Harness::start(fixture.store.clone(), None).await;
    let saved = harness
        .report(json!({ "kind": "feedback", "note": "hi" }))
        .await;
    assert!(
        saved["saved_to"].is_string(),
        "the 100th report is saved: {saved}"
    );
    let text = failed_report(&mut harness, "report_limit_reached").await;
    assert!(text.contains("100"), "{text}");
    assert!(text.contains("delete"), "{text}");
    assert!(
        !text.contains(&fixture.temp.path().display().to_string()),
        "{text}"
    );
    harness.close().await;
    assert_eq!(
        fs::read_dir(fixture.reports()).expect("dir").count(),
        110,
        "nothing deleted"
    );
}

#[tokio::test]
async fn test_saved_location_in_the_result_holds_no_absolute_path() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(fixture.store.clone(), None).await;
    let payload = harness.report(json!({ "kind": "bug" })).await;
    harness.close().await;
    let shown = payload["saved_to"].as_str().expect("saved location");
    assert!(!shown.starts_with('/') && !shown.contains(":\\"), "{shown}");
    let result = payload.to_string();
    assert!(
        !result.contains(&fixture.temp.path().display().to_string()),
        "{result}"
    );
    for name in ["HOME", "USER", "USERNAME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(name).and_then(|value| value.into_string().ok())
            && value.len() > 2
        {
            assert!(!result.contains(&value), "{name} appears in {result}");
        }
    }
    // Only the report itself is left: no temporary file.
    let names = fs::read_dir(fixture.reports())
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("UTF-8")
        })
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(
        names[0].starts_with("bug-") && names[0].ends_with(".txt"),
        "{names:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_reports_directory_and_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let mut harness = Harness::start(fixture.store.clone(), None).await;
    harness.report(json!({ "kind": "bug" })).await;
    harness.close().await;
    let mode =
        |path: &std::path::Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&fixture.reports()), 0o700);
    let file = fs::read_dir(fixture.reports())
        .expect("dir")
        .next()
        .expect("one")
        .expect("entry");
    assert_eq!(mode(&file.path()), 0o600);
}

#[cfg(unix)]
#[tokio::test]
async fn test_a_symlinked_reports_directory_is_refused() {
    let fixture = Fixture::new();
    let elsewhere = fixture.temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("target");
    std::os::unix::fs::symlink(&elsewhere, fixture.reports()).expect("symlink");
    let mut harness = Harness::start(fixture.store.clone(), None).await;
    failed_report(&mut harness, "report_directory_unusable").await;
    harness.close().await;
    assert_eq!(
        fs::read_dir(&elsewhere).expect("dir").count(),
        0,
        "link not followed"
    );
}

#[tokio::test]
async fn test_a_reports_path_that_is_a_file_is_refused() {
    let fixture = Fixture::new();
    fs::write(fixture.reports(), "not a directory").expect("file");
    let mut harness = Harness::start(fixture.store.clone(), None).await;
    failed_report(&mut harness, "report_directory_unusable").await;
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_an_unwritable_reports_directory_is_its_own_error() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::create_dir(fixture.reports()).expect("reports");
    fs::set_permissions(fixture.reports(), fs::Permissions::from_mode(0o500)).expect("chmod");
    let mut harness = Harness::start(fixture.store.clone(), None).await;
    failed_report(&mut harness, "report_directory_unwritable").await;
    harness.close().await;
    fs::set_permissions(fixture.reports(), fs::Permissions::from_mode(0o700)).expect("chmod back");
    assert_eq!(
        fs::read_dir(fixture.reports()).expect("dir").count(),
        0,
        "no partial file"
    );
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
