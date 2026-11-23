//! E17-51: the default main-session agent for a Claude Code project (D116).
//!
//! Every path here is under a scratch home and project directory.
#![cfg(unix)]

use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use clap::Parser;
use compresso_cli::{Cli, CliExitCode, IntegrationEnvironment, RootCommand, execute_integrations};
use compresso_integrations::MainAgentIntegration;
use serde_json::Value;
use tempfile::TempDir;

const CREATED_SETTINGS: &str = "{\n  \"agent\": \"compresso-main\"\n}\n";

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
            "main-agent-cli-test",
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

    fn agent_path(&self) -> PathBuf {
        self.root.join(".claude/agents/compresso-main.md")
    }

    fn settings_path(&self) -> PathBuf {
        self.root.join(".claude/settings.local.json")
    }

    fn entry_args(&self) -> Vec<String> {
        let config: Value =
            serde_json::from_slice(&fs::read(self.root.join(".mcp.json")).expect("entry"))
                .expect("entry JSON");
        config["mcpServers"]["compresso"]["args"]
            .as_array()
            .expect("args")
            .iter()
            .map(|value| value.as_str().expect("string").to_owned())
            .collect()
    }

    fn integrations(&self, arguments: &[&str]) -> (CliExitCode, Value, String) {
        let (exit, output, errors) = self.integrations_raw(arguments);
        (
            exit,
            serde_json::from_slice(output.as_bytes()).unwrap_or(Value::Null),
            errors,
        )
    }

    fn integrations_raw(&self, arguments: &[&str]) -> (CliExitCode, String, String) {
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
            String::from_utf8(output).expect("UTF-8 stdout"),
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

/// Every file under `root`, relative to it, with its bytes.
fn files_under(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
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
                let bytes = fs::read(&path).unwrap_or_default();
                files.push((
                    path.strip_prefix(root).expect("relative").to_path_buf(),
                    bytes,
                ));
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

fn has_lean_session(args: &[String]) -> bool {
    args.windows(3)
        .any(|window| window == ["--tools", "lean", "--session-tools"])
}

#[test]
fn test_project_install_adds_the_main_agent_and_the_lean_edit_entry_by_default() {
    let fixture = Fixture::new();
    let output = fixture.install(&[]);
    let args = fixture.entry_args();
    assert!(has_lean_session(&args), "{args:?}");
    assert!(args.iter().any(|arg| arg == "--edits"), "{args:?}");
    assert_eq!(
        fs::read_to_string(fixture.agent_path()).expect("agent file"),
        MainAgentIntegration::render(true)
    );
    assert_eq!(
        fs::read_to_string(fixture.settings_path()).expect("settings"),
        CREATED_SETTINGS
    );
    let steps = output["data"]["main_agent"].as_array().expect("steps");
    assert_eq!(
        steps
            .iter()
            .map(|step| step["step"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["main_agent", "main_agent_setting"]
    );
    assert!(steps.iter().all(|step| step["result"]["installed"] == true));
    // Nothing is written under the home directory.
    assert!(!fixture.home.join(".claude").exists());
}

#[test]
fn test_no_main_agent_keeps_the_previous_entry_and_writes_neither_file() {
    let fixture = Fixture::new();
    let output = fixture.install(&["--no-main-agent"]);
    let args = fixture.entry_args();
    assert!(!has_lean_session(&args), "{args:?}");
    assert!(!args.iter().any(|arg| arg == "--edits"), "{args:?}");
    assert!(!fixture.agent_path().exists());
    assert!(!fixture.settings_path().exists());
    assert!(output["data"].get("main_agent").is_none(), "{output}");
    // --edits still opts into the edit tool on the full profile.
    fixture.install(&["--no-main-agent", "--edits"]);
    let args = fixture.entry_args();
    assert!(!has_lean_session(&args) && args.iter().any(|arg| arg == "--edits"));
}

#[test]
fn test_no_edits_serves_no_edit_tool_and_the_agent_never_names_it() {
    let fixture = Fixture::new();
    fixture.install(&["--no-edits"]);
    let args = fixture.entry_args();
    assert!(has_lean_session(&args), "{args:?}");
    assert!(!args.iter().any(|arg| arg == "--edits"), "{args:?}");
    let agent = fs::read_to_string(fixture.agent_path()).expect("agent file");
    assert_eq!(agent, MainAgentIntegration::render(false));
    assert!(!agent.contains("compresso_edit"));
}

/// Every Compresso tool the agent body names is one its entry serves.
#[test]
fn test_the_agent_body_names_only_tools_its_entry_serves() {
    for edits in [true, false] {
        let body = MainAgentIntegration::body(edits);
        let (tools, _) = compresso_mcp::advertised_lean_session_surface(true, edits);
        let served = tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        let mut named = Vec::new();
        for (index, _) in body.match_indices("compresso_") {
            let name = body[index..]
                .chars()
                .take_while(|character| character.is_ascii_lowercase() || *character == '_')
                .collect::<String>();
            named.push(name.trim_end_matches('_').to_owned());
        }
        assert!(!named.is_empty());
        for name in named {
            assert!(
                served.contains(&name),
                "{name} is not served (edits {edits})"
            );
        }
    }
}

#[test]
fn test_user_scope_install_is_unchanged_and_says_why() {
    let fixture = Fixture::new();
    let (exit, output, errors) =
        fixture.integrations(&["install", "claude-code", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let config: Value =
        serde_json::from_slice(&fs::read(fixture.home.join(".claude.json")).expect("user config"))
            .expect("JSON");
    let args = config["mcpServers"]["compresso"]["args"].to_string();
    assert!(
        !args.contains("lean") && !args.contains("--edits"),
        "{args}"
    );
    assert!(!fixture.home.join(".claude").exists());
    assert!(!fixture.root.join(".claude").exists());
    assert!(
        warnings(&output)
            .iter()
            .any(|warning| warning.starts_with("main_agent_not_installed")),
        "{output}"
    );
}

#[test]
fn test_another_agent_value_is_kept_and_the_entry_is_written_as_without_the_agent() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".claude")).expect("claude dir");
    let original = "{\n  \"agent\": \"my-reviewer\",\n  \"model\": \"x\"\n}\n";
    fs::write(fixture.settings_path(), original).expect("settings");
    let output = fixture.install(&[]);
    assert_eq!(
        fs::read_to_string(fixture.settings_path()).expect("settings"),
        original
    );
    // Without the prompt the lean profile is not the measured setup.
    assert!(!fixture.agent_path().exists());
    let args = fixture.entry_args();
    assert!(!has_lean_session(&args) && !args.iter().any(|arg| arg == "--edits"));
    assert!(
        warnings(&output).iter().any(|warning| {
            warning.starts_with("main_agent_setting_skipped")
                && warning.contains("my-reviewer")
                && warning.contains("full profile")
                && warning.contains("install again")
        }),
        "{output}"
    );
}

#[test]
fn test_unparseable_settings_refuse_the_install_before_any_write() {
    for original in ["{\"model\": ", "[\"agent\"]"] {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.root.join(".claude")).expect("claude dir");
        fs::write(fixture.settings_path(), original).expect("settings");
        let before = files_under(&fixture.root);
        let (exit, _, errors) = fixture.integrations(&[
            "install",
            "claude-code",
            "--project",
            fixture.root_arg(),
            "--yes",
            "--json",
        ]);
        assert_eq!(exit, CliExitCode::Failure, "{original}");
        assert!(errors.contains("settings.local.json"), "{errors}");
        assert!(errors.contains("--no-main-agent"), "{errors}");
        assert_eq!(files_under(&fixture.root), before, "{original}");
    }
}

#[test]
fn test_a_symbolic_link_on_the_agent_path_refuses_before_any_write() {
    let fixture = Fixture::new();
    let elsewhere = fixture.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, fixture.root.join(".claude")).expect("link");
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Failure, "{errors}");
    assert!(!fixture.root.join(".mcp.json").exists());
    assert!(
        fs::read_dir(&elsewhere)
            .expect("elsewhere")
            .next()
            .is_none()
    );
}

