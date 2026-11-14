use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use compresso_integrations::{
    ClaudeCodeDiscovery, ClaudeCodeIntegration, CodexAdapter, CodexPlatform, CodexVersionEvidence,
    CursorCliEvidence, CursorDiscovery, CursorIntegration, CursorPlatform, Detection,
    FileReceiptStore, InstallCtx, InstallReceipt, Integration, IntegrationError, MutationKind,
    MutationPlan, PlanApproval, PlanManager, ReceiptError, ReceiptStore, RestoreReport, Scope,
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use toml_edit::{DocumentMut, value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Client {
    ClaudeCode,
    Codex,
    Cursor,
}

impl Client {
    const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Cursor];

    fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
        }
    }

    fn owned_path(self) -> &'static str {
        match self {
            Self::ClaudeCode | Self::Cursor => "mcpServers.compresso",
            Self::Codex => "mcp_servers.compresso",
        }
    }

    fn fixture_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code.json",
            Self::Codex => "codex.toml",
            Self::Cursor => "cursor.json",
        }
    }

    fn fixture_secret(self) -> &'static str {
        match self {
            Self::ClaudeCode => "integration-fixture-secret-claude-code",
            Self::Codex => "integration-fixture-secret-codex",
            Self::Cursor => "integration-fixture-secret-cursor",
        }
    }

    fn is_json(self) -> bool {
        !matches!(self, Self::Codex)
    }
}

struct Harness {
    _directory: TempDir,
    client: Client,
    adapter: Box<dyn Integration>,
    config_path: PathBuf,
    client_binary: PathBuf,
    compresso_binary: PathBuf,
    receipt_path: PathBuf,
    baseline: Vec<u8>,
}

impl Harness {
    fn new(client: Client) -> Self {
        let directory = tempdir().expect("temporary integration root");
        let home = directory.path().join("home");
        let bin = directory.path().join("bin");
        fs::create_dir_all(&home).expect("temporary home created");
        fs::create_dir_all(&bin).expect("temporary bin created");

        let compresso_binary = bin.join(executable_name("compresso"));
        write_executable(&compresso_binary);

        let (adapter, config_path, client_binary): (Box<dyn Integration>, _, _) = match client {
            Client::ClaudeCode => {
                let client_binary = bin.join(executable_name("claude"));
                write_executable(&client_binary);
                let adapter = ClaudeCodeIntegration::new(
                    home.clone(),
                    ClaudeCodeDiscovery::new([bin.clone()], Some("Claude Code 1.2.3".to_owned())),
                )
                .with_compresso_version("0.1.0");
                let canonical_home = fs::canonicalize(&home).expect("canonical temporary home");
                (
                    Box::new(adapter),
                    canonical_home.join(".claude.json"),
                    client_binary,
                )
            }
            Client::Codex => {
                let platform = codex_platform();
                let client_binary = bin.join(executable_name("codex"));
                write_executable(&client_binary);
                let evidence = CodexVersionEvidence::from_output(&client_binary, "codex 1.2.3")
                    .expect("Codex version evidence");
                let adapter = CodexAdapter::with_platform(
                    &home,
                    vec![bin.clone()],
                    Some(evidence),
                    "0.1.0",
                    platform,
                )
                .expect("Codex adapter");
                let config_path = adapter.config_path();
                (Box::new(adapter), config_path, client_binary)
            }
            Client::Cursor => {
                let platform = cursor_platform();
                let client_binary = bin.join(executable_name("cursor"));
                write_executable(&client_binary);
                let adapter = CursorIntegration::new(
                    &home,
                    CursorDiscovery::new(
                        platform,
                        [CursorCliEvidence::new(&client_binary, "Cursor 1.2.3")],
                        [],
                    ),
                )
                .with_compresso_version("0.1.0");
                let config_path = adapter
                    .config_path(&Scope::User)
                    .expect("safe Cursor config path");
                (Box::new(adapter), config_path, client_binary)
            }
        };

        let baseline = fs::read(fixture_path(client.fixture_name())).expect("fixture readable");
        fs::create_dir_all(config_path.parent().expect("config parent"))
            .expect("config parent created");
        fs::write(&config_path, &baseline).expect("baseline config written");
        let receipt_path = directory.path().join("state/receipts.json");

        Self {
            _directory: directory,
            client,
            adapter,
            config_path,
            client_binary,
            compresso_binary,
            receipt_path,
            baseline,
        }
    }

