use std::env;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use compresso_core::RuntimePaths;
use compresso_integrations::{
    ClaudeCodeDiscovery, ClaudeCodeIntegration, CodexIntegration, CompactionHookClient,
    CompactionHookIntegration, ConfigFormat, CursorApplicationEvidence, CursorCliEvidence,
    CursorDiscovery, CursorIntegration, DeferredInstallAuthorization, Detection,
    ExploreAgentIntegration, FileReceiptStore, InstallCtx, InstallReceipt, Integration,
    IntegrationError, LegacyProjectHook, MutationPlan, PlanApproval, PlanClock, PlanManager,
    ReceiptStore, Scope, SessionCommandsIntegration, VerificationCheck, VerificationReport,
    read_configured_server, receipt_authenticates_server,
};
use compresso_store::{ReceiptRecord, Store};

use super::mcp_probe::{self, LauncherTrust, McpServerHealth, Severity};
use super::transcript_stats::printable;
use super::{
    CliExitCode, IntegrationClientArg, IntegrationMutationArgs, IntegrationReadArgs,
    IntegrationsCommand, write_error,
};

const RECEIPT_STORE_NAME: &str = "integration-receipts.json";
const PACKAGE_LAUNCHER_ENV: &str = "COMPRESSO_LAUNCHER_PATH";

/// Injectable filesystem and invocation context for integration CLI commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrationEnvironment {
    pub home_dir: PathBuf,
    pub cwd: PathBuf,
    pub compresso_bin: PathBuf,
    pub data_dir: PathBuf,
    pub path_entries: Vec<PathBuf>,
    pub session_id: String,
    /// Deadline for the live MCP handshake run by `integrations status`.
    /// `None` (the default for injected environments) starts no process.
    pub mcp_probe_timeout: Option<Duration>,
    /// The index database `compresso index` and `mcp serve` open; injected
    /// environments use `data_dir/compresso.db`.
    pub store_path: PathBuf,
}

impl IntegrationEnvironment {
    /// Creates a deterministic environment for embedders and tests.
    pub fn new(
        home_dir: PathBuf,
        cwd: PathBuf,
        compresso_bin: PathBuf,
        data_dir: PathBuf,
        path_entries: Vec<PathBuf>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            home_dir,
            cwd,
            compresso_bin,
            store_path: data_dir.join("compresso.db"),
            data_dir,
            path_entries,
            session_id: session_id.into(),
            mcp_probe_timeout: None,
        }
    }

    /// Uses `path` as the index database (the one `compresso index` writes).
    pub fn with_store_path(mut self, path: PathBuf) -> Self {
        self.store_path = path;
        self
    }

    /// Enables the bounded live MCP handshake reported by `integrations status`.
    pub fn with_mcp_probe_timeout(mut self, timeout: Duration) -> Self {
        self.mcp_probe_timeout = Some(timeout);
        self
    }

    pub(crate) fn system() -> Result<Self, IntegrationCliError> {
        let cwd = env::current_dir().map_err(|_| {
            IntegrationCliError::environment("failed to resolve the current working directory")
        })?;
        Self::system_for(&cwd)
    }

    pub(crate) fn system_for(cwd: &Path) -> Result<Self, IntegrationCliError> {
        let home_dir = system_home_dir()?;
        let compresso_bin = resolve_compresso_command()?;
        let data_dir = RuntimePaths::system()
            .map_err(|_| {
                IntegrationCliError::environment("failed to resolve the local data directory")
            })?
            .data_dir;
        let path_entries = env::var_os("PATH")
            .map(|path| env::split_paths(&path).collect())
            .unwrap_or_default();
        let session_id = format!(
            "cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        // One store for everything: the database `compresso index` and
        // `mcp serve` open, whatever XDG_DATA_HOME says (E17-51 review).
        let store_path = Store::default_path().map_err(|_| {
            IntegrationCliError::environment("failed to resolve the local index database")
        })?;
        Ok(Self::new(
            home_dir,
            cwd.to_path_buf(),
            compresso_bin,
            data_dir,
            path_entries,
            session_id,
        )
        .with_store_path(store_path)
        .with_mcp_probe_timeout(mcp_probe::DEFAULT_PROBE_TIMEOUT))
    }

    fn receipt_store_path(&self) -> PathBuf {
        receipt_store_path(&self.data_dir)
    }

    fn database_path(&self) -> PathBuf {
        self.store_path.clone()
    }
}

/// Location of the canonical integration receipt store inside a data directory.
pub(super) fn receipt_store_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RECEIPT_STORE_NAME)
}

fn resolve_compresso_command() -> Result<PathBuf, IntegrationCliError> {
    if let Some(launcher) = env::var_os(PACKAGE_LAUNCHER_ENV) {
        return validate_stable_launcher(PathBuf::from(launcher));
    }

    env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| {
            IntegrationCliError::environment("failed to resolve the installed Compresso executable")
        })
}

fn validate_stable_launcher(launcher: PathBuf) -> Result<PathBuf, IntegrationCliError> {
    if !launcher.is_absolute()
        || launcher
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(IntegrationCliError::environment(format!(
            "{PACKAGE_LAUNCHER_ENV} must name a normalized absolute path"
        )));
    }

    // `Path::components` folds redundant separators and `.` components while
    // retaining a final symlink as a lexical path. Never canonicalize here:
    // package-manager upgrades deliberately replace the shim's backing file.
    let mut normalized = PathBuf::new();
    for component in launcher.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => unreachable!("parent components were rejected above"),
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }

    let metadata = std::fs::metadata(&normalized).map_err(|_| {
        IntegrationCliError::environment(format!(
            "{PACKAGE_LAUNCHER_ENV} does not resolve to an installed launcher"
        ))
    })?;
    if !metadata.is_file() {
        return Err(IntegrationCliError::environment(format!(
            "{PACKAGE_LAUNCHER_ENV} must resolve to a file"
        )));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(IntegrationCliError::environment(format!(
                "{PACKAGE_LAUNCHER_ENV} must resolve to an executable file"
            )));
        }
    }

    Ok(normalized)
}

/// Executes an integration command against an injected, process-free environment.
pub fn execute_integrations(
    command: IntegrationsCommand,
    environment: &IntegrationEnvironment,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    execute(command, environment, input, output, errors)
}

pub(crate) fn execute(
    command: IntegrationsCommand,
    environment: &IntegrationEnvironment,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = command.json();
    let result = match command {
        IntegrationsCommand::Install(arguments) => execute_mutation(
            MutationAction::Install,
            arguments,
            environment,
            input,
            output,
            errors,
        ),
        IntegrationsCommand::Uninstall(arguments) => execute_mutation(
            MutationAction::Uninstall,
            arguments,
            environment,
            input,
            output,
            errors,
        ),
        IntegrationsCommand::Status(arguments) => execute_status(arguments, environment, output),
        IntegrationsCommand::Verify(arguments) => execute_verify(arguments, environment, output),
    };
    match result {
        Ok(()) => CliExitCode::Success,
        Err(error) => {
            let _ = write_error(errors, error.code, &error.message, json);
            CliExitCode::Failure
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationAction {
    Install,
    Uninstall,
}

impl MutationAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
        }
    }
}

/// One exact integration effect retained while setup renews short-lived plan timing.
pub(crate) struct PreparedSetupInstall {
    client: IntegrationClientArg,
    compaction_hook: bool,
    scope: Scope,
    launcher_path: PathBuf,
    selected: SelectedIntegration,
    manager: PlanManager,
    receipts: FileReceiptStore,
    plan: MutationPlan,
}

pub(crate) struct SetupInstallResult {
    pub(crate) effective_plan_hash: String,
    pub(crate) effective_diff_hash: String,
    pub(crate) effective_backup_path: String,
    pub(crate) receipt_plan_hash: String,
    pub(crate) receipt_backup_path: String,
    pub(crate) already_applied: bool,
    pub(crate) warnings: Vec<String>,
}

impl PreparedSetupInstall {
    pub(crate) fn plan(&self) -> &MutationPlan {
        &self.plan
    }

    /// The validated launcher path this plan writes into client configuration.
    pub(crate) fn launcher_path(&self) -> &Path {
        &self.launcher_path
    }

    pub(crate) fn authorize(&self) -> Result<DeferredInstallAuthorization, IntegrationCliError> {
        let approval = PlanApproval::from(&self.plan);
        self.manager
            .authorize_deferred_install(&approval)
            .map_err(Into::into)
    }

    pub(crate) fn revalidate_target(
        &self,
        environment: &IntegrationEnvironment,
    ) -> Result<(), IntegrationCliError> {
        let current =
            SelectedIntegration::new(self.client, environment, &self.scope, self.compaction_hook)?;
        if current.config_path != self.selected.config_path {
            return Err(IntegrationCliError::new(
                "config_drift",
                "integration config target changed after setup preview",
            ));
        }
        Ok(())
    }

    pub(crate) fn apply(
        self,
        authorization: DeferredInstallAuthorization,
        environment: &IntegrationEnvironment,
    ) -> Result<SetupInstallResult, IntegrationCliError> {
        let current_launcher = validate_stable_launcher(environment.compresso_bin.clone())?;
        if current_launcher != self.launcher_path {
            return Err(IntegrationCliError::new(
                "config_drift",
                "Compresso launcher path changed after setup preview",
            ));
        }
        let applied = self
            .manager
            .apply_deferred_install(authorization, &self.receipts)?;
        let receipt = applied.report.receipt.as_ref().ok_or_else(|| {
            IntegrationCliError::new(
                "internal",
                "deferred integration install did not produce a receipt",
            )
        })?;
        let mut warnings = Vec::new();
        project_canonical_receipts(&self.receipts, environment, &mut warnings);
        Ok(SetupInstallResult {
            effective_plan_hash: applied.plan.plan_hash,
            effective_diff_hash: applied.plan.diff_hash,
            effective_backup_path: applied.plan.backup_path,
            receipt_plan_hash: receipt.plan_hash.clone(),
            receipt_backup_path: receipt.backup_path.clone(),
            already_applied: applied.report.already_applied,
            warnings,
        })
    }
}

/// Prepares a setup install without recovery or filesystem mutation.
///
/// Holding the manager and plan lets setup display one combined preview. After confirmation,
/// setup converts the short-lived preview into an opaque one-use authorization. After indexing,
/// the manager renews timing only and applies the exact approved diff and backup path. Pending
/// recovery or later drift therefore fails closed.
pub(crate) fn prepare_setup_install(
    client: IntegrationClientArg,
    project: Option<&Path>,
    session_recall: bool,
    entry: SetupEntry,
    environment: &IntegrationEnvironment,
    clock: Arc<dyn PlanClock>,
) -> Result<PreparedSetupInstall, IntegrationCliError> {
    prepare_install(
        client,
        project,
        false,
        session_recall,
        entry,
        environment,
        clock,
    )
}