#[test]
fn test_a_user_agent_file_is_never_overwritten_and_the_key_is_not_added() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.agent_path().parent().unwrap()).expect("agents dir");
    let user = "---\nname: compresso-main\ndescription: mine\n---\n\nMine.\n";
    fs::write(fixture.agent_path(), user).expect("user agent");
    let output = fixture.install(&[]);
    assert_eq!(fs::read_to_string(fixture.agent_path()).unwrap(), user);
    assert!(!fixture.settings_path().exists());
    let args = fixture.entry_args();
    assert!(!has_lean_session(&args) && !args.iter().any(|arg| arg == "--edits"));
    assert!(
        warnings(&output)
            .iter()
            .any(|warning| warning.starts_with("main_agent_skipped")),
        "{output}"
    );
}

#[test]
fn test_plain_uninstall_removes_the_key_the_agent_file_and_the_entry() {
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
    // The settings file Compresso created held nothing else, so it is gone.
    assert!(!fixture.settings_path().exists());
    assert!(!fixture.agent_path().exists());

    // A settings file the user had keeps every byte it had.
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".claude")).expect("claude dir");
    let original = "{\n\t\"permissions\": {\"allow\": []},\n\t\"model\": \"x\"\n}";
    fs::write(fixture.settings_path(), original).expect("settings");
    fixture.install(&[]);
    let installed = fs::read_to_string(fixture.settings_path()).expect("settings");
    assert!(installed.starts_with("{\n\t\"permissions\": {\"allow\": []},\n\t\"model\": \"x\""));
    assert!(installed.contains("\"agent\": \"compresso-main\""));
    let (exit, _, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        fs::read_to_string(fixture.settings_path()).expect("settings"),
        original
    );
}

