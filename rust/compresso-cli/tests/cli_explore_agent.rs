//! E17-28: the opt-in Claude Code exploration agent definition (D93).
//!
//! Every path here is under a scratch home and project directory.
#![cfg(unix)]
// Integration tests of a bounded-local crate may start processes (the
// capability matrix); the workspace clippy.toml disallows them elsewhere.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, IntegrationEnvironment, RootCommand, execute_integrations, execute_setup,
};
use compresso_integrations::EXPLORE_AGENT_NAME;
use compresso_store::Store;
use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    home: PathBuf,
    root: PathBuf,
    store: Store,
    environment: IntegrationEnvironment,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let base = fs::canonicalize(directory.path()).expect("canonical temporary directory");
        let home = base.join("home");
        let root = base.join("repo");
        let data = base.join("data");
        fs::create_dir(&home).expect("home");
        fs::create_dir_all(root.join("src")).expect("repository");
        fs::write(root.join("src/lib.rs"), "pub fn indexed() {}\n").expect("source");
        let executable = std::env::current_exe()
            .and_then(fs::canonicalize)
            .expect("test executable");
        let environment = IntegrationEnvironment::new(
            home.clone(),
            root.clone(),
            executable,
            data.clone(),
            Vec::new(),
            "explore-agent-cli-test",
        );
        let store = Store::open(&data.join("compresso.db")).expect("store");
        Self {
            _directory: directory,
            home,
            root,
            store,
            environment,
        }
    }

    fn agent_path(&self) -> PathBuf {
        self.root
            .join(".claude/agents")
            .join(format!("{EXPLORE_AGENT_NAME}.md"))
    }

    fn integrations(&self, arguments: &[&str]) -> (CliExitCode, Value, String) {
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
        (
            exit,
            serde_json::from_slice(&output).unwrap_or(Value::Null),
            String::from_utf8(errors).expect("UTF-8 stderr"),
        )
    }
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path.strip_prefix(root).expect("relative").to_path_buf());
            }
        }
    }
    files.sort();
    files
}

