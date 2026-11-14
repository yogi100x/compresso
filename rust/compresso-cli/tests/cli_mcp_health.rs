//! MCP server health reporting through `compresso doctor` and
//! `compresso integrations status`, exercised with stub server executables.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, DiagnosticEnvironment, IntegrationEnvironment, RootCommand,
    UnavailableAccountClient, execute_diagnostics, execute_integrations,
};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Deadline for stubs that never answer: the verdict is a timeout no matter
/// how slowly the host starts a freshly written script.
const SHORT_TIMEOUT: Duration = Duration::from_millis(750);
/// Deadline for stubs that answer or exit. First execution of a new script
/// can take seconds on macOS, so this is generous; probes return as soon as
/// the stub responds.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling proving an unresponsive server cannot stall diagnostics.
const BOUNDED: Duration = Duration::from_secs(20);

const HEALTHY_STUB: &str = r#"#!/bin/sh
echo ran > "$(dirname "$0")/ran"
while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"compresso","version":"0.0.0"}}}' ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"compresso_context"},{"name":"compresso_read"}]}}' ;;
  esac
done
"#;
const HANGING_STUB: &str = "#!/bin/sh\nexec sleep 30\n";
const HANGS_AFTER_INITIALIZE_STUB: &str = r#"#!/bin/sh
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"compresso","version":"0.0.0"}}}'
exec sleep 30
"#;
const CRASHING_STUB: &str = "#!/bin/sh\nexit 3\n";
const GARBAGE_STUB: &str = "#!/bin/sh\necho 'this is not json-rpc'\nexec sleep 30\n";
const FOREIGN_TOOLS_STUB: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"other","version":"1"}}}' ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"unrelated_tool"}]}}' ;;
  esac
done
"#;

struct Fixture {
    directory: TempDir,
    home: PathBuf,
    stub: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = fs::canonicalize(directory.path()).expect("temp directory should resolve");
        let home = root.join("home");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(root.join("bin")).expect("bin");
        let stub = root.join("bin/compresso");
        Self {
            directory,
            home,
            stub,
        }
    }

    fn with_stub(script: &str) -> Self {
        let fixture = Self::new();
        write_executable(&fixture.stub, script);
        fixture.configure_claude(json!({
            "type": "stdio",
            "command": fixture.stub,
            "args": ["mcp", "serve"]
        }));
        fixture
    }

    fn configure_claude(&self, entry: Value) {
        fs::write(
            self.home.join(".claude.json"),
            serde_json::to_vec_pretty(&json!({ "mcpServers": { "compresso": entry } }))
                .expect("config should serialize"),
        )
        .expect("claude config");
    }

    fn stub_ran(&self) -> bool {
        self.stub.with_file_name("ran").exists()
    }

    fn diagnostics(&self, probe_timeout: Option<Duration>) -> DiagnosticEnvironment {
        let data_dir = self.directory.path().join("data");
        DiagnosticEnvironment {
            config_path: self.directory.path().join("config.toml"),
            database_path: data_dir.join("compresso.db"),
            data_dir,
            executable_path: std::env::current_exe().expect("test executable"),
            environment: BTreeMap::new(),
            home_dir: Some(self.home.clone()),
            mcp_probe_timeout: probe_timeout,
        }
    }

    fn doctor(&self, probe_timeout: Option<Duration>) -> (CliExitCode, Value) {
        let cli = Cli::try_parse_from(["compresso", "doctor", "--json"]).expect("doctor parses");
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_diagnostics(
            cli,
            &UnavailableAccountClient,
            &self.diagnostics(probe_timeout),
            &mut output,
            &mut errors,
        );
        (
            exit,
            serde_json::from_slice(&output).expect("doctor output should be JSON"),
        )
    }

    fn doctor_human(&self, probe_timeout: Option<Duration>) -> String {
        let cli = Cli::try_parse_from(["compresso", "doctor"]).expect("doctor parses");
        let mut output = Vec::new();
        let _ = execute_diagnostics(
            cli,
            &UnavailableAccountClient,
            &self.diagnostics(probe_timeout),
            &mut output,
            &mut Vec::new(),
        );
        String::from_utf8(output).expect("doctor output should be UTF-8")
    }

    fn status(&self, arguments: &[&str], probe_timeout: Option<Duration>) -> (CliExitCode, String) {
        let cli = Cli::try_parse_from(
            ["compresso", "integrations", "status", "claude-code"]
                .into_iter()
                .chain(arguments.iter().copied()),
        )
        .expect("status parses");
        let RootCommand::Integrations { command } = cli.command else {
            panic!("expected integration command");
        };
        let mut environment = IntegrationEnvironment::new(
            self.home.clone(),
            self.home.clone(),
            self.stub.clone(),
            self.directory.path().join("data"),
            Vec::new(),
            "mcp-health-test",
        );
        if let Some(timeout) = probe_timeout {
            environment = environment.with_mcp_probe_timeout(timeout);
        }
        let mut output = Vec::new();
        let exit = execute_integrations(
            command,
            &environment,
            &mut Cursor::new(Vec::new()),
            &mut output,
            &mut Vec::new(),
        );
        (
            exit,
            String::from_utf8(output).expect("status output should be UTF-8"),
        )
    }
}

