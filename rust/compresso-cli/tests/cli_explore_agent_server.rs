//! E17-38 (D103): the exploration agent's own lean server, end to end.
//!
//! The agent file an install writes is read back, and its inline server's
//! exact `command` and `args` are started as a real process, next to the
//! shared full-profile server, both on one scratch store. Every process runs
//! with a scratch HOME.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;

use compresso_integrations::EXPLORE_AGENT_NAME;
use serde_json::{Value, json};

struct Fixture {
    _directory: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let base = fs::canonicalize(directory.path()).expect("canonical base");
        let home = base.join("home");
        let root = base.join("repo");
        fs::create_dir(&home).expect("home");
        fs::create_dir_all(root.join("src")).expect("repository");
        fs::write(
            root.join("src/lib.rs"),
            "pub fn alpha_value() -> u32 {\n    7\n}\n\npub fn caller() -> u32 {\n    alpha_value()\n}\n",
        )
        .expect("source");
        Self {
            _directory: directory,
            home,
            root,
        }
    }

    fn compresso(&self, arguments: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_compresso"))
            .args(arguments)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .output()
            .expect("compresso runs");
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }

    fn agent_text(&self) -> String {
        fs::read_to_string(
            self.root
                .join(".claude/agents")
                .join(format!("{EXPLORE_AGENT_NAME}.md")),
        )
        .expect("agent file")
    }

    fn shared_entry(&self) -> Value {
        let config: Value = serde_json::from_str(
            &fs::read_to_string(self.root.join(".mcp.json")).expect("project config"),
        )
        .expect("JSON");
        config["mcpServers"]["compresso"].clone()
    }

    fn start(&self, command: &str, args: &[String]) -> Server {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("server starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut server = Server {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let initialized = server.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "compresso-test", "version": "1.0.0" }
            }),
        );
        assert!(initialized.get("result").is_some(), "{initialized}");
        server.notify("notifications/initialized");
        server
    }
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn notify(&mut self, method: &str) {
        writeln!(
            self.stdin,
            "{}",
            json!({ "jsonrpc": "2.0", "method": method })
        )
        .expect("write");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
        )
        .expect("write");
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).expect("read") > 0, "EOF");
            let message: Value = serde_json::from_str(&line).expect("JSON-RPC line");
            if message["id"] == id {
                return message;
            }
        }
    }

    fn tool_names(&mut self) -> Vec<String> {
        self.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect()
    }

    /// The tool result's text and whether it is a tool error.
    fn call(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let response = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let result = &response["result"];
        (
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            result["isError"].as_bool().unwrap_or(false),
        )
    }

    fn close(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

/// The inline server's `command` and `args` as written in the agent file.
fn inline_server(agent: &str) -> (String, Vec<String>) {
    let value = |key: &str| {
        agent
            .lines()
            .find_map(|line| line.strip_prefix(&format!("      {key}: ")))
            .unwrap_or_else(|| panic!("{key} line"))
            .to_owned()
    };
    (
        serde_json::from_str(&value("command")).expect("command"),
        serde_json::from_str(&value("args")).expect("args"),
    )
}

fn root_str(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

/// The agent's inline server, started exactly as written, serves the lean
/// profile with the edit tool, while the shared entry stays the full
/// profile, byte for byte what a plain install writes (D92).
#[test]
fn test_the_agent_file_starts_a_lean_server_and_the_shared_entry_stays_full() {
    let fixture = Fixture::new();
    let root = root_str(&fixture.root).to_owned();
    fixture.compresso(&["index", &root, "--json"]);
    fixture.compresso(&[
        "integrations",
        "install",
        "claude-code",
        "--project",
        &root,
        // The default main-session agent (E17-51, D116) serves the lean
        // profile from the shared entry; this test pins the opt-out shape.
        "--no-main-agent",
        "--yes",
        "--json",
    ]);
    let shared_before = fs::read(fixture.root.join(".mcp.json")).expect("shared config");
    fixture.compresso(&[
        "integrations",
        "install",
        "claude-code",
        "--explore-agent",
        "--edits",
        "--yes",
        "--json",
    ]);
    assert_eq!(
        fs::read(fixture.root.join(".mcp.json")).expect("shared config"),
        shared_before,
        "the agent install leaves the shared entry untouched"
    );
    let shared = fixture.shared_entry();
    assert!(
        !shared["args"].to_string().contains("lean"),
        "with --no-main-agent no installer writes lean into the shared entry: {shared}"
    );

    let agent = fixture.agent_text();
    let (command, args) = inline_server(&agent);
    assert_eq!(Some(command.as_str()), shared["command"].as_str());
    assert_eq!(
        args,
        [
            "mcp", "serve", "--root", &root, "--tools", "lean", "--edits"
        ]
    );
    let mut lean = fixture.start(&command, &args);
    let served = lean.tool_names();
    let granted = agent
        .lines()
        .find_map(|line| line.strip_prefix("tools: "))
        .expect("tools line")
        .split(", ")
        .filter_map(|tool| tool.strip_prefix("mcp__compresso-lean__"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        served, granted,
        "the agent is granted exactly what its server serves"
    );
    assert!(served.contains(&"compresso_edit".to_owned()));
    lean.close();

    let (shared_command, shared_args): (String, Vec<String>) = (
        shared["command"].as_str().expect("command").to_owned(),
        serde_json::from_value(shared["args"].clone()).expect("args"),
    );
    let mut full = fixture.start(&shared_command, &shared_args);
    let full_tools = full.tool_names();
    assert!(
        full_tools.contains(&"compresso_impact".to_owned()),
        "{full_tools:?}"
    );
    assert!(!full_tools.contains(&"compresso_edit".to_owned()));
    full.close();

    let status = fixture.compresso(&[
        "integrations",
        "status",
        "claude-code",
        "--explore-agent",
        "--json",
    ]);
    assert_eq!(status["data"]["agent_installed"], true, "{status}");
    assert_eq!(status["data"]["agent_server"], "lean", "{status}");
    assert_eq!(status["data"]["folder_trust"], "not_recorded", "{status}");
}

/// Two Compresso servers on one store: the agent's lean server edits (a
/// write plus an index refresh) while the shared full server searches. The
/// store is SQLite in WAL mode with a busy timeout, so neither fails.
#[test]
fn test_lean_agent_server_edits_while_the_full_server_reads_the_same_store() {
    let fixture = Fixture::new();
    let root = root_str(&fixture.root).to_owned();
    fixture.compresso(&["index", &root, "--json"]);
    let command = env!("CARGO_BIN_EXE_compresso");
    let lean_args = [
        "mcp", "serve", "--root", &root, "--tools", "lean", "--edits",
    ]
    .map(str::to_owned);
    let full_args = ["mcp", "serve", "--root", &root].map(str::to_owned);
    let mut lean = fixture.start(command, &lean_args);
    let mut full = fixture.start(command, &full_args);

    let editor = thread::spawn(move || {
        for round in 0..12 {
            let (old, new) = if round % 2 == 0 {
                ("alpha_value", "beta_value")
            } else {
                ("beta_value", "alpha_value")
            };
            let (text, is_error) = lean.call(
                "compresso_edit",
                json!({ "operations": [{ "op": "rename", "old": old, "new": new }] }),
            );
            assert!(!is_error, "edit round {round}: {text}");
        }
        lean.close();
    });
    for round in 0..40 {
        let (text, is_error) = full.call(
            "compresso_grep",
            json!({ "patterns": ["_value"], "max_tokens": 600 }),
        );
        assert!(!is_error, "grep round {round}: {text}");
        let (text, is_error) = full.call("compresso_stats", json!({}));
        assert!(!is_error, "stats round {round}: {text}");
    }
    editor.join().expect("editor thread");
    // Twelve renames end where they started, and the full server sees it.
    let (text, is_error) = full.call("compresso_grep", json!({ "patterns": ["alpha_value"] }));
    assert!(!is_error && text.contains("alpha_value"), "{text}");
    full.close();
    assert!(
        fs::read_to_string(fixture.root.join("src/lib.rs"))
            .expect("source")
            .contains("alpha_value()")
    );
}