#[test]
fn test_uninstall_keeps_an_agent_key_that_no_longer_names_ours() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    fs::write(fixture.settings_path(), "{\"agent\": \"mine\"}\n").expect("settings");
    let (exit, _, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        fs::read_to_string(fixture.settings_path()).expect("settings"),
        "{\"agent\": \"mine\"}\n"
    );
}

#[test]
fn test_dry_run_previews_every_plan_and_writes_nothing() {
    let fixture = Fixture::new();
    let before = files_under(&fixture.root);
    let (exit, human, errors) = fixture.integrations_raw(&[
        "install",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--dry-run",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(human.contains("compresso-main.md"), "{human}");
    assert!(
        human.contains(".claude/settings.local.json: sets \"agent\": \"compresso-main\""),
        "{human}"
    );
    assert!(human.contains("\"--session-tools\""), "{human}");
    assert!(
        human.contains("# Main agent plan hash (main_agent_setting)"),
        "{human}"
    );
    assert_eq!(files_under(&fixture.root), before);
    assert!(!fixture.environment_data_written());
}

impl Fixture {
    fn environment_data_written(&self) -> bool {
        self.root
            .parent()
            .expect("base")
            .join("data")
            .join("integration-receipts.json")
            .exists()
    }
}

#[test]
fn test_a_git_repository_that_does_not_ignore_the_settings_file_gets_one_warning() {
    let not_ignored = |output: &Value| {
        warnings(output)
            .iter()
            .filter(|warning| warning.starts_with("settings_local_not_ignored"))
            .count()
    };
    let relative = Path::new(".claude/settings.local.json");

    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".git")).expect("git dir");
    let output = fixture.install(&[]);
    // The user's own global excludes file may already ignore it.
    let expected = usize::from(!compresso_atlas::is_globally_git_ignored(relative));
    assert_eq!(not_ignored(&output), expected, "{output}");
    assert!(
        !fixture.root.join(".gitignore").exists(),
        "nothing is edited"
    );

    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".git")).expect("git dir");
    fs::write(
        fixture.root.join(".gitignore"),
        ".claude/settings.local.json\n",
    )
    .expect("ignore");
    let output = fixture.install(&[]);
    assert_eq!(not_ignored(&output), 0, "{output}");

    // Not a git repository: no warning.
    let fixture = Fixture::new();
    let output = fixture.install(&[]);
    assert_eq!(not_ignored(&output), 0, "{output}");
}

#[test]
fn test_verify_reports_the_main_agent_checks() {
    let check = |output: &Value, name: &str| {
        output["data"]["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .find(|check| check["name"] == name)
            .map(|check| check["passed"] == true)
    };
    let fixture = Fixture::new();
    fixture.install(&[]);
    let (_, output, _) = fixture.integrations(&[
        "verify",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--json",
    ]);
    for name in [
        "main_agent_managed",
        "main_agent_current",
        "main_agent_setting",
        "main_agent_server_profile",
    ] {
        assert_eq!(check(&output, name), Some(true), "{name}: {output}");
    }
    fs::write(fixture.settings_path(), "{\"agent\": \"other\"}").expect("settings");
    let (_, output, _) = fixture.integrations(&[
        "verify",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--json",
    ]);
    assert_eq!(
        check(&output, "main_agent_setting"),
        Some(false),
        "{output}"
    );

    // A --no-main-agent install gets none of these checks.
    let fixture = Fixture::new();
    fixture.install(&["--no-main-agent"]);
    let (_, output, _) = fixture.integrations(&[
        "verify",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--json",
    ]);
    assert_eq!(check(&output, "main_agent_managed"), None, "{output}");
}

#[test]
fn test_no_main_agent_over_a_default_install_removes_the_agent_and_its_key() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    let output = fixture.install(&["--no-main-agent"]);
    assert!(!fixture.agent_path().exists());
    assert!(!fixture.settings_path().exists());
    assert!(!has_lean_session(&fixture.entry_args()));
    let steps = output["data"]["main_agent"].as_array().expect("steps");
    assert!(
        steps.iter().all(|step| step["result"]["removed"] == true),
        "{output}"
    );
    assert!(
        !warnings(&output)
            .iter()
            .any(|warning| warning.starts_with("settings_local_not_ignored")),
        "{output}"
    );
}

