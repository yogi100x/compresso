use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, IntegrationEnvironment, RootCommand, UnavailableAccountClient,
    execute_integrations, execute_with_store_and_input,
};
use compresso_integrations::{
    CodexIntegration, FileReceiptStore, InstallCtx, InstallReceipt, Integration, PlanApproval,
    PlanManager, ReceiptError, ReceiptStore, Scope,
};
use compresso_store::Store;
use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    home: PathBuf,
    project: PathBuf,
    data: PathBuf,
    environment: IntegrationEnvironment,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp directory");
        let home = directory.path().join("home");
        let project = directory.path().join("project");
        let data = directory.path().join("data");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&project).expect("project");
        let executable = std::env::current_exe()
            .and_then(fs::canonicalize)
            .expect("test executable should resolve");
        let environment = IntegrationEnvironment::new(
            fs::canonicalize(&home).expect("home should resolve"),
            fs::canonicalize(&project).expect("project should resolve"),
            executable,
            data.clone(),
            Vec::new(),
            "integration-cli-test",
        );
        Self {
            _directory: directory,
            home,
            project,
            data,
            environment,
        }
    }

    fn run(&self, arguments: &[&str], input: &str) -> (CliExitCode, Value, Value) {
        let cli =
            Cli::try_parse_from(std::iter::once("compresso").chain(arguments.iter().copied()))
                .expect("integration command should parse");
        let RootCommand::Integrations { command } = cli.command else {
            panic!("expected integration command");
        };
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_integrations(
            command,
            &self.environment,
            &mut Cursor::new(input.as_bytes()),
            &mut output,
            &mut errors,
        );
        (exit, parse_json(&output), parse_json(&errors))
    }

    fn codex_config(&self) -> PathBuf {
        self.home.join(".codex/config.toml")
    }

    fn database(&self) -> PathBuf {
        self.data.join("compresso.db")
    }
}

#[test]
fn test_exact_codex_dry_run_command_previews_without_any_writes() {
    let fixture = Fixture::new();
    let before = snapshot(fixture._directory.path());

    let (exit, output, errors) = fixture.run(
        &["integrations", "install", "codex", "--dry-run", "--json"],
        "",
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["data"]["action"], "install");
    assert_eq!(output["data"]["client"], "codex");
    assert_eq!(output["data"]["dry_run"], true);
    let plan = &output["data"]["plan"];
    assert!(plan["unified_diff"].as_str().is_some_and(|diff| {
        diff.contains("Compresso-owned keys: mcp_servers.compresso")
            && diff.contains("backup: (none: file created)")
    }));
    assert!(
        plan["plan_hash"]
            .as_str()
            .is_some_and(|hash| !hash.is_empty())
    );
    assert_eq!(snapshot(fixture._directory.path()), before);
    assert!(!fixture.codex_config().exists());
    assert!(!fixture.data.exists());
}

#[test]
fn test_dry_run_wiring_is_write_free_for_all_supported_clients() {
    for client in ["claude-code", "codex", "cursor"] {
        let fixture = Fixture::new();
        let before = snapshot(fixture._directory.path());

        let (exit, output, errors) = fixture.run(
            &["integrations", "install", client, "--dry-run", "--json"],
            "",
        );

        assert_eq!(exit, CliExitCode::Success, "{client}");
        assert!(errors.is_null(), "{client}: {errors}");
        assert_eq!(output["data"]["client"], client);
        assert_eq!(output["data"]["dry_run"], true);
        assert_eq!(snapshot(fixture._directory.path()), before, "{client}");
    }
}

#[test]
fn test_human_preview_includes_diff_owned_key_backup_and_plan_hash() {
    let fixture = Fixture::new();
    let cli = Cli::try_parse_from(["compresso", "integrations", "install", "codex", "--dry-run"])
        .expect("exact dry-run command should parse");
    let RootCommand::Integrations { command } = cli.command else {
        panic!("expected integration command");
    };
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let exit = execute_integrations(
        command,
        &fixture.environment,
        &mut std::io::empty(),
        &mut output,
        &mut errors,
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_empty());
    let preview = String::from_utf8(output).expect("UTF-8 preview");
    assert!(preview.starts_with("--- "));
    assert!(preview.contains("Compresso-owned keys: mcp_servers.compresso"));
    assert!(preview.contains("backup: (none: file created)"));
    assert!(preview.contains("# Plan hash: "));
    assert!(!fixture.codex_config().exists());
    assert!(!fixture.data.exists());
}

#[test]
fn test_json_install_requires_yes_and_leaves_all_state_untouched() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run(&["integrations", "install", "codex", "--json"], "");

    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(output["data"]["plan"]["client"], "codex");
    assert_eq!(errors["error"]["code"], "confirmation_required");
    assert!(!fixture.codex_config().exists());
    assert!(!fixture.data.exists());
}

