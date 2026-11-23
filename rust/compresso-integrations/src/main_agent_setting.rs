//! The `agent` setting that makes a Claude Code project session run the
//! main-session agent (E17-51, D116).
//!
//! Claude Code reads `{"agent": "<name>"}` from its settings files and runs
//! the whole session as that agent, as if started with `--agent <name>`.
//! This adapter writes exactly that one key, with the value
//! [`MAIN_AGENT_NAME`], into the project's personal settings file
//! `.claude/settings.local.json` (owner decision, D116). It owns only the
//! top-level `agent` key (`ConfigFormat::Json`, owned path `agent`).
//!
//! Splice rules, each covered by a test:
//!
//! - A missing file is created holding only the key.
//! - Otherwise only that key changes: the new member is spliced into the
//!   existing text after the last member, copying the file's own separator
//!   and colon spacing, so every other byte (formatting, key order, trailing
//!   newline) is kept. Uninstall removes exactly the spliced bytes.
//! - A file that is not valid JSON, whose root is not an object, that is not
//!   UTF-8, or that names `agent` more than once is refused, naming the file.
//! - An existing `agent` value that is not ours is never overwritten:
//!   [`MainAgentSettingIntegration::foreign_agent`] reports it so the caller
//!   skips this step with a warning; `plan_install` refuses it too.
//! - The write is the plan manager's atomic temporary-file-and-rename in the
//!   same directory; `.claude` and the file must not be symbolic links.
//! - Uninstall removes the key only while it still holds `compresso-main`;
//!   it deletes the file only when nothing else is left in it and the
//!   install receipt says Compresso created the file.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::main_agent::{MAIN_AGENT_NAME, check_path_types, checked_project_claude_dir};
use crate::plan::MutationRequest;
use crate::receipt::CREATED_FILE_BACKUP_SENTINEL;
use crate::text_fidelity::strip_bom_str;
use crate::{
    ConfigFormat, Detection, InstallCtx, InstallReceipt, Integration, IntegrationError,
    MutationKind, MutationPlan, PlanManager, Scope, VerificationCheck, VerificationReport,
};

/// The settings key this adapter owns.
pub const AGENT_SETTING_KEY: &str = "agent";
/// The settings file, relative to the project root.
pub const LOCAL_SETTINGS_FILE: &str = ".claude/settings.local.json";
const CLIENT_ID: &str = "claude-code";
/// Largest existing settings file inspected.
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

/// What the `agent` key of a settings file holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentSetting {
    /// The file or the key does not exist.
    Absent,
    /// The key names the main-session agent.
    Ours,
    /// The key holds another value, shown as compact JSON.
    Other(String),
}

/// Reversible adapter for the `agent` key in `.claude/settings.local.json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainAgentSettingIntegration {
    user_home: PathBuf,
    compresso_version: String,
}

