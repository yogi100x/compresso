//! Post-compaction routing-reminder hook adapter for Claude Code and Codex.
//!
//! Both clients run `SessionStart` hooks whose matcher is `compact` right
//! after they summarize a conversation, and add the hook's
//! `additionalContext` to the model's context. This adapter installs one such
//! hook that runs `compresso mcp routing-reminder --session-start-hook`.
//!
//! The adapter owns exactly `hooks.SessionStart` in the client's hook file and
//! only takes ownership when that key is absent or already holds a
//! Compresso-written value. It never merges into or rewrites hooks a user
//! defined: in that case installation is refused with the entry to add by
//! hand. Uninstall removes the key only while it still matches the receipt.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::claude_code::{
    canonical_json_hash, is_executable_file, read_existing_json_object,
    read_existing_json_object_for_verify, read_json_object, render_json,
    resolved_compresso_command,
};
use crate::plan::MutationRequest;
use crate::{
    ConfigFormat, Detection, InstallCtx, InstallReceipt, Integration, IntegrationError,
    MutationKind, MutationPlan, PlanManager, Scope, VerificationCheck, VerificationReport,
};

const OWNED_PATH: &str = "hooks.SessionStart";
const HOOK_EVENT: &str = "SessionStart";
const COMPACT_MATCHER: &str = "compact";
const HOOK_ARGUMENTS: &str = "mcp routing-reminder --session-start-hook";
/// Seconds the client waits for the reminder command.
const HOOK_TIMEOUT_SECONDS: u64 = 10;

/// Coding clients that document a post-compaction `SessionStart` hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactionHookClient {
    /// Claude Code: `.claude/settings.json`.
    ClaudeCode,
    /// Codex: `.codex/hooks.json`.
    Codex,
}

impl CompactionHookClient {
    const fn client_id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }

    const fn config_directory(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude",
            Self::Codex => ".codex",
        }
    }

    const fn config_file(self) -> &'static str {
        match self {
            Self::ClaudeCode => "settings.json",
            Self::Codex => "hooks.json",
        }
    }
}

/// Reversible adapter for the post-compaction routing-reminder hook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactionHookIntegration {
    client: CompactionHookClient,
    user_home: PathBuf,
    compresso_version: String,
}

