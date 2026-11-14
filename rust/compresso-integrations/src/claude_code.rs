//! Claude Code MCP configuration adapter.
//!
//! The adapter owns only `mcpServers.compresso`. Client discovery is a
//! bounded filesystem lookup; version output is supplied as evidence by the
//! caller so this crate never needs process execution.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::plan::MutationRequest;
use crate::text_fidelity::strip_bom;
use crate::{
    ConfigFormat, Detection, InstallCtx, InstallReceipt, Integration, IntegrationError,
    MutationKind, MutationPlan, PlanManager, Scope, VerificationCheck, VerificationReport,
};

const CLIENT_ID: &str = "claude-code";
const OWNED_PATH: &str = "mcpServers.compresso";
const USER_CONFIG_NAME: &str = ".claude.json";
const PROJECT_CONFIG_NAME: &str = ".mcp.json";
const MAX_DISCOVERY_DIRECTORIES: usize = 64;

/// Filesystem-only client discovery inputs.
///
/// `version_evidence` is the captured client version text supplied by a
/// process-capable outer boundary. This adapter parses but never obtains it by
/// spawning a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeCodeDiscovery {
    path_entries: Vec<PathBuf>,
    version_evidence: Option<String>,
}

impl ClaudeCodeDiscovery {
    /// Creates bounded discovery evidence from explicit PATH directories.
    pub fn new(
        path_entries: impl IntoIterator<Item = PathBuf>,
        version_evidence: Option<String>,
    ) -> Self {
        Self {
            path_entries: path_entries
                .into_iter()
                .take(MAX_DISCOVERY_DIRECTORIES)
                .collect(),
            version_evidence,
        }
    }

    fn resolve_executable(&self) -> Option<PathBuf> {
        self.path_entries.iter().find_map(|directory| {
            command_names().iter().find_map(|name| {
                let candidate = directory.join(name);
                if !is_executable_file(&candidate) {
                    return None;
                }
                fs::canonicalize(candidate).ok()
            })
        })
    }

    fn parsed_version(&self) -> Option<String> {
        self.version_evidence
            .as_deref()
            .and_then(parse_semver_evidence)
    }
}

/// Reversible adapter for Claude Code user and project MCP configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeCodeIntegration {
    user_home: PathBuf,
    discovery: ClaudeCodeDiscovery,
    compresso_version: String,
}

impl ClaudeCodeIntegration {
    /// Creates an adapter without consulting global HOME or PATH state.
    pub fn new(user_home: PathBuf, discovery: ClaudeCodeDiscovery) -> Self {
        Self {
            user_home,
            discovery,
            compresso_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Overrides the receipt version, primarily for deterministic embedders.
    pub fn with_compresso_version(mut self, version: impl Into<String>) -> Self {
        self.compresso_version = version.into();
        self
    }

    /// Resolves and validates the selected Claude Code MCP configuration path.
    pub fn config_path(&self, scope: &Scope) -> Result<PathBuf, IntegrationError> {
        self.mutation_config_path(scope)
    }

    fn mutation_config_path(&self, scope: &Scope) -> Result<PathBuf, IntegrationError> {
        match scope {
            Scope::User => checked_user_config_path(&self.user_home),
            Scope::Project(root) => checked_project_config_path(root),
        }
    }

    fn receipt_config_path(&self, path: &Path) -> Result<PathBuf, IntegrationError> {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(USER_CONFIG_NAME) => {
                let user_config = checked_user_config_path(&self.user_home)?;
                if path == user_config {
                    return Ok(user_config);
                }
                return Err(invalid_request(
                    "Claude Code receipt user config path is not canonical within the home root",
                ));
            }
            Some(PROJECT_CONFIG_NAME) => {}
            _ => {
                return Err(invalid_request(
                    "Claude Code receipt config path is neither the user nor project config",
                ));
            }
        }
        let Some(root) = path.parent() else {
            return Err(invalid_request(
                "Claude Code receipt config path has no project root",
            ));
        };
        let checked = checked_project_config_path(root)?;
        if checked != path {
            return Err(invalid_request(
                "Claude Code receipt config path is not canonical within its project root",
            ));
        }
        Ok(checked)
    }
}

impl Integration for ClaudeCodeIntegration {
    fn client_id(&self) -> &'static str {
        CLIENT_ID
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let Some(_executable) = self.discovery.resolve_executable() else {
            return Detection::NotFound;
        };
        let Some(version) = self.discovery.parsed_version() else {
            return Detection::Unsupported {
                reason: "client version evidence is missing or not semantic versioning".to_owned(),
            };
        };
        let config_path = match self.mutation_config_path(scope) {
            Ok(path) => path,
            Err(error) => {
                return Detection::Unsupported {
                    reason: error.to_string(),
                };
            }
        };
        Detection::Installed {
            version,
            config_path,
        }
    }

