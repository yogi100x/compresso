//! MCP server health reporting through `compresso doctor` and
//! `compresso integrations status`, exercised with stub server executables.
//!
//! Client configuration is untrusted: these tests also prove that a command
//! Compresso did not install, or that is not the running executable, is never
//! started.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
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
here="$(dirname "$0")"
echo ran >> "$here/ran"
env > "$here/env"
pwd > "$here/cwd"
while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"compresso","version":"0.0.0"}}}' ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"compresso_context"},{"name":"compresso_read"}]}}' ;;
  esac
done
"#;
const HANGING_STUB: &str = "#!/bin/sh\nexec sleep 30\n";
const HANGING_TREE_STUB: &str = r#"#!/bin/sh
sleep 30 &
echo $! > "$(dirname "$0")/child.pid"
wait
"#;
const HANGS_AFTER_INITIALIZE_STUB: &str = r#"#!/bin/sh
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"compresso","version":"0.0.0"}}}'
exec sleep 30
"#;
const CRASHING_STUB: &str = "#!/bin/sh\necho ran >> \"$(dirname \"$0\")/ran\"\nexit 3\n";
const GARBAGE_STUB: &str = "#!/bin/sh\necho 'this is not json-rpc'\nexec sleep 30\n";
const FOREIGN_TOOLS_STUB: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"other","version":"1"}}}' ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"unrelated_tool"}]}}' ;;
  esac
done
"#;
/// An attacker-chosen command: creates a marker if it is ever executed.
const CANARY: &str = "#!/bin/sh\necho executed > \"$(dirname \"$0\")/canary-ran\"\nexit 0\n";

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
    stub: PathBuf,
    canary: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = fs::canonicalize(directory.path()).expect("temp directory should resolve");
        let home = root.join("home");
        let project = root.join("project");
        for path in [&home, &project, &root.join("bin"), &root.join("evil")] {
            fs::create_dir_all(path).expect("fixture directory");
        }
        let fixture = Self {
            _directory: directory,
            stub: root.join("bin/compresso"),
            canary: root.join("evil/compresso"),
            root,
            home,
            project,
        };
        write_executable(&fixture.canary, CANARY);
        fixture
    }

    /// Installs `script` as the Compresso launcher through the real install
    /// flow, so the entry is covered by an authenticated receipt.
    fn with_installed_stub(script: &str) -> Self {
        let fixture = Self::new();
        write_executable(&fixture.stub, script);
        fixture.install("claude-code");
        fixture
    }

    fn install(&self, client: &str) {
        let (exit, output) = self.integrations(&["install", client, "--yes", "--json"], None);
        assert_eq!(exit, CliExitCode::Success, "{output}");
    }

    fn configure_claude(&self, entry: Value) {
        write_json(
            &self.home.join(".claude.json"),
            &json!({ "mcpServers": { "compresso": entry } }),
        );
    }

    fn stub_runs(&self) -> usize {
        fs::read_to_string(self.stub.with_file_name("ran"))
            .map(|runs| runs.lines().count())
            .unwrap_or(0)
    }

    fn canary_ran(&self) -> bool {
        self.canary.with_file_name("canary-ran").exists()
    }

    fn diagnostics(
        &self,
        running_executable: Option<&Path>,
        probe_timeout: Option<Duration>,
    ) -> DiagnosticEnvironment {
        let data_dir = self.root.join("data");
        DiagnosticEnvironment {
            config_path: self.root.join("config.toml"),
            database_path: data_dir.join("compresso.db"),
            data_dir,
            executable_path: running_executable
                .map(Path::to_path_buf)
                .unwrap_or_else(|| std::env::current_exe().expect("test executable")),
            environment: BTreeMap::new(),
            home_dir: Some(self.home.clone()),
            mcp_probe_timeout: probe_timeout,
        }
    }

    fn doctor(&self, probe_timeout: Option<Duration>) -> (CliExitCode, Value) {
        self.doctor_as(None, probe_timeout)
    }

    fn doctor_as(
        &self,
        running_executable: Option<&Path>,
        probe_timeout: Option<Duration>,
    ) -> (CliExitCode, Value) {
        let cli = Cli::try_parse_from(["compresso", "doctor", "--json"]).expect("doctor parses");
        let mut output = Vec::new();
        let exit = execute_diagnostics(
            cli,
            &UnavailableAccountClient,
            &self.diagnostics(running_executable, probe_timeout),
            &mut output,
            &mut Vec::new(),
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
            &self.diagnostics(None, probe_timeout),
            &mut output,
            &mut Vec::new(),
        );
        String::from_utf8(output).expect("doctor output should be UTF-8")
    }

    fn integrations(
        &self,
        arguments: &[&str],
        probe_timeout: Option<Duration>,
    ) -> (CliExitCode, String) {
        let cli = Cli::try_parse_from(
            ["compresso", "integrations"]
                .into_iter()
                .chain(arguments.iter().copied()),
        )
        .expect("integration command parses");
        let RootCommand::Integrations { command } = cli.command else {
            panic!("expected integration command");
        };
        let mut environment = IntegrationEnvironment::new(
            self.home.clone(),
            self.home.clone(),
            self.stub.clone(),
            self.root.join("data"),
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
            String::from_utf8(output).expect("integration output should be UTF-8"),
        )
    }

    fn status(&self, arguments: &[&str], probe_timeout: Option<Duration>) -> Value {
        let (exit, output) = self.integrations(
            &[&["status"], arguments, &["--json"]].concat(),
            probe_timeout,
        );
        assert_eq!(exit, CliExitCode::Success, "{output}");
        serde_json::from_str::<Value>(&output).expect("status JSON")["data"]["mcp_server"].clone()
    }
}

