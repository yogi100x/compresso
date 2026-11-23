//! The default main-session agent a Claude Code project install adds
//! (E17-51, D116): the agent file `.claude/agents/compresso-main.md` and the
//! `agent` key in `.claude/settings.local.json`, each with its own plan and
//! receipt beside the MCP entry's.
//!
//! Everything is planned before the first write, so a hard refusal (an
//! unparseable settings file, a symbolic link on a path) stops the install
//! with nothing written. A user file at the agent path, or an `agent` value
//! that is not ours, skips the step with a warning; the key is never written
//! without the agent file. Install applies the agent file, then the key;
//! uninstall removes the key, then the file, each only through its receipt.

use std::io::Write;
use std::path::Path;

use compresso_integrations::{
    InstallCtx, Integration, IntegrationError, LOCAL_SETTINGS_FILE, MAIN_AGENT_NAME,
    MainAgentIntegration, MainAgentSettingIntegration, MutationPlan, PlanApproval, PlanManager,
    ReceiptStore, Scope, VerificationCheck, server_enables_edits, server_serves_lean_session,
};

use super::integrations::IntegrationCliError;

/// One planned change: the agent file or the settings key.
pub(crate) struct MainAgentStep {
    /// `main_agent` or `main_agent_setting`.
    pub(crate) label: &'static str,
    adapter: Box<dyn Integration>,
    pub(crate) plan: MutationPlan,
}

/// The planned main-session agent changes, in apply order.
#[derive(Default)]
pub(crate) struct MainAgentCompanion {
    pub(crate) steps: Vec<MainAgentStep>,
    /// The steps remove what earlier installs wrote (uninstall, or an
    /// install with `--no-main-agent`).
    removal: bool,
}

/// What applying one step did.
pub(crate) enum StepOutcome {
    Installed,
    Removed { warnings: Vec<String> },
    Failed(String),
}

impl MainAgentCompanion {
    pub(crate) fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Whether the steps install (not remove) the main-session agent.
    pub(crate) fn installs(&self) -> bool {
        !self.removal
    }

    /// Writes each plan's preview, with the exact settings change spelled
    /// out (the plan preview redacts non-object values).
    pub(crate) fn write_preview(&self, output: &mut impl Write) -> std::io::Result<()> {
        for step in &self.steps {
            output.write_all(step.plan.unified_diff.as_bytes())?;
            if step.label == "main_agent_setting" {
                let change = match step.plan.operation {
                    compresso_integrations::MutationKind::Install => {
                        format!("sets \"agent\": \"{MAIN_AGENT_NAME}\"")
                    }
                    compresso_integrations::MutationKind::Uninstall => {
                        "removes \"agent\"".to_owned()
                    }
                };
                writeln!(
                    output,
                    "# {LOCAL_SETTINGS_FILE}: {change}; every other byte is kept{}",
                    if step.plan.removes_file() {
                        " (the file Compresso created is removed)"
                    } else {
                        ""
                    }
                )?;
            }
            writeln!(
                output,
                "# Main agent plan hash ({}): {}",
                step.label, step.plan.plan_hash
            )?;
        }
        Ok(())
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.steps
                .iter()
                .map(|step| {
                    serde_json::json!({
                        "step": step.label,
                        "plan": step.plan.public_preview(),
                    })
                })
                .collect(),
        )
    }

    /// Applies every step in order. A failure is reported, never raised, and
    /// an agent file that failed to install stops the settings key.
    pub(crate) fn apply(
        &self,
        manager: &PlanManager,
        receipts: &dyn ReceiptStore,
    ) -> Vec<(&'static str, StepOutcome)> {
        let install = !self.removal;
        let mut outcomes = Vec::new();
        for step in &self.steps {
            if install
                && step.label == "main_agent_setting"
                && outcomes
                    .iter()
                    .any(|(_, outcome)| matches!(outcome, StepOutcome::Failed(_)))
            {
                outcomes.push((
                    step.label,
                    StepOutcome::Failed(
                        "not set because the agent file was not installed".to_owned(),
                    ),
                ));
                continue;
            }
            let approval = PlanApproval::from(&step.plan);
            let outcome = if install {
                match step.adapter.install(manager, &approval, receipts) {
                    Ok(_) => StepOutcome::Installed,
                    Err(error) => StepOutcome::Failed(IntegrationCliError::from(error).message),
                }
            } else {
                match step.adapter.uninstall(manager, &approval, receipts) {
                    Ok(report) => StepOutcome::Removed {
                        warnings: report.warnings,
                    },
                    Err(error) => StepOutcome::Failed(IntegrationCliError::from(error).message),
                }
            };
            outcomes.push((step.label, outcome));
        }
        outcomes
    }
}