impl CompactionHookIntegration {
    /// Creates an adapter without consulting global HOME state.
    pub fn new(client: CompactionHookClient, user_home: PathBuf) -> Self {
        Self {
            client,
            user_home,
            compresso_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Overrides the receipt version, primarily for deterministic embedders.
    pub fn with_compresso_version(mut self, version: impl Into<String>) -> Self {
        self.compresso_version = version.into();
        self
    }

    /// Resolves and validates the selected hook configuration path.
    pub fn config_path(&self, scope: &Scope) -> Result<PathBuf, IntegrationError> {
        let root = match scope {
            Scope::User => {
                if !self.user_home.is_absolute()
                    || self
                        .user_home
                        .components()
                        .any(|component| matches!(component, Component::ParentDir))
                {
                    return Err(invalid(
                        "user home must be absolute without parent-directory components",
                    ));
                }
                &self.user_home
            }
            Scope::Project(root) => root,
        };
        let canonical_root = fs::canonicalize(root).map_err(|source| IntegrationError::Io {
            path: root.clone(),
            source,
        })?;
        if !canonical_root.is_dir() {
            return Err(invalid("hook scope root must resolve to a directory"));
        }
        let directory = canonical_root.join(self.client.config_directory());
        let target = directory.join(self.client.config_file());
        for (path, must_be_directory) in [(&directory, true), (&target, false)] {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(invalid(
                        "hook configuration path must not traverse a symbolic link",
                    ));
                }
                Ok(metadata) if metadata.is_dir() != must_be_directory => {
                    return Err(invalid(
                        "hook configuration path has an unexpected file type",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(IntegrationError::Io {
                        path: path.clone(),
                        source,
                    });
                }
            }
        }
        Ok(target)
    }

    /// The exact `hooks.SessionStart` value this adapter writes for a launcher.
    pub fn hook_value(compresso_bin: &Path) -> Result<Value, IntegrationError> {
        let command = hook_command(&resolved_compresso_command(compresso_bin)?)?;
        Ok(json!([
            {
                "matcher": COMPACT_MATCHER,
                "hooks": [
                    {
                        "type": "command",
                        "command": command,
                        "timeout": HOOK_TIMEOUT_SECONDS
                    }
                ]
            }
        ]))
    }

    fn receipt_config_path(&self, path: &Path) -> Result<PathBuf, IntegrationError> {
        let user = self.config_path(&Scope::User)?;
        if path == user {
            return Ok(user);
        }
        let root = path
            .parent()
            .filter(|directory| {
                directory.file_name().and_then(|name| name.to_str())
                    == Some(self.client.config_directory())
            })
            .and_then(Path::parent)
            .ok_or_else(|| invalid("receipt config path is not a hook configuration file"))?;
        let checked = self.config_path(&Scope::Project(root.to_path_buf()))?;
        if checked != path {
            return Err(invalid(
                "receipt config path is not canonical within its project root",
            ));
        }
        Ok(checked)
    }
}

impl Integration for CompactionHookIntegration {
    fn client_id(&self) -> &'static str {
        self.client.client_id()
    }

    /// Client presence is reported by the client's MCP adapter; this adapter
    /// only manages one hook key.
    fn detect(&self, _scope: &Scope) -> Detection {
        Detection::Unsupported {
            reason: "client detection is reported by the MCP integration for this client"
                .to_owned(),
        }
    }

    fn plan_install(
        &self,
        manager: &PlanManager,
        context: &InstallCtx,
    ) -> Result<MutationPlan, IntegrationError> {
        if cfg!(windows) {
            return Err(IntegrationError::Unsupported {
                reason: "the post-compaction hook is not supported on Windows".to_owned(),
            });
        }
        let config_path = self.config_path(&context.scope)?;
        let value = Self::hook_value(&context.compresso_bin)?;
        let mut document = read_json_object(&config_path)?;
        let hooks = document
            .entry("hooks".to_owned())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| invalid("hooks must be a JSON object"))?;
        if hooks
            .get(HOOK_EVENT)
            .is_some_and(|existing| !is_compresso_hook_value(existing))
        {
            return Err(IntegrationError::UserHooksPresent {
                path: config_path,
                key: OWNED_PATH.to_owned(),
                entry: value[0].to_string(),
            });
        }
        hooks.insert(HOOK_EVENT.to_owned(), value);
        manager.issue(MutationRequest {
            operation: MutationKind::Install,
            client: self.client.client_id().to_owned(),
            config_path,
            config_format: ConfigFormat::Json,
            target_contents: render_json(&document)?,
            owned_paths: vec![OWNED_PATH.to_owned()],
            compresso_version: self.compresso_version.clone(),
            prior_receipt: None,
            warnings: Vec::new(),
        })
    }

    fn verify(&self, scope: &Scope) -> VerificationReport {
        let parsed = self
            .config_path(scope)
            .map_err(|_| ())
            .and_then(|path| read_existing_json_object_for_verify(&path));
        let entry = parsed
            .as_ref()
            .ok()
            .and_then(|document| document.get("hooks"))
            .and_then(Value::as_object)
            .and_then(|hooks| hooks.get(HOOK_EVENT));
        let hook_entry = entry.is_some_and(is_compresso_hook_value);
        let binary_resolves = entry
            .and_then(hook_launcher)
            .is_some_and(|launcher| launcher.is_absolute() && is_executable_file(&launcher));
        let check = |name: &str, passed: bool, pass: &str, fail: &str| VerificationCheck {
            name: name.to_owned(),
            passed,
            detail: if passed { pass } else { fail }.to_owned(),
        };
        let checks = vec![
            check(
                "config_parses",
                parsed.is_ok(),
                "hook configuration is valid JSON",
                "hook configuration is missing, unreadable, malformed, or not an object",
            ),
            check(
                "hook_entry",
                hook_entry,
                "post-compaction routing-reminder hook has the expected shape",
                "post-compaction routing-reminder hook is missing or has an unexpected shape",
            ),
            check(
                "binary_resolves",
                binary_resolves,
                "hook command launcher is an absolute executable file",
                "hook command launcher is not an absolute executable file",
            ),
        ];
        VerificationReport {
            client: self.client.client_id().to_owned(),
            verified: checks.iter().all(|check| check.passed),
            checks,
        }
    }