    fn context(&self) -> InstallCtx {
        InstallCtx {
            compresso_bin: self.compresso_binary.clone(),
            scope: Scope::User,
            session_recall: false,
        }
    }

    fn plan_install(&self, manager: &PlanManager) -> MutationPlan {
        self.adapter
            .plan_install(manager, &self.context())
            .unwrap_or_else(|error| panic!("{} install plan failed: {error}", self.client.id()))
    }

    fn install(
        &self,
        manager: &PlanManager,
        receipts: &dyn ReceiptStore,
    ) -> (MutationPlan, InstallReceipt) {
        let plan = self.plan_install(manager);
        let receipt = self
            .adapter
            .install(manager, &PlanApproval::from(&plan), receipts)
            .unwrap_or_else(|error| panic!("{} install failed: {error}", self.client.id()));
        (plan, receipt)
    }

    fn file_receipts(&self) -> FileReceiptStore {
        FileReceiptStore::new(&self.receipt_path)
    }

    fn journal_path(&self) -> PathBuf {
        let name = self
            .config_path
            .file_name()
            .expect("config filename")
            .to_string_lossy();
        self.config_path
            .with_file_name(format!("{name}.compresso-journal"))
    }

    fn write_malformed_fixture(&self) {
        let name = if self.client.is_json() {
            "malformed.json"
        } else {
            "malformed.toml"
        };
        fs::write(
            &self.config_path,
            fs::read(fixture_path(name)).expect("malformed fixture readable"),
        )
        .expect("malformed fixture written");
    }

    fn drift_target(&self) {
        if self.client.is_json() {
            let mut document: Value =
                serde_json::from_slice(&fs::read(&self.config_path).expect("config readable"))
                    .expect("fixture JSON");
            document
                .as_object_mut()
                .expect("fixture object")
                .insert("concurrentEdit".to_owned(), json!(true));
            write_json(&self.config_path, &document);
        } else {
            let mut document = fs::read_to_string(&self.config_path)
                .expect("config readable")
                .parse::<DocumentMut>()
                .expect("fixture TOML");
            document["concurrent_edit"] = value(true);
            fs::write(&self.config_path, document.to_string()).expect("drifted TOML written");
        }
    }

    fn modify_owned_entry(&self) {
        let user_secret = format!("user-modified-{}", self.client.fixture_secret());
        if self.client.is_json() {
            let mut document: Value =
                serde_json::from_slice(&fs::read(&self.config_path).expect("config readable"))
                    .expect("installed JSON");
            let entry = document["mcpServers"]["compresso"]
                .as_object_mut()
                .expect("owned JSON entry");
            entry.insert("disabled".to_owned(), json!(true));
            entry.insert("userNote".to_owned(), json!(user_secret));
            write_json(&self.config_path, &document);
        } else {
            let mut document = fs::read_to_string(&self.config_path)
                .expect("config readable")
                .parse::<DocumentMut>()
                .expect("installed TOML");
            document["mcp_servers"]["compresso"]["enabled"] = value(false);
            document["mcp_servers"]["compresso"]["user_note"] = value(user_secret);
            fs::write(&self.config_path, document.to_string()).expect("modified TOML written");
        }
    }

