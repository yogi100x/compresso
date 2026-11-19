//! E17-31: tool result forms. The full profile's JSON results are pinned
//! byte for byte; the lean profile's compact text form is pinned by goldens.
//!
//! Set `COMPRESSO_UPDATE_GOLDEN=1` to rewrite the golden files.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_atlas::Indexer;
use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{
    McpDeps, McpRecallOptions, McpResultFormat, McpServeOptions, McpToolProfile,
    serve_io_with_options,
};
use compresso_recall::RecallSource;
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(20);
const SECRET_CANARY: &str = "result_format_secret_canary_7342";

struct Fixture {
    temp: TempDir,
    root: PathBuf,
    store: Store,
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("parent created");
    fs::write(path, contents).expect("file written");
}

const LIB_RS: &str = "//! Fixture crate for result forms.\npub mod session;\n\n/// A constant whose source holds quotes, a tab, and backslashes.\npub const GREETING: &str = \"say \\\"hi\\\"\\t\\\\ok\";\n\npub fn start_session(name: &str) -> String {\n\tlet session = session::Session::open(name);\n\tsession.describe()\n}\n\npub fn restart(name: &str) -> String {\n    start_session(name)\n}\n";

const SESSION_RS: &str = "pub struct Session {\n    name: String,\n}\n\nimpl Session {\n    pub fn open(name: &str) -> Self {\n        Self { name: name.to_owned() }\n    }\n\n    pub fn describe(&self) -> String {\n        format!(\"session \\\"{}\\\" C:\\\\tmp\", self.name)\n    }\n\n    pub fn close(&self) {}\n}\n\npub fn open(name: &str) -> Session {\n    Session::open(name)\n}\n\nimpl std::fmt::Display for Session {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \"{}\", self.name)\n    }\n}\n\nimpl std::fmt::Debug for Session {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \"Session({})\", self.name)\n    }\n}\n";

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("work/repo");
        write_file(&root, "src/lib.rs", LIB_RS);
        write_file(&root, "src/session.rs", SESSION_RS);
        let mut long = String::new();
        for index in 0..400 {
            long.push_str(&format!(
                "pub fn step_{index:03}(budget: &mut usize) {{ *budget += {index}; }}\n"
            ));
        }
        long.push_str("pub fn checksum_table() -> &'static [u32] {\n    &[\n");
        for index in 0..120 {
            long.push_str(&format!("        {},\n", index * 7919));
        }
        long.push_str("    ]\n}\n");
        write_file(&root, "src/steps.rs", &long);
        write_file(
            &root,
            "README.md",
            "# Fixture\n\nSession lifecycle notes: open, describe, close.\n",
        );
        write_file(&root, ".env", &format!("TOKEN={SECRET_CANARY}\n"));
        let store = Store::open(&temp.path().join("compresso.db")).expect("store");
        Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
            .index(&root, |_| {})
            .expect("fixture indexes");
        let root = fs::canonicalize(root).expect("root canonicalizes");
        // Created after indexing: gates must refuse them at read time.
        let mut big = b"x".to_vec();
        big.resize(17 * 1024 * 1024, b'x');
        fs::write(root.join("big.txt"), big).expect("oversize file");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join(".env"), root.join("link.txt")).expect("link");
        let fixture = Self { temp, root, store };
        fixture.write_transcript();
        fixture
    }

    fn home(&self) -> PathBuf {
        self.temp.path().join("home")
    }

    fn write_transcript(&self) {
        let path = self.home().join(".claude/projects/-work-repo/aaaa.jsonl");
        fs::create_dir_all(path.parent().expect("parent")).expect("transcript dir");
        let line = json!({
            "type": "assistant",
            "cwd": self.root,
            "sessionId": "session-a",
            "timestamp": "2026-05-01T10:00:00.000Z",
            "message": { "role": "assistant", "content": [{ "type": "text",
                "text": "Decision: keep \"session\" names short; see C:\\tmp." }] }
        });
        fs::write(path, format!("{line}\n")).expect("transcript written");
    }

    fn options(&self, tools: McpToolProfile) -> McpServeOptions {
        McpServeOptions {
            root: Some(self.root.clone()),
            recall: Some(McpRecallOptions {
                home_dir: self.home(),
                sources: vec![RecallSource::ClaudeCode],
            }),
            tools,
            ..McpServeOptions::default()
        }
    }
}

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
}

