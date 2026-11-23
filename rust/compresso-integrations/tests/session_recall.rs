//! Session recall as an install-time default: each client's server entry may
//! enable recall over that client's own transcripts, and only those.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use compresso_integrations::{
    ClaudeCodeDiscovery, ClaudeCodeIntegration, CodexAdapter, CodexPlatform, ConfigFormat,
    ConfiguredServer, CursorApplicationEvidence, CursorCliEvidence, CursorDiscovery,
    CursorIntegration, CursorPlatform, InstallCtx, InstallReceipt, Integration, MemoryReceiptStore,
    MutationPlan, PlanApproval, PlanManager, ReceiptStore, Scope, read_configured_server,
    receipt_authenticates_server, server_recall_source,
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use toml_edit::DocumentMut;

struct Harness {
    _directory: TempDir,
    home: PathBuf,
    project: PathBuf,
    launcher: PathBuf,
    receipts: MemoryReceiptStore,
}

impl Harness {
    fn new() -> Self {
        let directory = tempdir().expect("temporary directory");
        let root = fs::canonicalize(directory.path()).expect("canonical temporary directory");
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&project).expect("project");
        let launcher = root.join("compresso");
        fs::write(&launcher, "#!/bin/sh\nexit 0\n").expect("launcher");
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).expect("executable");
        Self {
            _directory: directory,
            home,
            project,
            launcher,
            receipts: MemoryReceiptStore::default(),
        }
    }

    fn claude(&self) -> ClaudeCodeIntegration {
        ClaudeCodeIntegration::new(
            self.home.clone(),
            ClaudeCodeDiscovery::new(Vec::<PathBuf>::new(), None),
        )
    }

    fn codex(&self) -> CodexAdapter {
        CodexAdapter::with_platform(
            self.home.clone(),
            Vec::new(),
            None,
            "0.1.0",
            CodexPlatform::Linux,
        )
        .expect("Codex adapter")
    }

    fn cursor(&self) -> CursorIntegration {
        CursorIntegration::new(
            self.home.clone(),
            CursorDiscovery::new(
                CursorPlatform::Linux,
                Vec::<CursorCliEvidence>::new(),
                Vec::<CursorApplicationEvidence>::new(),
            ),
        )
    }

    fn plan(
        &self,
        adapter: &dyn Integration,
        manager: &PlanManager,
        scope: Scope,
        session_recall: bool,
    ) -> MutationPlan {
        adapter
            .plan_install(
                manager,
                &InstallCtx {
                    compresso_bin: self.launcher.clone(),
                    scope,
                    session_recall,
                },
            )
            .expect("install plan")
    }

    fn install(
        &self,
        adapter: &dyn Integration,
        scope: Scope,
        session_recall: bool,
        session: &str,
    ) -> (MutationPlan, InstallReceipt) {
        let manager = PlanManager::new(session)
            .expect("plan manager")
            .with_backup_dir(test_backup_dir());
        let plan = self.plan(adapter, &manager, scope, session_recall);
        let receipt = adapter
            .install(&manager, &PlanApproval::from(&plan), &self.receipts)
            .expect("install applies");
        (plan, receipt)
    }

    fn uninstall(&self, adapter: &dyn Integration, config: &Path, session: &str) {
        let manager = PlanManager::new(session)
            .expect("plan manager")
            .with_backup_dir(test_backup_dir());
        let receipt = self
            .receipts
            .active_for(adapter.client_id(), config)
            .expect("receipt lookup")
            .expect("active receipt");
        let plan = adapter
            .plan_uninstall(&manager, &receipt)
            .expect("uninstall plan");
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        let report = adapter
            .uninstall(&manager, &PlanApproval::from(&plan), &self.receipts)
            .expect("uninstall applies");
        assert_eq!(report.removed_keys.len(), 1);
        assert!(report.retained_keys.is_empty());
    }
}

fn json_args(path: &Path) -> Value {
    let document: Value = serde_json::from_str(
        fs::read_to_string(path)
            .expect("JSON config")
            .trim_start_matches('\u{feff}'),
    )
    .expect("valid JSON");
    document["mcpServers"]["compresso"]["args"].clone()
}

