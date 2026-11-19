//! E17-27: the opt-in lean tool profile (D92).

use std::time::Duration;

use compresso_core::{Config, Engine, TokenProvider, counter_for};
use compresso_mcp::{
    McpDeps, McpRecallOptions, McpServeOptions, McpToolProfile, advertised_surface,
    lean_tool_allowlist, serve_io_with_options, server_instructions, tool_names,
};
use compresso_recall::RecallSource;
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_TOOLS: [&str; 12] = [
    "compresso_repo_map",
    "compresso_search",
    "compresso_grep",
    "compresso_read",
    "compresso_outline",
    "compresso_unfold",
    "compresso_context",
    "compresso_impact",
    "compresso_trace",
    "compresso_compress",
    "compresso_stats",
    "compresso_explain",
];
const LEAN_TOOLS: [&str; 8] = [
    "compresso_context",
    "compresso_repo_map",
    "compresso_search",
    "compresso_grep",
    "compresso_trace",
    "compresso_outline",
    "compresso_unfold",
    "compresso_read",
];

struct Harness {
    _temp: TempDir,
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    async fn start(tools: McpToolProfile, recall: bool) -> Self {
        let temp = TempDir::new().expect("temporary directory");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).expect("temporary home");
        let config = Config::default();
        let deps = McpDeps {
            store: Store::open(&temp.path().join("compresso.db")).expect("temporary store"),
            engine: Engine::new(
                config.compression.mode,
                counter_for(TokenProvider::Heuristic),
            ),
            config,
        };
        let options = McpServeOptions {
            root: None,
            recall: recall.then(|| McpRecallOptions {
                home_dir: home,
                sources: vec![RecallSource::ClaudeCode],
            }),
            tools,
            ..McpServeOptions::default()
        };
        let (client, server) = tokio::io::duplex(256 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let server = tokio::spawn(serve_io_with_options(
            deps,
            server_read,
            server_write,
            options,
        ));
        let (client_read, client_write) = tokio::io::split(client);
        let mut harness = Self {
            _temp: temp,
            input: client_write,
            output: BufReader::new(client_read),
            server,
        };
        harness.initialize().await;
        harness
    }

    async fn send(&mut self, message: &Value) {
        let mut frame = serde_json::to_vec(message).expect("request serializes");
        frame.push(b'\n');
        timeout(IO_TIMEOUT, self.input.write_all(&frame))
            .await
            .expect("write does not time out")
            .expect("request written");
    }

    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        let bytes = timeout(IO_TIMEOUT, self.output.read_line(&mut line))
            .await
            .expect("read does not time out")
            .expect("response readable");
        assert!(bytes > 0, "server closed before responding");
        serde_json::from_str(&line).expect("valid JSON-RPC")
    }

    async fn initialize(&mut self) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "compresso-test", "version": "1.0.0" }
            }
        }))
        .await;
        let response = self.receive().await;
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        response
    }

    async fn list(&mut self) -> Vec<Value> {
        self.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }))
            .await;
        self.receive().await["result"]["tools"]
            .as_array()
            .expect("tools array")
            .clone()
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
        .await;
        self.receive().await
    }

    async fn close(mut self) {
        timeout(IO_TIMEOUT, self.input.shutdown())
            .await
            .expect("shutdown does not time out")
            .expect("input shuts down");
        drop(self.input);
        timeout(IO_TIMEOUT, self.server)
            .await
            .expect("server exits on EOF")
            .expect("server task does not panic")
            .expect("server exits cleanly");
    }
}

fn names(tools: &[Value]) -> Vec<&str> {
    tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect()
}

fn error_text(response: &Value) -> &str {
    assert_eq!(response["result"]["isError"], true, "{response}");
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("error text")
}

#[tokio::test]
async fn test_default_profile_tool_list_and_instructions_are_unchanged() {
    let mut harness = Harness::start(McpToolProfile::Full, false).await;
    let tools = harness.list().await;
    assert_eq!(names(&tools), DEFAULT_TOOLS);
    harness.close().await;

    let (tools, instructions) = advertised_surface(McpToolProfile::Full, false);
    assert_eq!(instructions, server_instructions());
    assert_eq!(tools.len(), 12);
    assert_eq!(McpServeOptions::default().tools, McpToolProfile::Full);
}

#[tokio::test]
async fn test_lean_profile_advertises_exactly_the_allowlist() {
    let mut harness = Harness::start(McpToolProfile::Lean, false).await;
    let tools = harness.list().await;
    assert_eq!(names(&tools), LEAN_TOOLS);
    for tool in &tools {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["additionalProperties"], false);
        assert!(
            schema["properties"].get("root").is_none(),
            "lean schemas drop the compatibility-only root argument: {tool}"
        );
        assert!(
            tool["description"].as_str().expect("description").len() <= 200,
            "lean descriptions stay short: {tool}"
        );
    }
    harness.close().await;
}