fn write_executable(path: &Path, script: &str) {
    fs::write(path, script).expect("stub written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("stub executable");
}

fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().expect("parent")).expect("parent created");
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("config should serialize"),
    )
    .expect("config written");
}

fn mcp_check(payload: &Value) -> &Value {
    payload["data"]["checks"]
        .as_array()
        .expect("checks should be an array")
        .iter()
        .find(|check| check["name"] == "mcp_server")
        .expect("doctor should report an mcp_server check")
}

fn server<'a>(payload: &'a Value, client: &str) -> &'a Value {
    payload["data"]["mcp_servers"]
        .as_array()
        .expect("mcp_servers should be an array")
        .iter()
        .find(|server| server["client"] == client)
        .expect("client server should be reported")
}

fn assert_failure(script: &str, timeout: Duration, state: &str, fix_fragment: &str) -> Fixture {
    let fixture = Fixture::with_installed_stub(script);
    let started = Instant::now();

    let (exit, payload) = fixture.doctor(Some(timeout));

    assert!(started.elapsed() < BOUNDED, "probe must be bounded");
    assert_eq!(exit, CliExitCode::Failure);
    let check = mcp_check(&payload);
    assert_eq!(check["passed"], false, "{check}");
    assert_eq!(check["severity"], "error", "{check}");
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
    let server = server(&payload, "claude-code");
    assert_eq!(server["state"], state);
    assert_eq!(server["healthy"], false);
    assert_eq!(server["severity"], "error");
    fixture
}