fn write_executable(path: &Path, script: &str) {
    fs::write(path, script).expect("stub written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("stub executable");
}

fn mcp_check(payload: &Value) -> &Value {
    payload["data"]["checks"]
        .as_array()
        .expect("checks should be an array")
        .iter()
        .find(|check| check["name"] == "mcp_server")
        .expect("doctor should report an mcp_server check")
}

fn claude_server(payload: &Value) -> &Value {
    payload["data"]["mcp_servers"]
        .as_array()
        .expect("mcp_servers should be an array")
        .iter()
        .find(|server| server["client"] == "claude-code")
        .expect("claude-code server should be reported")
}

fn assert_failure(script: &str, timeout: Duration, state: &str, fix_fragment: &str) -> Fixture {
    let fixture = Fixture::with_stub(script);
    let started = Instant::now();

    let (exit, payload) = fixture.doctor(Some(timeout));

    assert!(started.elapsed() < BOUNDED, "probe must be bounded");
    assert_eq!(exit, CliExitCode::Failure);
    let check = mcp_check(&payload);
    assert_eq!(check["passed"], false, "{check}");
    assert!(
        check["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(state)),
        "{check}"
    );
    assert!(
        check["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains(fix_fragment)),
        "{check}"
    );
    let server = claude_server(&payload);
    assert_eq!(server["state"], state);
    assert_eq!(server["healthy"], false);
    fixture
}

#[test]
fn test_doctor_reports_healthy_for_responsive_server() {
    let fixture = Fixture::with_stub(HEALTHY_STUB);

    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    let check = mcp_check(&payload);
    assert_eq!(check["passed"], true);
    let server = claude_server(&payload);
    assert_eq!(server["state"], "healthy");
    assert!(
        server["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("2 tools"))
    );
    assert!(fixture.stub_ran());
}

#[test]
fn test_doctor_reports_handshake_timeout_for_unresponsive_server() {
    assert_failure(
        HANGING_STUB,
        SHORT_TIMEOUT,
        "handshake_timeout",
        "Restart the coding client",
    );
}

#[test]
fn test_doctor_reports_timeout_when_server_stops_responding_after_initialize() {
    assert_failure(
        HANGS_AFTER_INITIALIZE_STUB,
        SHORT_TIMEOUT,
        "handshake_timeout",
        "Restart the coding client",
    );
}

#[test]
fn test_doctor_reports_crash_with_exit_status() {
    let fixture = assert_failure(CRASHING_STUB, PROBE_TIMEOUT, "exited_early", "mcp serve");
    let (_, payload) = fixture.doctor(Some(PROBE_TIMEOUT));
    assert!(
        claude_server(&payload)["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("exit status 3"))
    );
}

#[test]
fn test_doctor_reports_malformed_output_and_foreign_tool_list() {
    assert_failure(
        GARBAGE_STUB,
        PROBE_TIMEOUT,
        "malformed_response",
        "Reinstall Compresso",
    );
    assert_failure(
        FOREIGN_TOOLS_STUB,
        PROBE_TIMEOUT,
        "malformed_response",
        "Reinstall Compresso",
    );
}

#[test]
fn test_doctor_reports_stale_and_non_executable_launcher_paths() {
    let fixture = Fixture::with_stub(HEALTHY_STUB);
    fs::remove_file(&fixture.stub).expect("stub removed");
    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));
    assert_eq!(exit, CliExitCode::Failure);
    let server = claude_server(&payload);
    assert_eq!(server["state"], "binary_missing");
    assert!(
        server["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("compresso integrations install claude-code"))
    );

    fs::write(&fixture.stub, HEALTHY_STUB).expect("non-executable stub");
    fs::set_permissions(&fixture.stub, fs::Permissions::from_mode(0o644)).expect("mode");
    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));
    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(claude_server(&payload)["state"], "binary_not_executable");
    assert!(!fixture.stub_ran());
}

#[test]
fn test_doctor_never_launches_a_hand_edited_entry() {
    let fixture = Fixture::new();
    write_executable(&fixture.stub, HEALTHY_STUB);
    for entry in [
        json!({ "type": "stdio", "command": fixture.stub, "args": ["mcp", "serve", "--extra"] }),
        json!({ "type": "stdio", "command": "compresso", "args": ["mcp", "serve"] }),
        json!({ "command": fixture.stub, "args": ["mcp", "serve"], "env": { "A": "B" } }),
    ] {
        fixture.configure_claude(entry);
        let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));
        assert_eq!(exit, CliExitCode::Failure);
        let server = claude_server(&payload);
        assert_eq!(server["state"], "entry_modified");
        assert!(
            server["fix"]
                .as_str()
                .is_some_and(|fix| fix.contains("compresso integrations install claude-code"))
        );
        assert!(!fixture.stub_ran(), "modified entries must not be executed");
    }
}