impl MainAgentSettingIntegration {
    /// Creates an adapter without consulting global HOME state. `user_home`
    /// is used only to refuse a project root that is the home directory.
    pub fn new(user_home: PathBuf) -> Self {
        Self {
            user_home,
            compresso_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Overrides the receipt version, primarily for deterministic embedders.
    pub fn with_compresso_version(mut self, version: impl Into<String>) -> Self {
        self.compresso_version = version.into();
        self
    }

    /// Resolves and validates the settings file path for `scope`: a project
    /// other than the home directory, with neither `.claude` nor the file a
    /// symbolic link.
    pub fn config_path(&self, scope: &Scope) -> Result<PathBuf, IntegrationError> {
        let claude = checked_project_claude_dir(scope, &self.user_home, "agent setting")?;
        let target = claude.join("settings.local.json");
        check_path_types(&[(&claude, true), (&target, false)])?;
        Ok(target)
    }

    /// What the settings file's `agent` key holds; an error names a file
    /// that cannot be read or parsed as a JSON object.
    pub fn agent_setting(&self, scope: &Scope) -> Result<AgentSetting, IntegrationError> {
        let path = self.config_path(scope)?;
        let Some(text) = read_existing(&path)? else {
            return Ok(AgentSetting::Absent);
        };
        let document = parse_object(&path, &text)?;
        Ok(classify(document.get(AGENT_SETTING_KEY)))
    }

    /// The other value an existing `agent` key holds, if any: such a key is
    /// never overwritten.
    pub fn foreign_agent(&self, scope: &Scope) -> Result<Option<String>, IntegrationError> {
        Ok(match self.agent_setting(scope)? {
            AgentSetting::Other(value) => Some(value),
            AgentSetting::Absent | AgentSetting::Ours => None,
        })
    }

    fn receipt_config_path(&self, path: &Path) -> Result<PathBuf, IntegrationError> {
        let named = |path: &Path, name: &str| {
            path.file_name().and_then(|value| value.to_str()) == Some(name)
        };
        let root = Some(path)
            .filter(|file| named(file, "settings.local.json"))
            .and_then(Path::parent)
            .filter(|claude| named(claude, ".claude"))
            .and_then(Path::parent)
            .ok_or_else(|| invalid("receipt path is not a project's local settings file"))?;
        let checked = self.config_path(&Scope::Project(root.to_path_buf()))?;
        if checked != path {
            return Err(invalid(
                "receipt path is not canonical within its project root",
            ));
        }
        Ok(checked)
    }
}

impl Integration for MainAgentSettingIntegration {
    fn client_id(&self) -> &'static str {
        CLIENT_ID
    }

    /// Client presence is reported by the Claude Code MCP adapter.
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
        let config_path = self.config_path(&context.scope)?;
        let target_contents = match read_existing(&config_path)? {
            None => created_settings(),
            Some(text) => {
                let document = parse_object(&config_path, &text)?;
                match classify(document.get(AGENT_SETTING_KEY)) {
                    AgentSetting::Ours => text,
                    AgentSetting::Other(value) => {
                        return Err(IntegrationError::InvalidRequest {
                            reason: format!(
                                "{} already sets \"agent\" to {value}; Compresso never overwrites it",
                                config_path.display()
                            ),
                        });
                    }
                    AgentSetting::Absent => insert_agent(&config_path, &text)?,
                }
            }
        };
        manager.issue(MutationRequest {
            operation: MutationKind::Install,
            client: CLIENT_ID.to_owned(),
            config_path,
            config_format: ConfigFormat::Json,
            target_contents,
            owned_paths: vec![AGENT_SETTING_KEY.to_owned()],
            compresso_version: self.compresso_version.clone(),
            prior_receipt: None,
            warnings: Vec::new(),
        })
    }

    fn verify(&self, scope: &Scope) -> VerificationReport {
        let setting = self.agent_setting(scope);
        let ours = matches!(setting, Ok(AgentSetting::Ours));
        VerificationReport {
            client: CLIENT_ID.to_owned(),
            verified: ours,
            checks: vec![VerificationCheck {
                name: "main_agent_setting".to_owned(),
                passed: ours,
                detail: match setting {
                    Ok(AgentSetting::Ours) => {
                        format!("{LOCAL_SETTINGS_FILE} sets \"agent\" to \"{MAIN_AGENT_NAME}\"")
                    }
                    Ok(AgentSetting::Absent) => {
                        format!("{LOCAL_SETTINGS_FILE} does not set \"agent\"")
                    }
                    Ok(AgentSetting::Other(_)) => format!(
                        "{LOCAL_SETTINGS_FILE} sets \"agent\" to another agent; Compresso leaves it unchanged"
                    ),
                    Err(_) => format!("{LOCAL_SETTINGS_FILE} cannot be read as a JSON object"),
                },
            }],
        }
    }