/// Plans setup's main-session agent step (E17-51) without writing
/// anything, for the combined preview: installing it, or (`remove`, for
/// `--no-main-agent` or a skipped step) removing what earlier receipts
/// cover. A hard refusal fails setup before it changes anything; skips are
/// warnings.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_setup_main_agent(
    root: &Path,
    entry: SetupEntry,
    session_recall: bool,
    environment: &IntegrationEnvironment,
    clock: Arc<dyn PlanClock>,
    dry_run: bool,
    remove: bool,
    warnings: &mut Vec<String>,
) -> Result<(super::main_agent::MainAgentCompanion, PlanManager), IntegrationCliError> {
    let scope = resolve_scope(Some(root), &environment.cwd)?;
    let launcher_path = validate_stable_launcher(environment.compresso_bin.clone())?;
    let manager = PlanManager::with_clock(&environment.session_id, clock)?;
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    if remove {
        let companion = super::main_agent::plan_uninstall(
            &environment.home_dir,
            &scope,
            &manager,
            &receipts,
            dry_run,
            warnings,
        );
        return Ok((companion, manager));
    }
    let companion = super::main_agent::plan_install(
        &environment.home_dir,
        &scope,
        entry.edits,
        &manager,
        &receipts,
        &launcher_path,
        session_recall,
        dry_run,
        warnings,
    )?;
    if companion.is_complete()
        && let Some(warning) = super::main_agent::settings_not_ignored_warning(&scope_root(&scope))
    {
        warnings.push(warning);
    }
    Ok((companion, manager))
}

/// Applies setup's main-session agent step after the MCP entry landed: the
/// step is planned again and applied only if it is exactly what was
/// previewed; otherwise nothing is written and a warning says so.
pub(crate) fn apply_setup_main_agent(
    root: &Path,
    entry: SetupEntry,
    session_recall: bool,
    previewed: &super::main_agent::MainAgentCompanion,
    environment: &IntegrationEnvironment,
    clock: Arc<dyn PlanClock>,
) -> (
    Vec<(&'static str, super::main_agent::StepOutcome)>,
    Vec<String>,
) {
    let mut warnings = Vec::new();
    let planned = plan_setup_main_agent(
        root,
        entry,
        session_recall,
        environment,
        clock,
        false,
        !previewed.installs(),
        &mut Vec::new(),
    );
    let (companion, manager) = match planned {
        Ok(planned) => planned,
        Err(error) => {
            warnings.push(format!("main_agent_not_installed: {}", error.message));
            return (Vec::new(), warnings);
        }
    };
    let same = companion.steps.len() == previewed.steps.len()
        && companion
            .steps
            .iter()
            .zip(&previewed.steps)
            .all(|(now, then)| {
                now.label == then.label
                    && now.plan.base_hash == then.plan.base_hash
                    && now.plan.result_hash == then.plan.result_hash
            });
    if !same {
        warnings.push(
            "main_agent_not_installed: the main-session agent files changed after the setup preview, so nothing was written; run `compresso integrations install claude-code --project <repository-root>` to review and add it".to_owned(),
        );
        return (Vec::new(), warnings);
    }
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let outcomes = companion.apply(&manager, &receipts);
    for (label, outcome) in &outcomes {
        if let super::main_agent::StepOutcome::Failed(message) = outcome {
            warnings.push(format!("{label}_failed: {message}"));
        }
    }
    project_canonical_receipts(&receipts, environment, &mut warnings);
    (outcomes, warnings)
}

/// Refuses setup while an interrupted change to the agent or settings file
/// is pending, naming the journal (setup previews before it writes).
pub(crate) fn refuse_pending_main_agent(
    root: &Path,
    environment: &IntegrationEnvironment,
) -> Result<(), IntegrationCliError> {
    let scope = resolve_scope(Some(root), &environment.cwd)?;
    let manager = PlanManager::new(&environment.session_id)?;
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    super::main_agent::recover_pending(&environment.home_dir, &scope, &manager, &receipts, true)
        .map(|_| ())
}

/// Removes the agent file (and key) setup just wrote through their receipts,
/// when the pair did not land in full.
pub(crate) fn rollback_setup_main_agent(
    root: &Path,
    environment: &IntegrationEnvironment,
) -> (
    Vec<(&'static str, super::main_agent::StepOutcome)>,
    Vec<String>,
) {
    let mut warnings = Vec::new();
    let (Ok(scope), Ok(manager)) = (
        resolve_scope(Some(root), &environment.cwd),
        PlanManager::new(&environment.session_id),
    ) else {
        return (Vec::new(), warnings);
    };
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let rollback = super::main_agent::plan_uninstall(
        &environment.home_dir,
        &scope,
        &manager,
        &receipts,
        false,
        &mut warnings,
    );
    let outcomes = rollback.apply(&manager, &receipts);
    project_canonical_receipts(&receipts, environment, &mut warnings);
    (outcomes, warnings)
}

fn scope_root(scope: &Scope) -> PathBuf {
    match scope {
        Scope::Project(root) => root.clone(),
        Scope::User => PathBuf::new(),
    }
}

/// Prepares the post-compaction routing-reminder hook install for setup, with
/// the same no-recovery, no-mutation guarantees as [`prepare_setup_install`].
///
/// The hook always goes into the client's user-level hook file, never into a
/// repository file, with its own plan and receipt. A refusal because the user
/// already defines hooks for the owned event carries the entry to add by hand
/// in `manual_hook_entry`.
pub(crate) fn prepare_setup_hook_install(
    client: IntegrationClientArg,
    environment: &IntegrationEnvironment,
    clock: Arc<dyn PlanClock>,
) -> Result<PreparedSetupInstall, IntegrationCliError> {
    prepare_install(
        client,
        None,
        true,
        false,
        SetupEntry::default(),
        environment,
        clock,
    )
}

/// What will happen to a reminder hook that an earlier Compresso version
/// installed in a repository's project-level hook file. The hook is user-level
/// now, so a receipt-owned project entry is removed; anything else is left
/// unchanged and reported.
pub(crate) enum LegacyHookMigration {
    /// The exact removal of the receipt-owned key, as previewed.
    Planned(Box<MutationPlan>),
    /// A Compresso-shaped hook that no receipt covers. Never modified.
    Unreceipted { config_path: PathBuf },
    /// A receipted hook that cannot be planned for removal right now.
    Unavailable {
        config_path: PathBuf,
        reason: String,
    },
}

impl LegacyHookMigration {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Planned(plan) => serde_json::json!({
                "status": "planned",
                "config_path": plan.config_path,
                "plan": plan.public_preview(),
            }),
            Self::Unreceipted { config_path } => serde_json::json!({
                "status": "unreceipted",
                "config_path": config_path,
                "warning": unreceipted_legacy_hook_warning(config_path),
            }),
            Self::Unavailable {
                config_path,
                reason,
            } => serde_json::json!({
                "status": "unavailable",
                "config_path": config_path,
                "reason": reason,
            }),
        }
    }

    /// Writes the human preview shown before confirmation.
    pub(crate) fn write_preview(&self, output: &mut impl Write) -> std::io::Result<()> {
        match self {
            Self::Planned(plan) => {
                writeln!(
                    output,
                    "# The reminder hook is user-level now. This also removes the hook an earlier Compresso version installed in the repository file {} (only the receipt-owned hooks.SessionStart key; a fresh backup is taken):",
                    plan.config_path.display()
                )?;
                output.write_all(plan.unified_diff.as_bytes())?;
                if !plan.unified_diff.ends_with('\n') {
                    writeln!(output)?;
                }
                for warning in &plan.warnings {
                    writeln!(output, "warning: {warning}")?;
                }
                writeln!(output, "# Migration plan hash: {}", plan.plan_hash)
            }
            Self::Unreceipted { config_path } => writeln!(
                output,
                "warning: {}",
                unreceipted_legacy_hook_warning(config_path)
            ),
            Self::Unavailable {
                config_path,
                reason,
            } => writeln!(
                output,
                "warning: the reminder hook an earlier Compresso version installed in {} will not be removed: {reason}",
                config_path.display()
            ),
        }
    }
}

fn unreceipted_legacy_hook_warning(config_path: &Path) -> String {
    format!(
        "legacy_project_hook_unreceipted: {} contains a Compresso reminder hook that no install receipt covers, so it is left unchanged; remove hooks.SessionStart from that file by hand if you do not want it in the repository",
        config_path.display()
    )
}

/// What happened to a legacy project-level reminder hook.
pub(crate) struct LegacyHookOutcome {
    status: &'static str,
    config_path: PathBuf,
    backup_path: Option<String>,
    detail: String,
}

impl LegacyHookOutcome {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status,
            "config_path": self.config_path,
            "backup_path": self.backup_path,
            "detail": self.detail,
        })
    }

    /// One operator-facing line; a `warning: ` prefix unless the hook was removed.
    pub(crate) fn human_line(&self) -> String {
        if self.status == "removed" {
            self.detail.clone()
        } else {
            format!("warning: {}", self.detail)
        }
    }
}

struct LegacyHookPlan {
    adapter: CompactionHookIntegration,
    manager: PlanManager,
    receipts: FileReceiptStore,
    plan: MutationPlan,
}

enum LegacyHookProbe {
    Planned(Box<LegacyHookPlan>),
    Other(LegacyHookMigration),
}

fn probe_legacy_hook(
    client: IntegrationClientArg,
    root: &Path,
    environment: &IntegrationEnvironment,
) -> Option<LegacyHookProbe> {
    let adapter = CompactionHookIntegration::new(
        compaction_hook_client(client).ok()?,
        environment.home_dir.clone(),
    );
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let receipt = match adapter.legacy_project_hook(root, &receipts) {
        Ok(LegacyProjectHook::Absent) => return None,
        Ok(LegacyProjectHook::Unreceipted { config_path }) => {
            return Some(LegacyHookProbe::Other(LegacyHookMigration::Unreceipted {
                config_path,
            }));
        }
        Ok(LegacyProjectHook::Receipted(receipt)) => receipt,
        // The project hook path is unusable (for example it traverses a
        // symbolic link), so nothing there is read or written.
        Err(_) => return None,
    };
    let unavailable = |reason: String| {
        Some(LegacyHookProbe::Other(LegacyHookMigration::Unavailable {
            config_path: receipt.config_path.clone(),
            reason,
        }))
    };
    let manager = match PlanManager::new(&environment.session_id) {
        Ok(manager) => manager,
        Err(error) => return unavailable(error.to_string()),
    };
    match adapter.plan_uninstall(&manager, &receipt) {
        Ok(plan) => Some(LegacyHookProbe::Planned(Box::new(LegacyHookPlan {
            adapter,
            manager,
            receipts,
            plan,
        }))),
        Err(error) => unavailable(error.to_string()),
    }
}

/// Previews the removal of a legacy project-level reminder hook in `root`.
/// Reads only. `None` means the repository has nothing to migrate.
pub(crate) fn plan_legacy_hook_migration(
    client: IntegrationClientArg,
    root: &Path,
    environment: &IntegrationEnvironment,
) -> Option<LegacyHookMigration> {
    Some(match probe_legacy_hook(client, root, environment)? {
        LegacyHookProbe::Planned(planned) => LegacyHookMigration::Planned(Box::new(planned.plan)),
        LegacyHookProbe::Other(migration) => migration,
    })
}

