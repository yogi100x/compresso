//! E17-32: the opt-in `compresso_edit` tool over MCP. A default server never
//! lists or runs it; a server started with edits enabled serves it in both
//! profiles, records it, and keeps the index fresh.
//!
//! Set `COMPRESSO_UPDATE_GOLDEN=1` to rewrite the golden file.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_atlas::Indexer;
use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{
    McpDeps, McpServeOptions, McpToolProfile, advertised_surface, advertised_surface_with,
    serve_io_with_options,
};
use compresso_store::{StatsFilter, Store};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(20);

const LIB_RS: &str = "/// Formats seconds; see format_utc_now.
pub fn format_utc(seconds: i64) -> String {
    seconds.to_string()
}

pub fn format_utc_now() -> String {
    format_utc(0)
}

pub fn label() -> &'static str {
    \"format_utc_v2\"
}
";

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).expect("src");
        fs::write(root.join("src/lib.rs"), LIB_RS).expect("lib");
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    let _ = lib::format_utc(5);\n}\n",
        )
        .expect("main");
        fs::write(root.join(".env"), "TOKEN=secret\n").expect("secret");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store");
        Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
            .index(&root, |_| {})
            .expect("fixture indexes");
        let root = fs::canonicalize(root).expect("root canonicalizes");
        Self {
            _temp: temp,
            root,
            store,
        }
    }

    fn options(&self, tools: McpToolProfile, edits: bool) -> McpServeOptions {
        McpServeOptions {
            root: Some(self.root.clone()),
            tools,
            edits,
            ..McpServeOptions::default()
        }
    }

    fn text(&self, path: &str) -> String {
        fs::read_to_string(self.root.join(path)).expect("file")
    }
}