impl Harness {
    async fn start(fixture: &Fixture, options: McpServeOptions) -> Self {
        let config = Config::default();
        let deps = McpDeps {
            store: fixture.store.clone(),
            engine: Engine::new(
                config.compression.mode,
                counter_for(TokenProvider::OpenAiO200k),
            ),
            config,
        };
        let (client, server) = tokio::io::duplex(1024 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let server = tokio::spawn(serve_io_with_options(
            deps,
            server_read,
            server_write,
            options,
        ));
        let (client_read, client_write) = tokio::io::split(client);
        let mut harness = Self {
            input: client_write,
            output: BufReader::new(client_read),
            server,
            next_id: 1,
        };
        harness
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "result-format-test", "version": "1.0.0" }
                }),
            )
            .await;
        harness
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        harness
    }

    /// The tool's text content and whether it is an error result.
    async fn call(&mut self, name: &str, arguments: &Value) -> (String, bool) {
        let response = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        assert!(
            response.get("error").is_none(),
            "protocol error: {response}"
        );
        let content = response["result"]["content"]
            .as_array()
            .expect("content array");
        assert_eq!(content.len(), 1, "one text block: {response}");
        (
            content[0]["text"].as_str().expect("text block").to_owned(),
            response["result"]["isError"] == true,
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
            .expect("response in time")
            .expect("response readable");
        assert!(bytes > 0, "server closed early");
        serde_json::from_str(&line).expect("valid JSON-RPC")
    }

    async fn send(&mut self, message: &Value) {
        let mut frame = serde_json::to_vec(message).expect("serializes");
        frame.push(b'\n');
        timeout(IO_TIMEOUT, self.input.write_all(&frame))
            .await
            .expect("write in time")
            .expect("written");
    }

    async fn close(mut self) {
        timeout(IO_TIMEOUT, self.input.shutdown())
            .await
            .expect("shutdown in time")
            .expect("shut down");
        drop(self.input);
        timeout(IO_TIMEOUT, self.server)
            .await
            .expect("server exits")
            .expect("no panic")
            .expect("clean exit");
    }
}

/// Every lean-served tool in its success, truncation, ambiguity, empty, and
/// per-item error states.
fn golden_calls() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("repo_map", "compresso_repo_map", json!({})),
        (
            "repo_map_cards",
            "compresso_repo_map",
            json!({ "cards": true, "max_tokens": 600 }),
        ),
        ("search", "compresso_search", json!({ "query": "Session" })),
        (
            "search_truncated",
            "compresso_search",
            json!({ "query": "step", "limit": 40, "max_tokens": 100 }),
        ),
        (
            "search_none",
            "compresso_search",
            json!({ "query": "zzzqqq" }),
        ),
        (
            "search_path_not_found",
            "compresso_search",
            json!({ "query": "open", "path": "nowhere" }),
        ),
        (
            "grep",
            "compresso_grep",
            json!({ "patterns": ["Session::open", "describe"] }),
        ),
        (
            "grep_context",
            "compresso_grep",
            json!({ "pattern": "GREETING", "context_lines": 1 }),
        ),
        (
            "grep_truncated",
            "compresso_grep",
            json!({ "pattern": "budget", "max_files": 1, "max_matches_per_file": 3 }),
        ),
        ("read", "compresso_read", json!({ "path": "src/lib.rs" })),
        (
            "read_range_clamped",
            "compresso_read",
            json!({ "path": "src/session.rs", "start_line": 15, "end_line": 900 }),
        ),
        (
            "read_truncated",
            "compresso_read",
            json!({ "path": "src/steps.rs", "max_tokens": 200 }),
        ),
        (
            "read_gate",
            "compresso_read",
            json!({ "path": "src/steps.rs", "gate": true, "max_tokens": 300 }),
        ),
        (
            "read_ranges",
            "compresso_read",
            json!({ "ranges": [
            { "path": "src/lib.rs", "start_line": 4, "end_line": 5 },
            { "path": ".env" },
            { "path": "big.txt" },
            { "path": "link.txt" },
            { "path": "src/session.rs", "start_line": 17 }
        ] }),
        ),
        (
            "outline",
            "compresso_outline",
            json!({ "path": "src/session.rs" }),
        ),
        (
            "outline_truncated",
            "compresso_outline",
            json!({ "path": "src/steps.rs", "max_tokens": 150 }),
        ),
        (
            "unfold",
            "compresso_unfold",
            json!({ "path": "src/session.rs", "qualified_name": "Session::describe" }),
        ),
        (
            "unfold_raw",
            "compresso_unfold",
            json!({ "path": "src/lib.rs", "qualified_name": "GREETING" }),
        ),
        (
            "unfold_truncated",
            "compresso_unfold",
            json!({ "path": "src/steps.rs", "qualified_name": "checksum_table", "max_tokens": 100 }),
        ),
        (
            "unfold_ambiguous",
            "compresso_unfold",
            json!({ "path": "src/session.rs", "qualified_name": "Session::fmt" }),
        ),
        (
            "unfold_not_found",
            "compresso_unfold",
            json!({ "path": "src/session.rs", "qualified_name": "Session::missing" }),
        ),
        (
            "context",
            "compresso_context",
            json!({ "task": "how does a session open and describe itself" }),
        ),
        (
            "context_no_match",
            "compresso_context",
            json!({ "task": "zzzqqq wwwvvv" }),
        ),
        (
            "trace_callers",
            "compresso_trace",
            json!({ "to": "start_session" }),
        ),
        (
            "trace_callees",
            "compresso_trace",
            json!({ "from": "start_session" }),
        ),
        (
            "trace_path",
            "compresso_trace",
            json!({ "from": "restart", "to": "Session::open" }),
        ),
        (
            "trace_ambiguous",
            "compresso_trace",
            json!({ "to": "open" }),
        ),
        (
            "trace_not_found",
            "compresso_trace",
            json!({ "to": "missing_symbol" }),
        ),
        (
            "recall",
            "compresso_recall",
            json!({ "query": "session names" }),
        ),
        ("error_secret", "compresso_read", json!({ "path": ".env" })),
        ("error_size", "compresso_read", json!({ "path": "big.txt" })),
        (
            "error_symlink",
            "compresso_read",
            json!({ "path": "link.txt" }),
        ),
        (
            "error_arguments",
            "compresso_grep",
            json!({ "pattern": "(" , "regex": true }),
        ),
    ]
}