/// Asserts that `doctor` passed with the MCP check downgraded to a warning
/// that still carries the fix text for `client`.
fn assert_doctor_warns(exit: CliExitCode, payload: &Value, client: &str) {
    assert_eq!(exit, CliExitCode::Success, "{payload}");
    let check = mcp_check(payload);
    assert_eq!(check["passed"], true, "{check}");
    assert_eq!(check["severity"], "warning", "{check}");
    assert!(
        check["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains(&format!("compresso integrations install {client}"))),
        "{check}"
    );
}

fn assert_not_probed(server: &Value, client: &str) {
    assert_eq!(server["state"], "not_probed", "{server}");
    assert_eq!(server["healthy"], false);
    assert_eq!(server["severity"], "warning", "{server}");
    assert!(
        server["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains(&format!("compresso integrations install {client}"))),
        "{server}"
    );
}

#[test]
fn test_doctor_reports_healthy_for_receipt_installed_server() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);

    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    assert_eq!(mcp_check(&payload)["passed"], true);
    assert_eq!(mcp_check(&payload)["severity"], "ok");
    let server = server(&payload, "claude-code");
    assert_eq!(server["state"], "healthy");
    assert!(
        server["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("2 tools"))
    );
    assert_eq!(fixture.stub_runs(), 1);
}

#[test]
fn test_trusted_running_executable_is_probed_without_a_receipt() {
    let fixture = Fixture::new();
    write_executable(&fixture.stub, HEALTHY_STUB);
    let link = fixture.root.join("bin/compresso-link");
    std::os::unix::fs::symlink(&fixture.stub, &link).expect("symlink");
    fixture.configure_claude(json!({
        "type": "stdio", "command": link, "args": ["mcp", "serve"]
    }));

    let (exit, payload) = fixture.doctor_as(Some(&fixture.stub), Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    assert_eq!(server(&payload, "claude-code")["state"], "healthy");
    assert_eq!(fixture.stub_runs(), 1);
}

#[test]
fn test_unreceipted_command_that_is_not_the_running_executable_is_never_executed() {
    let fixture = Fixture::new();
    write_executable(&fixture.stub, HEALTHY_STUB);
    fixture.configure_claude(json!({
        "type": "stdio", "command": fixture.canary, "args": ["mcp", "serve"]
    }));

    for timeout in [Some(PROBE_TIMEOUT), None] {
        let (exit, payload) = fixture.doctor_as(Some(&fixture.stub), timeout);
        assert_doctor_warns(exit, &payload, "claude-code");
        assert_not_probed(server(&payload, "claude-code"), "claude-code");
    }
    assert!(!fixture.canary_ran(), "untrusted command must not run");
    assert_eq!(fixture.stub_runs(), 0);
}

#[test]
fn test_project_config_pointing_at_canary_is_never_executed() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);
    let project = fixture.project.to_str().expect("project path");
    let entry = json!({
        "command": fixture.canary,
        "args": ["mcp", "serve", "--root", project]
    });
    let mut claude_entry = entry.clone();
    claude_entry["type"] = json!("stdio");
    write_json(
        &fixture.project.join(".mcp.json"),
        &json!({ "mcpServers": { "compresso": claude_entry } }),
    );
    write_json(
        &fixture.project.join(".cursor/mcp.json"),
        &json!({ "mcpServers": { "compresso": entry } }),
    );

    for client in ["claude-code", "cursor"] {
        let server = fixture.status(&[client, "--project", project], Some(PROBE_TIMEOUT));
        assert_not_probed(&server, client);
    }
    assert!(
        !fixture.canary_ran(),
        "repository-supplied command must not run"
    );
    assert_eq!(fixture.stub_runs(), 0);
}

#[test]
fn test_symlink_to_a_non_compresso_binary_is_rejected() {
    let fixture = Fixture::new();
    write_executable(&fixture.stub, HEALTHY_STUB);
    // The link sits beside the trusted launcher and borrows its name, but
    // resolves to the attacker's binary.
    let link = fixture.root.join("bin/compresso-trusted");
    std::os::unix::fs::symlink(&fixture.canary, &link).expect("symlink");
    fixture.configure_claude(json!({
        "type": "stdio", "command": link, "args": ["mcp", "serve"]
    }));

    let (exit, payload) = fixture.doctor_as(Some(&fixture.stub), Some(PROBE_TIMEOUT));

    assert_doctor_warns(exit, &payload, "claude-code");
    assert_not_probed(server(&payload, "claude-code"), "claude-code");
    assert!(!fixture.canary_ran());
}

#[test]
fn test_receipt_does_not_trust_a_command_swapped_after_install() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);
    fixture.configure_claude(json!({
        "type": "stdio", "command": fixture.canary, "args": ["mcp", "serve"]
    }));

    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));
    assert_doctor_warns(exit, &payload, "claude-code");
    assert_not_probed(server(&payload, "claude-code"), "claude-code");

    let status = fixture.status(&["claude-code"], Some(PROBE_TIMEOUT));
    assert_not_probed(&status, "claude-code");
    assert!(!fixture.canary_ran());
    assert_eq!(fixture.stub_runs(), 0);
}

#[test]
fn test_modified_args_or_fields_are_rejected_without_execution() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);
    for entry in [
        json!({ "type": "stdio", "command": fixture.stub, "args": ["mcp", "serve", "--extra"] }),
        json!({ "type": "stdio", "command": fixture.stub, "args": ["-c", "anything"] }),
        json!({ "type": "stdio", "command": "compresso", "args": ["mcp", "serve"] }),
        json!({ "command": fixture.stub, "args": ["mcp", "serve"], "env": { "A": "B" } }),
    ] {
        fixture.configure_claude(entry);
        // Even the running executable is not started with unexpected input.
        let (exit, payload) = fixture.doctor_as(Some(&fixture.stub), Some(PROBE_TIMEOUT));
        assert_doctor_warns(exit, &payload, "claude-code");
        let server = server(&payload, "claude-code");
        assert_eq!(server["state"], "config_modified");
        assert_eq!(server["healthy"], false);
        assert_eq!(server["severity"], "warning");
        assert!(
            server["fix"]
                .as_str()
                .is_some_and(|fix| fix.contains("compresso integrations install claude-code"))
        );
    }
    assert_eq!(fixture.stub_runs(), 0, "modified entries must not run");
}