struct Harness {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
    next_id: u64,
    initialize: Value,
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
            initialize: Value::Null,
        };
        harness.initialize = harness
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "edit-test", "version": "1.0.0" }
                }),
            )
            .await;
        harness
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        harness
    }

    fn instructions(&self) -> &str {
        self.initialize["result"]["instructions"]
            .as_str()
            .expect("instructions")
    }

    async fn tools(&mut self) -> Vec<Value> {
        self.request("tools/list", json!({})).await["result"]["tools"]
            .as_array()
            .expect("tools")
            .clone()
    }

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
        (
            response["result"]["content"][0]["text"]
                .as_str()
                .expect("text block")
                .to_owned(),
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

fn rename_call(dry_run: bool) -> Value {
    json!({
        "operations": [{ "op": "rename", "old": "format_utc", "new": "format_rfc3339_utc" }],
        "dry_run": dry_run
    })
}

fn tool_names(tools: &[Value]) -> Vec<&str> {
    tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect()
}

#[tokio::test]
async fn test_default_server_does_not_list_the_edit_tool_and_refuses_calls() {
    let fixture = Fixture::new();
    for profile in [McpToolProfile::Full, McpToolProfile::Lean] {
        let mut harness = Harness::start(&fixture, fixture.options(profile, false)).await;
        let tools = harness.tools().await;
        assert!(!tool_names(&tools).contains(&"compresso_edit"));
        let (expected_tools, expected_instructions) = advertised_surface(profile, false);
        assert_eq!(tools.len(), expected_tools.len());
        assert_eq!(harness.instructions(), expected_instructions);
        let (text, is_error) = harness.call("compresso_edit", &rename_call(false)).await;
        assert!(is_error);
        assert!(text.starts_with("[tool_unavailable]"), "{text}");
        assert!(text.contains("--edits"), "{text}");
        harness.close().await;
    }
    assert_eq!(fixture.text("src/lib.rs"), LIB_RS);
}

#[tokio::test]
async fn test_full_profile_with_edits_serves_records_and_reindexes() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Full, true)).await;
    let tools = harness.tools().await;
    let edit = tools
        .iter()
        .find(|tool| tool["name"] == "compresso_edit")
        .expect("edit tool listed");
    assert_eq!(edit["annotations"]["readOnlyHint"], false);
    assert_eq!(edit["annotations"]["destructiveHint"], true);
    assert!(!harness.instructions().contains("read-only access"));
    assert!(harness.instructions().contains("compresso_edit"));

    let (text, is_error) = harness.call("compresso_edit", &rename_call(false)).await;
    assert!(!is_error, "{text}");
    let report: Value = serde_json::from_str(&text).expect("JSON form");
    assert_eq!(report["sites_total"], 3);
    assert_eq!(report["files_total"], 2);
    assert_eq!(report["index"], "fresh");
    assert_eq!(report["operations"][0]["remaining_whole_total"], 0);
    assert_eq!(report["operations"][0]["remaining_substring_total"], 3);
    assert!(
        report["files"]
            .as_array()
            .expect("files")
            .iter()
            .all(|file| file["verified"] == true)
    );
    assert!(
        fixture
            .text("src/main.rs")
            .contains("lib::format_rfc3339_utc(5)")
    );

    // The index is fresh: the symbol is found by its new name.
    let (search, _) = harness
        .call(
            "compresso_search",
            &json!({ "query": "format_rfc3339_utc" }),
        )
        .await;
    assert!(
        search.contains("\"qualified_name\":\"format_rfc3339_utc\""),
        "{search}"
    );

    // The edit is recorded under its own tool, and explain answers for it.
    let operation_id = report["operation_id"].as_str().expect("id").to_owned();
    let (explain, explain_error) = harness
        .call(
            "compresso_explain",
            &json!({ "operation_id": operation_id }),
        )
        .await;
    assert!(explain_error);
    assert!(explain.starts_with("[not_found]"), "{explain}");
    let (stats, stats_error) = harness.call("compresso_stats", &json!({})).await;
    assert!(!stats_error, "{stats}");
    harness.close().await;
    let stored = fixture
        .store
        .operation_by_id(&operation_id)
        .expect("store")
        .expect("edit recorded");
    assert_eq!(stored.tool, "compresso_edit");
    assert!(stored.tokens_served.is_some_and(|tokens| tokens > 0));
    let stats = fixture
        .store
        .detailed_stats(&StatsFilter {
            since: None,
            session_id: None,
            operation_limit: None,
        })
        .expect("stats");
    assert!(
        stats
            .by_tool
            .iter()
            .any(|tool| tool.tool == "compresso_edit" && tool.operations == 1)
    );
}

#[tokio::test]
async fn test_errors_write_nothing_and_name_the_gate() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean, true)).await;
    for (arguments, code) in [
        (
            json!({ "operations": [{ "op": "replace", "path": ".env", "old": "secret", "new": "x" }] }),
            "[edit_refused]",
        ),
        (
            json!({ "operations": [{ "op": "replace", "path": "src/lib.rs", "old": "format_utc", "new": "x" }] }),
            "[count_mismatch]",
        ),
        (
            json!({ "operations": [{ "op": "rename", "old": "format_ut", "new": "x" }] }),
            "[no_matches]",
        ),
        (
            json!({ "operations": [{ "op": "rename", "old": "format_utc", "new": "x y" }] }),
            "[invalid_arguments]",
        ),
        (
            json!({ "operations": [{ "op": "rename", "old": "format_utc", "new": "format_utc_now" }] }),
            "[name_exists]",
        ),
        (
            json!({ "operations": [{ "op": "replace", "path": ".gitignore", "old": "x", "new": "y" }] }),
            "[edit_refused]",
        ),
        (
            json!({ "operations": [{ "op": "delete", "path": "src/lib.rs" }] }),
            "[invalid_arguments]",
        ),
        (
            json!({ "operations": [{ "op": "replace", "path": "../x.rs", "old": "a", "new": "b" }] }),
            "[edit_refused]",
        ),
    ] {
        let (text, is_error) = harness.call("compresso_edit", &arguments).await;
        assert!(is_error, "{text}");
        assert!(text.starts_with(code), "{code}: {text}");
    }
    harness.close().await;
    assert_eq!(fixture.text("src/lib.rs"), LIB_RS);
    assert_eq!(fixture.text(".env"), "TOKEN=secret\n");
}