/// Replaces the per-call operation id and the temporary root with stable
/// placeholders. Grep and context count their whole serialized payload,
/// operation id included, into `tokens_used`, so that one number varies with
/// the random id and is masked for those two tools only.
fn mask(tool: &str, text: &str, root: &Path) -> String {
    // The root reaches a result raw or JSON-escaped, and on Windows with the
    // verbatim `\\?\` prefix that canonicalization adds.
    let raw = root.display().to_string();
    let bare = raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_owned();
    let verbatim = format!(r"\\?\{bare}");
    let mut masked = text.to_owned();
    for form in [verbatim, bare] {
        masked = masked.replace(&form.replace('\\', "\\\\"), "<root>");
        masked = masked.replace(&form, "<root>");
    }
    // Ranking scores differ in their last digits between CPU architectures.
    masked = round_scores(&masked);
    if let Ok(value) = serde_json::from_str::<Value>(text)
        && let Some(id) = value["operation_id"].as_str()
        && !id.is_empty()
    {
        masked = masked.replace(id, "<operation_id>");
        // A search result's token count follows the exact digits of its
        // scores, which differ between CPU architectures.
        if matches!(
            tool,
            "compresso_grep" | "compresso_context" | "compresso_search"
        ) {
            masked = mask_number(&masked, "\"tokens_used\":");
        }
    }
    masked
}

/// Rewrites every `"score":<float>` to six decimal places.
fn round_scores(text: &str) -> String {
    const KEY: &str = "\"score\":";
    let mut rounded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(KEY) {
        let start = index + KEY.len();
        let length = rest[start..]
            .bytes()
            .take_while(|byte| {
                byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
            })
            .count();
        rounded.push_str(&rest[..start]);
        match rest[start..start + length].parse::<f64>() {
            Ok(score) => rounded.push_str(&format!("{score:.6}")),
            Err(_) => rounded.push_str(&rest[start..start + length]),
        }
        rest = &rest[start + length..];
    }
    rounded.push_str(rest);
    rounded
}

fn mask_number(text: &str, key: &str) -> String {
    let Some(start) = text.find(key).map(|index| index + key.len()) else {
        return text.to_owned();
    };
    let digits = text[start..].bytes().take_while(u8::is_ascii_digit).count();
    format!("{}<n>{}", &text[..start], &text[start + digits..])
}

/// Every golden call's label, tool, result text, and error flag.
async fn results(
    fixture: &Fixture,
    options: McpServeOptions,
) -> Vec<(&'static str, &'static str, String, bool)> {
    let mut harness = Harness::start(fixture, options).await;
    let mut results = Vec::new();
    for (label, tool, arguments) in golden_calls() {
        let (text, is_error) = harness.call(tool, &arguments).await;
        assert!(
            !text.contains(SECRET_CANARY),
            "{label}: secret content reached the client"
        );
        results.push((label, tool, text, is_error));
    }
    harness.close().await;
    results
}

