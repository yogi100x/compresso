use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, ValueEnum};
use compresso_integrations::{MutationPlan, PlanClock, SystemPlanClock};
use compresso_store::{Store, StoreError};
use serde_json::{Value, json};

use super::index::{self, IndexCommandError, Rendered as IndexRendered};
use super::integrations::{
    IntegrationCliError, IntegrationEnvironment, SetupInstallResult, prepare_setup_install,
};
use super::{CliExitCode, IndexArgs, IntegrationClientArg, OutputArgs, write_error};

/// Runtime dependencies kept injectable for write-free and plan-expiry setup tests.
pub trait SetupRuntime {
    fn open_default_store(&self) -> Result<Store, StoreError>;
    fn plan_clock(&self) -> Arc<dyn PlanClock>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSetupRuntime;

impl SetupRuntime for SystemSetupRuntime {
    fn open_default_store(&self) -> Result<Store, StoreError> {
        Store::open_default()
    }

    fn plan_clock(&self) -> Arc<dyn PlanClock> {
        Arc::new(SystemPlanClock)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SetupClientArg {
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
    Cursor,
}

impl SetupClientArg {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
        }
    }

    const fn integration_client(self) -> IntegrationClientArg {
        match self {
            Self::ClaudeCode => IntegrationClientArg::ClaudeCode,
            Self::Codex => IntegrationClientArg::Codex,
            Self::Cursor => IntegrationClientArg::Cursor,
        }
    }