#[tokio::test]
async fn test_a_tiny_budget_applies_the_edit_once_and_flags_the_cut() {
    let fixture = Fixture::new();
    let mut source = String::new();
    for index in 0..60 {
        source.push_str(&format!("fn caller_{index}() {{ old_name(); }}\n"));
    }
    source.push_str("fn old_name() {}\n");
    for index in 0..30 {
        source.push_str(&format!("fn old_name_{index}() {{}}\n"));
    }
    fs::write(fixture.root.join("src/many.rs"), &source).expect("many");
    Indexer::new(&fixture.store, counter_for(TokenProvider::OpenAiO200k))
        .index(&fixture.root, |_| {})
        .expect("reindex");
    for profile in [McpToolProfile::Lean, McpToolProfile::Full] {
        let fresh = profile == McpToolProfile::Lean;
        let (old, new) = if fresh {
            ("old_name", "new_name")
        } else {
            ("new_name", "newer_name")
        };
        let mut harness = Harness::start(&fixture, fixture.options(profile, true)).await;
        let (text, is_error) = harness
            .call(
                "compresso_edit",
                &json!({
                    "operations": [{ "op": "rename", "old": old, "new": new, "path": "src/many.rs" }],
                    "max_tokens": 300
                }),
            )
            .await;
        assert!(!is_error, "{text}");
        assert!(
            counter_for(TokenProvider::OpenAiO200k).count(&text) <= 300,
            "{text}"
        );
        if fresh {
            assert!(text.contains("report cut to max_tokens"), "{text}");
        } else {
            let report: Value = serde_json::from_str(&text).expect("JSON");
            assert_eq!(report["truncated"], true);
            assert_eq!(report["sites_total"], 61);
        }
        harness.close().await;
    }
    // Applied exactly once per call: a re-run would have found no match.
    let edited = fixture.text("src/many.rs");
    assert_eq!(edited.matches("newer_name();").count(), 60);
    assert_eq!(edited.matches("fn old_name_").count(), 30);
}

fn check_golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("COMPRESSO_UPDATE_GOLDEN").is_some() {
        fs::write(&path, actual).expect("golden written");
        return;
    }
    let expected = fs::read_to_string(&path).expect("golden file exists");
    assert_eq!(actual, expected, "{name} differs from its golden");
}

#[tokio::test]
async fn test_lean_edit_server_text_matches_its_golden() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean, true)).await;
    let mut transcript = String::new();
    transcript.push_str("=== instructions\n");
    transcript.push_str(harness.instructions());
    transcript.push_str("\n=== tools\n");
    let tools = harness.tools().await;
    transcript.push_str(&tool_names(&tools).join("\n"));
    transcript.push('\n');
    for (label, arguments) in [
        ("dry_run", rename_call(true)),
        ("rename", rename_call(false)),
        (
            "replace_and_insert",
            json!({ "operations": [
                { "op": "replace", "path": "src/main.rs", "old": "(5)", "new": "(6)" },
                { "op": "insert", "path": "src/main.rs", "anchor": "fn main() {\n", "text": "    // edited\n" }
            ] }),
        ),
        (
            "count_mismatch",
            json!({ "operations": [{ "op": "replace", "path": "src/lib.rs", "old": "format_rfc3339_utc", "new": "x" }] }),
        ),
    ] {
        let (text, is_error) = harness.call("compresso_edit", &arguments).await;
        transcript.push_str(&format!("=== {label} error={is_error}\n{text}"));
        if !text.ends_with('\n') {
            transcript.push('\n');
        }
    }
    harness.close().await;
    check_golden("lean_edit_text.txt", &transcript);
}