async fn transcript(fixture: &Fixture, options: McpServeOptions) -> String {
    results(fixture, options)
        .await
        .into_iter()
        .map(|(label, tool, text, is_error)| {
            format!(
                "=== {label}{}\n{}\n",
                if is_error { " (error)" } else { "" },
                mask(tool, &text, &fixture.root)
            )
        })
        .collect()
}

/// Whether this build's `serde_json` keeps insertion order. Cargo feature
/// unification enables `preserve_order` in a workspace build (and in the
/// shipped binary) but not in a build of this crate alone, so the JSON golden
/// exists once per key order and each build is checked byte for byte.
fn json_keeps_insertion_order() -> bool {
    let value: Value = serde_json::from_str(r#"{"b":1,"a":2}"#).expect("valid JSON");
    let serialized = serde_json::to_string(&value).expect("serializes");
    serialized.starts_with(r#"{"b""#)
}

fn check_golden(name: &str, actual: &str) {
    let name = match name.strip_suffix(".txt") {
        Some(stem) if stem.ends_with("_json") && json_keeps_insertion_order() => {
            format!("{stem}_insertion_order.txt")
        }
        _ => name.to_owned(),
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(&name);
    if std::env::var_os("COMPRESSO_UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().expect("golden dir")).expect("golden dir created");
        fs::write(&path, actual).expect("golden written");
        return;
    }
    let expected = fs::read_to_string(&path).expect("golden file exists");
    if expected != actual {
        let first = expected
            .lines()
            .zip(actual.lines())
            .position(|(left, right)| left != right)
            .unwrap_or(0);
        panic!(
            "{name} differs from its golden at line {}:\nexpected: {:?}\nactual:   {:?}",
            first + 1,
            expected.lines().nth(first),
            actual.lines().nth(first)
        );
    }
}

#[tokio::test]
async fn test_full_profile_results_are_byte_for_byte_unchanged() {
    let fixture = Fixture::new();
    let actual = transcript(&fixture, fixture.options(McpToolProfile::Full)).await;
    check_golden("full_profile_json.txt", &actual);
}

fn options_with(
    fixture: &Fixture,
    tools: McpToolProfile,
    results: McpResultFormat,
) -> McpServeOptions {
    McpServeOptions {
        results: Some(results),
        ..fixture.options(tools)
    }
}

#[tokio::test]
async fn test_lean_profile_text_results_match_their_goldens() {
    let fixture = Fixture::new();
    let actual = transcript(&fixture, fixture.options(McpToolProfile::Lean)).await;
    check_golden("lean_profile_text.txt", &actual);
}

#[tokio::test]
async fn test_each_profile_serves_the_other_form_when_the_operator_asks() {
    let fixture = Fixture::new();
    let lean_json = transcript(
        &fixture,
        options_with(&fixture, McpToolProfile::Lean, McpResultFormat::Json),
    )
    .await;
    check_golden("full_profile_json.txt", &lean_json);
    let full_text = transcript(
        &fixture,
        options_with(&fixture, McpToolProfile::Full, McpResultFormat::Text),
    )
    .await;
    check_golden("lean_profile_text.txt", &full_text);
}

#[tokio::test]
async fn test_errors_and_read_gates_are_identical_in_both_forms() {
    let fixture = Fixture::new();
    let json = results(&fixture, fixture.options(McpToolProfile::Full)).await;
    let text = results(&fixture, fixture.options(McpToolProfile::Lean)).await;
    for ((label, _, json, json_error), (_, _, text, text_error)) in json.iter().zip(&text) {
        assert_eq!(json_error, text_error, "{label}");
        if *json_error {
            assert_eq!(json, text, "{label}: errors keep one shape");
        }
    }
    let error = |label: &str| {
        text.iter()
            .find(|(name, ..)| *name == label)
            .map(|(_, _, text, _)| text.clone())
            .expect("golden call")
    };
    assert_eq!(
        error("error_secret"),
        "[not_found] The requested file was not found on disk."
    );
    assert!(error("error_size").starts_with("[file_too_large]"));
    assert_eq!(
        error("error_symlink"),
        "[not_found] The requested file was not found on disk."
    );
    // The same gates inside a multi-range read fail only their own range.
    let ranges = error("read_ranges");
    assert!(ranges.contains("@@ .env [not_found] The requested file was not found on disk.\n"));
    assert!(ranges.contains("@@ big.txt [file_too_large] "));
    assert!(ranges.contains("@@ link.txt [not_found] "));
    assert!(ranges.starts_with(
        "@@ src/lib.rs:4-5 of 14\n/// A constant whose source holds quotes, a tab, and backslashes.\n"
    ));
    assert!(
        ranges.contains("@@ src/session.rs:17-31 of 31\npub fn open(name: &str) -> Session {\n")
    );
}

#[tokio::test]
async fn test_text_form_carries_source_verbatim_without_json_escaping() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    let (text, _) = harness
        .call("compresso_read", &json!({ "path": "src/lib.rs" }))
        .await;
    assert_eq!(text, format!("@@ src/lib.rs:1-14 of 14\n{LIB_RS}"));
    let (text, _) = harness
        .call(
            "compresso_unfold",
            &json!({ "path": "src/session.rs", "qualified_name": "Session::describe" }),
        )
        .await;
    assert!(
        text.contains("        format!(\"session \\\"{}\\\" C:\\\\tmp\", self.name)\n"),
        "{text}"
    );
    let (text, _) = harness
        .call("compresso_grep", &json!({ "pattern": "GREETING" }))
        .await;
    assert!(
        text.contains("5 [def GREETING] pub const GREETING: &str = \"say \\\"hi\\\"\\t\\\\ok\";\n"),
        "{text}"
    );
    harness.close().await;
}

#[tokio::test]
async fn test_text_form_never_exceeds_max_tokens() {
    let fixture = Fixture::new();
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    let mut cases = Vec::new();
    for max_tokens in [100, 130, 200, 400, 900] {
        cases.extend([
            ("compresso_read", json!({ "path": "src/steps.rs", "max_tokens": max_tokens })),
            ("compresso_read", json!({ "ranges": [
                { "path": "src/steps.rs", "start_line": 1, "end_line": 300 },
                { "path": "src/session.rs" },
                { "path": ".env" }
            ], "max_tokens": max_tokens })),
            ("compresso_outline", json!({ "path": "src/steps.rs", "max_tokens": max_tokens })),
            ("compresso_unfold", json!({ "path": "src/steps.rs", "qualified_name": "checksum_table", "max_tokens": max_tokens })),
            ("compresso_search", json!({ "query": "step", "limit": 100, "max_tokens": max_tokens })),
            ("compresso_trace", json!({ "to": "start_session", "max_tokens": max_tokens })),
            ("compresso_repo_map", json!({ "cards": true, "max_tokens": max_tokens })),
            ("compresso_context", json!({ "task": "how does a session open and describe itself", "max_tokens": max_tokens })),
        ]);
        if max_tokens >= 300 {
            cases.push(("compresso_grep", json!({ "patterns": ["budget", "step_"], "max_matches_per_file": 50, "context_lines": 2, "max_tokens": max_tokens })));
        }
    }
    for (tool, arguments) in cases {
        let (text, is_error) = harness.call(tool, &arguments).await;
        assert!(!is_error, "{tool} {arguments}: {text}");
        let max_tokens = arguments["max_tokens"].as_u64().expect("budget") as usize;
        let tokens = counter.count(&text);
        assert!(
            tokens <= max_tokens,
            "{tool} {arguments}: {tokens} tokens over {max_tokens}:\n{text}"
        );
    }
    harness.close().await;
}

#[tokio::test]
async fn test_text_form_states_truncation_and_continuation() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    let (text, _) = harness
        .call(
            "compresso_read",
            &json!({ "path": "src/steps.rs", "max_tokens": 200 }),
        )
        .await;
    let last = text.lines().last().expect("trailing line");
    assert!(last.starts_with("[lines "), "{text}");
    assert!(
        last.contains("tokens over budget; continue with start_line "),
        "{text}"
    );
    let next = last
        .rsplit(' ')
        .next()
        .and_then(|number| number.trim_end_matches(']').parse::<usize>().ok())
        .expect("continuation line");
    // The budget cut line 8 mid-line, so the next read starts on it again.
    assert!(text.starts_with("@@ src/steps.rs:1-8 of 524\n"), "{text}");
    assert_eq!(next, 8, "{text}");

    let (text, _) = harness
        .call(
            "compresso_outline",
            &json!({ "path": "src/steps.rs", "max_tokens": 150 }),
        )
        .await;
    assert!(text.ends_with("symbols elided: token budget exceeded; read a line range or raise max_tokens for the rest]\n"), "{text}");
    let (text, _) = harness
        .call(
            "compresso_grep",
            &json!({ "pattern": "budget", "max_files": 1, "max_matches_per_file": 3 }),
        )
        .await;
    assert!(
        text.contains("matching lines counted but not listed"),
        "{text}"
    );
    let (text, _) = harness
        .call(
            "compresso_search",
            &json!({ "query": "step", "limit": 40, "max_tokens": 100 }),
        )
        .await;
    assert!(
        text.contains(" of 40 hits shown; more at src/steps.rs:"),
        "{text}"
    );
    harness.close().await;
}

#[tokio::test]
async fn test_text_form_reports_a_stale_index() {
    let fixture = Fixture::new();
    write_file(
        &fixture.root,
        "src/session.rs",
        &format!("{SESSION_RS}\npub fn added() {{}}\n"),
    );
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    for (tool, arguments) in [
        ("compresso_grep", json!({ "pattern": "added" })),
        ("compresso_outline", json!({ "path": "src/session.rs" })),
        ("compresso_search", json!({ "query": "Session" })),
        (
            "compresso_context",
            json!({ "task": "how does a session open" }),
        ),
    ] {
        let (text, _) = harness.call(tool, &arguments).await;
        assert!(
            text.ends_with("[Repository index is stale; run `compresso index` and retry.]\n"),
            "{tool}: {text}"
        );
    }
    let (text, _) = harness
        .call("compresso_grep", &json!({ "pattern": "added" }))
        .await;
    assert!(
        text.contains("src/session.rs (1, changed since indexed: no symbols)"),
        "{text}"
    );
    harness.close().await;
}

#[tokio::test]
async fn test_ledger_records_the_tokens_of_the_text_sent() {
    let fixture = Fixture::new();
    let counter = counter_for(TokenProvider::OpenAiO200k);
    for (tool, arguments) in [
        ("compresso_read", json!({ "path": "src/lib.rs" })),
        ("compresso_outline", json!({ "path": "src/steps.rs" })),
    ] {
        let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
        let (text, _) = harness.call(tool, &arguments).await;
        harness.close().await;
        let latest = fixture
            .store
            .detailed_stats(&compresso_store::StatsFilter {
                operation_limit: Some(1),
                ..compresso_store::StatsFilter::default()
            })
            .expect("stats")
            .by_operation
            .remove(0);
        let operation = fixture
            .store
            .operation_by_id(&latest.operation_id)
            .expect("lookup")
            .expect("recorded");
        assert_eq!(operation.tool, tool);
        assert_eq!(
            operation.tokens_served,
            Some(counter.count(&text)),
            "{tool}"
        );
        assert_eq!(operation.observed_tokens, counter.count(&text));
    }
}

// E17-33: the text form bounds how many lines one read serves and trims
// per-match overhead in grep, without hiding what was left out.

/// The content lines of a text read result between its `@@` header and
/// any trailing `[...]` note.
fn body_lines(text: &str) -> Vec<&str> {
    text.lines()
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .collect()
}

#[tokio::test]
async fn test_text_read_shows_a_window_and_names_the_symbols_past_it() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    let request =
        json!({ "path": "src/steps.rs", "start_line": 1, "end_line": 300, "max_tokens": 10000 });
    let (text, is_error) = harness.call("compresso_read", &request).await;
    assert!(!is_error, "{text}");
    assert!(text.starts_with("@@ src/steps.rs:1-80 of 524\n"), "{text}");
    assert_eq!(body_lines(&text).len(), 80, "{text}");
    assert!(
        text.ends_with(
            "[lines 81-300 not shown: past the 80-line read window; starts there: step_080 81, step_081 82, step_082 83, step_083 84, step_084 85, step_085 86, step_086 87, step_087 88, +212 more; continue with start_line 81]\n"
        ),
        "{text}"
    );

    // A whole-file read is windowed the same way, through the last line.
    let (text, _) = harness
        .call(
            "compresso_read",
            &json!({ "path": "src/steps.rs", "max_tokens": 10000 }),
        )
        .await;
    assert!(text.starts_with("@@ src/steps.rs:1-80 of 524\n"), "{text}");
    assert!(
        text.contains(
            "[lines 81-524 not shown: past the 80-line read window; starts there: step_080 81,"
        ),
        "{text}"
    );
    assert!(
        text.contains(", +313 more; continue with start_line 81]\n"),
        "{text}"
    );

    // A range inside the window is served whole, with no note.
    let (text, _) = harness
        .call(
            "compresso_read",
            &json!({ "path": "src/steps.rs", "start_line": 401, "end_line": 480 }),
        )
        .await;
    assert!(
        text.starts_with("@@ src/steps.rs:401-480 of 524\n"),
        "{text}"
    );
    assert_eq!(body_lines(&text).len(), 80);
    assert!(!text.contains("not shown"), "{text}");
    // A range a few lines over the window is served whole, not left as a
    // stub; one more line past that is windowed.
    let (text, _) = harness
        .call(
            "compresso_read",
            &json!({ "path": "src/steps.rs", "start_line": 401, "end_line": 490, "max_tokens": 10000 }),
        )
        .await;
    assert!(
        text.starts_with("@@ src/steps.rs:401-490 of 524\n"),
        "{text}"
    );
    assert!(!text.contains("not shown"), "{text}");
    let (text, _) = harness
        .call(
            "compresso_read",
            &json!({ "path": "src/steps.rs", "start_line": 401, "end_line": 491, "max_tokens": 10000 }),
        )
        .await;
    assert!(
        text.starts_with("@@ src/steps.rs:401-480 of 524\n"),
        "{text}"
    );
    assert!(text.contains("[lines 481-491 not shown: past the 80-line read window; continue with start_line 481]\n"), "{text}");

    // When the token budget cuts inside the window, the budget stopped the
    // read: the note is the elision of the whole requested range.
    let (text, _) = harness
        .call(
            "compresso_read",
            &json!({ "path": "src/steps.rs", "start_line": 1, "end_line": 300, "max_tokens": 300 }),
        )
        .await;
    let last = text.lines().last().expect("note");
    assert!(last.starts_with("[lines "), "{text}");
    assert!(last.contains("-300 elided: "), "{text}");
    assert!(
        last.contains("tokens over budget; continue with start_line "),
        "{text}"
    );
    assert!(!text.contains("read window"), "{text}");
    harness.close().await;

    // The JSON form is not windowed.
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Full)).await;
    let (text, _) = harness.call("compresso_read", &request).await;
    let value: Value = serde_json::from_str(&text).expect("JSON result");
    assert_eq!(value["range"]["end_line"], 300, "{text}");
    assert_eq!(
        value["content"].as_str().expect("content").lines().count(),
        300
    );
    harness.close().await;
}

