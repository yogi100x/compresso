//! `compresso setup` installs the post-compaction routing-reminder hook by
//! default for Claude Code and Codex, never rewrites hooks a user defined, and
//! still completes when the hook cannot be installed.
#![cfg(unix)]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, IntegrationEnvironment, RootCommand, SetupRuntime, execute_integrations,
    execute_setup, execute_setup_with_runtime,
};
use compresso_integrations::{PlanClock, SystemPlanClock};
use compresso_store::{Store, StoreError};
use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    base: PathBuf,
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
            "setup-hook-cli-test",
        );
        let store = Store::open(&data.join("compresso.db")).expect("store");
        Self {
            _directory: directory,
            base,
            home,
            root,
            store,
            environment,
        }
    }

    fn root_str(&self) -> &str {
        self.root.to_str().expect("UTF-8 root")
    }

    fn setup(&self, arguments: &[&str]) -> (CliExitCode, String, String) {
        let cli = Cli::try_parse_from(
            ["compresso", "setup"]
                .into_iter()
                .chain(arguments.iter().copied()),
        )
        .expect("setup command parses");
        let RootCommand::Setup(arguments) = cli.command else {
            panic!("expected setup command");
        };
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_setup(
            arguments,
            Some(&self.store),
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

    fn setup_json(&self, arguments: &[&str]) -> (CliExitCode, Value, String) {
        let (exit, output, errors) = self.setup(&[arguments, &["--json"]].concat());
        (
            exit,
            serde_json::from_str(&output).unwrap_or(Value::Null),
            errors,
        )
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

    fn claude_hooks(&self) -> PathBuf {
        self.root.join(".claude/settings.json")
    }

    fn codex_hooks(&self) -> PathBuf {
        self.home.join(".codex/hooks.json")
    }

    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut pending = vec![self.base.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).expect("directory").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = fs::read(&path).expect("file");
                    files.push((path, bytes));
                }
            }
        }
        files.sort();
        files
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("JSON file")).expect("valid JSON")
}

fn assert_compact_hook(document: &Value) {
    let groups = document["hooks"]["SessionStart"]
        .as_array()
        .expect("SessionStart groups");
    assert_eq!(groups.len(), 1, "{document}");
    assert_eq!(groups[0]["matcher"], "compact");
    assert!(
        groups[0]["hooks"][0]["command"]
            .as_str()
            .is_some_and(|command| command.ends_with("mcp routing-reminder --session-start-hook")),
        "{document}"
    );
}

#[test]
fn test_setup_installs_the_compaction_hook_by_default_for_claude_code_and_codex() {
    for (client, hooks) in [
        (
            "claude-code",
            Fixture::claude_hooks as fn(&Fixture) -> PathBuf,
        ),
        ("codex", Fixture::codex_hooks),
    ] {
        let fixture = Fixture::new();
        let (exit, output, errors) = fixture.setup_json(&[client, fixture.root_str(), "--yes"]);
        assert_eq!(exit, CliExitCode::Success, "{client}: {errors}");
        assert!(errors.is_empty(), "{client}: {errors}");
        assert_eq!(output["data"]["result"]["status"], "complete");
        assert_eq!(output["data"]["result"]["integration"]["installed"], true);

        let planned = &output["data"]["plan"]["compaction_hook"];
        assert_eq!(planned["status"], "planned", "{client}: {output}");
        assert_eq!(planned["plan"]["owned_keys"][0], "hooks.SessionStart");
        assert_eq!(planned["entry"]["matcher"], "compact");
        assert!(
            planned["entry"]["hooks"][0]["command"]
                .as_str()
                .is_some_and(
                    |command| command.ends_with("mcp routing-reminder --session-start-hook")
                ),
            "{client}: the preview shows the exact hook entry"
        );
        assert!(
            planned["plan"]["unified_diff"]
                .as_str()
                .is_some_and(|diff| diff.contains("SessionStart")),
            "{client}: the hook change is previewed"
        );
        let result = &output["data"]["result"]["compaction_hook"];
        assert_eq!(result["status"], "installed", "{client}: {output}");
        assert_eq!(result["already_applied"], false);

        let hooks = hooks(&fixture);
        assert_compact_hook(&read_json(&hooks));

        // The hook has its own receipt: status sees it and uninstall reverses it.
        let scope: Vec<&str> = if client == "claude-code" {
            vec!["--project", fixture.root_str()]
        } else {
            Vec::new()
        };
        let (exit, status, errors) = fixture.integrations(
            &[
                &["status", client, "--compaction-hook", "--json"],
                scope.as_slice(),
            ]
            .concat(),
        );
        assert_eq!(exit, CliExitCode::Success, "{client}: {errors}");
        assert_eq!(status["data"]["hook_installed"], true);
        assert_eq!(status["data"]["canonical_receipt_active"], true);

        let (exit, removed, errors) = fixture.integrations(
            &[
                &["uninstall", client, "--compaction-hook", "--yes", "--json"],
                scope.as_slice(),
            ]
            .concat(),
        );
        assert_eq!(exit, CliExitCode::Success, "{client}: {errors}");
        assert_eq!(
            removed["data"]["result"]["removed_keys"][0],
            "hooks.SessionStart"
        );
        assert!(read_json(&hooks)["hooks"].get("SessionStart").is_none());
    }
}

