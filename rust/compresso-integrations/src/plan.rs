use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml_edit::{Decor, DocumentMut, Item};

use crate::receipt::{
    CREATED_FILE_BACKUP_SENTINEL, InstallReceipt, RECEIPT_SCHEMA_VERSION, ReceiptError,
    ReceiptStore,
};
use crate::text_fidelity::{
    preserve_text_fidelity, pristine_json_backup, strip_bom, strip_bom_str,
};

/// Mutation plan schema version.
pub const PLAN_SCHEMA_VERSION: u32 = 1;
/// Integration plans are valid for exactly ten minutes.
pub const PLAN_TTL_SECONDS: i64 = 10 * 60;

/// User-wide or repository-local integration scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    /// Client's user configuration.
    User,
    /// Configuration rooted in one project directory.
    Project(PathBuf),
}

/// Inputs shared by integration install adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallCtx {
    /// Absolute installed Compresso binary path.
    pub compresso_bin: PathBuf,
    /// Selected config scope.
    pub scope: Scope,
    /// Whether the server entry also enables session recall over this
    /// client's own transcripts (`--recall-source <client>`). Adapters whose
    /// client has no supported transcript source ignore it.
    pub session_recall: bool,
}

/// Coding-client detection result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Detection {
    /// Client was found and its selected config path is known.
    Installed {
        version: String,
        config_path: PathBuf,
    },
    /// Client executable/application was not found.
    NotFound,
    /// Current platform or client version is unsupported without mutation.
    Unsupported { reason: String },
}

/// One non-mutating verification result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationCheck {
    /// Stable check identifier.
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Content-free diagnostic.
    pub detail: String,
}

/// Adapter verification report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReport {
    /// Client identifier.
    pub client: String,
    /// True only when every required check passed.
    pub verified: bool,
    /// Individual checks in stable order.
    pub checks: Vec<VerificationCheck>,
}

/// Install or uninstall mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    /// Add or replace Compresso-owned client state.
    Install,
    /// Remove only still-matching receipt-owned state.
    Uninstall,
}

/// Parser/projection used to prove that only adapter-owned paths changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFormat {
    /// JSON configuration with order-insensitive structural preservation.
    Json,
    /// TOML configuration with format-preserved unowned text.
    Toml,
    /// A whole Markdown file Compresso owns outright (E17-28). Its single
    /// owned path names the file; an uninstall that removes it deletes the
    /// file, represented by an empty target.
    Markdown,
}

/// Backup sentinel for an uninstall that deletes a whole owned file whose
/// bytes still equal what Compresso installed: nothing user-authored can be
/// lost, so no backup is written.
pub const UNMODIFIED_FILE_REMOVED_BACKUP_SENTINEL: &str =
    "(none: unmodified Compresso file removed)";

/// The file state a plan produces: a whole-file Markdown target that is
/// empty means the file is deleted. So does an empty JSON target, which
/// [`validate_request`] allows only for an uninstall that leaves a file
/// Compresso created holding nothing else (E17-51).
fn planned_result(format: ConfigFormat, target_contents: &str) -> TargetState {
    if matches!(format, ConfigFormat::Markdown | ConfigFormat::Json) && target_contents.is_empty() {
        TargetState::Missing
    } else {
        TargetState::Present(target_contents.as_bytes().to_vec())
    }
}

/// Exact before/after hashes for one adapter-owned key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedKeyMutation {
    /// Dot-separated adapter-owned key path.
    pub key_path: String,
    /// Canonical value hash before the requested mutation, if present.
    pub before_value_hash: Option<String>,
    /// Canonical value hash after the requested mutation, if retained/present.
    pub after_value_hash: Option<String>,
}

fn hash_owned_value(value: &[u8]) -> String {
    blake3::hash(value).to_hex().to_string()
}

/// Hashes a TOML-owned item including formatting attached to a standard table
/// header. `toml_edit::Table`'s `Display` implementation intentionally renders
/// only the table body, so hashing `Item::to_string()` alone would ignore a
/// user-added comment on `[mcp_servers.compresso]`.
pub(crate) fn hash_toml_owned_item(item: &Item) -> String {
    let mut hasher = blake3::Hasher::new();
    match item {
        Item::Table(table) => {
            hasher.update(b"table\0");
            hash_toml_decor(&mut hasher, table.decor());
        }
        Item::Value(_) => {
            hasher.update(b"value\0");
        }
        Item::ArrayOfTables(_) => {
            hasher.update(b"array_of_tables\0");
        }
        Item::None => {
            hasher.update(b"none\0");
        }
    }
    hasher.update(item.to_string().as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn hash_toml_decor(hasher: &mut blake3::Hasher, decor: &Decor) {
    for raw in [decor.prefix(), decor.suffix()] {
        match raw {
            Some(raw) => {
                hasher.update(b"some\0");
                // DocumentMut de-spans raw strings while parsing. A manually
                // constructed spanned value cannot occur at this boundary;
                // retain a distinct marker rather than silently equating it.
                match raw.as_str() {
                    Some(value) => hasher.update(value.as_bytes()),
                    None => hasher.update(b"spanned\0"),
                };
            }
            None => {
                hasher.update(b"default\0");
            }
        }
        hasher.update(b"\0");
    }
}

/// Trusted adapter request used to issue a plan.
///
/// `target_contents` must be the complete exact UTF-8 file that the adapter
/// rendered after changing only `owned_keys`.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Constructed by the E6-02..04 adapter modules.
pub(crate) struct MutationRequest {
    /// Install or uninstall.
    pub(crate) operation: MutationKind,
    /// Adapter client identifier.
    pub(crate) client: String,
    /// Absolute config path.
    pub(crate) config_path: PathBuf,
    /// Format-specific ownership validator.
    pub(crate) config_format: ConfigFormat,
    /// Complete exact UTF-8 target bytes.
    pub(crate) target_contents: String,
    /// Exact adapter-owned paths; hashes are derived from actual documents.
    pub(crate) owned_paths: Vec<String>,
    /// Version stored on an install receipt.
    pub(crate) compresso_version: String,
    /// Required for uninstall and forbidden for install.
    pub(crate) prior_receipt: Option<InstallReceipt>,
    /// Content-free warnings, including retained user modifications.
    pub(crate) warnings: Vec<String>,
}

/// Immutable, base-bound integration mutation plan.
///
/// This internal plan contains the exact target document required for safe
/// apply and crash recovery. User-facing boundaries must serialize
/// [`MutationPlan::public_preview`] instead so unrelated configuration and
/// credentials never enter terminal or assistant output.
#[derive(Clone, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationPlan {
    /// Boundary schema version.
    pub schema_version: u32,
    /// Discriminator used by the integration schema.
    pub document_type: String,
    /// Install or uninstall.
    pub operation: MutationKind,
    /// Format used for owned-path validation.
    pub config_format: ConfigFormat,
    /// Adapter client identifier.
    pub client: String,
    /// Absolute target path.
    pub config_path: PathBuf,
    /// Whether the target existed when previewed.
    pub base_exists: bool,
    /// Existence-aware full-file base hash.
    pub base_hash: String,
    /// Full-file result hash.
    pub result_hash: String,
    /// Complete exact output bytes as UTF-8.
    pub(crate) target_contents: String,
    /// Human-readable, secret-safe key-level unified diff.
    pub unified_diff: String,
    /// Hash of `unified_diff` bytes.
    pub diff_hash: String,
    /// Hash of every plan field except this field.
    pub plan_hash: String,
    /// Session that issued the plan.
    pub session_id: String,
    /// Unix issue timestamp in seconds.
    pub issued_at: i64,
    /// Unix expiry timestamp, exactly ten minutes after issue.
    pub expires_at: i64,
    /// Exact backup path or new-file sentinel.
    pub backup_path: String,
    /// Adapter-owned key changes in stable order.
    pub owned_keys: Vec<OwnedKeyMutation>,
    /// Version stored on installation.
    pub compresso_version: String,
    /// Receipt being uninstalled, if any.
    pub prior_receipt: Option<InstallReceipt>,
    /// Content-free preview warnings.
    pub warnings: Vec<String>,
}

/// Private wire projection used only for integrity hashing and authenticated
/// crash-recovery journals. Unlike [`MutationPlanPreview`], this contains the
/// exact target document and must never cross a user-facing boundary.
#[derive(Serialize)]
struct ExactMutationPlanRef<'a> {
    schema_version: u32,
    document_type: &'a str,
    operation: MutationKind,
    config_format: ConfigFormat,
    client: &'a str,
    config_path: &'a Path,
    base_exists: bool,
    base_hash: &'a str,
    result_hash: &'a str,
    target_contents: &'a str,
    unified_diff: &'a str,
    diff_hash: &'a str,
    plan_hash: &'a str,
    session_id: &'a str,
    issued_at: i64,
    expires_at: i64,
    backup_path: &'a str,
    owned_keys: &'a [OwnedKeyMutation],
    compresso_version: &'a str,
    prior_receipt: &'a Option<InstallReceipt>,
    warnings: &'a [String],
}

impl<'a> From<&'a MutationPlan> for ExactMutationPlanRef<'a> {
    fn from(plan: &'a MutationPlan) -> Self {
        Self {
            schema_version: plan.schema_version,
            document_type: &plan.document_type,
            operation: plan.operation,
            config_format: plan.config_format,
            client: &plan.client,
            config_path: &plan.config_path,
            base_exists: plan.base_exists,
            base_hash: &plan.base_hash,
            result_hash: &plan.result_hash,
            target_contents: &plan.target_contents,
            unified_diff: &plan.unified_diff,
            diff_hash: &plan.diff_hash,
            plan_hash: &plan.plan_hash,
            session_id: &plan.session_id,
            issued_at: plan.issued_at,
            expires_at: plan.expires_at,
            backup_path: &plan.backup_path,
            owned_keys: &plan.owned_keys,
            compresso_version: &plan.compresso_version,
            prior_receipt: &plan.prior_receipt,
            warnings: &plan.warnings,
        }
    }
}

fn serialize_exact_mutation_plan<S>(plan: &MutationPlan, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    ExactMutationPlanRef::from(plan).serialize(serializer)
}

/// Secret-safe user-facing projection of an integration mutation plan.
///
/// Exact target bytes and the prior receipt are deliberately absent. The
/// full-file base/result hashes retain drift and approval binding without
/// copying unrelated configuration into CLI or assistant-visible output.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MutationPlanPreview<'a> {
    pub schema_version: u32,
    pub document_type: &'static str,
    pub operation: MutationKind,
    pub config_format: ConfigFormat,
    pub client: &'a str,
    pub config_path: &'a Path,
    pub base_exists: bool,
    pub base_hash: &'a str,
    pub result_hash: &'a str,
    pub unified_diff: &'a str,
    pub diff_hash: &'a str,
    pub plan_hash: &'a str,
    pub session_id: &'a str,
    pub issued_at: i64,
    pub expires_at: i64,
    pub backup_path: &'a str,
    pub owned_keys: Vec<&'a str>,
    pub compresso_version: &'a str,
    pub warnings: &'a [String],
}

impl MutationPlan {
    /// Whether applying this plan deletes the target file.
    pub fn removes_file(&self) -> bool {
        matches!(
            planned_result(self.config_format, &self.target_contents),
            TargetState::Missing
        )
    }

    /// Returns the only representation safe to expose at a user-facing
    /// process boundary.
    pub fn public_preview(&self) -> MutationPlanPreview<'_> {
        MutationPlanPreview {
            schema_version: self.schema_version,
            document_type: "mutation_plan_preview",
            operation: self.operation,
            config_format: self.config_format,
            client: &self.client,
            config_path: &self.config_path,
            base_exists: self.base_exists,
            base_hash: &self.base_hash,
            result_hash: &self.result_hash,
            unified_diff: &self.unified_diff,
            diff_hash: &self.diff_hash,
            plan_hash: &self.plan_hash,
            session_id: &self.session_id,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            backup_path: &self.backup_path,
            owned_keys: self
                .owned_keys
                .iter()
                .map(|mutation| mutation.key_path.as_str())
                .collect(),
            compresso_version: &self.compresso_version,
            warnings: &self.warnings,
        }
    }
}

/// General-purpose serialization is intentionally restricted to the
/// secret-safe preview. Exact bytes have a separate private serializer used
/// only by authenticated local persistence and integrity hashing.
impl Serialize for MutationPlan {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.public_preview().serialize(serializer)
    }
}

impl fmt::Debug for MutationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MutationPlan")
            .field("public_preview", &self.public_preview())
            .field(
                "target_contents",
                &format_args!("<redacted:{} bytes>", self.target_contents.len()),
            )
            .field("prior_receipt", &self.prior_receipt)
            .finish()
    }
}

/// Minimal approval payload accepted by apply.
///
/// The exact target document remains in the manager's issued-plan registry;
/// callers cannot submit replacement bytes at apply time.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanApproval {
    /// Approved plan identity.
    pub plan_hash: String,
    /// Approved whole-file base.
    pub base_hash: String,
    /// Approved exact preview.
    pub diff_hash: String,
    /// Approving session.
    pub session_id: String,
}

impl From<&MutationPlan> for PlanApproval {
    fn from(plan: &MutationPlan) -> Self {
        Self {
            plan_hash: plan.plan_hash.clone(),
            base_hash: plan.base_hash.clone(),
            diff_hash: plan.diff_hash.clone(),
            session_id: plan.session_id.clone(),
        }
    }
}

/// Opaque authority to renew and apply one approved install after a long-running local step.
///
/// The authorization is bound to the exact [`PlanManager`] that issued it, cannot be cloned, and
/// is consumed by [`PlanManager::apply_deferred_install`]. Creating it removes the original plan
/// from the manager's normal apply registry so the same approval cannot be used twice.
pub struct DeferredInstallAuthorization {
    session_id: String,
    manager_identity: Arc<()>,
    plan: MutationPlan,
}

/// Safe uninstall result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreReport {
    /// Client whose configuration was checked.
    pub client: String,
    /// Target configuration.
    pub config_path: PathBuf,
    /// Receipt-owned keys actually removed.
    pub removed_keys: Vec<String>,
    /// User-modified owned keys deliberately retained.
    pub retained_keys: Vec<String>,
    /// Content-free warnings, including `key_modified`.
    pub warnings: Vec<String>,
    /// True when the same approved plan had already committed.
    pub already_applied: bool,
}

/// Result of a recoverable config transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyReport {
    /// Installation receipt for install plans.
    pub receipt: Option<InstallReceipt>,
    /// Restore report for uninstall plans.
    pub restore: Option<RestoreReport>,
    /// True when no new filesystem write was needed.
    pub already_applied: bool,
}

/// Result of applying a deferred install with freshly renewed timing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeferredInstallApplyReport {
    /// Exact renewed plan used for the commit.
    pub plan: MutationPlan,
    /// Full apply outcome, including whether an existing install was reused.
    pub report: ApplyReport,
}

/// Clock boundary for deterministic expiry tests.
pub trait PlanClock: Send + Sync {
    /// Current Unix timestamp in seconds.
    fn now_unix_seconds(&self) -> i64;
}

/// System wall clock used by production plan managers.
pub struct SystemPlanClock;

impl PlanClock for SystemPlanClock {
    fn now_unix_seconds(&self) -> i64 {
        unix_seconds(SystemTime::now())
    }
}