fn toml_args(path: &Path) -> Vec<String> {
    let document = fs::read_to_string(path)
        .expect("TOML config")
        .parse::<DocumentMut>()
        .expect("valid TOML");
    document["mcp_servers"]["compresso"]["args"]
        .as_array()
        .expect("args array")
        .iter()
        .map(|value| value.as_str().expect("string argument").to_owned())
        .collect()
}

fn check(adapter: &dyn Integration, scope: &Scope, name: &str) -> bool {
    adapter
        .verify(scope)
        .checks
        .iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("verification check {name} should exist"))
        .passed
}

fn base_args() -> Vec<String> {
    vec!["mcp".to_owned(), "serve".to_owned()]
}

#[test]
fn test_each_client_entry_enables_only_its_own_transcript_source() {
    let harness = Harness::new();
    let claude = harness.claude();
    let codex = harness.codex();
    let cursor = harness.cursor();

    let (claude_plan, _) = harness.install(&claude, Scope::User, true, "recall-claude");
    let (codex_plan, _) = harness.install(&codex, Scope::User, true, "recall-codex");
    let (cursor_plan, _) = harness.install(&cursor, Scope::User, true, "recall-cursor");

    assert_eq!(
        json_args(&claude_plan.config_path),
        json!(["mcp", "serve", "--recall-source", "claude-code"])
    );
    assert_eq!(
        toml_args(&codex_plan.config_path),
        ["mcp", "serve", "--recall-source", "codex"]
    );
    assert_eq!(
        json_args(&cursor_plan.config_path),
        json!(["mcp", "serve"]),
        "Cursor transcripts are unsupported, so its entry never names a recall source"
    );

    // The preview shows the exact reviewed arguments instead of redacting them.
    assert!(claude_plan.unified_diff.contains("--recall-source"));
    assert!(claude_plan.unified_diff.contains("claude-code"));
    assert!(!claude_plan.unified_diff.contains("codex"));
    assert!(codex_plan.unified_diff.contains("--recall-source"));
    assert!(codex_plan.unified_diff.contains("\"codex\""));
    assert!(!codex_plan.unified_diff.contains("claude-code"));
    assert!(!cursor_plan.unified_diff.contains("--recall-source"));
    for plan in [&claude_plan, &codex_plan] {
        assert!(
            !plan.unified_diff.contains("<redacted>"),
            "{}",
            plan.unified_diff
        );
    }

    assert!(check(&claude, &Scope::User, "server_entry"));
    assert!(check(&codex, &Scope::User, "server_entry"));
    assert!(check(&cursor, &Scope::User, "server_entry"));
}

#[test]
fn test_project_scope_entry_appends_recall_after_the_pinned_root() {
    let harness = Harness::new();
    let claude = harness.claude();
    let scope = Scope::Project(harness.project.clone());
    let (plan, _) = harness.install(&claude, scope.clone(), true, "recall-project");
    assert_eq!(
        json_args(&plan.config_path),
        json!([
            "mcp",
            "serve",
            "--root",
            harness.project.to_str().expect("UTF-8 project"),
            "--recall-source",
            "claude-code"
        ])
    );
    assert!(check(&claude, &scope, "server_entry"));
    assert!(!plan.unified_diff.contains("<redacted>"));
}

#[test]
fn test_opt_out_writes_the_legacy_shape_and_still_verifies() {
    let harness = Harness::new();
    let claude = harness.claude();
    let codex = harness.codex();
    let (claude_plan, _) = harness.install(&claude, Scope::User, false, "no-recall-claude");
    let (codex_plan, _) = harness.install(&codex, Scope::User, false, "no-recall-codex");
    assert_eq!(json_args(&claude_plan.config_path), json!(["mcp", "serve"]));
    assert_eq!(toml_args(&codex_plan.config_path), ["mcp", "serve"]);
    assert!(check(&claude, &Scope::User, "server_entry"));
    assert!(check(&codex, &Scope::User, "server_entry"));
}

