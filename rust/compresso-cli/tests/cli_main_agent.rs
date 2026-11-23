//! E17-51: the default main-session agent for a Claude Code project (D116).
//!
//! Every path here is under a scratch home and project directory.
#![cfg(unix)]

use std::fs;
use std::io::Cursor;
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
fn test_another_agent_value_is_kept_and_install_succeeds_with_a_warning() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".claude")).expect("claude dir");
    let original = "{\n  \"agent\": \"my-reviewer\",\n  \"model\": \"x\"\n}\n";
    fs::write(fixture.settings_path(), original).expect("settings");
    let output = fixture.install(&[]);
    assert_eq!(
        fs::read_to_string(fixture.settings_path()).expect("settings"),
        original
    );
    assert!(fixture.agent_path().is_file());
    assert!(
        warnings(&output).iter().any(|warning| {
            warning.starts_with("main_agent_setting_skipped") && warning.contains("my-reviewer")
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
    let global = |relative: &str| compresso_atlas::is_globally_git_ignored(Path::new(relative));
    let file = ".claude/settings.local.json";
    let backup = ".claude/settings.local.json.compresso-backup-2000-01-01T00-00-00Z";

    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".git")).expect("git dir");
    let output = fixture.install(&[]);
    // The user's own global excludes file may already ignore both.
    let expected = usize::from(!(global(file) && global(backup)));
    assert_eq!(not_ignored(&output), expected, "{output}");
    assert!(
        !fixture.root.join(".gitignore").exists(),
        "nothing is edited"
    );

    // Ignoring only the file still names the backups kept beside it.
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".git")).expect("git dir");
    fs::write(fixture.root.join(".gitignore"), format!("{file}\n")).expect("ignore");
    let output = fixture.install(&[]);
    if !global(backup) {
        assert_eq!(not_ignored(&output), 1, "{output}");
        assert!(
            warnings(&output)
                .iter()
                .any(|w| w.contains("compresso-backup-*"))
        );
    }

    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".git")).expect("git dir");
    fs::write(fixture.root.join(".gitignore"), format!("{file}*\n")).expect("ignore");
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
fn test_sql_env_keeps_the_full_profile_and_installs_the_agent_with_a_warning() {
    let fixture = Fixture::new();
    let output = fixture.install(&["--sql-env", "COMPRESSO_SQL_APP"]);
    let args = fixture.entry_args();
    assert!(!has_lean_session(&args), "{args:?}");
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--sql-env", "COMPRESSO_SQL_APP"]),
        "{args:?}"
    );
    assert!(args.iter().any(|arg| arg == "--edits"), "{args:?}");
    assert!(fixture.agent_path().is_file());
    assert!(
        warnings(&output)
            .iter()
            .any(|warning| warning.starts_with("main_agent_full_profile")),
        "{output}"
    );
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
    assert_eq!(profile["passed"], true, "{verify}");
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