#[test]
fn test_sql_env_keeps_the_full_profile_without_the_agent_or_edits() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    let output = fixture.install(&["--sql-env", "COMPRESSO_SQL_APP"]);
    let args = fixture.entry_args();
    assert!(!has_lean_session(&args), "{args:?}");
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--sql-env", "COMPRESSO_SQL_APP"]),
        "{args:?}"
    );
    assert!(!args.iter().any(|arg| arg == "--edits"), "{args:?}");
    // The agent an earlier install wrote is removed with its key.
    assert!(!fixture.agent_path().exists());
    assert!(!fixture.settings_path().exists());
    assert!(
        warnings(&output)
            .iter()
            .any(|warning| warning.starts_with("main_agent_not_installed")
                && warning.contains("--sql-env")),
        "{output}"
    );
    // The preview names every flag and redacts only the variable name.
    let (_, human, _) = fixture.integrations_raw(&[
        "install",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--sql-env",
        "COMPRESSO_SQL_APP",
        "--edits",
        "--dry-run",
    ]);
    assert!(
        human.contains("\"--edits\", \"--sql-env\", \"<redacted>\", \"--recall-source\""),
        "{human}"
    );
    assert!(!human.contains("COMPRESSO_SQL_APP"), "{human}");
}

#[test]
fn test_a_reinstall_that_drops_an_opt_in_flag_says_so() {
    let fixture = Fixture::new();
    fixture.install(&["--no-main-agent", "--sql-env", "COMPRESSO_SQL_APP"]);
    // Any opt-in a newer version added is named generically too.
    let path = fixture.root.join(".mcp.json");
    let text = fs::read_to_string(&path).expect("entry");
    fs::write(
        &path,
        text.replace(
            "\"--recall-source\"",
            "\"--mongo-env\",\n        \"COMPRESSO_MONGO_X\",\n        \"--recall-source\"",
        ),
    )
    .expect("entry");
    let (_, output, _) = fixture.integrations(&[
        "install",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--dry-run",
        "--json",
    ]);
    let dropped = warnings(&output)
        .into_iter()
        .filter(|warning| warning.starts_with("entry_flag_removed"))
        .collect::<Vec<_>>();
    assert!(
        dropped
            .iter()
            .any(|w| w.contains("--sql-env") && w.contains("pass --sql-env NAME again")),
        "{dropped:?}"
    );
    assert!(
        dropped.iter().any(|w| w.contains("--mongo-env")),
        "{dropped:?}"
    );
    assert!(
        !dropped.iter().any(|w| w.contains("COMPRESSO_MONGO_X")),
        "{dropped:?}"
    );
}

#[test]
fn test_a_full_install_and_uninstall_cycle_leaves_no_extra_file_in_claude() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".claude")).expect("claude dir");
    let original = "{\n  \"env\": {\"TOKEN\": \"secret\"}\n}\n";
    fs::write(fixture.settings_path(), original).expect("settings");
    fixture.install(&[]);
    fixture.install(&["--no-edits"]);
    let (exit, _, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let files = files_under(&fixture.root.join(".claude"))
        .into_iter()
        .map(|(path, _)| path)
        .collect::<Vec<_>>();
    assert_eq!(files, [PathBuf::from("settings.local.json")]);
    assert_eq!(
        fs::read_to_string(fixture.settings_path()).unwrap(),
        original
    );
}

#[test]
fn test_uninstall_keeps_the_agent_file_when_the_key_cannot_be_removed() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    fs::write(fixture.settings_path(), "{\"agent\": \"compresso-main\",").expect("broken");
    let (exit, output, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(fixture.agent_path().is_file(), "{output}");
    let warnings = warnings(&output);
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("main_agent_setting_not_removed")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("main_agent_not_removed")),
        "{warnings:?}"
    );
}

