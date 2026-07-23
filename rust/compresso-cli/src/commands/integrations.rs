use std::env;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use compresso_core::RuntimePaths;
use compresso_integrations::{
    ClaudeCodeDiscovery, ClaudeCodeIntegration, CodexIntegration, CursorApplicationEvidence,
    CursorCliEvidence, CursorDiscovery, CursorIntegration, DeferredInstallAuthorization, Detection,
    FileReceiptStore, InstallCtx, InstallReceipt, Integration, IntegrationError, MutationPlan,
    PlanApproval, PlanClock, PlanManager, ReceiptStore, Scope, VerificationCheck,
    VerificationReport,
};
use compresso_store::{ReceiptRecord, Store};

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
            data_dir,
            path_entries,
            session_id: session_id.into(),
        }
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
        Ok(Self::new(
            home_dir,
            cwd.to_path_buf(),
            compresso_bin,
            data_dir,
            path_entries,
            session_id,
        ))
    }

    fn receipt_store_path(&self) -> PathBuf {
        self.data_dir.join(RECEIPT_STORE_NAME)
    }

    fn database_path(&self) -> PathBuf {
        self.data_dir.join("compresso.db")
    }
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
        let current = SelectedIntegration::new(self.client, environment, &self.scope)?;
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
    environment: &IntegrationEnvironment,
    clock: Arc<dyn PlanClock>,
) -> Result<PreparedSetupInstall, IntegrationCliError> {
    let scope = resolve_scope(project, &environment.cwd)?;
    let launcher_path = validate_stable_launcher(environment.compresso_bin.clone())?;
    let selected = SelectedIntegration::new(client, environment, &scope)?;
    let manager = PlanManager::with_clock(&environment.session_id, clock)?;
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let plan = selected.adapter.plan_install(
        &manager,
        &InstallCtx {
            compresso_bin: launcher_path.clone(),
            scope: scope.clone(),
        },
    )?;
    Ok(PreparedSetupInstall {
        client,
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
    let scope = resolve_scope(arguments.project.as_deref(), &environment.cwd)?;
    let selected = SelectedIntegration::new(arguments.client, environment, &scope)?;
    let manager = PlanManager::new(&environment.session_id)?;
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let launcher_path = match action {
        MutationAction::Install => {
            Some(validate_stable_launcher(environment.compresso_bin.clone())?)
        }
        MutationAction::Uninstall => None,
    };

    // Recovery can mutate config and receipts, so it is intentionally never
    // attempted by dry-run. A pending journal instead makes planning fail.
    if !arguments.dry_run && manager.recover(&selected.config_path, &receipts)?.is_some() {
        project_canonical_receipts(&receipts, environment, &mut Vec::new());
    }

    let plan = match action {
        MutationAction::Install => selected.adapter.plan_install(
            &manager,
            &InstallCtx {
                compresso_bin: launcher_path
                    .clone()
                    .expect("install actions validate a launcher"),
                scope: scope.clone(),
            },
        )?,
        MutationAction::Uninstall => {
            let receipt = receipts
                .active_for(arguments.client.as_str(), &selected.config_path)?
                .ok_or_else(|| {
                    IntegrationCliError::new(
                        "receipt_not_found",
                        "no active canonical receipt authorizes this uninstall",
                    )
                })?;
            selected.adapter.plan_uninstall(&manager, &receipt)?
        }
    };

    if arguments.dry_run {
        return render_plan(
            output,
            &PlanRendering {
                action,
                client: arguments.client,
                dry_run: true,
                plan: &plan,
                result: None,
                warnings: &[],
                json: arguments.output.json,
            },
        );
    }

    if !arguments.output.json {
        output.write_all(plan.unified_diff.as_bytes())?;
        writeln!(output, "# Plan hash: {}", plan.plan_hash)?;
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
                    warnings: &[],
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

    let approval = PlanApproval::from(&plan);
    let result = match action {
        MutationAction::Install => {
            MutationResult::Install(selected.adapter.install(&manager, &approval, &receipts)?)
        }
        MutationAction::Uninstall => {
            MutationResult::Uninstall(selected.adapter.uninstall(&manager, &approval, &receipts)?)
        }
    };
    let mut warnings = Vec::new();
    project_canonical_receipts(&receipts, environment, &mut warnings);
    render_plan(
        output,
        &PlanRendering {
            action,
            client: arguments.client,
            dry_run: false,
            plan: &plan,
            result: Some(&result),
            warnings: &warnings,
            json: arguments.output.json,
        },
    )
}

fn execute_status(
    arguments: IntegrationReadArgs,
    environment: &IntegrationEnvironment,
    output: &mut impl Write,
) -> Result<(), IntegrationCliError> {
    let scope = resolve_scope(arguments.project.as_deref(), &environment.cwd)?;
    let selected = SelectedIntegration::new(arguments.client, environment, &scope)?;
    let receipts = FileReceiptStore::new(environment.receipt_store_path());
    let detection = selected.adapter.detect(&scope);
    let active_receipt = receipts
        .active_for(arguments.client.as_str(), &selected.config_path)?
        .is_some();
    if arguments.output.json {
        serde_json::to_writer(
            &mut *output,
            &serde_json::json!({
                "schema_version": 1,
                "data": {
                    "action": "status",
                    "client": arguments.client.as_str(),
                    "config_path": selected.config_path,
                    "detection": detection,
                    "canonical_receipt_active": active_receipt,
                }
            }),
        )?;
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
    }
    Ok(())
}

fn execute_verify(
    arguments: IntegrationReadArgs,
    environment: &IntegrationEnvironment,
    output: &mut impl Write,
) -> Result<(), IntegrationCliError> {
    let scope = resolve_scope(arguments.project.as_deref(), &environment.cwd)?;
    let selected = SelectedIntegration::new(arguments.client, environment, &scope)?;
    let mut report = selected.adapter.verify(&scope);
    if arguments.client == IntegrationClientArg::ClaudeCode {
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
    ) -> Result<Self, IntegrationCliError> {
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
    json: bool,
}

fn render_plan(
    output: &mut impl Write,
    rendering: &PlanRendering<'_>,
) -> Result<(), IntegrationCliError> {
    if rendering.json {
        let outcome = match rendering.result {
            Some(MutationResult::Install(receipt)) => serde_json::json!({
                "installed": true,
                "plan_hash": receipt.plan_hash,
            }),
            Some(MutationResult::Uninstall(report)) => serde_json::to_value(report)?,
            None => serde_json::Value::Null,
        };
        serde_json::to_writer(
            &mut *output,
            &serde_json::json!({
                "schema_version": 1,
                "data": {
                    "action": rendering.action.as_str(),
                    "client": rendering.client.as_str(),
                    "dry_run": rendering.dry_run,
                    "plan": rendering.plan.public_preview(),
                    "result": outcome,
                    "warnings": rendering.warnings,
                }
            }),
        )?;
        writeln!(output)?;
    } else if rendering.dry_run {
        output.write_all(rendering.plan.unified_diff.as_bytes())?;
        writeln!(output, "# Plan hash: {}", rendering.plan.plan_hash)?;
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
        serde_json::to_writer(
            &mut *output,
            &serde_json::json!({ "schema_version": 1, "data": report }),
        )?;
        writeln!(output)?;
    } else {
        writeln!(output, "verified: {}", report.verified)?;
        for check in &report.checks {
            writeln!(
                output,
                "{}: {} ({})",
                check.name,
                if check.passed { "pass" } else { "fail" },
                check.detail
            )?;
        }
    }
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

fn system_home_dir() -> Result<PathBuf, IntegrationCliError> {
    #[cfg(windows)]
    let value = env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let value = env::var_os("HOME");
    value.map(PathBuf::from).ok_or_else(|| {
        IntegrationCliError::environment("failed to resolve the user home directory")
    })
}

#[derive(Debug)]
pub(crate) struct IntegrationCliError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl IntegrationCliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn environment(message: impl Into<String>) -> Self {
        Self::new("environment", message)
    }
}

impl From<IntegrationError> for IntegrationCliError {
    fn from(error: IntegrationError) -> Self {
        let code = match error {
            IntegrationError::Unsupported { .. } => "unsupported",
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
        Self::new(code, error.to_string())
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