pub(crate) fn outcome_json(outcome: &StepOutcome) -> serde_json::Value {
    match outcome {
        StepOutcome::Installed => serde_json::json!({ "installed": true }),
        StepOutcome::Removed { warnings } => {
            serde_json::json!({ "removed": true, "warnings": warnings })
        }
        StepOutcome::Failed(message) => serde_json::json!({ "failed": message }),
    }
}

/// The human line for one applied step.
pub(crate) fn write_outcome(
    output: &mut impl Write,
    label: &str,
    outcome: &StepOutcome,
) -> std::io::Result<()> {
    let what = if label == "main_agent" {
        format!("main-session agent file .claude/agents/{MAIN_AGENT_NAME}.md")
    } else {
        format!("\"agent\" setting in {LOCAL_SETTINGS_FILE}")
    };
    match outcome {
        StepOutcome::Installed => writeln!(output, "{what}: installed"),
        StepOutcome::Removed { warnings } => {
            writeln!(output, "{what}: removed")?;
            for warning in warnings {
                writeln!(output, "warning: {warning}")?;
            }
            Ok(())
        }
        // Failures are already listed among the warnings.
        StepOutcome::Failed(_) => Ok(()),
    }
}

/// Plans the main-session agent for a project install. Returns an error for
/// a refusal that must stop the whole install before anything is written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_install(
    home: &Path,
    scope: &Scope,
    edits: bool,
    manager: &PlanManager,
    receipts: &dyn ReceiptStore,
    launcher: &Path,
    session_recall: bool,
    dry_run: bool,
    warnings: &mut Vec<String>,
) -> Result<MainAgentCompanion, IntegrationCliError> {
    plan_install_checked(
        home,
        scope,
        edits,
        manager,
        receipts,
        launcher,
        session_recall,
        dry_run,
        warnings,
    )
    .map_err(|error| {
        IntegrationCliError::new(
            error.code,
            format!(
                "main-session agent: {}; nothing was changed. Fix it, or install with --no-main-agent to leave the agent out",
                error.message
            ),
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn plan_install_checked(
    home: &Path,
    scope: &Scope,
    edits: bool,
    manager: &PlanManager,
    receipts: &dyn ReceiptStore,
    launcher: &Path,
    session_recall: bool,
    dry_run: bool,
    warnings: &mut Vec<String>,
) -> Result<MainAgentCompanion, IntegrationCliError> {
    let context = InstallCtx {
        compresso_bin: launcher.to_path_buf(),
        scope: scope.clone(),
        session_recall,
    };
    let agent = MainAgentIntegration::new(home.to_path_buf()).with_edits(edits);
    let setting = MainAgentSettingIntegration::new(home.to_path_buf());
    let agent_path = agent.config_path(scope)?;
    let setting_path = setting.config_path(scope)?;
    // Read the settings file first: an unparseable one stops the install.
    let foreign = setting.foreign_agent(scope)?;
    if !dry_run {
        manager.recover(&agent_path, receipts)?;
        manager.recover(&setting_path, receipts)?;
    }
    let mut companion = MainAgentCompanion::default();
    match agent.plan_install(manager, &context) {
        Ok(plan) => companion.steps.push(MainAgentStep {
            label: "main_agent",
            adapter: Box::new(agent),
            plan,
        }),
        Err(IntegrationError::UserFilePresent { path }) => {
            warnings.push(format!(
                "main_agent_skipped: {} is a file Compresso did not write, so it is left unchanged and the \"agent\" setting is not added; the MCP entry is installed",
                path.display()
            ));
            return Ok(companion);
        }
        Err(error) => return Err(error.into()),
    }
    if let Some(value) = foreign {
        warnings.push(format!(
            "main_agent_setting_skipped: {} already sets \"agent\" to {value}; Compresso never overwrites it. To use the Compresso main-session agent, set \"agent\" to \"{MAIN_AGENT_NAME}\" yourself or start Claude Code with --agent {MAIN_AGENT_NAME}",
            setting_path.display()
        ));
        return Ok(companion);
    }
    let plan = setting.plan_install(manager, &context)?;
    companion.steps.push(MainAgentStep {
        label: "main_agent_setting",
        adapter: Box::new(setting),
        plan,
    });
    Ok(companion)
}

/// Plans removing whatever of the main-session agent an active receipt
/// covers. Problems become warnings; the MCP entry uninstall goes ahead.
pub(crate) fn plan_uninstall(
    home: &Path,
    scope: &Scope,
    manager: &PlanManager,
    receipts: &dyn ReceiptStore,
    dry_run: bool,
    warnings: &mut Vec<String>,
) -> MainAgentCompanion {
    let mut companion = MainAgentCompanion {
        removal: true,
        ..MainAgentCompanion::default()
    };
    let adapters: [(
        &'static str,
        Box<dyn Integration>,
        Result<std::path::PathBuf, _>,
    ); 2] = {
        let setting = MainAgentSettingIntegration::new(home.to_path_buf());
        let agent = MainAgentIntegration::new(home.to_path_buf());
        let setting_path = setting.config_path(scope);
        let agent_path = agent.config_path(scope);
        [
            ("main_agent_setting", Box::new(setting), setting_path),
            ("main_agent", Box::new(agent), agent_path),
        ]
    };
    for (label, adapter, path) in adapters {
        let Ok(path) = path else {
            continue;
        };
        if !dry_run && let Err(error) = manager.recover(&path, receipts) {
            warnings.push(format!(
                "{label}_not_removed: {}",
                IntegrationCliError::from(error).message
            ));
            continue;
        }
        let Some(receipt) = receipts
            .active_for(adapter.client_id(), &path)
            .ok()
            .flatten()
        else {
            continue;
        };
        match adapter.plan_uninstall(manager, &receipt) {
            Ok(plan) => companion.steps.push(MainAgentStep {
                label,
                adapter,
                plan,
            }),
            Err(error) => warnings.push(format!(
                "{label}_not_removed: {}",
                IntegrationCliError::from(error).message
            )),
        }
    }
    companion
}

/// A one-line warning when `root` is a git repository whose rules (its own
/// `.gitignore` files, `info/exclude`, or the user's global excludes file)
/// do not ignore `.claude/settings.local.json`, or the timestamped backups
/// Compresso keeps beside it (`settings.local.json.compresso-backup-*`).
/// Nothing is changed.
pub(crate) fn settings_not_ignored_warning(root: &Path) -> Option<String> {
    if !root.join(".git").exists() {
        return None;
    }
    let ignored = |relative: &Path| {
        compresso_atlas::is_scanner_excluded(root, relative)
            || compresso_atlas::is_globally_git_ignored(relative)
    };
    let file = ignored(Path::new(LOCAL_SETTINGS_FILE));
    let backups = ignored(Path::new(&format!(
        "{LOCAL_SETTINGS_FILE}.compresso-backup-2000-01-01T00-00-00Z"
    )));
    let what = match (file, backups) {
        (true, true) => return None,
        (false, true) => LOCAL_SETTINGS_FILE.to_owned(),
        (true, false) => format!(
            "the backups Compresso keeps beside {LOCAL_SETTINGS_FILE} when it changes an existing file ({LOCAL_SETTINGS_FILE}.compresso-backup-*)"
        ),
        (false, false) => format!(
            "{LOCAL_SETTINGS_FILE} or the backups Compresso keeps beside it when it changes an existing file ({LOCAL_SETTINGS_FILE}.compresso-backup-*)"
        ),
    };
    Some(format!(
        "settings_local_not_ignored: this git repository does not ignore {what}; the file names your personal agent. Add `{LOCAL_SETTINGS_FILE}*` to .gitignore if they should not be committed"
    ))
}

/// The main-session checks `integrations verify claude-code --project`
/// adds when the agent file or the `agent` key is present; none when
/// neither is (a `--no-main-agent` install).
pub(crate) fn verify_checks(
    home: &Path,
    scope: &Scope,
    entry_args: Option<&[String]>,
) -> Vec<VerificationCheck> {
    let agent = MainAgentIntegration::new(home.to_path_buf());
    let setting = MainAgentSettingIntegration::new(home.to_path_buf());
    let file_present = agent.config_path(scope).is_ok_and(|path| path.exists());
    let key_present = !matches!(
        setting.agent_setting(scope),
        Ok(compresso_integrations::AgentSetting::Absent)
    );
    if !file_present && !key_present {
        return Vec::new();
    }
    let mut checks = agent.verify(scope).checks;
    checks.extend(setting.verify(scope).checks);
    // The entry serves the main-session profile, or the full profile when
    // it carries --sql-env (the lean profile never serves the SQL tool).
    let profile = entry_args.is_some_and(|args| {
        server_serves_lean_session(args) || compresso_integrations::server_sql_env(args).is_some()
    });
    let edits = entry_args.is_some_and(server_enables_edits);
    // An agent told to use compresso_edit needs a server that serves it.
    let edit_body = agent
        .config_path(scope)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|text| text == MainAgentIntegration::render(true));
    let profile = profile && (edits || !edit_body);
    checks.push(VerificationCheck {
        name: "main_agent_server_profile".to_owned(),
        passed: profile,
        detail: if profile {
            format!(
                "the project's compresso entry serves the main-session profile ({})",
                if edits { "with compresso_edit" } else { "without compresso_edit" }
            )
        } else {
            "the project's compresso entry does not serve the main-session profile (--tools lean --session-tools, and --edits when the agent file uses compresso_edit); reinstall with `compresso integrations install claude-code --project DIR`".to_owned()
        },
    });
    checks
}