#[test]
fn test_setup_and_default_installs_never_create_the_agent() {
    let fixture = Fixture::new();
    let cli = Cli::try_parse_from([
        "compresso",
        "setup",
        "claude-code",
        fixture.root.to_str().expect("UTF-8 root"),
        "--yes",
        "--json",
    ])
    .expect("setup parses");
    let RootCommand::Setup(arguments) = cli.command else {
        panic!("expected setup");
    };
    let exit = execute_setup(
        arguments,
        Some(&fixture.store),
        &fixture.environment,
        &mut Cursor::new(Vec::new()),
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_eq!(exit, CliExitCode::Success);
    let (exit, _, errors) = fixture.integrations(&["install", "claude-code", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--project",
        fixture.root.to_str().expect("UTF-8 root"),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(!fixture.root.join(".claude/agents").exists());
    assert!(!fixture.home.join(".claude/agents").exists());
}

#[test]
fn test_agent_dry_run_previews_and_writes_nothing() {
    let fixture = Fixture::new();
    let before = files_under(&fixture.root);
    let (exit, output, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let diff = output.to_string();
    assert!(diff.contains(EXPLORE_AGENT_NAME), "{diff}");
    assert!(diff.contains("agent_definition"), "{diff}");
    assert_eq!(files_under(&fixture.root), before);
    assert!(!fixture.root.join(".claude").exists());
    assert!(files_under(&fixture.home).is_empty());
}

#[test]
fn test_agent_install_status_verify_and_uninstall_round_trip() {
    let fixture = Fixture::new();
    let before = files_under(&fixture.root);

    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let agent = fs::read_to_string(fixture.agent_path()).expect("agent written");
    let tools = agent
        .lines()
        .find_map(|line| line.strip_prefix("tools: "))
        .expect("tools line")
        .split(", ")
        .collect::<Vec<_>>();
    // Without --edits the agent gets neither operator-enabled tool.
    let expected = compresso_mcp::lean_tool_allowlist()
        .into_iter()
        .filter(|name| !matches!(*name, "compresso_recall" | "compresso_edit"))
        .map(|name| format!("mcp__compresso-lean__{name}"))
        .chain(["Read", "Edit", "Write"].map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(tools, expected, "agent tools follow the lean allowlist");

    let (exit, status, _) =
        fixture.integrations(&["status", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(status["data"]["surface"], "explore_agent");
    assert_eq!(status["data"]["agent_installed"], true);
    assert_eq!(status["data"]["canonical_receipt_active"], true);
    let (exit, _, errors) =
        fixture.integrations(&["verify", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");

    let (exit, _, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(!fixture.agent_path().exists());
    assert_eq!(files_under(&fixture.root), before, "no file is left behind");
    let (_, status, _) =
        fixture.integrations(&["status", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(status["data"]["agent_installed"], false);
    assert_eq!(status["data"]["canonical_receipt_active"], false);
}

#[test]
fn test_agent_is_claude_code_only_and_exclusive_with_other_surfaces() {
    let fixture = Fixture::new();
    for client in ["codex", "cursor"] {
        let (exit, _, errors) =
            fixture.integrations(&["install", client, "--explore-agent", "--yes", "--json"]);
        assert_eq!(exit, CliExitCode::Failure);
        assert!(errors.contains("unsupported"), "{errors}");
    }
    for conflicting in ["--compaction-hook", "--no-recall"] {
        assert!(
            Cli::try_parse_from([
                "compresso",
                "integrations",
                "install",
                "claude-code",
                "--explore-agent",
                conflicting,
            ])
            .is_err()
        );
    }
    assert!(!fixture.root.join(".claude").exists());
}

#[test]
fn test_agent_install_refuses_a_user_agent_of_the_same_name() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".claude/agents")).expect("agents directory");
    fs::write(
        fixture.agent_path(),
        "---\nname: compresso-explorer\n---\nmine\n",
    )
    .expect("user agent");
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Failure);
    assert!(errors.contains("user_file_present"), "{errors}");
    assert_eq!(
        fs::read_to_string(fixture.agent_path()).expect("unchanged"),
        "---\nname: compresso-explorer\n---\nmine\n"
    );
}

#[test]
fn test_edits_opt_in_grants_the_edit_tool_and_round_trips() {
    let fixture = Fixture::new();
    let before = files_under(&fixture.root);
    let (exit, preview, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--edits",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(preview.to_string().contains("compresso_edit"));
    assert_eq!(files_under(&fixture.root), before, "dry run writes nothing");

    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--edits",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let agent = fs::read_to_string(fixture.agent_path()).expect("agent written");
    assert!(agent.contains("mcp__compresso-lean__compresso_edit"));
    assert!(agent.contains("not shell text substitution"));
    let (exit, _, errors) =
        fixture.integrations(&["verify", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let (exit, _, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(!fixture.agent_path().exists());

    // The server entry: --edits precedes the recall pair, and the entry
    // verifies and uninstalls like the default one.
    let (exit, _, errors) =
        fixture.integrations(&["install", "claude-code", "--edits", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let config: Value = serde_json::from_str(
        &fs::read_to_string(fixture.home.join(".claude.json")).expect("user config"),
    )
    .expect("JSON config");
    let args = config["mcpServers"]["compresso"]["args"]
        .as_array()
        .expect("args")
        .iter()
        .map(|argument| argument.as_str().expect("string"))
        .collect::<Vec<_>>();
    assert_eq!(
        &args[args.len() - 3..],
        ["--edits", "--recall-source", "claude-code"]
    );
    // Live client evidence is unavailable here; the entry shape is what an
    // --edits entry could break.
    let (_, verify, _) = fixture.integrations(&["verify", "claude-code", "--json"]);
    let server_entry = verify["data"]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "server_entry")
        .expect("server_entry check");
    assert_eq!(server_entry["passed"], true, "{verify}");
    let (exit, _, errors) = fixture.integrations(&["uninstall", "claude-code", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let config: Value = serde_json::from_str(
        &fs::read_to_string(fixture.home.join(".claude.json")).expect("user config"),
    )
    .expect("JSON config");
    assert!(config["mcpServers"].get("compresso").is_none());

    // Without --edits nothing about the entry changes.
    let (exit, _, errors) = fixture.integrations(&["install", "claude-code", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let config = fs::read_to_string(fixture.home.join(".claude.json")).expect("user config");
    assert!(!config.contains("--edits"));
}

#[test]
fn test_edits_is_install_only_and_claude_code_only() {
    let fixture = Fixture::new();
    let (exit, _, errors) =
        fixture.integrations(&["install", "codex", "--edits", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Failure);
    assert!(errors.contains("unsupported"), "{errors}");
    let (exit, _, errors) =
        fixture.integrations(&["uninstall", "claude-code", "--edits", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Failure);
    assert!(errors.contains("invalid_arguments"), "{errors}");
    assert!(
        Cli::try_parse_from([
            "compresso",
            "integrations",
            "install",
            "claude-code",
            "--edits",
            "--compaction-hook",
        ])
        .is_err()
    );
}

fn entry_args(fixture: &Fixture) -> Vec<String> {
    let config: Value = serde_json::from_str(
        &fs::read_to_string(fixture.home.join(".claude.json")).expect("user config"),
    )
    .expect("JSON config");
    config["mcpServers"]["compresso"]["args"]
        .as_array()
        .map(|args| {
            args.iter()
                .map(|argument| argument.as_str().expect("string").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn test_edits_can_be_turned_on_and_off_over_an_existing_install() {
    let fixture = Fixture::new();
    let install = |extra: &[&str]| {
        let mut arguments = vec!["install", "claude-code"];
        arguments.extend_from_slice(extra);
        arguments.extend(["--yes", "--json"]);
        let (exit, _, errors) = fixture.integrations(&arguments);
        assert_eq!(exit, CliExitCode::Success, "{extra:?}: {errors}");
    };
    install(&[]);
    assert!(!entry_args(&fixture).contains(&"--edits".to_owned()));
    install(&["--edits"]);
    assert!(
        entry_args(&fixture).contains(&"--edits".to_owned()),
        "turned on"
    );
    install(&[]);
    assert!(
        !entry_args(&fixture).contains(&"--edits".to_owned()),
        "turned off"
    );
    install(&["--edits"]);
    let (exit, _, errors) = fixture.integrations(&["uninstall", "claude-code", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(entry_args(&fixture).is_empty(), "uninstalled");

    // The agent: plain, then with edits over it, then uninstall.
    install(&["--explore-agent"]);
    install(&["--explore-agent", "--edits"]);
    let agent = fs::read_to_string(fixture.agent_path()).expect("agent");
    assert!(agent.contains("mcp__compresso-lean__compresso_edit"));
    install(&["--explore-agent"]);
    let agent = fs::read_to_string(fixture.agent_path()).expect("agent");
    assert!(!agent.contains("compresso_edit"));
    let (exit, _, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(!fixture.agent_path().exists());
}

#[test]
fn test_setup_rerun_keeps_an_installed_edits_entry() {
    let fixture = Fixture::new();
    let (exit, _, errors) =
        fixture.integrations(&["install", "claude-code", "--edits", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let cli = Cli::try_parse_from([
        "compresso",
        "setup",
        "claude-code",
        fixture.root.to_str().expect("UTF-8 root"),
        "--yes",
        "--json",
    ])
    .expect("setup parses");
    let RootCommand::Setup(arguments) = cli.command else {
        panic!("expected setup");
    };
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let exit = execute_setup(
        arguments,
        Some(&fixture.store),
        &fixture.environment,
        &mut Cursor::new(Vec::new()),
        &mut output,
        &mut errors,
    );
    // setup accepts the existing installer-written --edits entry and leaves
    // it as it is: re-running setup neither strips nor reports drift. Only an
    // explicit `integrations install` without --edits turns the tool off.
    assert_eq!(
        exit,
        CliExitCode::Success,
        "{}",
        String::from_utf8_lossy(&errors)
    );
    assert_eq!(
        entry_args(&fixture),
        ["mcp", "serve", "--edits", "--recall-source", "claude-code"]
    );
}

/// E17-38 (D103): an agent install warns that Claude Code starts the
/// agent's own server only in a trusted folder, unless the client already
/// records that trust; status reports the agent's server and the trust.
#[test]
fn test_agent_install_warns_about_folder_trust_until_it_is_recorded() {
    let fixture = Fixture::new();
    let install = || {
        let (exit, output, errors) = fixture.integrations(&[
            "install",
            "claude-code",
            "--explore-agent",
            "--yes",
            "--json",
        ]);
        assert_eq!(exit, CliExitCode::Success, "{errors}");
        output["data"]["warnings"].to_string()
    };
    assert!(install().contains("explore_agent_needs_folder_trust"));
    let (_, status, _) =
        fixture.integrations(&["status", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(status["data"]["agent_server"], "lean");
    assert_eq!(status["data"]["folder_trust"], "not_recorded");
    assert!(
        status["data"]["advice"]
            .as_str()
            .is_some_and(|advice| advice.starts_with("open Claude Code once interactively in this project folder and accept the folder trust prompt")),
        "{status}"
    );

    let record = serde_json::json!({ "projects": {
        fixture.root.to_str().expect("UTF-8"): { "hasTrustDialogAccepted": true }
    }});
    fs::write(fixture.home.join(".claude.json"), record.to_string()).expect("trust record");
    assert!(!install().contains("explore_agent_needs_folder_trust"));
    let (_, status, _) =
        fixture.integrations(&["status", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(status["data"]["folder_trust"], "accepted");
    assert!(status["data"].get("advice").is_none(), "{status}");
}

/// E17-38 (D103): an agent file written before E17-38 (granted the shared
/// full-profile server's tools) is reported with the reinstall command.
#[test]
fn test_status_reports_an_agent_file_written_for_the_shared_server() {
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let current = fs::read_to_string(fixture.agent_path()).expect("agent");
    let (head, body) = current.split_once("mcpServers:").expect("server block");
    let (_, rest) = body.split_once("\n---\n").expect("frontmatter end");
    let old = format!("{head}---\n{rest}").replace("mcp__compresso-lean__", "mcp__compresso__");
    fs::write(fixture.agent_path(), old).expect("old agent");
    let (_, status, _) =
        fixture.integrations(&["status", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(status["data"]["agent_installed"], false);
    assert_eq!(status["data"]["agent_server"], "shared_full_profile");
    assert!(
        status["data"]["advice"]
            .as_str()
            .is_some_and(|advice| advice.contains("install claude-code --explore-agent")),
        "{status}"
    );
    let (exit, _, _) =
        fixture.integrations(&["verify", "claude-code", "--explore-agent", "--json"]);
    assert_ne!(exit, CliExitCode::Success);
}

/// E17-38 (D103, review): status names the command the agent file runs
/// and warns when it is not this Compresso launcher.
#[test]
fn test_status_names_the_agent_command_and_warns_on_another_launcher() {
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let launcher = fixture
        .environment
        .compresso_bin
        .to_str()
        .expect("UTF-8")
        .to_owned();
    let (_, status, _) =
        fixture.integrations(&["status", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(status["data"]["agent_command"], launcher.as_str());
    let text = fs::read_to_string(fixture.agent_path()).expect("agent");
    fs::write(
        fixture.agent_path(),
        text.replace(
            &serde_json::to_string(&launcher).expect("json"),
            "\"/usr/local/bin/compresso\"",
        ),
    )
    .expect("other launcher");
    let (_, status, _) =
        fixture.integrations(&["status", "claude-code", "--explore-agent", "--json"]);
    assert_eq!(status["data"]["agent_command"], "/usr/local/bin/compresso");
    assert!(
        status["data"]["advice"]
            .as_str()
            .is_some_and(|advice| advice.contains("not this Compresso launcher")),
        "{status}"
    );
}

/// Runs an integrations command and returns its raw standard output.
fn integrations_raw(fixture: &Fixture, arguments: &[&str]) -> Vec<u8> {
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
    let _ = execute_integrations(
        command,
        &fixture.environment,
        &mut Cursor::new(Vec::new()),
        &mut output,
        &mut Vec::new(),
    );
    output
}

/// No raw C0 (except line feed), DEL, or C1 control reaches the output.
fn assert_no_raw_controls(bytes: &[u8]) {
    let text = String::from_utf8(bytes.to_vec()).expect("UTF-8 output");
    for character in text.chars() {
        assert!(
            character == '\n' || !character.is_control(),
            "raw control {character:?} in {text:?}"
        );
    }
}

/// E17-38 (D103, security review): an agent file is repository content, so
/// a command holding terminal escapes never reaches the terminal raw; JSON
/// still carries the original value.
#[test]
fn test_agent_command_with_control_characters_is_escaped_in_output() {
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let launcher = fixture
        .environment
        .compresso_bin
        .to_str()
        .expect("UTF-8")
        .to_owned();
    let hostile = "/opt/\u{1b}[2J\r\u{9b}31mcompresso";
    let text = fs::read_to_string(fixture.agent_path()).expect("agent");
    fs::write(
        fixture.agent_path(),
        text.replace(
            &serde_json::to_string(&launcher).expect("json"),
            &serde_json::to_string(hostile).expect("json"),
        ),
    )
    .expect("hostile agent");
    for arguments in [
        &["status", "claude-code", "--explore-agent"][..],
        &["verify", "claude-code", "--explore-agent"][..],
        &["status", "claude-code", "--explore-agent", "--json"][..],
        &["verify", "claude-code", "--explore-agent", "--json"][..],
    ] {
        let output = integrations_raw(&fixture, arguments);
        assert_no_raw_controls(&output);
        if arguments.contains(&"--json") {
            let value: Value = serde_json::from_slice(&output).expect("JSON output");
            if arguments[0] == "status" {
                assert_eq!(value["data"]["agent_command"], hostile);
                assert!(
                    value["data"]["advice"]
                        .as_str()
                        .is_some_and(|advice| advice.contains("not this Compresso launcher")),
                    "{value}"
                );
            }
        } else if arguments[0] == "status" {
            let text = String::from_utf8(output).expect("UTF-8");
            assert!(text.contains("agent command: /opt/"), "{text}");
        }
    }
}

/// E17-38 (D103, review): a FIFO in place of the client's configuration
/// never blocks `status`; the trust is reported as `unknown`.
#[test]
fn test_status_never_blocks_on_a_fifo_client_config() {
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--explore-agent",
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let status = std::process::Command::new("mkfifo")
        .arg(fixture.home.join(".claude.json"))
        .status()
        .expect("mkfifo runs");
    assert!(status.success());
    let (sender, receiver) = std::sync::mpsc::channel();
    let environment = fixture.environment.clone();
    std::thread::spawn(move || {
        let cli = Cli::try_parse_from([
            "compresso",
            "integrations",
            "status",
            "claude-code",
            "--explore-agent",
            "--json",
        ])
        .expect("parses");
        let RootCommand::Integrations { command } = cli.command else {
            panic!("expected integration command");
        };
        let mut output = Vec::new();
        let _ = execute_integrations(
            command,
            &environment,
            &mut Cursor::new(Vec::new()),
            &mut output,
            &mut Vec::new(),
        );
        let _ = sender.send(output);
    });
    let output = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("status does not block on a FIFO");
    let value: Value = serde_json::from_slice(&output).expect("JSON");
    assert_eq!(value["data"]["folder_trust"], "unknown", "{value}");
}