#[test]
fn test_lean_instructions_name_the_edit_rule_only_when_it_is_served() {
    let (_, without) = advertised_surface(McpToolProfile::Lean, false);
    let (tools, with) = advertised_surface_with(McpToolProfile::Lean, false, true);
    assert!(!without.contains("compresso_edit"));
    assert!(without.contains("read-only"));
    assert!(with.contains("compresso_edit, not shell text substitution"));
    assert!(with.contains("0 remaining whole matches"));
    assert!(!with.contains("read-only"));
    assert_eq!(
        tools.last().map(|tool| tool.name.as_ref()),
        Some("compresso_edit")
    );
}

#[tokio::test]
async fn test_the_report_cap_holds_for_a_full_batch_of_operations() {
    let fixture = Fixture::new();
    let mut source = String::new();
    // Two full batches (E17-47: 64 operations per call).
    let batch = compresso_atlas::EDIT_MAX_OPERATIONS;
    for index in 0..2 * batch {
        source.push_str(&format!(
            "pub const A_RATHER_LONG_CONFIGURATION_VALUE_NAME_{index:03}: u32 = 0;\n"
        ));
    }
    fs::write(fixture.root.join("src/values.rs"), &source).expect("values");
    Indexer::new(&fixture.store, counter_for(TokenProvider::OpenAiO200k))
        .index(&fixture.root, |_| {})
        .expect("reindex");
    let counter = counter_for(TokenProvider::OpenAiO200k);
    for (profile, offset) in [(McpToolProfile::Full, 0), (McpToolProfile::Lean, batch)] {
        let operations = (offset..offset + batch)
            .map(|index| {
                json!({
                    "op": "replace",
                    "path": "src/values.rs",
                    "old": format!("A_RATHER_LONG_CONFIGURATION_VALUE_NAME_{index:03}: u32 = 0"),
                    "new": format!("A_RATHER_LONG_CONFIGURATION_VALUE_NAME_{index:03}: u32 = 1")
                })
            })
            .collect::<Vec<_>>();
        let mut harness = Harness::start(&fixture, fixture.options(profile, true)).await;
        let (text, is_error) = harness
            .call(
                "compresso_edit",
                &json!({ "operations": operations, "max_tokens": 300 }),
            )
            .await;
        assert!(!is_error, "{text}");
        assert!(counter.count(&text) <= 300, "{profile:?}: {text}");
        if profile == McpToolProfile::Full {
            let report: Value = serde_json::from_str(&text).expect("JSON");
            assert_eq!(report["truncated"], true);
            assert_eq!(report["sites_total"], batch);
        } else {
            assert!(
                text.starts_with(&format!("edited {batch} site(s) in 1 file(s)")),
                "{text}"
            );
        }
        harness.close().await;
    }
    assert_eq!(
        fixture.text("src/values.rs").matches("u32 = 1;").count(),
        2 * batch
    );
}

#[tokio::test]
async fn test_the_safety_totals_survive_any_report_cut() {
    let fixture = Fixture::new();
    let mut source = String::new();
    for index in 0..16 {
        source.push_str(&format!(
            "pub fn a_quite_long_function_name_for_the_cap_test_{index:02}() {{}}\n"
        ));
    }
    fs::write(fixture.root.join("src/long.rs"), &source).expect("long");
    // An agent instruction file names one of them: that rename is incomplete.
    fs::write(
        fixture.root.join("AGENTS.md"),
        "Call a_quite_long_function_name_for_the_cap_test_00 first.\n",
    )
    .expect("agents");
    Indexer::new(&fixture.store, counter_for(TokenProvider::OpenAiO200k))
        .index(&fixture.root, |_| {})
        .expect("reindex");
    let counter = counter_for(TokenProvider::OpenAiO200k);
    for (profile, prefix) in [(McpToolProfile::Full, "b"), (McpToolProfile::Lean, "c")] {
        let from = if prefix == "b" { "a" } else { "b" };
        fs::write(
            fixture.root.join("AGENTS.md"),
            format!("Call {from}_quite_long_function_name_for_the_cap_test_00 first.\n"),
        )
        .expect("agents");
        let operations = (0..16)
            .map(|index| {
                json!({
                    "op": "rename",
                    "old": format!("{from}_quite_long_function_name_for_the_cap_test_{index:02}"),
                    "new": format!("{prefix}_quite_long_function_name_for_the_cap_test_{index:02}")
                })
            })
            .collect::<Vec<_>>();
        let mut harness = Harness::start(&fixture, fixture.options(profile, true)).await;
        let (text, is_error) = harness
            .call(
                "compresso_edit",
                &json!({ "operations": operations, "max_tokens": 300 }),
            )
            .await;
        harness.close().await;
        assert!(!is_error, "{text}");
        assert!(counter.count(&text) <= 300, "{text}");
        if profile == McpToolProfile::Full {
            let report: Value = serde_json::from_str(&text).expect("JSON");
            assert_eq!(report["incomplete_operations"], 1, "{text}");
            assert_eq!(report["files_skipped_total"], 1, "{text}");
            assert_eq!(report["remaining_whole_total"], 0, "{text}");
            assert_eq!(report["unverified_files"], 0, "{text}");
            assert!(report["remaining_adjacent_total"].is_u64(), "{text}");
        } else {
            let header = text.lines().next().expect("header");
            assert!(header.contains("INCOMPLETE"), "{text}");
        }
    }
}