#[tokio::test]
async fn test_text_read_ranges_share_one_call_window() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    let (text, is_error) = harness
        .call(
            "compresso_read",
            &json!({ "ranges": [
                { "path": "src/steps.rs", "start_line": 1, "end_line": 150 },
                { "path": "src/steps.rs", "start_line": 200, "end_line": 350 },
                { "path": ".env" },
                { "path": "src/steps.rs", "start_line": 401 },
                { "path": "src/session.rs" }
            ], "max_tokens": 10000 }),
        )
        .await;
    assert!(!is_error, "{text}");
    // 80 + 80 + 80 + 31 lines exceed the 200-line call window: the three
    // long ranges get an equal 56-line share and the short one stays whole.
    for (header, note) in [
        (
            "@@ src/steps.rs:1-56 of 524\n",
            "[lines 57-150 not shown: past the 56-line read window; starts there: step_056 57,",
        ),
        (
            "@@ src/steps.rs:200-255 of 524\n",
            "[lines 256-350 not shown: past the 56-line read window; starts there: step_255 256,",
        ),
        (
            "@@ src/steps.rs:401-456 of 524\n",
            "[lines 457-524 not shown: past the 56-line read window; continue with start_line 457]",
        ),
    ] {
        assert!(text.contains(header), "{header}: {text}");
        assert!(text.contains(note), "{note}: {text}");
    }
    assert!(text.contains("@@ .env [not_found] "), "{text}");
    assert!(text.contains("@@ src/session.rs:1-31 of 31\n"), "{text}");
    let shown = text
        .lines()
        .filter(|line| !line.starts_with("@@ ") && !line.starts_with('['))
        .count();
    assert!(shown <= 200, "{shown} lines served");
    harness.close().await;
}