#[test]
fn test_verify_rejects_a_foreign_or_extra_recall_argument() {
    let harness = Harness::new();
    let claude = harness.claude();
    let codex = harness.codex();
    let cursor = harness.cursor();
    let (claude_plan, _) = harness.install(&claude, Scope::User, true, "verify-claude");
    let (codex_plan, _) = harness.install(&codex, Scope::User, true, "verify-codex");
    let (cursor_plan, _) = harness.install(&cursor, Scope::User, true, "verify-cursor");

    for args in [
        json!(["mcp", "serve", "--recall-source", "codex"]),
        json!([
            "mcp",
            "serve",
            "--recall-source",
            "claude-code",
            "--recall-source",
            "codex"
        ]),
        json!(["mcp", "serve", "--recall-source", "claude-code", "--extra"]),
    ] {
        let mut document: Value =
            serde_json::from_str(&fs::read_to_string(&claude_plan.config_path).expect("config"))
                .expect("JSON");
        document["mcpServers"]["compresso"]["args"] = args.clone();
        fs::write(
            &claude_plan.config_path,
            serde_json::to_string_pretty(&document).expect("render"),
        )
        .expect("config rewritten");
        assert!(!check(&claude, &Scope::User, "server_entry"), "{args}");
    }

    let codex_text = fs::read_to_string(&codex_plan.config_path).expect("Codex config");
    fs::write(
        &codex_plan.config_path,
        codex_text.replace("\"codex\"", "\"claude-code\""),
    )
    .expect("Codex config rewritten");
    assert!(!check(&codex, &Scope::User, "server_entry"));

    let mut document: Value =
        serde_json::from_str(&fs::read_to_string(&cursor_plan.config_path).expect("config"))
            .expect("JSON");
    document["mcpServers"]["compresso"]["args"] =
        json!(["mcp", "serve", "--recall-source", "claude-code"]);
    fs::write(
        &cursor_plan.config_path,
        serde_json::to_string_pretty(&document).expect("render"),
    )
    .expect("Cursor config rewritten");
    assert!(
        !check(&cursor, &Scope::User, "server_entry"),
        "a recall source added to a Cursor entry is a modification"
    );
}

#[test]
fn test_upgrade_from_previous_install_adds_recall_idempotently_and_uninstalls_cleanly() {
    let harness = Harness::new();
    let claude = harness.claude();
    let config = claude.config_path(&Scope::User).expect("config path");
    // A user-owned file with a byte-order mark, CRLF endings, and a sibling.
    let original = "\u{feff}{\r\n  \"theme\": \"dark\",\r\n  \"mcpServers\": {\r\n    \"other\": {\r\n      \"command\": \"/bin/other\"\r\n    }\r\n  }\r\n}\r\n";
    fs::write(&config, original).expect("pre-existing config");

    // The previous release wrote no recall argument.
    let (_, legacy_receipt) = harness.install(&claude, Scope::User, false, "upgrade-legacy");
    assert_eq!(json_args(&config), json!(["mcp", "serve"]));
    let legacy_bytes = fs::read(&config).expect("legacy config");

    let manager = PlanManager::new("upgrade-new")
        .expect("plan manager")
        .with_backup_dir(test_backup_dir());
    let upgrade = harness.plan(&claude, &manager, Scope::User, true);
    let changed = upgrade
        .unified_diff
        .lines()
        .filter(|line| {
            (line.starts_with('+') || line.starts_with('-'))
                && !line.starts_with("+++")
                && !line.starts_with("---")
        })
        .collect::<Vec<_>>();
    assert!(
        changed.iter().all(|line| !line.contains("command")
            && !line.contains("other")
            && !line.contains("theme")),
        "the upgrade preview should change only the arguments: {changed:?}"
    );
    assert!(upgrade.unified_diff.contains("--recall-source"));
    assert_eq!(
        fs::read(&config).expect("config"),
        legacy_bytes,
        "preview writes nothing"
    );

    let upgraded_receipt = claude
        .install(&manager, &PlanApproval::from(&upgrade), &harness.receipts)
        .expect("upgrade applies");
    assert_ne!(upgraded_receipt.plan_hash, legacy_receipt.plan_hash);
    assert_eq!(
        json_args(&config),
        json!(["mcp", "serve", "--recall-source", "claude-code"])
    );
    let upgraded_bytes = fs::read(&config).expect("upgraded config");
    let upgraded_text = String::from_utf8(upgraded_bytes.clone()).expect("UTF-8");
    assert!(upgraded_text.starts_with('\u{feff}'), "BOM is kept");
    assert_eq!(
        upgraded_text.matches('\n').count(),
        upgraded_text.matches("\r\n").count(),
        "every line keeps the file's CRLF ending"
    );

    // The upgraded entry is the receipt-owned one: verify and trust both hold.
    assert!(check(&claude, &Scope::User, "server_entry"));
    let active = harness
        .receipts
        .active_for("claude-code", &config)
        .expect("receipt lookup")
        .expect("active receipt");
    assert_eq!(active.plan_hash, upgraded_receipt.plan_hash);
    assert!(receipt_authenticates_server(
        &active,
        ConfigFormat::Json,
        &config
    ));
    let ConfiguredServer::Exact { args, .. } =
        read_configured_server(ConfigFormat::Json, &config, &base_args())
    else {
        panic!("upgraded entry should have the adapter-written shape");
    };
    assert_eq!(server_recall_source(&args), Some("claude-code"));

    // Re-running the same install is a no-op plan that keeps the receipt.
    let repeat_manager = PlanManager::new("upgrade-repeat")
        .expect("plan manager")
        .with_backup_dir(test_backup_dir());
    let repeat = harness.plan(&claude, &repeat_manager, Scope::User, true);
    assert_eq!(repeat.base_hash, repeat.result_hash);
    let repeated_receipt = claude
        .install(
            &repeat_manager,
            &PlanApproval::from(&repeat),
            &harness.receipts,
        )
        .expect("repeat applies");
    assert_eq!(repeated_receipt.plan_hash, upgraded_receipt.plan_hash);
    assert_eq!(fs::read(&config).expect("config"), upgraded_bytes);

    // Uninstall removes the entry instead of restoring the legacy one.
    harness.uninstall(&claude, &config, "upgrade-uninstall");
    let restored = fs::read_to_string(&config).expect("restored config");
    assert_eq!(
        restored, original,
        "uninstall returns the pre-install bytes"
    );
}