/// Typed integration mutation failures.
#[derive(Debug, Error)]
pub enum IntegrationError {
    /// Plan manager session was empty.
    #[error("integration plan session must not be empty")]
    EmptySession,
    /// Adapter request was structurally invalid.
    #[error("invalid integration mutation request: {reason}")]
    InvalidRequest { reason: String },
    /// The requested integration scope or platform is intentionally unsupported.
    #[error("unsupported integration operation: {reason}")]
    Unsupported { reason: String },
    /// Plan approval was never issued by this manager or was altered.
    /// The client's hook file already defines hooks for an event Compresso
    /// would own. Compresso never merges into or rewrites them; `entry` is the
    /// exact group a user can add by hand.
    #[error(
        "{key} in {path} already defines hooks that Compresso did not write and will not rewrite; add this entry to that array by hand instead: {entry}"
    )]
    UserHooksPresent {
        path: PathBuf,
        key: String,
        entry: String,
    },

    /// A file Compresso would own outright already exists and was not
    /// written by Compresso. It is never overwritten.
    #[error(
        "{path} already exists and was not written by Compresso; refusing to overwrite it (move or rename it, then retry)"
    )]
    UserFilePresent { path: PathBuf },

    #[error("integration plan approval is forged or unknown")]
    ForgedPlan,
    /// Approval came from another local session.
    #[error("integration plan session mismatch")]
    SessionMismatch,
    /// Plan is no longer valid.
    #[error("integration plan expired at {expires_at}")]
    PlanExpired { expires_at: i64 },
    /// Target changed anywhere after preview.
    #[error("config_drift: target {path} changed after preview")]
    ConfigDrift { path: PathBuf },
    /// The uninstall target disappeared and must not be recreated implicitly.
    #[error("uninstall target {path} is missing; refusing to recreate it")]
    UninstallTargetMissing { path: PathBuf },
    /// No canonical receipt exists for the uninstall plan's claimed install.
    #[error("canonical uninstall receipt {plan_hash} was not found")]
    UninstallReceiptMissing { plan_hash: String },
    /// The uninstall plan's receipt differs from the canonical stored receipt.
    #[error("uninstall receipt {plan_hash} does not match canonical state")]
    UninstallReceiptMismatch { plan_hash: String },
    /// The receipt is not the currently active install for this target.
    #[error("uninstall receipt {plan_hash} is not active for this target")]
    UninstallReceiptNotActive { plan_hash: String },
    /// An already-uninstalled receipt was presented outside an exact repeat.
    #[error("uninstall receipt {plan_hash} is not an idempotent repeat for the current target")]
    UninstallReceiptNotIdempotent { plan_hash: String },
    /// A prior crash left a commit requiring recovery.
    #[error("pending integration recovery journal exists for {path}")]
    PendingRecovery { path: PathBuf },
    /// Recovery cannot safely decide between base and target.
    #[error("integration recovery conflict for {path}")]
    RecoveryConflict { path: PathBuf },
    /// Planned timestamped backup already exists with different bytes.
    #[error("integration backup collision at {path}")]
    BackupCollision { path: PathBuf },
    /// Filesystem operation failed.
    #[error("integration filesystem operation failed for {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Internal journal failed decoding or validation.
    #[error("invalid integration recovery journal at {path}: {reason}")]
    InvalidJournal { path: PathBuf, reason: String },
    /// Receipt persistence failed; the durable journal remains recoverable.
    #[error(transparent)]
    Receipt(#[from] ReceiptError),
    /// Trait default was invoked with the wrong plan operation.
    #[error("wrong integration plan kind: expected {expected:?}, got {actual:?}")]
    WrongPlanKind {
        expected: MutationKind,
        actual: MutationKind,
    },
    /// Plan registry lock was poisoned.
    #[error("integration plan registry lock was poisoned")]
    LockPoisoned,
}

/// Session-bound issuer and committer for integration mutation plans.
pub struct PlanManager {
    session_id: String,
    clock: Arc<dyn PlanClock>,
    deferred_identity: Arc<()>,
    issued: Mutex<BTreeMap<String, MutationPlan>>,
    completed: Mutex<BTreeMap<String, ApplyReport>>,
    apply_lock: Mutex<()>,
}

impl PlanManager {
    /// Starts a production plan session.
    pub fn new(session_id: impl Into<String>) -> Result<Self, IntegrationError> {
        Self::with_clock(session_id, Arc::new(SystemPlanClock))
    }

    /// Starts a plan session with an injected clock.
    pub fn with_clock(
        session_id: impl Into<String>,
        clock: Arc<dyn PlanClock>,
    ) -> Result<Self, IntegrationError> {
        let session_id = session_id.into();
        if session_id.trim().is_empty() {
            return Err(IntegrationError::EmptySession);
        }
        Ok(Self {
            session_id,
            clock,
            deferred_identity: Arc::new(()),
            issued: Mutex::new(BTreeMap::new()),
            completed: Mutex::new(BTreeMap::new()),
            apply_lock: Mutex::new(()),
        })
    }

    /// Issues a short-lived preview bound to the current full target state.
    #[allow(dead_code)] // Called by the adapter modules added in E6-02..04.
    pub(crate) fn issue(
        &self,
        mut request: MutationRequest,
    ) -> Result<MutationPlan, IntegrationError> {
        validate_request(&request)?;
        if journal_path(&request.config_path).exists() {
            return Err(IntegrationError::PendingRecovery {
                path: request.config_path,
            });
        }
        request.owned_paths.sort();
        request.warnings.sort();
        request.warnings.dedup();

        let base = read_target(&request.config_path)?;
        if let TargetState::Present(bytes) = &base
            && request.config_format != ConfigFormat::Markdown
            && !request.target_contents.is_empty()
        {
            // Bind the plan to bytes that keep the file's byte-order mark and
            // line endings; every hash below covers this final shaping.
            request.target_contents = preserve_text_fidelity(bytes, &request.target_contents);
        }
        if let Some(original) = pristine_uninstall_target(&request, &base) {
            request.target_contents = original;
        }
        let owned_keys = derive_owned_mutations(
            request.operation,
            request.config_format,
            &base,
            &request.target_contents,
            &request.owned_paths,
        )?;
        validate_owned_semantics(&request, &owned_keys)?;
        let base_hash = hash_target(&base);
        let target = planned_result(request.config_format, &request.target_contents);
        let result_hash = hash_target(&target);
        let issued_at = self.clock.now_unix_seconds();
        let backup_path = if matches!(base, TargetState::Missing) {
            CREATED_FILE_BACKUP_SENTINEL.to_owned()
        } else if matches!(target, TargetState::Missing) {
            // Ownership semantics allow deletion only while the file still
            // equals the installed bytes.
            UNMODIFIED_FILE_REMOVED_BACKUP_SENTINEL.to_owned()
        } else {
            planned_backup_path(&request.config_path, issued_at)?
                .to_string_lossy()
                .into_owned()
        };
        let unified_diff = make_unified_diff(
            &request.config_path,
            request.config_format,
            &base,
            &request.target_contents,
            request.operation,
            &owned_keys,
            &backup_path,
        )?;
        let diff_hash = blake3::hash(unified_diff.as_bytes()).to_hex().to_string();
        let mut plan = MutationPlan {
            schema_version: PLAN_SCHEMA_VERSION,
            document_type: "mutation_plan".to_owned(),
            operation: request.operation,
            config_format: request.config_format,
            client: request.client,
            config_path: request.config_path,
            base_exists: matches!(base, TargetState::Present(_)),
            base_hash,
            result_hash,
            target_contents: request.target_contents,
            unified_diff,
            diff_hash,
            plan_hash: String::new(),
            session_id: self.session_id.clone(),
            issued_at,
            expires_at: issued_at.saturating_add(PLAN_TTL_SECONDS),
            backup_path,
            owned_keys,
            compresso_version: request.compresso_version,
            prior_receipt: request.prior_receipt,
            warnings: request.warnings,
        };
        plan.plan_hash = compute_plan_hash(&plan)?;
        self.issued
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?
            .insert(plan.plan_hash.clone(), plan.clone());
        Ok(plan)
    }

    /// Applies an exact issued plan after all approval and base checks pass.
    pub fn apply(
        &self,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
    ) -> Result<ApplyReport, IntegrationError> {
        self.apply_for_kind(approval, receipts, None)
    }

    fn apply_for_kind(
        &self,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
        expected_kind: Option<MutationKind>,
    ) -> Result<ApplyReport, IntegrationError> {
        let _apply_guard = self
            .apply_lock
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?;
        let apply_at = self.clock.now_unix_seconds();
        self.apply_for_kind_locked(
            approval,
            receipts,
            expected_kind,
            PendingJournalPolicy::Recover,
            apply_at,
        )
    }

    fn apply_for_kind_locked(
        &self,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
        expected_kind: Option<MutationKind>,
        pending_journal: PendingJournalPolicy,
        apply_at: i64,
    ) -> Result<ApplyReport, IntegrationError> {
        let plan = self.validate_approval_at(approval, apply_at)?;
        if let Some(expected) = expected_kind
            && plan.operation != expected
        {
            return Err(IntegrationError::WrongPlanKind {
                expected,
                actual: plan.operation,
            });
        }
        if matches!(pending_journal, PendingJournalPolicy::Reject)
            && journal_path(&plan.config_path).exists()
        {
            return Err(IntegrationError::PendingRecovery {
                path: plan.config_path,
            });
        }
        if let Some(report) = self
            .completed
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?
            .get(&approval.plan_hash)
            .cloned()
        {
            if matches!(pending_journal, PendingJournalPolicy::Reject)
                && journal_path(&plan.config_path).exists()
            {
                return Err(IntegrationError::PendingRecovery {
                    path: plan.config_path,
                });
            }
            return Ok(mark_report_already_applied(report));
        }
        if journal_path(&plan.config_path).exists() {
            match pending_journal {
                PendingJournalPolicy::Reject => {
                    return Err(IntegrationError::PendingRecovery {
                        path: plan.config_path,
                    });
                }
                PendingJournalPolicy::Recover => {
                    let (recovered_hash, recovered) = recover_journal(&plan.config_path, receipts)?;
                    if recovered_hash == plan.plan_hash {
                        self.completed
                            .lock()
                            .map_err(|_| IntegrationError::LockPoisoned)?
                            .insert(plan.plan_hash.clone(), recovered.clone());
                        return Ok(mark_report_already_applied(recovered));
                    }
                }
            }
        }

        let current = read_target(&plan.config_path)?;
        let current_hash = hash_target(&current);
        let authorized_uninstall_receipt = if plan.operation == MutationKind::Uninstall {
            Some(authorize_uninstall_receipt(&plan, &current, receipts)?)
        } else {
            None
        };
        if plan.operation == MutationKind::Install
            && current_hash == plan.result_hash
            && let Some(receipt) = matching_active_receipt(&plan, receipts)?
        {
            if matches!(pending_journal, PendingJournalPolicy::Reject)
                && journal_path(&plan.config_path).exists()
            {
                return Err(IntegrationError::PendingRecovery {
                    path: plan.config_path,
                });
            }
            let report = ApplyReport {
                receipt: Some(receipt),
                restore: None,
                already_applied: true,
            };
            self.completed
                .lock()
                .map_err(|_| IntegrationError::LockPoisoned)?
                .insert(plan.plan_hash.clone(), report.clone());
            return Ok(report);
        }
        if plan.operation == MutationKind::Uninstall
            && current_hash == plan.result_hash
            && let Some(report) = matching_uninstall_noop(
                &plan,
                authorized_uninstall_receipt
                    .as_ref()
                    .expect("uninstall authorization was established"),
            )
        {
            if matches!(pending_journal, PendingJournalPolicy::Reject)
                && journal_path(&plan.config_path).exists()
            {
                return Err(IntegrationError::PendingRecovery {
                    path: plan.config_path,
                });
            }
            self.completed
                .lock()
                .map_err(|_| IntegrationError::LockPoisoned)?
                .insert(plan.plan_hash.clone(), report.clone());
            return Ok(report);
        }
        if current_hash != plan.base_hash {
            return Err(IntegrationError::ConfigDrift {
                path: plan.config_path,
            });
        }

        let commit = build_commit(&plan, apply_at, authorized_uninstall_receipt.as_ref())?;
        write_journal(&plan.config_path, &commit, receipts)?;
        finish_commit(&commit, receipts)?;
        self.completed
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?
            .insert(plan.plan_hash.clone(), commit.report.clone());
        Ok(commit.report)
    }

    /// Validates and consumes an approved install plan before a long-running local operation.
    ///
    /// The plan must still be unexpired, base-bound, and free of pending recovery. Authorization
    /// performs no filesystem mutation and makes the original approval ineligible for normal
    /// apply. The returned opaque value may be used exactly once with this manager.
    pub fn authorize_deferred_install(
        &self,
        approval: &PlanApproval,
    ) -> Result<DeferredInstallAuthorization, IntegrationError> {
        let _apply_guard = self
            .apply_lock
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?;
        let authorized_at = self.clock.now_unix_seconds();
        let plan = self.validate_approval_at(approval, authorized_at)?;
        if plan.operation != MutationKind::Install {
            return Err(IntegrationError::WrongPlanKind {
                expected: MutationKind::Install,
                actual: plan.operation,
            });
        }
        if journal_path(&plan.config_path).exists() {
            return Err(IntegrationError::PendingRecovery {
                path: plan.config_path,
            });
        }
        let current = read_target(&plan.config_path)?;
        if hash_target(&current) != plan.base_hash {
            return Err(IntegrationError::ConfigDrift {
                path: plan.config_path,
            });
        }

        let removed = self
            .issued
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?
            .remove(&plan.plan_hash)
            .ok_or(IntegrationError::ForgedPlan)?;
        Ok(DeferredInstallAuthorization {
            session_id: self.session_id.clone(),
            manager_identity: Arc::clone(&self.deferred_identity),
            plan: removed,
        })
    }

    /// Renews and applies one previously authorized deferred install.
    ///
    /// Renewal changes only `issued_at`, `expires_at`, and the derived `plan_hash`; the exact
    /// approved backup path, unified diff, diff hash, base, target bytes, and owned-key effect are
    /// preserved. Any pending recovery journal fails closed and is never replayed by this path.
    pub fn apply_deferred_install(
        &self,
        authorization: DeferredInstallAuthorization,
        receipts: &dyn ReceiptStore,
    ) -> Result<DeferredInstallApplyReport, IntegrationError> {
        let _apply_guard = self
            .apply_lock
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?;
        if authorization.session_id != self.session_id
            || !Arc::ptr_eq(&authorization.manager_identity, &self.deferred_identity)
        {
            return Err(IntegrationError::SessionMismatch);
        }

        let source = authorization.plan;
        if source.session_id != self.session_id
            || source.operation != MutationKind::Install
            || source.plan_hash != compute_plan_hash(&source)?
        {
            return Err(IntegrationError::ForgedPlan);
        }
        if journal_path(&source.config_path).exists() {
            return Err(IntegrationError::PendingRecovery {
                path: source.config_path,
            });
        }
        let current = read_target(&source.config_path)?;
        if hash_target(&current) != source.base_hash {
            return Err(IntegrationError::ConfigDrift {
                path: source.config_path,
            });
        }

        let apply_at = self.clock.now_unix_seconds();
        let mut renewed = source;
        renewed.issued_at = apply_at;
        renewed.expires_at = apply_at.saturating_add(PLAN_TTL_SECONDS);
        renewed.plan_hash.clear();
        renewed.plan_hash = compute_plan_hash(&renewed)?;
        self.issued
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?
            .insert(renewed.plan_hash.clone(), renewed.clone());

        let approval = PlanApproval::from(&renewed);
        let applied = self.apply_for_kind_locked(
            &approval,
            receipts,
            Some(MutationKind::Install),
            PendingJournalPolicy::Reject,
            apply_at,
        );
        let report = match applied {
            Ok(report) => report,
            Err(error) => {
                self.issued
                    .lock()
                    .map_err(|_| IntegrationError::LockPoisoned)?
                    .remove(&renewed.plan_hash);
                return Err(error);
            }
        };
        Ok(DeferredInstallApplyReport {
            plan: renewed,
            report,
        })
    }

    /// Applies an install approval and returns its receipt.
    ///
    /// A valid uninstall approval is rejected before any filesystem mutation.
    pub fn apply_install(
        &self,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
    ) -> Result<InstallReceipt, IntegrationError> {
        self.apply_for_kind(approval, receipts, Some(MutationKind::Install))?
            .receipt
            .ok_or_else(|| IntegrationError::InvalidRequest {
                reason: "install commit did not produce a receipt".to_owned(),
            })
    }

    /// Applies an uninstall approval and returns its restoration report.
    ///
    /// A valid install approval is rejected before any filesystem mutation.
    pub fn apply_uninstall(
        &self,
        approval: &PlanApproval,
        receipts: &dyn ReceiptStore,
    ) -> Result<RestoreReport, IntegrationError> {
        self.apply_for_kind(approval, receipts, Some(MutationKind::Uninstall))?
            .restore
            .ok_or_else(|| IntegrationError::InvalidRequest {
                reason: "uninstall commit did not produce a restore report".to_owned(),
            })
    }

