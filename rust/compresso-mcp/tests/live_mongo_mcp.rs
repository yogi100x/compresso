//! E17-52 (D117): the MongoDB tool through the MCP path against a live
//! server, for what the token fit does with a document that does not fit.
//! Returns at once unless `COMPRESSO_MONGO_TEST_HOST` is set (with
//! `COMPRESSO_SQL_TEST_REQUIRE_LIVE=1` a missing variable fails). It reads
//! the `wide` collection and the `reader` account that
//! `compresso-sql`'s `live_mongo` tests create; run those first.

use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{
    McpDeps, McpServeOptions, McpToolProfile, MongoSettings, serve_io_with_options,
};
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const ENV: &str = "COMPRESSO_MONGO_LIVE";

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

fn host() -> Option<String> {
    let value = std::env::var("COMPRESSO_MONGO_TEST_HOST")
        .ok()
        .filter(|value| !value.is_empty());
    if value.is_none()
        && std::env::var("COMPRESSO_SQL_TEST_REQUIRE_LIVE").is_ok_and(|value| value == "1")
    {
        panic!("COMPRESSO_MONGO_TEST_HOST is not set but COMPRESSO_SQL_TEST_REQUIRE_LIVE=1");
    }
    value
}

#[tokio::test]
async fn test_live_mcp_a_document_that_does_not_fit_is_cut_not_dropped() {
    let Some(host) = host() else { return };
    let url = format!(
        "mongodb://reader:{}@{host}/compresso_probe",
        concat!("pr0be", "-Pa55")
    );
    let fixture = Fixture::new();
    let mut harness = Harness::start(
        &fixture,
        fixture.options(McpToolProfile::Full, Some(Some(&url))),
    )
    .await;
    let counter = counter_for(TokenProvider::OpenAiO200k);
    for max_tokens in [300, 400] {
        let (text, is_error) = harness
            .call(
                "compresso_mongo",
                &json!({"action": "find", "collection": "wide", "filter": {}, "max_tokens": max_tokens}),
            )
            .await;
        assert!(!is_error, "{text}");
        assert!(
            counter.count(&text) <= max_tokens,
            "{}",
            counter.count(&text)
        );
        let value: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(value["count"], 1, "{text}");
        assert_eq!(value["documents"][0]["$truncated"], true, "{text}");
        assert_eq!(value["truncation_reason"], "max_tokens");
    }
    harness.close().await;
}