#[tokio::test]
async fn test_text_read_window_names_no_symbols_for_a_changed_file() {
    let fixture = Fixture::new();
    let mut steps = fs::read_to_string(fixture.root.join("src/steps.rs")).expect("steps");
    steps.insert_str(0, "// edited after indexing\n");
    write_file(&fixture.root, "src/steps.rs", &steps);
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    let (text, _) = harness
        .call(
            "compresso_read",
            &json!({ "path": "src/steps.rs", "start_line": 1, "end_line": 300, "max_tokens": 10000 }),
        )
        .await;
    assert!(text.starts_with("@@ src/steps.rs:1-80 of 525\n"), "{text}");
    assert!(
        text.contains(
            "[lines 81-300 not shown: past the 80-line read window; continue with start_line 81]\n"
        ),
        "{text}"
    );
    harness.close().await;
}

const TABLE_RS: &str = "pub fn table() -> Vec<u32> {\n    let mut rows = Vec::new();\n    rows.push(1);\n    rows.push(2);\n    rows\n}\n\npub fn rows_total() -> usize {\n    3\n}\n// rows end\n";

#[tokio::test]
async fn test_text_grep_does_not_repeat_the_enclosing_symbol() {
    let fixture = Fixture::new();
    write_file(&fixture.root, "src/table.rs", TABLE_RS);
    Indexer::new(&fixture.store, counter_for(TokenProvider::OpenAiO200k))
        .index(&fixture.root, |_| {})
        .expect("fixture reindexes");
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    let (text, _) = harness
        .call(
            "compresso_grep",
            &json!({ "pattern": "rows", "path": "src/table.rs", "max_matches_per_file": 20 }),
        )
        .await;
    assert!(
        text.contains(
            "src/table.rs (6)\n2 [table] let mut rows = Vec::new();\n3 rows.push(1);\n4 rows.push(2);\n5 rows\n8 [def rows_total] pub fn rows_total() -> usize {\n11 [-] // rows end\n"
        ),
        "{text}"
    );
    harness.close().await;

    // The JSON form keeps one symbol per row.
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Full)).await;
    let (text, _) = harness
        .call(
            "compresso_grep",
            &json!({ "pattern": "rows", "path": "src/table.rs", "max_matches_per_file": 20 }),
        )
        .await;
    assert!(text.contains("\"3 [table] rows.push(1);\""), "{text}");
    harness.close().await;
}