/// Carries out a previewed migration. The removal is planned again against
/// the file as it is now and applied only when it is the exact change that
/// was previewed (same full-file before and after hashes), so an edit made
/// since the preview is never overwritten. Never fails the caller.
pub(crate) fn apply_legacy_hook_migration(
    previewed: &LegacyHookMigration,
    client: IntegrationClientArg,
    root: &Path,
    environment: &IntegrationEnvironment,
) -> LegacyHookOutcome {
    let previewed = match previewed {
        LegacyHookMigration::Planned(plan) => plan,
        LegacyHookMigration::Unreceipted { config_path } => {
            return LegacyHookOutcome {
                status: "unreceipted",
                config_path: config_path.clone(),
                backup_path: None,
                detail: unreceipted_legacy_hook_warning(config_path),
            };
        }
        LegacyHookMigration::Unavailable {
            config_path,
            reason,
        } => {
            return LegacyHookOutcome {
                status: "skipped",
                config_path: config_path.clone(),
                backup_path: None,
                detail: legacy_hook_not_removed(client, root, config_path, reason),
            };
        }
    };
    let config_path = previewed.config_path.clone();
    let not_removed = |status: &'static str, reason: &str| LegacyHookOutcome {
        status,
        config_path: config_path.clone(),
        backup_path: None,
        detail: legacy_hook_not_removed(client, root, &config_path, reason),
    };
    let Some(LegacyHookProbe::Planned(current)) = probe_legacy_hook(client, root, environment)
    else {
        return not_removed(
            "skipped",
            "the file or its receipt changed after the preview",
        );
    };
    if current.plan.config_path != previewed.config_path
        || current.plan.base_hash != previewed.base_hash
        || current.plan.result_hash != previewed.result_hash
    {
        return not_removed("skipped", "the file changed after the preview");
    }
    let report = match current.adapter.uninstall(
        &current.manager,
        &PlanApproval::from(&current.plan),
        &current.receipts,
    ) {
        Ok(report) => report,
        Err(error) => return not_removed("failed", &error.to_string()),
    };
    project_canonical_receipts(&current.receipts, environment, &mut Vec::new());
    if report.removed_keys.is_empty() {
        LegacyHookOutcome {
            status: "retained",
            config_path: config_path.clone(),
            backup_path: None,
            detail: format!(
                "legacy_project_hook_retained: hooks.SessionStart in {} was changed after Compresso installed it, so it was left unchanged; remove it by hand if you do not want it in the repository",
                config_path.display()
            ),
        }
    } else {
        LegacyHookOutcome {
            status: "removed",
            detail: format!(
                "Removed the reminder hook an earlier Compresso version installed in {} (backup: {}).",
                config_path.display(),
                current.plan.backup_path
            ),
            config_path,
            backup_path: Some(current.plan.backup_path),
        }
    }
}

fn legacy_hook_not_removed(
    client: IntegrationClientArg,
    root: &Path,
    config_path: &Path,
    reason: &str,
) -> String {
    format!(
        "legacy_project_hook_not_removed: the reminder hook an earlier Compresso version installed in {} was not removed ({reason}); remove it with `compresso integrations uninstall {} --compaction-hook --project {}`",
        config_path.display(),
        client.as_str(),
        root.display()
    )
}

/// How setup's MCP entry is written: plain, or (Claude Code, E17-51) the
/// main-session profile with or without the edit tool.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SetupEntry {
    pub(crate) main_agent: bool,
    pub(crate) edits: bool,
}

#[allow(clippy::too_many_arguments)]
fn prepare_install(
    client: IntegrationClientArg,
    project: Option<&Path>,
    compaction_hook: bool,
    session_recall: bool,
    entry: SetupEntry,
    environment: &IntegrationEnvironment,
    clock: Arc<dyn PlanClock>,
) -> Result<PreparedSetupInstall, IntegrationCliError> {
    let scope = resolve_scope(project, &environment.cwd)?;
    let launcher_path = validate_stable_launcher(environment.compresso_bin.clone())?;
    let selected = if client == IntegrationClientArg::ClaudeCode
        && !compaction_hook
        && (entry.main_agent || entry.edits)
    {
        SelectedIntegration::claude_code_with_operator_tools(
            environment,
            &scope,
            entry.edits,
            None,
            entry.main_agent && matches!(scope, Scope::Project(_)),
        )?
    } else {
        SelectedIntegration::new(client, environment, &scope, compaction_hook)?
    };
    let manager = PlanManager::with_clock(&environment.session_id, clock)?;
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let plan = selected.adapter.plan_install(
        &manager,
        &InstallCtx {
            compresso_bin: launcher_path.clone(),
            scope: scope.clone(),
            session_recall,
        },
    )?;
    Ok(PreparedSetupInstall {
        client,
        compaction_hook,
        scope,
        launcher_path,
        selected,
        manager,
        receipts,
        plan,
    })
}

