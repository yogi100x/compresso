//! E17-50: the opt-in, read-only `compresso_sql` tool over MCP. A default or
//! lean server never lists or runs it; a full server started with
//! `--sql-env` serves it against the one configured database, writes no
//! ledger row, and states the truth about its network use.
//!
//! Set `COMPRESSO_UPDATE_GOLDEN=1` to rewrite the golden file.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{
    McpDeps, McpServeOptions, McpToolProfile, SqlEngine, SqlSettings, SqlSurface,
    advertised_surface, advertised_surface_for, serve_io_with_options,
};
use compresso_store::{StatsFilter, Store};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const ENV: &str = "COMPRESSO_SQL_FIXTURE_DB";
const NETWORK_CLAIM: &str = "Every tool bounds output, executes no shell command, and makes no independent network request.";

struct Fixture {
    _temp: TempDir,
    _data: TempDir,
    store_path: PathBuf,
    store: Store,
    database: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let store_path = temp.path().join("compresso.db");
        let store = Store::open(&store_path).expect("store");
        let data = tempfile::tempdir().expect("data directory");
        let database = data.path().join("shop.db");
        let connection = Connection::open(&database).expect("database");
        connection
            .execute_batch(
                "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                 INSERT INTO customers (name) VALUES ('ada'), ('grace'), ('edsger');",
            )
            .expect("schema");
        drop(connection);
        Self {
            _temp: temp,
            _data: data,
            store_path,
            store,
            database,
        }
    }

    fn sqlite_url(&self) -> String {
        format!("sqlite:{}", self.database.display())
    }

    fn options(&self, tools: McpToolProfile, edits: bool, sql: Option<&str>) -> McpServeOptions {
        McpServeOptions {
            root: None,
            tools,
            edits,
            sql: sql.map(|value| SqlSettings::with_value(ENV, Some(value))),
            ..McpServeOptions::default()
        }
    }

    fn operations(&self) -> u64 {
        self.store
            .detailed_stats(&StatsFilter::default())
            .expect("stats")
            .summary
            .operations
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
                    "clientInfo": { "name": "sql-test", "version": "1.0.0" }
                }),
            )
            .await;
        harness
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        harness
    }

    fn instructions(&self) -> String {
        self.initialize["result"]["instructions"]
            .as_str()
            .expect("instructions")
            .to_owned()
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

fn names(tools: &[Value]) -> Vec<&str> {
    tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect()
}

fn query(sql: &str) -> Value {
    json!({ "action": "query", "query": sql })
}

#[tokio::test]
async fn test_servers_without_the_flag_and_lean_servers_never_serve_sql() {
    // The exploration agent's tools are the lean allowlist, so it never
    // gets the SQL tool either.
    assert!(!compresso_mcp::lean_tool_allowlist().contains(&"compresso_sql"));
    let fixture = Fixture::new();
    let url = fixture.sqlite_url();
    for (profile, sql, enabled) in [
        (McpToolProfile::Full, None, false),
        (McpToolProfile::Lean, None, false),
        (McpToolProfile::Lean, Some(url.as_str()), true),
    ] {
        let mut harness = Harness::start(&fixture, fixture.options(profile, false, sql)).await;
        let tools = harness.tools().await;
        assert!(!names(&tools).contains(&"compresso_sql"), "{profile:?}");
        let (expected_tools, expected_instructions) = advertised_surface(profile, false);
        assert_eq!(tools.len(), expected_tools.len());
        assert_eq!(harness.instructions(), expected_instructions);
        let (text, is_error) = harness
            .call("compresso_sql", &query("SELECT * FROM customers"))
            .await;
        assert!(is_error);
        assert!(text.starts_with("[tool_unavailable]"), "{text}");
        if enabled {
            assert!(text.contains("lean"), "{text}");
        } else {
            assert!(text.contains("--sql-env"), "{text}");
        }
        assert!(!text.contains("ada"), "{text}");
        harness.close().await;
    }
}

#[tokio::test]
async fn test_sql_server_reads_a_sqlite_file_and_refuses_writes() {
    let fixture = Fixture::new();
    let before = fs::read(&fixture.database).expect("bytes");
    let operations = fixture.operations();
    let url = fixture.sqlite_url();
    let mut harness = Harness::start(
        &fixture,
        fixture.options(McpToolProfile::Full, false, Some(&url)),
    )
    .await;
    let tools = harness.tools().await;
    let sql = tools
        .iter()
        .find(|tool| tool["name"] == "compresso_sql")
        .expect("sql tool listed");
    assert_eq!(sql["annotations"]["readOnlyHint"], true);
    assert_eq!(sql["annotations"]["openWorldHint"], false);
    assert!(
        sql["description"]
            .as_str()
            .unwrap()
            .contains("one sqlite database")
    );
    let instructions = harness.instructions();
    assert!(!instructions.contains(NETWORK_CLAIM));
    assert!(instructions.contains("one local SQLite file"));
    assert!(instructions.contains("no network at all"));

    let (text, is_error) = harness
        .call(
            "compresso_sql",
            &query("SELECT id, name FROM customers ORDER BY id"),
        )
        .await;
    assert!(!is_error, "{text}");
    let result: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(result["engine"], "sqlite");
    assert_eq!(result["action"], "query");
    assert_eq!(result["columns"], json!(["id", "name"]));
    assert_eq!(result["rows"][0], json!([1, "ada"]));
    assert_eq!(result["truncated"], false);

    for (statement, code) in [
        (
            "INSERT INTO customers (name) VALUES ('eve')",
            "[sql_refused]",
        ),
        ("SELECT 1; DELETE FROM customers", "[sql_refused]"),
        ("ATTACH DATABASE ':memory:' AS x", "[sql_refused]"),
    ] {
        let (text, is_error) = harness.call("compresso_sql", &query(statement)).await;
        assert!(is_error, "{statement}");
        assert!(text.starts_with(code), "{statement}: {text}");
    }
    let (text, is_error) = harness
        .call("compresso_sql", &json!({ "action": "query" }))
        .await;
    assert!(
        is_error && text.starts_with("[invalid_arguments]"),
        "{text}"
    );

    let (text, _) = harness
        .call("compresso_sql", &json!({ "action": "tables" }))
        .await;
    let tables: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(tables["tables"][0]["name"], "customers");
    let (text, _) = harness
        .call(
            "compresso_sql",
            &json!({ "action": "describe", "table": "customers" }),
        )
        .await;
    let described: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(described["columns"][1]["name"], "name");
    assert_eq!(described["columns"][1]["nullable"], false);

    // max_rows and max_tokens both bound the rows.
    let (text, _) = harness
        .call(
            "compresso_sql",
            &json!({ "action": "query", "query": "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 5000) SELECT x, 'some padding text ' || x FROM c", "max_rows": 1000, "max_tokens": 300 }),
        )
        .await;
    assert!(
        counter_for(TokenProvider::OpenAiO200k).count(&text) <= 300,
        "{text}"
    );
    let bounded: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(bounded["truncated"], true);
    assert_eq!(bounded["truncation_reason"], "max_tokens");

    // Column names count too: a 99,000-character expression name and 1,500
    // columns both fit max_tokens (E17-50 review).
    for statement in [
        format!("SELECT '{}'", "a".repeat(99_000)),
        format!(
            "SELECT {}",
            (0..1_500)
                .map(|index| format!("{index} AS c{index:045}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ] {
        let (text, is_error) = harness
            .call(
                "compresso_sql",
                &json!({ "action": "query", "query": statement, "max_tokens": 300 }),
            )
            .await;
        assert!(!is_error, "{}", &text[..text.len().min(300)]);
        assert!(
            counter_for(TokenProvider::OpenAiO200k).count(&text) <= 300,
            "{} characters",
            text.len()
        );
        let bounded: Value = serde_json::from_str(&text).expect("json");
        let columns = bounded["columns"].as_array().expect("columns");
        assert!(columns.iter().all(|name| name.as_str().unwrap().chars().count() < 100));
        if statement.contains(" AS c") {
            assert_eq!(bounded["truncated"], true);
            assert!(bounded["columns_omitted"].as_u64().unwrap() > 0);
        }
    }

    // Nothing reaches the ledger, and the file is unchanged.
    assert_eq!(fixture.operations(), operations);
    assert_eq!(fs::read(&fixture.database).expect("bytes"), before);

    // The report names the engine and the remembered failure, never the
    // variable or its value.
    let (text, _) = harness
        .call("compresso_report", &json!({ "kind": "status" }))
        .await;
    assert!(text.contains("sql sqlite"), "{text}");
    assert!(text.contains("compresso_sql"), "{text}");
    assert!(!text.contains(ENV), "{text}");
    assert!(!text.contains("shop.db"), "{text}");
    harness.close().await;
}

#[tokio::test]
async fn test_the_store_itself_is_refused_as_a_sqlite_target() {
    let fixture = Fixture::new();
    let url = format!("sqlite:{}", fixture.store_path.display());
    let mut harness = Harness::start(
        &fixture,
        fixture.options(McpToolProfile::Full, false, Some(&url)),
    )
    .await;
    let (text, is_error) = harness
        .call("compresso_sql", &query("SELECT * FROM operations"))
        .await;
    assert!(is_error);
    assert!(text.starts_with("[sql_refused]"), "{text}");
    harness.close().await;
}

#[tokio::test]
async fn test_unconfigured_and_network_servers_state_their_network_use_and_leak_nothing() {
    let fixture = Fixture::new();
    // Missing variable.
    let mut options = fixture.options(McpToolProfile::Full, false, None);
    options.sql = Some(SqlSettings::with_value(ENV, None));
    let mut harness = Harness::start(&fixture, options).await;
    assert!(names(&harness.tools().await).contains(&"compresso_sql"));
    assert!(
        harness
            .instructions()
            .contains("has no usable database configured")
    );
    let (text, is_error) = harness.call("compresso_sql", &query("SELECT 1")).await;
    assert!(
        is_error && text.starts_with("[sql_not_configured]"),
        "{text}"
    );
    assert!(!text.contains(ENV), "{text}");
    harness.close().await;

    // A Postgres URL whose server refuses the connection (nothing listens
    // on loopback port 1; this crate's tests may not open sockets).
    let port = 1;
    let password = concat!("canary", "-", "Pw", "-", "9902");
    let user = concat!("canary", "_", "user");
    let url = format!("postgres://{user}:{password}@127.0.0.1:{port}/canary_db?sslmode=disable");
    let mut harness = Harness::start(
        &fixture,
        fixture.options(McpToolProfile::Full, true, Some(&url)),
    )
    .await;
    let instructions = harness.instructions();
    assert!(
        instructions.contains("No tool makes an independent network request except compresso_sql")
    );
    assert!(instructions.contains("compresso_edit"));
    let tools = harness.tools().await;
    let sql = tools
        .iter()
        .find(|tool| tool["name"] == "compresso_sql")
        .expect("listed");
    assert_eq!(sql["annotations"]["openWorldHint"], true);
    let (text, is_error) = harness.call("compresso_sql", &query("SELECT 1")).await;
    assert!(is_error && text.starts_with("[sql_connection]"), "{text}");
    for arguments in [
        json!({ "kind": "status" }),
        json!({ "kind": "bug", "note": "it broke" }),
    ] {
        let (report, _) = harness.call("compresso_report", &arguments).await;
        assert!(report.contains("sql postgres"), "{report}");
        for canary in [password, user, "canary_db", ENV] {
            assert!(!report.contains(canary), "{canary} in {report}");
        }
    }
    for canary in [password, user, "canary_db"] {
        assert!(!text.contains(canary), "{canary} in {text}");
        assert!(!instructions.contains(canary));
        assert!(!serde_json::to_string(&tools).unwrap().contains(canary));
    }
    harness.close().await;
}

fn surface_text(profile: McpToolProfile, edits: bool, sql: SqlSurface) -> String {
    let (tools, instructions) = advertised_surface_for(profile, false, edits, sql);
    let mut text = format!(
        "== {} profile, edits {}, sql {:?}\n{instructions}\n",
        profile.name(),
        if edits { "on" } else { "off" },
        sql
    );
    for tool in &tools {
        text.push_str(&format!("- {}\n", tool.name));
        if tool.name == "compresso_sql" {
            let value = canonical(serde_json::to_value(tool).expect("tool json"));
            text.push_str(&serde_json::to_string_pretty(&value).expect("tool json"));
            text.push('\n');
        }
    }
    text
}

/// Keys sorted at every level, so the golden does not depend on whether a
/// workspace build turns on `serde_json`'s insertion-ordered maps.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted = map
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect::<std::collections::BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
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

#[test]
fn test_every_edits_and_sql_combination_matches_its_golden() {
    let mut transcript = String::new();
    for profile in [McpToolProfile::Full, McpToolProfile::Lean] {
        for edits in [false, true] {
            for sql in [
                SqlSurface::Off,
                SqlSurface::Unconfigured,
                SqlSurface::Sqlite,
                SqlSurface::Network(SqlEngine::Postgres),
            ] {
                transcript.push_str(&surface_text(profile, edits, sql));
            }
        }
    }
    check_golden("sql_surfaces.txt", &transcript);

    // Off is byte-identical to the surface without the SQL tool, and the
    // lean profile is identical with or without it.
    for profile in [McpToolProfile::Full, McpToolProfile::Lean] {
        for edits in [false, true] {
            let off = advertised_surface_for(profile, false, edits, SqlSurface::Off);
            assert_eq!(
                off,
                compresso_mcp::advertised_surface_with(profile, false, edits)
            );
        }
        let lean_on = advertised_surface_for(
            McpToolProfile::Lean,
            true,
            true,
            SqlSurface::Network(SqlEngine::MySql),
        );
        assert_eq!(
            lean_on,
            compresso_mcp::advertised_surface_with(McpToolProfile::Lean, true, true)
        );
    }
}

#[test]
fn test_the_sql_tool_definition_stays_small() {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    for sql in [
        SqlSurface::Unconfigured,
        SqlSurface::Sqlite,
        SqlSurface::Network(SqlEngine::Postgres),
        SqlSurface::Network(SqlEngine::MySql),
    ] {
        let (tools, _) = advertised_surface_for(McpToolProfile::Full, false, false, sql);
        let tool = tools
            .iter()
            .find(|tool| tool.name == "compresso_sql")
            .expect("sql tool");
        let tokens = counter.count(&serde_json::to_string(tool).expect("json"));
        assert!(tokens <= 350, "{sql:?}: {tokens} tokens");
    }
}