    /// Completes a transaction interrupted after its durable journal write.
    ///
    /// Recovery is intentionally not session-bound: a new process must be able
    /// to finish an already-approved local commit after a crash.
    pub fn recover(
        &self,
        config_path: &Path,
        receipts: &dyn ReceiptStore,
    ) -> Result<Option<ApplyReport>, IntegrationError> {
        let _apply_guard = self
            .apply_lock
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?;
        if !journal_path(config_path).exists() {
            return Ok(None);
        }
        recover_journal(config_path, receipts).map(|(_, report)| Some(report))
    }

    fn validate_approval_at(
        &self,
        approval: &PlanApproval,
        apply_at: i64,
    ) -> Result<MutationPlan, IntegrationError> {
        if approval.session_id != self.session_id {
            return Err(IntegrationError::SessionMismatch);
        }
        let plan = self
            .issued
            .lock()
            .map_err(|_| IntegrationError::LockPoisoned)?
            .get(&approval.plan_hash)
            .cloned()
            .ok_or(IntegrationError::ForgedPlan)?;
        if plan.session_id != self.session_id
            || plan.base_hash != approval.base_hash
            || plan.diff_hash != approval.diff_hash
            || plan.plan_hash != compute_plan_hash(&plan)?
        {
            return Err(IntegrationError::ForgedPlan);
        }
        if apply_at >= plan.expires_at {
            return Err(IntegrationError::PlanExpired {
                expires_at: plan.expires_at,
            });
        }
        Ok(plan)
    }
}

#[derive(Clone, Copy)]
enum PendingJournalPolicy {
    Recover,
    Reject,
}

fn matching_active_receipt(
    plan: &MutationPlan,
    receipts: &dyn ReceiptStore,
) -> Result<Option<InstallReceipt>, ReceiptError> {
    Ok(receipts
        .active_for(&plan.client, &plan.config_path)?
        .filter(|receipt| {
            receipt.keys_owned
                == plan
                    .owned_keys
                    .iter()
                    .map(|change| change.key_path.clone())
                    .collect::<Vec<_>>()
                && receipt.owned_value_hashes == installed_value_hashes(plan)
        }))
}

fn authorize_uninstall_receipt(
    plan: &MutationPlan,
    current: &TargetState,
    receipts: &dyn ReceiptStore,
) -> Result<InstallReceipt, IntegrationError> {
    let prior = plan
        .prior_receipt
        .as_ref()
        .ok_or_else(|| IntegrationError::InvalidRequest {
            reason: "uninstall plan is missing its prior receipt".to_owned(),
        })?;
    authorize_uninstall_claim(plan, current, receipts, prior, false)
}

fn authorize_pending_uninstall_receipt(
    commit: &PendingCommit,
    current: &TargetState,
    receipts: &dyn ReceiptStore,
) -> Result<InstallReceipt, IntegrationError> {
    match authorize_uninstall_receipt(&commit.plan, current, receipts) {
        Ok(receipt) => Ok(receipt),
        Err(IntegrationError::UninstallReceiptMismatch { .. })
            if commit.persisted_receipt.uninstalled_at.is_some() =>
        {
            // A crash may occur after the canonical record was updated but
            // before its active head or journal was removed. The verified
            // journal binds this exact post-commit receipt.
            authorize_uninstall_claim(
                &commit.plan,
                current,
                receipts,
                &commit.persisted_receipt,
                true,
            )
        }
        Err(error) => Err(error),
    }
}

fn authorize_uninstall_claim(
    plan: &MutationPlan,
    current: &TargetState,
    receipts: &dyn ReceiptStore,
    claimed: &InstallReceipt,
    authenticated_commit_result: bool,
) -> Result<InstallReceipt, IntegrationError> {
    // A whole-file removal whose target is already absent is the planned
    // result (a crash between deletion and receipt persistence), not drift.
    if matches!(current, TargetState::Missing) && hash_target(current) != plan.result_hash {
        if plan.base_exists {
            return Err(IntegrationError::ConfigDrift {
                path: plan.config_path.clone(),
            });
        }
        return Err(IntegrationError::UninstallTargetMissing {
            path: plan.config_path.clone(),
        });
    }
    let stored = receipts.by_plan_hash(&claimed.plan_hash)?.ok_or_else(|| {
        IntegrationError::UninstallReceiptMissing {
            plan_hash: claimed.plan_hash.clone(),
        }
    })?;
    if stored != *claimed {
        return Err(IntegrationError::UninstallReceiptMismatch {
            plan_hash: claimed.plan_hash.clone(),
        });
    }

    let active = receipts.active_for(&plan.client, &plan.config_path)?;
    if stored.uninstalled_at.is_none() {
        if active.as_ref() != Some(&stored) {
            return Err(IntegrationError::UninstallReceiptNotActive {
                plan_hash: stored.plan_hash,
            });
        }
        return Ok(stored);
    }

    if active.is_some() {
        return Err(IntegrationError::UninstallReceiptNotActive {
            plan_hash: stored.plan_hash,
        });
    }
    if hash_target(current) != plan.result_hash
        || (!authenticated_commit_result && matching_uninstall_noop(plan, &stored).is_none())
    {
        return Err(IntegrationError::UninstallReceiptNotIdempotent {
            plan_hash: stored.plan_hash,
        });
    }
    Ok(stored)
}

