use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, IntegrationEnvironment, RootCommand, SetupRuntime, execute_integrations,
    execute_setup, execute_setup_with_runtime,
};
use compresso_integrations::{
    CodexIntegration, FileReceiptStore, InstallCtx, InstallReceipt, Integration, PlanApproval,
    PlanClock, PlanManager, ReceiptError, ReceiptStore, Scope,
};
use compresso_store::{Store, StoreError};
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn test_setup_dry_run_renders_both_plans_without_any_writes() {
    let fixture = Fixture::new();
    let before = snapshot(fixture.directory.path());
    let changes_before = fixture
        .store
        .total_changes()
        .expect("change count should load");

    let (exit, output, errors) = fixture.run_setup_json(
        &["setup", "codex", ".", "--dry-run", "--json"],
        None,
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["data"]["action"], "setup");
    assert_eq!(output["data"]["client"], "codex");
    assert_eq!(output["data"]["dry_run"], true);
    assert_eq!(output["data"]["root"], canonical_text(&fixture.root));
    assert_eq!(output["data"]["plan"]["repository"]["action"], "index");
    assert_eq!(output["data"]["plan"]["repository"]["dry_run"], true);
    let diff = output["data"]["plan"]["integration"]["unified_diff"]
        .as_str()
        .expect("integration diff should be present");
    assert!(diff.contains("mcp_servers.compresso"));
    assert!(diff.contains("args = [\"mcp\", \"serve\"]"));
    assert!(!diff.contains("--root"));
    assert!(output["data"]["result"].is_null());
    assert_eq!(snapshot(fixture.directory.path()), before);
    assert_eq!(
        fixture
            .store
            .total_changes()
            .expect("change count should load"),
        changes_before
    );
    assert!(
        fixture
            .store
            .approved_repositories()
            .expect("repositories should list")
            .is_empty()
    );
    assert!(!fixture.codex_config().exists());
    assert!(!fixture.receipt_store().exists());
}