#[test]
fn test_the_store_directory_is_protected_under_a_non_canonical_root() {
    let temp = tempfile::tempdir().expect("temporary directory");
    // On macOS the temporary directory is reached through a symbolic link
    // (/var -> /private/var), so this root is not canonical.
    let root = temp.path().join("home");
    let data = root.join(".local/share/compresso");
    fs::create_dir_all(&data).expect("data");
    let store = Store::open(&data.join("compresso.db")).expect("store");
    let protected = compresso_mcp::edit::protected_paths(&store, &root);
    let canonical = fs::canonicalize(&data).expect("canonical");
    assert!(protected.contains(&canonical), "{protected:?}");
}

/// E17-36 (D101): one call edits several files, all or nothing: a gate that
/// fails on any file's operation writes no file; when every gate passes,
/// every file is written.
#[tokio::test]
async fn test_one_call_edits_several_files_all_or_nothing() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean, true)).await;
    let main_rs = fixture.text("src/main.rs");
    let (text, is_error) = harness
        .call(
            "compresso_edit",
            &json!({ "operations": [
                { "op": "replace", "path": "src/main.rs", "old": "format_utc(5)", "new": "format_utc(6)" },
                { "op": "replace", "path": "src/lib.rs", "old": "format_utc", "new": "x" }
            ] }),
        )
        .await;
    assert!(is_error && text.starts_with("[count_mismatch]"), "{text}");
    assert_eq!(fixture.text("src/main.rs"), main_rs);
    assert_eq!(fixture.text("src/lib.rs"), LIB_RS);
    let (text, is_error) = harness
        .call(
            "compresso_edit",
            &json!({ "operations": [
                { "op": "replace", "path": "src/main.rs", "old": "format_utc(5)", "new": "format_utc(6)" },
                { "op": "replace", "path": "src/lib.rs", "old": "seconds.to_string()", "new": "seconds.to_string() + \"s\"" }
            ] }),
        )
        .await;
    assert!(!is_error, "{text}");
    harness.close().await;
    assert!(fixture.text("src/main.rs").contains("format_utc(6)"));
    assert!(
        fixture
            .text("src/lib.rs")
            .contains("seconds.to_string() + \"s\"")
    );
}

