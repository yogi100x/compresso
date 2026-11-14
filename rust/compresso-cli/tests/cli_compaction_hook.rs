//! Post-compaction tool-routing reminder: `mcp routing-reminder` and
//! the `--compaction-hook` integration surface.
#![cfg(unix)]

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, IntegrationEnvironment, RootCommand, UnavailableAccountClient, execute,
    execute_integrations,
};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
    environment: IntegrationEnvironment,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = fs::canonicalize(directory.path()).expect("temp root");
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&project).expect("project");
        let compresso = fs::canonicalize(env!("CARGO_BIN_EXE_compresso")).expect("compresso");
        let environment = IntegrationEnvironment::new(
            home.clone(),
            project.clone(),
            compresso,
            root.join("data"),
            Vec::new(),
            "compaction-hook-cli-test",
        );
        Self {
            _directory: directory,
            root,
            home,
            project,
            environment,
        }
    }

    fn run(&self, arguments: &[&str]) -> (CliExitCode, Value, Value) {
        let cli = Cli::try_parse_from(
            ["compresso", "integrations"]
                .into_iter()
                .chain(arguments.iter().copied()),
        )
        .expect("integration command parses");
        let RootCommand::Integrations { command } = cli.command else {
            panic!("expected integration command");
        };
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_integrations(
            command,
            &self.environment,
            &mut Cursor::new(Vec::new()),
            &mut output,
            &mut errors,
        );
        (exit, parse(&output), parse(&errors))
    }

    fn claude_settings(&self) -> PathBuf {
        self.home.join(".claude/settings.json")
    }

    fn snapshot(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut pending = vec![self.root.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).expect("directory").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path.clone());
                }
                paths.push(path);
            }
        }
        paths.sort();
        paths
    }
}

fn parse(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(bytes).expect("CLI output should be JSON")
    }
}

fn read(path: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("config readable")).expect("config JSON")
}

fn routing_reminder(arguments: &[&str]) -> String {
    let cli = Cli::try_parse_from(
        ["compresso", "mcp", "routing-reminder"]
            .into_iter()
            .chain(arguments.iter().copied()),
    )
    .expect("routing-reminder parses");
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let exit = execute(cli, &UnavailableAccountClient, &mut output, &mut errors);
    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_empty());
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn test_routing_reminder_prints_the_bounded_constant_text() {
    let first = routing_reminder(&[]);
    let second = routing_reminder(&[]);

    assert_eq!(first, second, "repeated emission must be byte-identical");
    assert_eq!(first, format!("{}\n", compresso_mcp::routing_reminder()));
    assert!(first.len() <= compresso_mcp::ROUTING_REMINDER_MAX_BYTES + 1);
    assert!(first.contains("compresso_context"));
}

#[test]
fn test_routing_reminder_hook_envelope_is_session_start_additional_context() {
    let output = routing_reminder(&["--session-start-hook"]);

    assert!(output.len() <= 1024, "hook output must stay small");
    let envelope: Value = serde_json::from_str(&output).expect("hook JSON");
    assert_eq!(
        envelope,
        json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": compresso_mcp::routing_reminder(),
            }
        })
    );
}