fn execute_mutation(
    action: MutationAction,
    arguments: IntegrationMutationArgs,
    environment: &IntegrationEnvironment,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> Result<(), IntegrationCliError> {
    if arguments.no_recall && action == MutationAction::Uninstall {
        return Err(IntegrationCliError::new(
            "invalid_arguments",
            "--no-recall applies only to `integrations install`; uninstall removes the whole Compresso entry",
        ));
    }
    if arguments.edits && action == MutationAction::Uninstall {
        return Err(IntegrationCliError::new(
            "invalid_arguments",
            "--edits applies only to `integrations install`; uninstall removes the whole Compresso entry",
        ));
    }
    if arguments.sql_env.is_some() && action == MutationAction::Uninstall {
        return Err(IntegrationCliError::new(
            "invalid_arguments",
            "--sql-env applies only to `integrations install`; uninstall removes the whole Compresso entry",
        ));
    }
    if let Some(name) = &arguments.sql_env
        && !compresso_integrations::is_sql_env_name(name)
    {
        return Err(IntegrationCliError::new(
            "invalid_arguments",
            "--sql-env needs a variable name matching COMPRESSO_SQL_[A-Z0-9_]+ (the name of the variable, never the URL)",
        ));
    }
    if (arguments.no_main_agent || arguments.no_edits) && action == MutationAction::Uninstall {
        return Err(IntegrationCliError::new(
            "invalid_arguments",
            "--no-main-agent and --no-edits apply only to `integrations install`; a project uninstall removes the main-session agent with the entry",
        ));
    }
    if (arguments.no_main_agent || arguments.no_edits)
        && arguments.client != IntegrationClientArg::ClaudeCode
    {
        return Err(IntegrationCliError::new(
            "unsupported",
            "--no-main-agent and --no-edits are available for claude-code only",
        ));
    }
    if arguments.no_session_commands && action == MutationAction::Uninstall {
        return Err(IntegrationCliError::new(
            "invalid_arguments",
            "--no-session-commands applies only to `integrations install`; use --session-commands to remove only the session command skill",
        ));
    }
    if arguments.edits && arguments.client != IntegrationClientArg::ClaudeCode {
        return Err(IntegrationCliError::new(
            "unsupported",
            "--edits is available for claude-code only; other clients can run `compresso mcp serve --edits` from their own configuration",
        ));
    }
    if arguments.sql_env.is_some() && arguments.client != IntegrationClientArg::ClaudeCode {
        return Err(IntegrationCliError::new(
            "unsupported",
            "--sql-env is available for claude-code only; other clients can run `compresso mcp serve --sql-env NAME` from their own configuration",
        ));
    }
    let requested_scope = if arguments.explore_agent || arguments.session_commands {
        explore_agent_scope(arguments.project.as_deref(), &environment.cwd)?
    } else {
        resolve_scope(arguments.project.as_deref(), &environment.cwd)?
    };
    // The reminder hook is installed only in user-level configuration. For a
    // hook install `--project` (or else the working directory) just names the
    // repository whose legacy project-level hook is migrated away.
    let (scope, migration_root) = if arguments.compaction_hook && action == MutationAction::Install
    {
        let root = match requested_scope {
            Scope::Project(root) => Some(root),
            Scope::User => std::fs::canonicalize(&environment.cwd).ok(),
        };
        (Scope::User, root)
    } else {
        (requested_scope, None)
    };
    let manager = PlanManager::new(&environment.session_id)?;
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let launcher_path = match action {
        MutationAction::Install => {
            Some(validate_stable_launcher(environment.compresso_bin.clone())?)
        }
        MutationAction::Uninstall => None,
    };
    // The default main-session agent (E17-51, D116): a plain Claude Code
    // project install adds it unless --no-main-agent or --sql-env; its entry
    // serves the main-session profile with the edit tool unless --no-edits.
    // The agent file, the `agent` key, and the entry always agree: when the
    // agent cannot be installed in full, the entry is written exactly as
    // --no-main-agent writes it and any agent an earlier install left is
    // removed through its receipts. Everything is planned before any write.
    let project_entry = arguments.client == IntegrationClientArg::ClaudeCode
        && plain_entry_change(&arguments)
        && matches!(scope, Scope::Project(_));
    let mut main_agent_warnings = Vec::new();
    // An interrupted change to the agent file or the settings file is
    // finished before anything reads them; a dry run refuses instead.
    if project_entry
        && super::main_agent::recover_pending(
            &environment.home_dir,
            &scope,
            &manager,
            &receipts,
            arguments.dry_run,
        )?
    {
        project_canonical_receipts(&receipts, environment, &mut Vec::new());
    }
    if action == MutationAction::Install
        && arguments.client == IntegrationClientArg::ClaudeCode
        && !arguments.no_main_agent
        && plain_entry_change(&arguments)
        && matches!(scope, Scope::User)
    {
        main_agent_warnings.push(
            "main_agent_not_installed: the main-session agent is a project setting; install with --project DIR to add it to a repository".to_owned(),
        );
    }
    let wants_main_agent = action == MutationAction::Install
        && main_agent_default(&arguments, &scope)
        && arguments.sql_env.is_none();
    if action == MutationAction::Install
        && main_agent_default(&arguments, &scope)
        && arguments.sql_env.is_some()
    {
        main_agent_warnings.push(
            "main_agent_not_installed: with --sql-env the entry keeps the full tool profile (the lean profile never serves compresso_sql), and the main-session agent was measured only with the lean profile, so it is not installed (an agent an earlier install wrote is removed); install without --sql-env to get the agent and the lean profile instead of the SQL tool".to_owned(),
        );
    }
    let mut main_agent_companion = if wants_main_agent {
        super::main_agent::plan_install(
            &environment.home_dir,
            &scope,
            !arguments.no_edits,
            &manager,
            &receipts,
            launcher_path
                .as_deref()
                .expect("install actions validate a launcher"),
            !arguments.no_recall,
            arguments.dry_run,
            &mut main_agent_warnings,
        )?
    } else {
        super::main_agent::MainAgentCompanion::default()
    };
    let main_agent = wants_main_agent && main_agent_companion.is_complete();
    if !main_agent && project_entry {
        // An uninstall, --no-main-agent, --sql-env, or a skipped step: remove
        // what earlier receipts cover, so no agent runs against an entry that
        // does not serve its measured profile.
        main_agent_companion = super::main_agent::plan_uninstall(
            &environment.home_dir,
            &scope,
            &manager,
            &receipts,
            arguments.dry_run,
            &mut main_agent_warnings,
        );
    }
    let edits = arguments.edits || (main_agent && !arguments.no_edits);
    let selected = if arguments.explore_agent {
        SelectedIntegration::explore_agent(arguments.client, environment, &scope, arguments.edits)?
    } else if arguments.session_commands {
        SelectedIntegration::session_commands(arguments.client, environment, &scope)?
    } else if main_agent || edits || arguments.sql_env.is_some() {
        SelectedIntegration::claude_code_with_operator_tools(
            environment,
            &scope,
            edits,
            arguments.sql_env.clone(),
            main_agent,
        )?
    } else {
        SelectedIntegration::new(
            arguments.client,
            environment,
            &scope,
            arguments.compaction_hook,
        )?
    };

    // Recovery can mutate config and receipts, so it is intentionally never
    // attempted by dry-run. A pending journal instead makes planning fail.
    if !arguments.dry_run && manager.recover(&selected.config_path, &receipts)?.is_some() {
        project_canonical_receipts(&receipts, environment, &mut Vec::new());
    }

    let install_context = launcher_path.clone().map(|compresso_bin| InstallCtx {
        compresso_bin,
        scope: scope.clone(),
        session_recall: !arguments.no_recall,
    });
    let planned = match action {
        MutationAction::Install => selected
            .adapter
            .plan_install(
                &manager,
                install_context
                    .as_ref()
                    .expect("install actions validate a launcher"),
            )
            .map_err(IntegrationCliError::from),
        MutationAction::Uninstall => receipts
            .active_for(arguments.client.as_str(), &selected.config_path)
            .map_err(IntegrationCliError::from)
            .and_then(|receipt| {
                receipt.ok_or_else(|| {
                    IntegrationCliError::new(
                        "receipt_not_found",
                        "no active canonical receipt authorizes this uninstall",
                    )
                })
            })
            .and_then(|receipt| {
                selected
                    .adapter
                    .plan_uninstall(&manager, &receipt)
                    .map_err(IntegrationCliError::from)
            }),
    };
    // Each owned item is removed independently: an entry that is already
    // gone (or whose receipt a crashed run consumed) does not stop the
    // agent file and key from being removed (E17-51 review).
    let plan = match planned {
        Ok(plan) => plan,
        Err(error)
            if action == MutationAction::Uninstall
                && project_entry
                && !main_agent_companion.is_empty() =>
        {
            return uninstall_main_agent_without_entry(
                &arguments,
                environment,
                &manager,
                &receipts,
                &main_agent_companion,
                main_agent_warnings,
                &error,
                output,
            );
        }
        Err(error) => return Err(error),
    };
    // With the main-session agent, the entry written when the agent file or
    // the key cannot be written at apply time: exactly what --no-main-agent
    // writes. It is part of the preview, so nothing unpreviewed is written.
    let fallback = if main_agent {
        let fallback_selected = if arguments.edits {
            SelectedIntegration::claude_code_with_operator_tools(
                environment,
                &scope,
                true,
                None,
                false,
            )?
        } else {
            SelectedIntegration::new(arguments.client, environment, &scope, false)?
        };
        let fallback_plan = fallback_selected.adapter.plan_install(
            &manager,
            install_context
                .as_ref()
                .expect("install actions validate a launcher"),
        )?;
        Some((fallback_selected, fallback_plan))
    } else {
        None
    };
    if action == MutationAction::Install && arguments.client == IntegrationClientArg::ClaudeCode {
        main_agent_warnings.extend(removed_entry_flags(
            &selected.config_path,
            &plan,
            &arguments,
        ));
    }
    if main_agent
        && let Scope::Project(root) = &scope
        && let Some(warning) = super::main_agent::settings_not_ignored_warning(root)
    {
        main_agent_warnings.push(warning);
    }
    let migration = migration_root
        .as_deref()
        .and_then(|root| plan_legacy_hook_migration(arguments.client, root, environment));
    let mut companion_warnings = main_agent_warnings;
    let companion = plan_session_commands_companion(
        action,
        &arguments,
        environment,
        &scope,
        &manager,
        &receipts,
        launcher_path.as_deref(),
        &mut companion_warnings,
    );

    if arguments.dry_run {
        return render_plan(
            output,
            &PlanRendering {
                action,
                client: arguments.client,
                dry_run: true,
                plan: &plan,
                result: None,
                warnings: &companion_warnings,
                migration: migration.as_ref(),
                migration_outcome: None,
                session_commands: companion.as_ref().map(|companion| &companion.plan),
                session_commands_result: None,
                main_agent: &main_agent_companion,
                main_agent_result: &[],
                fallback_entry: fallback.as_ref().map(|(_, plan)| plan),
                json: arguments.output.json,
            },
        );
    }

    if !arguments.output.json {
        output.write_all(plan.unified_diff.as_bytes())?;
        writeln!(output, "# Plan hash: {}", plan.plan_hash)?;
        if let Some(migration) = &migration {
            migration.write_preview(output)?;
        }
        if let Some(companion) = &companion {
            write_session_commands_preview(output, &companion.plan)?;
        }
        main_agent_companion.write_preview(output)?;
        if let Some((_, fallback_plan)) = &fallback {
            write_fallback_preview(output, fallback_plan)?;
        }
    }
    if !arguments.yes {
        if arguments.output.json {
            render_plan(
                output,
                &PlanRendering {
                    action,
                    client: arguments.client,
                    dry_run: false,
                    plan: &plan,
                    result: None,
                    warnings: &companion_warnings,
                    migration: migration.as_ref(),
                    migration_outcome: None,
                    session_commands: companion.as_ref().map(|companion| &companion.plan),
                    session_commands_result: None,
                    main_agent: &main_agent_companion,
                    main_agent_result: &[],
                    fallback_entry: fallback.as_ref().map(|(_, plan)| plan),
                    json: true,
                },
            )?;
            return Err(IntegrationCliError::new(
                "confirmation_required",
                "JSON mutations require --yes after reviewing the returned plan",
            ));
        }
        write!(errors, "Apply? [y/N] ")?;
        errors.flush()?;
        match read_confirmation(input)? {
            Confirmation::Approved => {}
            Confirmation::Declined => {
                writeln!(output, "cancelled")?;
                return Ok(());
            }
            Confirmation::Unavailable => {
                return Err(IntegrationCliError::new(
                    "confirmation_required",
                    "interactive confirmation is unavailable; rerun with --yes",
                ));
            }
        }
    }

    if let Some(planned_launcher) = launcher_path {
        let current_launcher = validate_stable_launcher(environment.compresso_bin.clone())?;
        if current_launcher != planned_launcher {
            return Err(IntegrationCliError::new(
                "config_drift",
                "Compresso launcher path changed after integration preview",
            ));
        }
    }

    // Order (E17-51 review): with the main-session agent, the agent file and
    // the key are written first and the lean entry only if both landed;
    // otherwise the agent file just written is removed and the previewed
    // fallback entry is written. An uninstall removes the key and the agent
    // file before the entry; any other install writes the entry first.
    let mut late_warnings = Vec::new();
    let mut main_agent_result = Vec::new();
    let mut effective_plan = &plan;
    let result = if let Some((fallback_selected, fallback_plan)) = &fallback {
        main_agent_result = main_agent_companion.apply(&manager, &receipts);
        let landed = main_agent_result
            .iter()
            .filter(|(_, outcome)| matches!(outcome, super::main_agent::StepOutcome::Installed))
            .count()
            == 2;
        if landed {
            match selected
                .adapter
                .install(&manager, &PlanApproval::from(&plan), &receipts)
            {
                Ok(receipt) => MutationResult::Install(receipt),
                Err(error) => {
                    // Keep the agent and the entry in agreement: the agent
                    // just written is removed again when its entry fails.
                    let rollback = super::main_agent::plan_uninstall(
                        &environment.home_dir,
                        &scope,
                        &manager,
                        &receipts,
                        false,
                        &mut Vec::new(),
                    );
                    rollback.apply(&manager, &receipts);
                    project_canonical_receipts(&receipts, environment, &mut Vec::new());
                    return Err(error.into());
                }
            }
        } else {
            let rollback = super::main_agent::plan_uninstall(
                &environment.home_dir,
                &scope,
                &manager,
                &receipts,
                false,
                &mut late_warnings,
            );
            main_agent_result.extend(rollback.apply(&manager, &receipts));
            late_warnings.push(
                "main_agent_not_installed: the agent file or the \"agent\" setting could not be written (a file changed after the preview), so the full-profile entry shown as the alternative was written and any agent file just written was removed; run the install again to get the default".to_owned(),
            );
            effective_plan = fallback_plan;
            MutationResult::Install(fallback_selected.adapter.install(
                &manager,
                &PlanApproval::from(fallback_plan),
                &receipts,
            )?)
        }
    } else {
        if action == MutationAction::Uninstall {
            main_agent_result = main_agent_companion.apply(&manager, &receipts);
        }
        let approval = PlanApproval::from(&plan);
        let result = match action {
            MutationAction::Install => {
                MutationResult::Install(selected.adapter.install(&manager, &approval, &receipts)?)
            }
            MutationAction::Uninstall => MutationResult::Uninstall(
                selected.adapter.uninstall(&manager, &approval, &receipts)?,
            ),
        };
        if action == MutationAction::Install {
            main_agent_result = main_agent_companion.apply(&manager, &receipts);
        }
        result
    };
    // The session command skill is applied only after the main entry, and
    // its failure never fails the command (its own plan and receipt).
    let session_commands_result = companion.as_ref().map(|companion| {
        let approval = PlanApproval::from(&companion.plan);
        match action {
            MutationAction::Install => companion
                .selected
                .adapter
                .install(&manager, &approval, &receipts)
                .map(MutationResult::Install),
            MutationAction::Uninstall => companion
                .selected
                .adapter
                .uninstall(&manager, &approval, &receipts)
                .map(MutationResult::Uninstall),
        }
        .map_err(|error| IntegrationCliError::from(error).message)
    });
    let mut warnings = companion_warnings;
    warnings.extend(late_warnings);
    if let Some(Err(message)) = &session_commands_result {
        warnings.push(format!("session_commands_failed: {message}"));
    }
    for (label, outcome) in &main_agent_result {
        if let super::main_agent::StepOutcome::Failed(message) = outcome {
            warnings.push(format!("{label}_failed: {message}"));
        }
    }
    project_canonical_receipts(&receipts, environment, &mut warnings);
    if main_agent
        && std::ptr::eq(effective_plan, &plan)
        && let Scope::Project(root) = &scope
        && let Some(warning) = project_not_indexed_warning(root, environment)
    {
        warnings.push(warning);
    }
    if action == MutationAction::Install && arguments.explore_agent {
        let adapter = ExploreAgentIntegration::new(
            environment.home_dir.clone(),
            explore_agent_tools_with(false),
        );
        if adapter.folder_trust(&scope) != "accepted" {
            warnings.push(
                "explore_agent_needs_folder_trust: to let Claude Code start the agent's own lean Compresso server, open Claude Code once interactively in this project folder and accept the folder trust prompt (a parent folder's trust and `-p` sessions do not count; --strict-mcp-config, --bare, and managed MCP policies also skip the server). Until then the agent has no Compresso tools and stops; `compresso integrations status claude-code --explore-agent` shows the recorded trust".to_owned(),
            );
        }
    }
    if action == MutationAction::Uninstall
        && !arguments.explore_agent
        && !arguments.session_commands
    {
        if arguments.compaction_hook {
            if matches!(scope, Scope::User)
                && matches!(&result, MutationResult::Uninstall(report) if !report.removed_keys.is_empty())
            {
                warnings.push(format!(
                    "compaction_hook_removed_for_all_repositories: the hook in {} was user-level, so the post-compaction reminder is now off in every repository; `compresso setup` or `compresso integrations install {} --compaction-hook` installs it again",
                    selected.config_path.display(),
                    arguments.client.as_str(),
                ));
            }
        } else {
            warnings.extend(remaining_hook_warnings(
                arguments.client,
                environment,
                &scope,
                &receipts,
            ));
        }
    }
    // The user-level hook is in place; only now is the repository's legacy
    // project-level hook removed. Its outcome never fails the install.
    let migration_outcome = match (&migration, migration_root.as_deref()) {
        (Some(migration), Some(root)) => Some(apply_legacy_hook_migration(
            migration,
            arguments.client,
            root,
            environment,
        )),
        _ => None,
    };
    render_plan(
        output,
        &PlanRendering {
            action,
            client: arguments.client,
            dry_run: false,
            plan: effective_plan,
            result: Some(&result),
            warnings: &warnings,
            migration: migration.as_ref(),
            migration_outcome: migration_outcome.as_ref(),
            session_commands: companion.as_ref().map(|companion| &companion.plan),
            session_commands_result: session_commands_result
                .as_ref()
                .and_then(|result| result.as_ref().ok()),
            main_agent: &main_agent_companion,
            main_agent_result: &main_agent_result,
            fallback_entry: None,
            json: arguments.output.json,
        },
    )
}

fn write_fallback_preview(output: &mut impl Write, plan: &MutationPlan) -> std::io::Result<()> {
    writeln!(
        output,
        "# If the agent file or the \"agent\" setting cannot be written when this is applied, this entry is written instead (as --no-main-agent writes it) and the agent file is removed:"
    )?;
    output.write_all(plan.unified_diff.as_bytes())?;
    writeln!(output, "# Fallback plan hash: {}", plan.plan_hash)
}

/// A project uninstall whose entry cannot be planned (already gone, or its
/// receipt consumed by an interrupted run) still removes the agent file and
/// the key, and says what was left and why.
#[allow(clippy::too_many_arguments)]
fn uninstall_main_agent_without_entry(
    arguments: &IntegrationMutationArgs,
    environment: &IntegrationEnvironment,
    manager: &PlanManager,
    receipts: &FileReceiptStore,
    companion: &super::main_agent::MainAgentCompanion,
    mut warnings: Vec<String>,
    entry_error: &IntegrationCliError,
    output: &mut impl Write,
) -> Result<(), IntegrationCliError> {
    warnings.push(format!(
        "entry_not_removed: the compresso entry was left as it is ({}: {}); it is already absent or no receipt covers it",
        entry_error.code, entry_error.message
    ));
    let outcomes = if arguments.dry_run || !arguments.yes {
        Vec::new()
    } else {
        companion.apply(manager, receipts)
    };
    for (label, outcome) in &outcomes {
        if let super::main_agent::StepOutcome::Failed(message) = outcome {
            warnings.push(format!("{label}_failed: {message}"));
        }
    }
    if !outcomes.is_empty() {
        project_canonical_receipts(receipts, environment, &mut warnings);
    }
    if arguments.output.json {
        let mut steps = companion.to_json();
        if let Some(steps) = steps.as_array_mut() {
            for step in steps {
                let label = step["step"].as_str().unwrap_or_default().to_owned();
                step["result"] = outcomes
                    .iter()
                    .find(|(name, _)| *name == label)
                    .map_or(serde_json::Value::Null, |(_, outcome)| {
                        super::main_agent::outcome_json(outcome)
                    });
            }
        }
        serde_json::to_writer(
            &mut *output,
            &serde_json::json!({
                "schema_version": 1,
                "data": {
                    "action": "uninstall",
                    "client": arguments.client.as_str(),
                    "dry_run": arguments.dry_run,
                    "plan": null,
                    "result": null,
                    "warnings": warnings,
                    "main_agent": steps,
                }
            }),
        )?;
        writeln!(output)?;
    } else {
        companion.write_preview(output)?;
        if !arguments.dry_run && !arguments.yes {
            writeln!(output, "rerun with --yes to remove these")?;
        }
        for (label, outcome) in &outcomes {
            super::main_agent::write_outcome(output, label, outcome)?;
        }
        for warning in &warnings {
            writeln!(output, "warning: {warning}")?;
        }
    }
    if !arguments.dry_run && !arguments.yes {
        return Err(IntegrationCliError::new(
            "confirmation_required",
            "removing the main-session agent without its entry requires --yes",
        ));
    }
    Ok(())
}

/// A warning when the project root is not approved and indexed: the server
/// then exits at start and the main-session agent falls back to the client's
/// tools (E17-51 review). Runs after the install, which opens the store
/// anyway; never during a dry run.
fn project_not_indexed_warning(
    root: &Path,
    environment: &IntegrationEnvironment,
) -> Option<String> {
    let canonical = std::fs::canonicalize(root).ok()?;
    let indexed = Store::open(&environment.database_path())
        .ok()
        .and_then(|store| store.repo_by_root(&canonical).ok().flatten())
        .is_some_and(|record| record.last_indexed_at.is_some());
    (!indexed).then(|| {
        format!(
            "project_not_indexed: {} is not approved and indexed in Compresso, so the server stops at start and the agent tells you to index it; run `compresso index {}` before starting Claude Code",
            canonical.display(),
            canonical.display()
        )
    })
}

/// One warning per option the current Claude Code entry starts the server
/// with that the planned entry drops (E17-51 review): an install never keeps
/// an opt-in implicitly, so it says what goes and how to keep it. Unknown
/// flags, such as an opt-in a newer version added, are named generically.
fn removed_entry_flags(
    config_path: &Path,
    plan: &MutationPlan,
    arguments: &IntegrationMutationArgs,
) -> Vec<String> {
    // Only flag names: `--flag=VALUE` loses its value, and the argument
    // after a flag is never a name, so no value is ever printed.
    let flags = |args: &[String]| {
        args.iter()
            .filter(|arg| arg.starts_with("--"))
            .map(|arg| arg.split_once('=').map_or(arg.as_str(), |(name, _)| name))
            .filter(|name| *name != "--root")
            .map(|name| {
                name.chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' {
                            c
                        } else {
                            '?'
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let read_args = |bytes: &[u8]| {
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        serde_json::from_slice::<serde_json::Value>(bytes)
            .ok()
            .and_then(|document| {
                document["mcpServers"]["compresso"]["args"]
                    .as_array()?
                    .iter()
                    .map(|value| value.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
    };
    let Some(old) = std::fs::read(config_path)
        .ok()
        .and_then(|bytes| read_args(&bytes))
    else {
        return Vec::new();
    };
    let Some(new) = plan.target_server_args() else {
        return Vec::new();
    };
    let new = flags(&new);
    flags(&old)
        .into_iter()
        .filter(|flag| !new.contains(flag))
        .map(|flag| {
            let keep = match flag.as_str() {
                "--sql-env" => "pass --sql-env NAME again to keep it".to_owned(),
                "--edits" if arguments.no_edits => "drop --no-edits to keep it".to_owned(),
                "--edits" => "pass --edits, or install without --no-main-agent, to keep it".to_owned(),
                "--recall-source" => "drop --no-recall to keep it".to_owned(),
                "--tools" | "--session-tools" => "it belongs to the main-session agent, which this install does not install (see the other warnings)".to_owned(),
                _ => format!("re-run the install with the option that added {flag} to keep it"),
            };
            format!(
                "entry_flag_removed: the current compresso entry starts the server with {flag}; the new entry does not; {keep}"
            )
        })
        .collect()
}

/// Whether the mutation changes the plain Claude Code MCP entry, not one of
/// the separately managed files.
fn plain_entry_change(arguments: &IntegrationMutationArgs) -> bool {
    !arguments.explore_agent && !arguments.session_commands && !arguments.compaction_hook
}

/// Whether a Claude Code install adds the default main-session agent
/// (E17-51, D116): a plain project install without --no-main-agent.
fn main_agent_default(arguments: &IntegrationMutationArgs, scope: &Scope) -> bool {
    arguments.client == IntegrationClientArg::ClaudeCode
        && plain_entry_change(arguments)
        && !arguments.no_main_agent
        && matches!(scope, Scope::Project(_))
}

/// The reminder hook is user-level and shared by every repository, with its
/// own file and receipt, so removing one MCP entry never removes it. Says so,
/// with the exact command that removes it, and also names a legacy
/// project-level hook that is still in the uninstalled repository.
fn remaining_hook_warnings(
    client: IntegrationClientArg,
    environment: &IntegrationEnvironment,
    scope: &Scope,
    receipts: &FileReceiptStore,
) -> Vec<String> {
    let active = |scope: &Scope| {
        let hook = SelectedIntegration::compaction_hook(client, environment, scope).ok()?;
        receipts
            .active_for(client.as_str(), &hook.config_path)
            .ok()
            .flatten()
            .map(|_| hook.config_path)
    };
    let mut warnings = Vec::new();
    let user_hook = active(&Scope::User);
    if let Some(path) = &user_hook {
        warnings.push(format!(
            "compaction_hook_still_installed: the post-compaction routing reminder hook in {} is user-level and shared by every repository, so it is still installed; once no repository uses Compresso, remove it with `compresso integrations uninstall {} --compaction-hook`",
            path.display(),
            client.as_str(),
        ));
    }
    if let Scope::Project(root) = scope
        && let Some(path) = active(scope)
        && user_hook.as_ref() != Some(&path)
    {
        warnings.push(format!(
            "legacy_project_hook_still_installed: an earlier Compresso version installed a reminder hook in the repository file {}; remove it with `compresso integrations uninstall {} --compaction-hook --project {}`",
            path.display(),
            client.as_str(),
            root.display(),
        ));
    }
    warnings
}

/// Status and verify always inspect the user-level hook file.
fn reject_project_scoped_hook_read(
    arguments: &IntegrationReadArgs,
) -> Result<(), IntegrationCliError> {
    if arguments.compaction_hook && arguments.project.is_some() {
        return Err(IntegrationCliError::new(
            "invalid_arguments",
            "the post-compaction hook is installed in user-level configuration; run this command without --project",
        ));
    }
    Ok(())
}

fn execute_status(
    arguments: IntegrationReadArgs,
    environment: &IntegrationEnvironment,
    output: &mut impl Write,
) -> Result<(), IntegrationCliError> {
    reject_project_scoped_hook_read(&arguments)?;
    let (scope, selected) = select_for_read(&arguments, environment)?;
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let receipt = receipts.active_for(arguments.client.as_str(), &selected.config_path)?;
    let active_receipt = receipt.is_some();
    if arguments.compaction_hook || arguments.explore_agent || arguments.session_commands {
        let agent = arguments.explore_agent.then(|| {
            let adapter = ExploreAgentIntegration::new(
                environment.home_dir.clone(),
                explore_agent_tools_with(false),
            )
            .with_launcher(environment.compresso_bin.clone());
            let command = adapter.agent_command(&scope);
            AgentServerStatus {
                server: adapter.agent_server(&scope),
                folder_trust: adapter.folder_trust(&scope),
                other_launcher: command
                    .as_deref()
                    .is_some_and(|command| adapter.is_other_launcher(command)),
                command,
            }
        });
        return render_hook_status(
            output,
            &arguments,
            &selected,
            active_receipt,
            selected.adapter.verify(&scope).verified,
            agent,
        );
    }
    let detection = selected.adapter.detect(&scope);
    // A Claude Code project install also writes the session command skill.
    let session_commands = (arguments.client == IntegrationClientArg::ClaudeCode
        && matches!(scope, Scope::Project(_)))
    .then(|| {
        // (installed by Compresso, current with this version)
        SelectedIntegration::session_commands(arguments.client, environment, &scope).map_or(
            (false, false),
            |skill| {
                let report = skill.adapter.verify(&scope);
                let passed = |name: &str| {
                    report
                        .checks
                        .iter()
                        .any(|check| check.name == name && check.passed)
                };
                (
                    passed("session_commands_managed"),
                    passed("session_commands_current"),
                )
            },
        )
    });
    let server = mcp_server_health(
        arguments.client,
        &selected.config_path,
        &scope,
        receipt.as_ref(),
        environment.mcp_probe_timeout,
    );
    if arguments.output.json {
        serde_json::to_writer(&mut *output, &{
            let mut document = serde_json::json!({
                "schema_version": 1,
                "data": {
                    "action": "status",
                    "client": arguments.client.as_str(),
                    "config_path": selected.config_path,
                    "detection": detection,
                    "canonical_receipt_active": active_receipt,
                    "mcp_server": server.to_json(),
                }
            });
            if let Some((installed, current)) = session_commands {
                document["data"]["session_commands_installed"] = installed.into();
                document["data"]["session_commands_current"] = current.into();
            }
            document
        })?;
        writeln!(output)?;
    } else {
        writeln!(output, "client: {}", arguments.client.as_str())?;
        writeln!(output, "config: {}", selected.config_path.display())?;
        writeln!(output, "detection: {}", detection_label(&detection))?;
        writeln!(
            output,
            "canonical receipt: {}",
            if active_receipt { "active" } else { "none" }
        )?;
        writeln!(
            output,
            "mcp server: {} ({})",
            server.state.code(),
            server.detail
        )?;
        if let Some((installed, current)) = session_commands {
            writeln!(
                output,
                "session commands: {}",
                match (installed, current) {
                    (true, true) => "installed (/compresso)",
                    (true, false) =>
                        "installed, out of date; re-install to update (`compresso integrations install claude-code --project DIR`)",
                    _ =>
                        "not installed (`compresso integrations install claude-code --session-commands --project DIR`)",
                }
            )?;
        }
        match server.state.severity() {
            Severity::Ok => {}
            Severity::Warning => writeln!(output, "warning: {}", server.fix)?,
            Severity::Error => writeln!(output, "fix: {}", server.fix)?,
        }
    }
    Ok(())
}

/// Where the exploration agent's Compresso tools come from (E17-38, D103).
struct AgentServerStatus {
    /// `lean`, `shared_full_profile`, `absent`, or `unknown`.
    server: &'static str,
    /// Advisory: `accepted`, `not_recorded`, `unknown`, or `not_required`.
    folder_trust: &'static str,
    /// The command the agent file's inline server runs.
    command: Option<String>,
    /// That command is not this Compresso's launcher.
    other_launcher: bool,
}

impl AgentServerStatus {
    /// What to do about the agent's server, if anything.
    fn advice(&self) -> Option<&'static str> {
        match (self.server, self.folder_trust) {
            ("shared_full_profile", _) => Some(
                "the agent file predates its own lean server and runs on the shared full-profile server; reinstall it with `compresso integrations install claude-code --explore-agent`",
            ),
            ("lean", _) if self.other_launcher => Some(
                "the agent's server runs a command that is not this Compresso launcher; reinstall it with `compresso integrations install claude-code --explore-agent` to point it here",
            ),
            ("lean", "not_recorded" | "unknown") => Some(
                "open Claude Code once interactively in this project folder and accept the folder trust prompt, so it can start the agent's own lean server (a parent folder's trust and `-p` sessions do not count; --strict-mcp-config, --bare, and managed MCP policies also skip it); until then the agent has no Compresso tools and stops",
            ),
            _ => None,
        }
    }
}

fn render_hook_status(
    output: &mut impl Write,
    arguments: &IntegrationReadArgs,
    selected: &SelectedIntegration,
    active_receipt: bool,
    installed: bool,
    agent: Option<AgentServerStatus>,
) -> Result<(), IntegrationCliError> {
    if arguments.output.json {
        let (surface, installed_key) = if arguments.explore_agent {
            ("explore_agent", "agent_installed")
        } else if arguments.session_commands {
            ("session_commands", "session_commands_installed")
        } else {
            ("compaction_hook", "hook_installed")
        };
        let mut data = serde_json::json!({
            "action": "status",
            "surface": surface,
            "client": arguments.client.as_str(),
            "config_path": selected.config_path,
            "canonical_receipt_active": active_receipt,
        });
        data[installed_key] = serde_json::Value::Bool(installed);
        if let Some(agent) = &agent {
            data["agent_server"] = agent.server.into();
            data["folder_trust"] = agent.folder_trust.into();
            if let Some(command) = &agent.command {
                data["agent_command"] = command.as_str().into();
            }
            if let Some(advice) = agent.advice() {
                data["advice"] = advice.into();
            }
        }
        write_json_escaped(
            output,
            &serde_json::json!({ "schema_version": 1, "data": data }),
        )?;
    } else {
        writeln!(output, "client: {}", arguments.client.as_str())?;
        writeln!(
            output,
            "surface: {}",
            if arguments.explore_agent {
                "exploration agent definition"
            } else if arguments.session_commands {
                "session command skill (/compresso)"
            } else {
                "post-compaction routing-reminder hook"
            }
        )?;
        writeln!(output, "config: {}", selected.config_path.display())?;
        writeln!(
            output,
            "{}: {}",
            if arguments.explore_agent {
                "agent"
            } else if arguments.session_commands {
                "session commands"
            } else {
                "hook"
            },
            if installed {
                "installed"
            } else {
                "not installed"
            }
        )?;
        writeln!(
            output,
            "canonical receipt: {}",
            if active_receipt { "active" } else { "none" }
        )?;
        if let Some(agent) = &agent {
            writeln!(output, "agent server: {}", agent.server)?;
            if let Some(command) = &agent.command {
                // Repository content: never written to the terminal raw.
                writeln!(output, "agent command: {}", printable(command))?;
            }
            writeln!(output, "folder trust: {} (advisory)", agent.folder_trust)?;
            if let Some(advice) = agent.advice() {
                writeln!(output, "warning: {advice}")?;
            }
        }
    }
    Ok(())
}

/// Reports whether the server entry in the selected config starts and answers.
fn mcp_server_health(
    client: IntegrationClientArg,
    config_path: &Path,
    scope: &Scope,
    receipt: Option<&InstallReceipt>,
    probe_timeout: Option<Duration>,
) -> McpServerHealth {
    let format = match client {
        IntegrationClientArg::Codex => ConfigFormat::Toml,
        _ => ConfigFormat::Json,
    };
    let mut args = mcp_probe::user_scope_args();
    if let Scope::Project(root) = scope {
        // Adapters pin the canonical project root; an unresolvable root can
        // never match and is reported as a modified entry.
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        args.extend(["--root".to_owned(), root.to_string_lossy().into_owned()]);
    }
    // The configuration may sit inside an untrusted repository. Its command
    // is started only when it is the running executable or when an
    // authenticated receipt proves Compresso wrote this exact entry.
    let running_executable = env::current_exe().ok();
    let trust = LauncherTrust::new(
        running_executable.as_deref(),
        receipt.is_some_and(|receipt| receipt_authenticates_server(receipt, format, config_path)),
    );
    mcp_probe::assess(
        &read_configured_server(format, config_path, &args),
        client.as_str(),
        &trust,
        probe_timeout,
    )
}

fn execute_verify(
    arguments: IntegrationReadArgs,
    environment: &IntegrationEnvironment,
    output: &mut impl Write,
) -> Result<(), IntegrationCliError> {
    reject_project_scoped_hook_read(&arguments)?;
    let (scope, selected) = select_for_read(&arguments, environment)?;
    let mut report = selected.adapter.verify(&scope);
    if arguments.client == IntegrationClientArg::ClaudeCode
        && !arguments.compaction_hook
        && !arguments.explore_agent
        && !arguments.session_commands
    {
        // The main-session agent checks (E17-51) apply when its file or its
        // setting is present in the project.
        if matches!(scope, Scope::Project(_)) {
            let entry_args = std::fs::read(&selected.config_path)
                .ok()
                .and_then(|bytes| {
                    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
                    serde_json::from_slice::<serde_json::Value>(bytes).ok()
                })
                .and_then(|document| {
                    document["mcpServers"]["compresso"]["args"]
                        .as_array()?
                        .iter()
                        .map(|value| value.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                });
            report.checks.extend(super::main_agent::verify_checks(
                &environment.home_dir,
                &scope,
                entry_args.as_deref(),
            ));
        }
        report.checks.push(VerificationCheck {
            name: "live_client_evidence".to_owned(),
            passed: false,
            detail: "live Claude Code verification evidence is unavailable at this process-free boundary"
                .to_owned(),
        });
        report.verified = false;
    }
    render_verification(output, &report, arguments.output.json)?;
    if report.verified {
        Ok(())
    } else {
        Err(IntegrationCliError::new(
            "verification_failed",
            "one or more integration verification checks failed",
        ))
    }
}

struct SelectedIntegration {
    adapter: Box<dyn Integration>,
    config_path: PathBuf,
}

impl SelectedIntegration {
    fn new(
        client: IntegrationClientArg,
        environment: &IntegrationEnvironment,
        scope: &Scope,
        compaction_hook: bool,
    ) -> Result<Self, IntegrationCliError> {
        if compaction_hook {
            return Self::compaction_hook(client, environment, scope);
        }
        match client {
            IntegrationClientArg::ClaudeDesktop => Err(IntegrationCliError::new(
                "unsupported",
                "Claude Desktop integration is excluded by decision D5",
            )),
            IntegrationClientArg::ClaudeCode => {
                let adapter = ClaudeCodeIntegration::new(
                    environment.home_dir.clone(),
                    ClaudeCodeDiscovery::new(environment.path_entries.clone(), None),
                );
                let config_path = adapter.config_path(scope)?;
                Ok(Self {
                    adapter: Box::new(adapter),
                    config_path,
                })
            }
            IntegrationClientArg::Codex => {
                let adapter = CodexIntegration::new(
                    environment.home_dir.clone(),
                    environment.path_entries.clone(),
                    None,
                    env!("CARGO_PKG_VERSION"),
                )?;
                if matches!(scope, Scope::Project(_)) {
                    return Err(IntegrationCliError::new(
                        "unsupported",
                        "Codex supports only user scope",
                    ));
                }
                // Recovery runs before plan creation, so the CLI must validate
                // the Codex target here instead of relying on the adapter's
                // later plan-time check.
                let config_path = adapter.checked_config_path()?;
                Ok(Self {
                    adapter: Box::new(adapter),
                    config_path,
                })
            }
            IntegrationClientArg::Cursor => {
                let applications = known_cursor_applications(environment);
                let cli_candidates = cursor_cli_candidates(&environment.path_entries);
                let adapter = CursorIntegration::new(
                    environment.home_dir.clone(),
                    CursorDiscovery::current(cli_candidates, applications),
                );
                let config_path = adapter.config_path(scope)?;
                Ok(Self {
                    adapter: Box::new(adapter),
                    config_path,
                })
            }
        }
    }
}

impl SelectedIntegration {
    /// The Claude Code MCP adapter writing an entry that starts
    /// `mcp serve` with `--edits` (E17-32, D97) and/or `--sql-env NAME`
    /// (E17-50, D115).
    fn claude_code_with_operator_tools(
        environment: &IntegrationEnvironment,
        scope: &Scope,
        edits: bool,
        sql_env: Option<String>,
        lean_session: bool,
    ) -> Result<Self, IntegrationCliError> {
        let adapter = ClaudeCodeIntegration::new(
            environment.home_dir.clone(),
            ClaudeCodeDiscovery::new(environment.path_entries.clone(), None),
        )
        .with_edits(edits)
        .with_sql_env(sql_env)
        .with_lean_session(lean_session);
        let config_path = adapter.config_path(scope)?;
        Ok(Self {
            adapter: Box::new(adapter),
            config_path,
        })
    }
}

impl SelectedIntegration {
    /// Selects the post-compaction routing-reminder hook adapter.
    fn compaction_hook(
        client: IntegrationClientArg,
        environment: &IntegrationEnvironment,
        scope: &Scope,
    ) -> Result<Self, IntegrationCliError> {
        let adapter = CompactionHookIntegration::new(
            compaction_hook_client(client)?,
            environment.home_dir.clone(),
        );
        let config_path = adapter.config_path(scope)?;
        Ok(Self {
            adapter: Box::new(adapter),
            config_path,
        })
    }
}

impl SelectedIntegration {
    /// Selects the opt-in Claude Code exploration agent adapter. Its tool list
    /// is the lean MCP exploration allowlist, so the agent and
    /// `mcp serve --tools lean` share one source of truth.
    fn explore_agent(
        client: IntegrationClientArg,
        environment: &IntegrationEnvironment,
        scope: &Scope,
        edits: bool,
    ) -> Result<Self, IntegrationCliError> {
        if client != IntegrationClientArg::ClaudeCode {
            return Err(IntegrationCliError::new(
                "unsupported",
                "the exploration agent definition is a Claude Code subagent; use claude-code",
            ));
        }
        let adapter = ExploreAgentIntegration::new(
            environment.home_dir.clone(),
            explore_agent_tools_with(edits),
        )
        .with_edits(edits)
        .with_launcher(environment.compresso_bin.clone());
        let config_path = adapter.config_path(scope)?;
        Ok(Self {
            adapter: Box::new(adapter),
            config_path,
        })
    }
}

impl SelectedIntegration {
    /// Selects the Claude Code in-session command skill adapter (E17-48).
    fn session_commands(
        client: IntegrationClientArg,
        environment: &IntegrationEnvironment,
        scope: &Scope,
    ) -> Result<Self, IntegrationCliError> {
        if client != IntegrationClientArg::ClaudeCode {
            return Err(IntegrationCliError::new(
                "unsupported",
                "the session commands are a Claude Code project skill; use claude-code",
            ));
        }
        let adapter = SessionCommandsIntegration::new(environment.home_dir.clone());
        let config_path = adapter.config_path(scope)?;
        Ok(Self {
            adapter: Box::new(adapter),
            config_path,
        })
    }
}

/// The in-session command skill planned with a Claude Code MCP entry install
/// or uninstall (E17-48, D113): its own plan and receipt beside the entry's.
struct SessionCommandsCompanion {
    selected: SelectedIntegration,
    plan: MutationPlan,
}

/// Plans the session command skill that goes with a plain Claude Code
/// install (unless `--no-session-commands`) or uninstall (when its receipt is
/// active). It is a project file: a user-scope install adds a note instead.
/// Any refusal, such as a user file of the same name, becomes a warning and
/// never stops the MCP entry change.
#[allow(clippy::too_many_arguments)]
fn plan_session_commands_companion(
    action: MutationAction,
    arguments: &IntegrationMutationArgs,
    environment: &IntegrationEnvironment,
    scope: &Scope,
    manager: &PlanManager,
    receipts: &FileReceiptStore,
    launcher_path: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Option<SessionCommandsCompanion> {
    if arguments.client != IntegrationClientArg::ClaudeCode
        || arguments.explore_agent
        || arguments.compaction_hook
        || arguments.session_commands
        || arguments.no_session_commands
    {
        return None;
    }
    let install = action == MutationAction::Install;
    if !matches!(scope, Scope::Project(_)) {
        if install {
            warnings.push(
                "session_commands_not_installed: the /compresso session commands are a project skill; install with --project DIR to add them to a repository".to_owned(),
            );
        }
        return None;
    }
    let mut skipped = |message: String| {
        if install {
            warnings.push(format!(
                "session_commands_skipped: {message}; the MCP entry is installed without /compresso"
            ));
        }
    };
    let selected = match SelectedIntegration::session_commands(arguments.client, environment, scope)
    {
        Ok(selected) => selected,
        Err(error) => {
            skipped(error.message);
            return None;
        }
    };
    if !arguments.dry_run
        && let Err(error) = manager.recover(&selected.config_path, receipts)
    {
        skipped(IntegrationCliError::from(error).message);
        return None;
    }
    let plan = match action {
        MutationAction::Install => selected.adapter.plan_install(
            manager,
            &InstallCtx {
                compresso_bin: launcher_path?.to_path_buf(),
                scope: scope.clone(),
                session_recall: !arguments.no_recall,
            },
        ),
        MutationAction::Uninstall => {
            let receipt = receipts
                .active_for(arguments.client.as_str(), &selected.config_path)
                .ok()
                .flatten()?;
            selected.adapter.plan_uninstall(manager, &receipt)
        }
    };
    match plan {
        Ok(plan) => Some(SessionCommandsCompanion { selected, plan }),
        Err(IntegrationError::UserFilePresent { path }) => {
            skipped(format!(
                "{} is a file Compresso did not write, so it is left unchanged",
                path.display()
            ));
            None
        }
        Err(error) => {
            if install {
                skipped(IntegrationCliError::from(error).message);
            } else {
                warnings.push(format!(
                    "session_commands_not_removed: {}; remove it with `compresso integrations uninstall claude-code --session-commands`",
                    IntegrationCliError::from(error).message
                ));
            }
            None
        }
    }
}

fn write_session_commands_preview(
    output: &mut impl Write,
    plan: &MutationPlan,
) -> Result<(), IntegrationCliError> {
    output.write_all(plan.unified_diff.as_bytes())?;
    writeln!(output, "# Session commands plan hash: {}", plan.plan_hash)?;
    Ok(())
}

/// Compresso tools the exploration agent may call: the lean allowlist without
/// `compresso_recall`, which a server serves only with `--recall-source`, and
/// without `compresso_edit` unless the operator opted into edits.
pub(crate) fn explore_agent_tools_with(edits: bool) -> Vec<String> {
    compresso_mcp::lean_tool_allowlist()
        .into_iter()
        .filter(|name| *name != "compresso_recall")
        .filter(|name| edits || *name != compresso_mcp::edit::TOOL_NAME)
        .map(str::to_owned)
        .collect()
}

/// The exploration agent is project-scoped: `--project`, or else the working
/// directory.
fn explore_agent_scope(project: Option<&Path>, cwd: &Path) -> Result<Scope, IntegrationCliError> {
    resolve_scope(Some(project.unwrap_or(cwd)), cwd)
}

fn select_for_read(
    arguments: &IntegrationReadArgs,
    environment: &IntegrationEnvironment,
) -> Result<(Scope, SelectedIntegration), IntegrationCliError> {
    if arguments.explore_agent {
        let scope = explore_agent_scope(arguments.project.as_deref(), &environment.cwd)?;
        let selected =
            SelectedIntegration::explore_agent(arguments.client, environment, &scope, false)?;
        return Ok((scope, selected));
    }
    if arguments.session_commands {
        let scope = explore_agent_scope(arguments.project.as_deref(), &environment.cwd)?;
        let selected =
            SelectedIntegration::session_commands(arguments.client, environment, &scope)?;
        return Ok((scope, selected));
    }
    let scope = resolve_scope(arguments.project.as_deref(), &environment.cwd)?;
    let selected = SelectedIntegration::new(
        arguments.client,
        environment,
        &scope,
        arguments.compaction_hook,
    )?;
    Ok((scope, selected))
}

fn compaction_hook_client(
    client: IntegrationClientArg,
) -> Result<CompactionHookClient, IntegrationCliError> {
    match client {
        IntegrationClientArg::ClaudeCode => Ok(CompactionHookClient::ClaudeCode),
        IntegrationClientArg::Codex => Ok(CompactionHookClient::Codex),
        IntegrationClientArg::Cursor => Err(IntegrationCliError::new(
            "unsupported",
            "Cursor has no post-compaction hook that can add model context (its preCompact hook is observational); Compresso routing guidance there comes only from the MCP server instructions",
        )),
        IntegrationClientArg::ClaudeDesktop => Err(IntegrationCliError::new(
            "unsupported",
            "Claude Desktop integration is excluded by decision D5",
        )),
    }
}

fn cursor_cli_candidates(path_entries: &[PathBuf]) -> Vec<CursorCliEvidence> {
    let names: &[&str] = if cfg!(windows) {
        &["cursor.exe", "cursor.cmd", "cursor.bat", "cursor"]
    } else {
        &["cursor"]
    };
    path_entries
        .iter()
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .take(64)
        // Empty evidence is deliberate: filesystem discovery remains honest,
        // while version-dependent status stays Unsupported until a trusted
        // process-capable boundary supplies `cursor --version` output.
        .map(|path| CursorCliEvidence::new(path, ""))
        .collect()
}

fn known_cursor_applications(
    environment: &IntegrationEnvironment,
) -> Vec<CursorApplicationEvidence> {
    if cfg!(target_os = "macos") {
        vec![CursorApplicationEvidence::new(
            "/Applications/Cursor.app",
            None,
        )]
    } else if cfg!(target_os = "windows") {
        vec![CursorApplicationEvidence::new(
            environment
                .home_dir
                .join("AppData/Local/Programs/cursor/Cursor.exe"),
            None,
        )]
    } else {
        Vec::new()
    }
}

enum MutationResult {
    Install(InstallReceipt),
    Uninstall(compresso_integrations::RestoreReport),
}

struct PlanRendering<'a> {
    action: MutationAction,
    client: IntegrationClientArg,
    dry_run: bool,
    plan: &'a MutationPlan,
    result: Option<&'a MutationResult>,
    warnings: &'a [String],
    /// A legacy project-level reminder hook this install migrates away.
    migration: Option<&'a LegacyHookMigration>,
    migration_outcome: Option<&'a LegacyHookOutcome>,
    /// The in-session command skill changed with the MCP entry (E17-48).
    session_commands: Option<&'a MutationPlan>,
    session_commands_result: Option<&'a MutationResult>,
    /// The main-session agent changed with the MCP entry (E17-51).
    main_agent: &'a super::main_agent::MainAgentCompanion,
    main_agent_result: &'a [(&'static str, super::main_agent::StepOutcome)],
    /// The entry written instead when the agent cannot be (previews only).
    fallback_entry: Option<&'a MutationPlan>,
    json: bool,
}

fn outcome_json(result: Option<&MutationResult>) -> Result<serde_json::Value, IntegrationCliError> {
    Ok(match result {
        Some(MutationResult::Install(receipt)) => serde_json::json!({
            "installed": true,
            "plan_hash": receipt.plan_hash,
        }),
        Some(MutationResult::Uninstall(report)) => serde_json::to_value(report)?,
        None => serde_json::Value::Null,
    })
}

fn render_plan(
    output: &mut impl Write,
    rendering: &PlanRendering<'_>,
) -> Result<(), IntegrationCliError> {
    if rendering.json {
        let outcome = outcome_json(rendering.result)?;
        let mut document = serde_json::json!({
            "schema_version": 1,
            "data": {
                "action": rendering.action.as_str(),
                "client": rendering.client.as_str(),
                "dry_run": rendering.dry_run,
                "plan": rendering.plan.public_preview(),
                "result": outcome,
                "warnings": rendering.warnings,
            }
        });
        if let Some(migration) = rendering.migration {
            document["data"]["legacy_project_hook"] = serde_json::json!({
                "plan": migration.to_json(),
                "result": rendering.migration_outcome.map(LegacyHookOutcome::to_json),
            });
        }
        if let Some(plan) = rendering.session_commands {
            document["data"]["session_commands"] = serde_json::json!({
                "plan": plan.public_preview(),
                "result": outcome_json(rendering.session_commands_result)?,
            });
        }
        if !rendering.main_agent.is_empty() {
            let mut steps = rendering.main_agent.to_json();
            if let Some(steps) = steps.as_array_mut() {
                for step in steps {
                    let label = step["step"].as_str().unwrap_or_default().to_owned();
                    step["result"] = rendering
                        .main_agent_result
                        .iter()
                        .find(|(name, _)| *name == label)
                        .map_or(serde_json::Value::Null, |(_, outcome)| {
                            super::main_agent::outcome_json(outcome)
                        });
                }
            }
            document["data"]["main_agent"] = steps;
        }
        if let Some(plan) = rendering.fallback_entry {
            document["data"]["fallback_entry"] =
                serde_json::json!({ "plan": plan.public_preview() });
        }
        serde_json::to_writer(&mut *output, &document)?;
        writeln!(output)?;
    } else if rendering.dry_run {
        output.write_all(rendering.plan.unified_diff.as_bytes())?;
        writeln!(output, "# Plan hash: {}", rendering.plan.plan_hash)?;
        if let Some(migration) = rendering.migration {
            migration.write_preview(output)?;
        }
        if let Some(plan) = rendering.session_commands {
            write_session_commands_preview(output, plan)?;
        }
        rendering.main_agent.write_preview(output)?;
        if let Some(plan) = rendering.fallback_entry {
            write_fallback_preview(output, plan)?;
        }
        for warning in rendering.warnings {
            writeln!(output, "warning: {warning}")?;
        }
    } else {
        writeln!(
            output,
            "{} complete for {}",
            rendering.action.as_str(),
            rendering.client.as_str()
        )?;
        for warning in rendering.warnings {
            writeln!(output, "warning: {warning}")?;
        }
        if let Some(outcome) = rendering.migration_outcome {
            writeln!(output, "{}", outcome.human_line())?;
        }
        match rendering.session_commands_result {
            Some(MutationResult::Install(_)) => writeln!(
                output,
                "session commands: installed; type /compresso in a Claude Code session in this project"
            )?,
            Some(MutationResult::Uninstall(report)) => {
                writeln!(output, "session commands: removed")?;
                for warning in &report.warnings {
                    writeln!(output, "warning: {warning}")?;
                }
            }
            None => {}
        }
        for (label, outcome) in rendering.main_agent_result {
            super::main_agent::write_outcome(output, label, outcome)?;
        }
        if let Some(MutationResult::Uninstall(report)) = rendering.result {
            for warning in &report.warnings {
                writeln!(output, "warning: {warning}")?;
            }
        }
    }
    Ok(())
}

fn render_verification(
    output: &mut impl Write,
    report: &VerificationReport,
    json: bool,
) -> Result<(), IntegrationCliError> {
    if json {
        write_json_escaped(
            output,
            &serde_json::json!({ "schema_version": 1, "data": report }),
        )?;
    } else {
        writeln!(output, "verified: {}", report.verified)?;
        // A detail may quote a configuration file's content.
        for check in &report.checks {
            writeln!(
                output,
                "{}: {} ({})",
                printable(&check.name),
                if check.passed { "pass" } else { "fail" },
                printable(&check.detail)
            )?;
        }
    }
    Ok(())
}

/// Writes `value` as one JSON line with DEL and C1 controls escaped too
/// (`serde_json` escapes only C0), so a value read from a repository file
/// cannot drive the terminal; the JSON still parses to the same value.
fn write_json_escaped(
    output: &mut impl Write,
    value: &serde_json::Value,
) -> Result<(), IntegrationCliError> {
    let text = serde_json::to_string(value)?;
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() {
            // Only inside strings: JSON has no raw controls elsewhere.
            escaped.push_str(&format!("\\u{:04x}", u32::from(character)));
        } else {
            escaped.push(character);
        }
    }
    writeln!(output, "{escaped}")?;
    Ok(())
}

fn project_canonical_receipts(
    receipts: &FileReceiptStore,
    environment: &IntegrationEnvironment,
    warnings: &mut Vec<String>,
) {
    let result = (|| {
        let canonical = receipts.all()?;
        let store =
            Store::open(&environment.database_path()).map_err(|_| ProjectionError::Store)?;
        for receipt in canonical {
            project_receipt(&store, &receipt).map_err(|_| ProjectionError::Store)?;
        }
        Ok::<(), ProjectionError>(())
    })();
    if result.is_err() {
        warnings.push("sqlite_projection_failed".to_owned());
    }
}

fn project_receipt(store: &Store, receipt: &InstallReceipt) -> Result<(), ()> {
    let config_path = receipt.config_path.to_str().ok_or(())?.to_owned();
    let mut projection = ReceiptRecord {
        id: None,
        client: receipt.client.clone(),
        config_path,
        keys_owned: receipt.keys_owned.clone(),
        backup_path: receipt.backup_path.clone(),
        installed_at: receipt.installed_at,
        compresso_version: receipt.compresso_version.clone(),
        uninstalled_at: receipt.uninstalled_at,
    };
    let existing = store.receipts(Some(&receipt.client)).map_err(|_| ())?;
    if let Some(stored) = existing.into_iter().find(|stored| {
        stored.config_path == projection.config_path
            && stored.backup_path == projection.backup_path
            && stored.installed_at == projection.installed_at
            && stored.compresso_version == projection.compresso_version
    }) {
        projection.id = stored.id;
    }
    store.save_receipt(&projection).map_err(|_| ())
}

enum ProjectionError {
    Store,
    Receipt,
}

impl From<compresso_integrations::ReceiptError> for ProjectionError {
    fn from(_: compresso_integrations::ReceiptError) -> Self {
        Self::Receipt
    }
}

enum Confirmation {
    Approved,
    Declined,
    Unavailable,
}

fn read_confirmation(input: &mut dyn Read) -> Result<Confirmation, std::io::Error> {
    let mut bytes = Vec::new();
    while bytes.len() < 64 {
        let mut byte = [0_u8; 1];
        match input.read(&mut byte)? {
            0 => break,
            1 => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            _ => unreachable!("one-byte read returned more than one byte"),
        }
    }
    if bytes.is_empty() {
        return Ok(Confirmation::Unavailable);
    }
    let first_line = String::from_utf8_lossy(&bytes);
    let answer = first_line.lines().next().unwrap_or_default().trim();
    if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        Ok(Confirmation::Approved)
    } else {
        Ok(Confirmation::Declined)
    }
}

fn resolve_scope(project: Option<&Path>, cwd: &Path) -> Result<Scope, IntegrationCliError> {
    let Some(project) = project else {
        return Ok(Scope::User);
    };
    let path = if project.is_absolute() {
        project.to_path_buf()
    } else {
        cwd.join(project)
    };
    let canonical = std::fs::canonicalize(&path).map_err(|_| {
        IntegrationCliError::new(
            "invalid_arguments",
            "project scope must identify an existing directory",
        )
    })?;
    if !canonical.is_dir() {
        return Err(IntegrationCliError::new(
            "invalid_arguments",
            "project scope must identify an existing directory",
        ));
    }
    Ok(Scope::Project(canonical))
}

fn detection_label(detection: &Detection) -> String {
    match detection {
        Detection::Installed { version, .. } => format!("installed ({version})"),
        Detection::NotFound => "not_found".to_owned(),
        Detection::Unsupported { reason } => format!("unsupported ({reason})"),
    }
}

pub(super) fn system_home_dir() -> Result<PathBuf, IntegrationCliError> {
    super::recall::system_home_dir().ok_or_else(|| {
        IntegrationCliError::environment("failed to resolve the user home directory")
    })
}

#[derive(Debug)]
pub(crate) struct IntegrationCliError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    /// The hook group a user can add by hand when install was refused because
    /// the client already defines its own hooks for the owned event.
    pub(crate) manual_hook_entry: Option<String>,
}

impl IntegrationCliError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            manual_hook_entry: None,
        }
    }

    fn environment(message: impl Into<String>) -> Self {
        Self::new("environment", message)
    }
}