    fn plan_uninstall(
        &self,
        manager: &PlanManager,
        receipt: &InstallReceipt,
    ) -> Result<MutationPlan, IntegrationError> {
        let config_path = self.receipt_config_path(&receipt.config_path)?;
        let mut document = read_existing_json_object(&config_path)?;
        let installed_hash = receipt
            .owned_value_hashes
            .get(OWNED_PATH)
            .ok_or_else(|| invalid("receipt does not contain the hook owned value hash"))?;
        let current_hash = document
            .get("hooks")
            .and_then(Value::as_object)
            .and_then(|hooks| hooks.get(HOOK_EVENT))
            .map(canonical_json_hash)
            .transpose()?;
        let mut warnings = Vec::new();
        if current_hash.as_ref() == Some(installed_hash) {
            document
                .get_mut("hooks")
                .and_then(Value::as_object_mut)
                .expect("a matching owned hash requires an object parent")
                .remove(HOOK_EVENT);
        } else {
            warnings.push(format!("key_modified:{OWNED_PATH}"));
        }
        manager.issue(MutationRequest {
            operation: MutationKind::Uninstall,
            client: self.client.client_id().to_owned(),
            config_path,
            config_format: ConfigFormat::Json,
            target_contents: render_json(&document)?,
            owned_paths: vec![OWNED_PATH.to_owned()],
            compresso_version: receipt.compresso_version.clone(),
            prior_receipt: Some(receipt.clone()),
            warnings,
        })
    }
}

/// Renders the shell command a client runs, single-quoting the launcher.
fn hook_command(launcher: &str) -> Result<String, IntegrationError> {
    if launcher
        .chars()
        .any(|character| character == '\'' || character.is_control())
    {
        return Err(invalid(
            "compresso binary path must not contain quotes or control characters",
        ));
    }
    Ok(format!("'{launcher}' {HOOK_ARGUMENTS}"))
}

/// Extracts the launcher path from a Compresso-written hook value.
fn hook_launcher(value: &Value) -> Option<PathBuf> {
    let [group] = value.as_array()?.as_slice() else {
        return None;
    };
    let group = group.as_object()?;
    if group.len() != 2 || group.get("matcher")?.as_str()? != COMPACT_MATCHER {
        return None;
    }
    let [handler] = group.get("hooks")?.as_array()?.as_slice() else {
        return None;
    };
    let handler = handler.as_object()?;
    if handler.len() != 3
        || handler.get("type")?.as_str()? != "command"
        || handler.get("timeout")?.as_u64()? != HOOK_TIMEOUT_SECONDS
    {
        return None;
    }
    let launcher = handler
        .get("command")?
        .as_str()?
        .strip_suffix(HOOK_ARGUMENTS)?
        .strip_suffix("' ")?
        .strip_prefix('\'')?;
    (!launcher.contains('\'')).then(|| PathBuf::from(launcher))
}

/// Whether `value` is exactly a value this adapter writes for some launcher.
fn is_compresso_hook_value(value: &Value) -> bool {
    hook_launcher(value).is_some_and(|launcher| launcher.is_absolute())
}