#[test]
fn test_a_dry_run_never_recovers_a_pending_journal() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".claude/agents")).expect("dirs");
    let journal = fixture
        .root
        .join(".claude/settings.local.json.compresso-journal");
    fs::write(&journal, "{\"not\": \"a journal\"}").expect("journal");
    let before = files_under(&fixture.root);
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--dry-run",
        "--json",
    ]);
    // Planning stops at the pending journal; recovery (which would read,
    // replay, or reject it) is never attempted by a dry run.
    assert_eq!(exit, CliExitCode::Failure);
    assert!(errors.contains("pending_recovery"), "{errors}");
    assert_eq!(files_under(&fixture.root), before);
    assert!(!fixture.environment_data_written());
}

#[test]
fn test_status_says_an_out_of_date_skill_is_installed() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    let skill = fixture.root.join(".claude/skills/compresso/SKILL.md");
    let text = fs::read_to_string(&skill).expect("skill");
    fs::write(&skill, text.replace("only with --session-tools", "not")).expect("older skill");
    let (_, human, _) =
        fixture.integrations_raw(&["status", "claude-code", "--project", fixture.root_arg()]);
    assert!(
        human.contains("session commands: installed, out of date; re-install to update"),
        "{human}"
    );
}

#[test]
fn test_verify_fails_the_profile_when_the_agent_names_an_edit_tool_the_entry_does_not_serve() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    let path = fixture.root.join(".mcp.json");
    let text = fs::read_to_string(&path).expect("entry");
    fs::write(&path, text.replace("\"--edits\",", "")).expect("entry");
    assert!(!fixture.entry_args().iter().any(|arg| arg == "--edits"));
    let (_, verify, _) = fixture.integrations(&[
        "verify",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--json",
    ]);
    let profile = verify["data"]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "main_agent_server_profile")
        .expect("profile check");
    assert_eq!(profile["passed"], false, "{verify}");
}

/// Writes a file when the confirmation is read, as a client session that
/// grants a permission while the preview is on screen would.
struct ChangeOnConfirm {
    path: PathBuf,
    bytes: &'static str,
    answered: bool,
}

impl Read for ChangeOnConfirm {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if !self.answered {
            self.answered = true;
            fs::write(&self.path, self.bytes)?;
            buffer[0] = b'y';
            return Ok(1);
        }
        if self.path.as_os_str().is_empty() {
            return Ok(0);
        }
        self.path = PathBuf::new();
        buffer[0] = b'\n';
        Ok(1)
    }
}

fn verify_checks(fixture: &Fixture) -> Vec<(String, bool)> {
    let (_, verify, _) = fixture.integrations(&[
        "verify",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--json",
    ]);
    verify["data"]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter(|check| {
            check["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("main_agent"))
        })
        .map(|check| {
            (
                check["name"].as_str().unwrap().to_owned(),
                check["passed"] == true,
            )
        })
        .collect()
}

#[test]
fn test_a_settings_change_between_preview_and_apply_writes_the_fallback_entry() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".claude")).expect("claude dir");
    fs::write(
        fixture.settings_path(),
        "{\n  \"permissions\": {\"allow\": []}\n}\n",
    )
    .expect("settings");
    let changed = "{\n  \"permissions\": {\"allow\": [\"Bash(ls:*)\"]}\n}\n";
    let cli = Cli::try_parse_from([
        "compresso",
        "integrations",
        "install",
        "claude-code",
        "--project",
        fixture.root_arg(),
    ])
    .expect("parses");
    let RootCommand::Integrations { command } = cli.command else {
        panic!("integrations");
    };
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let exit = execute_integrations(
        command,
        &fixture.environment,
        &mut ChangeOnConfirm {
            path: fixture.settings_path(),
            bytes: changed,
            answered: false,
        },
        &mut output,
        &mut errors,
    );
    let human = String::from_utf8(output).unwrap();
    assert_eq!(
        exit,
        CliExitCode::Success,
        "{human} {}",
        String::from_utf8_lossy(&errors)
    );
    assert!(
        human.contains("If the agent file or the \"agent\" setting cannot be written"),
        "{human}"
    );
    assert!(human.contains("main_agent_not_installed"), "{human}");
    // The end state agrees: full-profile entry, no agent, the user's bytes.
    let args = fixture.entry_args();
    assert!(
        !has_lean_session(&args) && !args.iter().any(|arg| arg == "--edits"),
        "{args:?}"
    );
    assert!(!fixture.agent_path().exists());
    assert_eq!(
        fs::read_to_string(fixture.settings_path()).unwrap(),
        changed
    );
    assert!(verify_checks(&fixture).iter().all(|(_, passed)| *passed));
}