    fn project(self, root: &Path) -> Option<&Path> {
        match self {
            Self::Codex => None,
            Self::ClaudeCode | Self::Cursor => Some(root),
        }
    }
}

#[derive(Args, Debug)]
pub struct SetupArgs {
    /// Coding client to connect to Compresso.
    #[arg(value_enum)]
    pub client: SetupClientArg,
    /// Repository directory to index and approve.
    #[arg(value_name = "ROOT")]
    pub root: PathBuf,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Preview repository indexing and the exact client config plan without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Apply the combined displayed plan without an interactive prompt.
    #[arg(long)]
    pub yes: bool,
}

/// Executes setup against injected local state and integration paths.
///
/// Dry-run callers may pass `None` for `store`, proving that preview cannot approve or index a
/// repository. Real setup lazily opens the default Store only after approval and root
/// revalidation, then renews timing and applies the exact authorized integration plan.
pub fn execute_setup(
    arguments: SetupArgs,
    store: Option<&Store>,
    environment: &IntegrationEnvironment,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    execute_setup_with_runtime(
        arguments,
        store,
        environment,
        &SystemSetupRuntime,
        input,
        output,
        errors,
    )
}

/// Executes setup with injected clock and default-Store acquisition.
pub fn execute_setup_with_runtime(
    arguments: SetupArgs,
    store: Option<&Store>,
    environment: &IntegrationEnvironment,
    runtime: &dyn SetupRuntime,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json_output = arguments.output.json;
    match execute_inner(
        arguments,
        store,
        environment,
        runtime,
        input,
        output,
        errors,
    ) {
        Ok(()) => CliExitCode::Success,
        Err(error) => {
            let _ = write_error(errors, error.code, &error.message, json_output);
            CliExitCode::Failure
        }
    }
}

fn execute_inner(
    arguments: SetupArgs,
    store: Option<&Store>,
    environment: &IntegrationEnvironment,
    runtime: &dyn SetupRuntime,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> Result<(), SetupCliError> {
    let root =
        index::resolve_root(&arguments.root, &environment.cwd).map_err(SetupCliError::index)?;
    let index_preview = index::execute(
        IndexArgs {
            path: root.clone(),
            output: OutputArgs { json: true },
            dry_run: true,
        },
        None,
        &environment.cwd,
    )
    .map_err(SetupCliError::index)?;
    let clock = runtime.plan_clock();
    let prepared = prepare_setup_install(
        arguments.client.integration_client(),
        arguments.client.project(&root),
        environment,
        Arc::clone(&clock),
    )
    .map_err(SetupCliError::integration)?;
    let plan = SetupPlan {
        client: arguments.client,
        root,
        repository: index_preview,
        integration: prepared.plan().clone(),
    };

    if arguments.dry_run {
        return render_plan(output, &plan, true, None, arguments.output.json);
    }

    if arguments.output.json && !arguments.yes {
        render_plan(output, &plan, false, None, true)?;
        return Err(SetupCliError::confirmation_required(
            "JSON setup requires --yes after reviewing the returned combined plan",
        ));
    }

    if !arguments.output.json {
        render_plan(output, &plan, false, None, false)?;
    }
    if !arguments.yes {
        write!(errors, "Apply setup? [y/N] ").map_err(SetupCliError::io)?;
        errors.flush().map_err(SetupCliError::io)?;
        match read_confirmation(input).map_err(SetupCliError::io)? {
            Confirmation::Approved => {}
            Confirmation::Declined => {
                writeln!(output, "Cancelled; no setup changes were made.")
                    .map_err(SetupCliError::io)?;
                return Ok(());
            }
            Confirmation::Unavailable => {
                return Err(SetupCliError::confirmation_required(
                    "interactive confirmation is unavailable; rerun with --yes",
                ));
            }
        }
    }

    // Convert the still-valid preview into an opaque, one-use authorization before indexing.
    // This keeps a slow local index from weakening plan expiry while retaining the exact diff and
    // backup path the user just approved.
    let authorization = prepared.authorize().map_err(SetupCliError::integration)?;

    let current_root =
        index::resolve_root(&plan.root, &environment.cwd).map_err(SetupCliError::root_changed)?;
    if current_root != plan.root {
        return Err(SetupCliError::root_changed_since_preview());
    }

    let owned_store = if store.is_none() {
        Some(
            runtime
                .open_default_store()
                .map_err(SetupCliError::store_open)?,
        )
    } else {
        None
    };
    let store = store
        .or(owned_store.as_ref())
        .ok_or_else(SetupCliError::store_unavailable)?;
    let indexed = index::execute(
        IndexArgs {
            path: plan.root.clone(),
            output: OutputArgs { json: true },
            dry_run: false,
        },
        Some(store),
        &environment.cwd,
    )
    .map_err(SetupCliError::index_after_approval)?;

    if !arguments.output.json {
        output
            .write_all(indexed.human.as_bytes())
            .map_err(SetupCliError::io)?;
    }

    let post_index_root = match index::resolve_root(&plan.root, &environment.cwd) {
        Ok(root) => root,
        Err(error) => {
            let message = format!(
                "repository root could not be revalidated after indexing ({}); client configuration was not applied",
                error.message
            );
            return fail_after_index(
                output,
                &plan,
                &indexed,
                arguments.output.json,
                IntegrationFailure {
                    stage: IntegrationFailureStage::BeforeApply,
                    attempted_plan_hash: None,
                    cause_code: "setup_root_changed",
                    cause_message: &message,
                },
            );
        }
    };
    if post_index_root != plan.root {
        return fail_after_index(
            output,
            &plan,
            &indexed,
            arguments.output.json,
            IntegrationFailure {
                stage: IntegrationFailureStage::BeforeApply,
                attempted_plan_hash: None,
                cause_code: "setup_root_changed",
                cause_message: "repository root resolved to a different directory after indexing; client configuration was not applied",
            },
        );
    }

    if let Err(error) = prepared.revalidate_target(environment) {
        return fail_after_index(
            output,
            &plan,
            &indexed,
            arguments.output.json,
            IntegrationFailure {
                stage: IntegrationFailureStage::BeforeApply,
                attempted_plan_hash: None,
                cause_code: error.code,
                cause_message: &error.message,
            },
        );
    }

    let installed = match prepared.apply(authorization, environment) {
        Ok(installed) => installed,
        Err(error) => {
            return fail_after_index(
                output,
                &plan,
                &indexed,
                arguments.output.json,
                IntegrationFailure {
                    stage: IntegrationFailureStage::ApplyAttempted,
                    attempted_plan_hash: None,
                    cause_code: error.code,
                    cause_message: &error.message,
                },
            );
        }
    };
    let plan_hash_changed = plan.integration.plan_hash != installed.effective_plan_hash;

    if arguments.output.json {
        render_plan(
            output,
            &plan,
            false,
            Some(success_result(
                &indexed,
                &installed,
                &plan.integration.plan_hash,
                plan_hash_changed,
            )),
            true,
        )?;
    } else {
        if installed.already_applied {
            writeln!(
                output,
                "The {} MCP integration was already active; no client configuration write was needed (receipt {}).",
                plan.client.as_str(),
                installed.receipt_plan_hash
            )
            .map_err(SetupCliError::io)?;
        } else {
            writeln!(
                output,
                "Installed the {} MCP integration.",
                plan.client.as_str()
            )
            .map_err(SetupCliError::io)?;
        }
        for warning in &installed.warnings {
            writeln!(output, "warning: {warning}").map_err(SetupCliError::io)?;
        }
        if plan_hash_changed {
            writeln!(
                output,
                "Integration timing was renewed after indexing; the exact approved diff and backup path were preserved. Effective plan hash: {}",
                installed.effective_plan_hash
            )
            .map_err(SetupCliError::io)?;
        }
        writeln!(
            output,
            "Setup complete for {} at {}.",
            plan.client.as_str(),
            plan.root.display()
        )
        .map_err(SetupCliError::io)?;
    }
    Ok(())
}

struct SetupPlan {
    client: SetupClientArg,
    root: PathBuf,
    repository: IndexRendered,
    integration: MutationPlan,
}

fn render_plan(
    output: &mut impl Write,
    plan: &SetupPlan,
    dry_run: bool,
    result: Option<Value>,
    json_output: bool,
) -> Result<(), SetupCliError> {
    if json_output {
        serde_json::to_writer(
            &mut *output,
            &json!({
                "schema_version": 1,
                "data": {
                    "action": "setup",
                    "client": plan.client.as_str(),
                    "dry_run": dry_run,
                    "root": plan.root,
                    "plan": {
                        "repository": plan.repository.json["data"].clone(),
                        "integration": plan.integration,
                    },
                    "result": result,
                },
            }),
        )
        .map_err(SetupCliError::json)?;
        writeln!(output).map_err(SetupCliError::io)?;
        return Ok(());
    }

    writeln!(
        output,
        "Setup plan for {} at {}:",
        plan.client.as_str(),
        plan.root.display()
    )
    .map_err(SetupCliError::io)?;
    output
        .write_all(plan.repository.human.as_bytes())
        .map_err(SetupCliError::io)?;
    output
        .write_all(plan.integration.unified_diff.as_bytes())
        .map_err(SetupCliError::io)?;
    if !plan.integration.unified_diff.ends_with('\n') {
        writeln!(output).map_err(SetupCliError::io)?;
    }
    writeln!(output, "# Plan hash: {}", plan.integration.plan_hash).map_err(SetupCliError::io)?;
    if dry_run {
        writeln!(output, "Dry run: no setup changes were made.").map_err(SetupCliError::io)?;
    }
    Ok(())
}

fn success_result(
    indexed: &IndexRendered,
    installed: &SetupInstallResult,
    approved_plan_hash: &str,
    plan_hash_changed: bool,
) -> Value {
    let applied_plan_hash =
        (!installed.already_applied).then_some(installed.effective_plan_hash.as_str());
    json!({
        "status": "complete",
        "repository": indexed.json["data"].clone(),
        "integration": {
            "installed": true,
            "already_applied": installed.already_applied,
            "plan_refreshed": plan_hash_changed,
            "approved_plan_hash": approved_plan_hash,
            "effective_plan_hash": installed.effective_plan_hash,
            "applied_plan_hash": applied_plan_hash,
            "receipt_plan_hash": installed.receipt_plan_hash,
            "plan_hash": installed.effective_plan_hash,
            "effective_diff_hash": installed.effective_diff_hash,
            "effective_backup_path": installed.effective_backup_path,
            "receipt_backup_path": installed.receipt_backup_path,
            "warnings": installed.warnings,
        },
    })
}

fn fail_after_index(
    output: &mut impl Write,
    plan: &SetupPlan,
    indexed: &IndexRendered,
    json_output: bool,
    failure: IntegrationFailure<'_>,
) -> Result<(), SetupCliError> {
    let recovery = partial_failure_recovery(plan.client, failure.cause_code, failure.cause_message);
    if json_output {
        render_plan(
            output,
            plan,
            false,
            Some(json!({
                "status": "partial_failure",
                "repository": indexed.json["data"].clone(),
                "integration": {
                    "state": failure.stage.state(),
                    "outcome_known": failure.stage.outcome_known(),
                    "recovery_required": true,
                    "approved_plan_hash": plan.integration.plan_hash,
                    "attempted_plan_hash": failure.attempted_plan_hash,
                    "cause": {
                        "code": failure.cause_code,
                        "message": failure.cause_message,
                    },
                },
                "recovery": recovery,
            })),
            true,
        )?;
    }
    Err(SetupCliError {
        code: "setup_partial_failure",
        message: recovery,
    })
}

struct IntegrationFailure<'a> {
    stage: IntegrationFailureStage,
    attempted_plan_hash: Option<&'a str>,
    cause_code: &'a str,
    cause_message: &'a str,
}

#[derive(Clone, Copy)]
enum IntegrationFailureStage {
    BeforeApply,
    ApplyAttempted,
}

impl IntegrationFailureStage {
    const fn state(self) -> &'static str {
        match self {
            Self::BeforeApply => "not_applied",
            Self::ApplyAttempted => "uncertain",
        }
    }