impl From<IntegrationError> for IntegrationCliError {
    fn from(error: IntegrationError) -> Self {
        let manual_hook_entry = match &error {
            IntegrationError::UserHooksPresent { entry, .. } => Some(entry.clone()),
            _ => None,
        };
        let code = match error {
            IntegrationError::Unsupported { .. } | IntegrationError::UserHooksPresent { .. } => {
                "unsupported"
            }
            IntegrationError::UserFilePresent { .. } => "user_file_present",
            IntegrationError::ConfigDrift { .. } => "config_drift",
            IntegrationError::PendingRecovery { .. } => "pending_recovery",
            IntegrationError::PlanExpired { .. } => "plan_expired",
            IntegrationError::BackupCollision { .. } => "backup_collision",
            IntegrationError::UninstallReceiptMissing { .. }
            | IntegrationError::UninstallReceiptMismatch { .. }
            | IntegrationError::UninstallReceiptNotActive { .. }
            | IntegrationError::UninstallReceiptNotIdempotent { .. } => "receipt_invalid",
            _ => "integration_error",
        };
        Self {
            manual_hook_entry,
            ..Self::new(code, error.to_string())
        }
    }
}

impl From<compresso_integrations::ReceiptError> for IntegrationCliError {
    fn from(error: compresso_integrations::ReceiptError) -> Self {
        Self::new("receipt_invalid", error.to_string())
    }
}

