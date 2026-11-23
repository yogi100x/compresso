//! E17-48: the Claude Code in-session command skill (D113).
//!
//! Every path here is under a scratch home and project directory.
#![cfg(unix)]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use clap::Parser;
use compresso_cli::{Cli, CliExitCode, IntegrationEnvironment, RootCommand, execute_integrations};
use compresso_integrations::SessionCommandsIntegration;
use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    home: PathBuf,
    root: PathBuf,
    environment: IntegrationEnvironment,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let base = fs::canonicalize(directory.path()).expect("canonical temporary directory");
        let home = base.join("home");
        let root = base.join("repo");
        fs::create_dir(&home).expect("home");
        fs::create_dir_all(root.join("src")).expect("repository");
        let executable = std::env::current_exe()
            .and_then(fs::canonicalize)
            .expect("test executable");
        let environment = IntegrationEnvironment::new(
            home.clone(),
            root.clone(),
            executable,
            base.join("data"),
            Vec::new(),
            "session-commands-cli-test",
        );
        Self {
            _directory: directory,
            home,
            root,
            environment,
        }
    }

    fn root_arg(&self) -> &str {
        self.root.to_str().expect("UTF-8 root")
    }

    fn skill_path(&self) -> PathBuf {
        self.root
            .join(".claude")
            .join("skills")
            .join("compresso")
            .join("SKILL.md")
    }

    fn entry_path(&self) -> PathBuf {
        self.root.join(".mcp.json")
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

    fn install(&self, extra: &[&str]) -> Value {
        let mut arguments = vec!["install", "claude-code", "--project", self.root_arg()];
        arguments.extend_from_slice(extra);
        arguments.extend(["--yes", "--json"]);
        let (exit, output, errors) = self.integrations(&arguments);
        assert_eq!(exit, CliExitCode::Success, "{errors} {output}");
        output
    }
}

/// Every file under `root`, relative to it.
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

fn warnings(output: &Value) -> Vec<String> {
    output["data"]["warnings"]
        .as_array()
        .map(|warnings| {
            warnings
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn test_project_install_adds_the_session_command_skill_by_default() {
    let fixture = Fixture::new();
    let output = fixture.install(&[]);
    assert!(fixture.entry_path().is_file());
    assert_eq!(
        fs::read_to_string(fixture.skill_path()).expect("skill written"),
        SessionCommandsIntegration::render()
    );
    assert_eq!(
        output["data"]["session_commands"]["result"]["installed"], true,
        "{output}"
    );
    assert!(
        files_under(&fixture.home).is_empty(),
        "a project install writes nothing under the home directory"
    );

    let (exit, verified, errors) = fixture.integrations(&[
        "verify",
        "claude-code",
        "--session-commands",
        "--project",
        fixture.root_arg(),
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors} {verified}");
    let (exit, status, errors) = fixture.integrations(&[
        "status",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        status["data"]["session_commands_installed"], true,
        "{status}"
    );
    let (exit, status, errors) = fixture.integrations(&[
        "status",
        "claude-code",
        "--session-commands",
        "--project",
        fixture.root_arg(),
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(status["data"]["surface"], "session_commands");
    assert_eq!(status["data"]["session_commands_installed"], true);
    assert_eq!(status["data"]["canonical_receipt_active"], true);
}

#[test]
fn test_no_session_commands_leaves_the_skill_out() {
    let fixture = Fixture::new();
    let output = fixture.install(&["--no-session-commands"]);
    assert!(fixture.entry_path().is_file());
    assert!(!fixture.root.join(".claude/skills").exists());
    assert!(output["data"].get("session_commands").is_none(), "{output}");
}

#[test]
fn test_user_scope_install_writes_no_skill_and_says_why() {
    let fixture = Fixture::new();
    let (exit, output, errors) =
        fixture.integrations(&["install", "claude-code", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(
        warnings(&output)
            .iter()
            .any(|warning| warning.starts_with("session_commands_not_installed:")),
        "{output}"
    );
    assert!(!fixture.home.join(".claude").exists());
    assert!(!fixture.root.join(".claude").exists());
    let (exit, _, _) = fixture.integrations(&[
        "install",
        "claude-code",
        "--session-commands",
        "--project",
        fixture.home.to_str().expect("UTF-8 home"),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Failure, "the home directory is refused");
    assert!(!fixture.home.join(".claude").exists());
}

#[test]
fn test_a_user_skill_of_the_same_name_is_never_overwritten() {
    let fixture = Fixture::new();
    let path = fixture.skill_path();
    fs::create_dir_all(path.parent().expect("parent")).expect("skill directory");
    fs::write(&path, "---\nname: compresso\n---\nmine\n").expect("user skill");
    let output = fixture.install(&[]);
    assert!(
        fixture.entry_path().is_file(),
        "the MCP entry still installs"
    );
    assert!(
        warnings(&output)
            .iter()
            .any(|warning| warning.starts_with("session_commands_skipped:")),
        "{output}"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("unchanged"),
        "---\nname: compresso\n---\nmine\n"
    );
    let (exit, _, _) = fixture.integrations(&[
        "install",
        "claude-code",
        "--session-commands",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(
        fs::read_to_string(&path).expect("unchanged"),
        "---\nname: compresso\n---\nmine\n"
    );
}

#[test]
fn test_plain_uninstall_removes_the_entry_and_the_skill() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    let (exit, output, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors} {output}");
    assert!(!fixture.skill_path().exists());
    assert!(
        output["data"]["session_commands"]["result"].is_object(),
        "{output}"
    );
}

#[test]
fn test_session_commands_flag_manages_only_the_skill() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    let entry = fs::read(fixture.entry_path()).expect("entry");
    let (exit, _, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--session-commands",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(!fixture.skill_path().exists());
    assert_eq!(fs::read(fixture.entry_path()).expect("entry kept"), entry);
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--session-commands",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(fixture.skill_path().is_file());
    assert_eq!(fs::read(fixture.entry_path()).expect("entry kept"), entry);
}

#[test]
fn test_dry_run_previews_both_plans_and_writes_nothing() {
    let fixture = Fixture::new();
    let before = files_under(&fixture.root);
    let (exit, output, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--dry-run",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(
        output["data"]["session_commands"]["plan"].is_object(),
        "{output}"
    );
    assert!(output["data"]["session_commands"]["result"].is_null());
    assert_eq!(files_under(&fixture.root), before);
    assert!(files_under(&fixture.home).is_empty());
}