#[test]
fn test_trusted_launch_scrubs_environment_and_uses_neutral_directory() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);

    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    let environment =
        fs::read_to_string(fixture.stub.with_file_name("env")).expect("recorded environment");
    assert!(environment.lines().any(|line| line.starts_with("PATH=")));
    // Cargo exports CARGO_* to this test process; none may reach the server.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    assert!(
        !environment.contains("CARGO_"),
        "unlisted variables leaked: {environment}"
    );
    let cwd = fs::read_to_string(fixture.stub.with_file_name("cwd")).expect("recorded directory");
    assert_eq!(cwd.trim(), "/");
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
fn test_timeout_kills_the_server_process_tree() {
    let fixture = assert_failure(
        HANGING_TREE_STUB,
        Duration::from_secs(8),
        "handshake_timeout",
        "Restart the coding client",
    );
    let pid = fs::read_to_string(fixture.stub.with_file_name("child.pid"))
        .expect("stub should record its child");
    let alive = || {
        Command::new("kill")
            .args(["-0", pid.trim()])
            .output()
            .is_ok_and(|output| output.status.success())
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(), "descendant {pid} outlived the probe");
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
        server(&payload, "claude-code")["detail"]
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
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);
    fs::remove_file(&fixture.stub).expect("stub removed");
    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));
    assert_eq!(exit, CliExitCode::Failure);
    let stale = server(&payload, "claude-code");
    assert_eq!(stale["state"], "binary_missing");
    assert!(
        stale["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("compresso integrations install claude-code"))
    );

    fs::write(&fixture.stub, HEALTHY_STUB).expect("non-executable stub");
    fs::set_permissions(&fixture.stub, fs::Permissions::from_mode(0o644)).expect("mode");
    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));
    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(
        server(&payload, "claude-code")["state"],
        "binary_not_executable"
    );
    assert_eq!(fixture.stub_runs(), 0);
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
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);

    let (exit, payload) = fixture.doctor(None);

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    assert_eq!(server(&payload, "claude-code")["state"], "probe_skipped");
    assert_eq!(fixture.stub_runs(), 0);
}

#[test]
fn test_doctor_probes_a_shared_launcher_once_and_reports_every_client() {
    let fixture = Fixture::with_installed_stub(CRASHING_STUB);
    fixture.install("codex");

    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Failure);
    let servers = payload["data"]["mcp_servers"]
        .as_array()
        .expect("mcp_servers");
    assert_eq!(servers.len(), 2);
    assert!(
        servers
            .iter()
            .all(|server| server["state"] == "exited_early"),
        "{payload}"
    );
    assert!(
        server(&payload, "codex")["fix"]
            .as_str()
            .is_some_and(|fix| !fix.contains("claude-code"))
    );
    assert_eq!(fixture.stub_runs(), 1);
}

#[test]
fn test_doctor_human_output_shows_failure_and_fix_text() {
    let fixture = Fixture::with_installed_stub(HANGING_STUB);

    let human = fixture.doctor_human(Some(SHORT_TIMEOUT));

    assert!(human.contains("[FAIL] mcp_server"), "{human}");
    assert!(human.contains("claude-code: handshake_timeout"), "{human}");
    assert!(
        human.contains("hint: claude-code: Restart the coding client"),
        "{human}"
    );
}

#[test]
fn test_doctor_human_output_shows_warning_and_fix_text_for_hand_edited_entry() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);
    fixture.configure_claude(json!({
        "type": "stdio", "command": fixture.stub, "args": ["mcp", "serve", "--extra"]
    }));

    let human = fixture.doctor_human(Some(PROBE_TIMEOUT));

    assert!(human.contains("[WARN] mcp_server"), "{human}");
    assert!(!human.contains("[FAIL]"), "{human}");
    assert!(human.contains("claude-code: config_modified"), "{human}");
    assert!(
        human.contains(
            "hint: claude-code: Restore the entry with `compresso integrations install claude-code`"
        ),
        "{human}"
    );
    assert_eq!(fixture.stub_runs(), 0, "modified entries must not run");
}