#[test]
fn test_noninteractive_install_requires_yes() {
    let fixture = Fixture::new();
    let cli = Cli::try_parse_from(["compresso", "integrations", "install", "codex"])
        .expect("command should parse");
    let RootCommand::Integrations { command } = cli.command else {
        panic!("expected integration command");
    };
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let exit = execute_integrations(
        command,
        &fixture.environment,
        &mut std::io::empty(),
        &mut output,
        &mut errors,
    );

    assert_eq!(exit, CliExitCode::Failure);
    assert!(String::from_utf8_lossy(&errors).contains("confirmation_required"));
    assert!(!fixture.codex_config().exists());
    assert!(!fixture.data.exists());
}

#[test]
fn test_yes_install_commits_canonical_receipt_before_sqlite_projection() {
    let fixture = Fixture::new();

    let (exit, output, errors) =
        fixture.run(&["integrations", "install", "codex", "--yes", "--json"], "");

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["result"]["installed"], true);
    assert!(
        fs::read_to_string(fixture.codex_config())
            .expect("Codex config")
            .contains("[mcp_servers.compresso]")
    );
    assert!(fixture.data.join("integration-receipts.json.d").is_dir());
    assert!(
        fixture
            .data
            .join("integration-receipts.json.approval-key")
            .is_file()
    );
    let store = Store::open(&fixture.database()).expect("projected store");
    let records = store.receipts(Some("codex")).expect("projected receipt");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].uninstalled_at, None);
}

#[test]
fn test_uninstall_uses_canonical_receipt_and_updates_projection() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .run(&["integrations", "install", "codex", "--yes", "--json"], "",)
            .0,
        CliExitCode::Success
    );

    let (exit, output, errors) = fixture.run(
        &["integrations", "uninstall", "codex", "--yes", "--json"],
        "",
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(
        output["data"]["result"]["removed_keys"][0],
        "mcp_servers.compresso"
    );
    assert!(
        !fs::read_to_string(fixture.codex_config())
            .expect("Codex config remains")
            .contains("[mcp_servers.compresso]")
    );
    let store = Store::open(&fixture.database()).expect("projected store");
    let records = store.receipts(Some("codex")).expect("projected receipt");
    assert_eq!(records.len(), 1);
    assert!(records[0].uninstalled_at.is_some());
}

#[test]
fn test_uninstall_retains_user_modified_owned_table_with_content_free_warning() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .run(&["integrations", "install", "codex", "--yes", "--json"], "",)
            .0,
        CliExitCode::Success
    );
    let config = fixture.codex_config();
    let original = fs::read_to_string(&config).expect("installed config");
    fs::write(
        &config,
        original.replace(
            "args = [\"mcp\", \"serve\"]",
            "args = [\"mcp\", \"serve\", \"user-edit\"]",
        ),
    )
    .expect("user edit");

    let (exit, output, errors) = fixture.run(
        &["integrations", "uninstall", "codex", "--yes", "--json"],
        "",
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(
        output["data"]["result"]["retained_keys"][0],
        "mcp_servers.compresso"
    );
    assert_eq!(
        output["data"]["result"]["warnings"][0],
        "key_modified:mcp_servers.compresso"
    );
    assert!(
        fs::read_to_string(config)
            .expect("retained config")
            .contains("user-edit")
    );
}

#[test]
fn test_projection_failure_warns_without_invalidating_canonical_commit() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.database()).expect("database path made unusable as a file");

    let (exit, output, errors) =
        fixture.run(&["integrations", "install", "codex", "--yes", "--json"], "");

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["warnings"][0], "sqlite_projection_failed");
    assert!(fixture.codex_config().is_file());
    assert!(fixture.data.join("integration-receipts.json.d").is_dir());
}

#[test]
fn test_project_scope_is_explicit_and_does_not_touch_user_config() {
    let fixture = Fixture::new();

    let (exit, _, errors) = fixture.run(
        &[
            "integrations",
            "install",
            "claude-code",
            "--project",
            fixture.project.to_str().expect("UTF-8 project"),
            "--yes",
            "--json",
        ],
        "",
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert!(fixture.project.join(".mcp.json").is_file());
    assert!(!fixture.home.join(".claude.json").exists());
}

#[test]
fn test_verify_does_not_fake_missing_live_evidence() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .run(&["integrations", "install", "codex", "--yes", "--json"], "",)
            .0,
        CliExitCode::Success
    );

    let (exit, output, errors) = fixture.run(&["integrations", "verify", "codex", "--json"], "");

    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(output["data"]["verified"], false);
    assert!(
        output["data"]["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .any(|check| check["passed"] == false
                && check["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("evidence")))
    );
    assert_eq!(errors["error"]["code"], "verification_failed");
}

#[test]
fn test_status_is_read_only_even_without_receipt_state() {
    let fixture = Fixture::new();
    let before = snapshot(fixture._directory.path());

    let (exit, output, errors) = fixture.run(&["integrations", "status", "codex", "--json"], "");

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["canonical_receipt_active"], false);
    assert_eq!(snapshot(fixture._directory.path()), before);
    assert!(!fixture.data.exists());
}