    fn assert_unowned_state_preserved(&self) {
        let installed = fs::read(&self.config_path).expect("installed config readable");
        if self.client.is_json() {
            let baseline: Value = serde_json::from_slice(&self.baseline).expect("baseline JSON");
            let mut installed: Value = serde_json::from_slice(&installed).expect("installed JSON");
            installed["mcpServers"]
                .as_object_mut()
                .expect("MCP servers object")
                .remove("compresso");
            assert_eq!(installed, baseline, "{} unowned JSON", self.client.id());
        } else {
            let mut installed = std::str::from_utf8(&installed)
                .expect("installed UTF-8")
                .parse::<DocumentMut>()
                .expect("installed TOML");
            installed["mcp_servers"]
                .as_table_like_mut()
                .expect("MCP servers table")
                .remove("compresso");
            assert_eq!(
                installed.to_string().as_bytes(),
                self.baseline,
                "{} unowned TOML",
                self.client.id()
            );
        }
    }

    fn assert_restored(&self) {
        let restored = fs::read(&self.config_path).expect("restored config readable");
        if self.client.is_json() {
            assert_eq!(
                serde_json::from_slice::<Value>(&restored).expect("restored JSON"),
                serde_json::from_slice::<Value>(&self.baseline).expect("baseline JSON"),
                "{} restoration",
                self.client.id()
            );
        } else {
            assert_eq!(restored, self.baseline, "{} restoration", self.client.id());
        }
    }

    fn assert_no_transient_files(&self) {
        let parent = self.config_path.parent().expect("config parent");
        let transient = fs::read_dir(parent)
            .expect("config directory readable")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                name.contains("compresso-journal")
                    || name.contains("compresso-tmp")
                    || name.contains(".reserve-")
            })
            .collect::<Vec<_>>();
        assert!(
            transient.is_empty(),
            "{} left transient files: {transient:?}",
            self.client.id()
        );
    }
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

#[test]
fn test_detection_contract_reports_installed_and_not_found() {
    for client in Client::ALL {
        let harness = Harness::new(client);
        assert_eq!(
            harness.adapter.detect(&Scope::User),
            Detection::Installed {
                version: "1.2.3".to_owned(),
                config_path: harness.config_path.clone(),
            },
            "{} installed detection",
            client.id()
        );

        fs::remove_file(&harness.client_binary).expect("client binary removed");
        assert_eq!(
            harness.adapter.detect(&Scope::User),
            Detection::NotFound,
            "{} missing detection",
            client.id()
        );
        assert_eq!(
            fs::read(&harness.config_path).expect("config remains readable"),
            harness.baseline,
            "{} detection is read-only",
            client.id()
        );
    }
}

#[test]
fn test_base_plan_atomic_apply_receipt_and_unknown_key_contract() {
    for client in Client::ALL {
        let harness = Harness::new(client);
        let manager = PlanManager::new(format!("parity-base-{}", client.id())).expect("manager");
        let receipts = harness.file_receipts();
        let plan = harness.plan_install(&manager);

        assert_eq!(
            plan.operation,
            MutationKind::Install,
            "{} kind",
            client.id()
        );
        assert_eq!(plan.client, client.id(), "{} client", client.id());
        assert_eq!(
            plan.config_path,
            harness.config_path,
            "{} path",
            client.id()
        );
        assert!(plan.base_exists, "{} existing fixture", client.id());
        assert_eq!(plan.owned_keys.len(), 1, "{} owned count", client.id());
        assert_eq!(
            plan.owned_keys[0].key_path,
            client.owned_path(),
            "{} owned path",
            client.id()
        );
        assert_eq!(plan.base_hash, present_hash(&harness.baseline));
        assert_ne!(plan.result_hash, plan.base_hash);
        assert_eq!(
            plan.diff_hash,
            blake3::hash(plan.unified_diff.as_bytes())
                .to_hex()
                .to_string()
        );
        let public_plan = serde_json::to_value(&plan).expect("public plan should serialize");
        assert_eq!(public_plan["document_type"], "mutation_plan_preview");
        assert!(public_plan.get("target_contents").is_none());
        assert_eq!(
            fs::read(&harness.config_path).expect("preview target readable"),
            harness.baseline,
            "{} preview is read-only",
            client.id()
        );
        assert!(!Path::new(&plan.backup_path).exists());

        let receipt = harness
            .adapter
            .install(&manager, &PlanApproval::from(&plan), &receipts)
            .unwrap_or_else(|error| panic!("{} apply failed: {error}", client.id()));
        assert_eq!(receipt.client, client.id());
        assert_eq!(receipt.config_path, harness.config_path);
        assert_eq!(receipt.keys_owned, [client.owned_path()]);
        assert_eq!(receipt.plan_hash, plan.plan_hash);
        assert_eq!(
            plan.result_hash,
            present_hash(
                &fs::read(&harness.config_path).expect("installed config should be readable")
            ),
            "{} result hash binds the private target",
            client.id()
        );
        assert_eq!(
            fs::read(&receipt.backup_path).expect("byte-exact backup readable"),
            harness.baseline,
            "{} backup bytes",
            client.id()
        );
        assert_eq!(
            receipts
                .by_plan_hash(&receipt.plan_hash)
                .expect("receipt query"),
            Some(receipt.clone()),
            "{} canonical receipt",
            client.id()
        );
        assert_eq!(
            receipts
                .active_for(client.id(), &harness.config_path)
                .expect("active receipt query"),
            Some(receipt),
            "{} active receipt",
            client.id()
        );
        harness.assert_unowned_state_preserved();
        harness.assert_no_transient_files();
    }
}