/// E17-36 (D101): a rename scoped by path says how many whole matches of
/// the old name it left outside its scope, and is not done while any remain.
#[tokio::test]
async fn test_a_scoped_rename_reports_whole_matches_outside_its_scope() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean, true)).await;
    let (text, is_error) = harness
        .call(
            "compresso_edit",
            &json!({ "operations": [
                { "op": "rename", "old": "format_utc", "new": "format_seconds", "path": "src/lib.rs" }
            ] }),
        )
        .await;
    assert!(!is_error, "{text}");
    assert!(text.contains("INCOMPLETE: 1 rename(s)"), "{text}");
    assert!(
        text.contains("remain outside a rename's path scope"),
        "{text}"
    );
    assert!(
        text.contains("under src/lib.rs; remaining in scope: 0 whole"),
        "{text}"
    );
    assert!(
        text.contains("; 1 whole outside scope, not renamed"),
        "{text}"
    );
    assert!(
        text.contains("INCOMPLETE: the old name remains outside the scope"),
        "{text}"
    );
    assert!(text.contains("  outside scope src/main.rs:2: "), "{text}");
    harness.close().await;
    assert!(fixture.text("src/main.rs").contains("lib::format_utc(5)"));
}

/// E17-47 (D112): the schema advertises the limit, an over-limit call says
/// the limit and the count, and one call creates a file, edits a file made
/// since the last index, and reports an identical replace as unchanged.
#[tokio::test]
async fn test_large_batches_create_and_unchanged_replaces_over_mcp() {
    let limit = compresso_atlas::EDIT_MAX_OPERATIONS;
    let (tools, _) = advertised_surface_with(McpToolProfile::Full, false, true);
    let edit = tools
        .iter()
        .find(|tool| tool.name == "compresso_edit")
        .expect("edit tool");
    let schema = serde_json::to_value(edit.input_schema.as_ref()).expect("schema");
    assert_eq!(schema["properties"]["operations"]["maxItems"], json!(limit));
    let ops = schema["properties"]["operations"]["items"]["properties"]["op"]["enum"].clone();
    assert!(
        ops.as_array().expect("enum").contains(&json!("create")),
        "{ops}"
    );
    let description = edit.description.as_deref().unwrap_or_default();
    assert!(
        description.contains(&format!("Up to {limit} operations")),
        "{description}"
    );

    let fixture = Fixture::new();
    fs::write(fixture.root.join("src/late.rs"), "fn late() {}\n").expect("late");
    for profile in [McpToolProfile::Full, McpToolProfile::Lean] {
        let mut harness = Harness::start(&fixture, fixture.options(profile, true)).await;
        let too_many = (0..=limit)
            .map(|_| json!({ "op": "replace", "path": "src/lib.rs", "old": "a", "new": "b" }))
            .collect::<Vec<_>>();
        let (text, is_error) = harness
            .call("compresso_edit", &json!({ "operations": too_many }))
            .await;
        assert!(is_error);
        assert!(text.starts_with("[limit_exceeded]"), "{text}");
        assert!(
            text.contains(&format!("({} > {limit})", limit + 1)) && text.contains("split"),
            "{text}"
        );
        let (text, is_error) = harness
            .call(
                "compresso_edit",
                &json!({ "operations": [{ "op": "create", "path": "src/lib.rs", "contents": "x" }] }),
            )
            .await;
        assert!(is_error);
        assert!(text.contains("create takes path, content"), "{text}");
        harness.close().await;
    }

    let calls = |file: &str| {
        json!({ "operations": [
            { "op": "create", "path": file, "content": "pub fn made() {}\n" },
            { "op": "replace", "path": "src/lib.rs", "old": "pub fn label", "new": "pub fn label" },
            { "op": "replace", "path": "src/late.rs", "old": "fn late", "new": "fn later" }
        ] })
    };
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Full, true)).await;
    let (text, is_error) = harness.call("compresso_edit", &calls("src/made.rs")).await;
    assert!(!is_error, "{text}");
    let report: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(report["files_created"], 1);
    assert_eq!(report["unchanged_operations"], 1);
    assert_eq!(report["operations"][0]["op"], "create");
    assert_eq!(report["operations"][1]["unchanged"], true);
    assert_eq!(report["operations"][1]["matched"], 1);
    assert_eq!(report["operations"][2]["sites"], 1);
    let created = report["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|file| file["path"] == "src/made.rs")
        .expect("created file reported");
    assert_eq!(created["created"], true);
    assert_eq!(created["verified"], true);
    harness.close().await;
    assert_eq!(fixture.text("src/made.rs"), "pub fn made() {}\n");
    assert_eq!(fixture.text("src/late.rs"), "fn later() {}\n");
    assert_eq!(fixture.text("src/lib.rs"), LIB_RS);

    // The lean text form states both.
    fs::write(fixture.root.join("src/late.rs"), "fn late() {}\n").expect("late again");
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean, true)).await;
    let (text, is_error) = harness.call("compresso_edit", &calls("src/other.rs")).await;
    assert!(!is_error, "{text}");
    assert!(text.contains("1 file(s) created"), "{text}");
    assert!(text.contains("@@ src/other.rs: created"), "{text}");
    assert!(
        text.contains(
            "replace in src/lib.rs: old and new are identical; 1 match(es) found, unchanged"
        ),
        "{text}"
    );
    harness.close().await;
}