fn matching_uninstall_noop(plan: &MutationPlan, stored: &InstallReceipt) -> Option<ApplyReport> {
    let fully_removed = plan
        .owned_keys
        .iter()
        .all(|change| change.before_value_hash.is_none() && change.after_value_hash.is_none())
        && stored.uninstalled_at.is_some();
    let safely_retained = plan.owned_keys.iter().all(|change| {
        change.before_value_hash.is_some()
            && change.before_value_hash == change.after_value_hash
            && change.before_value_hash != stored.owned_value_hashes.get(&change.key_path).cloned()
    }) && stored.uninstalled_at.is_none();
    if !fully_removed && !safely_retained {
        return None;
    }
    let restore = RestoreReport {
        client: plan.client.clone(),
        config_path: plan.config_path.clone(),
        removed_keys: if fully_removed {
            plan.owned_keys
                .iter()
                .map(|change| change.key_path.clone())
                .collect()
        } else {
            Vec::new()
        },
        retained_keys: if safely_retained {
            plan.owned_keys
                .iter()
                .map(|change| change.key_path.clone())
                .collect()
        } else {
            Vec::new()
        },
        warnings: plan.warnings.clone(),
        already_applied: true,
    };
    Some(ApplyReport {
        receipt: None,
        restore: Some(restore),
        already_applied: true,
    })
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingCommit {
    journal_version: u32,
    #[serde(serialize_with = "serialize_exact_mutation_plan")]
    plan: MutationPlan,
    base_contents: Option<String>,
    persisted_receipt: InstallReceipt,
    report: ApplyReport,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalEnvelope {
    envelope_version: u32,
    commit: PendingCommit,
    mac: String,
}

fn build_commit(
    plan: &MutationPlan,
    now: i64,
    authorized_uninstall_receipt: Option<&InstallReceipt>,
) -> Result<PendingCommit, IntegrationError> {
    let base = read_target(&plan.config_path)?;
    if hash_target(&base) != plan.base_hash {
        return Err(IntegrationError::ConfigDrift {
            path: plan.config_path.clone(),
        });
    }
    let base_contents = match base {
        TargetState::Missing => None,
        TargetState::Present(bytes) => {
            Some(
                String::from_utf8(bytes).map_err(|_| IntegrationError::InvalidRequest {
                    reason: "existing config is not UTF-8".to_owned(),
                })?,
            )
        }
    };
    match plan.operation {
        MutationKind::Install => {
            let receipt = InstallReceipt {
                schema_version: RECEIPT_SCHEMA_VERSION,
                document_type: "install_receipt".to_owned(),
                plan_hash: plan.plan_hash.clone(),
                client: plan.client.clone(),
                config_path: plan.config_path.clone(),
                keys_owned: plan
                    .owned_keys
                    .iter()
                    .map(|change| change.key_path.clone())
                    .collect(),
                owned_value_hashes: installed_value_hashes(plan),
                backup_path: plan.backup_path.clone(),
                installed_at: now,
                compresso_version: plan.compresso_version.clone(),
                uninstalled_at: None,
            };
            receipt.validate()?;
            Ok(PendingCommit {
                journal_version: 1,
                plan: plan.clone(),
                base_contents,
                persisted_receipt: receipt.clone(),
                report: ApplyReport {
                    receipt: Some(receipt),
                    restore: None,
                    already_applied: false,
                },
            })
        }
        MutationKind::Uninstall => {
            let mut receipt = authorized_uninstall_receipt.cloned().ok_or_else(|| {
                IntegrationError::InvalidRequest {
                    reason: "uninstall commit is missing canonical receipt authorization"
                        .to_owned(),
                }
            })?;
            let removed_keys = plan
                .owned_keys
                .iter()
                .filter(|change| change.after_value_hash.is_none())
                .map(|change| change.key_path.clone())
                .collect::<Vec<_>>();
            let retained_keys = plan
                .owned_keys
                .iter()
                .filter(|change| change.after_value_hash.is_some())
                .map(|change| change.key_path.clone())
                .collect::<Vec<_>>();
            if retained_keys.is_empty() {
                receipt.uninstalled_at = Some(now);
            }
            receipt.validate()?;
            Ok(PendingCommit {
                journal_version: 1,
                plan: plan.clone(),
                base_contents,
                persisted_receipt: receipt,
                report: ApplyReport {
                    receipt: None,
                    restore: Some(RestoreReport {
                        client: plan.client.clone(),
                        config_path: plan.config_path.clone(),
                        removed_keys,
                        retained_keys,
                        warnings: plan.warnings.clone(),
                        already_applied: false,
                    }),
                    already_applied: false,
                },
            })
        }
    }
}

fn installed_value_hashes(plan: &MutationPlan) -> BTreeMap<String, String> {
    plan.owned_keys
        .iter()
        .filter_map(|change| {
            change
                .after_value_hash
                .as_ref()
                .map(|hash| (change.key_path.clone(), hash.clone()))
        })
        .collect()
}

fn finish_commit(
    commit: &PendingCommit,
    receipts: &dyn ReceiptStore,
) -> Result<(), IntegrationError> {
    validate_pending_commit(commit)?;
    let current = read_target(&commit.plan.config_path)?;
    if commit.plan.operation == MutationKind::Uninstall {
        // Re-establish canonical authority during both normal commit and
        // authenticated crash recovery before backup or config side effects.
        authorize_pending_uninstall_receipt(commit, &current, receipts)?;
    }
    let current_hash = hash_target(&current);
    if current_hash != commit.plan.base_hash && current_hash != commit.plan.result_hash {
        return Err(IntegrationError::RecoveryConflict {
            path: commit.plan.config_path.clone(),
        });
    }
    ensure_backup(commit)?;
    if current_hash == commit.plan.base_hash {
        // Close the window between journal creation and replacement as much as
        // portable std permits by re-reading immediately before rename.
        let rechecked = read_target(&commit.plan.config_path)?;
        if hash_target(&rechecked) != commit.plan.base_hash {
            return Err(IntegrationError::ConfigDrift {
                path: commit.plan.config_path.clone(),
            });
        }
        match planned_result(commit.plan.config_format, &commit.plan.target_contents) {
            TargetState::Present(bytes) => atomic_replace(
                &commit.plan.config_path,
                &bytes,
                &commit.plan.plan_hash[..12],
            ),
            TargetState::Missing => fs::remove_file(&commit.plan.config_path),
        }
        .map_err(|source| IntegrationError::Io {
            path: commit.plan.config_path.clone(),
            source,
        })?;
        sync_parent(&commit.plan.config_path).map_err(|source| IntegrationError::Io {
            path: commit.plan.config_path.clone(),
            source,
        })?;
    }
    let written = read_target(&commit.plan.config_path)?;
    if hash_target(&written) != commit.plan.result_hash {
        return Err(IntegrationError::RecoveryConflict {
            path: commit.plan.config_path.clone(),
        });
    }
    receipts.save(&commit.persisted_receipt)?;
    let journal = journal_path(&commit.plan.config_path);
    fs::remove_file(&journal).map_err(|source| IntegrationError::Io {
        path: journal,
        source,
    })?;
    // The config and receipt are committed before journal deletion. A failed
    // directory fsync here must not report failure after commit; if deletion
    // is not durable the authenticated journal may safely replay idempotently.
    let _post_commit_sync = sync_parent(&commit.plan.config_path);
    Ok(())
}

fn recover_journal(
    config_path: &Path,
    receipts: &dyn ReceiptStore,
) -> Result<(String, ApplyReport), IntegrationError> {
    let path = journal_path(config_path);
    let bytes = fs::read(&path).map_err(|source| IntegrationError::Io {
        path: path.clone(),
        source,
    })?;
    let envelope: JournalEnvelope =
        serde_json::from_slice(&bytes).map_err(|error| IntegrationError::InvalidJournal {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    if envelope.envelope_version != 1 {
        return Err(IntegrationError::InvalidJournal {
            path,
            reason: "unsupported journal envelope version".to_owned(),
        });
    }
    let payload =
        serde_json::to_vec(&envelope.commit).map_err(|error| IntegrationError::InvalidJournal {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    // Authentication precedes validation, backup creation, target reads, and
    // receipt writes. An adjacent self-consistent forged journal has no
    // authority without the user-private approval key.
    receipts.verify_journal(&payload, &envelope.mac)?;
    let commit = envelope.commit;
    validate_pending_commit(&commit)?;
    if commit.plan.config_path != config_path {
        return Err(IntegrationError::InvalidJournal {
            path,
            reason: "journal target path mismatch".to_owned(),
        });
    }
    finish_commit(&commit, receipts)?;
    Ok((commit.plan.plan_hash, commit.report))
}

fn validate_pending_commit(commit: &PendingCommit) -> Result<(), IntegrationError> {
    if commit.journal_version != 1
        || commit.plan.schema_version != PLAN_SCHEMA_VERSION
        || commit.plan.document_type != "mutation_plan"
        || commit.plan.plan_hash != compute_plan_hash(&commit.plan)?
        || commit.plan.result_hash
            != hash_target(&planned_result(
                commit.plan.config_format,
                &commit.plan.target_contents,
            ))
        || commit.plan.diff_hash
            != blake3::hash(commit.plan.unified_diff.as_bytes())
                .to_hex()
                .to_string()
    {
        return Err(IntegrationError::InvalidJournal {
            path: journal_path(&commit.plan.config_path),
            reason: "journal hashes or version do not validate".to_owned(),
        });
    }
    let stored_base = commit
        .base_contents
        .as_ref()
        .map_or(TargetState::Missing, |contents| {
            TargetState::Present(contents.as_bytes().to_vec())
        });
    if hash_target(&stored_base) != commit.plan.base_hash
        || commit.persisted_receipt.client != commit.plan.client
        || commit.persisted_receipt.config_path != commit.plan.config_path
    {
        return Err(IntegrationError::InvalidJournal {
            path: journal_path(&commit.plan.config_path),
            reason: "journal payload does not match the approved plan".to_owned(),
        });
    }
    commit.persisted_receipt.validate()?;
    validate_commit_outcome(commit)?;
    Ok(())
}

fn validate_commit_outcome(commit: &PendingCommit) -> Result<(), IntegrationError> {
    let invalid_outcome = || IntegrationError::InvalidJournal {
        path: journal_path(&commit.plan.config_path),
        reason: "journal receipt/report does not match the approved plan".to_owned(),
    };
    if commit.report.already_applied {
        return Err(invalid_outcome());
    }
    match commit.plan.operation {
        MutationKind::Install => {
            let receipt = &commit.persisted_receipt;
            if receipt.plan_hash != commit.plan.plan_hash
                || receipt.keys_owned
                    != commit
                        .plan
                        .owned_keys
                        .iter()
                        .map(|change| change.key_path.clone())
                        .collect::<Vec<_>>()
                || receipt.owned_value_hashes != installed_value_hashes(&commit.plan)
                || receipt.backup_path != commit.plan.backup_path
                || receipt.compresso_version != commit.plan.compresso_version
                || receipt.uninstalled_at.is_some()
                || receipt.installed_at < commit.plan.issued_at
                || receipt.installed_at >= commit.plan.expires_at
                || commit.report.receipt.as_ref() != Some(receipt)
                || commit.report.restore.is_some()
            {
                return Err(invalid_outcome());
            }
        }
        MutationKind::Uninstall => {
            let prior = commit
                .plan
                .prior_receipt
                .as_ref()
                .ok_or_else(invalid_outcome)?;
            let expected_removed = commit
                .plan
                .owned_keys
                .iter()
                .filter(|change| change.after_value_hash.is_none())
                .map(|change| change.key_path.clone())
                .collect::<Vec<_>>();
            let expected_retained = commit
                .plan
                .owned_keys
                .iter()
                .filter(|change| change.after_value_hash.is_some())
                .map(|change| change.key_path.clone())
                .collect::<Vec<_>>();
            let mut expected_receipt = prior.clone();
            if expected_retained.is_empty() {
                expected_receipt.uninstalled_at = commit.persisted_receipt.uninstalled_at;
                if expected_receipt
                    .uninstalled_at
                    .is_none_or(|at| at < commit.plan.issued_at || at >= commit.plan.expires_at)
                {
                    return Err(invalid_outcome());
                }
            }
            let Some(restore) = &commit.report.restore else {
                return Err(invalid_outcome());
            };
            if commit.persisted_receipt != expected_receipt
                || commit.report.receipt.is_some()
                || restore.client != commit.plan.client
                || restore.config_path != commit.plan.config_path
                || restore.removed_keys != expected_removed
                || restore.retained_keys != expected_retained
                || restore.warnings != commit.plan.warnings
                || restore.already_applied
            {
                return Err(invalid_outcome());
            }
        }
    }
    Ok(())
}

fn write_journal(
    config_path: &Path,
    commit: &PendingCommit,
    receipts: &dyn ReceiptStore,
) -> Result<(), IntegrationError> {
    let path = journal_path(config_path);
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| IntegrationError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let payload = serde_json::to_vec(commit).map_err(|error| IntegrationError::InvalidJournal {
        path: path.clone(),
        reason: error.to_string(),
    })?;
    let envelope = JournalEnvelope {
        envelope_version: 1,
        mac: receipts.seal_journal(&payload)?,
        commit: commit.clone(),
    };
    let bytes =
        serde_json::to_vec(&envelope).map_err(|error| IntegrationError::InvalidJournal {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    reserve_journal(&path, &bytes).map_err(|source| {
        if source.kind() == std::io::ErrorKind::AlreadyExists {
            IntegrationError::PendingRecovery {
                path: config_path.to_path_buf(),
            }
        } else {
            IntegrationError::Io {
                path: path.clone(),
                source,
            }
        }
    })
}

fn reserve_journal(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut random = [0_u8; 12];
    getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = path.with_file_name(format!(".{name}.reserve-{suffix}"));
    write_new_private(&temp, bytes)?;
    let link_result = publish_completed_temp_no_replace(&temp, path);
    if link_result.is_ok() {
        sync_parent(path)?;
    }
    let cleanup_result = if temp.exists() {
        fs::remove_file(&temp)
    } else {
        Ok(())
    };
    if link_result.is_ok() {
        cleanup_result?;
        sync_parent(path)?;
    }
    link_result
}

#[cfg(not(windows))]
pub(crate) fn publish_completed_temp_no_replace(
    temp: &Path,
    destination: &Path,
) -> std::io::Result<()> {
    // hard_link is an atomic, no-replace directory operation: only one
    // process can publish its fully fsynced inode.
    fs::hard_link(temp, destination)
}

#[cfg(windows)]
pub(crate) fn publish_completed_temp_no_replace(
    temp: &Path,
    destination: &Path,
) -> std::io::Result<()> {
    move_file_windows(temp, destination, false)
}

fn ensure_backup(commit: &PendingCommit) -> Result<(), IntegrationError> {
    let Some(base_contents) = &commit.base_contents else {
        return Ok(());
    };
    if commit.plan.backup_path == UNMODIFIED_FILE_REMOVED_BACKUP_SENTINEL {
        return Ok(());
    }
    let path = PathBuf::from(&commit.plan.backup_path);
    if path.exists() {
        let existing = fs::read(&path).map_err(|source| IntegrationError::Io {
            path: path.clone(),
            source,
        })?;
        if existing == base_contents.as_bytes() {
            return Ok(());
        }
        return Err(IntegrationError::BackupCollision { path });
    }
    write_new_private(&path, base_contents.as_bytes()).map_err(|source| IntegrationError::Io {
        path: path.clone(),
        source,
    })?;
    sync_parent(&path).map_err(|source| IntegrationError::Io { path, source })
}

/// Returns the exact pre-install bytes when a JSON uninstall of a file left
/// untouched since its install would otherwise rewrite an equivalent document
/// with different insignificant bytes.
fn pristine_uninstall_target(request: &MutationRequest, base: &TargetState) -> Option<String> {
    if request.operation != MutationKind::Uninstall || request.config_format != ConfigFormat::Json {
        return None;
    }
    let TargetState::Present(current) = base else {
        return None;
    };
    let receipt = request.prior_receipt.as_ref()?;
    if receipt.backup_path == CREATED_FILE_BACKUP_SENTINEL {
        return None;
    }
    pristine_json_backup(
        &request.config_path,
        Path::new(&receipt.backup_path),
        current,
        &request.target_contents,
    )
}

fn validate_request(request: &MutationRequest) -> Result<(), IntegrationError> {
    if !matches!(request.client.as_str(), "claude-code" | "codex" | "cursor") {
        return Err(invalid("client must be claude-code, codex, or cursor"));
    }
    if !request.config_path.is_absolute() {
        return Err(invalid("config_path must be absolute"));
    }
    if request.compresso_version.trim().is_empty() {
        return Err(invalid("compresso_version must not be empty"));
    }
    if request.owned_paths.is_empty() {
        return Err(invalid("owned_paths must not be empty"));
    }
    let mut unique = BTreeSet::new();
    for path in &request.owned_paths {
        if path.is_empty() || path.split('.').any(str::is_empty) || !unique.insert(path.as_str()) {
            return Err(invalid("owned key paths must be non-empty and unique"));
        }
    }
    match request.operation {
        MutationKind::Install => {
            if request.prior_receipt.is_some() {
                return Err(invalid("install must not carry a prior receipt"));
            }
            if request.config_format == ConfigFormat::Json && request.target_contents.is_empty() {
                return Err(invalid("an install target must not be empty"));
            }
        }
        MutationKind::Uninstall => {
            let receipt = request
                .prior_receipt
                .as_ref()
                .ok_or_else(|| invalid("uninstall requires a prior receipt"))?;
            receipt.validate()?;
            // A JSON file is deleted only when Compresso created it; the
            // owned-path check then proves nothing else is left in it.
            if request.config_format == ConfigFormat::Json
                && request.target_contents.is_empty()
                && receipt.backup_path != CREATED_FILE_BACKUP_SENTINEL
            {
                return Err(invalid(
                    "a JSON configuration is removed only when Compresso created it",
                ));
            }
            let requested = request
                .owned_paths
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            let owned = receipt
                .keys_owned
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if requested != owned
                || receipt.client != request.client
                || receipt.config_path != request.config_path
            {
                return Err(invalid(
                    "uninstall target and keys must exactly match the receipt",
                ));
            }
        }
    }
    Ok(())
}

fn validate_owned_semantics(
    request: &MutationRequest,
    changes: &[OwnedKeyMutation],
) -> Result<(), IntegrationError> {
    match request.operation {
        MutationKind::Install => {
            if changes
                .iter()
                .any(|change| change.after_value_hash.is_none())
            {
                return Err(invalid("install target is missing an owned key"));
            }
        }
        MutationKind::Uninstall => {
            let receipt = request
                .prior_receipt
                .as_ref()
                .expect("basic request validation requires an uninstall receipt");
            for change in changes {
                let installed = receipt
                    .owned_value_hashes
                    .get(&change.key_path)
                    .expect("receipt validation checked exact keys");
                let matches_installed = change.before_value_hash.as_ref() == Some(installed);
                if receipt.uninstalled_at.is_some()
                    && change.before_value_hash.is_none()
                    && change.after_value_hash.is_none()
                {
                    continue;
                }
                if matches_installed && change.after_value_hash.is_some() {
                    return Err(invalid(
                        "matching receipt-owned state must be removed by uninstall",
                    ));
                }
                if !matches_installed && change.after_value_hash != change.before_value_hash {
                    return Err(invalid(
                        "user-modified owned state must be retained byte-for-byte",
                    ));
                }
                if !matches_installed
                    && !request
                        .warnings
                        .iter()
                        .any(|warning| warning.contains("key_modified"))
                {
                    return Err(invalid(
                        "retaining a modified owned key requires a key_modified warning",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn derive_owned_mutations(
    operation: MutationKind,
    format: ConfigFormat,
    base: &TargetState,
    target: &str,
    owned_paths: &[String],
) -> Result<Vec<OwnedKeyMutation>, IntegrationError> {
    match format {
        ConfigFormat::Json => derive_json_owned_mutations(operation, base, target, owned_paths),
        ConfigFormat::Toml => derive_toml_owned_mutations(operation, base, target, owned_paths),
        ConfigFormat::Markdown => derive_whole_file_owned_mutation(base, target, owned_paths),
    }
}

/// The single owned path of a whole-file format covers every byte.
fn derive_whole_file_owned_mutation(
    base: &TargetState,
    target: &str,
    owned_paths: &[String],
) -> Result<Vec<OwnedKeyMutation>, IntegrationError> {
    let [key_path] = owned_paths else {
        return Err(invalid("a whole-file format owns exactly one path"));
    };
    Ok(vec![OwnedKeyMutation {
        key_path: key_path.clone(),
        before_value_hash: match base {
            TargetState::Present(bytes) => Some(hash_owned_value(bytes)),
            TargetState::Missing => None,
        },
        after_value_hash: (!target.is_empty()).then(|| hash_owned_value(target.as_bytes())),
    }])
}

fn derive_json_owned_mutations(
    operation: MutationKind,
    base: &TargetState,
    target: &str,
    owned_paths: &[String],
) -> Result<Vec<OwnedKeyMutation>, IntegrationError> {
    let mut base_document = parse_json_target(base, "base")?;
    let mut target_document = parse_json_text_target(target)?;
    if !target_document.is_object() {
        return Err(invalid("target JSON must be an object"));
    }
    let mut changes = Vec::with_capacity(owned_paths.len());
    for path in owned_paths {
        let segments = path.split('.').collect::<Vec<_>>();
        let before = json_value_at(&base_document, &segments)
            .map(canonical_json_hash)
            .transpose()?;
        let after = json_value_at(&target_document, &segments)
            .map(canonical_json_hash)
            .transpose()?;
        changes.push(OwnedKeyMutation {
            key_path: path.clone(),
            before_value_hash: before,
            after_value_hash: after,
        });
        remove_json_owned_path(
            &mut base_document,
            &segments,
            operation == MutationKind::Install,
        )?;
        remove_json_owned_path(
            &mut target_document,
            &segments,
            operation == MutationKind::Install,
        )?;
    }
    if base_document != target_document {
        return Err(invalid(
            "target JSON changes content outside the declared owned paths",
        ));
    }
    Ok(changes)
}

/// A JSON target document; the empty target of a file removal is `{}`.
fn parse_json_text_target(target: &str) -> Result<serde_json::Value, IntegrationError> {
    if target.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(strip_bom_str(target)).map_err(|error| IntegrationError::InvalidRequest {
        reason: format!("target JSON is malformed: {error}"),
    })
}

fn parse_json_target(
    target: &TargetState,
    label: &str,
) -> Result<serde_json::Value, IntegrationError> {
    let document = match target {
        TargetState::Missing => serde_json::json!({}),
        TargetState::Present(bytes) => {
            serde_json::from_slice(strip_bom(bytes)).map_err(|error| {
                IntegrationError::InvalidRequest {
                    reason: format!("{label} JSON is malformed: {error}"),
                }
            })?
        }
    };
    if document.is_object() {
        Ok(document)
    } else {
        Err(invalid("JSON configuration must be an object"))
    }
}

fn json_value_at<'a>(
    value: &'a serde_json::Value,
    segments: &[&str],
) -> Option<&'a serde_json::Value> {
    segments
        .iter()
        .try_fold(value, |current, segment| current.as_object()?.get(*segment))
}

fn canonical_json_hash(value: &serde_json::Value) -> Result<String, IntegrationError> {
    let bytes = serde_json::to_vec(value).map_err(|error| IntegrationError::InvalidRequest {
        reason: format!("owned JSON value cannot be canonicalized: {error}"),
    })?;
    Ok(hash_owned_value(&bytes))
}

fn remove_json_owned_path(
    value: &mut serde_json::Value,
    segments: &[&str],
    prune_empty_ancestors: bool,
) -> Result<bool, IntegrationError> {
    let Some((head, tail)) = segments.split_first() else {
        return Err(invalid("owned JSON path is empty"));
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(false);
    };
    if tail.is_empty() {
        object.remove(*head);
    } else if let Some(child) = object.get_mut(*head) {
        let child_empty = remove_json_owned_path(child, tail, prune_empty_ancestors)?;
        if child_empty && prune_empty_ancestors {
            object.remove(*head);
        }
    }
    Ok(object.is_empty())
}

fn derive_toml_owned_mutations(
    operation: MutationKind,
    base: &TargetState,
    target: &str,
    owned_paths: &[String],
) -> Result<Vec<OwnedKeyMutation>, IntegrationError> {
    let mut base_document = parse_toml_target(base, "base")?;
    let mut target_document = strip_bom_str(target).parse::<DocumentMut>().map_err(|_| {
        IntegrationError::InvalidRequest {
            reason: "target TOML is malformed".to_owned(),
        }
    })?;
    let mut changes = Vec::with_capacity(owned_paths.len());
    for path in owned_paths {
        let segments = path.split('.').collect::<Vec<_>>();
        let before = toml_item_at(base_document.as_item(), &segments).map(hash_toml_owned_item);
        let after = toml_item_at(target_document.as_item(), &segments).map(hash_toml_owned_item);
        changes.push(OwnedKeyMutation {
            key_path: path.clone(),
            before_value_hash: before,
            after_value_hash: after,
        });
        remove_toml_owned_path(
            base_document.as_item_mut(),
            &segments,
            operation == MutationKind::Install,
        )?;
        remove_toml_owned_path(
            target_document.as_item_mut(),
            &segments,
            operation == MutationKind::Install,
        )?;
    }
    if base_document.to_string() != target_document.to_string() {
        return Err(invalid(
            "target TOML changes bytes outside the declared owned paths",
        ));
    }
    Ok(changes)
}

fn parse_toml_target(target: &TargetState, label: &str) -> Result<DocumentMut, IntegrationError> {
    match target {
        TargetState::Missing => Ok(DocumentMut::new()),
        TargetState::Present(bytes) => std::str::from_utf8(strip_bom(bytes))
            .map_err(|_| invalid("TOML configuration must be UTF-8"))?
            .parse::<DocumentMut>()
            .map_err(|_| IntegrationError::InvalidRequest {
                reason: format!("{label} TOML is malformed"),
            }),
    }
}

fn toml_item_at<'a>(item: &'a Item, segments: &[&str]) -> Option<&'a Item> {
    segments
        .iter()
        .try_fold(item, |current, segment| current.get(*segment))
}

fn remove_toml_owned_path(
    item: &mut Item,
    segments: &[&str],
    prune_empty_ancestors: bool,
) -> Result<bool, IntegrationError> {
    let Some((head, tail)) = segments.split_first() else {
        return Err(invalid("owned TOML path is empty"));
    };
    let Some(table) = item.as_table_like_mut() else {
        return Ok(false);
    };
    if tail.is_empty() {
        table.remove(head);
    } else if let Some(child) = table.get_mut(head) {
        let child_empty = remove_toml_owned_path(child, tail, prune_empty_ancestors)?;
        if child_empty && prune_empty_ancestors {
            table.remove(head);
        }
    }
    Ok(table.is_empty())
}

fn invalid(reason: &str) -> IntegrationError {
    IntegrationError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

fn compute_plan_hash(plan: &MutationPlan) -> Result<String, IntegrationError> {
    let mut unsigned = plan.clone();
    unsigned.plan_hash.clear();
    let bytes = serde_json::to_vec(&ExactMutationPlanRef::from(&unsigned)).map_err(|error| {
        IntegrationError::InvalidRequest {
            reason: format!("plan serialization failed: {error}"),
        }
    })?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TargetState {
    Missing,
    Present(Vec<u8>),
}

fn read_target(path: &Path) -> Result<TargetState, IntegrationError> {
    match fs::read(path) {
        Ok(bytes) => Ok(TargetState::Present(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(TargetState::Missing),
        Err(source) => Err(IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn hash_target(state: &TargetState) -> String {
    let mut hasher = blake3::Hasher::new();
    match state {
        TargetState::Missing => hasher.update(b"missing\0"),
        TargetState::Present(bytes) => {
            hasher.update(b"present\0");
            hasher.update(bytes)
        }
    };
    hasher.finalize().to_hex().to_string()
}

fn make_unified_diff(
    path: &Path,
    format: ConfigFormat,
    base: &TargetState,
    target: &str,
    operation: MutationKind,
    owned: &[OwnedKeyMutation],
    backup_path: &str,
) -> Result<String, IntegrationError> {
    let label = path.display();
    let action = match operation {
        MutationKind::Install => "after compresso integration install",
        MutationKind::Uninstall => "after compresso integration uninstall",
    };
    let mut diff = format!(
        "--- {label} (current Compresso-owned values)\n+++ {label} ({action}; Compresso-owned values)\n"
    );
    if format == ConfigFormat::Markdown {
        append_whole_file_diff(&mut diff, base, target, owned);
    }
    for change in owned.iter().filter(|_| format != ConfigFormat::Markdown) {
        let (before, after) = safe_owned_projections(format, base, target, &change.key_path)?;
        diff.push_str(&format!("@@ {} @@\n", change.key_path));
        append_projection_diff(&mut diff, format, &before, &after, change);
    }
    let keys = owned
        .iter()
        .map(|change| change.key_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    diff.push_str("# Unrelated configuration omitted; sensitive values are never displayed.\n");
    diff.push_str(&format!(
        "# Compresso-owned keys: {keys}; backup: {backup_path}\n"
    ));
    Ok(diff)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SafeOwnedProjection {
    present: bool,
    server_type: Option<String>,
    command: Option<String>,
    args: Option<String>,
    redacted_fields: usize,
}

fn safe_owned_projections(
    format: ConfigFormat,
    base: &TargetState,
    target: &str,
    key_path: &str,
) -> Result<(SafeOwnedProjection, SafeOwnedProjection), IntegrationError> {
    let segments = key_path.split('.').collect::<Vec<_>>();
    match format {
        ConfigFormat::Markdown => Err(invalid(
            "whole-file formats are previewed by append_whole_file_diff",
        )),
        ConfigFormat::Json => {
            let base = parse_json_target(base, "base")?;
            let target = parse_json_text_target(target)?;
            Ok((
                safe_json_projection(json_value_at(&base, &segments)),
                safe_json_projection(json_value_at(&target, &segments)),
            ))
        }
        ConfigFormat::Toml => {
            let base = parse_toml_target(base, "base")?;
            let target = strip_bom_str(target).parse::<DocumentMut>().map_err(|_| {
                IntegrationError::InvalidRequest {
                    reason: "target TOML is malformed".to_owned(),
                }
            })?;
            Ok((
                safe_toml_projection(toml_item_at(base.as_item(), &segments)),
                safe_toml_projection(toml_item_at(target.as_item(), &segments)),
            ))
        }
    }
}

fn safe_json_projection(value: Option<&serde_json::Value>) -> SafeOwnedProjection {
    let Some(value) = value else {
        return SafeOwnedProjection::default();
    };
    let Some(object) = value.as_object() else {
        return SafeOwnedProjection {
            present: true,
            redacted_fields: 1,
            ..SafeOwnedProjection::default()
        };
    };
    let mut projection = SafeOwnedProjection {
        present: true,
        redacted_fields: object
            .keys()
            .filter(|key| !matches!(key.as_str(), "type" | "command" | "args"))
            .count(),
        ..SafeOwnedProjection::default()
    };
    projection.server_type = object
        .get("type")
        .map(|value| safe_type_value(value.as_str()));
    projection.command = object
        .get("command")
        .map(|value| safe_command_value(value.as_str()));
    projection.args = object
        .get("args")
        .map(|value| safe_json_args_value(value.as_array()));
    projection
}

fn safe_toml_projection(item: Option<&Item>) -> SafeOwnedProjection {
    let Some(item) = item else {
        return SafeOwnedProjection::default();
    };
    let Some(table) = item.as_table_like() else {
        return SafeOwnedProjection {
            present: true,
            redacted_fields: 1,
            ..SafeOwnedProjection::default()
        };
    };
    let mut projection = SafeOwnedProjection {
        present: true,
        redacted_fields: table
            .iter()
            .filter(|(key, _)| !matches!(*key, "type" | "command" | "args"))
            .count(),
        ..SafeOwnedProjection::default()
    };
    projection.server_type = table
        .get("type")
        .map(|value| safe_type_value(value.as_str()));
    projection.command = table
        .get("command")
        .map(|value| safe_command_value(value.as_str()));
    projection.args = table
        .get("args")
        .map(|value| safe_toml_args_value(value.as_array()));
    projection
}

fn safe_type_value(value: Option<&str>) -> String {
    value
        .filter(|value| *value == "stdio")
        .map(quoted_preview_value)
        .unwrap_or_else(redacted_preview_value)
}

fn safe_command_value(value: Option<&str>) -> String {
    value
        .filter(|value| {
            is_lexically_absolute_preview_path(value)
                && !value.contains("://")
                && !value
                    .strip_prefix("\\\\?\\")
                    .unwrap_or(value)
                    .contains(['?', '#'])
                && !value.chars().any(char::is_control)
        })
        .map(quoted_preview_value)
        .unwrap_or_else(redacted_preview_value)
}

fn safe_json_args_value(value: Option<&Vec<serde_json::Value>>) -> String {
    let Some(arguments) = value.and_then(|values| {
        values
            .iter()
            .map(serde_json::Value::as_str)
            .collect::<Option<Vec<_>>>()
    }) else {
        return redacted_preview_value();
    };
    safe_args_value(&arguments)
}

fn safe_toml_args_value(value: Option<&toml_edit::Array>) -> String {
    let Some(arguments) = value.and_then(|values| {
        values
            .iter()
            .map(toml_edit::Value::as_str)
            .collect::<Option<Vec<_>>>()
    }) else {
        return redacted_preview_value();
    };
    safe_args_value(&arguments)
}

fn safe_args_value(all_arguments: &[&str]) -> String {
    // An adapter-written recall suffix is a closed set, so it is safe to show.
    let arguments = match all_arguments {
        [base @ .., flag, source]
            if *flag == crate::server_entry::RECALL_SOURCE_FLAG
                && crate::server_entry::is_recall_source(source) =>
        {
            base
        }
        _ => all_arguments,
    };
    // So are the main-session profile flags and `--edits` (E17-51), in the
    // order an adapter writes them.
    let mut arguments = arguments;
    if let [base @ .., flag] = arguments
        && *flag == crate::server_entry::EDITS_FLAG
    {
        arguments = base;
    }
    let session = crate::server_entry::LEAN_SESSION_ARGS;
    if arguments.len() >= session.len() && arguments[arguments.len() - session.len()..] == session {
        arguments = &arguments[..arguments.len() - session.len()];
    }
    let safe = arguments == ["mcp", "serve"]
        || (arguments.len() == 4
            && arguments[..3] == ["mcp", "serve", "--root"]
            && is_lexically_absolute_preview_path(arguments[3])
            && !arguments[3].chars().any(char::is_control));
    if !safe {
        return redacted_preview_value();
    }
    format!(
        "[{}]",
        all_arguments
            .iter()
            .map(|value| quoted_preview_value(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn is_lexically_absolute_preview_path(value: &str) -> bool {
    if Path::new(value).is_absolute() || value.starts_with('/') || value.starts_with(r"\\") {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn quoted_preview_value(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| redacted_preview_value())
}

fn redacted_preview_value() -> String {
    "\"<redacted>\"".to_owned()
}

fn append_projection_diff(
    diff: &mut String,
    format: ConfigFormat,
    before: &SafeOwnedProjection,
    after: &SafeOwnedProjection,
    mutation: &OwnedKeyMutation,
) {
    let mut displayed = 0;
    for (name, before, after) in [
        (
            "type",
            before.server_type.as_deref(),
            after.server_type.as_deref(),
        ),
        (
            "command",
            before.command.as_deref(),
            after.command.as_deref(),
        ),
        ("args", before.args.as_deref(), after.args.as_deref()),
    ] {
        if before == after {
            continue;
        }
        if let Some(value) = before {
            append_preview_line(diff, '-', format, name, value);
            displayed += 1;
        }
        if let Some(value) = after {
            append_preview_line(diff, '+', format, name, value);
            displayed += 1;
        }
    }
    if before.redacted_fields != after.redacted_fields {
        if before.redacted_fields > 0 {
            diff.push_str(&format!(
                "- <{} additional field(s) redacted>\n",
                before.redacted_fields
            ));
            displayed += 1;
        }
        if after.redacted_fields > 0 {
            diff.push_str(&format!(
                "+ <{} additional field(s) redacted>\n",
                after.redacted_fields
            ));
            displayed += 1;
        }
    }
    if before.present != after.present && displayed == 0 {
        let prefix = if before.present { '-' } else { '+' };
        diff.push_str(&format!("{prefix} <Compresso-owned value>\n"));
        displayed += 1;
    }
    if displayed == 0 && mutation.before_value_hash != mutation.after_value_hash {
        diff.push_str("- <changed fields redacted>\n+ <changed fields redacted>\n");
        displayed += 2;
    }
    if displayed == 0 {
        diff.push_str("  <no Compresso-owned value changes>\n");
    }
}

/// Previews a whole owned file. Only Compresso-rendered target lines are
/// shown; current bytes, which a user may have edited, are summarized.
fn append_whole_file_diff(
    diff: &mut String,
    base: &TargetState,
    target: &str,
    owned: &[OwnedKeyMutation],
) {
    for change in owned {
        diff.push_str(&format!("@@ {} @@\n", change.key_path));
        if let TargetState::Present(bytes) = base {
            let lines = String::from_utf8_lossy(bytes).lines().count();
            diff.push_str(&format!("- <current file: {lines} line(s)>\n"));
        }
        match (&change.before_value_hash, &change.after_value_hash) {
            (before, Some(after)) if before.as_ref() == Some(after) => {
                diff.push_str("  <file retained unchanged>\n");
            }
            (_, Some(_)) => {
                for line in target.lines() {
                    diff.push_str(&format!("+ {line}\n"));
                }
            }
            (_, None) => diff.push_str("+ <file removed>\n"),
        }
    }
}

fn append_preview_line(
    diff: &mut String,
    prefix: char,
    format: ConfigFormat,
    name: &str,
    value: &str,
) {
    match format {
        ConfigFormat::Json => diff.push_str(&format!("{prefix} \"{name}\": {value}\n")),
        ConfigFormat::Toml => diff.push_str(&format!("{prefix} {name} = {value}\n")),
        ConfigFormat::Markdown => diff.push_str(&format!("{prefix} {name}: {value}\n")),
    }
}

fn planned_backup_path(path: &Path, now: i64) -> Result<PathBuf, IntegrationError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| invalid("config_path must identify a file"))?
        .to_string_lossy();
    // The documented timestamp has one-second precision. Back-to-back install
    // and uninstall plans may therefore collide; reserve the next free
    // timestamp rather than ever overwriting a prior backup.
    for offset in 0..=PLAN_TTL_SECONDS {
        let timestamp = DateTime::<Utc>::from_timestamp(now.saturating_add(offset), 0)
            .ok_or_else(|| invalid("plan timestamp is outside the supported range"))?
            .format("%Y-%m-%dT%H-%M-%SZ");
        let candidate = path.with_file_name(format!("{file_name}.compresso-backup-{timestamp}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(invalid(
        "no collision-free timestamped backup path is available",
    ))
}

fn journal_path(path: &Path) -> PathBuf {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{file_name}.compresso-journal"))
}

fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

fn mark_report_already_applied(mut report: ApplyReport) -> ApplyReport {
    report.already_applied = true;
    if let Some(restore) = &mut report.restore {
        restore.already_applied = true;
    }
    report
}

fn write_new_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8], tag: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut random = [0_u8; 12];
    getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let temp = path.with_file_name(format!(".{name}.compresso-tmp-{tag}-{suffix}"));
    let result = (|| {
        write_new_private(&temp, bytes)?;
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temp, metadata.permissions())?;
        }
        replace_file(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ignored = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    move_file_windows(source, destination, true)
}

#[cfg(windows)]
fn move_file_windows(
    source: &Path,
    destination: &Path,
    replace_existing: bool,
) -> std::io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both pointers reference live, NUL-terminated UTF-16 buffers for
    // the duration of the call. MoveFileExW does not retain either pointer.
    let mut flags = MOVEFILE_WRITE_THROUGH;
    if replace_existing {
        flags |= MOVEFILE_REPLACE_EXISTING;
    }
    let moved = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()
    }
    #[cfg(not(unix))]
    {
        // Windows replacement uses MOVEFILE_WRITE_THROUGH above, which is the
        // platform durability primitive for the rename itself.
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use jsonschema::Draft;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::{FileReceiptStore, MemoryReceiptStore, ReceiptError, ReceiptStore};

    struct FakeClock(AtomicI64);

    impl FakeClock {
        fn new(now: i64) -> Self {
            Self(AtomicI64::new(now))
        }

        fn advance(&self, seconds: i64) {
            self.0.fetch_add(seconds, Ordering::SeqCst);
        }
    }

    impl PlanClock for FakeClock {
        fn now_unix_seconds(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct SequenceClock(Mutex<Vec<i64>>);

    impl SequenceClock {
        fn new(values: Vec<i64>) -> Self {
            Self(Mutex::new(values.into_iter().rev().collect()))
        }
    }

    impl PlanClock for SequenceClock {
        fn now_unix_seconds(&self) -> i64 {
            self.0
                .lock()
                .expect("sequence clock lock")
                .pop()
                .expect("clock was read more than expected")
        }
    }

    struct ActiveLookupJournalInjector {
        inner: MemoryReceiptStore,
        pending_commit: Mutex<Option<PendingCommit>>,
    }

    impl ActiveLookupJournalInjector {
        fn new(inner: MemoryReceiptStore, pending_commit: PendingCommit) -> Self {
            Self {
                inner,
                pending_commit: Mutex::new(Some(pending_commit)),
            }
        }
    }

    impl ReceiptStore for ActiveLookupJournalInjector {
        fn by_plan_hash(&self, plan_hash: &str) -> Result<Option<InstallReceipt>, ReceiptError> {
            self.inner.by_plan_hash(plan_hash)
        }

        fn active_for(
            &self,
            client: &str,
            config_path: &Path,
        ) -> Result<Option<InstallReceipt>, ReceiptError> {
            if let Some(commit) = self
                .pending_commit
                .lock()
                .map_err(|_| ReceiptError::LockPoisoned)?
                .take()
            {
                write_journal(config_path, &commit, &self.inner)
                    .expect("authenticated foreign journal should be injected");
            }
            self.inner.active_for(client, config_path)
        }

        fn save(&self, receipt: &InstallReceipt) -> Result<(), ReceiptError> {
            self.inner.save(receipt)
        }

        fn seal_journal(&self, payload: &[u8]) -> Result<String, ReceiptError> {
            self.inner.seal_journal(payload)
        }

        fn verify_journal(&self, payload: &[u8], mac: &str) -> Result<(), ReceiptError> {
            self.inner.verify_journal(payload, mac)
        }
    }

    fn manager(clock: Arc<FakeClock>) -> PlanManager {
        PlanManager::with_clock("session-a", clock).expect("manager")
    }

    fn install_request(path: PathBuf, target: &str) -> MutationRequest {
        MutationRequest {
            operation: MutationKind::Install,
            client: "claude-code".to_owned(),
            config_path: path,
            config_format: ConfigFormat::Json,
            target_contents: target.to_owned(),
            owned_paths: vec!["mcpServers.compresso".to_owned()],
            compresso_version: "0.1.0".to_owned(),
            prior_receipt: None,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn test_plan_rejects_outside_diff_rewrite_as_config_drift() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, r#"{"unknown":{"keep":true}}"#).expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let plan = manager
            .issue(install_request(
                path.clone(),
                r#"{"unknown":{"keep":true},"mcpServers":{"compresso":{}}}"#,
            ))
            .expect("plan issued");
        fs::write(&path, r#"{"unknown":{"keep":false}}"#).expect("outside key changed");
        let error = manager
            .apply(&PlanApproval::from(&plan), &MemoryReceiptStore::default())
            .expect_err("whole-file drift rejected");
        assert!(matches!(error, IntegrationError::ConfigDrift { .. }));
        assert_eq!(
            fs::read_to_string(path).expect("config readable"),
            r#"{"unknown":{"keep":false}}"#
        );
    }

    #[test]
    fn test_forged_expired_and_session_mismatched_approvals_reject_without_write() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let plan = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");

        let mut forged = PlanApproval::from(&plan);
        forged.diff_hash = "f".repeat(64);
        assert!(matches!(
            manager.apply(&forged, &MemoryReceiptStore::default()),
            Err(IntegrationError::ForgedPlan)
        ));
        let mut wrong_session = PlanApproval::from(&plan);
        wrong_session.session_id = "session-b".to_owned();
        assert!(matches!(
            manager.apply(&wrong_session, &MemoryReceiptStore::default()),
            Err(IntegrationError::SessionMismatch)
        ));
        clock.advance(PLAN_TTL_SECONDS);
        assert!(matches!(
            manager.apply(&PlanApproval::from(&plan), &MemoryReceiptStore::default()),
            Err(IntegrationError::PlanExpired { .. })
        ));
        assert_eq!(fs::read_to_string(path).expect("config readable"), "{}\n");
    }

    #[test]
    fn test_apply_uses_one_captured_timestamp_at_ttl_boundary() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(SequenceClock::new(vec![100, 699]));
        let manager = PlanManager::with_clock("ttl-session", clock).expect("manager");
        let plan = manager
            .issue(install_request(
                path,
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");
        let receipt = manager
            .apply_install(&PlanApproval::from(&plan), &MemoryReceiptStore::default())
            .expect("apply just before expiry succeeds");
        assert_eq!(receipt.installed_at, 699);
        assert_eq!(plan.expires_at, 700);
    }

    #[test]
    fn test_deferred_install_renews_only_timing_and_hash_after_original_expiry() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let receipts = MemoryReceiptStore::default();
        let original = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");
        let approval = PlanApproval::from(&original);
        let authorization = manager
            .authorize_deferred_install(&approval)
            .expect("deferred install authorized");

        assert!(matches!(
            manager.authorize_deferred_install(&approval),
            Err(IntegrationError::ForgedPlan)
        ));
        assert!(matches!(
            manager.apply_install(&approval, &receipts),
            Err(IntegrationError::ForgedPlan)
        ));
        clock.advance(PLAN_TTL_SECONDS + 37);

        let outcome = manager
            .apply_deferred_install(authorization, &receipts)
            .expect("deferred install applies after original expiry");
        assert_eq!(
            outcome.plan.issued_at,
            original.issued_at + PLAN_TTL_SECONDS + 37
        );
        assert_eq!(
            outcome.plan.expires_at,
            outcome.plan.issued_at + PLAN_TTL_SECONDS
        );
        assert_ne!(outcome.plan.plan_hash, original.plan_hash);
        assert_eq!(outcome.plan.backup_path, original.backup_path);
        assert_eq!(outcome.plan.unified_diff, original.unified_diff);
        assert_eq!(outcome.plan.diff_hash, original.diff_hash);

        let mut normalized = outcome.plan.clone();
        normalized.issued_at = original.issued_at;
        normalized.expires_at = original.expires_at;
        normalized.plan_hash = original.plan_hash.clone();
        assert_eq!(
            normalized, original,
            "renewal may change only timing and hash"
        );
        assert!(!outcome.report.already_applied);
        let receipt = outcome.report.receipt.as_ref().expect("install receipt");
        assert_eq!(receipt.plan_hash, outcome.plan.plan_hash);
        assert_eq!(receipt.backup_path, original.backup_path);
        assert!(Path::new(&original.backup_path).is_file());
    }

    #[test]
    fn test_deferred_install_authorization_is_bound_to_exact_manager() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let issuing_manager = manager(clock.clone());
        let original = issuing_manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");
        let authorization = issuing_manager
            .authorize_deferred_install(&PlanApproval::from(&original))
            .expect("deferred install authorized");
        let same_named_session = manager(clock);

        let error = same_named_session
            .apply_deferred_install(authorization, &MemoryReceiptStore::default())
            .expect_err("another manager cannot consume the authorization");
        assert!(matches!(error, IntegrationError::SessionMismatch));
        assert_eq!(fs::read_to_string(path).expect("config readable"), "{}\n");
    }

    #[test]
    fn test_deferred_install_rejects_foreign_pending_journal_without_recovery() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let base = "{}\n";
        fs::write(&path, base).expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let approved = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"approved\"}}}\n",
            ))
            .expect("approved plan issued");
        let authorization = manager
            .authorize_deferred_install(&PlanApproval::from(&approved))
            .expect("deferred install authorized");

        let receipts = MemoryReceiptStore::default();
        let foreign_manager =
            PlanManager::with_clock("foreign-session", clock.clone()).expect("foreign manager");
        let foreign = foreign_manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"foreign\"}}}\n",
            ))
            .expect("foreign plan issued");
        let foreign_commit =
            build_commit(&foreign, clock.now_unix_seconds(), None).expect("foreign commit built");
        write_journal(&path, &foreign_commit, &receipts).expect("foreign journal injected");
        let journal = journal_path(&path);
        let journal_before = fs::read(&journal).expect("foreign journal readable");

        let error = manager
            .apply_deferred_install(authorization, &receipts)
            .expect_err("foreign pending recovery must fail closed");
        assert!(matches!(error, IntegrationError::PendingRecovery { .. }));
        assert_eq!(fs::read_to_string(&path).expect("config readable"), base);
        assert_eq!(
            fs::read(&journal).expect("foreign journal remains readable"),
            journal_before
        );
        assert!(
            receipts
                .by_plan_hash(&foreign.plan_hash)
                .expect("foreign receipt lookup")
                .is_none()
        );
        assert!(!Path::new(&foreign.backup_path).exists());
        assert!(!Path::new(&approved.backup_path).exists());
    }

    #[test]
    fn test_strict_deferred_install_rechecks_journal_after_active_receipt_lookup() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let base = "{}\n";
        let target = "{\"mcpServers\":{\"compresso\":{\"command\":\"installed\"}}}\n";
        fs::write(&path, base).expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let receipts = MemoryReceiptStore::default();
        let first_manager = manager(clock.clone());
        let installed = first_manager
            .issue(install_request(path.clone(), target))
            .expect("initial plan issued");
        let installed_receipt = first_manager
            .apply_install(&PlanApproval::from(&installed), &receipts)
            .expect("initial install applies");
        let installed_bytes = fs::read(&path).expect("installed config readable");
        let backups_before = backup_files(directory.path());

        let setup_manager =
            PlanManager::with_clock("setup-session", clock.clone()).expect("setup manager");
        let approved = setup_manager
            .issue(install_request(path.clone(), target))
            .expect("idempotent setup plan issued");
        let authorization = setup_manager
            .authorize_deferred_install(&PlanApproval::from(&approved))
            .expect("idempotent setup plan authorized");

        let foreign_manager = PlanManager::with_clock("foreign-active-lookup", clock.clone())
            .expect("foreign manager");
        let foreign = foreign_manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"foreign\"}}}\n",
            ))
            .expect("foreign plan issued");
        let foreign_commit =
            build_commit(&foreign, clock.now_unix_seconds(), None).expect("foreign commit built");
        let receipts = ActiveLookupJournalInjector::new(receipts, foreign_commit);

        let error = setup_manager
            .apply_deferred_install(authorization, &receipts)
            .expect_err("journal injected during active lookup must fail closed");

        assert!(matches!(error, IntegrationError::PendingRecovery { .. }));
        assert_eq!(
            fs::read(&path).expect("installed config remains readable"),
            installed_bytes
        );
        assert!(journal_path(&path).exists());
        assert_eq!(backup_files(directory.path()), backups_before);
        assert_eq!(
            receipts
                .inner
                .active_for("claude-code", &path)
                .expect("active receipt lookup"),
            Some(installed_receipt)
        );
    }

    #[test]
    fn test_strict_apply_rechecks_journal_after_uninstall_active_receipt_lookup() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let receipts = MemoryReceiptStore::default();
        let installed = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"installed\"}}}\n",
            ))
            .expect("install plan issued");
        let installed_receipt = manager
            .apply_install(&PlanApproval::from(&installed), &receipts)
            .expect("install applies");

        let user_modified = "{\"mcpServers\":{\"compresso\":{\"command\":\"user-modified\"}}}\n";
        fs::write(&path, user_modified).expect("user-modified config written");
        let uninstall = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: user_modified.to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(installed_receipt.clone()),
                warnings: vec!["key_modified:mcpServers.compresso".to_owned()],
            })
            .expect("retained-key uninstall plan issued");
        let backups_before = backup_files(directory.path());

        clock.advance(1);
        let foreign_manager = PlanManager::with_clock("foreign-uninstall-lookup", clock.clone())
            .expect("foreign manager");
        let foreign = foreign_manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"foreign\"}}}\n",
            ))
            .expect("foreign plan issued");
        let foreign_commit =
            build_commit(&foreign, clock.now_unix_seconds(), None).expect("foreign commit built");
        let receipts = ActiveLookupJournalInjector::new(receipts, foreign_commit);

        let _apply_guard = manager.apply_lock.lock().expect("apply lock");
        let error = manager
            .apply_for_kind_locked(
                &PlanApproval::from(&uninstall),
                &receipts,
                Some(MutationKind::Uninstall),
                PendingJournalPolicy::Reject,
                clock.now_unix_seconds(),
            )
            .expect_err("journal injected during uninstall lookup must fail closed");

        assert!(matches!(error, IntegrationError::PendingRecovery { .. }));
        assert_eq!(
            fs::read_to_string(&path).expect("config remains readable"),
            user_modified
        );
        assert!(journal_path(&path).exists());
        assert_eq!(backup_files(directory.path()), backups_before);
        assert_eq!(
            receipts
                .inner
                .active_for("claude-code", &path)
                .expect("active receipt lookup"),
            Some(installed_receipt)
        );
    }

    #[test]
    fn test_strict_apply_rejects_pending_journal_before_completed_plan_shortcut() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let receipts = MemoryReceiptStore::default();
        let installed = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"installed\"}}}\n",
            ))
            .expect("install plan issued");
        manager
            .apply_install(&PlanApproval::from(&installed), &receipts)
            .expect("install applies");
        let installed_bytes = fs::read(&path).expect("installed config readable");

        let foreign_manager =
            PlanManager::with_clock("foreign-completed", clock.clone()).expect("foreign manager");
        let foreign = foreign_manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"foreign\"}}}\n",
            ))
            .expect("foreign plan issued");
        let foreign_commit =
            build_commit(&foreign, clock.now_unix_seconds(), None).expect("foreign commit built");
        write_journal(&path, &foreign_commit, &receipts).expect("foreign journal injected");
        let journal = journal_path(&path);
        let journal_before = fs::read(&journal).expect("foreign journal readable");

        let _apply_guard = manager.apply_lock.lock().expect("apply lock");
        let error = manager
            .apply_for_kind_locked(
                &PlanApproval::from(&installed),
                &receipts,
                Some(MutationKind::Install),
                PendingJournalPolicy::Reject,
                clock.now_unix_seconds(),
            )
            .expect_err("strict apply must reject before returning completed state");
        assert!(matches!(error, IntegrationError::PendingRecovery { .. }));
        assert_eq!(
            fs::read(&path).expect("config remains readable"),
            installed_bytes
        );
        assert_eq!(
            fs::read(&journal).expect("foreign journal remains readable"),
            journal_before
        );
        assert!(
            receipts
                .by_plan_hash(&foreign.plan_hash)
                .expect("foreign receipt lookup")
                .is_none()
        );
    }

    #[test]
    fn test_issue_rejects_target_rewrite_outside_owned_json_path() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{\"unknown\":{\"keep\":true}}\n").expect("base written");
        let manager = PlanManager::new("owned-delta").expect("manager");
        let error = manager
            .issue(install_request(
                path,
                "{\"unknown\":{\"keep\":false},\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect_err("unknown sibling rewrite rejected");
        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
    }

    #[test]
    fn test_uninstall_plan_cannot_delete_unowned_parent_namespace() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let manager = PlanManager::new("owned-parent").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let install = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("install plan");
        let receipt = manager
            .apply_install(&PlanApproval::from(&install), &receipts)
            .expect("install applies");
        let error = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path,
                config_format: ConfigFormat::Json,
                target_contents: "{}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(receipt.clone()),
                warnings: Vec::new(),
            })
            .expect_err("unowned parent deletion rejected");
        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
    }

    #[test]
    fn test_toml_owned_projection_preserves_unowned_comments_and_values() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.toml");
        let base = "# user comment\nmodel = \"gpt-5\"\n";
        fs::write(&path, base).expect("base written");
        let manager = PlanManager::new("toml-owned").expect("manager");
        let valid = format!(
            "{base}\n[mcp_servers.compresso]\ncommand = \"/bin/compresso\"\nargs = [\"mcp\", \"serve\"]\n"
        );
        manager
            .issue(MutationRequest {
                operation: MutationKind::Install,
                client: "codex".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Toml,
                target_contents: valid,
                owned_paths: vec!["mcp_servers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: None,
                warnings: Vec::new(),
            })
            .expect("owned TOML table addition accepted");

        let rewritten = "# rewritten\nmodel = \"gpt-4\"\n\n[mcp_servers.compresso]\ncommand = \"/bin/compresso\"\n";
        let error = manager
            .issue(MutationRequest {
                operation: MutationKind::Install,
                client: "codex".to_owned(),
                config_path: path,
                config_format: ConfigFormat::Toml,
                target_contents: rewritten.to_owned(),
                owned_paths: vec!["mcp_servers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: None,
                warnings: Vec::new(),
            })
            .expect_err("unowned TOML rewrite rejected");
        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
    }

    #[test]
    fn test_toml_owned_hash_includes_standard_table_header_decor() {
        let plain = "[mcp_servers.compresso]\ncommand = \"/bin/compresso\"\n";
        let commented = "[mcp_servers.compresso] # user note\ncommand = \"/bin/compresso\"\n";
        let plain = plain.parse::<DocumentMut>().expect("plain TOML");
        let commented = commented.parse::<DocumentMut>().expect("commented TOML");
        let path = ["mcp_servers", "compresso"];

        assert_ne!(
            hash_toml_owned_item(toml_item_at(plain.as_item(), &path).expect("plain owned table")),
            hash_toml_owned_item(
                toml_item_at(commented.as_item(), &path).expect("commented owned table")
            )
        );
    }

    #[test]
    fn test_key_level_diff_omits_unrelated_values_and_redacts_unknown_owned_fields() {
        const CANARY: &str = "PLAN_PREVIEW_SECRET_CANARY_aa179e42";

        let path = Path::new("/tmp/config.toml");
        let base = format!(
            "[mcp_servers.cubic]\nhttp_headers = {{ Authorization = \"Bearer {CANARY}\" }}\n\n[mcp_servers.compresso]\ncommand = \"/old/compresso\"\nargs = [\"mcp\", \"serve\"]\nhttp_headers = {{ Authorization = \"Bearer {CANARY}\" }}\n"
        );
        let target = format!(
            "[mcp_servers.cubic]\nhttp_headers = {{ Authorization = \"Bearer {CANARY}\" }}\n\n[mcp_servers.compresso]\ncommand = \"/new/compresso\"\nargs = [\"mcp\", \"serve\"]\n"
        );
        let base_state = TargetState::Present(base.into_bytes());
        let owned_paths = vec!["mcp_servers.compresso".to_owned()];
        let owned = derive_owned_mutations(
            MutationKind::Install,
            ConfigFormat::Toml,
            &base_state,
            &target,
            &owned_paths,
        )
        .expect("owned mutation should derive");

        let diff = make_unified_diff(
            path,
            ConfigFormat::Toml,
            &base_state,
            &target,
            MutationKind::Install,
            &owned,
            CREATED_FILE_BACKUP_SENTINEL,
        )
        .expect("safe key-level diff should render");

        assert!(diff.contains("- command = \"/old/compresso\""));
        assert!(diff.contains("+ command = \"/new/compresso\""));
        assert!(!diff.contains(CANARY));
        assert!(!diff.contains("Authorization"));
        assert!(!diff.contains("mcp_servers.cubic"));
        assert!(diff.contains("<1 additional field(s) redacted>"));
        assert!(
            diff.lines().count() <= 10,
            "preview was not minimal: {diff}"
        );
    }

    #[test]
    fn test_secret_safe_preview_recognizes_absolute_paths_cross_platform() {
        for absolute in [
            "/opt/compresso",
            r"C:\Program Files\Compresso\compresso.exe",
            r"\\server\share\compresso.exe",
            r"\\?\C:\tools\compresso.exe",
        ] {
            assert_ne!(
                safe_command_value(Some(absolute)),
                redacted_preview_value(),
                "absolute path should be previewable: {absolute}"
            );
        }
        for unsafe_value in [
            "relative/compresso",
            r"..\compresso.exe",
            "https://example.invalid/compresso",
            "/opt/compresso?token=secret",
        ] {
            assert_eq!(
                safe_command_value(Some(unsafe_value)),
                redacted_preview_value(),
                "unsafe command should be redacted: {unsafe_value}"
            );
        }
    }

    #[test]
    fn test_public_preview_and_debug_never_expose_exact_target_contents() {
        const CANARY: &str = "MUTATION_TARGET_SECRET_CANARY_47c1be20";

        let directory = tempdir().expect("temp directory");
        let manager = manager(Arc::new(FakeClock::new(1_700_000_000)));
        let target = format!(
            "{{\"mcpServers\":{{\"compresso\":{{\"type\":\"stdio\",\"command\":\"/bin/compresso\",\"args\":[\"mcp\",\"serve\"],\"headers\":{{\"Authorization\":\"Bearer {CANARY}\"}}}}}}}}\n"
        );
        let plan = manager
            .issue(install_request(
                directory.path().join("config.json"),
                &target,
            ))
            .expect("plan should issue");

        assert!(plan.target_contents.contains(CANARY));
        let directly_serialized =
            serde_json::to_value(&plan).expect("public plan serialization should be safe");
        let directly_serialized_text = serde_json::to_string(&directly_serialized)
            .expect("public plan serialization should encode");
        assert_eq!(
            directly_serialized["document_type"],
            "mutation_plan_preview"
        );
        assert!(directly_serialized.get("target_contents").is_none());
        assert!(!directly_serialized_text.contains(CANARY));
        assert!(!directly_serialized_text.contains("Authorization"));
        let preview =
            serde_json::to_value(plan.public_preview()).expect("preview should serialize");
        let serialized = serde_json::to_string(&preview).expect("preview JSON should encode");
        assert!(preview.get("target_contents").is_none());
        assert_eq!(preview["owned_keys"], json!(["mcpServers.compresso"]));
        assert!(!serialized.contains(CANARY));
        assert!(!serialized.contains("Authorization"));
        let debug = format!("{plan:?}");
        assert!(!debug.contains(CANARY));
        assert!(!debug.contains("Authorization"));
        assert!(debug.contains("<redacted:"));
    }

    #[test]
    fn test_crash_recovery_finishes_config_and_atomic_receipt() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let plan = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");
        let receipts = MemoryReceiptStore::default();
        let commit = build_commit(&plan, clock.now_unix_seconds(), None).expect("commit built");
        write_journal(&path, &commit, &receipts).expect("durable journal written");
        atomic_replace(&path, plan.target_contents.as_bytes(), "crash-test")
            .expect("simulated pre-crash config replacement");

        let fresh = PlanManager::with_clock("new-process", clock).expect("fresh manager");
        let report = fresh
            .recover(&path, &receipts)
            .expect("recovery succeeds")
            .expect("journal recovered");
        assert_eq!(
            report.receipt.as_ref().expect("receipt").plan_hash,
            plan.plan_hash
        );
        assert!(
            receipts
                .by_plan_hash(&plan.plan_hash)
                .expect("receipt lookup")
                .is_some()
        );
        assert!(!journal_path(&path).exists());
        assert!(Path::new(&plan.backup_path).exists());
    }

    #[test]
    fn test_uninstall_recovery_accepts_exact_committed_receipt_and_clears_journal() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let receipts = MemoryReceiptStore::default();
        let install = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"/one\"}}}\n",
            ))
            .expect("install plan");
        let receipt = manager
            .apply_install(&PlanApproval::from(&install), &receipts)
            .expect("install applies");
        let uninstall = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: "{\"mcpServers\":{}}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(receipt.clone()),
                warnings: Vec::new(),
            })
            .expect("uninstall plan");
        let current = read_target(&path).expect("installed config readable");
        let canonical = authorize_uninstall_receipt(&uninstall, &current, &receipts)
            .expect("active receipt authorizes uninstall");
        let commit = build_commit(&uninstall, clock.now_unix_seconds(), Some(&canonical))
            .expect("uninstall commit built");
        write_journal(&path, &commit, &receipts).expect("journal written");
        ensure_backup(&commit).expect("backup committed");
        atomic_replace(
            &path,
            uninstall.target_contents.as_bytes(),
            "uninstall-crash",
        )
        .expect("config committed");
        receipts
            .save(&commit.persisted_receipt)
            .expect("receipt committed");
        assert!(
            receipts
                .active_for("claude-code", &path)
                .expect("active lookup")
                .is_none()
        );
        assert!(journal_path(&path).exists());

        let fresh = PlanManager::with_clock("uninstall-recovery", clock).expect("fresh manager");
        let report = fresh
            .recover(&path, &receipts)
            .expect("recovery succeeds")
            .expect("journal recovered");

        assert_eq!(
            report.restore.expect("restore report").removed_keys,
            ["mcpServers.compresso"]
        );
        assert_eq!(
            receipts
                .by_plan_hash(&receipt.plan_hash)
                .expect("receipt lookup"),
            Some(commit.persisted_receipt)
        );
        assert!(!journal_path(&path).exists());
    }

    fn whole_file_request(
        path: PathBuf,
        operation: MutationKind,
        target: &str,
        prior_receipt: Option<InstallReceipt>,
    ) -> MutationRequest {
        MutationRequest {
            operation,
            client: "claude-code".to_owned(),
            config_path: path,
            config_format: ConfigFormat::Markdown,
            target_contents: target.to_owned(),
            owned_paths: vec!["agent_definition".to_owned()],
            compresso_version: "0.1.0".to_owned(),
            prior_receipt,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn test_whole_file_uninstall_recovers_after_deletion_before_or_after_receipt_save() {
        for receipt_saved_before_crash in [false, true] {
            let directory = tempdir().expect("temp directory");
            let path = directory.path().join("agents").join("agent.md");
            let clock = Arc::new(FakeClock::new(1_700_000_000));
            let manager = manager(clock.clone());
            let receipts = MemoryReceiptStore::default();
            let install = manager
                .issue(whole_file_request(
                    path.clone(),
                    MutationKind::Install,
                    "---\nname: agent\n---\nbody\n",
                    None,
                ))
                .expect("install plan");
            assert_eq!(install.backup_path, CREATED_FILE_BACKUP_SENTINEL);
            let receipt = manager
                .apply_install(&PlanApproval::from(&install), &receipts)
                .expect("install applies and creates the directory");
            let uninstall = manager
                .issue(whole_file_request(
                    path.clone(),
                    MutationKind::Uninstall,
                    "",
                    Some(receipt.clone()),
                ))
                .expect("uninstall plan");
            assert_eq!(uninstall.result_hash, hash_target(&TargetState::Missing));
            assert_eq!(
                uninstall.backup_path,
                UNMODIFIED_FILE_REMOVED_BACKUP_SENTINEL
            );
            let current = read_target(&path).expect("installed file readable");
            let canonical = authorize_uninstall_receipt(&uninstall, &current, &receipts)
                .expect("active receipt authorizes uninstall");
            let commit = build_commit(&uninstall, clock.now_unix_seconds(), Some(&canonical))
                .expect("uninstall commit built");
            write_journal(&path, &commit, &receipts).expect("journal written");
            fs::remove_file(&path).expect("simulated deletion before the crash");
            if receipt_saved_before_crash {
                receipts
                    .save(&commit.persisted_receipt)
                    .expect("receipt committed");
            }

            let fresh =
                PlanManager::with_clock("whole-file-recovery", clock).expect("fresh manager");
            let report = fresh
                .recover(&path, &receipts)
                .expect("recovery succeeds")
                .expect("journal recovered");
            assert_eq!(
                report.restore.expect("restore report").removed_keys,
                ["agent_definition"]
            );
            assert!(!path.exists(), "recovery never recreates the deleted file");
            assert!(!journal_path(&path).exists());
            assert!(
                receipts
                    .active_for("claude-code", &path)
                    .expect("active lookup")
                    .is_none()
            );
            assert_eq!(
                fs::read_dir(directory.path().join("agents"))
                    .expect("directory")
                    .count(),
                0,
                "no backup is kept for an unmodified Compresso file"
            );
        }
    }

    #[test]
    fn test_whole_file_format_owns_exactly_one_path() {
        let directory = tempdir().expect("temp directory");
        let manager = manager(Arc::new(FakeClock::new(1_700_000_000)));
        let mut request = whole_file_request(
            directory.path().join("agent.md"),
            MutationKind::Install,
            "body\n",
            None,
        );
        request.owned_paths.push("second".to_owned());
        assert!(matches!(
            manager.issue(request),
            Err(IntegrationError::InvalidRequest { .. })
        ));
        let empty = whole_file_request(
            directory.path().join("agent.md"),
            MutationKind::Install,
            "",
            None,
        );
        assert!(
            manager.issue(empty).is_err(),
            "an install must write the owned file"
        );
    }

    #[test]
    fn test_file_store_restart_authenticates_and_recovers_committed_target() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let state = directory.path().join("state/receipts.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let plan = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");
        let first_store = FileReceiptStore::new(&state);
        let commit = build_commit(&plan, clock.now_unix_seconds(), None).expect("commit built");
        write_journal(&path, &commit, &first_store).expect("authenticated journal written");
        atomic_replace(&path, plan.target_contents.as_bytes(), "restart-test")
            .expect("simulated config commit");
        drop(first_store);

        let restarted_store = FileReceiptStore::new(&state);
        let restarted_manager =
            PlanManager::with_clock("restarted", clock).expect("restarted manager");
        let report = restarted_manager
            .recover(&path, &restarted_store)
            .expect("authenticated recovery succeeds")
            .expect("journal recovered");
        assert_eq!(report.receipt.expect("receipt").plan_hash, plan.plan_hash);
        assert!(
            restarted_store
                .by_plan_hash(&plan.plan_hash)
                .expect("receipt lookup")
                .is_some()
        );
    }

    #[test]
    fn test_missing_or_permissive_approval_key_fails_closed_before_config_write() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let state = directory.path().join("state/receipts.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let plan = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");
        let store = FileReceiptStore::new(&state);
        let commit = build_commit(&plan, clock.now_unix_seconds(), None).expect("commit built");
        write_journal(&path, &commit, &store).expect("authenticated journal written");
        let key_path = state.with_file_name("receipts.json.approval-key");
        fs::remove_file(&key_path).expect("test key removed");

        let restarted = PlanManager::with_clock("restarted", clock).expect("restarted manager");
        let error = restarted
            .recover(&path, &FileReceiptStore::new(&state))
            .expect_err("missing key must fail closed");
        assert!(matches!(
            error,
            IntegrationError::Receipt(ReceiptError::ApprovalKey { .. })
        ));
        assert_eq!(fs::read_to_string(&path).expect("config readable"), "{}\n");
        assert!(journal_path(&path).exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::write(&key_path, [7_u8; 32]).expect("replacement key written");
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644))
                .expect("permissive mode set");
            let error = restarted
                .recover(&path, &FileReceiptStore::new(&state))
                .expect_err("permissive key must fail closed");
            assert!(matches!(
                error,
                IntegrationError::Receipt(ReceiptError::ApprovalKey { .. })
            ));
            assert_eq!(fs::read_to_string(&path).expect("config readable"), "{}\n");

            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
                .expect("private mode restored");
            let error = restarted
                .recover(&path, &FileReceiptStore::new(&state))
                .expect_err("wrong private key must fail MAC");
            assert!(matches!(
                error,
                IntegrationError::Receipt(ReceiptError::InvalidJournalMac)
            ));
            assert_eq!(fs::read_to_string(&path).expect("config readable"), "{}\n");
        }
    }

    #[test]
    fn test_forged_adjacent_journal_fails_mac_before_any_recovery_side_effect() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let state = directory.path().join("state/receipts.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let plan = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");
        let store = FileReceiptStore::new(&state);
        let commit = build_commit(&plan, clock.now_unix_seconds(), None).expect("commit built");
        write_journal(&path, &commit, &store).expect("journal written");
        let journal = journal_path(&path);
        let mut forged: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal).expect("journal readable"))
                .expect("journal JSON");
        forged["commit"]["plan"]["target_contents"] = json!("{\"attacker\":true}\n");
        fs::write(
            &journal,
            serde_json::to_vec(&forged).expect("forged journal serializes"),
        )
        .expect("forged journal written");

        let restarted = PlanManager::with_clock("restarted", clock).expect("restarted manager");
        let error = restarted
            .recover(&path, &FileReceiptStore::new(&state))
            .expect_err("forged journal rejected");
        assert!(matches!(
            error,
            IntegrationError::Receipt(ReceiptError::InvalidJournalMac)
        ));
        assert_eq!(fs::read_to_string(&path).expect("config readable"), "{}\n");
        assert!(!Path::new(&plan.backup_path).exists());
    }

    #[test]
    fn test_journal_reservation_never_overwrites_another_manager() {
        let directory = tempdir().expect("temp directory");
        let journal = directory.path().join("config.json.compresso-journal");
        reserve_journal(&journal, b"first").expect("first reservation succeeds");
        let error = reserve_journal(&journal, b"second").expect_err("second reservation fails");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(journal).expect("journal readable"), b"first");
    }

    #[test]
    fn test_install_apply_is_idempotent_and_preserves_unknown_keys() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, r#"{"unknown":{"nested":[1,2]}}"#).expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let target = r#"{"unknown":{"nested":[1,2]},"mcpServers":{"compresso":{}}}"#;
        let plan = manager
            .issue(install_request(path.clone(), target))
            .expect("plan issued");
        let receipts = MemoryReceiptStore::default();
        let first = manager
            .apply(&PlanApproval::from(&plan), &receipts)
            .expect("first apply");
        let second = manager
            .apply(&PlanApproval::from(&plan), &receipts)
            .expect("second apply");
        assert!(!first.already_applied);
        assert!(second.already_applied);
        assert_eq!(receipts.all().expect("receipt snapshot").len(), 1);
        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).expect("config readable"))
                .expect("valid json");
        assert_eq!(parsed["unknown"], json!({"nested": [1, 2]}));
    }

    #[test]
    fn test_fresh_plan_for_exact_installed_state_reuses_active_receipt_without_writes() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let target = "{\"mcpServers\":{\"compresso\":{}}}\n";
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let first_manager = manager(clock.clone());
        let first_plan = first_manager
            .issue(install_request(path.clone(), target))
            .expect("first plan issued");
        let receipts = MemoryReceiptStore::default();
        let first_receipt = first_manager
            .apply(&PlanApproval::from(&first_plan), &receipts)
            .expect("first apply")
            .receipt
            .expect("first receipt");
        let bytes_after_first = fs::read(&path).expect("installed config readable");
        let backups_after_first = backup_files(directory.path());

        let fresh_manager =
            PlanManager::with_clock("session-b", clock).expect("fresh plan manager");
        let fresh_plan = fresh_manager
            .issue(install_request(path.clone(), target))
            .expect("fresh plan issued");
        assert_eq!(fresh_plan.base_hash, fresh_plan.result_hash);
        let report = fresh_manager
            .apply(&PlanApproval::from(&fresh_plan), &receipts)
            .expect("fresh plan is idempotent");

        assert!(report.already_applied);
        assert_eq!(report.receipt, Some(first_receipt));
        assert_eq!(fs::read(&path).expect("config readable"), bytes_after_first);
        assert_eq!(backup_files(directory.path()), backups_after_first);
        assert!(!journal_path(&path).exists());
        assert_eq!(receipts.all().expect("receipt snapshot").len(), 1);
    }

    #[test]
    fn test_same_second_changed_value_reinstall_atomically_supersedes_active_receipt() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let state = directory.path().join("state/receipts.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let first_manager = manager(clock.clone());
        let first_plan = first_manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"/one\"}}}\n",
            ))
            .expect("first plan");
        let store = FileReceiptStore::new(&state);
        let first = first_manager
            .apply_install(&PlanApproval::from(&first_plan), &store)
            .expect("first install");

        let second_manager = manager(clock.clone());
        let second_plan = second_manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"/two\"}}}\n",
            ))
            .expect("changed-value plan");
        let second = second_manager
            .apply_install(&PlanApproval::from(&second_plan), &store)
            .expect("changed-value reinstall");
        assert_eq!(first.installed_at, second.installed_at);
        assert_ne!(first.plan_hash, second.plan_hash);
        assert_eq!(
            FileReceiptStore::new(&state)
                .active_for("claude-code", &path)
                .expect("active lookup")
                .expect("active receipt")
                .plan_hash,
            second.plan_hash
        );

        let backups = backup_files(directory.path());
        let repeat_manager = manager(clock);
        let repeat_plan = repeat_manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"command\":\"/two\"}}}\n",
            ))
            .expect("repeat plan");
        let repeated = repeat_manager
            .apply(
                &PlanApproval::from(&repeat_plan),
                &FileReceiptStore::new(&state),
            )
            .expect("repeat install");
        assert!(repeated.already_applied);
        assert_eq!(
            repeated.receipt.expect("active receipt").plan_hash,
            second.plan_hash
        );
        assert_eq!(backup_files(directory.path()), backups);
        assert_eq!(
            FileReceiptStore::new(state)
                .load_unlocked()
                .expect("canonical receipts")
                .len(),
            2
        );
    }

    #[test]
    fn test_tampered_canonical_receipt_is_rejected_before_config_mutation() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let state = directory.path().join("state/receipts.json");
        let target = "{\"mcpServers\":{\"compresso\":{\"command\":\"/one\"}}}\n";
        fs::write(&path, "{}\n").expect("base written");
        let manager = PlanManager::new("receipt-tamper-install").expect("manager");
        let store = FileReceiptStore::new(&state);
        let plan = manager
            .issue(install_request(path.clone(), target))
            .expect("install plan");
        let receipt = manager
            .apply_install(&PlanApproval::from(&plan), &store)
            .expect("install applies");
        let config_before = fs::read(&path).expect("installed config readable");
        let backups_before = backup_files(directory.path());

        let record_path = state
            .with_file_name("receipts.json.d")
            .join(format!("{}.json", receipt.plan_hash));
        let mut envelope: serde_json::Value =
            serde_json::from_slice(&fs::read(&record_path).expect("receipt record readable"))
                .expect("receipt envelope JSON");
        envelope["receipt"]["owned_value_hashes"]["mcpServers.compresso"] = json!("f".repeat(64));
        fs::write(
            &record_path,
            serde_json::to_vec_pretty(&envelope).expect("tampered envelope serializes"),
        )
        .expect("tampered receipt record written");

        let fresh = PlanManager::new("receipt-tamper-repeat").expect("fresh manager");
        let repeat = fresh
            .issue(install_request(path.clone(), target))
            .expect("repeat plan");
        let error = fresh
            .apply(&PlanApproval::from(&repeat), &FileReceiptStore::new(&state))
            .expect_err("tampered canonical receipt must fail authentication");
        assert!(matches!(
            error,
            IntegrationError::Receipt(ReceiptError::InvalidReceiptMac)
        ));
        assert_eq!(fs::read(&path).expect("config readable"), config_before);
        assert_eq!(backup_files(directory.path()), backups_before);
        assert!(!journal_path(&path).exists());
    }

    #[test]
    fn test_forged_owned_hashes_cannot_authorize_uninstall() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let installed = "{\"mcpServers\":{\"compresso\":{\"command\":\"/one\"}}}\n";
        let user_modified = "{\"mcpServers\":{\"compresso\":{\"command\":\"/user\"}}}\n";
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let receipts = MemoryReceiptStore::default();
        let install = manager
            .issue(install_request(path.clone(), installed))
            .expect("install plan");
        let receipt = manager
            .apply_install(&PlanApproval::from(&install), &receipts)
            .expect("install applies");

        fs::write(&path, user_modified).expect("user edit written");
        let mut forged = receipt.clone();
        forged.owned_value_hashes.insert(
            "mcpServers.compresso".to_owned(),
            canonical_json_hash(&json!({"command": "/user"})).expect("canonical hash"),
        );
        let uninstall = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: "{\"mcpServers\":{}}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(forged),
                warnings: Vec::new(),
            })
            .expect("forged receipt is structurally valid");
        let backups_before = backup_files(directory.path());

        let error = manager
            .apply_uninstall(&PlanApproval::from(&uninstall), &receipts)
            .expect_err("caller-owned hashes have no uninstall authority");
        assert!(matches!(
            error,
            IntegrationError::UninstallReceiptMismatch { .. }
        ));
        assert_eq!(
            fs::read_to_string(&path).expect("config readable"),
            user_modified
        );
        assert_eq!(backup_files(directory.path()), backups_before);
        assert!(!journal_path(&path).exists());
        assert_eq!(
            receipts
                .active_for("claude-code", &path)
                .expect("active lookup"),
            Some(receipt)
        );
    }

    #[test]
    fn test_missing_canonical_receipt_cannot_authorize_uninstall() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let installed = "{\"mcpServers\":{\"compresso\":{\"command\":\"/one\"}}}\n";
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let receipts = MemoryReceiptStore::default();
        let install = manager
            .issue(install_request(path.clone(), installed))
            .expect("install plan");
        let receipt = manager
            .apply_install(&PlanApproval::from(&install), &receipts)
            .expect("install applies");
        let mut forged = receipt.clone();
        forged.plan_hash = "f".repeat(64);
        let uninstall = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: "{\"mcpServers\":{}}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(forged),
                warnings: Vec::new(),
            })
            .expect("forged receipt is structurally valid");
        let config_before = fs::read(&path).expect("config readable");
        let backups_before = backup_files(directory.path());

        let error = manager
            .apply_uninstall(&PlanApproval::from(&uninstall), &receipts)
            .expect_err("missing canonical receipt has no authority");
        assert!(matches!(
            error,
            IntegrationError::UninstallReceiptMissing { .. }
        ));
        assert_eq!(fs::read(&path).expect("config readable"), config_before);
        assert_eq!(backup_files(directory.path()), backups_before);
        assert!(!journal_path(&path).exists());
        assert_eq!(
            receipts
                .active_for("claude-code", &path)
                .expect("active lookup"),
            Some(receipt)
        );
    }

    #[test]
    fn test_superseded_receipt_cannot_uninstall_current_install() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let first_target = "{\"mcpServers\":{\"compresso\":{\"command\":\"/one\"}}}\n";
        let second_target = "{\"mcpServers\":{\"compresso\":{\"command\":\"/two\"}}}\n";
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let receipts = MemoryReceiptStore::default();
        let first_plan = manager
            .issue(install_request(path.clone(), first_target))
            .expect("first install plan");
        let first = manager
            .apply_install(&PlanApproval::from(&first_plan), &receipts)
            .expect("first install");
        let second_plan = manager
            .issue(install_request(path.clone(), second_target))
            .expect("second install plan");
        let second = manager
            .apply_install(&PlanApproval::from(&second_plan), &receipts)
            .expect("second install");

        fs::write(&path, first_target).expect("target manually reverted");
        let uninstall = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: "{\"mcpServers\":{}}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(first),
                warnings: Vec::new(),
            })
            .expect("stale uninstall plan");
        let backups_before = backup_files(directory.path());

        let error = manager
            .apply_uninstall(&PlanApproval::from(&uninstall), &receipts)
            .expect_err("superseded receipt has no uninstall authority");
        assert!(matches!(
            error,
            IntegrationError::UninstallReceiptNotActive { .. }
        ));
        assert_eq!(
            fs::read_to_string(&path).expect("config readable"),
            first_target
        );
        assert_eq!(backup_files(directory.path()), backups_before);
        assert!(!journal_path(&path).exists());
        assert_eq!(
            receipts
                .active_for("claude-code", &path)
                .expect("active lookup"),
            Some(second)
        );
    }

    #[test]
    fn test_missing_uninstall_target_is_not_recreated_and_receipt_stays_active() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let installed = "{\"mcpServers\":{\"compresso\":{\"command\":\"/one\"}}}\n";
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let receipts = MemoryReceiptStore::default();
        let install = manager
            .issue(install_request(path.clone(), installed))
            .expect("install plan");
        let receipt = manager
            .apply_install(&PlanApproval::from(&install), &receipts)
            .expect("install applies");
        fs::remove_file(&path).expect("config deleted outside Compresso");
        let uninstall = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: "{}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(receipt.clone()),
                warnings: vec!["key_modified:mcpServers.compresso".to_owned()],
            })
            .expect("missing-target uninstall preview");
        let backups_before = backup_files(directory.path());

        let error = manager
            .apply_uninstall(&PlanApproval::from(&uninstall), &receipts)
            .expect_err("missing config must not be recreated");
        assert!(matches!(
            error,
            IntegrationError::UninstallTargetMissing { .. }
        ));
        assert!(!path.exists());
        assert_eq!(backup_files(directory.path()), backups_before);
        assert!(!journal_path(&path).exists());
        assert_eq!(
            receipts
                .active_for("claude-code", &path)
                .expect("active lookup"),
            Some(receipt)
        );
    }

    fn backup_files(directory: &Path) -> Vec<PathBuf> {
        let mut paths = fs::read_dir(directory)
            .expect("directory readable")
            .map(|entry| entry.expect("entry readable").path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".compresso-backup-"))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    #[test]
    fn test_uninstall_retains_user_modified_owned_key_with_warning() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock.clone());
        let install = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{\"args\":[\"mcp\",\"serve\"]}}}\n",
            ))
            .expect("install plan");
        let receipts = MemoryReceiptStore::default();
        let receipt = manager
            .apply(&PlanApproval::from(&install), &receipts)
            .expect("install")
            .receipt
            .expect("install receipt");

        let current =
            "{\"mcpServers\":{\"compresso\":{\"args\":[\"mcp\",\"serve\",\"--user-edit\"]}}}\n";
        fs::write(&path, current).expect("user edit written");
        let uninstall = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: current.to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(receipt.clone()),
                warnings: vec!["key_modified:mcpServers.compresso".to_owned()],
            })
            .expect("safe uninstall plan");
        let report = manager
            .apply(&PlanApproval::from(&uninstall), &receipts)
            .expect("safe uninstall")
            .restore
            .expect("restore report");
        assert_eq!(report.removed_keys, Vec::<String>::new());
        assert_eq!(report.retained_keys, ["mcpServers.compresso"]);
        assert_eq!(fs::read_to_string(&path).expect("config readable"), current);
        assert_eq!(
            receipts
                .active_for("claude-code", &path)
                .expect("active lookup")
                .expect("receipt remains active")
                .uninstalled_at,
            None
        );

        let backups = backup_files(directory.path());
        let fresh = PlanManager::new("retained-repeat").expect("fresh manager");
        let repeat = fresh
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: current.to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(receipt),
                warnings: vec!["key_modified:mcpServers.compresso".to_owned()],
            })
            .expect("repeat retention plan");
        let repeated = fresh
            .apply_uninstall(&PlanApproval::from(&repeat), &receipts)
            .expect("repeat retention is idempotent");
        assert!(repeated.already_applied);
        assert_eq!(backup_files(directory.path()), backups);
        assert_eq!(receipts.all().expect("receipt snapshot").len(), 1);
    }

    #[test]
    fn test_uninstall_removes_matching_owned_key_and_preserves_unknown_sibling() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{\"unknown\":true}\n").expect("base written");
        let installed_config =
            "{\"unknown\":true,\"mcpServers\":{\"compresso\":{\"command\":\"/bin/compresso\"}}}\n";
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let install_request = install_request(path.clone(), installed_config);
        let install = manager.issue(install_request).expect("install plan");
        let receipts = MemoryReceiptStore::default();
        let receipt = manager
            .apply_install(&PlanApproval::from(&install), &receipts)
            .expect("install applies");

        let uninstall = manager
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: "{\"unknown\":true,\"mcpServers\":{}}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(receipt.clone()),
                warnings: Vec::new(),
            })
            .expect("uninstall plan");
        let report = manager
            .apply_uninstall(&PlanApproval::from(&uninstall), &receipts)
            .expect("uninstall applies");

        assert_eq!(report.removed_keys, ["mcpServers.compresso"]);
        assert!(report.retained_keys.is_empty());
        let result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("restored config readable"))
                .expect("restored config valid");
        assert_eq!(result["unknown"], json!(true));
        assert!(
            receipts
                .active_for("claude-code", &path)
                .expect("active lookup")
                .is_none()
        );

        let stored = receipts
            .by_plan_hash(&receipt.plan_hash)
            .expect("receipt lookup")
            .expect("stored receipt");
        let backups = backup_files(directory.path());
        let fresh = PlanManager::new("removed-repeat").expect("fresh manager");
        let repeat = fresh
            .issue(MutationRequest {
                operation: MutationKind::Uninstall,
                client: "claude-code".to_owned(),
                config_path: path.clone(),
                config_format: ConfigFormat::Json,
                target_contents: "{\"unknown\":true,\"mcpServers\":{}}\n".to_owned(),
                owned_paths: vec!["mcpServers.compresso".to_owned()],
                compresso_version: "0.1.0".to_owned(),
                prior_receipt: Some(stored),
                warnings: Vec::new(),
            })
            .expect("repeat uninstall plan");
        let repeated = fresh
            .apply_uninstall(&PlanApproval::from(&repeat), &receipts)
            .expect("repeat uninstall is idempotent");
        assert!(repeated.already_applied);
        assert_eq!(backup_files(directory.path()), backups);
        assert_eq!(receipts.all().expect("receipt snapshot").len(), 1);
    }

    #[test]
    fn test_wrong_apply_kind_rejects_before_config_write() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "{}\n").expect("base written");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let plan = manager
            .issue(install_request(
                path.clone(),
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("install plan");
        let error = manager
            .apply_uninstall(&PlanApproval::from(&plan), &MemoryReceiptStore::default())
            .expect_err("wrong kind rejected");
        assert!(matches!(
            error,
            IntegrationError::WrongPlanKind {
                expected: MutationKind::Uninstall,
                actual: MutationKind::Install,
            }
        ));
        assert_eq!(fs::read_to_string(path).expect("config readable"), "{}\n");
    }

    #[test]
    fn test_schema_compiles_and_validates_plan_and_receipt() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../schemas/integrations/v1.json"))
                .expect("schema parses");
        let validator = jsonschema::options()
            .with_draft(Draft::Draft7)
            .build(&schema)
            .expect("schema compiles");

        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("config.json");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let manager = manager(clock);
        let plan = manager
            .issue(install_request(
                path,
                "{\"mcpServers\":{\"compresso\":{}}}\n",
            ))
            .expect("plan issued");
        let exact_plan_value = serde_json::to_value(ExactMutationPlanRef::from(&plan))
            .expect("private exact plan serializes");
        assert!(validator.is_valid(&exact_plan_value));
        assert_eq!(exact_plan_value["document_type"], "mutation_plan");
        assert!(exact_plan_value.get("target_contents").is_some());

        let plan_value = serde_json::to_value(&plan).expect("public plan serialization is safe");
        assert!(validator.is_valid(&plan_value));
        let preview_value =
            serde_json::to_value(plan.public_preview()).expect("public preview serializes");
        assert!(validator.is_valid(&preview_value));
        assert_eq!(plan_value, preview_value);
        assert_eq!(preview_value["document_type"], "mutation_plan_preview");
        assert!(preview_value.get("target_contents").is_none());
        assert!(preview_value.get("prior_receipt").is_none());

        let receipts = MemoryReceiptStore::default();
        let receipt = manager
            .apply(&PlanApproval::from(&plan), &receipts)
            .expect("plan applies")
            .receipt
            .expect("receipt");
        let receipt_value = serde_json::to_value(&receipt).expect("receipt serializes");
        assert!(validator.is_valid(&receipt_value));

        let mut unknown = plan_value;
        unknown
            .as_object_mut()
            .expect("plan object")
            .insert("unknown".to_owned(), json!(true));
        assert!(!validator.is_valid(&unknown));
    }
}
