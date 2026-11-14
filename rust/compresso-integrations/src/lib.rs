//! Reversible coding-client configuration adapters and installation receipts.
//!
//! Configuration mutation is deliberately split into preview and approval. A
//! [`PlanManager`] retains the exact plan it issued, so apply never trusts a
//! caller-supplied replacement document. Plans expire after ten minutes and
//! are bound to the session that created them.
//!
//! # Line endings and byte-order marks
//!
//! A mutation never changes how a user-owned file is encoded. An existing
//! UTF-8 byte-order mark is kept and none is ever added. Every line Compresso
//! does not change keeps its exact terminator (`\r\n`, `\n`, or none for a
//! final line without a trailing newline). Lines Compresso writes use the
//! file's dominant ending: `\r\n` when the file has strictly more `\r\n`
//! than bare `\n` terminators, otherwise `\n`; so in a mixed file the majority
//! wins, a tie is `\n`, and existing lines are never normalized. A file
//! Compresso creates uses `\n` and no byte-order mark on every platform.
//!
//! Uninstalling a config that is untouched since its install returns the
//! pre-install bytes exactly, including indentation and mixed line endings.
//! After any later user edit the uninstall removes only the owned entry and
//! applies the rules above instead, so that edit is never reverted.

mod claude_code;
mod codex;
mod compaction_hook;
mod cursor;
mod plan;
mod receipt;
mod server_entry;
mod text_fidelity;

use std::path::PathBuf;

pub use claude_code::{ClaudeCodeDiscovery, ClaudeCodeIntegration};
pub use codex::{
    CodexAdapter, CodexIntegration, CodexPlatform, CodexVerificationEvidence,
    CodexVerificationMethod, CodexVersionEvidence,
};
pub use compaction_hook::{CompactionHookClient, CompactionHookIntegration};
pub use cursor::{
    CursorApplicationEvidence, CursorCliEvidence, CursorDiscovery, CursorIntegration,
    CursorLiveProbeEvidence, CursorPlatform,
};
pub use plan::{
    ApplyReport, ConfigFormat, DeferredInstallApplyReport, DeferredInstallAuthorization, Detection,
    InstallCtx, IntegrationError, MutationKind, MutationPlan, MutationPlanPreview,
    OwnedKeyMutation, PLAN_SCHEMA_VERSION, PLAN_TTL_SECONDS, PlanApproval, PlanClock, PlanManager,
    RestoreReport, Scope, SystemPlanClock, VerificationCheck, VerificationReport,
};
pub use receipt::{
    CREATED_FILE_BACKUP_SENTINEL, FileReceiptStore, InstallReceipt, MemoryReceiptStore,
    RECEIPT_SCHEMA_VERSION, ReceiptError, ReceiptStore,
};
pub use server_entry::{
    ConfiguredServer, read_configured_server, recall_source_for_client,
    receipt_authenticates_server, server_recall_source,
};

/// Adapter contract shared by Claude Code, Codex, and Cursor.
///
/// Implementations render only their documented owned keys. The manager then
/// binds those exact output bytes to a short-lived plan and performs the
/// recoverable filesystem commit after approval.
pub trait Integration {
    /// Stable client identifier (`claude-code`, `codex`, or `cursor`).
    fn client_id(&self) -> &'static str;

    /// Detects whether the coding client and its selected configuration exist.
    fn detect(&self, scope: &Scope) -> Detection;

    /// Produces an install preview through the supplied plan manager.
    fn plan_install(
        &self,
        manager: &PlanManager,
        context: &InstallCtx,
    ) -> Result<MutationPlan, IntegrationError>;

    /// Applies an install plan previously issued by `manager`.
    fn install(
        &self,
        manager: &PlanManager,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
    ) -> Result<InstallReceipt, IntegrationError> {
        manager.apply_install(approval, receipts)
    }

    /// Checks parseability and binary resolution without changing config.
    fn verify(&self, scope: &Scope) -> VerificationReport;

    /// Produces an uninstall preview for the receipt-owned keys.
    fn plan_uninstall(
        &self,
        manager: &PlanManager,
        receipt: &InstallReceipt,
    ) -> Result<MutationPlan, IntegrationError>;

    /// Applies a previously previewed uninstall plan.
    fn uninstall(
        &self,
        manager: &PlanManager,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
    ) -> Result<RestoreReport, IntegrationError> {
        manager.apply_uninstall(approval, receipts)
    }
}

/// Helper used by adapters to select a concrete configuration path.
pub fn scoped_path(scope: &Scope, user_path: PathBuf, project_name: &str) -> PathBuf {
    match scope {
        Scope::User => user_path,
        Scope::Project(root) => root.join(project_name),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::plan::MutationRequest;

    struct FakeIntegration;

    impl Integration for FakeIntegration {
        fn client_id(&self) -> &'static str {
            "claude-code"
        }

        fn detect(&self, _scope: &Scope) -> Detection {
            Detection::NotFound
        }

        fn plan_install(
            &self,
            _manager: &PlanManager,
            _context: &InstallCtx,
        ) -> Result<MutationPlan, IntegrationError> {
            unreachable!("not used by this default-method regression")
        }

        fn verify(&self, _scope: &Scope) -> VerificationReport {
            VerificationReport {
                client: self.client_id().to_owned(),
                verified: false,
                checks: Vec::new(),
            }
        }

        fn plan_uninstall(
            &self,
            _manager: &PlanManager,
            _receipt: &InstallReceipt,
        ) -> Result<MutationPlan, IntegrationError> {
            unreachable!("not used by this default-method regression")
        }
    }

    #[test]
    fn test_trait_defaults_reject_wrong_plan_kind_before_mutating() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let manager = PlanManager::new("trait-regression").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let installed_config =
            "{\"mcpServers\":{\"compresso\":{\"command\":\"/bin/compresso\"}}}\n";
        let install_plan = manager
            .issue(MutationRequest {
                operation: MutationKind::Install,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: installed_config.to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: None,
                warnings: Vec::new(),
            })
            .expect("install plan");

        let error = FakeIntegration
            .uninstall(&manager, &PlanApproval::from(&install_plan), &receipts)
            .expect_err("install approval cannot run through uninstall");
        assert!(matches!(
            error,
            IntegrationError::WrongPlanKind {
                expected: MutationKind::Uninstall,
                actual: MutationKind::Install,
            }
        ));
        assert_eq!(fs::read_to_string(&path).expect("config readable"), "{}\n");

        let receipt = FakeIntegration
            .install(&manager, &PlanApproval::from(&install_plan), &receipts)
            .expect("correct install applies");
        let uninstall_plan = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: "{\"mcpServers\":{}}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(receipt),
                warnings: Vec::new(),
            })
            .expect("uninstall plan");
        let before_wrong_install = fs::read(&path).expect("installed config readable");

        let error = FakeIntegration
            .install(&manager, &PlanApproval::from(&uninstall_plan), &receipts)
            .expect_err("uninstall approval cannot run through install");
        assert!(matches!(
            error,
            IntegrationError::WrongPlanKind {
                expected: MutationKind::Install,
                actual: MutationKind::Uninstall,
            }
        ));
        assert_eq!(
            fs::read(&path).expect("config readable"),
            before_wrong_install
        );
    }
}