#[test]
fn test_full_file_drift_rejects_without_receipt_or_rewrite() {
    for client in Client::ALL {
        let harness = Harness::new(client);
        let manager = PlanManager::new(format!("parity-drift-{}", client.id())).expect("manager");
        let receipts = harness.file_receipts();
        let plan = harness.plan_install(&manager);
        harness.drift_target();
        let drifted = fs::read(&harness.config_path).expect("drifted config readable");

        let error = harness
            .adapter
            .install(&manager, &PlanApproval::from(&plan), &receipts)
            .expect_err("drift must reject");
        assert!(
            matches!(error, IntegrationError::ConfigDrift { .. }),
            "{} drift error was {error}",
            client.id()
        );
        assert_eq!(
            fs::read(&harness.config_path).expect("drifted config remains"),
            drifted,
            "{} drift preservation",
            client.id()
        );
        assert_eq!(
            receipts
                .active_for(client.id(), &harness.config_path)
                .expect("active receipt query"),
            None,
            "{} no receipt on drift",
            client.id()
        );
        harness.assert_no_transient_files();
    }
}

#[test]
fn test_atomic_receipt_failure_recovers_each_adapter_commit() {
    for client in Client::ALL {
        let harness = Harness::new(client);
        let manager =
            PlanManager::new(format!("parity-recovery-{}", client.id())).expect("manager");
        let receipts = FailOnceReceiptStore::new(&harness.receipt_path);
        let plan = harness.plan_install(&manager);

        let error = harness
            .adapter
            .install(&manager, &PlanApproval::from(&plan), &receipts)
            .expect_err("first receipt persistence is injected to fail");
        assert!(
            matches!(error, IntegrationError::Receipt(ReceiptError::LockPoisoned)),
            "{} receipt failure was {error}",
            client.id()
        );
        assert_eq!(
            present_hash(
                &fs::read(&harness.config_path).expect("committed config should be readable")
            ),
            plan.result_hash,
            "{} private target committed before receipt failure",
            client.id()
        );
        assert!(
            harness.journal_path().exists(),
            "{} recovery journal",
            client.id()
        );
        assert_eq!(
            fs::read(&plan.backup_path).expect("recovery backup readable"),
            harness.baseline,
            "{} recovery backup",
            client.id()
        );

        let recovered = manager
            .recover(&harness.config_path, &receipts)
            .unwrap_or_else(|error| panic!("{} recovery failed: {error}", client.id()))
            .expect("pending commit recovered");
        let receipt = recovered.receipt.expect("recovered install receipt");
        assert_eq!(receipt.plan_hash, plan.plan_hash);
        assert_eq!(
            receipts
                .by_plan_hash(&receipt.plan_hash)
                .expect("recovered receipt query"),
            Some(receipt.clone())
        );
        assert_eq!(
            receipts
                .active_for(client.id(), &harness.config_path)
                .expect("recovered active receipt"),
            Some(receipt)
        );
        harness.assert_unowned_state_preserved();
        harness.assert_no_transient_files();
    }
}

