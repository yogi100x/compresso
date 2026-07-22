use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use compresso_core::RuntimePaths;
use compresso_integrations::{
    ClaudeCodeDiscovery, ClaudeCodeIntegration, CodexIntegration, CursorApplicationEvidence,
    CursorCliEvidence, CursorDiscovery, CursorIntegration, Detection, FileReceiptStore, InstallCtx,
    InstallReceipt, Integration, IntegrationError, MutationPlan, PlanApproval, PlanManager,
    ReceiptStore, Scope, VerificationCheck, VerificationReport,
};
use compresso_store::{ReceiptRecord, Store};

use super::{
    CliExitCode, IntegrationClientArg, IntegrationMutationArgs, IntegrationReadArgs,
    IntegrationsCommand, write_error,
};

const RECEIPT_STORE_NAME: &str = "integration-receipts.json";

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
        let compresso_bin = env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|_| {
                IntegrationCliError::environment(
                    "failed to resolve the installed Compresso executable",
                )
            })?;
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

    // Recovery can mutate config and receipts, so it is intentionally never
    // attempted by dry-run. A pending journal instead makes planning fail.
    if !arguments.dry_run && manager.recover(&selected.config_path, &receipts)?.is_some() {
        project_canonical_receipts(&receipts, environment, &mut Vec::new());
    }

    let plan = match action {
        MutationAction::Install => selected.adapter.plan_install(
            &manager,
            &InstallCtx {
                compresso_bin: environment.compresso_bin.clone(),
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
                    "plan": rendering.plan,
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
