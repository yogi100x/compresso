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

    /// Whether both the agent file and the `agent` key are planned: only
    /// then may the entry serve the main-session profile.
    pub(crate) fn is_complete(&self) -> bool {
        !self.removal
            && ["main_agent", "main_agent_setting"]
                .iter()
                .all(|label| self.steps.iter().any(|step| step.label == *label))
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

    /// Applies every step in order. A failure is reported, never raised. An
    /// agent file that failed to install stops the settings key; a key that
    /// could not be removed keeps the agent file it names.
    pub(crate) fn apply(
        &self,
        manager: &PlanManager,
        receipts: &dyn ReceiptStore,
    ) -> Vec<(&'static str, StepOutcome)> {
        let install = !self.removal;
        let mut outcomes = Vec::new();
        for step in &self.steps {
            let earlier_failed = outcomes
                .iter()
                .any(|(_, outcome)| matches!(outcome, StepOutcome::Failed(_)));
            if earlier_failed && install && step.label == "main_agent_setting" {
                outcomes.push((
                    step.label,
                    StepOutcome::Failed(
                        "not set because the agent file was not installed".to_owned(),
                    ),
                ));
                continue;
            }
            if earlier_failed && !install && step.label == "main_agent" {
                outcomes.push((
                    step.label,
                    StepOutcome::Failed(
                        "kept because the \"agent\" setting that names it was not removed"
                            .to_owned(),
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

/// Finishes an interrupted change to the agent file or the settings file
/// before anything else reads them (E17-51 review). A journal left by a
/// crash holds the previewed bytes, the settings bytes included, until it is
/// recovered. A dry run never recovers: it refuses, naming the journal.
pub(crate) fn recover_pending(
    home: &Path,
    scope: &Scope,
    manager: &PlanManager,
    receipts: &dyn ReceiptStore,
    dry_run: bool,
) -> Result<bool, IntegrationCliError> {
    let paths = [
        MainAgentIntegration::new(home.to_path_buf()).config_path(scope),
        MainAgentSettingIntegration::new(home.to_path_buf()).config_path(scope),
    ];
    let mut recovered = false;
    for path in paths.into_iter().flatten() {
        let Some(journal) = manager.pending_journal(&path) else {
            continue;
        };
        if dry_run {
            return Err(IntegrationCliError::new(
                "pending_recovery",
                format!(
                    "{} holds an interrupted Compresso change; finish it first with `compresso integrations install claude-code --project DIR` (not a dry run), which recovers it",
                    journal.display()
                ),
            ));
        }
        recovered |= manager.recover(&path, receipts)?.is_some();
    }
    Ok(recovered)
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
                "main_agent_skipped: {} is a file Compresso did not write, so it is left unchanged, the \"agent\" setting is not added, and the MCP entry is written as --no-main-agent writes it (full profile); to get the default, move that file away and install again",
                path.display()
            ));
            return Ok(companion);
        }
        Err(error) => return Err(error.into()),
    }
    if let Some(value) = foreign {
        warnings.push(format!(
            "main_agent_setting_skipped: {} already sets \"agent\" to {value}; Compresso never overwrites it, so the main-session agent is not installed and the MCP entry is written as --no-main-agent writes it (full profile); to get the default, remove that \"agent\" value and install again",
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
    let mut setting_kept = false;
    for (label, adapter, path) in adapters {
        let Ok(path) = path else {
            continue;
        };
        if label == "main_agent" && setting_kept {
            if receipts
                .active_for(adapter.client_id(), &path)
                .ok()
                .flatten()
                .is_some()
            {
                warnings.push(
                    "main_agent_not_removed: the \"agent\" setting that names it could not be removed, so the agent file is kept too".to_owned(),
                );
            }
            continue;
        }
        if !dry_run && let Err(error) = manager.recover(&path, receipts) {
            warnings.push(format!(
                "{label}_not_removed: {}",
                IntegrationCliError::from(error).message
            ));
            setting_kept |= label == "main_agent_setting";
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
            Err(error) => {
                warnings.push(format!(
                    "{label}_not_removed: {}",
                    IntegrationCliError::from(error).message
                ));
                setting_kept |= label == "main_agent_setting";
            }
        }
    }
    companion
}

/// A one-line warning when `root` is a git repository whose rules (its own
/// `.gitignore` files, `info/exclude`, or the user's global excludes file)
/// do not ignore `.claude/settings.local.json`. Nothing is changed.
pub(crate) fn settings_not_ignored_warning(root: &Path) -> Option<String> {
    if !root.join(".git").exists() {
        return None;
    }
    let relative = Path::new(LOCAL_SETTINGS_FILE);
    if compresso_atlas::is_scanner_excluded(root, relative)
        || compresso_atlas::is_globally_git_ignored(relative)
    {
        return None;
    }
    Some(format!(
        "settings_local_not_ignored: this git repository does not ignore {LOCAL_SETTINGS_FILE}, which now names your personal agent; Compresso did not edit .gitignore: add the file there yourself if it should not be committed"
    ))
}

/// The main-session checks `integrations verify claude-code --project`
/// adds. They are required when the entry serves the lean session profile
/// or Compresso's agent file (with its marker) is present: the file, the key,
/// and the entry must then agree, and a mismatch is named. Otherwise a user's
/// own `agent` value or own `compresso-main.md` is reported as informational
/// (passed, not managed by Compresso); nothing present adds no check.
pub(crate) fn verify_checks(
    home: &Path,
    scope: &Scope,
    entry_args: Option<&[String]>,
) -> Vec<VerificationCheck> {
    use compresso_integrations::AgentSetting;
    let agent = MainAgentIntegration::new(home.to_path_buf());
    let setting = MainAgentSettingIntegration::new(home.to_path_buf());
    let agent_ours = agent.is_installed(scope);
    let file_present = agent.config_path(scope).is_ok_and(|path| path.exists());
    let key = setting.agent_setting(scope);
    let lean = entry_args.is_some_and(server_serves_lean_session);
    if !lean && !agent_ours {
        let mut notes = Vec::new();
        if file_present {
            notes.push(format!(
                ".claude/agents/{MAIN_AGENT_NAME}.md was not written by Compresso"
            ));
        }
        match &key {
            Ok(AgentSetting::Other(_)) => notes.push(format!(
                "{LOCAL_SETTINGS_FILE} sets \"agent\" to your own agent"
            )),
            Ok(AgentSetting::Ours) => notes.push(format!(
                "{LOCAL_SETTINGS_FILE} sets \"agent\" to \"{MAIN_AGENT_NAME}\", but no Compresso agent file is installed"
            )),
            _ => {}
        }
        if notes.is_empty() {
            return Vec::new();
        }
        return vec![VerificationCheck {
            name: "main_agent_not_managed".to_owned(),
            passed: true,
            detail: format!(
                "{}; Compresso does not manage it and changes nothing",
                notes.join("; ")
            ),
        }];
    }
    let mut checks = agent.verify(scope).checks;
    checks.extend(setting.verify(scope).checks);
    let edits = entry_args.is_some_and(server_enables_edits);
    let edit_body = agent
        .config_path(scope)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|text| text == MainAgentIntegration::render(true));
    let restore = "run `compresso integrations install claude-code --project DIR` (it installs the agent, or writes the full-profile entry when it cannot)";
    let (passed, detail) = if entry_args.is_none() {
        (
            false,
            format!(
                "mismatch: Compresso's main-session agent file is present but the project has no readable compresso entry; {restore}"
            ),
        )
    } else if !lean {
        (
            false,
            format!(
                "mismatch: Compresso's main-session agent file is present but the compresso entry serves the full profile, not --tools lean --session-tools; {restore}, or add --no-main-agent to remove the agent (delete the file if Compresso has no receipt for it)"
            ),
        )
    } else if !agent_ours || !matches!(key, Ok(AgentSetting::Ours)) {
        (
            false,
            format!(
                "mismatch: the compresso entry serves the main-session profile but Compresso's agent file or its \"agent\" setting is missing; {restore}"
            ),
        )
    } else if edit_body && !edits {
        (
            false,
            format!(
                "mismatch: the agent file tells the model to use compresso_edit but the compresso entry has no --edits; {restore}"
            ),
        )
    } else {
        (
            true,
            format!(
                "the compresso entry serves the main-session profile {} compresso_edit, as the agent file expects",
                if edits { "with" } else { "without" }
            ),
        )
    };
    checks.push(VerificationCheck {
        name: "main_agent_server_profile".to_owned(),
        passed,
        detail,
    });
    checks
}

#[cfg(test)]
mod tests {
    use std::fs;

    use compresso_integrations::MemoryReceiptStore;

    use super::*;

    /// The key is never written when the agent file step failed at apply
    /// time, even though both were planned (E17-51 review, mutation M14).
    #[test]
    fn test_the_key_is_not_applied_after_the_agent_file_failed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let base = fs::canonicalize(directory.path()).expect("canonical");
        let home = base.join("home");
        let project = base.join("project");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&project).expect("project");
        let launcher = base.join("compresso");
        fs::write(&launcher, "").expect("launcher");
        let manager = PlanManager::new("main-agent-apply-test")
            .expect("manager")
            .with_backup_dir(
                base.join("data")
                    .join(compresso_integrations::BACKUP_DIR_NAME),
            );
        let receipts = MemoryReceiptStore::default();
        let scope = Scope::Project(project.clone());
        let companion = plan_install(
            &home,
            &scope,
            true,
            &manager,
            &receipts,
            &launcher,
            true,
            false,
            &mut Vec::new(),
        )
        .expect("plan");
        assert!(companion.is_complete());
        // The agent file changes between the preview and the apply.
        let agent = project.join(".claude/agents/compresso-main.md");
        fs::create_dir_all(agent.parent().unwrap()).expect("agents dir");
        fs::write(&agent, "mine\n").expect("user file");
        let outcomes = companion.apply(&manager, &receipts);
        assert!(matches!(
            outcomes[0],
            ("main_agent", StepOutcome::Failed(_))
        ));
        assert!(matches!(
            outcomes[1],
            ("main_agent_setting", StepOutcome::Failed(_))
        ));
        assert!(!project.join(".claude/settings.local.json").exists());
        assert_eq!(fs::read_to_string(&agent).unwrap(), "mine\n");
    }

    /// A key whose removal fails at apply time keeps the agent file it
    /// names (E17-51 review).
    #[test]
    fn test_the_agent_file_is_kept_when_the_key_removal_failed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let base = fs::canonicalize(directory.path()).expect("canonical");
        let home = base.join("home");
        let project = base.join("project");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&project).expect("project");
        let launcher = base.join("compresso");
        fs::write(&launcher, "").expect("launcher");
        let manager = PlanManager::new("main-agent-removal-test")
            .expect("manager")
            .with_backup_dir(
                base.join("data")
                    .join(compresso_integrations::BACKUP_DIR_NAME),
            );
        let receipts = MemoryReceiptStore::default();
        let scope = Scope::Project(project.clone());
        let install = plan_install(
            &home,
            &scope,
            true,
            &manager,
            &receipts,
            &launcher,
            true,
            false,
            &mut Vec::new(),
        )
        .expect("plan");
        install.apply(&manager, &receipts);
        let removal = plan_uninstall(&home, &scope, &manager, &receipts, false, &mut Vec::new());
        assert_eq!(removal.steps.len(), 2);
        // The settings file changes between the preview and the apply.
        let settings = project.join(".claude/settings.local.json");
        fs::write(
            &settings,
            "{\"agent\": \"compresso-main\", \"model\": \"x\"}\n",
        )
        .unwrap();
        let outcomes = removal.apply(&manager, &receipts);
        assert!(matches!(
            outcomes[0],
            ("main_agent_setting", StepOutcome::Failed(_))
        ));
        assert!(matches!(
            outcomes[1],
            ("main_agent", StepOutcome::Failed(_))
        ));
        assert!(project.join(".claude/agents/compresso-main.md").is_file());
    }
}