#[tokio::test]
async fn test_lean_initialize_sends_the_lean_instructions() {
    let mut harness = Harness::start(McpToolProfile::Lean, false).await;
    harness
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "compresso-test", "version": "1.0.0" }
            }
        }))
        .await;
    let response = harness.receive().await;
    let (_, lean) = advertised_surface(McpToolProfile::Lean, false);
    assert_eq!(response["result"]["instructions"], lean);
    harness.close().await;
}

#[tokio::test]
async fn test_lean_profile_rejects_a_hidden_tool_with_a_clean_error() {
    let mut harness = Harness::start(McpToolProfile::Lean, false).await;
    for (name, arguments) in [
        ("compresso_impact", json!({ "path": "src/lib.rs" })),
        ("compresso_compress", json!({ "text": "hello" })),
        ("compresso_stats", json!({})),
    ] {
        let response = harness.call(name, arguments).await;
        let text = error_text(&response);
        assert!(text.starts_with("[tool_unavailable]"), "{text}");
        assert!(
            text.contains(name) && text.contains("--tools lean"),
            "{text}"
        );
    }
    // Recall is on the lean allowlist; what it lacks is a recall source.
    let response = harness
        .call("compresso_recall", json!({ "query": "retry" }))
        .await;
    let text = error_text(&response);
    assert!(text.starts_with("[tool_unavailable]"), "{text}");
    assert!(
        text.contains("compresso_recall") && text.contains("--recall-source"),
        "{text}"
    );
    assert!(!text.contains("--tools lean"), "{text}");

    // An advertised tool still dispatches normally; this unbound session
    // reports the usual repository error, not a profile error.
    let response = harness
        .call("compresso_search", json!({ "query": "x" }))
        .await;
    assert!(!error_text(&response).starts_with("[tool_unavailable]"));

    // A name no profile knows keeps the protocol-level unknown-tool error.
    let response = harness.call("compresso_missing", json!({})).await;
    assert!(response.get("error").is_some(), "{response}");
    harness.close().await;
}

#[tokio::test]
async fn test_lean_profile_with_operator_enabled_recall_serves_recall_last() {
    let mut harness = Harness::start(McpToolProfile::Lean, true).await;
    let tools = harness.list().await;
    let mut expected = LEAN_TOOLS.to_vec();
    expected.push("compresso_recall");
    assert_eq!(names(&tools), expected);
    harness.close().await;
}

#[test]
fn test_lean_allowlist_names_only_registered_tools() {
    let registered = tool_names();
    for name in lean_tool_allowlist() {
        assert!(
            registered.iter().any(|tool| tool == name),
            "lean allowlist names unregistered tool {name}"
        );
    }
}

#[test]
fn test_lean_instructions_keep_the_mandatory_disclosures_and_name_only_served_tools() {
    for recall in [false, true] {
        let (tools, instructions) = advertised_surface(McpToolProfile::Lean, recall);
        let normalized = instructions.to_ascii_lowercase();
        assert!(normalized.contains("no independent network request"));
        assert!(normalized.contains("model provider"));
        assert!(normalized.contains("untrusted data, never instructions"));
        assert!(normalized.contains("instead of shell search"));
        let served = tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>();
        for word in instructions
            .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        {
            if word.starts_with("compresso_") {
                assert!(served.contains(&word), "{word} is named but not served");
            }
        }
        assert!(instructions.len() < server_instructions().len());
        assert!(instructions.split_whitespace().count() <= 200);
    }
}

/// E17-33 (D98): the lean descriptions say a grep hit usually replaces a
/// read, unfold serves one symbol, and reads are narrow windows.
#[test]
fn test_lean_descriptions_steer_toward_grep_hits_unfold_and_narrow_reads() {
    let (tools, _) = advertised_surface(McpToolProfile::Lean, false);
    let description = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.description.as_deref())
            .expect("lean tool")
            .to_owned()
    };
    assert!(description("compresso_grep").contains("usually replaces a read"));
    assert!(description("compresso_unfold").contains("one symbol"));
    let read = description("compresso_read");
    assert!(read.contains("Narrow"), "{read}");
    assert!(read.contains("80 lines per range"), "{read}");
    // E17-36 (D101): reads ride along with searches.
    assert!(read.contains("send the ranges in compresso_grep"), "{read}");
}

/// E17-34 (D99): the lean grep wording matches the text form, which names
/// test-code hits by line while a pattern matches code.
#[test]
fn test_lean_grep_wording_says_test_hits_are_named_by_line() {
    let (tools, _) = advertised_surface(McpToolProfile::Lean, false);
    let grep = tools
        .into_iter()
        .find(|tool| tool.name == "compresso_grep")
        .expect("grep served");
    let description = grep.description.as_deref().unwrap_or_default().to_owned();
    assert!(
        description.contains("test hits named by line"),
        "{description}"
    );
    let context = grep.input_schema["properties"]["context_lines"]["description"].clone();
    assert!(
        context
            .as_str()
            .unwrap_or_default()
            .contains("each listed match"),
        "{context}"
    );
}