    fn plan_uninstall(
        &self,
        manager: &PlanManager,
        receipt: &InstallReceipt,
    ) -> Result<MutationPlan, IntegrationError> {
        let config_path = self.receipt_config_path(&receipt.config_path)?;
        let Some(text) = read_existing(&config_path)? else {
            return Err(IntegrationError::UninstallTargetMissing { path: config_path });
        };
        let document = parse_object(&config_path, &text)?;
        let (target_contents, warnings) = match classify(document.get(AGENT_SETTING_KEY)) {
            AgentSetting::Ours => {
                let removed = remove_agent(&config_path, &text)?;
                let empty = parse_object(&config_path, &removed)?.is_empty();
                if empty && receipt.backup_path == CREATED_FILE_BACKUP_SENTINEL {
                    (String::new(), Vec::new())
                } else {
                    (removed, Vec::new())
                }
            }
            AgentSetting::Absent | AgentSetting::Other(_) => {
                (text, vec![format!("key_modified:{AGENT_SETTING_KEY}")])
            }
        };
        manager.issue(MutationRequest {
            operation: MutationKind::Uninstall,
            client: CLIENT_ID.to_owned(),
            config_path,
            config_format: ConfigFormat::Json,
            target_contents,
            owned_paths: vec![AGENT_SETTING_KEY.to_owned()],
            compresso_version: receipt.compresso_version.clone(),
            prior_receipt: Some(receipt.clone()),
            warnings,
        })
    }
}

/// The file created when none exists: the one key and a trailing newline.
fn created_settings() -> String {
    format!("{{\n  \"{AGENT_SETTING_KEY}\": \"{MAIN_AGENT_NAME}\"\n}}\n")
}

fn classify(value: Option<&Value>) -> AgentSetting {
    match value {
        None => AgentSetting::Absent,
        Some(Value::String(name)) if name == MAIN_AGENT_NAME => AgentSetting::Ours,
        Some(other) => AgentSetting::Other(printable(&other.to_string())),
    }
}

/// A value read from a user file, made safe for a terminal and bounded.
fn printable(text: &str) -> String {
    let mut rendered = String::new();
    for character in text.chars().take(80) {
        if character.is_ascii_graphic() || character == ' ' {
            rendered.push(character);
        } else {
            rendered.extend(character.escape_unicode());
        }
    }
    if text.chars().count() > 80 {
        rendered.push_str("...");
    }
    rendered
}

/// Reads an existing settings file as UTF-8, `None` when it does not exist.
fn read_existing(path: &Path) -> Result<Option<String>, IntegrationError> {
    let io_error = |source| IntegrationError::Io {
        path: path.to_path_buf(),
        source,
    };
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(IntegrationError::InvalidRequest {
                reason: format!("{} is not a regular file", path.display()),
            });
        }
        Ok(metadata) if metadata.len() > MAX_SETTINGS_BYTES => {
            return Err(IntegrationError::InvalidRequest {
                reason: format!("{} is too large to inspect", path.display()),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(source)),
    }
    let bytes = fs::read(path).map_err(io_error)?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| IntegrationError::InvalidRequest {
            reason: format!("{} is not UTF-8 text", path.display()),
        })
}

/// Parses `text` as a JSON object with at most one `agent` key, naming
/// `path` in every refusal.
fn parse_object(
    path: &Path,
    text: &str,
) -> Result<serde_json::Map<String, Value>, IntegrationError> {
    let refuse = |what: &str| IntegrationError::InvalidRequest {
        reason: format!(
            "{} {what}; fix or remove it, or install with --no-main-agent",
            path.display()
        ),
    };
    let value: Value =
        serde_json::from_str(strip_bom_str(text)).map_err(|_| refuse("is not valid JSON"))?;
    let Value::Object(object) = value else {
        return Err(refuse("does not hold a JSON object"));
    };
    let members = scan_members(strip_bom_str(text)).ok_or_else(|| refuse("is not valid JSON"))?;
    if members
        .members
        .iter()
        .filter(|member| member.key == AGENT_SETTING_KEY)
        .count()
        > 1
    {
        return Err(refuse("names \"agent\" more than once"));
    }
    Ok(object)
}