#[test]
fn test_malformed_configs_fail_closed_without_side_effects() {
    for client in Client::ALL {
        let harness = Harness::new(client);
        harness.write_malformed_fixture();
        let malformed = fs::read(&harness.config_path).expect("malformed config readable");
        let manager =
            PlanManager::new(format!("parity-malformed-{}", client.id())).expect("manager");

        let error = harness
            .adapter
            .plan_install(&manager, &harness.context())
            .expect_err("malformed config must reject");
        assert!(
            matches!(error, IntegrationError::InvalidRequest { .. }),
            "{} malformed error was {error}",
            client.id()
        );
        assert_eq!(
            fs::read(&harness.config_path).expect("malformed config remains"),
            malformed,
            "{} malformed preservation",
            client.id()
        );
        harness.assert_no_transient_files();
    }
}

#[test]
fn test_clean_uninstall_restores_unknown_keys_and_repeats_idempotently() {
    for client in Client::ALL {
        let harness = Harness::new(client);
        let receipts = harness.file_receipts();
        let install_manager =
            PlanManager::new(format!("parity-roundtrip-install-{}", client.id())).expect("manager");
        let (_plan, receipt) = harness.install(&install_manager, &receipts);
        harness.assert_unowned_state_preserved();

        let repeat_plan = harness.plan_install(&install_manager);
        let repeated_receipt = harness
            .adapter
            .install(
                &install_manager,
                &PlanApproval::from(&repeat_plan),
                &receipts,
            )
            .expect("repeat install");
        assert_eq!(repeated_receipt, receipt, "{} repeat install", client.id());

        let uninstall_manager =
            PlanManager::new(format!("parity-roundtrip-uninstall-{}", client.id()))
                .expect("manager");
        let uninstall_plan = harness
            .adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .unwrap_or_else(|error| panic!("{} uninstall plan failed: {error}", client.id()));
        let approval = PlanApproval::from(&uninstall_plan);
        let first = harness
            .adapter
            .uninstall(&uninstall_manager, &approval, &receipts)
            .unwrap_or_else(|error| panic!("{} uninstall failed: {error}", client.id()));
        let repeated = harness
            .adapter
            .uninstall(&uninstall_manager, &approval, &receipts)
            .unwrap_or_else(|error| panic!("{} repeat uninstall failed: {error}", client.id()));
        assert_eq!(first.removed_keys, [client.owned_path()]);
        assert!(first.retained_keys.is_empty());
        assert!(first.warnings.is_empty());
        assert!(!first.already_applied);
        assert!(repeated.already_applied);
        assert_eq!(repeated.removed_keys, first.removed_keys);
        harness.assert_restored();

        let canonical = receipts
            .by_plan_hash(&receipt.plan_hash)
            .expect("canonical receipt query")
            .expect("canonical receipt");
        assert!(canonical.uninstalled_at.is_some());
        assert_eq!(
            receipts
                .active_for(client.id(), &harness.config_path)
                .expect("active receipt query"),
            None
        );
        harness.assert_no_transient_files();
    }
}