#[test]
fn test_doctor_passes_when_no_client_is_configured() {
    let fixture = Fixture::new();

    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    let check = mcp_check(&payload);
    assert_eq!(check["passed"], true);
    assert!(
        check["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("compresso setup"))
    );
    assert_eq!(payload["data"]["mcp_servers"], json!([]));
}

#[test]
fn test_doctor_without_probe_timeout_checks_paths_but_starts_nothing() {
    let fixture = Fixture::with_stub(HEALTHY_STUB);

    let (exit, payload) = fixture.doctor(None);

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    assert_eq!(claude_server(&payload)["state"], "probe_skipped");
    assert!(!fixture.stub_ran());
}

#[test]
fn test_doctor_probes_a_shared_launcher_once_and_reports_every_client() {
    let fixture = Fixture::with_stub(CRASHING_STUB);
    fs::create_dir_all(fixture.home.join(".codex")).expect("codex dir");
    fs::write(
        fixture.home.join(".codex/config.toml"),
        format!(
            "[mcp_servers.compresso]\ncommand = \"{}\"\nargs = [\"mcp\", \"serve\"]\n",
            fixture.stub.display()
        ),
    )
    .expect("codex config");

    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Failure);
    let servers = payload["data"]["mcp_servers"]
        .as_array()
        .expect("mcp_servers");
    assert_eq!(servers.len(), 2);
    assert!(
        servers
            .iter()
            .all(|server| server["state"] == "exited_early")
    );
    assert!(servers.iter().any(|server| server["client"] == "codex"));
}

#[test]
fn test_doctor_human_output_shows_failure_and_fix_text() {
    let fixture = Fixture::with_stub(HANGING_STUB);

    let human = fixture.doctor_human(Some(SHORT_TIMEOUT));

    assert!(human.contains("[FAIL] mcp_server"), "{human}");
    assert!(human.contains("claude-code: handshake_timeout"), "{human}");
    assert!(
        human.contains("hint: claude-code: Restart the coding client"),
        "{human}"
    );
}

#[test]
fn test_doctor_reports_healthy_for_real_compresso_server() {
    let fixture = Fixture::new();
    let real = fs::canonicalize(env!("CARGO_BIN_EXE_compresso")).expect("compresso binary");
    let state = fixture.directory.path().join("server-state");
    fs::create_dir_all(&state).expect("server state");
    write_executable(
        &fixture.stub,
        &format!(
            "#!/bin/sh\nexport HOME='{0}' XDG_DATA_HOME='{0}/data' XDG_CONFIG_HOME='{0}/config'\nexec '{1}' \"$@\"\n",
            state.display(),
            real.display()
        ),
    );
    fixture.configure_claude(json!({
        "type": "stdio",
        "command": fixture.stub,
        "args": ["mcp", "serve"]
    }));

    let (exit, payload) = fixture.doctor(Some(Duration::from_secs(30)));

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    assert_eq!(claude_server(&payload)["state"], "healthy");
}

#[test]
fn test_integration_status_reports_unresponsive_server_with_fix_text() {
    let fixture = Fixture::with_stub(HANGING_STUB);
    let started = Instant::now();

    let (exit, output) = fixture.status(&["--json"], Some(SHORT_TIMEOUT));

    assert!(started.elapsed() < BOUNDED, "probe must be bounded");
    assert_eq!(exit, CliExitCode::Success);
    let payload: Value = serde_json::from_str(&output).expect("status JSON");
    let server = &payload["data"]["mcp_server"];
    assert_eq!(server["state"], "handshake_timeout");
    assert_eq!(server["healthy"], false);
    assert!(
        server["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("Restart the coding client"))
    );

    let (_, human) = fixture.status(&[], Some(SHORT_TIMEOUT));
    assert!(human.contains("mcp server: handshake_timeout"), "{human}");
    assert!(human.contains("fix: Restart the coding client"), "{human}");
}

#[test]
fn test_integration_status_reports_healthy_and_stale_servers() {
    let fixture = Fixture::with_stub(HEALTHY_STUB);
    let (_, output) = fixture.status(&["--json"], Some(PROBE_TIMEOUT));
    let payload: Value = serde_json::from_str(&output).expect("status JSON");
    assert_eq!(payload["data"]["mcp_server"]["state"], "healthy");
    assert_eq!(payload["data"]["mcp_server"]["healthy"], true);

    fs::remove_file(&fixture.stub).expect("stub removed");
    let (exit, output) = fixture.status(&["--json"], Some(PROBE_TIMEOUT));
    assert_eq!(exit, CliExitCode::Success);
    let payload: Value = serde_json::from_str(&output).expect("status JSON");
    assert_eq!(payload["data"]["mcp_server"]["state"], "binary_missing");
}

#[test]
fn test_integration_status_default_environment_starts_no_process() {
    let fixture = Fixture::with_stub(HEALTHY_STUB);

    let (exit, output) = fixture.status(&["--json"], None);

    assert_eq!(exit, CliExitCode::Success);
    let payload: Value = serde_json::from_str(&output).expect("status JSON");
    assert_eq!(payload["data"]["mcp_server"]["state"], "probe_skipped");
    assert!(!fixture.stub_ran());
}