/// One top-level member of a JSON object, as byte offsets into the text.
#[derive(Debug)]
struct Member {
    key: String,
    /// Offset of the key's opening quote.
    key_start: usize,
    /// Offset just after the key's closing quote.
    key_end: usize,
    /// Offset of the value's first byte.
    value_start: usize,
    /// Offset just after the value's last byte.
    value_end: usize,
}

/// The top-level members of an object text, with the offsets of its braces.
#[derive(Debug)]
struct Members {
    open: usize,
    close: usize,
    members: Vec<Member>,
}

/// Scans the top level of a JSON object text that `serde_json` accepted.
fn scan_members(text: &str) -> Option<Members> {
    let bytes = text.as_bytes();
    let mut at = skip_whitespace(bytes, 0);
    if bytes.get(at) != Some(&b'{') {
        return None;
    }
    let open = at;
    at = skip_whitespace(bytes, at + 1);
    let mut members = Vec::new();
    if bytes.get(at) == Some(&b'}') {
        return Some(Members {
            open,
            close: at,
            members,
        });
    }
    loop {
        let key_start = at;
        let key_end = skip_string(bytes, at)?;
        let key: String = serde_json::from_str(&text[key_start..key_end]).ok()?;
        at = skip_whitespace(bytes, key_end);
        if bytes.get(at) != Some(&b':') {
            return None;
        }
        let value_start = skip_whitespace(bytes, at + 1);
        let value_end = skip_value(bytes, value_start)?;
        members.push(Member {
            key,
            key_start,
            key_end,
            value_start,
            value_end,
        });
        at = skip_whitespace(bytes, value_end);
        match bytes.get(at) {
            Some(b',') => at = skip_whitespace(bytes, at + 1),
            Some(b'}') => {
                return Some(Members {
                    open,
                    close: at,
                    members,
                });
            }
            _ => return None,
        }
    }
}