#[test]
fn test_hook_install_status_verify_and_uninstall_round_trip() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run(&[
        "install",
        "claude-code",
        "--compaction-hook",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        output["data"]["plan"]["config_path"],
        json!(fixture.claude_settings())
    );
    let settings = read(&fixture.claude_settings());
    assert_eq!(settings["hooks"]["SessionStart"][0]["matcher"], "compact");
    assert!(
        !fixture.home.join(".claude.json").exists(),
        "the hook surface must not touch the MCP configuration"
    );

    let (exit, status, _) = fixture.run(&["status", "claude-code", "--compaction-hook", "--json"]);
    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(status["data"]["surface"], "compaction_hook");
    assert_eq!(status["data"]["hook_installed"], true);
    assert_eq!(status["data"]["canonical_receipt_active"], true);

    let (exit, verify, errors) =
        fixture.run(&["verify", "claude-code", "--compaction-hook", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(verify["data"]["verified"], true);

    let (exit, _, errors) = fixture.run(&[
        "uninstall",
        "claude-code",
        "--compaction-hook",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(read(&fixture.claude_settings()), json!({ "hooks": {} }));
    let (_, status, _) = fixture.run(&["status", "claude-code", "--compaction-hook", "--json"]);
    assert_eq!(status["data"]["hook_installed"], false);
    assert_eq!(status["data"]["canonical_receipt_active"], false);
}

#[test]
fn test_hook_and_mcp_entry_have_independent_receipts() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["install", "claude-code", "--yes", "--json"],
        vec![
            "install",
            "claude-code",
            "--compaction-hook",
            "--yes",
            "--json",
        ],
        vec![
            "install",
            "claude-code",
            "--compaction-hook",
            "--yes",
            "--json",
        ],
        vec![
            "uninstall",
            "claude-code",
            "--compaction-hook",
            "--yes",
            "--json",
        ],
    ] {
        let (exit, _, errors) = fixture.run(&arguments);
        assert_eq!(exit, CliExitCode::Success, "{arguments:?}: {errors}");
    }

    let mcp = read(&fixture.home.join(".claude.json"));
    assert!(mcp["mcpServers"]["compresso"].is_object());
    let (_, status, _) = fixture.run(&["status", "claude-code", "--json"]);
    assert_eq!(status["data"]["canonical_receipt_active"], true);
    assert!(read(&fixture.claude_settings())["hooks"]["SessionStart"].is_null());
}

#[test]
fn test_codex_hook_installs_into_hooks_json_without_touching_config_toml() {
    let fixture = Fixture::new();

    let (exit, _, errors) =
        fixture.run(&["install", "codex", "--compaction-hook", "--yes", "--json"]);

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let hooks = read(&fixture.home.join(".codex/hooks.json"));
    assert_eq!(hooks["hooks"]["SessionStart"][0]["matcher"], "compact");
    assert!(!fixture.home.join(".codex/config.toml").exists());
}

#[test]
fn test_hook_dry_run_and_unsupported_clients_write_nothing() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();

    let (exit, output, _) = fixture.run(&[
        "install",
        "claude-code",
        "--compaction-hook",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(output["data"]["dry_run"], true);

    let (exit, _, errors) =
        fixture.run(&["install", "cursor", "--compaction-hook", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(errors["error"]["code"], "unsupported");
    assert!(
        errors["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("no post-compaction hook"))
    );

    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn test_existing_user_session_start_hooks_are_refused_untouched() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join(".claude")).expect("config directory");
    let existing = "{\"hooks\":{\"SessionStart\":[{\"matcher\":\"startup\",\"hooks\":[]}]}}\n";
    fs::write(fixture.claude_settings(), existing).expect("settings");

    let (exit, _, errors) = fixture.run(&[
        "install",
        "claude-code",
        "--compaction-hook",
        "--yes",
        "--json",
    ]);

    assert_eq!(exit, CliExitCode::Failure);
    assert!(
        errors["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("by hand")),
        "{errors}"
    );
    assert_eq!(
        fs::read_to_string(fixture.claude_settings()).expect("settings"),
        existing
    );
}

#[test]
fn test_installed_hook_command_emits_the_reminder_when_run_by_a_shell() {
    let fixture = Fixture::new();
    let project = fixture.project.to_str().expect("project path");
    let (exit, _, errors) = fixture.run(&[
        "install",
        "claude-code",
        "--compaction-hook",
        "--project",
        project,
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let settings = read(&fixture.project.join(".claude/settings.json"));
    let command = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .expect("hook command");

    // Clients hand the command string to a shell, with the hook event on stdin.
    let output = Command::new("/bin/sh")
        .args(["-c", command])
        .env_clear()
        .current_dir(&fixture.root)
        .output()
        .expect("hook command should run");

    assert!(output.status.success());
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("hook JSON");
    assert_eq!(
        envelope["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    assert_eq!(
        envelope["hookSpecificOutput"]["additionalContext"],
        compresso_mcp::routing_reminder()
    );
    assert_eq!(fs::read_dir(&fixture.root).expect("root").count(), 3);
}
