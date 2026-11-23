#![cfg(unix)]
// Integration tests of a bounded-local crate may start processes (the
// capability matrix); the workspace clippy.toml disallows them elsewhere.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn test_global_codex_setup_binds_each_launched_session_from_its_cwd() {
    let fixture = Fixture::new();

    fixture.setup(&fixture.root_a);
    let first_config = fs::read(&fixture.codex_config).expect("Codex config should exist");
    fixture.setup(&fixture.root_b);
    assert_eq!(
        fs::read(&fixture.codex_config).expect("Codex config should remain readable"),
        first_config,
        "approving a second repository must not pin or rewrite the global Codex entry"
    );

    let configured = configured_server(&fixture.codex_config);
    assert_eq!(
        configured.args,
        ["mcp", "serve", "--recall-source", "codex"],
        "setup enables recall over Codex transcripts only, and pins no root"
    );
    assert!(
        !configured.args.iter().any(|argument| argument == "--root"),
        "the global Codex entry must let each session bind from its own cwd"
    );

    for (root, canary) in [
        (&fixture.root_a, "visible_only_in_repo_a"),
        (&fixture.root_b, "visible_only_in_repo_b"),
    ] {
        let response = fixture.call_repo_map(&configured, root);
        let result = successful_tool_json(&response);
        assert_eq!(
            result["root"],
            fs::canonicalize(root)
                .expect("repository should canonicalize")
                .to_string_lossy()
                .as_ref()
        );
        assert!(
            result.to_string().contains(canary),
            "the MCP session should expose the repository selected by its cwd"
        );
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    home: PathBuf,
    data_home: PathBuf,
    config_home: PathBuf,
    root_a: PathBuf,
    root_b: PathBuf,
    codex_config: PathBuf,
    compresso: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let home = directory.path().join("home");
        let data_home = home.join(".local/share");
        let config_home = home.join(".config");
        let root_a = directory.path().join("repo-a");
        let root_b = directory.path().join("repo-b");
        fs::create_dir_all(root_a.join("src")).expect("repo A should be created");
        fs::create_dir_all(root_b.join("src")).expect("repo B should be created");
        fs::create_dir_all(&home).expect("home should be created");
        fs::write(
            root_a.join("src/lib.rs"),
            "pub fn visible_only_in_repo_a() {}\n",
        )
        .expect("repo A source should be written");
        fs::write(
            root_b.join("src/lib.rs"),
            "pub fn visible_only_in_repo_b() {}\n",
        )
        .expect("repo B source should be written");
        let compresso = fs::canonicalize(env!("CARGO_BIN_EXE_compresso"))
            .expect("test Compresso binary should resolve");
        let codex_config = home.join(".codex/config.toml");
        Self {
            _directory: directory,
            home,
            data_home,
            config_home,
            root_a,
            root_b,
            codex_config,
            compresso,
        }
    }

    fn setup(&self, root: &Path) {
        let output = self
            .command(&self.compresso)
            .args(["setup", "codex"])
            .arg(root)
            .args(["--yes", "--json"])
            .current_dir(root)
            .output()
            .expect("setup should execute");
        assert!(
            output.status.success(),
            "setup failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let body: Value =
            serde_json::from_slice(&output.stdout).expect("setup stdout should be JSON");
        assert_eq!(body["data"]["result"]["status"], "complete");
    }

    fn call_repo_map(&self, configured: &ConfiguredServer, cwd: &Path) -> Value {
        let mut child = self
            .command(&configured.command)
            .args(&configured.args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("configured MCP command should launch");
        let mut input = child.stdin.take().expect("MCP stdin should be piped");
        for request in [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "codex-cwd-test", "version": "1.0.0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "compresso_repo_map", "arguments": {} }
            }),
        ] {
            serde_json::to_writer(&mut input, &request).expect("request should serialize");
            input.write_all(b"\n").expect("request should be written");
        }
        drop(input);

        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if child
                .try_wait()
                .expect("MCP process status should be readable")
                .is_some()
            {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("configured MCP command did not exit after stdin closed");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child
            .wait_with_output()
            .expect("MCP process output should be collected");
        assert!(
            output.status.success(),
            "MCP process failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("MCP stdout should be UTF-8")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("response should be JSON-RPC"))
            .find(|message| message["id"] == 2)
            .expect("tools/call response should be returned")
    }

    fn command(&self, executable: &Path) -> Command {
        let mut command = Command::new(executable);
        command
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env_remove("COMPRESSO_LAUNCHER_PATH")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY");
        command
    }
}

struct ConfiguredServer {
    command: PathBuf,
    args: Vec<String>,
}

fn configured_server(path: &Path) -> ConfiguredServer {
    let document: toml::Value =
        toml::from_str(&fs::read_to_string(path).expect("Codex config should be readable UTF-8"))
            .expect("Codex config should be valid TOML");
    let server = document
        .get("mcp_servers")
        .and_then(|servers| servers.get("compresso"))
        .expect("Codex config should contain Compresso");
    let command = server
        .get("command")
        .and_then(toml::Value::as_str)
        .map(PathBuf::from)
        .expect("Compresso command should be a string");
    let args = server
        .get("args")
        .and_then(toml::Value::as_array)
        .expect("Compresso args should be an array")
        .iter()
        .map(|argument| {
            argument
                .as_str()
                .expect("Compresso argument should be a string")
                .to_owned()
        })
        .collect();
    ConfiguredServer { command, args }
}

fn successful_tool_json(response: &Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "unexpected protocol error: {response}"
    );
    assert_ne!(
        response["result"]["isError"], true,
        "unexpected tool error: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .expect("tool result should contain content");
    serde_json::from_str(
        content
            .first()
            .and_then(|entry| entry["text"].as_str())
            .expect("tool content should be text"),
    )
    .expect("tool content should be structured JSON")
}