#[test]
fn test_setup_codex_dry_run_redacts_unrelated_authorization_and_target_document() {
    const CANARY: &str = "CUBIC_BEARER_CANARY_DO_NOT_PRINT_347d10be";

    let fixture = Fixture::new();
    let config = fixture.codex_config();
    fs::create_dir_all(config.parent().expect("Codex config parent"))
        .expect("Codex config parent should be created");
    let old_command = fixture.directory.path().join("old/bin/compresso");
    let old_command_literal =
        serde_json::to_string(&old_command.to_string_lossy()).expect("old command should encode");
    let new_command_literal =
        serde_json::to_string(&fixture.environment.compresso_bin.to_string_lossy())
            .expect("new command should encode");
    let original = format!(
        "# preserve this exact comment\n[mcp_servers.cubic]\nhttp_headers = {{ Authorization = \"Bearer {CANARY}\" }}\n\n[mcp_servers.compresso]\ncommand = {}\nargs = [\"mcp\", \"serve\"]\n",
        old_command_literal,
    );
    fs::write(&config, &original).expect("secret-bearing Codex fixture should be written");

    let (exit, output, errors) = fixture.run_setup_json(
        &["setup", "codex", fixture.root_str(), "--dry-run", "--json"],
        None,
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(errors.is_null());
    let serialized = serde_json::to_string(&output).expect("setup preview should encode");
    assert!(!serialized.contains(CANARY));
    assert!(!serialized.contains("Authorization"));
    let integration = &output["data"]["plan"]["integration"];
    assert!(integration.get("target_contents").is_none());
    let diff = integration["unified_diff"]
        .as_str()
        .expect("safe integration diff should be present");
    assert!(diff.contains(&old_command_literal));
    assert!(diff.contains(&new_command_literal));
    assert!(!diff.contains("mcp_servers.cubic"));
    assert!(!diff.contains("preserve this exact comment"));
    assert!(
        diff.lines().count() <= 10,
        "preview was not minimal: {diff}"
    );
    assert_eq!(
        fs::read_to_string(config).expect("Codex config should remain readable"),
        original
    );
}

#[test]
fn test_setup_accepts_only_the_three_coding_clients() {
    for unsupported in ["claude-desktop", "unknown"] {
        assert!(
            Cli::try_parse_from(["compresso", "setup", unsupported, "."]).is_err(),
            "{unsupported} must not parse as a setup client"
        );
    }
}

#[test]
fn test_setup_decline_prompts_once_and_mutates_nothing() {
    let fixture = Fixture::new();
    let before = snapshot(fixture.directory.path());
    let mut input = Cursor::new(b"no\n".as_slice());

    let (exit, output, errors) = fixture.run_setup(
        &["setup", "codex", fixture.root_str()],
        Some(&fixture.store),
        &mut input,
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(output.contains("Setup plan for codex"));
    assert!(output.contains("Would index"));
    assert!(output.contains("mcp_servers.compresso"));
    assert!(output.contains("Cancelled; no setup changes were made."));
    assert_eq!(errors.matches("Apply setup? [y/N]").count(), 1);
    assert!(!errors.contains("Apply? [y/N]"));
    assert_eq!(snapshot(fixture.directory.path()), before);
    assert!(
        fixture
            .store
            .approved_repositories()
            .expect("repositories should list")
            .is_empty()
    );
}

#[test]
fn test_setup_cold_home_decline_does_not_open_or_create_default_store() {
    let fixture = ColdFixture::new();
    let before = snapshot(fixture.directory.path());
    let runtime = TestSetupRuntime::fixed(fixture.database_path(), 1_000);
    let mut input = Cursor::new(b"no\n".as_slice());

    let (exit, output, errors) = run_setup_with_runtime(
        &["setup", "codex", fixture.root_str()],
        None,
        &fixture.environment,
        &runtime,
        &mut input,
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(output.contains("Cancelled; no setup changes were made."));
    assert_eq!(errors.matches("Apply setup? [y/N]").count(), 1);
    assert_eq!(runtime.open_count(), 0);
    assert!(!fixture.environment.data_dir.exists());
    assert_eq!(snapshot(fixture.directory.path()), before);
}

#[test]
fn test_setup_cold_home_json_without_yes_does_not_open_or_create_default_store() {
    let fixture = ColdFixture::new();
    let before = snapshot(fixture.directory.path());
    let runtime = TestSetupRuntime::fixed(fixture.database_path(), 1_000);

    let (exit, output, errors) = run_setup_json_with_runtime(
        &["setup", "codex", fixture.root_str(), "--json"],
        None,
        &fixture.environment,
        &runtime,
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(output["data"]["action"], "setup");
    assert!(output["data"]["result"].is_null());
    assert_eq!(errors["error"]["code"], "confirmation_required");
    assert_eq!(runtime.open_count(), 0);
    assert!(!fixture.environment.data_dir.exists());
    assert_eq!(snapshot(fixture.directory.path()), before);
}

#[test]
fn test_setup_real_install_is_idempotent_and_uninstall_keeps_repository_indexed() {
    let fixture = Fixture::new();

    let (exit, first, errors) = fixture.run_setup_json(
        &["setup", "codex", fixture.root_str(), "--yes", "--json"],
        Some(&fixture.store),
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(first["data"]["result"]["status"], "complete");
    assert_eq!(
        first["data"]["result"]["repository"]["report"]["files_indexed"],
        1
    );
    assert_eq!(first["data"]["result"]["integration"]["installed"], true);
    let first_integration = &first["data"]["result"]["integration"];
    assert_eq!(first_integration["already_applied"], false);
    assert_eq!(
        first_integration["applied_plan_hash"],
        first_integration["effective_plan_hash"]
    );
    assert_eq!(
        first_integration["receipt_plan_hash"],
        first_integration["effective_plan_hash"]
    );
    let first_receipt_plan_hash = first_integration["receipt_plan_hash"]
        .as_str()
        .expect("first setup should report its receipt hash")
        .to_owned();
    let canonical = fs::canonicalize(&fixture.root).expect("root should canonicalize");
    let repository = fixture
        .store
        .repo_by_root(&canonical)
        .expect("repository lookup should work")
        .expect("repository should be approved");
    assert_eq!(
        fixture
            .store
            .indexed_files(repository.id)
            .expect("indexed files should load")
            .len(),
        1
    );
    let first_config = fs::read(fixture.codex_config()).expect("Codex config should exist");
    let first_text = String::from_utf8(first_config.clone()).expect("config should be UTF-8");
    assert!(first_text.contains("args = [\"mcp\", \"serve\"]"));
    assert!(!first_text.contains("--root"));
    assert!(fixture.receipt_store().is_dir());

    let (exit, second, errors) = fixture.run_setup_json(
        &["setup", "codex", fixture.root_str(), "--yes", "--json"],
        Some(&fixture.store),
        &mut std::io::empty(),
    );
    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(second["data"]["result"]["status"], "complete");
    let second_integration = &second["data"]["result"]["integration"];
    assert_eq!(second_integration["already_applied"], true);
    assert!(
        second_integration["applied_plan_hash"].is_null(),
        "an idempotent setup must not claim that its effective plan committed"
    );
    assert_eq!(
        second_integration["receipt_plan_hash"],
        first_receipt_plan_hash
    );
    assert_ne!(
        second_integration["effective_plan_hash"], second_integration["receipt_plan_hash"],
        "the current checked plan and the older active receipt need distinct identities"
    );
    assert_eq!(
        second["data"]["result"]["repository"]["report"]["files_indexed"],
        0
    );
    assert_eq!(
        fs::read(fixture.codex_config()).expect("Codex config should remain"),
        first_config
    );
    assert_eq!(
        fixture
            .store
            .approved_repositories()
            .expect("repositories should list")
            .len(),
        1
    );

    let (exit, output, errors) =
        fixture.run_integration(&["integrations", "uninstall", "codex", "--yes", "--json"]);
    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert!(!output["data"]["result"].is_null());
    assert!(
        !fs::read_to_string(fixture.codex_config())
            .expect("Codex config should remain parseable")
            .contains("mcp_servers.compresso")
    );
    assert!(
        fixture
            .store
            .repo_by_root(&canonical)
            .expect("repository lookup should work")
            .is_some()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("src/lib.rs"))
            .expect("source should remain untouched"),
        "pub fn indexed() {}\n"
    );
}

#[test]
fn test_setup_reissues_materially_identical_plan_after_original_expiry_without_sleeping() {
    let fixture = Fixture::new();
    // The approved plan is issued at 1_000 and expires at 1_600. Indexing then
    // advances the deterministic clock to exactly 1_600; only a fresh plan can
    // be applied successfully.
    let runtime = TestSetupRuntime::new(
        fixture.environment.data_dir.join("compresso.db"),
        Arc::new(SequenceClock::new([1_000, 1_000, 1_600])),
    );

    let (exit, output, errors) = fixture.run_setup_json_with_runtime(
        &["setup", "codex", fixture.root_str(), "--yes", "--json"],
        Some(&fixture.store),
        &runtime,
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(errors.is_null());
    let approved = output["data"]["plan"]["integration"]["plan_hash"]
        .as_str()
        .expect("approved hash should be present");
    let result = &output["data"]["result"]["integration"];
    assert_eq!(result["approved_plan_hash"], approved);
    assert_eq!(result["plan_refreshed"], true);
    assert_ne!(result["applied_plan_hash"], approved);
    assert_eq!(result["plan_hash"], result["applied_plan_hash"]);
    assert_eq!(runtime.open_count(), 0);
}

#[test]
fn test_setup_plan_renewal_preserves_approved_diff_and_backup_identity() {
    let fixture = Fixture::new();
    let original = b"[mcp_servers.other]\ncommand = \"/opt/other\"\n";
    fs::create_dir_all(
        fixture
            .codex_config()
            .parent()
            .expect("Codex config should have a parent"),
    )
    .expect("Codex config parent should be created");
    fs::write(fixture.codex_config(), original).expect("existing Codex config should be written");
    // The approved plan expires while indexing. Renewal may extend its timing
    // window, but must retain the exact previewed diff and backup destination.
    let runtime = TestSetupRuntime::new(
        fixture.environment.data_dir.join("compresso.db"),
        Arc::new(SequenceClock::new([1_000, 1_000, 1_600])),
    );

    let (exit, output, errors) = fixture.run_setup_json_with_runtime(
        &["setup", "codex", fixture.root_str(), "--yes", "--json"],
        Some(&fixture.store),
        &runtime,
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(errors.is_null());
    let approved = &output["data"]["plan"]["integration"];
    let installed = &output["data"]["result"]["integration"];
    assert_eq!(installed["already_applied"], false);
    assert_eq!(installed["plan_refreshed"], true);
    assert_ne!(
        installed["effective_plan_hash"], approved["plan_hash"],
        "renewal should have a fresh expiry-bound plan identity"
    );
    assert_eq!(
        installed["effective_diff_hash"], approved["diff_hash"],
        "renewal must preserve the exact diff that the user approved"
    );
    assert_eq!(
        installed["effective_backup_path"], approved["backup_path"],
        "renewal must preserve the backup destination that the user approved"
    );
    assert_eq!(
        installed["applied_plan_hash"],
        installed["effective_plan_hash"]
    );
    assert_eq!(
        installed["receipt_plan_hash"],
        installed["effective_plan_hash"]
    );
    assert!(approved.get("target_contents").is_none());
    let installed_config =
        fs::read_to_string(fixture.codex_config()).expect("installed config should be readable");
    assert!(
        installed_config.starts_with(std::str::from_utf8(original).expect("fixture is UTF-8")),
        "unrelated Codex formatting must remain byte-preserved"
    );
    let installed_toml: toml::Value =
        toml::from_str(&installed_config).expect("installed config should remain valid TOML");
    assert_eq!(
        installed_toml["mcp_servers"]["compresso"]["command"].as_str(),
        fixture.environment.compresso_bin.to_str()
    );

    let receipts = FileReceiptStore::new(fixture.receipt_store_path());
    let receipt = receipts
        .active_for("codex", &fixture.codex_config())
        .expect("active receipt lookup should succeed")
        .expect("setup should persist an active receipt");
    let approved_backup = approved["backup_path"]
        .as_str()
        .expect("approved backup path should be text");
    assert_eq!(receipt.backup_path, approved_backup);
    assert_eq!(installed["receipt_backup_path"], approved_backup);
    assert_eq!(
        receipt.plan_hash,
        installed["receipt_plan_hash"]
            .as_str()
            .expect("receipt hash should be reported")
    );
    assert_eq!(
        fs::read(approved_backup).expect("approved backup should be created"),
        original
    );
}

#[test]
fn test_setup_rejects_valid_pending_journal_injected_after_final_plan() {
    let fixture = Fixture::new();
    let (journal_path, journal_bytes) = authenticated_install_journal(&fixture);
    let clock = Arc::new(JournalInjectClock::new(journal_path.clone(), journal_bytes));
    let runtime = TestSetupRuntime::new(
        fixture.environment.data_dir.join("compresso.db"),
        clock.clone(),
    );

    let (exit, output, errors) = fixture.run_setup_json_with_runtime(
        &["setup", "codex", fixture.root_str(), "--yes", "--json"],
        Some(&fixture.store),
        &runtime,
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Failure, "{output}");
    assert!(
        clock.injected(),
        "journal should be injected after final planning"
    );
    assert_eq!(errors["error"]["code"], "setup_partial_failure");
    assert_eq!(output["data"]["result"]["status"], "partial_failure");
    assert!(
        !fixture.codex_config().exists(),
        "setup must not recover or apply a newly appeared journal implicitly"
    );
    let receipts = FileReceiptStore::new(fixture.receipt_store_path());
    assert!(
        receipts
            .all()
            .expect("receipt snapshot should load")
            .is_empty(),
        "setup must not persist the pending journal's receipt"
    );
    assert!(
        journal_path.exists(),
        "the authenticated journal should remain available for explicit recovery"
    );

    // Prove this was a structurally valid, correctly authenticated journal rather
    // than a marker file that setup rejected on existence alone. These mutations
    // occur only after all post-setup fail-closed assertions above.
    let recovered = PlanManager::new("explicit-journal-fixture-proof")
        .expect("recovery manager should initialize")
        .recover(&fixture.codex_config(), &receipts)
        .expect("authenticated journal should recover")
        .expect("pending journal should produce a recovery report");
    assert!(!recovered.already_applied);
    assert!(fixture.codex_config().exists());
    assert_eq!(
        receipts.all().expect("recovered receipt should load").len(),
        1
    );
    assert!(!journal_path.exists());
}

#[test]
fn test_setup_rejects_material_drift_after_authorization_and_indexing() {
    let fixture = Fixture::new();
    let runtime = TestSetupRuntime::new(
        fixture.environment.data_dir.join("compresso.db"),
        Arc::new(SequenceClock::new([1_000, 1_000, 1_600])),
    )
    .with_open_write(
        fixture.codex_config(),
        b"[mcp_servers.other]\ncommand = \"/opt/other\"\n".to_vec(),
    );

    let (exit, output, errors) = fixture.run_setup_json_with_runtime(
        &["setup", "codex", fixture.root_str(), "--yes", "--json"],
        None,
        &runtime,
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(runtime.open_count(), 1);
    assert_eq!(output["data"]["result"]["status"], "partial_failure");
    let integration = &output["data"]["result"]["integration"];
    assert_eq!(integration["state"], "uncertain");
    assert_eq!(integration["outcome_known"], false);
    assert_eq!(integration["recovery_required"], true);
    assert_eq!(integration["cause"]["code"], "config_drift");
    assert!(!integration["approved_plan_hash"].is_null());
    assert!(integration["attempted_plan_hash"].is_null());
    assert_eq!(errors["error"]["code"], "setup_partial_failure");
    assert!(
        errors["error"]["message"]
            .as_str()
            .expect("error message should be text")
            .contains("compresso integrations install codex --yes")
    );
    assert!(
        !errors["error"]["message"]
            .as_str()
            .expect("error message should be text")
            .contains("--dry-run")
    );
    let canonical = fs::canonicalize(&fixture.root).expect("root should canonicalize");
    assert!(
        fixture
            .store
            .repo_by_root(&canonical)
            .expect("repository lookup should work")
            .is_some()
    );
    let config = fs::read_to_string(fixture.codex_config()).expect("drift config should remain");
    assert!(config.contains("mcp_servers.other"));
    assert!(!config.contains("mcp_servers.compresso"));
}

#[test]
fn test_setup_project_clients_pin_exact_canonical_root() {
    for (client, config_relative) in [("claude-code", ".mcp.json"), ("cursor", ".cursor/mcp.json")]
    {
        let fixture = Fixture::new();
        let (exit, output, errors) = fixture.run_setup_json(
            &["setup", client, fixture.root_str(), "--yes", "--json"],
            Some(&fixture.store),
            &mut std::io::empty(),
        );
        assert_eq!(exit, CliExitCode::Success, "{client}: {errors}");
        assert!(errors.is_null(), "{client}: {errors}");
        assert_eq!(output["data"]["result"]["status"], "complete");
        let config: Value = serde_json::from_slice(
            &fs::read(fixture.root.join(config_relative)).expect("project config should exist"),
        )
        .expect("project config should be JSON");
        assert_eq!(
            config["mcpServers"]["compresso"]["args"],
            serde_json::json!(["mcp", "serve", "--root", canonical_text(&fixture.root)]),
            "{client}"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_setup_passes_normalized_stable_launcher_path_to_client_config() {
    let fixture = Fixture::new();
    let launcher_directory = fixture.directory.path().join("launchers");
    fs::create_dir(&launcher_directory).expect("launcher directory should be created");
    let launcher = launcher_directory.join("compresso");
    std::os::unix::fs::symlink(&fixture.environment.compresso_bin, &launcher)
        .expect("stable launcher should be linked");
    let raw_launcher = PathBuf::from(format!(
        "{}/./launchers//compresso",
        fixture.directory.path().display()
    ));
    let mut environment = fixture.environment.clone();
    environment.compresso_bin = raw_launcher.clone();
    let runtime = TestSetupRuntime::fixed(environment.data_dir.join("unused.db"), 1_000);

    let (exit, output, errors) = run_setup_json_with_runtime(
        &["setup", "codex", fixture.root_str(), "--dry-run", "--json"],
        None,
        &environment,
        &runtime,
        &mut std::io::empty(),
    );

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(errors.is_null());
    let preview = output["data"]["plan"]["integration"]["unified_diff"]
        .as_str()
        .expect("safe preview should be text");
    assert!(preview.contains(&launcher.to_string_lossy().into_owned()));
    assert!(!preview.contains(&raw_launcher.to_string_lossy().into_owned()));
    assert!(
        output["data"]["plan"]["integration"]
            .get("target_contents")
            .is_none()
    );
    assert_eq!(runtime.open_count(), 0);
}

#[cfg(unix)]
#[test]
fn test_setup_revalidates_project_config_targets_after_index_before_apply() {
    for client in ["claude-code", "cursor"] {
        let fixture = Fixture::new();
        let (swap, target, protected_file) = if client == "claude-code" {
            let outside = fixture.directory.path().join("outside-claude.json");
            fs::write(&outside, b"{\"protected\":true}\n")
                .expect("outside Claude config should be written");
            (
                ProjectTargetSwap::Claude {
                    target: fixture.root.join(".mcp.json"),
                    outside: outside.clone(),
                },
                fixture.root.join(".mcp.json"),
                outside,
            )
        } else {
            let outside_directory = fixture.directory.path().join("outside-cursor");
            fs::create_dir(&outside_directory).expect("outside Cursor directory should exist");
            let outside_config = outside_directory.join("mcp.json");
            fs::write(&outside_config, b"{\"protected\":true}\n")
                .expect("outside Cursor config should be written");
            (
                ProjectTargetSwap::Cursor {
                    target_directory: fixture.root.join(".cursor"),
                    outside_directory,
                },
                fixture.root.join(".cursor"),
                outside_config,
            )
        };
        let protected_before = fs::read(&protected_file).expect("outside bytes should be readable");
        let runtime =
            TestSetupRuntime::fixed(fixture.environment.data_dir.join("compresso.db"), 1_000)
                .with_open_swap(swap);

        let (exit, output, errors) = fixture.run_setup_json_with_runtime(
            &["setup", client, fixture.root_str(), "--yes", "--json"],
            None,
            &runtime,
            &mut std::io::empty(),
        );

        assert_eq!(exit, CliExitCode::Failure, "{client}: {output}");
        assert_eq!(errors["error"]["code"], "setup_partial_failure", "{client}");
        assert_eq!(output["data"]["result"]["status"], "partial_failure");
        assert_eq!(
            output["data"]["result"]["integration"]["state"], "not_applied",
            "{client}"
        );
        assert_eq!(
            fs::read(&protected_file).expect("outside bytes should remain readable"),
            protected_before,
            "{client}"
        );
        assert!(
            fs::symlink_metadata(&target)
                .expect("swapped target should remain present")
                .file_type()
                .is_symlink(),
            "{client}"
        );
        let canonical = fs::canonicalize(&fixture.root).expect("root should canonicalize");
        assert!(
            fixture
                .store
                .repo_by_root(&canonical)
                .expect("repository lookup should work")
                .is_some(),
            "{client}"
        );
        assert!(!fixture.receipt_store().exists(), "{client}");
    }
}

#[test]
fn test_setup_authorization_rejects_pre_index_config_drift() {
    let fixture = Fixture::new();
    let mut input = DriftOnApproval::new(fixture.codex_config());

    let (exit, output, errors) = fixture.run_setup(
        &["setup", "codex", fixture.root_str()],
        Some(&fixture.store),
        &mut input,
    );

    assert_eq!(exit, CliExitCode::Failure);
    assert!(!output.contains("Indexed"));
    assert!(!output.contains("Setup complete"));
    assert_eq!(errors.matches("Apply setup? [y/N]").count(), 1);
    assert!(errors.contains("error[config_drift]"));
    assert!(!errors.contains("setup_partial_failure"));
    let canonical = fs::canonicalize(&fixture.root).expect("root should canonicalize");
    assert!(
        fixture
            .store
            .repo_by_root(&canonical)
            .expect("repository lookup should work")
            .is_none()
    );
    let config = fs::read_to_string(fixture.codex_config()).expect("drift config should remain");
    assert!(config.contains("mcp_servers.other"));
    assert!(!config.contains("mcp_servers.compresso"));
    assert_eq!(
        fs::read_to_string(fixture.root.join("src/lib.rs"))
            .expect("source should remain untouched"),
        "pub fn indexed() {}\n"
    );
    assert!(!fixture.receipt_store().exists());
}

#[cfg(unix)]
#[test]
fn test_setup_rejects_repository_symlink_retarget_after_preview() {
    let fixture = Fixture::new();
    let replacement = fixture.directory.path().join("replacement-repo");
    fs::create_dir(&replacement).expect("replacement repository should be created");
    fs::write(
        replacement.join("replacement.rs"),
        "pub fn replacement() {}\n",
    )
    .expect("replacement source should be written");
    let parked = fixture.directory.path().join("original-repo");
    let mut input =
        RetargetRootOnApproval::new(fixture.root.clone(), parked.clone(), replacement.clone());
    let unopened_database = fixture.directory.path().join("unopened/compresso.db");
    let runtime = TestSetupRuntime::fixed(unopened_database.clone(), 1_000);

    let (exit, output, errors) = fixture.run_setup_with_runtime(
        &["setup", "codex", fixture.root_str()],
        None,
        &runtime,
        &mut input,
    );

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.contains("Setup plan for codex"));
    assert!(!output.contains("Indexed"));
    assert!(errors.contains("setup_root_changed"));
    assert_eq!(runtime.open_count(), 0);
    assert!(!unopened_database.exists());
    assert!(
        fixture
            .store
            .approved_repositories()
            .expect("repositories should list")
            .is_empty()
    );
    assert!(!fixture.codex_config().exists());
    assert_eq!(
        fs::read_to_string(parked.join("src/lib.rs")).expect("original source should remain"),
        "pub fn indexed() {}\n"
    );
    assert_eq!(
        fs::read_to_string(replacement.join("replacement.rs"))
            .expect("replacement source should remain"),
        "pub fn replacement() {}\n"
    );
}

struct Fixture {
    directory: TempDir,
    root: PathBuf,
    store: Store,
    environment: IntegrationEnvironment,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let home = directory.path().join("home");
        let root = directory.path().join("repo");
        let data = directory.path().join("data");
        fs::create_dir(&home).expect("home should be created");
        fs::create_dir_all(root.join("src")).expect("repository should be created");
        fs::write(root.join("src/lib.rs"), "pub fn indexed() {}\n")
            .expect("source should be written");
        let executable = std::env::current_exe()
            .and_then(fs::canonicalize)
            .expect("test executable should resolve");
        let environment = IntegrationEnvironment::new(
            fs::canonicalize(&home).expect("home should canonicalize"),
            fs::canonicalize(&root).expect("root should canonicalize"),
            executable,
            data.clone(),
            Vec::new(),
            "setup-cli-test",
        );
        let store = Store::open(&data.join("compresso.db")).expect("store should open");
        Self {
            directory,
            root,
            store,
            environment,
        }
    }

    fn root_str(&self) -> &str {
        self.root.to_str().expect("temporary root should be UTF-8")
    }

    fn codex_config(&self) -> PathBuf {
        self.environment.home_dir.join(".codex/config.toml")
    }

    fn receipt_store(&self) -> PathBuf {
        self.environment
            .data_dir
            .join("integration-receipts.json.d")
    }

    fn receipt_store_path(&self) -> PathBuf {
        self.environment.data_dir.join("integration-receipts.json")
    }

    fn run_setup(
        &self,
        arguments: &[&str],
        store: Option<&Store>,
        input: &mut dyn Read,
    ) -> (CliExitCode, String, String) {
        let cli = parse(arguments);
        let RootCommand::Setup(arguments) = cli.command else {
            panic!("expected setup command");
        };
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_setup(
            arguments,
            store,
            &self.environment,
            input,
            &mut output,
            &mut errors,
        );
        (
            exit,
            String::from_utf8(output).expect("stdout should be UTF-8"),
            String::from_utf8(errors).expect("stderr should be UTF-8"),
        )
    }

    fn run_setup_json(
        &self,
        arguments: &[&str],
        store: Option<&Store>,
        input: &mut dyn Read,
    ) -> (CliExitCode, Value, Value) {
        let (exit, output, errors) = self.run_setup(arguments, store, input);
        (exit, parse_or_null(&output), parse_or_null(&errors))
    }

    fn run_setup_with_runtime(
        &self,
        arguments: &[&str],
        store: Option<&Store>,
        runtime: &dyn SetupRuntime,
        input: &mut dyn Read,
    ) -> (CliExitCode, String, String) {
        run_setup_with_runtime(arguments, store, &self.environment, runtime, input)
    }

    fn run_setup_json_with_runtime(
        &self,
        arguments: &[&str],
        store: Option<&Store>,
        runtime: &dyn SetupRuntime,
        input: &mut dyn Read,
    ) -> (CliExitCode, Value, Value) {
        let (exit, output, errors) = self.run_setup_with_runtime(arguments, store, runtime, input);
        (exit, parse_or_null(&output), parse_or_null(&errors))
    }

    fn run_integration(&self, arguments: &[&str]) -> (CliExitCode, Value, Value) {
        let cli = parse(arguments);
        let RootCommand::Integrations { command } = cli.command else {
            panic!("expected integration command");
        };
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_integrations(
            command,
            &self.environment,
            &mut std::io::empty(),
            &mut output,
            &mut errors,
        );
        (
            exit,
            parse_or_null(&String::from_utf8(output).expect("stdout should be UTF-8")),
            parse_or_null(&String::from_utf8(errors).expect("stderr should be UTF-8")),
        )
    }
}

struct ColdFixture {
    directory: TempDir,
    root: PathBuf,
    environment: IntegrationEnvironment,
}

impl ColdFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let home = directory.path().join("home");
        let root = directory.path().join("repo");
        let data = directory.path().join("cold-data");
        fs::create_dir(&home).expect("home should be created");
        fs::create_dir_all(root.join("src")).expect("repository should be created");
        fs::write(root.join("src/lib.rs"), "pub fn indexed() {}\n")
            .expect("source should be written");
        let executable = std::env::current_exe()
            .and_then(fs::canonicalize)
            .expect("test executable should resolve");
        let environment = IntegrationEnvironment::new(
            fs::canonicalize(&home).expect("home should canonicalize"),
            fs::canonicalize(&root).expect("root should canonicalize"),
            executable,
            data,
            Vec::new(),
            "cold-setup-cli-test",
        );
        Self {
            directory,
            root,
            environment,
        }
    }

    fn root_str(&self) -> &str {
        self.root.to_str().expect("temporary root should be UTF-8")
    }

    fn database_path(&self) -> PathBuf {
        self.environment.data_dir.join("compresso.db")
    }
}

fn run_setup_with_runtime(
    arguments: &[&str],
    store: Option<&Store>,
    environment: &IntegrationEnvironment,
    runtime: &dyn SetupRuntime,
    input: &mut dyn Read,
) -> (CliExitCode, String, String) {
    let cli = parse(arguments);
    let RootCommand::Setup(arguments) = cli.command else {
        panic!("expected setup command");
    };
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let exit = execute_setup_with_runtime(
        arguments,
        store,
        environment,
        runtime,
        input,
        &mut output,
        &mut errors,
    );
    (
        exit,
        String::from_utf8(output).expect("stdout should be UTF-8"),
        String::from_utf8(errors).expect("stderr should be UTF-8"),
    )
}

fn run_setup_json_with_runtime(
    arguments: &[&str],
    store: Option<&Store>,
    environment: &IntegrationEnvironment,
    runtime: &dyn SetupRuntime,
    input: &mut dyn Read,
) -> (CliExitCode, Value, Value) {
    let (exit, output, errors) =
        run_setup_with_runtime(arguments, store, environment, runtime, input);
    (exit, parse_or_null(&output), parse_or_null(&errors))
}

struct TestSetupRuntime {
    database_path: PathBuf,
    clock: Arc<dyn PlanClock>,
    opens: AtomicUsize,
    open_write: Mutex<Option<(PathBuf, Vec<u8>)>>,
    #[cfg(unix)]
    open_swap: Mutex<Option<ProjectTargetSwap>>,
}

impl TestSetupRuntime {
    fn new(database_path: PathBuf, clock: Arc<dyn PlanClock>) -> Self {
        Self {
            database_path,
            clock,
            opens: AtomicUsize::new(0),
            open_write: Mutex::new(None),
            #[cfg(unix)]
            open_swap: Mutex::new(None),
        }
    }

    fn fixed(database_path: PathBuf, now: i64) -> Self {
        Self::new(database_path, Arc::new(SequenceClock::new([now])))
    }

    fn with_open_write(self, path: PathBuf, contents: Vec<u8>) -> Self {
        *self
            .open_write
            .lock()
            .expect("open-write action lock should remain available") = Some((path, contents));
        self
    }

    #[cfg(unix)]
    fn with_open_swap(self, swap: ProjectTargetSwap) -> Self {
        *self
            .open_swap
            .lock()
            .expect("open-swap action lock should remain available") = Some(swap);
        self
    }

    fn open_count(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }
}

impl SetupRuntime for TestSetupRuntime {
    fn open_default_store(&self) -> Result<Store, StoreError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        if let Some((path, contents)) = self
            .open_write
            .lock()
            .expect("open-write action lock should remain available")
            .take()
        {
            fs::create_dir_all(path.parent().expect("config path should have a parent"))
                .expect("config parent should be created");
            fs::write(path, contents).expect("open-write action should succeed");
        }
        #[cfg(unix)]
        if let Some(swap) = self
            .open_swap
            .lock()
            .expect("open-swap action lock should remain available")
            .take()
        {
            swap.apply();
        }
        Store::open(&self.database_path)
    }

    fn plan_clock(&self) -> Arc<dyn PlanClock> {
        Arc::clone(&self.clock)
    }
}

struct SequenceClock {
    values: Vec<i64>,
    next: AtomicUsize,
}

impl SequenceClock {
    fn new(values: impl IntoIterator<Item = i64>) -> Self {
        let values = values.into_iter().collect::<Vec<_>>();
        assert!(!values.is_empty(), "clock sequence must not be empty");
        Self {
            values,
            next: AtomicUsize::new(0),
        }
    }
}

impl PlanClock for SequenceClock {
    fn now_unix_seconds(&self) -> i64 {
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        self.values
            .get(index)
            .or_else(|| self.values.last())
            .copied()
            .expect("clock sequence is non-empty")
    }
}

struct JournalInjectClock {
    calls: AtomicUsize,
    journal_path: PathBuf,
    journal_bytes: Vec<u8>,
}

impl JournalInjectClock {
    fn new(journal_path: PathBuf, journal_bytes: Vec<u8>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            journal_path,
            journal_bytes,
        }
    }

    fn injected(&self) -> bool {
        self.calls.load(Ordering::SeqCst) >= 3 && self.journal_path.exists()
    }
}

impl PlanClock for JournalInjectClock {
    fn now_unix_seconds(&self) -> i64 {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 2 {
            fs::create_dir_all(
                self.journal_path
                    .parent()
                    .expect("journal should have a parent"),
            )
            .expect("journal parent should be created");
            fs::write(&self.journal_path, &self.journal_bytes)
                .expect("authenticated journal should be injected");
        }
        if call < 2 { 1_000 } else { 1_600 }
    }
}

fn authenticated_install_journal(fixture: &Fixture) -> (PathBuf, Vec<u8>) {
    let manager = PlanManager::new("authenticated-journal-fixture")
        .expect("journal fixture manager should initialize");
    let adapter = CodexIntegration::new(
        fixture.environment.home_dir.clone(),
        fixture.environment.path_entries.clone(),
        None,
        "setup-cli-test",
    )
    .expect("Codex adapter should initialize");
    let plan = adapter
        .plan_install(
            &manager,
            &InstallCtx {
                compresso_bin: fixture.environment.compresso_bin.clone(),
                scope: Scope::User,
            },
        )
        .expect("internal journal plan should be issued");
    let receipts = FileReceiptStore::new(fixture.receipt_store_path());
    let file_name = plan
        .config_path
        .file_name()
        .expect("config path should have a file name")
        .to_string_lossy();
    let journal_path = plan
        .config_path
        .with_file_name(format!("{file_name}.compresso-journal"));
    let failing_store = FailAfterJournalStore { inner: &receipts };
    let error = manager
        .apply_install(&PlanApproval::from(&plan), &failing_store)
        .expect_err("fixture store should stop after the target and journal are written");
    assert!(matches!(
        error,
        compresso_integrations::IntegrationError::Receipt(ReceiptError::Invalid { .. })
    ));
    let journal_bytes =
        fs::read(&journal_path).expect("production apply should leave an authenticated journal");
    fs::remove_file(&plan.config_path).expect("fixture should restore the missing base target");
    fs::remove_file(&journal_path).expect("fixture should defer journal injection to the clock");
    assert!(
        receipts
            .all()
            .expect("receipt snapshot should load")
            .is_empty(),
        "the failing store must not persist a receipt"
    );
    (journal_path, journal_bytes)
}

struct FailAfterJournalStore<'a> {
    inner: &'a FileReceiptStore,
}

impl ReceiptStore for FailAfterJournalStore<'_> {
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

    fn save(&self, _receipt: &InstallReceipt) -> Result<(), ReceiptError> {
        Err(ReceiptError::Invalid {
            reason: "intentional post-journal fixture failure".to_owned(),
        })
    }

    fn seal_journal(&self, payload: &[u8]) -> Result<String, ReceiptError> {
        self.inner.seal_journal(payload)
    }

    fn verify_journal(&self, payload: &[u8], mac: &str) -> Result<(), ReceiptError> {
        self.inner.verify_journal(payload, mac)
    }
}

#[cfg(unix)]
enum ProjectTargetSwap {
    Claude {
        target: PathBuf,
        outside: PathBuf,
    },
    Cursor {
        target_directory: PathBuf,
        outside_directory: PathBuf,
    },
}

#[cfg(unix)]
impl ProjectTargetSwap {
    fn apply(self) {
        match self {
            Self::Claude { target, outside } => {
                std::os::unix::fs::symlink(outside, target)
                    .expect("Claude project config should be swapped to a symlink");
            }
            Self::Cursor {
                target_directory,
                outside_directory,
            } => {
                std::os::unix::fs::symlink(outside_directory, target_directory)
                    .expect("Cursor project directory should be swapped to a symlink");
            }
        }
    }
}

struct DriftOnApproval {
    config_path: PathBuf,
    response: &'static [u8],
    offset: usize,
    drifted: bool,
}

impl DriftOnApproval {
    fn new(config_path: PathBuf) -> Self {
        Self {
            config_path,
            response: b"yes\n",
            offset: 0,
            drifted: false,
        }
    }
}

impl Read for DriftOnApproval {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if !self.drifted {
            if let Some(parent) = self.config_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(
                &self.config_path,
                "[mcp_servers.other]\ncommand = \"/opt/other\"\n",
            )?;
            self.drifted = true;
        }
        if self.offset == self.response.len() || buffer.is_empty() {
            return Ok(0);
        }
        let count = buffer.len().min(self.response.len() - self.offset);
        buffer[..count].copy_from_slice(&self.response[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

#[cfg(unix)]
struct RetargetRootOnApproval {
    root: PathBuf,
    parked: PathBuf,
    replacement: PathBuf,
    response: &'static [u8],
    offset: usize,
    retargeted: bool,
}

#[cfg(unix)]
impl RetargetRootOnApproval {
    fn new(root: PathBuf, parked: PathBuf, replacement: PathBuf) -> Self {
        Self {
            root,
            parked,
            replacement,
            response: b"yes\n",
            offset: 0,
            retargeted: false,
        }
    }
}

#[cfg(unix)]
impl Read for RetargetRootOnApproval {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if !self.retargeted {
            fs::rename(&self.root, &self.parked)?;
            std::os::unix::fs::symlink(&self.replacement, &self.root)?;
            self.retargeted = true;
        }
        if self.offset == self.response.len() || buffer.is_empty() {
            return Ok(0);
        }
        let count = buffer.len().min(self.response.len() - self.offset);
        buffer[..count].copy_from_slice(&self.response[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

fn parse(arguments: &[&str]) -> Cli {
    let argv = std::iter::once(OsString::from("compresso"))
        .chain(arguments.iter().map(OsString::from))
        .collect::<Vec<_>>();
    Cli::try_parse_from(argv).expect("command should parse")
}

fn canonical_text(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("path should canonicalize")
        .to_string_lossy()
        .into_owned()
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    collect_snapshot(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn collect_snapshot(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
    let mut entries = fs::read_dir(directory)
        .expect("snapshot directory should be readable")
        .map(|entry| entry.expect("snapshot entry should be readable").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_snapshot(root, &path, files);
        } else {
            files.push((
                path.strip_prefix(root)
                    .expect("snapshot path should stay under root")
                    .to_path_buf(),
                fs::read(path).expect("snapshot file should be readable"),
            ));
        }
    }
}

fn parse_or_null(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(value).expect("command output should be JSON")
    }
}