#[test]
fn test_setup_dry_run_previews_the_hook_and_writes_nothing() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();

    let (exit, output, errors) = fixture.setup_json(&["codex", fixture.root_str(), "--dry-run"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        output["data"]["plan"]["compaction_hook"]["status"],
        "planned"
    );
    assert!(output["data"]["result"].is_null());

    let (exit, human, errors) = fixture.setup(&["codex", fixture.root_str(), "--dry-run"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(human.contains("hooks.SessionStart"), "{human}");
    assert!(
        human.contains("mcp routing-reminder --session-start-hook"),
        "the preview shows the exact command the hook runs: {human}"
    );
    assert!(human.contains("\"matcher\":\"compact\""), "{human}");
    assert!(human.contains("--no-compaction-hook"), "{human}");
    assert_eq!(fixture.snapshot(), before, "dry run writes nothing");
}

#[test]
fn test_setup_no_compaction_hook_opts_out() {
    let fixture = Fixture::new();
    let (exit, output, errors) =
        fixture.setup_json(&["codex", fixture.root_str(), "--no-compaction-hook", "--yes"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        output["data"]["plan"]["compaction_hook"]["status"],
        "disabled"
    );
    assert_eq!(
        output["data"]["result"]["compaction_hook"]["status"],
        "disabled"
    );
    assert!(!fixture.codex_hooks().exists());
    assert!(fixture.home.join(".codex/config.toml").exists());
}

#[test]
fn test_setup_skips_the_hook_with_warning_and_fix_when_user_session_start_hooks_exist() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join(".codex")).expect("Codex directory");
    let existing = "{\"hooks\":{\"SessionStart\":[{\"matcher\":\"startup\",\"hooks\":[{\"type\":\"command\",\"command\":\"echo mine\"}]}]}}\n";
    fs::write(fixture.codex_hooks(), existing).expect("user hooks");

    // The skip is already visible in the preview.
    let (exit, preview, errors) = fixture.setup_json(&["codex", fixture.root_str(), "--dry-run"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        preview["data"]["plan"]["compaction_hook"]["status"],
        "skipped"
    );

    let (exit, output, errors) = fixture.setup_json(&["codex", fixture.root_str(), "--yes"]);
    assert_eq!(
        exit,
        CliExitCode::Success,
        "setup must still succeed: {errors}"
    );
    assert!(errors.is_empty(), "{errors}");
    assert_eq!(output["data"]["result"]["status"], "complete");
    assert_eq!(output["data"]["result"]["integration"]["installed"], true);
    let hook = &output["data"]["result"]["compaction_hook"];
    assert_eq!(hook["status"], "skipped", "{output}");
    assert_eq!(hook["code"], "user_hooks_present");
    assert!(
        hook["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("hooks.SessionStart")),
        "{hook}"
    );
    let fix = hook["fix"].as_str().expect("fix text");
    assert!(
        fix.contains("routing-reminder --session-start-hook"),
        "{fix}"
    );
    assert!(fix.contains("--no-compaction-hook"), "{fix}");
    assert_eq!(
        fs::read_to_string(fixture.codex_hooks()).expect("user hooks"),
        existing,
        "user-defined hooks are never rewritten"
    );
    assert!(
        fs::read_to_string(fixture.home.join(".codex/config.toml"))
            .expect("Codex config")
            .contains("mcp_servers.compresso"),
        "the MCP entry is still installed"
    );

    let other = Fixture::new();
    fs::create_dir_all(other.home.join(".codex")).expect("Codex directory");
    fs::write(other.codex_hooks(), existing).expect("user hooks");
    let (exit, human, errors) = other.setup(&["codex", other.root_str(), "--yes"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(
        human.contains("warning: the compaction reminder hook was not installed"),
        "{human}"
    );
    assert!(human.contains("fix: "), "{human}");
    assert!(human.contains("Setup complete for codex"), "{human}");
}

#[test]
fn test_setup_skips_the_hook_when_the_hook_file_is_unusable_and_still_succeeds() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join(".codex")).expect("Codex directory");
    fs::write(fixture.codex_hooks(), "{ not json").expect("malformed hooks");

    let (exit, output, errors) = fixture.setup_json(&["codex", fixture.root_str(), "--yes"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(output["data"]["result"]["status"], "complete");
    let hook = &output["data"]["result"]["compaction_hook"];
    assert_eq!(hook["status"], "skipped", "{output}");
    assert!(
        hook["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("integrations install codex --compaction-hook")),
        "{hook}"
    );
    assert_eq!(
        fs::read_to_string(fixture.codex_hooks()).expect("hooks"),
        "{ not json"
    );
}

#[test]
fn test_setup_cursor_has_no_hook_step_and_no_warning() {
    let fixture = Fixture::new();
    let (exit, output, errors) = fixture.setup_json(&["cursor", fixture.root_str(), "--yes"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(
        output["data"]["plan"]["compaction_hook"]["status"],
        "unsupported"
    );
    assert_eq!(
        output["data"]["result"]["compaction_hook"]["status"],
        "unsupported"
    );

    let other = Fixture::new();
    let (exit, human, errors) = other.setup(&["cursor", other.root_str(), "--yes"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(!human.contains("warning:"), "{human}");
    assert!(!human.contains("fix:"), "{human}");
    assert!(!human.contains("routing reminder"), "{human}");
    assert!(!other.root.join(".cursor/hooks.json").exists());
}

#[test]
fn test_setup_repeat_keeps_the_hook_and_reports_it_already_installed() {
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.setup_json(&["codex", fixture.root_str(), "--yes"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let installed = fs::read(fixture.codex_hooks()).expect("hooks");

    let (exit, output, errors) = fixture.setup_json(&["codex", fixture.root_str(), "--yes"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let hook = &output["data"]["result"]["compaction_hook"];
    assert_eq!(hook["status"], "installed", "{output}");
    assert_eq!(hook["already_applied"], true);
    assert_eq!(fs::read(fixture.codex_hooks()).expect("hooks"), installed);
}

#[test]
fn test_uninstalling_the_mcp_entry_warns_that_the_hook_is_still_installed() {
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.setup_json(&["codex", fixture.root_str(), "--yes"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");

    let (exit, output, errors) = fixture.integrations(&["uninstall", "codex", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let warnings = output["data"]["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning.as_str().is_some_and(
            |warning| warning.contains("integrations uninstall codex --compaction-hook")
        )),
        "{output}"
    );
    assert!(
        fixture.codex_hooks().exists(),
        "the hook is left for its own uninstall"
    );
}

#[test]
fn test_integrations_install_does_not_install_the_hook_without_the_flag() {
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.integrations(&["install", "codex", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(
        !fixture.codex_hooks().exists(),
        "`integrations install` changes exactly one owned key per plan"
    );
}

/// Rewrites the hook file while setup opens the store, which happens after
/// the plans were approved and authorized but before either is applied.
struct HookDriftRuntime {
    database: PathBuf,
    hooks: PathBuf,
    contents: &'static str,
}

impl SetupRuntime for HookDriftRuntime {
    fn open_default_store(&self) -> Result<Store, StoreError> {
        fs::create_dir_all(self.hooks.parent().expect("hook directory")).expect("hook directory");
        fs::write(&self.hooks, self.contents).expect("concurrent hook edit");
        Store::open(&self.database)
    }

    fn plan_clock(&self) -> std::sync::Arc<dyn PlanClock> {
        std::sync::Arc::new(SystemPlanClock)
    }
}

#[test]
fn test_setup_succeeds_when_the_hook_file_changes_after_approval() {
    let fixture = Fixture::new();
    let concurrent = "{\"hooks\":{\"SessionStart\":[{\"matcher\":\"startup\",\"hooks\":[]}]}}\n";
    let runtime = HookDriftRuntime {
        database: fixture.base.join("data/compresso.db"),
        hooks: fixture.codex_hooks(),
        contents: concurrent,
    };
    let cli = Cli::try_parse_from([
        "compresso",
        "setup",
        "codex",
        fixture.root_str(),
        "--yes",
        "--json",
    ])
    .expect("setup command parses");
    let RootCommand::Setup(arguments) = cli.command else {
        panic!("expected setup command");
    };
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let exit = execute_setup_with_runtime(
        arguments,
        None,
        &fixture.environment,
        &runtime,
        &mut Cursor::new(Vec::new()),
        &mut output,
        &mut errors,
    );

    let errors = String::from_utf8(errors).expect("UTF-8 stderr");
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(errors.is_empty(), "{errors}");
    let output: Value = serde_json::from_slice(&output).expect("setup JSON");
    assert_eq!(
        output["data"]["plan"]["compaction_hook"]["status"],
        "planned"
    );
    assert_eq!(output["data"]["result"]["status"], "complete");
    assert_eq!(output["data"]["result"]["integration"]["installed"], true);
    let hook = &output["data"]["result"]["compaction_hook"];
    assert_eq!(hook["status"], "failed", "{output}");
    assert_eq!(hook["code"], "config_drift");
    assert!(
        hook["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("integrations install codex --compaction-hook")),
        "{hook}"
    );
    assert_eq!(
        fs::read_to_string(fixture.codex_hooks()).expect("hooks"),
        concurrent,
        "an edit made after approval is never overwritten"
    );
    assert!(
        fs::read_to_string(fixture.home.join(".codex/config.toml"))
            .expect("Codex config")
            .contains("mcp_servers.compresso")
    );
}