#[test]
fn test_user_disabled_entries_are_retained_with_content_free_reports() {
    for client in Client::ALL {
        let harness = Harness::new(client);
        let receipts = harness.file_receipts();
        let install_manager =
            PlanManager::new(format!("parity-disable-install-{}", client.id())).expect("manager");
        let (_plan, receipt) = harness.install(&install_manager, &receipts);
        harness.modify_owned_entry();
        let modified = fs::read(&harness.config_path).expect("modified config readable");

        let uninstall_manager =
            PlanManager::new(format!("parity-disable-uninstall-{}", client.id())).expect("manager");
        let uninstall_plan = harness
            .adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .unwrap_or_else(|error| panic!("{} uninstall plan failed: {error}", client.id()));
        let expected_warning = format!("key_modified:{}", client.owned_path());
        assert_eq!(
            uninstall_plan.warnings.as_slice(),
            std::slice::from_ref(&expected_warning)
        );

        let report = harness
            .adapter
            .uninstall(
                &uninstall_manager,
                &PlanApproval::from(&uninstall_plan),
                &receipts,
            )
            .unwrap_or_else(|error| panic!("{} retained uninstall failed: {error}", client.id()));
        assert!(report.removed_keys.is_empty());
        assert_eq!(report.retained_keys, [client.owned_path()]);
        assert_eq!(report.warnings, [expected_warning]);
        assert!(report.already_applied, "{} retained no-op", client.id());
        assert_eq!(
            fs::read(&harness.config_path).expect("modified config retained"),
            modified,
            "{} user modification",
            client.id()
        );
        assert_eq!(
            receipts
                .active_for(client.id(), &harness.config_path)
                .expect("active receipt query"),
            Some(receipt.clone()),
            "{} retained entry keeps uninstall authority",
            client.id()
        );
        assert_content_free(&receipt, &report, &harness);
        harness.assert_no_transient_files();
    }
}

fn assert_content_free(receipt: &InstallReceipt, report: &RestoreReport, harness: &Harness) {
    let secret = harness.client.fixture_secret();
    let receipt_json = serde_json::to_string(receipt).expect("receipt JSON");
    let report_json = serde_json::to_string(report).expect("report JSON");
    let detection_json =
        serde_json::to_string(&harness.adapter.detect(&Scope::User)).expect("detection JSON");
    let verification_json =
        serde_json::to_string(&harness.adapter.verify(&Scope::User)).expect("verification JSON");
    for (name, content) in [
        ("receipt", receipt_json.as_str()),
        ("restore report", report_json.as_str()),
        ("detection", detection_json.as_str()),
        ("verification", verification_json.as_str()),
    ] {
        assert!(
            !content.contains(secret),
            "{} leaked fixture content in {name}",
            harness.client.id()
        );
    }

    let records_dir = harness.receipt_path.with_file_name("receipts.json.d");
    for entry in fs::read_dir(records_dir).expect("canonical receipt records") {
        let entry = entry.expect("receipt record entry");
        if entry.file_type().expect("receipt record type").is_file() {
            let bytes = fs::read(entry.path()).expect("receipt record readable");
            assert!(
                !String::from_utf8_lossy(&bytes).contains(secret),
                "{} leaked fixture content into canonical receipt state",
                harness.client.id()
            );
        }
    }
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures/integrations")
        .join(name)
}

fn present_hash(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"present\0");
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

fn write_json(path: &Path, document: &Value) {
    let mut rendered = serde_json::to_string_pretty(document).expect("JSON rendered");
    rendered.push('\n');
    fs::write(path, rendered).expect("JSON written");
}

fn write_executable(path: &Path) {
    fs::create_dir_all(path.parent().expect("executable parent"))
        .expect("executable parent created");
    fs::write(path, b"synthetic integration executable").expect("executable written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
    }
}

fn executable_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

fn codex_platform() -> CodexPlatform {
    if cfg!(target_os = "macos") {
        CodexPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        CodexPlatform::Windows
    } else {
        CodexPlatform::Linux
    }
}

fn cursor_platform() -> CursorPlatform {
    if cfg!(target_os = "macos") {
        CursorPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        CursorPlatform::Windows
    } else {
        CursorPlatform::Linux
    }
}