/// E17-49 (D114): create makes missing directories and says which; a
/// refused directory is named in the error.
#[tokio::test]
async fn test_create_reports_the_directories_it_makes_over_mcp() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Full, true)).await;
    let create =
        |path: &str| json!({ "operations": [{ "op": "create", "path": path, "content": "x\n" }] });
    let (text, is_error) = harness
        .call("compresso_edit", &create("src/deep/new/made.rs"))
        .await;
    assert!(!is_error, "{text}");
    let report: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(
        report["directories_created"],
        json!(["src/deep", "src/deep/new"])
    );
    let (text, is_error) = harness
        .call("compresso_edit", &create("src/plain.rs"))
        .await;
    assert!(!is_error, "{text}");
    let report: Value = serde_json::from_str(&text).expect("JSON");
    assert!(report.get("directories_created").is_none(), "{report}");
    let (text, is_error) = harness
        .call("compresso_edit", &create("src/.cache/x/y.rs"))
        .await;
    assert!(is_error);
    assert!(
        text.starts_with("[edit_refused]") && text.contains("(directory \"src/.cache\")"),
        "{text}"
    );
    harness.close().await;
    assert!(!fixture.root.join("src/.cache").exists());
    assert_eq!(fixture.text("src/deep/new/made.rs"), "x\n");

    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean, true)).await;
    let (text, is_error) = harness
        .call(
            "compresso_edit",
            &json!({ "dry_run": true, "operations": [{ "op": "create", "path": "lib/one/two.rs", "content": "x\n" }] }),
        )
        .await;
    assert!(!is_error, "{text}");
    assert!(
        text.contains("2 directories to create: lib, lib/one"),
        "{text}"
    );
    harness.close().await;
    assert!(!fixture.root.join("lib").exists());
}

/// E17-49 (D114), lean text form end to end: a grep limited to one file
/// names the file outside it where the pattern also matches, and a call at
/// the smallest budget still succeeds within it. (Here because this harness
/// serves lean.)
#[tokio::test]
async fn test_lean_grep_text_names_a_file_outside_the_path_filter() {
    let fixture = Fixture::new();
    let mut harness = Harness::start(&fixture, fixture.options(McpToolProfile::Lean, true)).await;
    let (text, is_error) = harness
        .call(
            "compresso_edit",
            &json!({ "operations": [{ "op": "create", "path": "src/other.rs", "content": "pub fn other() -> String {\n    crate::format_utc(1);\n    format_utc(2)\n}\n" }] }),
        )
        .await;
    assert!(!is_error, "{text}");
    let grep = |max_tokens: u64| json!({ "patterns": ["format_utc("], "path": "src/lib.rs", "max_tokens": max_tokens });
    let (text, is_error) = harness.call("compresso_grep", &grep(1_000)).await;
    assert!(!is_error, "{text}");
    assert!(
        text.contains("[also matches outside this path: src/other.rs (2), src/main.rs (1)]"),
        "{text}"
    );
    let (text, is_error) = harness.call("compresso_grep", &grep(300)).await;
    assert!(!is_error, "{text}");
    harness.close().await;
}