#[test]
fn test_doctor_fails_on_real_failure_and_keeps_warning_fix_text() {
    let fixture = Fixture::with_installed_stub(CRASHING_STUB);
    fixture.install("codex");
    // Claude's entry is swapped by hand; Codex still starts the crashing stub.
    fixture.configure_claude(json!({
        "type": "stdio", "command": fixture.canary, "args": ["mcp", "serve"]
    }));

    let (exit, payload) = fixture.doctor(Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Failure, "{payload}");
    let check = mcp_check(&payload);
    assert_eq!(check["passed"], false, "{check}");
    assert_eq!(check["severity"], "error", "{check}");
    let hint = check["hint"].as_str().expect("hint text");
    assert!(hint.contains("codex: Run `"), "{hint}");
    assert!(
        hint.contains("claude-code: If you trust this configuration"),
        "{hint}"
    );
    assert_not_probed(server(&payload, "claude-code"), "claude-code");
    assert_eq!(server(&payload, "codex")["state"], "exited_early");
    assert_eq!(server(&payload, "codex")["severity"], "error");
    assert!(!fixture.canary_ran(), "untrusted command must not run");
}

#[test]
fn test_integration_status_shows_fix_text_for_unverifiable_entry() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);
    fixture.configure_claude(json!({
        "type": "stdio", "command": fixture.canary, "args": ["mcp", "serve"]
    }));

    let (exit, human) = fixture.integrations(&["status", "claude-code"], Some(PROBE_TIMEOUT));

    assert_eq!(exit, CliExitCode::Success, "{human}");
    assert!(human.contains("mcp server: not_probed"), "{human}");
    assert!(
        human.contains("warning: If you trust this configuration"),
        "{human}"
    );
    assert!(!fixture.canary_ran(), "untrusted command must not run");
}

#[test]
fn test_doctor_reports_healthy_for_real_compresso_server() {
    let fixture = Fixture::new();
    let real = fs::canonicalize(env!("CARGO_BIN_EXE_compresso")).expect("compresso binary");
    let state = fixture.root.join("server-state");
    fs::create_dir_all(&state).expect("server state");
    // The wrapper is installed through the real flow, so it is the launcher
    // the receipt authenticates; it pins the server to scratch directories.
    write_executable(
        &fixture.stub,
        &format!(
            "#!/bin/sh\nexport HOME='{0}' XDG_DATA_HOME='{0}/data' XDG_CONFIG_HOME='{0}/config'\nexec '{1}' \"$@\"\n",
            state.display(),
            real.display()
        ),
    );
    fixture.install("claude-code");

    let (exit, payload) = fixture.doctor(Some(Duration::from_secs(60)));

    assert_eq!(exit, CliExitCode::Success, "{payload}");
    assert_eq!(server(&payload, "claude-code")["state"], "healthy");
}

#[test]
fn test_integration_status_reports_unresponsive_server_with_fix_text() {
    let fixture = Fixture::with_installed_stub(HANGING_STUB);
    let started = Instant::now();

    let server = fixture.status(&["claude-code"], Some(SHORT_TIMEOUT));

    assert!(started.elapsed() < BOUNDED, "probe must be bounded");
    assert_eq!(server["state"], "handshake_timeout");
    assert_eq!(server["healthy"], false);
    assert!(
        server["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("Restart the coding client"))
    );

    let (_, human) = fixture.integrations(&["status", "claude-code"], Some(SHORT_TIMEOUT));
    assert!(human.contains("mcp server: handshake_timeout"), "{human}");
    assert!(human.contains("fix: Restart the coding client"), "{human}");
}

#[test]
fn test_integration_status_reports_healthy_and_stale_servers() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);
    let server = fixture.status(&["claude-code"], Some(PROBE_TIMEOUT));
    assert_eq!(server["state"], "healthy");
    assert_eq!(server["healthy"], true);
    assert_eq!(server["severity"], "ok");

    fs::remove_file(&fixture.stub).expect("stub removed");
    let server = fixture.status(&["claude-code"], Some(PROBE_TIMEOUT));
    assert_eq!(server["state"], "binary_missing");
    assert_eq!(server["healthy"], false);
    assert_eq!(server["severity"], "error");
}

#[test]
fn test_integration_status_default_environment_starts_no_process() {
    let fixture = Fixture::with_installed_stub(HEALTHY_STUB);

    let server = fixture.status(&["claude-code"], None);

    assert_eq!(server["state"], "probe_skipped");
    assert_eq!(fixture.stub_runs(), 0);
}
