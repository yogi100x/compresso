//! E17-52: the opt-in, read-only `compresso_mongo` tool over MCP. A default
//! or lean server never lists or runs it; a full server started with
//! `--mongo-env` serves it against the one configured server, refuses
//! arguments off the allow-list before connecting, writes no ledger row, and
//! states the truth about its network use. With the tool off every surface is
//! byte-identical to the one without it.
//!
//! Set `COMPRESSO_UPDATE_GOLDEN=1` to rewrite the golden file.

use std::fs;
use std::path::Path;
use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{
    McpDeps, McpServeOptions, McpToolProfile, MongoSettings, MongoSurface, SqlEngine, SqlSettings,
    SqlSurface, advertised_surface, advertised_surface_for, advertised_surface_with_databases,
    serve_io_with_options,
};
use compresso_store::{StatsFilter, Store};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const ENV: &str = "COMPRESSO_MONGO_FIXTURE";
const PASSWORD: &str = concat!("canary", "-", "Pw", "-", "5217");
const USER: &str = concat!("canary", "_", "reader");

/// Nothing listens on loopback port 1 (this crate's tests may not open
/// sockets), so an allowed call fails to connect.
fn refused_url() -> String {
    format!("mongodb://{USER}:{PASSWORD}@127.0.0.1:1/canary_db")
}

