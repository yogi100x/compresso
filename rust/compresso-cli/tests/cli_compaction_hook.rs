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
use compresso_integrations::{
    CompactionHookClient, CompactionHookIntegration, FileReceiptStore, InstallCtx, Integration,
    PlanApproval, PlanManager, Scope,
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

    fn legacy_claude_settings(&self) -> PathBuf {
        self.project.join(".claude/settings.json")
    }

    /// Reproduces a receipted project-level hook install by an earlier version.
    fn install_legacy_project_hook(&self) {
        let adapter =
            CompactionHookIntegration::new(CompactionHookClient::ClaudeCode, self.home.clone());
        let manager = PlanManager::new("legacy-install").expect("manager");
        let receipts = FileReceiptStore::new(self.root.join("data/integration-receipts.json"));
        let plan = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: self.environment.compresso_bin.clone(),
                    scope: Scope::Project(self.project.clone()),
                    session_recall: false,
                },
            )
            .expect("legacy plan");
        adapter
            .install(&manager, &PlanApproval::from(&plan), &receipts)
            .expect("legacy install");
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
    let (exit, _, errors) = fixture.run(&[
        "install",
        "claude-code",
        "--compaction-hook",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let settings = read(&fixture.claude_settings());
    let command = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .expect("hook command");
    for repository_path in [&fixture.project, &fixture.root] {
        assert!(
            !command.contains(repository_path.join("").to_str().expect("UTF-8")),
            "the user-level hook command names no repository: {command}"
        );
    }

    // Clients hand the command string to a shell, with the hook event on
    // stdin. The hook is user-level, so it runs from whichever repository the
    // session is in and must not depend on that directory or the environment.
    let run_from = |directory: &std::path::Path| {
        Command::new("/bin/sh")
            .args(["-c", command])
            .env_clear()
            .current_dir(directory)
            .output()
            .expect("hook command should run")
    };
    let output = run_from(&fixture.root);
    let elsewhere = run_from(&fixture.project);
    let filesystem_root = run_from(std::path::Path::new("/"));
    assert!(elsewhere.status.success() && filesystem_root.status.success());
    assert_eq!(output.stdout, elsewhere.stdout);
    assert_eq!(output.stdout, filesystem_root.stdout);

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
    assert_eq!(fs::read_dir(&fixture.project).expect("project").count(), 0);
}

#[test]
fn test_hook_install_with_project_still_writes_only_user_level_settings() {
    let fixture = Fixture::new();
    let project = fixture.project.to_str().expect("project path");

    for client in ["claude-code", "codex"] {
        let (exit, output, errors) = fixture.run(&[
            "install",
            client,
            "--compaction-hook",
            "--project",
            project,
            "--yes",
            "--json",
        ]);
        assert_eq!(exit, CliExitCode::Success, "{client}: {errors}");
        assert!(
            output["data"]["plan"]["config_path"]
                .as_str()
                .is_some_and(|path| path.starts_with(fixture.home.to_str().expect("UTF-8"))),
            "{client}: {output}"
        );
        assert!(output["data"].get("legacy_project_hook").is_none());
    }

    assert_eq!(
        read(&fixture.claude_settings())["hooks"]["SessionStart"][0]["matcher"],
        "compact"
    );
    assert!(fixture.home.join(".codex/hooks.json").exists());
    assert_eq!(
        fs::read_dir(&fixture.project).expect("project").count(),
        0,
        "no hook file is ever written into a repository"
    );

    // Status and verify inspect the user-level file only.
    for action in ["status", "verify"] {
        let (exit, _, errors) = fixture.run(&[
            action,
            "claude-code",
            "--compaction-hook",
            "--project",
            project,
            "--json",
        ]);
        assert_eq!(exit, CliExitCode::Failure);
        assert_eq!(errors["error"]["code"], "invalid_arguments", "{errors}");
    }
}

#[test]
fn test_hook_install_migrates_a_receipted_legacy_project_hook() {
    let fixture = Fixture::new();
    fixture.install_legacy_project_hook();
    let legacy_before = fs::read(fixture.legacy_claude_settings()).expect("legacy settings");

    // The working directory is the repository, so no --project is needed.
    let (exit, preview, errors) = fixture.run(&[
        "install",
        "claude-code",
        "--compaction-hook",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let legacy = &preview["data"]["legacy_project_hook"];
    assert_eq!(legacy["plan"]["status"], "planned", "{preview}");
    assert_eq!(
        legacy["plan"]["config_path"],
        json!(fixture.legacy_claude_settings())
    );
    assert!(legacy["result"].is_null());
    assert_eq!(
        fs::read(fixture.legacy_claude_settings()).expect("legacy settings"),
        legacy_before,
        "dry run writes nothing"
    );
    assert!(!fixture.claude_settings().exists());

    let (exit, output, errors) = fixture.run(&[
        "install",
        "claude-code",
        "--compaction-hook",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        output["data"]["legacy_project_hook"]["result"]["status"], "removed",
        "{output}"
    );
    assert_eq!(
        read(&fixture.claude_settings())["hooks"]["SessionStart"][0]["matcher"],
        "compact"
    );
    assert_eq!(
        read(&fixture.legacy_claude_settings()),
        json!({ "hooks": {} })
    );
    let backup = output["data"]["legacy_project_hook"]["result"]["backup_path"]
        .as_str()
        .expect("fresh backup");
    assert_eq!(fs::read(backup).expect("backup"), legacy_before);
}

#[test]
fn test_legacy_project_hook_can_be_removed_explicitly_without_touching_the_user_hook() {
    let fixture = Fixture::new();
    let project = fixture.project.to_str().expect("project path");
    fixture.install_legacy_project_hook();
    fs::create_dir_all(fixture.home.join(".claude")).expect("config directory");
    fs::write(
        fixture.claude_settings(),
        "{\"hooks\":{\"Stop\":[{\"hooks\":[]}]}}\n",
    )
    .expect("user settings");
    let (exit, _, errors) = fixture.run(&[
        "install",
        "claude-code",
        "--compaction-hook",
        "--project",
        fixture.home.to_str().expect("UTF-8"),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let user = fs::read(fixture.claude_settings()).expect("user settings");

    let (exit, output, errors) = fixture.run(&[
        "uninstall",
        "claude-code",
        "--compaction-hook",
        "--project",
        project,
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        output["data"]["plan"]["config_path"],
        json!(fixture.legacy_claude_settings())
    );
    assert_eq!(
        read(&fixture.legacy_claude_settings()),
        json!({ "hooks": {} })
    );
    assert_eq!(
        fs::read(fixture.claude_settings()).expect("user settings"),
        user,
        "the user-level hook is untouched"
    );

    // Removing the user-level hook is explicit, says it is user-wide, and
    // leaves the user's other hooks in place.
    let (exit, output, errors) = fixture.run(&[
        "uninstall",
        "claude-code",
        "--compaction-hook",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(
        output["data"]["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|warning| warning.contains("every repository"))),
        "{output}"
    );
    assert_eq!(
        read(&fixture.claude_settings()),
        json!({ "hooks": { "Stop": [{ "hooks": [] }] } })
    );
}