    fn plan_install(
        &self,
        manager: &PlanManager,
        context: &InstallCtx,
    ) -> Result<MutationPlan, IntegrationError> {
        let config_path = self.mutation_config_path(&context.scope)?;
        let command = resolved_compresso_command(&context.compresso_bin)?;
        let args = expected_server_args(&context.scope)?;
        let mut document = read_json_object(&config_path)?;
        let servers = object_at_or_insert(&mut document, "mcpServers")?;
        servers.insert(
            "compresso".to_owned(),
            json!({
                "type": "stdio",
                "command": command,
                "args": args
            }),
        );
        manager.issue(MutationRequest {
            operation: MutationKind::Install,
            client: CLIENT_ID.to_owned(),
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
            .mutation_config_path(scope)
            .map_err(|_| ())
            .and_then(|path| read_existing_json_object_for_verify(&path));
        let config_parses = parsed.is_ok();
        let entry = parsed
            .as_ref()
            .ok()
            .and_then(|document| document.get("mcpServers"))
            .and_then(Value::as_object)
            .and_then(|servers| servers.get("compresso"));
        let server_entry_valid = expected_server_args(scope)
            .ok()
            .is_some_and(|args| entry.is_some_and(|entry| is_exact_server_entry(entry, &args)));
        let binary_resolves = entry
            .and_then(|entry| entry.get("command"))
            .and_then(Value::as_str)
            .map(Path::new)
            .is_some_and(|command| command.is_absolute() && is_executable_file(command));
        let checks = vec![
            VerificationCheck {
                name: "config_parses".to_owned(),
                passed: config_parses,
                detail: if config_parses {
                    "configuration is valid JSON".to_owned()
                } else {
                    "configuration is missing, unreadable, malformed, or not an object".to_owned()
                },
            },
            VerificationCheck {
                name: "server_entry".to_owned(),
                passed: server_entry_valid,
                detail: if server_entry_valid {
                    "Compresso stdio entry has the expected shape".to_owned()
                } else {
                    "Compresso stdio entry is missing or has an unexpected shape".to_owned()
                },
            },
            VerificationCheck {
                name: "binary_resolves".to_owned(),
                passed: binary_resolves,
                detail: if binary_resolves {
                    "configured command is an absolute executable file".to_owned()
                } else {
                    "configured command is not an absolute executable file".to_owned()
                },
            },
        ];
        VerificationReport {
            client: CLIENT_ID.to_owned(),
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
        let installed_hash = receipt.owned_value_hashes.get(OWNED_PATH).ok_or_else(|| {
            invalid_request("receipt does not contain the Claude Code owned value hash")
        })?;
        let current_hash = document
            .get("mcpServers")
            .and_then(Value::as_object)
            .and_then(|servers| servers.get("compresso"))
            .map(canonical_json_hash)
            .transpose()?;
        let mut warnings = Vec::new();
        if current_hash.as_ref() == Some(installed_hash) {
            let servers = document
                .get_mut("mcpServers")
                .and_then(Value::as_object_mut)
                .expect("a matching owned hash requires an object parent");
            servers.remove("compresso");
        } else {
            warnings.push(format!("key_modified:{OWNED_PATH}"));
        }
        manager.issue(MutationRequest {
            operation: MutationKind::Uninstall,
            client: CLIENT_ID.to_owned(),
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

fn checked_user_config_path(home: &Path) -> Result<PathBuf, IntegrationError> {
    if !home.is_absolute() {
        return Err(invalid_request(
            "Claude Code user home must be an absolute path",
        ));
    }
    if home
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid_request(
            "Claude Code user home must not contain parent-directory components",
        ));
    }
    let canonical_home = fs::canonicalize(home).map_err(|source| IntegrationError::Io {
        path: home.to_path_buf(),
        source,
    })?;
    checked_config_child(&canonical_home, USER_CONFIG_NAME, "user")
}

fn checked_project_config_path(root: &Path) -> Result<PathBuf, IntegrationError> {
    let canonical_root = fs::canonicalize(root).map_err(|source| IntegrationError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    checked_config_child(&canonical_root, PROJECT_CONFIG_NAME, "project")
}

fn checked_config_child(
    canonical_root: &Path,
    config_name: &str,
    scope_name: &str,
) -> Result<PathBuf, IntegrationError> {
    if !canonical_root.is_absolute() {
        return Err(invalid_request(format!(
            "Claude Code {scope_name} root must resolve to an absolute path"
        )));
    }
    let metadata = fs::metadata(canonical_root).map_err(|source| IntegrationError::Io {
        path: canonical_root.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(invalid_request(format!(
            "Claude Code {scope_name} scope root must resolve to a directory"
        )));
    }

    let target = canonical_root.join(config_name);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(invalid_request(format!(
            "Claude Code {scope_name} config must not be a symbolic link"
        ))),
        Ok(_) => {
            let canonical_target =
                fs::canonicalize(&target).map_err(|source| IntegrationError::Io {
                    path: target,
                    source,
                })?;
            if !canonical_target.starts_with(canonical_root) {
                return Err(invalid_request(format!(
                    "Claude Code {scope_name} config resolves outside its canonical root"
                )));
            }
            Ok(canonical_target)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(target),
        Err(source) => Err(IntegrationError::Io {
            path: target,
            source,
        }),
    }
}

pub(crate) fn read_json_object(path: &Path) -> Result<Map<String, Value>, IntegrationError> {
    match fs::read(path) {
        Ok(bytes) => parse_json_object(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn read_existing_json_object(
    path: &Path,
) -> Result<Map<String, Value>, IntegrationError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(IntegrationError::UninstallTargetMissing {
                path: path.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(IntegrationError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse_json_object(&bytes)
}

pub(crate) fn read_existing_json_object_for_verify(path: &Path) -> Result<Map<String, Value>, ()> {
    fs::read(path)
        .map_err(|_| ())
        .and_then(|bytes| parse_json_object(&bytes).map_err(|_| ()))
}

fn parse_json_object(bytes: &[u8]) -> Result<Map<String, Value>, IntegrationError> {
    serde_json::from_slice::<Value>(strip_bom(bytes))
        .map_err(|error| invalid_request(format!("configuration JSON is malformed: {error}")))?
        .as_object()
        .cloned()
        .ok_or_else(|| invalid_request("configuration JSON must be an object"))
}

fn object_at_or_insert<'a>(
    document: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>, IntegrationError> {
    document
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| invalid_request(format!("{key} must be a JSON object")))
}

pub(crate) fn render_json(document: &Map<String, Value>) -> Result<String, IntegrationError> {
    let mut rendered =
        serde_json::to_string_pretty(&Value::Object(document.clone())).map_err(|error| {
            invalid_request(format!("configuration JSON cannot be rendered: {error}"))
        })?;
    rendered.push('\n');
    Ok(rendered)
}

pub(crate) fn canonical_json_hash(value: &Value) -> Result<String, IntegrationError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        invalid_request(format!("owned JSON value cannot be canonicalized: {error}"))
    })?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(crate) fn resolved_compresso_command(path: &Path) -> Result<String, IntegrationError> {
    if !is_normalized_absolute_path(path) {
        return Err(invalid_request(
            "compresso binary path must be normalized and absolute",
        ));
    }
    let resolved = fs::canonicalize(path).map_err(|source| IntegrationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !is_executable_file(&resolved) {
        return Err(invalid_request(
            "compresso binary path must resolve to an executable file",
        ));
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid_request("compresso binary path must be valid UTF-8"))
}

fn is_normalized_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
}

fn expected_server_args(scope: &Scope) -> Result<Vec<String>, IntegrationError> {
    let mut args = vec!["mcp".to_owned(), "serve".to_owned()];
    if let Scope::Project(root) = scope {
        let canonical_root = fs::canonicalize(root).map_err(|source| IntegrationError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        if !canonical_root.is_dir() {
            return Err(invalid_request(
                "Claude Code project scope root must resolve to a directory",
            ));
        }
        let root = canonical_root
            .to_str()
            .ok_or_else(|| invalid_request("Claude Code project root must be valid UTF-8"))?;
        args.extend(["--root".to_owned(), root.to_owned()]);
    }
    Ok(args)
}

fn is_exact_server_entry(value: &Value, expected_args: &[String]) -> bool {
    let Some(entry) = value.as_object() else {
        return false;
    };
    entry.get("type").and_then(Value::as_str) == Some("stdio")
        && entry
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| Path::new(command).is_absolute())
        && entry.get("args") == Some(&json!(expected_args))
}

pub(crate) fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(windows)]
fn command_names() -> &'static [&'static str] {
    &["claude.exe", "claude.cmd", "claude.bat", "claude"]
}

#[cfg(not(windows))]
fn command_names() -> &'static [&'static str] {
    &["claude"]
}

fn parse_semver_evidence(evidence: &str) -> Option<String> {
    evidence
        .split_whitespace()
        .map(|part| {
            let trimmed = part.trim_matches(|character: char| {
                matches!(character, ',' | ';' | ':' | '(' | ')' | '[' | ']')
            });
            trimmed.strip_prefix('v').unwrap_or(trimmed)
        })
        .find(|candidate| is_semver(candidate))
        .map(str::to_owned)
}

fn is_semver(candidate: &str) -> bool {
    let (core_and_prerelease, build) = candidate
        .split_once('+')
        .map_or((candidate, None), |(left, right)| (left, Some(right)));
    if build.is_some_and(|identifiers| !valid_semver_identifiers(identifiers, false)) {
        return false;
    }
    let (core, prerelease) = core_and_prerelease
        .split_once('-')
        .map_or((core_and_prerelease, None), |(left, right)| {
            (left, Some(right))
        });
    if prerelease.is_some_and(|identifiers| !valid_semver_identifiers(identifiers, true)) {
        return false;
    }
    let mut core = core.split('.');
    [core.next(), core.next(), core.next()]
        .into_iter()
        .all(|part| part.is_some_and(is_semver_number))
        && core.next().is_none()
}

fn valid_semver_identifiers(value: &str, reject_leading_zero_numeric: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!reject_leading_zero_numeric
                    || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || is_semver_number(identifier))
        })
}

fn is_semver_number(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn invalid_request(reason: impl Into<String>) -> IntegrationError {
    IntegrationError::InvalidRequest {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{Integration, MemoryReceiptStore, PlanApproval, ReceiptStore, Scope};

    struct Harness {
        _directory: TempDir,
        home: PathBuf,
        project: PathBuf,
        compresso: PathBuf,
        adapter: ClaudeCodeIntegration,
    }

    impl Harness {
        fn new() -> Self {
            let directory = tempdir().expect("temp directory");
            let home = directory.path().join("home");
            let project = directory.path().join("project");
            let bin = directory.path().join("bin");
            fs::create_dir_all(&home).expect("home created");
            fs::create_dir_all(&project).expect("project created");
            fs::create_dir_all(&bin).expect("bin created");
            let compresso = bin.join("compresso");
            let claude = bin.join(command_names()[0]);
            write_executable(&compresso);
            write_executable(&claude);
            let adapter = ClaudeCodeIntegration::new(
                home.clone(),
                ClaudeCodeDiscovery::new([bin], Some("Claude Code v2.1.216".to_owned())),
            )
            .with_compresso_version("0.1.0");
            Self {
                _directory: directory,
                home,
                project,
                compresso,
                adapter,
            }
        }

        fn user_config(&self) -> PathBuf {
            fs::canonicalize(&self.home)
                .expect("home canonicalized")
                .join(USER_CONFIG_NAME)
        }

        fn install(&self, scope: Scope) -> (PlanManager, MemoryReceiptStore, InstallReceipt) {
            let manager = PlanManager::new("claude-adapter-test").expect("manager");
            let receipts = MemoryReceiptStore::default();
            let plan = self
                .adapter
                .plan_install(
                    &manager,
                    &InstallCtx {
                        compresso_bin: self.compresso.clone(),
                        scope,
                    },
                )
                .expect("install plan");
            let receipt = self
                .adapter
                .install(&manager, &PlanApproval::from(&plan), &receipts)
                .expect("install applies");
            (manager, receipts, receipt)
        }
    }

    #[test]
    fn test_install_preserves_existing_servers() {
        let harness = Harness::new();
        let original = json!({
            "theme": {"name": "night", "unknown": [1, 2, 3]},
            "mcpServers": {
                "other": {"command": "/opt/other", "args": ["--safe"]}
            }
        });
        write_json(&harness.user_config(), &original);

        let (_manager, _receipts, _receipt) = harness.install(Scope::User);
        let installed = read_json(&harness.user_config());
        assert_eq!(installed["theme"], original["theme"]);
        assert_eq!(
            installed["mcpServers"]["other"],
            original["mcpServers"]["other"]
        );
        assert_eq!(
            installed["mcpServers"]["compresso"],
            json!({
                "type": "stdio",
                "command": harness.compresso.to_str().expect("UTF-8 command"),
                "args": ["mcp", "serve"]
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_install_rejects_non_normalized_absolute_launcher_paths() {
        let harness = Harness::new();
        let parent = harness.compresso.parent().expect("bin directory");
        let parent_text = parent.to_str().expect("UTF-8 bin directory");
        let nested = parent.join("nested");
        fs::create_dir(&nested).expect("nested directory created");
        let cases = [
            PathBuf::from(format!("{parent_text}/./compresso")),
            PathBuf::from(format!("{parent_text}//compresso")),
            nested.join("../compresso"),
        ];
        let manager = PlanManager::new("claude-non-normalized-launcher").expect("manager");

        for path in cases {
            let error = harness
                .adapter
                .plan_install(
                    &manager,
                    &InstallCtx {
                        compresso_bin: path,
                        scope: Scope::User,
                    },
                )
                .expect_err("non-normalized launcher path rejected");
            assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        }
        assert!(!harness.user_config().exists());
    }

    #[test]
    fn test_install_preserves_untouched_key_order_after_two_space_normalization() {
        let harness = Harness::new();
        fs::write(
            harness.user_config(),
            br#"{"z-top":{"third":3,"first":1,"second":{"right":true,"left":false}},"mcpServers":{"z-server":{"z-last":9,"a-first":1},"a-server":{"second":2,"first":1}},"alpha":{"omega":24,"beta":2}}"#,
        )
        .expect("ordered JSON written");

        let (_manager, _receipts, _receipt) = harness.install(Scope::User);
        let rendered = fs::read_to_string(harness.user_config()).expect("installed JSON read");
        let installed = read_json(&harness.user_config());

        assert_eq!(object_keys(&installed), ["z-top", "mcpServers", "alpha"]);
        assert_eq!(
            object_keys(&installed["z-top"]),
            ["third", "first", "second"]
        );
        assert_eq!(
            object_keys(&installed["z-top"]["second"]),
            ["right", "left"]
        );
        assert_eq!(
            object_keys(&installed["mcpServers"]),
            ["z-server", "a-server", "compresso"]
        );
        assert_eq!(
            object_keys(&installed["mcpServers"]["z-server"]),
            ["z-last", "a-first"]
        );
        assert!(rendered.contains("\n  \"z-top\": {\n    \"third\": 3,"));
        assert!(rendered.ends_with("}\n"));
        assert!(rendered.lines().all(|line| {
            let indentation = line.len() - line.trim_start_matches(' ').len();
            indentation % 2 == 0
        }));
    }

    #[test]
    fn test_verify_fails_on_missing_binary() {
        let harness = Harness::new();
        write_json(
            &harness.user_config(),
            &json!({
                "mcpServers": {
                    "compresso": {
                        "type": "stdio",
                        "command": harness.home.join("missing-compresso"),
                        "args": ["mcp", "serve"]
                    }
                }
            }),
        );

        let report = harness.adapter.verify(&Scope::User);
        assert!(!report.verified);
        assert_eq!(check(&report, "config_parses"), Some(true));
        assert_eq!(check(&report, "server_entry"), Some(true));
        assert_eq!(check(&report, "binary_resolves"), Some(false));
    }

    #[test]
    fn test_uninstall_removes_only_owned_key() {
        let harness = Harness::new();
        let original = json!({
            "top": {"preserved": true},
            "mcpServers": {"other": {"url": "http://127.0.0.1:1234"}}
        });
        write_json(&harness.user_config(), &original);
        let (manager, receipts, receipt) = harness.install(Scope::User);

        let plan = harness
            .adapter
            .plan_uninstall(&manager, &receipt)
            .expect("uninstall plan");
        let report = harness
            .adapter
            .uninstall(&manager, &PlanApproval::from(&plan), &receipts)
            .expect("uninstall applies");

        assert_eq!(read_json(&harness.user_config()), original);
        assert_eq!(report.removed_keys, [OWNED_PATH]);
        assert!(report.retained_keys.is_empty());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn test_uninstall_skips_user_modified_entry() {
        let harness = Harness::new();
        let (manager, receipts, receipt) = harness.install(Scope::User);
        let mut modified = read_json(&harness.user_config());
        modified["mcpServers"]["compresso"]["args"] = json!(["mcp", "serve", "--changed"]);
        write_json(&harness.user_config(), &modified);

        let plan = harness
            .adapter
            .plan_uninstall(&manager, &receipt)
            .expect("uninstall plan");
        assert_eq!(plan.warnings, [format!("key_modified:{OWNED_PATH}")]);
        let report = harness
            .adapter
            .uninstall(&manager, &PlanApproval::from(&plan), &receipts)
            .expect("safe uninstall report");

        assert_eq!(read_json(&harness.user_config()), modified);
        assert!(report.removed_keys.is_empty());
        assert_eq!(report.retained_keys, [OWNED_PATH]);
        assert_eq!(report.warnings, [format!("key_modified:{OWNED_PATH}")]);
        assert!(
            receipts
                .active_for(CLIENT_ID, &harness.user_config())
                .expect("active receipt query")
                .is_some()
        );
    }

    #[test]
    fn test_project_scope_pins_and_verifies_exact_canonical_root() {
        let harness = Harness::new();
        let user_original = json!({"mcpServers": {"user": {"command": "untouched"}}});
        write_json(&harness.user_config(), &user_original);

        let (_manager, _receipts, receipt) =
            harness.install(Scope::Project(harness.project.clone()));
        let project_path = fs::canonicalize(&harness.project)
            .expect("project root canonicalized")
            .join(PROJECT_CONFIG_NAME);

        assert_eq!(receipt.config_path, project_path);
        assert_eq!(read_json(&harness.user_config()), user_original);
        let installed = read_json(&project_path);
        assert_eq!(
            installed["mcpServers"]["compresso"]["args"],
            json!([
                "mcp",
                "serve",
                "--root",
                fs::canonicalize(&harness.project)
                    .expect("canonical project root")
                    .to_str()
                    .expect("UTF-8 project root")
            ])
        );
        assert!(
            harness
                .adapter
                .verify(&Scope::Project(harness.project.clone()))
                .verified
        );

        let mut changed = installed;
        changed["mcpServers"]["compresso"]["args"][3] =
            json!(fs::canonicalize(&harness.home).expect("canonical home"));
        write_json(&project_path, &changed);
        assert_eq!(
            check(
                &harness
                    .adapter
                    .verify(&Scope::Project(harness.project.clone())),
                "server_entry"
            ),
            Some(false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_user_scope_rejects_config_symlink_before_read_or_plan() {
        use std::os::unix::fs::symlink;

        let harness = Harness::new();
        let outside = harness._directory.path().join("outside-user-secret.json");
        let outside_bytes = b"{ definitely-not-json SECRET_OUTSIDE_HOME";
        fs::write(&outside, outside_bytes).expect("outside target written");
        let user_config = harness.user_config();
        symlink(&outside, &user_config).expect("outside user symlink created");
        let manager = PlanManager::new("user-symlink").expect("manager");

        let error = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::User,
                },
            )
            .expect_err("user config symlink rejected before planning");

        match error {
            IntegrationError::InvalidRequest { reason } => {
                assert!(reason.contains("symbolic link"));
                assert!(!reason.contains("SECRET_OUTSIDE_HOME"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(matches!(
            harness.adapter.detect(&Scope::User),
            Detection::Unsupported { reason } if reason.contains("symbolic link")
        ));
        assert_eq!(
            check(&harness.adapter.verify(&Scope::User), "config_parses"),
            Some(false)
        );
        assert_eq!(
            fs::read(&outside).expect("outside target read"),
            outside_bytes
        );
        assert!(
            fs::symlink_metadata(&user_config)
                .expect("user symlink metadata")
                .file_type()
                .is_symlink()
        );
        assert!(integration_sidecars(&harness.home).is_empty());
        assert!(integration_sidecars(harness._directory.path()).is_empty());
    }

    #[test]
    fn test_user_scope_rejects_relative_and_parent_directory_homes() {
        let harness = Harness::new();
        let original = b"{\"user_only\":true}\n";
        fs::write(harness.user_config(), original).expect("user config written");
        let relative_home = PathBuf::from(format!(
            "relative-home-{}",
            harness
                ._directory
                .path()
                .file_name()
                .expect("temporary directory name")
                .to_string_lossy()
        ));
        let parent_home = harness.home.join("unused-child").join("..");

        for (home, expected_reason) in [
            (relative_home.clone(), "absolute path"),
            (parent_home, "parent-directory"),
        ] {
            let adapter = ClaudeCodeIntegration::new(home, harness.adapter.discovery.clone());
            let manager = PlanManager::new("unsafe-home").expect("manager");
            let error = adapter
                .plan_install(
                    &manager,
                    &InstallCtx {
                        compresso_bin: harness.compresso.clone(),
                        scope: Scope::User,
                    },
                )
                .expect_err("unsafe user home rejected");

            assert!(matches!(
                error,
                IntegrationError::InvalidRequest { reason }
                    if reason.contains(expected_reason)
            ));
            assert!(matches!(
                adapter.detect(&Scope::User),
                Detection::Unsupported { reason } if reason.contains(expected_reason)
            ));
        }

        assert!(!relative_home.exists());
        assert_eq!(
            fs::read(harness.user_config()).expect("user config read"),
            original
        );
        assert!(integration_sidecars(&harness.home).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_user_uninstall_rejects_config_replaced_by_outside_symlink() {
        use std::os::unix::fs::symlink;

        let harness = Harness::new();
        let (manager, receipts, receipt) = harness.install(Scope::User);
        let outside = harness
            ._directory
            .path()
            .join("outside-user-uninstall-secret.json");
        let outside_bytes = b"{ definitely-not-json SECRET_OUTSIDE_USER_UNINSTALL";
        fs::write(&outside, outside_bytes).expect("outside target written");
        fs::remove_file(&receipt.config_path).expect("installed config removed");
        symlink(&outside, &receipt.config_path).expect("outside symlink created");

        let error = harness
            .adapter
            .plan_uninstall(&manager, &receipt)
            .expect_err("outside symlink rejected before uninstall planning");

        match error {
            IntegrationError::InvalidRequest { reason } => {
                assert!(reason.contains("symbolic link"));
                assert!(!reason.contains("SECRET_OUTSIDE_USER_UNINSTALL"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            fs::read(&outside).expect("outside target read"),
            outside_bytes
        );
        assert!(
            receipts
                .active_for(CLIENT_ID, &receipt.config_path)
                .expect("active receipt query")
                .is_some()
        );
        assert!(integration_sidecars(&harness.home).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_project_scope_rejects_outside_config_symlink_before_read_or_plan() {
        use std::os::unix::fs::symlink;

        let harness = Harness::new();
        let outside = harness._directory.path().join("outside-secret.json");
        let outside_bytes = b"{ definitely-not-json SECRET_OUTSIDE_PROJECT";
        fs::write(&outside, outside_bytes).expect("outside target written");
        let project_config = harness.project.join(PROJECT_CONFIG_NAME);
        symlink(&outside, &project_config).expect("outside symlink created");
        let user_original = b"{\"user_only\":true}\n";
        fs::write(harness.user_config(), user_original).expect("user config written");
        let manager = PlanManager::new("project-symlink").expect("manager");

        let error = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::Project(harness.project.clone()),
                },
            )
            .expect_err("project config symlink rejected before planning");

        match error {
            IntegrationError::InvalidRequest { reason } => {
                assert!(reason.contains("symbolic link"));
                assert!(!reason.contains("SECRET_OUTSIDE_PROJECT"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            fs::read(&outside).expect("outside target read"),
            outside_bytes
        );
        assert_eq!(
            fs::read(harness.user_config()).expect("user config read"),
            user_original
        );
        assert!(
            fs::symlink_metadata(&project_config)
                .expect("project symlink metadata")
                .file_type()
                .is_symlink()
        );
        assert!(integration_sidecars(&harness.project).is_empty());
        assert!(integration_sidecars(harness._directory.path()).is_empty());
        assert_eq!(
            check(
                &harness
                    .adapter
                    .verify(&Scope::Project(harness.project.clone())),
                "config_parses"
            ),
            Some(false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_project_scope_canonicalizes_symlinked_root() {
        use std::os::unix::fs::symlink;

        let harness = Harness::new();
        let actual_root = harness._directory.path().join("actual-project");
        let alias_root = harness._directory.path().join("project-alias");
        fs::create_dir(&actual_root).expect("actual project created");
        symlink(&actual_root, &alias_root).expect("project root alias created");
        let user_original = b"{\"user_only\":true}\n";
        fs::write(harness.user_config(), user_original).expect("user config written");

        let (_manager, _receipts, receipt) = harness.install(Scope::Project(alias_root.clone()));
        let canonical_root = fs::canonicalize(actual_root).expect("project root canonicalized");

        assert_eq!(
            receipt.config_path,
            canonical_root.join(PROJECT_CONFIG_NAME)
        );
        assert!(read_json(&receipt.config_path)["mcpServers"]["compresso"].is_object());
        assert_eq!(
            read_json(&receipt.config_path)["mcpServers"]["compresso"]["args"],
            json!([
                "mcp",
                "serve",
                "--root",
                canonical_root.to_str().expect("UTF-8 canonical root")
            ])
        );
        assert_eq!(
            fs::read(harness.user_config()).expect("user config read"),
            user_original
        );
        assert_eq!(
            harness.adapter.detect(&Scope::Project(alias_root)),
            Detection::Installed {
                version: "2.1.216".to_owned(),
                config_path: canonical_root.join(PROJECT_CONFIG_NAME),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_project_uninstall_rejects_config_replaced_by_outside_symlink() {
        use std::os::unix::fs::symlink;

        let harness = Harness::new();
        let (manager, receipts, receipt) = harness.install(Scope::Project(harness.project.clone()));
        let outside = harness
            ._directory
            .path()
            .join("outside-uninstall-secret.json");
        let outside_bytes = b"{ definitely-not-json SECRET_OUTSIDE_UNINSTALL";
        fs::write(&outside, outside_bytes).expect("outside target written");
        fs::remove_file(&receipt.config_path).expect("installed config removed");
        symlink(&outside, &receipt.config_path).expect("outside symlink created");

        let error = harness
            .adapter
            .plan_uninstall(&manager, &receipt)
            .expect_err("outside symlink rejected before uninstall planning");

        match error {
            IntegrationError::InvalidRequest { reason } => {
                assert!(reason.contains("symbolic link"));
                assert!(!reason.contains("SECRET_OUTSIDE_UNINSTALL"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            fs::read(&outside).expect("outside target read"),
            outside_bytes
        );
        assert!(
            receipts
                .active_for(CLIENT_ID, &receipt.config_path)
                .expect("active receipt query")
                .is_some()
        );
        assert!(
            integration_sidecars(receipt.config_path.parent().expect("project root")).is_empty()
        );
    }

    #[test]
    fn test_project_scope_rejects_missing_root_without_creating_it() {
        let harness = Harness::new();
        let missing_root = harness._directory.path().join("missing-project");
        let manager = PlanManager::new("missing-project").expect("manager");

        let error = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::Project(missing_root.clone()),
                },
            )
            .expect_err("missing project rejected");

        assert!(matches!(
            error,
            IntegrationError::Io { source, .. }
                if source.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(!missing_root.exists());
        assert!(integration_sidecars(harness._directory.path()).is_empty());
    }

    #[test]
    fn test_project_scope_does_not_consult_unsafe_user_home() {
        let harness = Harness::new();
        let relative_home = PathBuf::from(format!(
            "unsafe-relative-home-{}",
            harness
                ._directory
                .path()
                .file_name()
                .expect("temporary directory name")
                .to_string_lossy()
        ));
        let adapter =
            ClaudeCodeIntegration::new(relative_home.clone(), harness.adapter.discovery.clone())
                .with_compresso_version("0.1.0");
        let manager = PlanManager::new("project-isolation").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let scope = Scope::Project(harness.project.clone());
        let install_plan = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: scope.clone(),
                },
            )
            .expect("project install plan does not inspect user home");
        let receipt = adapter
            .install(&manager, &PlanApproval::from(&install_plan), &receipts)
            .expect("project install applies");

        assert!(adapter.verify(&scope).verified);
        let uninstall_plan = adapter
            .plan_uninstall(&manager, &receipt)
            .expect("project uninstall plan does not inspect user home");
        adapter
            .uninstall(&manager, &PlanApproval::from(&uninstall_plan), &receipts)
            .expect("project uninstall applies");
        assert_eq!(
            object_keys(&read_json(&receipt.config_path)["mcpServers"]),
            Vec::<&str>::new()
        );
        assert!(!relative_home.exists());
    }

    #[test]
    fn test_detect_uses_bounded_injected_evidence() {
        let harness = Harness::new();
        assert_eq!(harness.adapter.client_id(), "claude-code");
        assert_eq!(
            harness.adapter.detect(&Scope::User),
            Detection::Installed {
                version: "2.1.216".to_owned(),
                config_path: harness.user_config(),
            }
        );

        let missing = ClaudeCodeIntegration::new(
            harness.home.clone(),
            ClaudeCodeDiscovery::new([], Some("1.2.3".to_owned())),
        );
        assert_eq!(missing.detect(&Scope::User), Detection::NotFound);

        let invalid_version = ClaudeCodeIntegration::new(
            harness.home.clone(),
            ClaudeCodeDiscovery::new(
                [harness
                    .compresso
                    .parent()
                    .expect("bin parent")
                    .to_path_buf()],
                Some("unknown".to_owned()),
            ),
        );
        assert!(matches!(
            invalid_version.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));

        let mut bounded_entries = (0..MAX_DISCOVERY_DIRECTORIES)
            .map(|index| harness.home.join(format!("missing-{index}")))
            .collect::<Vec<_>>();
        bounded_entries.push(
            harness
                .compresso
                .parent()
                .expect("bin parent")
                .to_path_buf(),
        );
        let bounded = ClaudeCodeIntegration::new(
            harness.home.clone(),
            ClaudeCodeDiscovery::new(bounded_entries, Some("1.2.3".to_owned())),
        );
        assert_eq!(bounded.detect(&Scope::User), Detection::NotFound);

        let version_directory = harness.home.join("versions").join("3.4.5-beta.1+local");
        fs::create_dir_all(&version_directory).expect("version directory created");
        write_executable(&version_directory.join(command_names()[0]));
        let missing_evidence = ClaudeCodeIntegration::new(
            harness.home.clone(),
            ClaudeCodeDiscovery::new([version_directory], None),
        );
        assert!(matches!(
            missing_evidence.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
        let invalid_evidence_takes_precedence = ClaudeCodeIntegration::new(
            harness.home.clone(),
            ClaudeCodeDiscovery::new(
                [harness.home.join("versions").join("3.4.5-beta.1+local")],
                Some("not-a-version".to_owned()),
            ),
        );
        assert!(matches!(
            invalid_evidence_takes_precedence.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
    }

    #[test]
    fn test_version_evidence_requires_strict_semver() {
        for valid in [
            "0.0.0",
            "1.2.3",
            "1.0.0-alpha",
            "1.0.0-alpha.1+build.05",
            "1.0.0-0A.0-1+001",
        ] {
            assert!(is_semver(valid), "expected valid SemVer: {valid}");
            assert_eq!(
                parse_semver_evidence(&format!("Claude Code v{valid}")),
                Some(valid.to_owned())
            );
        }
        for invalid in [
            "1.2",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-alpha..1",
            "1.2.3+build..1",
            "1.2.3-alpha.01",
            "1.2.3+one+two",
            "1.2.3-alpha_1",
            "vv1.2.3",
        ] {
            assert!(!is_semver(invalid), "expected invalid SemVer: {invalid}");
            assert_eq!(parse_semver_evidence(invalid), None);
        }
    }

    #[test]
    fn test_malformed_config_fails_open_without_mutation() {
        let harness = Harness::new();
        fs::write(harness.user_config(), b"{ malformed").expect("malformed config written");
        let before = fs::read(harness.user_config()).expect("malformed config readable");
        let manager = PlanManager::new("malformed").expect("manager");

        let error = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::User,
                },
            )
            .expect_err("malformed config rejected");

        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        assert_eq!(
            fs::read(harness.user_config()).expect("malformed config readable"),
            before
        );
        assert!(!harness.adapter.verify(&Scope::User).verified);
    }

    #[test]
    fn test_install_rejects_config_drift() {
        let harness = Harness::new();
        write_json(&harness.user_config(), &json!({"untouched": 1}));
        let manager = PlanManager::new("drift").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let plan = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::User,
                },
            )
            .expect("install plan");
        write_json(&harness.user_config(), &json!({"untouched": 2}));

        let error = harness
            .adapter
            .install(&manager, &PlanApproval::from(&plan), &receipts)
            .expect_err("drift rejected");

        assert!(matches!(error, IntegrationError::ConfigDrift { .. }));
        assert!(receipts.all().expect("receipt snapshot").is_empty());
        assert_eq!(read_json(&harness.user_config())["untouched"], 2);
    }

    #[test]
    fn test_uninstall_missing_config_fails_without_side_effects() {
        let harness = Harness::new();
        let (manager, receipts, receipt) = harness.install(Scope::User);
        fs::remove_file(harness.user_config()).expect("installed config deleted");
        assert!(integration_sidecars(&harness.home).is_empty());

        let error = harness
            .adapter
            .plan_uninstall(&manager, &receipt)
            .expect_err("missing target rejected before planning");

        assert!(matches!(
            error,
            IntegrationError::UninstallTargetMissing { .. }
        ));
        assert!(!harness.user_config().exists());
        assert!(integration_sidecars(&harness.home).is_empty());
        assert_eq!(
            receipts
                .active_for(CLIENT_ID, &harness.user_config())
                .expect("active receipt query")
                .as_ref()
                .map(|active| active.plan_hash.as_str()),
            Some(receipt.plan_hash.as_str())
        );
    }

    #[test]
    fn test_uninstall_config_deleted_after_plan_is_drift() {
        let harness = Harness::new();
        let (manager, receipts, receipt) = harness.install(Scope::User);
        let plan = harness
            .adapter
            .plan_uninstall(&manager, &receipt)
            .expect("uninstall plan");
        fs::remove_file(harness.user_config()).expect("target deleted after plan");

        let error = harness
            .adapter
            .uninstall(&manager, &PlanApproval::from(&plan), &receipts)
            .expect_err("post-plan deletion is config drift");

        assert!(matches!(error, IntegrationError::ConfigDrift { .. }));
        assert!(!harness.user_config().exists());
        assert!(integration_sidecars(&harness.home).is_empty());
        assert!(
            receipts
                .active_for(CLIENT_ID, &harness.user_config())
                .expect("active receipt query")
                .is_some()
        );
    }

    #[test]
    fn test_repeated_install_and_uninstall_are_idempotent() {
        let harness = Harness::new();
        let (manager, receipts, first_receipt) = harness.install(Scope::User);
        let install_plan = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::User,
                },
            )
            .expect("repeat install plan");
        let repeat_receipt = harness
            .adapter
            .install(&manager, &PlanApproval::from(&install_plan), &receipts)
            .expect("repeat install applies idempotently");
        assert_eq!(repeat_receipt, first_receipt);

        let uninstall_plan = harness
            .adapter
            .plan_uninstall(&manager, &first_receipt)
            .expect("uninstall plan");
        let approval = PlanApproval::from(&uninstall_plan);
        let first = harness
            .adapter
            .uninstall(&manager, &approval, &receipts)
            .expect("first uninstall");
        let second = harness
            .adapter
            .uninstall(&manager, &approval, &receipts)
            .expect("repeat uninstall");
        assert!(!first.already_applied);
        assert!(second.already_applied);
    }

    fn write_executable(path: &Path) {
        fs::write(path, b"test executable").expect("test executable written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .expect("test executable mode");
        }
    }

    fn write_json(path: &Path, value: &Value) {
        fs::write(
            path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(value).expect("JSON rendered")
            ),
        )
        .expect("JSON written");
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).expect("JSON read")).expect("JSON parsed")
    }

    fn object_keys(value: &Value) -> Vec<&str> {
        value
            .as_object()
            .expect("JSON object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn check(report: &VerificationReport, name: &str) -> Option<bool> {
        report
            .checks
            .iter()
            .find(|check| check.name == name)
            .map(|check| check.passed)
    }

    fn integration_sidecars(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .expect("directory readable")
            .map(|entry| entry.expect("directory entry").path())
            .filter(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                name.contains(".compresso-backup-") || name.ends_with(".compresso-journal")
            })
            .collect()
    }
}