struct Fixture {
    _temp: TempDir,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store");
        Self { _temp: temp, store }
    }

    fn options(&self, tools: McpToolProfile, mongo: Option<Option<&str>>) -> McpServeOptions {
        McpServeOptions {
            root: None,
            tools,
            mongo: mongo.map(|value| MongoSettings::with_value(ENV, value)),
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
                    "clientInfo": { "name": "mongo-test", "version": "1.0.0" }
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

fn find() -> Value {
    json!({ "action": "find", "collection": "items", "filter": {"a": 1} })
}

#[tokio::test]
async fn test_servers_without_the_flag_and_lean_servers_never_serve_mongo() {
    // The exploration agent's tools are the lean allowlist, so it never gets
    // the MongoDB tool either.
    assert!(!compresso_mcp::lean_tool_allowlist().contains(&"compresso_mongo"));
    let fixture = Fixture::new();
    let url = refused_url();
    for (profile, mongo, enabled) in [
        (McpToolProfile::Full, None, false),
        (McpToolProfile::Lean, None, false),
        (McpToolProfile::Lean, Some(Some(url.as_str())), true),
    ] {
        let mut harness = Harness::start(&fixture, fixture.options(profile, mongo)).await;
        let tools = harness.tools().await;
        assert!(!names(&tools).contains(&"compresso_mongo"), "{profile:?}");
        let (expected_tools, expected_instructions) = advertised_surface(profile, false);
        assert_eq!(tools.len(), expected_tools.len());
        assert_eq!(harness.instructions(), expected_instructions);
        let (text, is_error) = harness.call("compresso_mongo", &find()).await;
        assert!(is_error);
        assert!(text.starts_with("[tool_unavailable]"), "{text}");
        if enabled {
            assert!(text.contains("lean"), "{text}");
        } else {
            assert!(text.contains("--mongo-env"), "{text}");
        }
        harness.close().await;
    }
}

#[tokio::test]
async fn test_a_mongo_server_refuses_before_connecting_states_its_network_use_and_leaks_nothing() {
    let fixture = Fixture::new();
    let url = refused_url();
    let mut harness = Harness::start(
        &fixture,
        fixture.options(McpToolProfile::Full, Some(Some(&url))),
    )
    .await;
    let instructions = harness.instructions();
    assert!(
        instructions
            .contains("No tool makes an independent network request except compresso_mongo"),
        "{instructions}"
    );
    let tools = harness.tools().await;
    let mongo = tools
        .iter()
        .find(|tool| tool["name"] == "compresso_mongo")
        .expect("listed");
    assert_eq!(mongo["annotations"]["readOnlyHint"], true);
    assert_eq!(mongo["annotations"]["openWorldHint"], true);
    let before = fixture.operations();

    for (arguments, code) in [
        (
            json!({"action": "aggregate", "collection": "items", "pipeline": [{"$out": "copy"}]}),
            "[mongo_refused]",
        ),
        (
            json!({"action": "find", "collection": "items", "filter": {"$where": "1"}}),
            "[mongo_refused]",
        ),
        (
            json!({"action": "aggregate", "collection": "items", "pipeline": [{"$merge": {"into": "x"}}]}),
            "[mongo_refused]",
        ),
        (json!({"action": "find"}), "[invalid_arguments]"),
        (
            json!({"action": "find", "collection": "items", "pipeline": []}),
            "[invalid_arguments]",
        ),
        (
            json!({"action": "aggregate", "collection": "items"}),
            "[invalid_arguments]",
        ),
        (
            json!({"action": "find", "collection": "items", "filter": "a"}),
            "[invalid_arguments]",
        ),
        (
            json!({"action": "collections", "collection": "items"}),
            "[invalid_arguments]",
        ),
        (
            json!({"action": "drop", "collection": "items"}),
            "[invalid_arguments]",
        ),
        (
            json!({"action": "find", "collection": "items", "command": {"drop": "items"}}),
            "[invalid_arguments]",
        ),
        (
            json!({"action": "find", "collection": "items", "filter": {"a": "x".repeat(100_001)}}),
            "[invalid_arguments]",
        ),
        (
            json!({"action": "find", "collection": "items", "sort": {"a": 1, "b": -1}}),
            "[mongo_refused]",
        ),
        (find(), "[mongo_connection]"),
    ] {
        let (text, is_error) = harness.call("compresso_mongo", &arguments).await;
        assert!(is_error && text.starts_with(code), "{arguments}: {text}");
        for canary in [PASSWORD, USER, "canary_db", ENV] {
            assert!(!text.contains(canary), "{canary} in {text}");
        }
    }
    assert_eq!(
        fixture.operations(),
        before,
        "a MongoDB call wrote a ledger row"
    );
    for arguments in [
        json!({ "kind": "status" }),
        json!({ "kind": "bug", "note": "it broke" }),
    ] {
        let (report, _) = harness.call("compresso_report", &arguments).await;
        assert!(report.contains("mongo on"), "{report}");
        for canary in [PASSWORD, USER, "canary_db", ENV] {
            assert!(!report.contains(canary), "{canary} in {report}");
        }
    }
    for canary in [PASSWORD, USER, "canary_db"] {
        assert!(!instructions.contains(canary));
        assert!(!serde_json::to_string(&tools).unwrap().contains(canary));
    }
    harness.close().await;
}

#[tokio::test]
async fn test_an_unconfigured_mongo_server_says_so_and_connects_to_nothing() {
    let fixture = Fixture::new();
    for value in [
        None,
        Some(format!(
            "mongodb+srv://{USER}:{PASSWORD}@cluster.example.com/canary_db"
        )),
    ] {
        let mut harness = Harness::start(
            &fixture,
            fixture.options(McpToolProfile::Full, Some(value.as_deref())),
        )
        .await;
        assert!(names(&harness.tools().await).contains(&"compresso_mongo"));
        let instructions = harness.instructions();
        assert!(
            instructions.contains("has no usable server configured"),
            "{instructions}"
        );
        let (text, is_error) = harness.call("compresso_mongo", &find()).await;
        assert!(
            is_error && text.starts_with("[mongo_not_configured]"),
            "{text}"
        );
        assert!(!text.contains(ENV) && !text.contains(PASSWORD), "{text}");
        let (report, _) = harness
            .call("compresso_report", &json!({"kind": "status"}))
            .await;
        assert!(report.contains("mongo not configured"), "{report}");
        harness.close().await;
    }
}

#[tokio::test]
async fn test_sql_and_mongo_together_are_both_served_and_named() {
    let fixture = Fixture::new();
    let mut options = fixture.options(McpToolProfile::Full, Some(Some(&refused_url())));
    options.sql = Some(SqlSettings::with_value(
        "COMPRESSO_SQL_FIXTURE",
        Some("postgres://u:p@127.0.0.1:1/db?sslmode=disable"),
    ));
    let mut harness = Harness::start(&fixture, options).await;
    let tools = harness.tools().await;
    assert!(names(&tools).contains(&"compresso_sql"));
    assert!(names(&tools).contains(&"compresso_mongo"));
    let instructions = harness.instructions();
    assert!(
        instructions.contains(
            "No tool makes an independent network request except compresso_sql and compresso_mongo"
        ),
        "{instructions}"
    );
    harness.close().await;
}

fn surface_text(profile: McpToolProfile, sql: SqlSurface, mongo: MongoSurface) -> String {
    let (tools, instructions) =
        advertised_surface_with_databases(profile, false, false, sql, mongo);
    let mut text = format!(
        "== {} profile, sql {sql:?}, mongo {mongo:?}\n{instructions}\n",
        profile.name(),
    );
    for tool in &tools {
        text.push_str(&format!("- {}\n", tool.name));
        if tool.name == "compresso_mongo" {
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
fn test_every_sql_and_mongo_combination_matches_its_golden() {
    let mut transcript = String::new();
    for profile in [McpToolProfile::Full, McpToolProfile::Lean] {
        for sql in [
            SqlSurface::Off,
            SqlSurface::Unconfigured,
            SqlSurface::Sqlite,
            SqlSurface::Network(SqlEngine::Postgres),
        ] {
            for mongo in [
                MongoSurface::Off,
                MongoSurface::Unconfigured,
                MongoSurface::On,
            ] {
                transcript.push_str(&surface_text(profile, sql, mongo));
            }
        }
    }
    check_golden("mongo_surfaces.txt", &transcript);

    // Off is byte-identical to the surface without the MongoDB tool, for
    // every SQL state, and the lean profile never changes.
    for profile in [McpToolProfile::Full, McpToolProfile::Lean] {
        for edits in [false, true] {
            for sql in [
                SqlSurface::Off,
                SqlSurface::Unconfigured,
                SqlSurface::Sqlite,
                SqlSurface::Network(SqlEngine::MySql),
            ] {
                assert_eq!(
                    advertised_surface_with_databases(
                        profile,
                        false,
                        edits,
                        sql,
                        MongoSurface::Off
                    ),
                    advertised_surface_for(profile, false, edits, sql)
                );
            }
        }
        for mongo in [MongoSurface::Unconfigured, MongoSurface::On] {
            assert_eq!(
                advertised_surface_with_databases(
                    McpToolProfile::Lean,
                    true,
                    true,
                    SqlSurface::Off,
                    mongo
                ),
                compresso_mcp::advertised_surface_with(McpToolProfile::Lean, true, true)
            );
        }
    }
}

#[test]
fn test_the_mongo_tool_definition_stays_small() {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    for mongo in [MongoSurface::Unconfigured, MongoSurface::On] {
        let (tools, _) = advertised_surface_with_databases(
            McpToolProfile::Full,
            false,
            false,
            SqlSurface::Off,
            mongo,
        );
        let tool = tools
            .iter()
            .find(|tool| tool.name == "compresso_mongo")
            .expect("mongo tool");
        let tokens = counter.count(&serde_json::to_string(tool).expect("json"));
        println!("compresso_mongo definition ({mongo:?}): {tokens} o200k tokens");
        assert!(tokens <= 600, "{mongo:?}: {tokens} tokens");
    }
}