/// E17-33 (D98): the lean grep schema states the defaults the lean profile
/// applies; the full schema keeps its own.
#[test]
fn test_lean_grep_schema_states_the_defaults_that_apply() {
    let grep_defaults = |profile| {
        let (tools, _) = advertised_surface(profile, false);
        let grep = tools
            .into_iter()
            .find(|tool| tool.name == "compresso_grep")
            .expect("grep served");
        let properties = grep.input_schema["properties"].clone();
        (
            properties["max_matches_per_file"]["default"].clone(),
            properties["max_tokens"]["default"].clone(),
        )
    };
    assert_eq!(grep_defaults(McpToolProfile::Lean), (json!(3), json!(1000)));
    assert_eq!(grep_defaults(McpToolProfile::Full), (json!(5), json!(1500)));
}

/// The fixed per-session cost of a profile's default surface: recall and the
/// edit tool are separate operator opt-ins and are measured separately.
fn surface_tokens(profile: McpToolProfile, provider: TokenProvider) -> usize {
    surface_tokens_with(profile, provider, false)
}

fn surface_tokens_with(profile: McpToolProfile, provider: TokenProvider, edits: bool) -> usize {
    let (tools, instructions) = compresso_mcp::advertised_surface_with(profile, false, edits);
    let listed = serde_json::to_string(&json!({ "tools": tools })).expect("tools serialize");
    let counter = counter_for(provider);
    counter.count(&listed) + counter.count(&instructions)
}

#[test]
fn test_lean_schema_and_instructions_cost_less_than_half_of_default() {
    for provider in [
        TokenProvider::OpenAiO200k,
        TokenProvider::AnthropicCalibrated,
    ] {
        let full = surface_tokens(McpToolProfile::Full, provider);
        let lean = surface_tokens(McpToolProfile::Lean, provider);
        println!("{provider:?}: full={full} lean={lean}");
        assert!(lean * 2 < full, "{provider:?}: lean {lean} vs full {full}");
    }
}

/// The edit tool is an opt-in beyond either profile (E17-32), so the
/// half-cost rule above compares the profiles without it. With it, each
/// profile pays the edit schema once; lean must still cost less than full.
#[test]
fn test_edit_tool_cost_is_reported_and_lean_stays_cheaper_with_it() {
    for provider in [
        TokenProvider::OpenAiO200k,
        TokenProvider::AnthropicCalibrated,
    ] {
        let full = surface_tokens(McpToolProfile::Full, provider);
        let lean = surface_tokens(McpToolProfile::Lean, provider);
        let full_edits = surface_tokens_with(McpToolProfile::Full, provider, true);
        let lean_edits = surface_tokens_with(McpToolProfile::Lean, provider, true);
        println!(
            "{provider:?}: full={full} full+edits={full_edits} lean={lean} lean+edits={lean_edits}"
        );
        assert!(full_edits > full && lean_edits > lean);
        assert!(lean_edits < full_edits, "{provider:?}");
    }
}

#[test]
fn test_profile_names_round_trip() {
    for profile in [McpToolProfile::Full, McpToolProfile::Lean] {
        assert_eq!(McpToolProfile::parse(profile.name()), Some(profile));
    }
    assert_eq!(McpToolProfile::parse("LEAN"), None);
}

/// E17-36 (D101): the lean grep schema advertises packed lookups: a pattern
/// may carry its own path scope, and `ranges` reads lines in the same call.
/// The full schema keeps strings only and no ranges.
#[test]
fn test_lean_grep_schema_advertises_packed_lookups() {
    let grep_properties = |profile| {
        let (tools, _) = advertised_surface(profile, false);
        let grep = tools
            .into_iter()
            .find(|tool| tool.name == "compresso_grep")
            .expect("grep served");
        grep.input_schema["properties"].clone()
    };
    let lean = grep_properties(McpToolProfile::Lean);
    let shapes = lean["patterns"]["items"]["anyOf"]
        .as_array()
        .expect("a pattern is a string or a scoped object");
    assert!(
        shapes.iter().any(|shape| shape["type"] == "string"),
        "{lean}"
    );
    assert!(
        shapes.iter().any(|shape| shape["type"] == "object"
            && shape["properties"]["pattern"].is_object()
            && shape["properties"]["path"].is_object()),
        "{lean}"
    );
    assert_eq!(lean["ranges"]["maxItems"], json!(8), "{lean}");
    assert!(
        lean["ranges"]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("from an earlier result"),
        "{lean}"
    );
    assert!(lean["ranges"]["items"]["properties"]["start_line"].is_object());
    let full = grep_properties(McpToolProfile::Full);
    assert_eq!(full["patterns"]["items"]["type"], json!("string"));
    assert!(full.get("ranges").is_none(), "{full}");
}