#[tokio::test]
async fn test_text_grep_lists_fewer_lines_by_default_and_keeps_requested_context() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    // Context the call asks for is served in full: it is often what spares
    // a read.
    let (text, _) = harness
        .call(
            "compresso_grep",
            &json!({ "pattern": "fn checksum_table", "context_lines": 10 }),
        )
        .await;
    let context = text
        .lines()
        .filter(|line| {
            line.split_once("- ")
                .is_some_and(|(number, _)| number.parse::<u32>().is_ok())
        })
        .count();
    assert_eq!(context, 20, "ten lines on each side: {text}");
    // Three matching lines per file unless the call asks for more.
    let (text, _) = harness
        .call("compresso_grep", &json!({ "pattern": "budget" }))
        .await;
    assert!(text.contains("src/steps.rs (400)\n"), "{text}");
    let rows = text
        .lines()
        .filter(|line| {
            line.split_once(' ')
                .is_some_and(|(number, _)| number.parse::<u32>().is_ok())
        })
        .count();
    assert_eq!(rows, 3, "{text}");
    harness.close().await;

    // The lower defaults belong to the lean profile, whose schema states
    // them; the full profile keeps its advertised defaults in either form.
    let mut harness = Harness::start(
        &fixture,
        options_with(&fixture, McpToolProfile::Full, McpResultFormat::Text),
    )
    .await;
    let (text, _) = harness
        .call("compresso_grep", &json!({ "pattern": "budget" }))
        .await;
    let rows = text
        .lines()
        .filter(|line| {
            line.split_once(' ')
                .is_some_and(|(number, _)| number.parse::<u32>().is_ok())
        })
        .count();
    assert_eq!(rows, 5, "{text}");
    harness.close().await;
}