#[test]
fn test_codex_upgrade_adds_recall_and_uninstall_restores_the_original_bytes() {
    let harness = Harness::new();
    let codex = harness.codex();
    let config = codex.config_path();
    fs::create_dir_all(config.parent().expect("config parent")).expect("config directory");
    let original =
        "# user settings\r\nmodel = \"example\"\r\n\r\n[features]\r\nexperimental = true\r\n";
    fs::write(&config, original).expect("pre-existing config");

    harness.install(&codex, Scope::User, false, "codex-legacy");
    assert_eq!(toml_args(&config), ["mcp", "serve"]);

    let (upgrade, upgraded_receipt) = harness.install(&codex, Scope::User, true, "codex-upgrade");
    assert!(upgrade.unified_diff.contains("--recall-source"));
    assert!(!upgrade.unified_diff.contains("model"));
    assert_eq!(
        toml_args(&config),
        ["mcp", "serve", "--recall-source", "codex"]
    );
    assert!(check(&codex, &Scope::User, "server_entry"));
    let active = harness
        .receipts
        .active_for("codex", &config)
        .expect("receipt lookup")
        .expect("active receipt");
    assert!(receipt_authenticates_server(
        &active,
        ConfigFormat::Toml,
        &config
    ));

    let upgraded_bytes = fs::read(&config).expect("upgraded config");
    let (repeat, repeated_receipt) = harness.install(&codex, Scope::User, true, "codex-repeat");
    assert_eq!(repeat.base_hash, repeat.result_hash);
    assert_eq!(repeated_receipt.plan_hash, upgraded_receipt.plan_hash);
    assert_eq!(fs::read(&config).expect("config"), upgraded_bytes);

    harness.uninstall(&codex, &config, "codex-uninstall");
    assert_eq!(
        fs::read_to_string(&config).expect("restored config"),
        original
    );
}

#[test]
fn test_opting_out_after_a_default_install_removes_the_recall_argument() {
    let harness = Harness::new();
    let claude = harness.claude();
    let (plan, _) = harness.install(&claude, Scope::User, true, "opt-out-default");
    let (opt_out, _) = harness.install(&claude, Scope::User, false, "opt-out-disable");
    assert!(opt_out.unified_diff.contains("--recall-source"));
    assert_eq!(json_args(&plan.config_path), json!(["mcp", "serve"]));
    assert!(check(&claude, &Scope::User, "server_entry"));
}

/// A backup directory private to the calling test thread, removed when the
/// thread ends (E17-53: backups are kept in the data directory).
fn test_backup_dir() -> std::path::PathBuf {
    thread_local! {
        static DIR: tempfile::TempDir = tempfile::tempdir().expect("test backup directory");
    }
    DIR.with(|dir| dir.path().join(compresso_integrations::BACKUP_DIR_NAME))
}
