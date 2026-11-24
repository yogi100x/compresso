use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use compresso_atlas::Indexer;
use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{McpDeps, serve_io_for_root};
use compresso_store::{FileLanguage, FileRecord, Store};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const PROTOCOL_VERSION: &str = "2024-11-05";
const SECRET_CANARY: &str = "grep_secret_canary_5521";

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Harness {
    async fn start(store: Store, root: PathBuf) -> Self {
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
                    "clientInfo": { "name": "compresso-grep-test", "version": "1.0.0" }
                }),
            )
            .await;
        assert_eq!(response["result"]["serverInfo"]["name"], "compresso");
        harness
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }))
            .await;
        harness
    }

    async fn call(&mut self, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": "compresso_grep", "arguments": arguments }),
        )
        .await
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

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
    repo: i64,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        write_file(
            &root,
            "src/scanner.rs",
            "pub fn is_secret_path(path: &str) -> bool {\n    path.ends_with(\".env\")\n}\n",
        );
        write_file(
            &root,
            "src/queries.rs",
            "pub fn resolve(path: &str) -> bool {\n    is_secret_path(path)\n}\n\npub fn scan(path: &str) -> bool {\n    !is_secret_path(path)\n}\n",
        );
        for index in 0..12 {
            write_file(
                &root,
                &format!("src/steps/step_{index:02}.rs"),
                &format!(
                    "pub fn step_{index:02}(budget: &mut usize) {{\n{}}}\n",
                    "    *budget += RETRY_BUDGET;\n".repeat(6)
                ),
            );
        }
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
            .index(&root, |_| {})
            .expect("fixture should index");
        let root = fs::canonicalize(root).expect("root should canonicalize");
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

    /// Writes a file and records it in the index directly, as an index built
    /// under older rules could hold it.
    fn inject_indexed(&self, path: &str, source: &[u8]) {
        let destination = self.root.join(path);
        fs::create_dir_all(destination.parent().expect("parent")).expect("parent created");
        fs::write(&destination, source).expect("file written");
        self.store
            .replace_indexed_file(
                self.repo,
                &FileRecord {
                    path: path.to_owned(),
                    hash: blake3::hash(source).to_hex().to_string(),
                    language: Some(FileLanguage::Rust),
                    size_bytes: source.len() as u64,
                    token_estimate: 1,
                    indexed_at: 0,
                },
                &[],
                &[],
            )
            .expect("injected file indexes");
    }

    async fn harness(self) -> (Harness, TempDir, PathBuf) {
        let root = self.root.clone();
        let harness = Harness::start(self.store, self.root).await;
        (harness, self._temp, root)
    }
}

fn successful_json(response: &Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "protocol error: {response}"
    );
    assert_ne!(
        response["result"]["isError"], true,
        "tool error: {response}"
    );
    serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("tool content should be text"),
    )
    .expect("tool content should be JSON")
}

fn assert_tool_error(response: &Value, code: &str) {
    assert_eq!(
        response["result"]["isError"], true,
        "expected error: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error should contain text");
    assert!(
        text.starts_with(&format!("[{code}]")),
        "unexpected tool error: {response}"
    );
}