#[test]
fn test_verify_reports_a_lean_entry_without_the_agent() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    fs::remove_file(fixture.agent_path()).expect("agent");
    fs::remove_file(fixture.settings_path()).expect("settings");
    let checks = verify_checks(&fixture);
    assert!(
        checks.contains(&("main_agent_server_profile".to_owned(), false)),
        "{checks:?}"
    );
}

#[test]
fn test_verify_reports_a_users_own_agent_as_not_managed() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.agent_path().parent().unwrap()).expect("dirs");
    fs::write(fixture.settings_path(), "{\"agent\": \"mine\"}\n").expect("settings");
    fs::write(
        fixture.agent_path(),
        "---\nname: compresso-main\ndescription: mine\n---\n\nMine.\n",
    )
    .expect("agent");
    fixture.install(&["--no-main-agent"]);
    let checks = verify_checks(&fixture);
    assert_eq!(
        checks,
        [("main_agent_not_managed".to_owned(), true)],
        "{checks:?}"
    );
    let (exit, _, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        verify_checks(&fixture),
        [("main_agent_not_managed".to_owned(), true)]
    );
    assert_eq!(
        fs::read_to_string(fixture.settings_path()).unwrap(),
        "{\"agent\": \"mine\"}\n"
    );
}

#[test]
fn test_uninstall_removes_the_agent_and_key_when_the_entry_is_gone() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    fs::remove_file(fixture.root.join(".mcp.json")).expect("entry");
    let (exit, output, errors) = fixture.integrations(&[
        "uninstall",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors} {output}");
    assert!(!fixture.agent_path().exists());
    assert!(!fixture.settings_path().exists());
    assert!(
        warnings(&output)
            .iter()
            .any(|warning| warning.starts_with("entry_not_removed")),
        "{output}"
    );
}

#[test]
fn test_removed_flags_are_named_without_their_values() {
    let fixture = Fixture::new();
    fixture.install(&["--no-main-agent"]);
    let path = fixture.root.join(".mcp.json");
    let text = fs::read_to_string(&path).expect("entry");
    fs::write(
        &path,
        text.replace(
            "\"--recall-source\"",
            "\"--token=s3cret\",\n        \"--recall-source\"",
        ),
    )
    .expect("entry");
    let (_, output, _) = fixture.integrations(&[
        "install",
        "claude-code",
        "--project",
        fixture.root_arg(),
        "--no-main-agent",
        "--dry-run",
        "--json",
    ]);
    let text = output.to_string();
    assert!(
        text.contains(
            "entry_flag_removed: the current compresso entry starts the server with --token;"
        ),
        "{text}"
    );
    assert!(!text.contains("s3cret"), "{text}");
}

#[test]
fn test_project_not_indexed_is_judged_on_the_store_index_uses_with_xdg_data_home_set() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let base = fs::canonicalize(directory.path()).expect("canonical");
    let home = base.join("home");
    let indexed = base.join("indexed");
    let unindexed = base.join("unindexed");
    for path in [&home, &indexed, &unindexed] {
        fs::create_dir_all(path.join("src")).expect("dirs");
    }
    fs::write(indexed.join("src/a.rs"), "pub fn a() {}\n").expect("source");
    let run = |arguments: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_compresso"))
            .args(arguments)
            .env_clear()
            .env("HOME", &home)
            .env("XDG_DATA_HOME", base.join("xdg-data"))
            .env("XDG_CONFIG_HOME", base.join("xdg-config"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .output()
            .expect("compresso runs");
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("UTF-8")
    };
    run(&["index", indexed.to_str().unwrap(), "--json"]);
    let out = run(&[
        "integrations",
        "install",
        "claude-code",
        "--project",
        indexed.to_str().unwrap(),
        "--yes",
        "--json",
    ]);
    assert!(!out.contains("project_not_indexed"), "{out}");
    let out = run(&[
        "integrations",
        "install",
        "claude-code",
        "--project",
        unindexed.to_str().unwrap(),
        "--yes",
        "--json",
    ]);
    assert!(out.contains("project_not_indexed"), "{out}");
}
