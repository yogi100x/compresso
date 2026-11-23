//! E17-50: `integrations install claude-code --sql-env NAME` writes a server
//! entry that serves the read-only SQL tool, storing only the variable's
//! name. Every path here is under a scratch home and project directory.
#![cfg(unix)]

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, IntegrationEnvironment, RootCommand, execute_integrations, execute_setup,
};
use compresso_store::Store;
use serde_json::Value;
use tempfile::TempDir;

const NAME: &str = "COMPRESSO_SQL_APP";

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
            "sql-env-cli-test",
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

    fn config(&self) -> String {
        fs::read_to_string(self.home.join(".claude.json")).unwrap_or_default()
    }

    fn entry_args(&self) -> Vec<String> {
        let config: Value = serde_json::from_str(&self.config()).unwrap_or(Value::Null);
        config["mcpServers"]["compresso"]["args"]
            .as_array()
            .map(|args| {
                args.iter()
                    .map(|argument| argument.as_str().expect("string").to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[test]
fn test_install_writes_the_name_only_and_verifies_and_uninstalls() {
    let fixture = Fixture::new();
    // The dry-run plan explains where the variable must be set.
    let (exit, plan, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--edits",
        "--sql-env",
        NAME,
        "--dry-run",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let warnings = plan["data"]["plan"]["warnings"].to_string();
    assert!(warnings.contains(NAME), "{plan}");
    assert!(
        warnings.contains("environment Claude Code runs in"),
        "{plan}"
    );
    assert!(fixture.config().is_empty(), "a dry run writes nothing");

    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--edits",
        "--sql-env",
        NAME,
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let args = fixture.entry_args();
    assert_eq!(
        &args[args.len() - 5..],
        [
            "--edits",
            "--sql-env",
            NAME,
            "--recall-source",
            "claude-code"
        ]
    );
    assert!(!fixture.config().contains("\"env\""));

    let (_, verify, _) = fixture.integrations(&["verify", "claude-code", "--json"]);
    let server_entry = verify["data"]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "server_entry")
        .expect("server_entry check");
    assert_eq!(server_entry["passed"], true, "{verify}");

    // setup keeps the installer-written entry as it is.
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
    assert_eq!(
        exit,
        CliExitCode::Success,
        "{}",
        String::from_utf8_lossy(&errors)
    );
    assert_eq!(fixture.entry_args(), args);

    // A plain install turns it off again; uninstall removes the entry.
    let (exit, _, errors) = fixture.integrations(&["install", "claude-code", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(!fixture.config().contains("--sql-env"));
    let (exit, _, errors) = fixture.integrations(&[
        "install",
        "claude-code",
        "--sql-env",
        NAME,
        "--yes",
        "--json",
    ]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let (exit, _, errors) = fixture.integrations(&["uninstall", "claude-code", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(fixture.entry_args().is_empty());
}

#[test]
fn test_sql_env_is_install_only_claude_code_only_and_needs_a_valid_name() {
    let fixture = Fixture::new();
    for (arguments, code) in [
        (
            &["install", "codex", "--sql-env", NAME, "--yes", "--json"][..],
            "unsupported",
        ),
        (
            &[
                "uninstall",
                "claude-code",
                "--sql-env",
                NAME,
                "--yes",
                "--json",
            ],
            "invalid_arguments",
        ),
        (
            &[
                "install",
                "claude-code",
                "--sql-env",
                "DATABASE_URL",
                "--yes",
                "--json",
            ],
            "invalid_arguments",
        ),
        (
            &[
                "install",
                "claude-code",
                "--sql-env",
                concat!("postgres://u:", "canary", "@h/db"),
                "--yes",
                "--json",
            ],
            "invalid_arguments",
        ),
    ] {
        let (exit, _, errors) = fixture.integrations(arguments);
        assert_eq!(exit, CliExitCode::Failure, "{arguments:?}");
        assert!(errors.contains(code), "{arguments:?}: {errors}");
        assert!(!errors.contains("canary"), "{errors}");
    }
    assert!(fixture.config().is_empty());
    for conflicting in ["--explore-agent", "--compaction-hook", "--session-commands"] {
        assert!(
            Cli::try_parse_from([
                "compresso",
                "integrations",
                "install",
                "claude-code",
                "--sql-env",
                NAME,
                conflicting,
            ])
            .is_err(),
            "{conflicting}"
        );
    }
}