#[test]
fn test_claude_desktop_is_explicitly_refused_without_writes() {
    let fixture = Fixture::new();
    let before = snapshot(fixture._directory.path());

    let (exit, output, errors) = fixture.run(
        &[
            "integrations",
            "install",
            "claude-desktop",
            "--yes",
            "--json",
        ],
        "",
    );

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_null());
    assert_eq!(errors["error"]["code"], "unsupported");
    assert!(
        errors["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("D5"))
    );
    assert_eq!(snapshot(fixture._directory.path()), before);
}

#[test]
fn test_generic_injected_boundary_never_falls_back_to_real_user_paths() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.database()).expect("injected store");
    let cli = Cli::try_parse_from([
        "compresso",
        "integrations",
        "install",
        "codex",
        "--yes",
        "--json",
    ])
    .expect("integration command should parse");
    let before = snapshot(&fixture.home);
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let exit = execute_with_store_and_input(
        cli,
        &UnavailableAccountClient,
        &store,
        &fixture.project,
        &mut std::io::empty(),
        &mut output,
        &mut errors,
    );

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_empty());
    assert_eq!(parse_json(&errors)["error"]["code"], "environment_required");
    assert_eq!(snapshot(&fixture.home), before);
}

#[cfg(unix)]
#[test]
fn test_codex_recovery_rejects_config_directory_replaced_by_symlink() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let adapter = CodexIntegration::new(
        fixture.environment.home_dir.clone(),
        Vec::new(),
        None,
        env!("CARGO_PKG_VERSION"),
    )
    .expect("Codex adapter");
    let manager = PlanManager::new("cli-codex-recovery-symlink").expect("plan manager");
    let plan = adapter
        .plan_install(
            &manager,
            &InstallCtx {
                compresso_bin: fixture.environment.compresso_bin.clone(),
                scope: Scope::User,
            },
        )
        .expect("install plan");
    let receipts = FailOnceReceiptStore::new(fixture.data.join("integration-receipts.json"));

    adapter
        .install(&manager, &PlanApproval::from(&plan), &receipts)
        .expect_err("injected receipt failure leaves an authenticated journal");
    let config_dir = fixture.home.join(".codex");
    let outside = fixture._directory.path().join("outside-codex");
    fs::rename(&config_dir, &outside).expect("move pending recovery outside home");
    fs::remove_file(outside.join("config.toml")).expect("restore interrupted base state");
    symlink(&outside, &config_dir).expect("replace Codex directory with symlink");
    let journal = outside.join("config.toml.compresso-journal");
    assert!(journal.is_file());

    let (exit, output, errors) =
        fixture.run(&["integrations", "install", "codex", "--yes", "--json"], "");

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_null());
    assert_eq!(errors["error"]["code"], "unsupported");
    assert!(
        errors["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("symlink"))
    );
    assert!(!outside.join("config.toml").exists());
    assert!(
        journal.is_file(),
        "recovery must not touch the symlink target"
    );
}

struct FailOnceReceiptStore {
    inner: FileReceiptStore,
    fail_next_save: AtomicBool,
}

impl FailOnceReceiptStore {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: FileReceiptStore::new(path),
            fail_next_save: AtomicBool::new(true),
        }
    }
}

impl ReceiptStore for FailOnceReceiptStore {
    fn by_plan_hash(&self, plan_hash: &str) -> Result<Option<InstallReceipt>, ReceiptError> {
        self.inner.by_plan_hash(plan_hash)
    }

    fn active_for(
        &self,
        client: &str,
        config_path: &Path,
    ) -> Result<Option<InstallReceipt>, ReceiptError> {
        self.inner.active_for(client, config_path)
    }

    fn save(&self, receipt: &InstallReceipt) -> Result<(), ReceiptError> {
        if self.fail_next_save.swap(false, Ordering::SeqCst) {
            return Err(ReceiptError::LockPoisoned);
        }
        self.inner.save(receipt)
    }

    fn seal_journal(&self, payload: &[u8]) -> Result<String, ReceiptError> {
        self.inner.seal_journal(payload)
    }

    fn verify_journal(&self, payload: &[u8], mac: &str) -> Result<(), ReceiptError> {
        self.inner.verify_journal(payload, mac)
    }
}

fn parse_json(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(bytes).expect("output should be JSON")
    }
}

fn snapshot(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, current: &Path, files: &mut Vec<PathBuf>) {
        let mut entries = fs::read_dir(current)
            .expect("snapshot directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("snapshot entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            files.push(path.strip_prefix(root).expect("relative").to_path_buf());
            if path.is_dir() {
                walk(root, &path, files);
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files
}