const SWAP_RS: &str = "// swap first\npub fn swap(pair: [u8; 2]) -> [u8; 2] {\n    let swapped = [pair[1], pair[0]];\n    [swapped[0], swapped[1]] // swap back\n}\n";

#[tokio::test]
async fn test_text_grep_rows_are_unambiguous_at_a_block_start_and_before_a_bracket() {
    let fixture = Fixture::new();
    write_file(&fixture.root, "src/swap.rs", SWAP_RS);
    Indexer::new(&fixture.store, counter_for(TokenProvider::OpenAiO200k))
        .index(&fixture.root, |_| {})
        .expect("fixture reindexes");
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean)).await;
    let (text, _) = harness
        .call(
            "compresso_grep",
            &json!({ "pattern": "swap", "path": "src/swap.rs", "max_matches_per_file": 20 }),
        )
        .await;
    // A first row without a symbol says so; an untagged row always shares
    // the row above; a snippet starting with `[` keeps its tag.
    assert!(
        text.contains(
            "src/swap.rs (4)\n1 [-] // swap first\n2 [def swap] pub fn swap(pair: [u8; 2]) -> [u8; 2] {\n3 let swapped = [pair[1], pair[0]];\n4 [swap] [swapped[0], swapped[1]] // swap back\n"
        ),
        "{text}"
    );
    harness.close().await;
}