impl From<std::io::Error> for IntegrationCliError {
    fn from(error: std::io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}

impl From<serde_json::Error> for IntegrationCliError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("internal", error.to_string())
    }
}

#[cfg(test)]
mod launcher_tests {
    use std::fs;

    use tempfile::tempdir;

    use super::validate_stable_launcher;

    #[test]
    fn test_stable_launcher_path_is_not_canonicalized() {
        let temporary = tempdir().expect("temporary directory should be created");
        let version_one = temporary.path().join("compresso-v1");
        let version_two = temporary.path().join("compresso-v2");
        make_executable(&version_one);
        make_executable(&version_two);
        let launcher = temporary.path().join("compresso");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&version_one, &launcher)
            .expect("first launcher should be linked");
        #[cfg(windows)]
        fs::copy(&version_one, &launcher).expect("first launcher should be copied");

        assert_eq!(
            validate_stable_launcher(launcher.clone()).expect("launcher should be accepted"),
            launcher
        );

        fs::remove_file(&launcher).expect("first launcher should be removed");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&version_two, &launcher)
            .expect("second launcher should be linked");
        #[cfg(windows)]
        fs::copy(&version_two, &launcher).expect("second launcher should be copied");

        assert_eq!(
            validate_stable_launcher(launcher.clone())
                .expect("upgraded launcher should be accepted"),
            launcher
        );
    }

    #[test]
    fn test_stable_launcher_rejects_relative_missing_and_directory_paths() {
        let temporary = tempdir().expect("temporary directory should be created");
        assert!(validate_stable_launcher("compresso".into()).is_err());
        assert!(validate_stable_launcher(temporary.path().join("missing-compresso")).is_err());
        assert!(validate_stable_launcher(temporary.path().to_path_buf()).is_err());
        assert!(
            validate_stable_launcher(
                temporary
                    .path()
                    .join("not-allowed")
                    .join("..")
                    .join("compresso")
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_stable_launcher_normalizes_dot_and_duplicate_separators_without_resolving_shim() {
        let temporary = tempdir().expect("temporary directory should be created");
        let launcher_directory = temporary.path().join("launchers");
        fs::create_dir(&launcher_directory).expect("launcher directory should be created");
        let payload = temporary.path().join("compresso-v1");
        make_executable(&payload);
        let launcher = launcher_directory.join("compresso");
        std::os::unix::fs::symlink(&payload, &launcher).expect("launcher should be linked");
        let raw = std::path::PathBuf::from(format!(
            "{}/./launchers//compresso",
            temporary.path().display()
        ));

        assert_eq!(
            validate_stable_launcher(raw).expect("launcher should be accepted"),
            launcher
        );
    }

    fn make_executable(path: &std::path::Path) {
        fs::write(path, b"compresso launcher fixture").expect("launcher fixture should be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                .expect("launcher fixture should be executable");
        }
    }
}