fn file_paths(result: &Value) -> Vec<String> {
    result["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|file| file["path"].as_str().expect("path").to_owned())
        .collect()
}

#[tokio::test]
async fn test_grep_tool_returns_use_sites_for_several_patterns_in_one_call() {
    let (mut harness, _temp, _root) = Fixture::new().harness().await;

    let payload = successful_json(
        &harness
            .call(json!({ "patterns": ["is_secret_path", "fn scan"] }))
            .await,
    );

    assert_eq!(payload["mode"], "literal");
    assert_eq!(payload["complete"], true);
    assert_eq!(payload["truncated"], false);
    assert!(
        payload["operation_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    let results = payload["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["pattern"], "is_secret_path");
    assert_eq!(results[0]["matches_total"], 3);
    assert_eq!(
        file_paths(&results[0]),
        ["src/scanner.rs", "src/queries.rs"]
    );
    let uses = results[0]["files"][1]["lines"].as_array().expect("lines");
    assert_eq!(uses[0], "2 [resolve] is_secret_path(path)");
    assert_eq!(uses[1], "6 [scan] !is_secret_path(path)");
    assert_eq!(
        results[0]["files"][0]["lines"][0],
        "1 [def is_secret_path] pub fn is_secret_path(path: &str) -> bool {"
    );
    assert_eq!(results[1]["matches_total"], 1);
    assert!(
        payload["tokens_used"]
            .as_u64()
            .is_some_and(|tokens| tokens <= 1_500),
        "{payload}"
    );
    let single = successful_json(&harness.call(json!({ "pattern": "fn scan" })).await);
    assert_eq!(single["results"][0]["matches_total"], 1);
    harness.close().await;
}

#[tokio::test]
async fn test_grep_tool_reports_totals_and_truncation_under_caps_and_budget() {
    let (mut harness, _temp, _root) = Fixture::new().harness().await;

    let capped = successful_json(
        &harness
            .call(json!({ "pattern": "RETRY_BUDGET", "max_files": 3, "max_matches_per_file": 2 }))
            .await,
    );
    let result = &capped["results"][0];
    assert_eq!(result["matches_total"], 72);
    assert_eq!(result["files_total"], 12);
    assert_eq!(result["files"].as_array().map(Vec::len), Some(3));
    assert_eq!(result["lines_listed"], 6);
    assert_eq!(result["files"][0]["matches"], 6);
    assert_eq!(capped["complete"], false);
    assert_eq!(capped["truncated"], true);
    assert!(
        capped["note"]
            .as_str()
            .is_some_and(|note| note.contains("66 matching lines")),
        "{capped}"
    );

    let eight = (0..8)
        .map(|index| format!("RETRY_BUDGET{}", "x".repeat(index)))
        .collect::<Vec<_>>();
    let long_eight = (0..8)
        .map(|index| format!("{index}{}", "y".repeat(511)))
        .collect::<Vec<_>>();
    for (patterns, max_tokens) in [(&eight, 300_u64), (&eight, 1_500), (&long_eight, 1_500)] {
        let response = harness
            .call(json!({ "patterns": patterns, "max_tokens": max_tokens }))
            .await;
        if response["result"]["isError"] == true {
            assert_tool_error(&response, "invalid_arguments");
            continue;
        }
        let payload = successful_json(&response);
        let tokens = payload["tokens_used"].as_u64().expect("tokens used");
        assert!(
            tokens <= max_tokens,
            "{tokens} > {max_tokens} for 8 patterns"
        );
    }
    let rejected = harness
        .call(json!({ "patterns": long_eight, "max_tokens": 300 }))
        .await;
    assert_tool_error(&rejected, "invalid_arguments");

    for max_tokens in [300, 400, 700] {
        let budgeted = successful_json(
            &harness
                .call(json!({
                    "pattern": "RETRY_BUDGET",
                    "max_files": 100,
                    "max_matches_per_file": 50,
                    "max_tokens": max_tokens
                }))
                .await,
        );
        let tokens = budgeted["tokens_used"].as_u64().expect("tokens used");
        assert!(tokens <= max_tokens, "{tokens} > {max_tokens}");
        assert_eq!(budgeted["truncated"], true);
        assert_eq!(budgeted["results"][0]["matches_total"], 72);
    }
    harness.close().await;
}

#[tokio::test]
async fn test_grep_tool_never_returns_secret_or_oversize_file_content() {
    let fixture = Fixture::new();
    fixture.inject_indexed(
        "config/.env.local.rs",
        format!("const KEY: &str = \"{SECRET_CANARY}\";\n").as_bytes(),
    );
    let mut oversize = format!("{SECRET_CANARY}\n").into_bytes();
    oversize.resize(16 * 1024 * 1024 + 1, b'x');
    fixture.inject_indexed("generated/huge.rs", &oversize);
    let (mut harness, _temp, _root) = fixture.harness().await;

    for arguments in [
        // Partial patterns, so the echoed pattern is not the canary itself.
        json!({ "pattern": "secret_canary" }),
        json!({ "pattern": "grep_secret.*", "regex": true, "path": "**/.env*" }),
        json!({ "pattern": "KEY", "path": "config" }),
    ] {
        let response = harness.call(arguments).await;
        let rendered = response.to_string();
        assert!(!rendered.contains(SECRET_CANARY), "{rendered}");
        assert!(!rendered.contains(".env.local"), "{rendered}");
        let payload = successful_json(&response);
        assert!(
            payload["results"][0]["files"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );
    }
    let payload = successful_json(&harness.call(json!({ "pattern": "secret_canary" })).await);
    assert_eq!(payload["scan"]["files_skipped_oversize"], 1);
    harness.close().await;
}

#[tokio::test]
async fn test_grep_tool_rejects_invalid_patterns_and_arguments() {
    let (mut harness, _temp, root) = Fixture::new().harness().await;

    for arguments in [
        json!({ "pattern": "(", "regex": true }),
        json!({ "pattern": "(?<=a)b", "regex": true }),
        json!({ "patterns": [] }),
        json!({ "patterns": ["a", "b", "c", "d", "e", "f", "g", "h", "i"] }),
        json!({ "pattern": "x".repeat(513) }),
        json!({ "pattern": "a\nb" }),
        json!({ "pattern": "x", "path": "../outside" }),
    ] {
        assert_tool_error(&harness.call(arguments).await, "invalid_arguments");
    }
    for arguments in [
        json!({}),
        json!({ "pattern": "x", "patterns": ["y"] }),
        json!({ "patterns": [1] }),
        json!({ "pattern": "x", "max_files": 0 }),
        json!({ "pattern": "x", "max_tokens": 99 }),
        json!({ "pattern": "x", "extra": true }),
        json!({ "pattern": "x", "root": "relative" }),
    ] {
        assert_tool_error(&harness.call(arguments).await, "invalid_arguments");
    }
    let other = tempfile::tempdir().expect("temporary directory");
    assert_tool_error(
        &harness
            .call(json!({ "pattern": "x", "root": other.path() }))
            .await,
        "outside_root",
    );
    assert!(root.is_dir());
    harness.close().await;
}

#[tokio::test]
async fn test_grep_tool_case_flag_and_path_filter() {
    let (mut harness, _temp, _root) = Fixture::new().harness().await;

    let sensitive = successful_json(&harness.call(json!({ "pattern": "retry_budget" })).await);
    let insensitive = successful_json(
        &harness
            .call(json!({ "pattern": "retry_budget", "case_insensitive": true }))
            .await,
    );
    assert_eq!(sensitive["results"][0]["matches_total"], 0);
    assert_eq!(insensitive["results"][0]["matches_total"], 72);

    let prefix = successful_json(
        &harness
            .call(json!({ "pattern": "is_secret_path", "path": "src/queries.rs" }))
            .await,
    );
    assert_eq!(file_paths(&prefix["results"][0]), ["src/queries.rs"]);
    let glob = successful_json(
        &harness
            .call(json!({ "pattern": "RETRY_BUDGET", "path": "src/steps/step_0?.rs" }))
            .await,
    );
    assert_eq!(glob["results"][0]["files_total"], 10);
    assert_eq!(glob["scan"]["files_in_scope"], 10);
    harness.close().await;
}

#[tokio::test]
async fn test_grep_tool_stays_linear_on_a_long_line_and_a_pathological_pattern() {
    let fixture = Fixture::new();
    let long_line = "aaaa needle ".repeat(80_000);
    fixture.inject_indexed("src/long.rs", format!("// {long_line}\n").as_bytes());
    let (mut harness, _temp, _root) = fixture.harness().await;

    let started = Instant::now();
    let literal = successful_json(&harness.call(json!({ "pattern": "needle" })).await);
    let pathological = successful_json(
        &harness
            .call(json!({ "patterns": ["(a+)+$", "(a|aa)*c"], "regex": true }))
            .await,
    );
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    assert_eq!(literal["results"][0]["matches_total"], 1);
    let line = literal["results"][0]["files"][0]["lines"][0]
        .as_str()
        .expect("line");
    assert!(
        line.len() < 220,
        "a long line is cut to a snippet: {}",
        line.len()
    );
    assert_eq!(pathological["results"].as_array().map(Vec::len), Some(2));
    harness.close().await;
}

#[tokio::test]
async fn test_grep_tool_marks_changed_files_and_the_stale_index() {
    let fixture = Fixture::new();
    let root = fixture.root.clone();
    let (mut harness, _temp, _) = fixture.harness().await;
    fs::write(
        root.join("src/queries.rs"),
        "// edited\npub fn resolve(path: &str) -> bool {\n    is_secret_path(path)\n}\n",
    )
    .expect("file rewritten");

    let payload = successful_json(&harness.call(json!({ "pattern": "is_secret_path" })).await);

    assert_eq!(payload["stale"], true);
    assert!(payload["reindex_hint"].is_string());
    assert_eq!(payload["scan"]["stale_files"], 1);
    let changed = payload["results"][0]["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|file| file["path"] == "src/queries.rs")
        .expect("changed file still searched");
    assert_eq!(changed["stale"], true);
    assert_eq!(changed["lines"][0], "3 is_secret_path(path)");
    harness.close().await;
}

fn write_file(root: &Path, path: &str, source: &str) {
    let destination = root.join(path);
    fs::create_dir_all(destination.parent().expect("parent")).expect("parent created");
    fs::write(destination, source).expect("fixture written");
}

#[tokio::test]
async fn test_grep_tool_context_lines_surround_matches_within_the_token_cap() {
    let (mut harness, _temp, _root) = Fixture::new().harness().await;

    let plain = successful_json(&harness.call(json!({ "pattern": "is_secret_path" })).await);
    let with_context = successful_json(
        &harness
            .call(json!({ "pattern": "is_secret_path", "context_lines": 1 }))
            .await,
    );
    assert_eq!(
        with_context["results"][0]["matches_total"],
        plain["results"][0]["matches_total"]
    );
    let lines = with_context["results"][0]["files"][1]["lines"]
        .as_array()
        .expect("lines");
    assert_eq!(
        lines,
        &[
            json!("1- pub fn resolve(path: &str) -> bool {"),
            json!("2 [resolve] is_secret_path(path)"),
            json!("3- }"),
            json!("5- pub fn scan(path: &str) -> bool {"),
            json!("6 [scan] !is_secret_path(path)"),
            json!("7- }"),
        ]
    );
    assert_eq!(with_context["results"][0]["lines_listed"], 3);
    assert_eq!(with_context["complete"], true);

    // Context is counted inside max_tokens and cut with its match.
    let capped = successful_json(
        &harness
            .call(json!({ "pattern": "RETRY_BUDGET", "context_lines": 10, "max_tokens": 300 }))
            .await,
    );
    assert!(
        capped["tokens_used"]
            .as_u64()
            .is_some_and(|tokens| tokens <= 300)
    );
    assert_eq!(capped["truncated"], true);

    // A decimal-digit string is read as its integer since E17-54 (D119);
    // other strings are still refused.
    for bad in [json!(11), json!(-1), json!("11"), json!("2.0"), json!(" 2")] {
        assert_tool_error(
            &harness
                .call(json!({ "pattern": "scan", "context_lines": bad }))
                .await,
            "invalid_arguments",
        );
    }
    harness.close().await;
}

/// E17-49 (D114): a path filter that leaves matches out adds one bounded
/// note naming where the pattern also matches; without a filter, or when
/// nothing matches outside it, there is none.
#[tokio::test]
async fn test_grep_tool_names_files_outside_the_path_filter_that_also_match() {
    let (mut harness, _temp, _root) = Fixture::new().harness().await;
    let payload = successful_json(
        &harness
            .call(json!({ "pattern": "is_secret_path", "path": "src/scanner.rs" }))
            .await,
    );
    assert_eq!(
        payload["outside_matches"],
        json!({
            "files_total": 1,
            "matches_total": 2,
            "files": [{ "path": "src/queries.rs", "matches": 2 }]
        }),
        "{payload}"
    );
    for arguments in [
        json!({ "pattern": "is_secret_path" }),
        json!({ "pattern": "is_secret_path", "path": "src" }),
        json!({ "pattern": "fn scan", "path": "src/scanner.rs" }),
        json!({ "pattern": "ends_with", "path": "src/scanner.rs" }),
    ] {
        let payload = successful_json(&harness.call(arguments.clone()).await);
        assert!(
            payload.get("outside_matches").is_none(),
            "{arguments}: {payload}"
        );
    }
    harness.close().await;
}