    const fn outcome_known(self) -> bool {
        matches!(self, Self::BeforeApply)
    }
}

fn partial_failure_recovery(
    client: SetupClientArg,
    cause_code: &str,
    cause_message: &str,
) -> String {
    let integration_command = match client {
        SetupClientArg::Codex => "compresso integrations install codex --yes".to_owned(),
        SetupClientArg::ClaudeCode | SetupClientArg::Cursor => format!(
            "compresso integrations install {} --project <repository-root> --yes",
            client.as_str()
        ),
    };
    format!(
        "repository remains indexed, but the {} integration did not complete cleanly ({cause_code}): {cause_message}. Client configuration may have changed; inspect its Compresso backup/receipt state instead of assuming install or rollback completed. Repository source files were not deleted. After resolving the client-config issue, recover with `{integration_command}`",
        client.as_str(),
    )
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

struct SetupCliError {
    code: &'static str,
    message: String,
}

impl SetupCliError {
    fn confirmation_required(message: &str) -> Self {
        Self {
            code: "confirmation_required",
            message: message.to_owned(),
        }
    }

    fn index(error: IndexCommandError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }

    fn index_after_approval(error: IndexCommandError) -> Self {
        Self {
            code: "setup_index_failed",
            message: format!(
                "repository indexing did not complete ({}): {}; client configuration was not changed",
                error.code, error.message
            ),
        }
    }

    fn root_changed(error: IndexCommandError) -> Self {
        Self {
            code: "setup_root_changed",
            message: format!(
                "repository root changed after preview ({}): no repository or client-config change was applied",
                error.message
            ),
        }
    }

    fn root_changed_since_preview() -> Self {
        Self {
            code: "setup_root_changed",
            message: "repository root resolved to a different directory after preview; no repository or client-config change was applied"
                .to_owned(),
        }
    }

    fn integration(error: IntegrationCliError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }

    fn store_unavailable() -> Self {
        Self {
            code: "internal",
            message: "setup requires the local Compresso store".to_owned(),
        }
    }

    fn store_open(_error: StoreError) -> Self {
        Self {
            code: "internal",
            message: "failed to open the local Compresso store after setup approval".to_owned(),
        }
    }

    fn io(_error: std::io::Error) -> Self {
        Self {
            code: "io",
            message: "setup command I/O failed".to_owned(),
        }
    }

    fn json(_error: serde_json::Error) -> Self {
        Self {
            code: "internal",
            message: "failed to encode setup output".to_owned(),
        }
    }
}