fn skip_whitespace(bytes: &[u8], mut at: usize) -> usize {
    while matches!(bytes.get(at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        at += 1;
    }
    at
}

/// The offset just after the string starting at `at`.
fn skip_string(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    let mut index = at + 1;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// The offset just after the value starting at `at`.
fn skip_value(bytes: &[u8], at: usize) -> Option<usize> {
    match bytes.get(at)? {
        b'"' => skip_string(bytes, at),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut index = at;
            while let Some(&byte) = bytes.get(index) {
                match byte {
                    b'"' => {
                        index = skip_string(bytes, index)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            None
        }
        _ => {
            let mut index = at;
            while let Some(&byte) = bytes.get(index) {
                if matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                    break;
                }
                index += 1;
            }
            (index > at).then_some(index)
        }
    }
}

fn bom_length(text: &str) -> usize {
    text.len() - strip_bom_str(text).len()
}

/// `text` with `"agent": "compresso-main"` spliced in after the last member,
/// copying the separator before the last member and its colon spacing; every
/// other byte is kept.
fn insert_agent(path: &Path, text: &str) -> Result<String, IntegrationError> {
    let offset = bom_length(text);
    let body = &text[offset..];
    let scanned = scan_members(body).ok_or_else(|| IntegrationError::InvalidRequest {
        reason: format!("{} is not valid JSON", path.display()),
    })?;
    let value = format!("\"{MAIN_AGENT_NAME}\"");
    let key = format!("\"{AGENT_SETTING_KEY}\"");
    let mut result = String::with_capacity(text.len() + 32);
    result.push_str(&text[..offset]);
    match scanned.members.last() {
        None => {
            let newline = if body.contains("\r\n") { "\r\n" } else { "\n" };
            result.push_str(&body[..=scanned.open]);
            result.push_str(&format!("{newline}  {key}: {value}{newline}"));
            result.push_str(&body[scanned.close..]);
        }
        Some(last) => {
            let separator_start = match scanned.members.len() {
                1 => scanned.open + 1,
                count => {
                    let previous = &scanned.members[count - 2];
                    // After the comma that follows the previous value.
                    skip_whitespace(body.as_bytes(), previous.value_end) + 1
                }
            };
            let separator = &body[separator_start..last.key_start];
            let colon = &body[last.key_end..last.value_start];
            result.push_str(&body[..last.value_end]);
            result.push(',');
            result.push_str(separator);
            result.push_str(&key);
            result.push_str(colon);
            result.push_str(&value);
            result.push_str(&body[last.value_end..]);
        }
    }
    Ok(result)
}

/// `text` without its `agent` member and the separator that joined it, the
/// inverse of [`insert_agent`]; every other byte is kept.
fn remove_agent(path: &Path, text: &str) -> Result<String, IntegrationError> {
    let offset = bom_length(text);
    let body = &text[offset..];
    let scanned = scan_members(body).ok_or_else(|| IntegrationError::InvalidRequest {
        reason: format!("{} is not valid JSON", path.display()),
    })?;
    let Some(index) = scanned
        .members
        .iter()
        .position(|member| member.key == AGENT_SETTING_KEY)
    else {
        return Ok(text.to_owned());
    };
    let count = scanned.members.len();
    let (start, end) = if count == 1 {
        (scanned.open + 1, scanned.close)
    } else if index + 1 == count {
        // The last member: from the end of the previous value.
        (
            scanned.members[index - 1].value_end,
            scanned.members[index].value_end,
        )
    } else {
        // Up to the next member's key.
        (
            scanned.members[index].key_start,
            scanned.members[index + 1].key_start,
        )
    };
    let mut result = String::with_capacity(text.len());
    result.push_str(&text[..offset]);
    result.push_str(&body[..start]);
    result.push_str(&body[end..]);
    Ok(result)
}

fn invalid(reason: &str) -> IntegrationError {
    IntegrationError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::{MemoryReceiptStore, PlanApproval};

    struct Harness {
        _directory: TempDir,
        home: PathBuf,
        project: PathBuf,
        adapter: MainAgentSettingIntegration,
        manager: PlanManager,
        receipts: MemoryReceiptStore,
    }

    impl Harness {
        fn new() -> Self {
            let directory = TempDir::new().expect("temporary directory");
            let home = directory.path().join("home");
            let project = directory.path().join("project");
            fs::create_dir_all(&home).expect("home");
            fs::create_dir_all(project.join(".claude")).expect("project");
            let home = fs::canonicalize(home).expect("home canonicalizes");
            let project = fs::canonicalize(project).expect("project canonicalizes");
            Self {
                _directory: directory,
                adapter: MainAgentSettingIntegration::new(home.clone())
                    .with_compresso_version("0.1.0"),
                home,
                project,
                manager: PlanManager::new("main-agent-setting-test").expect("manager"),
                receipts: MemoryReceiptStore::default(),
            }
        }

        fn scope(&self) -> Scope {
            Scope::Project(self.project.clone())
        }

        fn path(&self) -> PathBuf {
            self.project.join(LOCAL_SETTINGS_FILE)
        }

        fn plan(&self) -> Result<MutationPlan, IntegrationError> {
            self.adapter.plan_install(
                &self.manager,
                &InstallCtx {
                    compresso_bin: self.project.join("bin/compresso"),
                    scope: self.scope(),
                    session_recall: true,
                },
            )
        }

        fn install(&self) -> Result<InstallReceipt, IntegrationError> {
            let plan = self.plan()?;
            self.adapter
                .install(&self.manager, &PlanApproval::from(&plan), &self.receipts)
        }

        fn uninstall(&self, receipt: &InstallReceipt) -> crate::RestoreReport {
            let plan = self
                .adapter
                .plan_uninstall(&self.manager, receipt)
                .expect("uninstall plan");
            self.adapter
                .uninstall(&self.manager, &PlanApproval::from(&plan), &self.receipts)
                .expect("uninstall")
        }

        fn read(&self) -> String {
            fs::read_to_string(self.path()).expect("settings")
        }
    }

    #[test]
    fn test_a_missing_settings_file_is_created_with_only_the_key_and_removed_on_uninstall() {
        let harness = Harness::new();
        let receipt = harness.install().expect("install");
        assert_eq!(harness.read(), "{\n  \"agent\": \"compresso-main\"\n}\n");
        assert_eq!(receipt.backup_path, CREATED_FILE_BACKUP_SENTINEL);
        assert_eq!(receipt.keys_owned, [AGENT_SETTING_KEY]);
        assert!(harness.adapter.verify(&harness.scope()).verified);
        harness.uninstall(&receipt);
        assert!(!harness.path().exists());
    }

    #[test]
    fn test_only_the_agent_key_changes_and_every_other_byte_is_kept() {
        for original in [
            "{\n  \"permissions\": {\n    \"allow\": [\"Bash(ls)\"]\n  }\n}\n",
            "{\n\t\"b\": 1,\n\t\"a\": [1, {\"x\": \"}\"}]\n}",
            "{\"z\":true,\"y\":null}\n",
            "{ \"spaced\" :  \"v\" }\n\n",
            "{\r\n  \"crlf\": \"yes\",\r\n  \"n\": 2\r\n}\r\n",
            "\u{feff}{\n  \"bom\": 1\n}\n",
            "{}\n",
            "{}",
        ] {
            let harness = Harness::new();
            fs::write(harness.path(), original).unwrap();
            let receipt = harness.install().expect("install");
            let installed = harness.read();
            let mut expected: Value = serde_json::from_str(strip_bom_str(original)).unwrap();
            expected[AGENT_SETTING_KEY] = Value::from(MAIN_AGENT_NAME);
            assert_eq!(
                serde_json::from_str::<Value>(strip_bom_str(&installed)).unwrap(),
                expected,
                "{installed:?}"
            );
            // Every original byte survives in order around the one member.
            assert_eq!(
                remove_agent(&harness.path(), &installed).unwrap(),
                original,
                "{installed:?}"
            );
            harness.uninstall(&receipt);
            // A file the user had is never deleted; its bytes come back.
            assert_eq!(harness.read(), original);
        }
    }

    #[test]
    fn test_unparseable_or_non_object_settings_are_refused_naming_the_file() {
        for original in ["{\"a\": ", "[1, 2]", "\"agent\""] {
            let harness = Harness::new();
            fs::write(harness.path(), original).unwrap();
            let error = harness.plan().expect_err("refused").to_string();
            assert!(error.contains("settings.local.json"), "{error}");
            assert_eq!(harness.read(), original);
        }
    }

    #[test]
    fn test_a_settings_file_naming_agent_twice_is_refused() {
        let harness = Harness::new();
        let original = "{\"agent\": \"x\", \"agent\": \"compresso-main\"}";
        fs::write(harness.path(), original).unwrap();
        let error = harness.plan().expect_err("refused").to_string();
        assert!(error.contains("more than once"), "{error}");
        assert_eq!(harness.read(), original);
    }

    #[test]
    fn test_another_agent_value_is_never_overwritten() {
        for value in ["\"my-agent\"", "null", "{\"name\": \"x\"}"] {
            let harness = Harness::new();
            let original = format!("{{\n  \"agent\": {value}\n}}\n");
            fs::write(harness.path(), &original).unwrap();
            assert!(
                harness
                    .adapter
                    .foreign_agent(&harness.scope())
                    .unwrap()
                    .is_some()
            );
            assert!(harness.plan().is_err());
            assert_eq!(harness.read(), original);
            assert!(!harness.adapter.verify(&harness.scope()).verified);
        }
    }

    #[test]
    fn test_uninstall_keeps_a_key_that_no_longer_holds_our_agent() {
        let harness = Harness::new();
        fs::write(harness.path(), "{\n  \"model\": \"x\"\n}\n").unwrap();
        let receipt = harness.install().expect("install");
        let changed = "{\n  \"model\": \"x\",\n  \"agent\": \"mine\"\n}\n";
        fs::write(harness.path(), changed).unwrap();
        let report = harness.uninstall(&receipt);
        assert!(report.warnings.iter().any(|w| w.contains("key_modified")));
        assert_eq!(harness.read(), changed);
    }

    #[test]
    fn test_a_created_file_that_gained_other_keys_is_kept_on_uninstall() {
        let harness = Harness::new();
        let receipt = harness.install().expect("install");
        let grown = "{\n  \"agent\": \"compresso-main\",\n  \"model\": \"x\"\n}\n";
        fs::write(harness.path(), grown).unwrap();
        harness.uninstall(&receipt);
        assert_eq!(harness.read(), "{\n  \"model\": \"x\"\n}\n");
    }

    #[test]
    fn test_a_file_we_did_not_create_is_never_deleted_even_when_left_empty() {
        let harness = Harness::new();
        fs::write(harness.path(), "{\n  \"model\": \"x\"\n}\n").unwrap();
        let receipt = harness.install().expect("install");
        // The user removed their own key meanwhile.
        fs::write(harness.path(), "{\n  \"agent\": \"compresso-main\"\n}\n").unwrap();
        harness.uninstall(&receipt);
        assert!(harness.path().exists());
        assert_eq!(
            serde_json::from_str::<Value>(&harness.read()).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn test_a_json_file_is_deleted_only_for_a_receipt_that_created_it() {
        // The plan manager itself refuses an empty JSON target otherwise.
        let harness = Harness::new();
        fs::write(harness.path(), "{\"model\": \"x\"}").unwrap();
        let mut receipt = harness.install().expect("install");
        fs::write(harness.path(), "{\"agent\": \"compresso-main\"}").unwrap();
        receipt.backup_path = "/elsewhere/backup".to_owned();
        let request = MutationRequest {
            operation: MutationKind::Uninstall,
            client: CLIENT_ID.to_owned(),
            config_path: harness.path(),
            config_format: ConfigFormat::Json,
            target_contents: String::new(),
            owned_paths: vec![AGENT_SETTING_KEY.to_owned()],
            compresso_version: "0.1.0".to_owned(),
            prior_receipt: Some(receipt),
            warnings: Vec::new(),
        };
        assert!(harness.manager.issue(request).is_err());
    }

    #[test]
    fn test_an_empty_json_install_target_is_refused() {
        let harness = Harness::new();
        let request = MutationRequest {
            operation: MutationKind::Install,
            client: CLIENT_ID.to_owned(),
            config_path: harness.path(),
            config_format: ConfigFormat::Json,
            target_contents: String::new(),
            owned_paths: vec![AGENT_SETTING_KEY.to_owned()],
            compresso_version: "0.1.0".to_owned(),
            prior_receipt: None,
            warnings: Vec::new(),
        };
        let error = harness.manager.issue(request).expect_err("refused");
        assert!(error.to_string().contains("must not be empty"), "{error}");
    }

    #[test]
    fn test_dry_run_planning_writes_nothing() {
        let harness = Harness::new();
        harness.plan().expect("plan");
        assert!(!harness.path().exists());
        fs::write(harness.path(), "{\"a\": 1}").unwrap();
        harness.plan().expect("plan");
        assert_eq!(harness.read(), "{\"a\": 1}");
    }

    #[test]
    fn test_symbolic_links_user_scope_and_the_home_root_are_refused() {
        let harness = Harness::new();
        assert!(harness.adapter.config_path(&Scope::User).is_err());
        assert!(
            harness
                .adapter
                .config_path(&Scope::Project(harness.home.clone()))
                .is_err()
        );
        #[cfg(unix)]
        {
            let elsewhere = harness.home.join("settings.json");
            fs::write(&elsewhere, "{}").unwrap();
            std::os::unix::fs::symlink(&elsewhere, harness.path()).unwrap();
            assert!(harness.plan().is_err());
            assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "{}");
        }
    }

    #[test]
    fn test_reinstall_over_our_key_changes_nothing() {
        let harness = Harness::new();
        fs::write(harness.path(), "{\"a\": 1}\n").unwrap();
        harness.install().expect("install");
        let first = harness.read();
        harness.install().expect("reinstall");
        assert_eq!(harness.read(), first);
    }
}