fn invalid(reason: &str) -> IntegrationError {
    IntegrationError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::TempDir;

    use super::*;
    use crate::{MemoryReceiptStore, PlanApproval};

    struct Harness {
        _directory: TempDir,
        home: PathBuf,
        project: PathBuf,
        compresso: PathBuf,
    }

    impl Harness {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("temp directory");
            let root = fs::canonicalize(directory.path()).expect("temp root");
            let home = root.join("home");
            let project = root.join("project");
            let compresso = root.join("bin/compresso");
            for path in [&home, &project, &root.join("bin")] {
                fs::create_dir_all(path).expect("directory");
            }
            fs::write(&compresso, "#!/bin/sh\n").expect("launcher");
            fs::set_permissions(&compresso, fs::Permissions::from_mode(0o755)).expect("mode");
            Self {
                _directory: directory,
                home,
                project,
                compresso,
            }
        }

        fn adapter(&self, client: CompactionHookClient) -> CompactionHookIntegration {
            CompactionHookIntegration::new(client, self.home.clone())
                .with_compresso_version("9.9.9")
        }

        fn context(&self, scope: Scope) -> InstallCtx {
            InstallCtx {
                compresso_bin: self.compresso.clone(),
                scope,
                session_recall: false,
            }
        }

        fn install(
            &self,
            client: CompactionHookClient,
            scope: Scope,
        ) -> (PlanManager, MemoryReceiptStore, InstallReceipt) {
            let manager = PlanManager::new("compaction-hook-test").expect("manager");
            let receipts = MemoryReceiptStore::default();
            let adapter = self.adapter(client);
            let plan = adapter
                .plan_install(&manager, &self.context(scope))
                .expect("install plan");
            let receipt = adapter
                .install(&manager, &PlanApproval::from(&plan), &receipts)
                .expect("install applies");
            (manager, receipts, receipt)
        }

        fn claude_settings(&self) -> PathBuf {
            self.home.join(".claude/settings.json")
        }
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).expect("config readable")).expect("config JSON")
    }

    fn write(path: &Path, value: &Value) {
        fs::create_dir_all(path.parent().expect("parent")).expect("parent created");
        fs::write(path, serde_json::to_vec_pretty(value).expect("JSON")).expect("config written");
    }

    #[test]
    fn test_install_writes_compact_matcher_hook_and_preserves_other_settings() {
        let harness = Harness::new();
        write(
            &harness.claude_settings(),
            &json!({
                "permissions": { "allow": ["Bash(ls)"] },
                "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [] }] }
            }),
        );

        let (_, _, receipt) = harness.install(CompactionHookClient::ClaudeCode, Scope::User);

        let settings = read(&harness.claude_settings());
        assert_eq!(settings["permissions"]["allow"], json!(["Bash(ls)"]));
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        let group = &settings["hooks"]["SessionStart"][0];
        assert_eq!(group["matcher"], "compact");
        assert_eq!(group["hooks"][0]["type"], "command");
        assert_eq!(
            group["hooks"][0]["command"],
            format!(
                "'{}' mcp routing-reminder --session-start-hook",
                harness.compresso.display()
            )
        );
        assert_eq!(receipt.client, "claude-code");
        assert_eq!(receipt.keys_owned, vec!["hooks.SessionStart".to_owned()]);
        assert_eq!(receipt.config_path, harness.claude_settings());
    }

    #[test]
    fn test_codex_install_creates_hooks_json_in_user_and_project_scope() {
        let harness = Harness::new();

        harness.install(CompactionHookClient::Codex, Scope::User);
        harness.install(
            CompactionHookClient::Codex,
            Scope::Project(harness.project.clone()),
        );

        for path in [
            harness.home.join(".codex/hooks.json"),
            harness.project.join(".codex/hooks.json"),
        ] {
            let hooks = read(&path);
            assert_eq!(hooks["hooks"]["SessionStart"][0]["matcher"], "compact");
            assert_eq!(hooks.as_object().expect("object").len(), 1);
        }
        assert!(!harness.home.join(".codex/config.toml").exists());
    }

    #[test]
    fn test_install_refuses_to_rewrite_user_defined_session_start_hooks() {
        let harness = Harness::new();
        let existing = json!({
            "hooks": { "SessionStart": [{ "matcher": "startup", "hooks": [
                { "type": "command", "command": "echo hello" }
            ] }] }
        });
        write(&harness.claude_settings(), &existing);
        let before = fs::read(harness.claude_settings()).expect("settings");
        let manager = PlanManager::new("compaction-hook-test").expect("manager");

        let error = harness
            .adapter(CompactionHookClient::ClaudeCode)
            .plan_install(&manager, &harness.context(Scope::User))
            .expect_err("user hooks must not be rewritten");

        let reason = error.to_string();
        let IntegrationError::UserHooksPresent { path, key, entry } = error else {
            panic!("expected a typed user-hooks refusal");
        };
        assert_eq!(path, harness.claude_settings());
        assert_eq!(key, "hooks.SessionStart");
        assert!(entry.contains("routing-reminder --session-start-hook"));
        assert!(entry.contains("\"matcher\":\"compact\""));
        assert!(reason.contains("by hand"));
        assert!(reason.contains("routing-reminder --session-start-hook"));
        assert_eq!(
            fs::read(harness.claude_settings()).expect("settings"),
            before
        );
    }

    #[test]
    fn test_repeated_install_is_idempotent_and_follows_a_moved_launcher() {
        let harness = Harness::new();
        harness.install(CompactionHookClient::ClaudeCode, Scope::User);
        let first = fs::read(harness.claude_settings()).expect("settings");

        harness.install(CompactionHookClient::ClaudeCode, Scope::User);
        assert_eq!(
            fs::read(harness.claude_settings()).expect("settings"),
            first
        );
        let groups = read(&harness.claude_settings())["hooks"]["SessionStart"]
            .as_array()
            .expect("groups")
            .len();
        assert_eq!(groups, 1);

        let moved = harness.compresso.with_file_name("compresso-next");
        fs::copy(&harness.compresso, &moved).expect("moved launcher");
        let manager = PlanManager::new("compaction-hook-test").expect("manager");
        let adapter = harness.adapter(CompactionHookClient::ClaudeCode);
        let plan = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: moved.clone(),
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect("upgrade plan");
        adapter
            .install(
                &manager,
                &PlanApproval::from(&plan),
                &MemoryReceiptStore::default(),
            )
            .expect("upgrade applies");
        let settings = read(&harness.claude_settings());
        let groups = settings["hooks"]["SessionStart"]
            .as_array()
            .expect("groups");
        assert_eq!(groups.len(), 1);
        assert!(
            groups[0]["hooks"][0]["command"]
                .as_str()
                .is_some_and(|command| command.contains("compresso-next"))
        );
    }

    #[test]
    fn test_uninstall_removes_only_the_owned_key() {
        let harness = Harness::new();
        write(
            &harness.claude_settings(),
            &json!({ "model": "example", "hooks": { "Stop": [] } }),
        );
        let (manager, receipts, receipt) =
            harness.install(CompactionHookClient::ClaudeCode, Scope::User);
        let adapter = harness.adapter(CompactionHookClient::ClaudeCode);

        let plan = adapter
            .plan_uninstall(&manager, &receipt)
            .expect("uninstall plan");
        let report = adapter
            .uninstall(&manager, &PlanApproval::from(&plan), &receipts)
            .expect("uninstall applies");

        assert_eq!(report.removed_keys, vec!["hooks.SessionStart".to_owned()]);
        assert_eq!(
            read(&harness.claude_settings()),
            json!({ "model": "example", "hooks": { "Stop": [] } })
        );
    }

    #[test]
    fn test_uninstall_retains_hooks_the_user_added_after_install() {
        let harness = Harness::new();
        let (manager, receipts, receipt) =
            harness.install(CompactionHookClient::ClaudeCode, Scope::User);
        let mut settings = read(&harness.claude_settings());
        settings["hooks"]["SessionStart"]
            .as_array_mut()
            .expect("groups")
            .push(json!({ "matcher": "startup", "hooks": [] }));
        write(&harness.claude_settings(), &settings);
        let adapter = harness.adapter(CompactionHookClient::ClaudeCode);

        let plan = adapter
            .plan_uninstall(&manager, &receipt)
            .expect("uninstall plan");
        let report = adapter
            .uninstall(&manager, &PlanApproval::from(&plan), &receipts)
            .expect("uninstall applies");

        assert!(report.removed_keys.is_empty());
        assert_eq!(report.retained_keys, vec!["hooks.SessionStart".to_owned()]);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("key_modified"))
        );
        assert_eq!(read(&harness.claude_settings()), settings);
    }

    #[test]
    fn test_verify_checks_shape_and_launcher() {
        let harness = Harness::new();
        let adapter = harness.adapter(CompactionHookClient::ClaudeCode);
        assert!(!adapter.verify(&Scope::User).verified);

        harness.install(CompactionHookClient::ClaudeCode, Scope::User);
        assert!(adapter.verify(&Scope::User).verified);

        fs::remove_file(&harness.compresso).expect("launcher removed");
        let report = adapter.verify(&Scope::User);
        assert!(!report.verified);
        let failed = report
            .checks
            .iter()
            .filter(|check| !check.passed)
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(failed, vec!["binary_resolves"]);
    }

    #[test]
    fn test_symlinked_config_directory_and_file_are_rejected_without_writes() {
        let harness = Harness::new();
        let outside = harness.home.with_file_name("outside");
        fs::create_dir_all(&outside).expect("outside");
        std::os::unix::fs::symlink(&outside, harness.home.join(".claude")).expect("symlink");
        let manager = PlanManager::new("compaction-hook-test").expect("manager");
        let adapter = harness.adapter(CompactionHookClient::ClaudeCode);

        assert!(
            adapter
                .plan_install(&manager, &harness.context(Scope::User))
                .is_err()
        );
        assert_eq!(fs::read_dir(&outside).expect("outside").count(), 0);

        fs::create_dir_all(harness.project.join(".claude")).expect("project config dir");
        fs::write(outside.join("settings.json"), "{}").expect("outside settings");
        std::os::unix::fs::symlink(
            outside.join("settings.json"),
            harness.project.join(".claude/settings.json"),
        )
        .expect("file symlink");
        assert!(
            adapter
                .plan_install(
                    &manager,
                    &harness.context(Scope::Project(harness.project.clone()))
                )
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(outside.join("settings.json")).expect("outside settings"),
            "{}"
        );
    }

    #[test]
    fn test_launcher_paths_that_cannot_be_quoted_are_rejected() {
        assert!(hook_command("/opt/it's/compresso").is_err());
        assert!(hook_command("/opt/new\nline/compresso").is_err());
        assert_eq!(
            hook_command("/opt/with space/compresso").expect("quoted"),
            "'/opt/with space/compresso' mcp routing-reminder --session-start-hook"
        );
    }

    #[test]
    fn test_hook_value_recognition_rejects_lookalikes() {
        let harness = Harness::new();
        let value = CompactionHookIntegration::hook_value(&harness.compresso).expect("value");
        assert!(is_compresso_hook_value(&value));
        assert_eq!(hook_launcher(&value), Some(harness.compresso.clone()));

        let mut other_matcher = value.clone();
        other_matcher[0]["matcher"] = json!("startup");
        let mut extra_field = value.clone();
        extra_field[0]["hooks"][0]["async"] = json!(true);
        let mut other_command = value.clone();
        other_command[0]["hooks"][0]["command"] = json!("rm -rf / routing-reminder");
        let mut two_groups = value.clone();
        two_groups
            .as_array_mut()
            .expect("array")
            .push(json!({ "matcher": "compact", "hooks": [] }));
        for lookalike in [other_matcher, extra_field, other_command, two_groups] {
            assert!(!is_compresso_hook_value(&lookalike), "{lookalike}");
        }
    }
}
