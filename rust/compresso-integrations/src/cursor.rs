//! Cursor MCP configuration adapter.
//!
//! Cursor user and project configuration use the same JSON shape. This
//! adapter owns only `mcpServers.compresso`; discovery is bounded and consumes
//! caller-supplied version evidence so the integrations crate never spawns a
//! client process.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::plan::MutationRequest;
use crate::text_fidelity::strip_bom;
use crate::{
    ConfigFormat, Detection, InstallCtx, InstallReceipt, Integration, IntegrationError,
    MutationKind, MutationPlan, PlanManager, Scope, VerificationCheck, VerificationReport,
};

const CLIENT_ID: &str = "cursor";
const OWNED_PATH: &str = "mcpServers.compresso";
const USER_CONFIG_RELATIVE_PATH: &str = ".cursor/mcp.json";
const PROJECT_CONFIG_RELATIVE_PATH: &str = ".cursor/mcp.json";
const MAX_CLI_CANDIDATES: usize = 64;
const MAX_APPLICATIONS: usize = 16;
const MAX_VERSION_METADATA_BYTES: u64 = 256 * 1024;
const EXPECTED_MCP_TOOLS: [&str; 10] = [
    "compresso_repo_map",
    "compresso_search",
    "compresso_read",
    "compresso_outline",
    "compresso_unfold",
    "compresso_context",
    "compresso_impact",
    "compresso_compress",
    "compresso_stats",
    "compresso_explain",
];

/// Platform family used for bounded Cursor discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CursorPlatform {
    /// Cursor.app or the `cursor` launcher on macOS.
    MacOs,
    /// A native package, AppImage, or the `cursor` launcher on Linux.
    Linux,
    /// A native installation or Cursor launcher on Windows.
    Windows,
    /// A platform with no documented first-slice integration contract.
    Unsupported(String),
}

impl CursorPlatform {
    /// Returns the platform family selected at compile time.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Unsupported(std::env::consts::OS.to_owned())
        }
    }

    fn supported(&self) -> bool {
        matches!(self, Self::MacOs | Self::Linux | Self::Windows)
    }

    fn command_names(&self) -> &'static [&'static str] {
        match self {
            Self::Windows => &["cursor.exe", "cursor.cmd", "cursor.bat", "cursor"],
            Self::MacOs | Self::Linux | Self::Unsupported(_) => &["cursor"],
        }
    }
}

/// Version evidence obtained by the host from one exact Cursor launcher.
///
/// The integrations crate does not execute child processes. A process-capable
/// host may run the fixed `<path> --version` command, then bind its content-free
/// output to that same path through this type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorCliEvidence {
    path: PathBuf,
    version_evidence: String,
}

impl CursorCliEvidence {
    /// Binds version output to the launcher that produced it.
    pub fn new(path: impl Into<PathBuf>, version_evidence: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            version_evidence: version_evidence.into(),
        }
    }
}

/// Evidence for one platform-native Cursor application installation.
///
/// The host boundary may obtain `version_evidence` from an app bundle or
/// package manifest. The adapter only validates the installation path and
/// parses the supplied evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorApplicationEvidence {
    path: PathBuf,
    version_evidence: Option<String>,
}

impl CursorApplicationEvidence {
    /// Creates application evidence for a known installation path.
    pub fn new(path: impl Into<PathBuf>, version_evidence: Option<String>) -> Self {
        Self {
            path: path.into(),
            version_evidence,
        }
    }
}

/// Result of a trusted host's live MCP initialize and tool-list probe.
///
/// Evidence is bound to the exact configured command. The adapter validates
/// the complete first-slice tool set without spawning the command itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorLiveProbeEvidence {
    command: PathBuf,
    initialized: bool,
    tool_names: Vec<String>,
}

impl CursorLiveProbeEvidence {
    /// Records content-free evidence from a live MCP probe performed by the
    /// process-capable host boundary.
    pub fn new(
        command: impl Into<PathBuf>,
        initialized: bool,
        tool_names: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            command: command.into(),
            initialized,
            tool_names: tool_names.into_iter().collect(),
        }
    }

    fn proves_expected_tools_for(&self, command: &Path) -> bool {
        if !self.initialized || !paths_resolve_to_same_file(&self.command, command) {
            return false;
        }
        let actual = self
            .tool_names
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = EXPECTED_MCP_TOOLS.into_iter().collect::<BTreeSet<_>>();
        self.tool_names.len() == EXPECTED_MCP_TOOLS.len() && actual == expected
    }
}

/// Bounded, filesystem-only Cursor discovery inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorDiscovery {
    platform: CursorPlatform,
    cli_candidates: Vec<CursorCliEvidence>,
    applications: Vec<CursorApplicationEvidence>,
}

impl CursorDiscovery {
    /// Creates explicit discovery inputs for deterministic platform handling.
    pub fn new(
        platform: CursorPlatform,
        cli_candidates: impl IntoIterator<Item = CursorCliEvidence>,
        applications: impl IntoIterator<Item = CursorApplicationEvidence>,
    ) -> Self {
        Self {
            platform,
            cli_candidates: cli_candidates
                .into_iter()
                .take(MAX_CLI_CANDIDATES)
                .collect(),
            applications: applications.into_iter().take(MAX_APPLICATIONS).collect(),
        }
    }

    /// Creates discovery inputs for the compile-time platform.
    pub fn current(
        cli_candidates: impl IntoIterator<Item = CursorCliEvidence>,
        applications: impl IntoIterator<Item = CursorApplicationEvidence>,
    ) -> Self {
        Self::new(CursorPlatform::current(), cli_candidates, applications)
    }

    fn installed_cli(&self) -> Option<&CursorCliEvidence> {
        self.cli_candidates.iter().find(|candidate| {
            command_name_matches(&self.platform, &candidate.path)
                && is_executable_file(&self.platform, &candidate.path)
        })
    }

    fn installed_application(&self) -> Option<&CursorApplicationEvidence> {
        self.applications
            .iter()
            .find(|application| application_path_exists(&self.platform, &application.path))
    }

    fn detection_version(&self) -> Result<Option<String>, String> {
        if !self.platform.supported() {
            let name = match &self.platform {
                CursorPlatform::Unsupported(name) => name,
                _ => unreachable!("supported platform handled above"),
            };
            return Err(format!("Cursor integration is unsupported on {name}"));
        }
        if let Some(cli) = self.installed_cli() {
            return parse_version_evidence(&cli.version_evidence)
                .map(Some)
                .ok_or_else(|| {
                    "Cursor CLI version evidence is unavailable or malformed".to_owned()
                });
        }
        let Some(application) = self.installed_application() else {
            return Ok(None);
        };
        application_version(&self.platform, application)
            .map(Some)
            .ok_or_else(|| {
                "Cursor application version evidence is unavailable or malformed".to_owned()
            })
    }
}

/// Reversible Cursor adapter for user and repository-local MCP configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorIntegration {
    user_home: PathBuf,
    discovery: CursorDiscovery,
    compresso_version: String,
    live_probe: Option<CursorLiveProbeEvidence>,
}

impl CursorIntegration {
    /// Creates an adapter without consulting process-global HOME or PATH.
    pub fn new(user_home: impl Into<PathBuf>, discovery: CursorDiscovery) -> Self {
        Self {
            user_home: user_home.into(),
            discovery,
            compresso_version: env!("CARGO_PKG_VERSION").to_owned(),
            live_probe: None,
        }
    }

    /// Overrides the version recorded in installation receipts.
    pub fn with_compresso_version(mut self, version: impl Into<String>) -> Self {
        self.compresso_version = version.into();
        self
    }

    /// Adds content-free evidence from the host's live MCP probe.
    pub fn with_live_probe_evidence(mut self, evidence: CursorLiveProbeEvidence) -> Self {
        self.live_probe = Some(evidence);
        self
    }

    /// Resolves the selected Cursor MCP configuration path.
    pub fn config_path(&self, scope: &Scope) -> Result<PathBuf, IntegrationError> {
        match scope {
            Scope::User => self.safe_user_config_path(),
            Scope::Project(root) => self.safe_project_config_path(root),
        }
    }

    fn safe_user_config_path(&self) -> Result<PathBuf, IntegrationError> {
        if !self.user_home.is_absolute()
            || self
                .user_home
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(invalid_request(
                "Cursor user home must be an absolute, normalized path",
            ));
        }
        let home_metadata =
            fs::symlink_metadata(&self.user_home).map_err(|source| IntegrationError::Io {
                path: self.user_home.clone(),
                source,
            })?;
        if home_metadata.file_type().is_symlink() || !home_metadata.is_dir() {
            return Err(invalid_request(
                "Cursor user home must be a real directory, not a symbolic link",
            ));
        }
        let canonical_home =
            fs::canonicalize(&self.user_home).map_err(|source| IntegrationError::Io {
                path: self.user_home.clone(),
                source,
            })?;
        checked_cursor_config_path(&canonical_home, "user")
    }

    fn safe_project_config_path(&self, root: &Path) -> Result<PathBuf, IntegrationError> {
        let canonical_root = fs::canonicalize(root).map_err(|source| IntegrationError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        if !canonical_root.is_dir() {
            return Err(invalid_request("Cursor project scope must be a directory"));
        }

        checked_cursor_config_path(&canonical_root, "project")
    }

    fn validate_receipt_target(&self, path: &Path) -> Result<(), IntegrationError> {
        match fs::symlink_metadata(path) {
            Ok(_) => {}
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
        }
        let user_candidate = self.user_home.join(USER_CONFIG_RELATIVE_PATH);
        if let Ok(resolved_user_candidate) = resolve_path_identity(&user_candidate)
            && resolved_user_candidate == path
        {
            let user_path = self.safe_user_config_path()?;
            if path == user_path {
                return Ok(());
            }
            return Err(invalid_request(
                "Cursor receipt user target is not canonical within the home root",
            ));
        }
        let project_root = path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid_request("Cursor receipt path has no project root"))?;
        let safe_path = self.safe_project_config_path(project_root)?;
        if safe_path != path {
            return Err(invalid_request(
                "Cursor receipt target is outside its project scope",
            ));
        }
        Ok(())
    }

    fn ensure_supported(&self) -> Result<(), IntegrationError> {
        match &self.discovery.platform {
            CursorPlatform::Unsupported(name) => Err(IntegrationError::Unsupported {
                reason: format!("Cursor integration is unsupported on {name}"),
            }),
            CursorPlatform::MacOs | CursorPlatform::Linux | CursorPlatform::Windows => Ok(()),
        }
    }
}

impl Integration for CursorIntegration {
    fn client_id(&self) -> &'static str {
        CLIENT_ID
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let config_path = match self.config_path(scope) {
            Ok(path) => path,
            Err(error) => {
                return Detection::Unsupported {
                    reason: error.to_string(),
                };
            }
        };
        match self.discovery.detection_version() {
            Ok(Some(version)) if config_path.is_file() => Detection::Installed {
                version,
                config_path,
            },
            Ok(Some(_)) => Detection::NotFound,
            Ok(None) => Detection::NotFound,
            Err(reason) => Detection::Unsupported { reason },
        }
    }

    fn plan_install(
        &self,
        manager: &PlanManager,
        context: &InstallCtx,
    ) -> Result<MutationPlan, IntegrationError> {
        self.ensure_supported()?;
        let config_path = self.config_path(&context.scope)?;
        let command = resolved_compresso_command(&self.discovery.platform, &context.compresso_bin)?;
        let args = expected_server_args(&context.scope)?;
        let mut document = read_json_object(&config_path)?;
        let servers = object_at_or_insert(&mut document, "mcpServers")?;
        servers.insert(
            "compresso".to_owned(),
            json!({
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
        if let CursorPlatform::Unsupported(name) = &self.discovery.platform {
            return VerificationReport {
                client: CLIENT_ID.to_owned(),
                verified: false,
                checks: vec![VerificationCheck {
                    name: "platform_supported".to_owned(),
                    passed: false,
                    detail: format!("Cursor integration is unsupported on {name}"),
                }],
            };
        }

        let path = match self.config_path(scope) {
            Ok(path) => path,
            Err(error) => {
                return VerificationReport {
                    client: CLIENT_ID.to_owned(),
                    verified: false,
                    checks: vec![
                        VerificationCheck {
                            name: "scope_isolated".to_owned(),
                            passed: false,
                            detail: error.to_string(),
                        },
                        cursor_restart_instruction(),
                    ],
                };
            }
        };
        let parsed = read_existing_json_object_for_verify(&path);
        let config_parses = parsed.is_ok();
        let entry = parsed
            .as_ref()
            .ok()
            .and_then(|document| document.get("mcpServers"))
            .and_then(Value::as_object)
            .and_then(|servers| servers.get("compresso"));
        let server_entry_valid = expected_server_args(scope)
            .ok()
            .is_some_and(|args| entry.is_some_and(|entry| is_cursor_server_entry(entry, &args)));
        let binary_resolves = entry
            .and_then(|entry| entry.get("command"))
            .and_then(Value::as_str)
            .map(Path::new)
            .is_some_and(|command| {
                command.is_absolute() && is_executable_file(&self.discovery.platform, command)
            });
        let configured_command = entry
            .and_then(|entry| entry.get("command"))
            .and_then(Value::as_str)
            .map(Path::new);
        let live_probe_passed = configured_command.is_some_and(|command| {
            self.live_probe
                .as_ref()
                .is_some_and(|probe| probe.proves_expected_tools_for(command))
        });
        let checks = vec![
            VerificationCheck {
                name: "config_parses".to_owned(),
                passed: config_parses,
                detail: if config_parses {
                    "Cursor configuration is valid JSON".to_owned()
                } else {
                    "Cursor configuration is missing, unreadable, malformed, or not an object"
                        .to_owned()
                },
            },
            VerificationCheck {
                name: "server_entry".to_owned(),
                passed: server_entry_valid,
                detail: if server_entry_valid {
                    "Compresso stdio entry has the expected Cursor shape".to_owned()
                } else {
                    "Compresso stdio entry is missing or has an unexpected Cursor shape".to_owned()
                },
            },
            VerificationCheck {
                name: "binary_resolves".to_owned(),
                passed: binary_resolves,
                detail: if binary_resolves {
                    "configured command is an absolute executable file".to_owned()
                } else {
                    "configured command is missing, relative, or not executable".to_owned()
                },
            },
            VerificationCheck {
                name: "live_mcp_probe".to_owned(),
                passed: live_probe_passed,
                detail: if live_probe_passed {
                    "live initialize probe returned the complete Compresso tool set".to_owned()
                } else {
                    "live initialize evidence is missing, stale, or has an incomplete tool set"
                        .to_owned()
                },
            },
            cursor_restart_instruction(),
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
        self.ensure_supported()?;
        validate_cursor_receipt(receipt)?;
        self.validate_receipt_target(&receipt.config_path)?;
        let mut document = read_existing_json_object(&receipt.config_path)?;
        let installed_hash = receipt
            .owned_value_hashes
            .get(OWNED_PATH)
            .ok_or_else(|| invalid_request("receipt is missing the Cursor owned value hash"))?;
        let current_hash = document
            .get("mcpServers")
            .and_then(Value::as_object)
            .and_then(|servers| servers.get("compresso"))
            .map(canonical_json_hash)
            .transpose()?;
        let mut warnings = Vec::new();
        if current_hash.as_ref() == Some(installed_hash) {
            document
                .get_mut("mcpServers")
                .and_then(Value::as_object_mut)
                .expect("matching owned hash requires an object parent")
                .remove("compresso");
        } else if !(receipt.uninstalled_at.is_some() && current_hash.is_none()) {
            warnings.push(format!("key_modified:{OWNED_PATH}"));
        }

        manager.issue(MutationRequest {
            operation: MutationKind::Uninstall,
            client: CLIENT_ID.to_owned(),
            config_path: receipt.config_path.clone(),
            config_format: ConfigFormat::Json,
            target_contents: render_json(&document)?,
            owned_paths: vec![OWNED_PATH.to_owned()],
            compresso_version: receipt.compresso_version.clone(),
            prior_receipt: Some(receipt.clone()),
            warnings,
        })
    }
}

fn validate_cursor_receipt(receipt: &InstallReceipt) -> Result<(), IntegrationError> {
    if receipt.client != CLIENT_ID
        || receipt.keys_owned != [OWNED_PATH]
        || !receipt.config_path.is_absolute()
        || !receipt
            .config_path
            .ends_with(Path::new(PROJECT_CONFIG_RELATIVE_PATH))
    {
        return Err(invalid_request(
            "receipt does not belong to a Cursor MCP configuration",
        ));
    }
    Ok(())
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>, IntegrationError> {
    match fs::read(path) {
        Ok(bytes) => parse_json_object(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_existing_json_object(path: &Path) -> Result<Map<String, Value>, IntegrationError> {
    match fs::read(path) {
        Ok(bytes) => parse_json_object(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(IntegrationError::UninstallTargetMissing {
                path: path.to_path_buf(),
            })
        }
        Err(source) => Err(IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_existing_json_object_for_verify(path: &Path) -> Result<Map<String, Value>, ()> {
    fs::read(path)
        .map_err(|_| ())
        .and_then(|bytes| parse_json_object(&bytes).map_err(|_| ()))
}

fn parse_json_object(bytes: &[u8]) -> Result<Map<String, Value>, IntegrationError> {
    serde_json::from_slice::<Value>(strip_bom(bytes))
        .map_err(|error| {
            invalid_request(format!("Cursor configuration is malformed JSON: {error}"))
        })?
        .as_object()
        .cloned()
        .ok_or_else(|| invalid_request("Cursor configuration JSON must be an object"))
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

fn render_json(document: &Map<String, Value>) -> Result<String, IntegrationError> {
    let mut rendered =
        serde_json::to_string_pretty(&Value::Object(document.clone())).map_err(|error| {
            invalid_request(format!("Cursor configuration cannot be rendered: {error}"))
        })?;
    rendered.push('\n');
    Ok(rendered)
}

fn canonical_json_hash(value: &Value) -> Result<String, IntegrationError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| invalid_request(format!("owned JSON value cannot be hashed: {error}")))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn resolved_compresso_command(
    platform: &CursorPlatform,
    path: &Path,
) -> Result<String, IntegrationError> {
    if !is_normalized_absolute_path(path) {
        return Err(invalid_request(
            "Compresso command path must be normalized and absolute",
        ));
    }
    let resolved = fs::canonicalize(path).map_err(|source| IntegrationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !is_executable_file(platform, &resolved) {
        return Err(invalid_request(
            "Compresso command path must resolve to an executable file",
        ));
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid_request("Compresso command path must be valid UTF-8"))
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
                "Cursor project scope root must resolve to a directory",
            ));
        }
        let root = canonical_root
            .to_str()
            .ok_or_else(|| invalid_request("Cursor project root must be valid UTF-8"))?;
        args.extend(["--root".to_owned(), root.to_owned()]);
    }
    Ok(args)
}

fn is_cursor_server_entry(value: &Value, expected_args: &[String]) -> bool {
    let Some(entry) = value.as_object() else {
        return false;
    };
    entry.len() == 2
        && entry
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| Path::new(command).is_absolute())
        && entry.get("args") == Some(&json!(expected_args))
}

fn is_executable_file(platform: &CursorPlatform, path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    if matches!(platform, CursorPlatform::Windows) {
        return windows_executable_extension(path);
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

fn windows_executable_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "exe" | "com" | "cmd" | "bat"
            )
        })
}

fn command_name_matches(platform: &CursorPlatform, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    platform.command_names().iter().any(|expected| {
        if matches!(platform, CursorPlatform::Windows) {
            name.eq_ignore_ascii_case(expected)
        } else {
            name == *expected
        }
    })
}

fn application_path_exists(platform: &CursorPlatform, path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    match platform {
        CursorPlatform::MacOs => {
            metadata.is_dir()
                && path.file_name().is_some_and(|name| name == "Cursor.app")
                && is_executable_file(
                    platform,
                    &path.join("Contents").join("MacOS").join("Cursor"),
                )
        }
        CursorPlatform::Linux => {
            if metadata.is_file() {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.to_ascii_lowercase().contains("cursor"))
                    && is_executable_file(platform, path)
            } else {
                metadata.is_dir()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.to_ascii_lowercase().contains("cursor"))
                    && [path.join("cursor"), path.join("Cursor")]
                        .iter()
                        .any(|candidate| is_executable_file(platform, candidate))
            }
        }
        CursorPlatform::Windows => {
            if metadata.is_file() {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("Cursor.exe"))
                    && is_executable_file(platform, path)
            } else {
                metadata.is_dir()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.to_ascii_lowercase().contains("cursor"))
                    && is_executable_file(platform, &path.join("Cursor.exe"))
            }
        }
        CursorPlatform::Unsupported(_) => false,
    }
}

fn application_version(
    platform: &CursorPlatform,
    application: &CursorApplicationEvidence,
) -> Option<String> {
    platform_native_application_version(platform, &application.path).or_else(|| {
        application
            .version_evidence
            .as_deref()
            .and_then(parse_version_evidence)
    })
}

fn platform_native_application_version(platform: &CursorPlatform, path: &Path) -> Option<String> {
    match platform {
        CursorPlatform::MacOs => {
            read_macos_bundle_version(&path.join("Contents").join("Info.plist"))
        }
        CursorPlatform::Linux | CursorPlatform::Windows => {
            let root = if path.is_dir() { path } else { path.parent()? };
            let metadata_paths = [
                root.join("resources").join("app").join("package.json"),
                root.join("package.json"),
            ];
            metadata_paths
                .iter()
                .find_map(|metadata| read_package_version(metadata))
                .or_else(|| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .and_then(parse_version_evidence)
                })
        }
        CursorPlatform::Unsupported(_) => None,
    }
}

fn read_macos_bundle_version(path: &Path) -> Option<String> {
    let text = read_bounded_utf8(path)?;
    let key_position = text.find("<key>CFBundleShortVersionString</key>")?;
    let following = &text[key_position..];
    let value_start = following.find("<string>")? + "<string>".len();
    let value_end = following[value_start..].find("</string>")? + value_start;
    let candidate = following[value_start..value_end].trim();
    is_semver(candidate).then(|| candidate.to_owned())
}

fn read_package_version(path: &Path) -> Option<String> {
    let text = read_bounded_utf8(path)?;
    let document: Value = serde_json::from_str(&text).ok()?;
    let version = document.get("version")?.as_str()?;
    is_semver(version).then(|| version.to_owned())
}

fn read_bounded_utf8(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_VERSION_METADATA_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)
        .ok()?
        .take(MAX_VERSION_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_VERSION_METADATA_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn parse_version_evidence(evidence: &str) -> Option<String> {
    evidence
        .split_whitespace()
        .map(|part| {
            let trimmed = part.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && !matches!(character, '.' | '-' | '+')
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

fn cursor_restart_instruction() -> VerificationCheck {
    VerificationCheck {
        name: "cursor_restart_instruction".to_owned(),
        passed: true,
        detail: "Restart Cursor, then confirm compresso appears under Cursor Settings -> MCP"
            .to_owned(),
    }
}

fn reject_symlink_if_present(path: &Path, label: &str) -> Result<(), IntegrationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(invalid_request(format!(
            "{label} must not be a symbolic link"
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn checked_cursor_config_path(
    canonical_root: &Path,
    scope_name: &str,
) -> Result<PathBuf, IntegrationError> {
    if !canonical_root.is_absolute() || !canonical_root.is_dir() {
        return Err(invalid_request(format!(
            "Cursor {scope_name} root must be an absolute directory"
        )));
    }
    let cursor_directory = canonical_root.join(".cursor");
    reject_symlink_if_present(
        &cursor_directory,
        &format!("Cursor {scope_name} .cursor directory"),
    )?;
    if cursor_directory.exists() {
        let canonical_cursor =
            fs::canonicalize(&cursor_directory).map_err(|source| IntegrationError::Io {
                path: cursor_directory.clone(),
                source,
            })?;
        if !canonical_cursor.is_dir()
            || canonical_cursor != cursor_directory
            || !canonical_cursor.starts_with(canonical_root)
        {
            return Err(invalid_request(format!(
                "Cursor {scope_name} configuration directory escapes its root"
            )));
        }
    }

    let config_path = cursor_directory.join("mcp.json");
    reject_symlink_if_present(&config_path, &format!("Cursor {scope_name} configuration"))?;
    if config_path.exists() {
        let canonical_config =
            fs::canonicalize(&config_path).map_err(|source| IntegrationError::Io {
                path: config_path.clone(),
                source,
            })?;
        if !canonical_config.is_file()
            || canonical_config != config_path
            || !canonical_config.starts_with(canonical_root)
        {
            return Err(invalid_request(format!(
                "Cursor {scope_name} configuration escapes its root"
            )));
        }
    }
    Ok(config_path)
}

fn resolve_path_identity(path: &Path) -> Result<PathBuf, IntegrationError> {
    if path.exists() {
        return fs::canonicalize(path).map_err(|source| IntegrationError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid_request("Cursor configuration path has no parent"))?;
    if parent.exists() {
        return fs::canonicalize(parent)
            .map(|parent| parent.join(path.file_name().expect("path with parent has a name")))
            .map_err(|source| IntegrationError::Io {
                path: parent.to_path_buf(),
                source,
            });
    }
    let grandparent = parent
        .parent()
        .ok_or_else(|| invalid_request("Cursor configuration parent has no root"))?;
    let canonical_grandparent =
        fs::canonicalize(grandparent).map_err(|source| IntegrationError::Io {
            path: grandparent.to_path_buf(),
            source,
        })?;
    Ok(canonical_grandparent
        .join(
            parent
                .file_name()
                .expect("configuration parent beneath a root has a name"),
        )
        .join(path.file_name().expect("path with parent has a name")))
}

fn paths_resolve_to_same_file(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
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
    use crate::{MemoryReceiptStore, PlanApproval};

    struct Harness {
        _directory: TempDir,
        home: PathBuf,
        project: PathBuf,
        bin_directory: PathBuf,
        compresso: PathBuf,
        adapter: CursorIntegration,
    }

    impl Harness {
        fn new(platform: CursorPlatform) -> Self {
            let directory = tempdir().expect("temporary directory");
            let home = directory.path().join("home");
            let project = directory.path().join("project");
            let bin_directory = directory.path().join("bin");
            fs::create_dir_all(&home).expect("home created");
            fs::create_dir_all(&project).expect("project created");
            fs::create_dir_all(&bin_directory).expect("bin created");
            let cursor_name = if platform == CursorPlatform::Windows {
                "cursor.exe"
            } else {
                "cursor"
            };
            let cursor = bin_directory.join(cursor_name);
            write_executable(&cursor);
            let compresso = bin_directory.join(if platform == CursorPlatform::Windows {
                "compresso.exe"
            } else {
                "compresso"
            });
            write_executable(&compresso);
            let discovery = CursorDiscovery::new(
                platform,
                [CursorCliEvidence::new(cursor, "Cursor 1.4.2")],
                [],
            );
            let adapter = CursorIntegration::new(home.clone(), discovery)
                .with_compresso_version("0.1.0")
                .with_live_probe_evidence(expected_probe(&compresso));
            Self {
                _directory: directory,
                home,
                project,
                bin_directory,
                compresso,
                adapter,
            }
        }

        fn user_config(&self) -> PathBuf {
            self.home.join(USER_CONFIG_RELATIVE_PATH)
        }

        fn project_config(&self) -> PathBuf {
            self.project.join(PROJECT_CONFIG_RELATIVE_PATH)
        }

        fn install(
            &self,
            scope: Scope,
            manager: &PlanManager,
            receipts: &MemoryReceiptStore,
        ) -> InstallReceipt {
            let plan = self
                .adapter
                .plan_install(
                    manager,
                    &InstallCtx {
                        compresso_bin: self.compresso.clone(),
                        scope,
                        session_recall: false,
                    },
                )
                .expect("install plan");
            self.adapter
                .install(manager, &PlanApproval::from(&plan), receipts)
                .expect("install applies")
        }
    }

    #[test]
    fn test_cursor_install_adds_entry() {
        let harness = Harness::new(CursorPlatform::Linux);
        let original = json!({
            "theme": {"name": "night", "unknown": [1, 2, 3]},
            "mcpServers": {
                "other": {"command": "/opt/other", "args": ["--safe"]}
            }
        });
        write_json(&harness.user_config(), &original);
        let manager = PlanManager::new("cursor-install").expect("manager");
        let receipts = MemoryReceiptStore::default();

        let receipt = harness.install(Scope::User, &manager, &receipts);
        let installed = read_json(&harness.user_config());

        assert_eq!(installed["theme"], original["theme"]);
        assert_eq!(
            installed["mcpServers"]["other"],
            original["mcpServers"]["other"]
        );
        assert_eq!(
            installed["mcpServers"]["compresso"],
            json!({
                "command": harness.compresso.to_str().expect("UTF-8 command"),
                "args": ["mcp", "serve"]
            })
        );
        assert_eq!(receipt.keys_owned, [OWNED_PATH]);
        assert!(receipt.backup_path.contains(".compresso-backup-"));
        assert!(harness.adapter.verify(&Scope::User).verified);
    }

    #[cfg(unix)]
    #[test]
    fn test_cursor_rejects_non_normalized_absolute_launcher_paths() {
        let harness = Harness::new(CursorPlatform::Linux);
        let parent = harness.compresso.parent().expect("bin directory");
        let parent_text = parent.to_str().expect("UTF-8 bin directory");
        let nested = parent.join("nested");
        fs::create_dir(&nested).expect("nested directory created");
        let cases = [
            PathBuf::from(format!("{parent_text}/./compresso")),
            PathBuf::from(format!("{parent_text}//compresso")),
            nested.join("../compresso"),
        ];
        let manager = PlanManager::new("cursor-non-normalized-launcher").expect("manager");

        for path in cases {
            let error = harness
                .adapter
                .plan_install(
                    &manager,
                    &InstallCtx {
                        compresso_bin: path,
                        scope: Scope::User,
                        session_recall: false,
                    },
                )
                .expect_err("non-normalized launcher path rejected");
            assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        }
        assert!(!harness.user_config().exists());
    }

    #[test]
    fn test_cursor_install_and_uninstall_preserve_unknown_json_key_order() {
        let harness = Harness::new(CursorPlatform::Linux);
        let original = concat!(
            "{\n",
            "  \"zeta\": {\"second\": 2, \"first\": 1},\n",
            "  \"mcpServers\": {\"other\": {\"z\": true, \"a\": false}},\n",
            "  \"alpha\": [3, 2, 1]\n",
            "}\n"
        );
        write_bytes(&harness.user_config(), original.as_bytes());
        let install_manager = PlanManager::new("cursor-order-install").expect("manager");
        let receipts = MemoryReceiptStore::default();

        let receipt = harness.install(Scope::User, &install_manager, &receipts);
        let installed = fs::read_to_string(harness.user_config()).expect("installed JSON");
        assert_in_order(&installed, &["\"zeta\"", "\"mcpServers\"", "\"alpha\""]);
        assert_in_order(&installed, &["\"second\"", "\"first\""]);
        assert_in_order(&installed, &["\"z\"", "\"a\""]);

        let uninstall_manager = PlanManager::new("cursor-order-uninstall").expect("manager");
        let plan = harness
            .adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect("uninstall plan");
        harness
            .adapter
            .uninstall(&uninstall_manager, &PlanApproval::from(&plan), &receipts)
            .expect("uninstall applies");
        let restored = fs::read_to_string(harness.user_config()).expect("restored JSON");
        assert_in_order(&restored, &["\"zeta\"", "\"mcpServers\"", "\"alpha\""]);
        assert_in_order(&restored, &["\"second\"", "\"first\""]);
        assert_in_order(&restored, &["\"z\"", "\"a\""]);
    }

    #[test]
    fn test_cursor_uninstall_restores() {
        let harness = Harness::new(CursorPlatform::MacOs);
        let original = json!({
            "unknown": {"keep": true},
            "mcpServers": {"other": {"url": "http://127.0.0.1:7000"}}
        });
        write_json(&harness.user_config(), &original);
        let install_manager = PlanManager::new("cursor-uninstall-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = harness.install(Scope::User, &install_manager, &receipts);
        let uninstall_manager = PlanManager::new("cursor-uninstall").expect("manager");

        let plan = harness
            .adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect("uninstall plan");
        let report = harness
            .adapter
            .uninstall(&uninstall_manager, &PlanApproval::from(&plan), &receipts)
            .expect("uninstall applies");

        assert_eq!(read_json(&harness.user_config()), original);
        assert_eq!(report.removed_keys, [OWNED_PATH]);
        assert!(report.retained_keys.is_empty());
    }

    #[test]
    fn test_cursor_project_scope_pins_and_verifies_exact_canonical_root() {
        let harness = Harness::new(CursorPlatform::Linux);
        let user_original = b"{\"userOnly\":true}\n";
        write_bytes(&harness.user_config(), user_original);
        let manager = PlanManager::new("cursor-project").expect("manager");
        let receipts = MemoryReceiptStore::default();

        let receipt = harness.install(Scope::Project(harness.project.clone()), &manager, &receipts);

        assert_eq!(
            receipt.config_path,
            harness
                .adapter
                .config_path(&Scope::Project(harness.project.clone()))
                .expect("safe project config")
        );
        assert_eq!(
            fs::read(harness.user_config()).expect("user config read"),
            user_original
        );
        let installed = read_json(&harness.project_config());
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
        write_json(&harness.project_config(), &changed);
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

    #[test]
    fn test_fresh_install_leaves_empty_parent_on_uninstall() {
        let harness = Harness::new(CursorPlatform::Linux);
        let install_manager = PlanManager::new("cursor-fresh-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = harness.install(Scope::User, &install_manager, &receipts);
        assert_eq!(receipt.backup_path, crate::CREATED_FILE_BACKUP_SENTINEL);
        let uninstall_manager = PlanManager::new("cursor-fresh-uninstall").expect("manager");
        let plan = harness
            .adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect("uninstall plan");
        harness
            .adapter
            .uninstall(&uninstall_manager, &PlanApproval::from(&plan), &receipts)
            .expect("uninstall applies");

        assert_eq!(read_json(&harness.user_config()), json!({"mcpServers": {}}));
    }

    #[test]
    fn test_modified_owned_key_is_retained_with_warning() {
        let harness = Harness::new(CursorPlatform::Linux);
        let install_manager = PlanManager::new("cursor-modified-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = harness.install(Scope::User, &install_manager, &receipts);
        let mut modified = read_json(&harness.user_config());
        modified["mcpServers"]["compresso"]["args"] = json!(["mcp", "serve", "--user-change"]);
        write_json(&harness.user_config(), &modified);
        let uninstall_manager = PlanManager::new("cursor-modified-uninstall").expect("manager");

        let plan = harness
            .adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect("uninstall plan");
        assert_eq!(plan.warnings, [format!("key_modified:{OWNED_PATH}")]);
        let report = harness
            .adapter
            .uninstall(&uninstall_manager, &PlanApproval::from(&plan), &receipts)
            .expect("safe uninstall");

        assert_eq!(read_json(&harness.user_config()), modified);
        assert_eq!(report.retained_keys, [OWNED_PATH]);
        assert!(report.removed_keys.is_empty());
    }

    #[test]
    fn test_malformed_config_is_never_rewritten() {
        let harness = Harness::new(CursorPlatform::Linux);
        let malformed = b"{ malformed Cursor JSON";
        write_bytes(&harness.user_config(), malformed);
        let manager = PlanManager::new("cursor-malformed").expect("manager");

        let error = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("malformed config rejected");

        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        assert_eq!(
            fs::read(harness.user_config()).expect("malformed config read"),
            malformed
        );
        assert!(!harness.adapter.verify(&Scope::User).verified);
    }

    #[test]
    fn test_verify_rejects_missing_binary() {
        let harness = Harness::new(CursorPlatform::Linux);
        write_json(
            &harness.user_config(),
            &json!({
                "mcpServers": {
                    "compresso": {
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
    fn test_repeat_install_and_uninstall_are_idempotent() {
        let harness = Harness::new(CursorPlatform::Linux);
        let manager = PlanManager::new("cursor-repeat").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let first_receipt = harness.install(Scope::User, &manager, &receipts);

        let repeat_plan = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect("repeat install plan");
        let repeat_receipt = harness
            .adapter
            .install(&manager, &PlanApproval::from(&repeat_plan), &receipts)
            .expect("repeat install");
        assert_eq!(repeat_receipt, first_receipt);

        let uninstall_manager = PlanManager::new("cursor-repeat-uninstall").expect("manager");
        let uninstall_plan = harness
            .adapter
            .plan_uninstall(&uninstall_manager, &first_receipt)
            .expect("uninstall plan");
        let approval = PlanApproval::from(&uninstall_plan);
        let first = harness
            .adapter
            .uninstall(&uninstall_manager, &approval, &receipts)
            .expect("first uninstall");
        let second = harness
            .adapter
            .uninstall(&uninstall_manager, &approval, &receipts)
            .expect("repeat uninstall");
        assert!(!first.already_applied);
        assert!(second.already_applied);
    }

    #[test]
    fn test_detection_uses_cli_or_application_evidence_by_platform() {
        let harness = Harness::new(CursorPlatform::Windows);
        write_json(&harness.user_config(), &json!({}));
        write_json(&harness.project_config(), &json!({}));
        assert_eq!(
            harness.adapter.detect(&Scope::User),
            Detection::Installed {
                version: "1.4.2".to_owned(),
                config_path: harness
                    .adapter
                    .config_path(&Scope::User)
                    .expect("safe user config"),
            }
        );
        assert_eq!(
            harness
                .adapter
                .detect(&Scope::Project(harness.project.clone())),
            Detection::Installed {
                version: "1.4.2".to_owned(),
                config_path: harness
                    .adapter
                    .config_path(&Scope::Project(harness.project.clone()))
                    .expect("safe project config"),
            }
        );

        let app_home = tempdir().expect("app-only home");
        let app = app_home.path().join("Cursor.app");
        write_macos_application(&app, "2.3.4");
        write_json(&app_home.path().join(USER_CONFIG_RELATIVE_PATH), &json!({}));
        let app_only = CursorIntegration::new(
            app_home.path(),
            CursorDiscovery::new(
                CursorPlatform::MacOs,
                [],
                [CursorApplicationEvidence::new(app, None)],
            ),
        );
        assert!(matches!(
            app_only.detect(&Scope::User),
            Detection::Installed { version, .. } if version == "2.3.4"
        ));
    }

    #[test]
    fn test_detection_is_bounded_and_unsupported_platform_is_a_noop() {
        let directory = tempdir().expect("temporary directory");
        let search = (0..=MAX_CLI_CANDIDATES)
            .map(|index| {
                CursorCliEvidence::new(
                    directory.path().join(format!("bin-{index}/cursor")),
                    "Cursor 1.2.3",
                )
            })
            .collect::<Vec<_>>();
        let beyond_bound = &search.last().expect("last path").path;
        write_executable(beyond_bound);
        write_json(
            &directory.path().join(USER_CONFIG_RELATIVE_PATH),
            &json!({}),
        );
        let bounded = CursorIntegration::new(
            directory.path(),
            CursorDiscovery::new(CursorPlatform::Linux, search, []),
        );
        assert_eq!(bounded.detect(&Scope::User), Detection::NotFound);

        let config_before = fs::read(directory.path().join(USER_CONFIG_RELATIVE_PATH))
            .expect("configuration readable before unsupported plan");
        let unsupported = CursorIntegration::new(
            directory.path(),
            CursorDiscovery::new(
                CursorPlatform::Unsupported("plan9".to_owned()),
                [CursorCliEvidence::new(
                    directory.path().join("cursor"),
                    "Cursor 1.2.3",
                )],
                [],
            ),
        );
        assert!(matches!(
            unsupported.detect(&Scope::User),
            Detection::Unsupported { .. }
        ));
        assert!(!unsupported.verify(&Scope::User).verified);
        let manager = PlanManager::new("cursor-unsupported").expect("manager");
        let error = unsupported
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness_compresso(&directory),
                    scope: Scope::User,
                    session_recall: false,
                },
            )
            .expect_err("unsupported platform cannot mutate");
        assert!(matches!(error, IntegrationError::Unsupported { .. }));
        assert_eq!(
            fs::read(directory.path().join(USER_CONFIG_RELATIVE_PATH))
                .expect("configuration readable after unsupported plan"),
            config_before
        );
    }

    #[test]
    fn test_detection_requires_selected_config_to_exist() {
        let harness = Harness::new(CursorPlatform::Linux);

        assert_eq!(harness.adapter.detect(&Scope::User), Detection::NotFound);
        assert_eq!(
            harness
                .adapter
                .detect(&Scope::Project(harness.project.clone())),
            Detection::NotFound
        );

        write_json(&harness.project_config(), &json!({}));
        assert!(matches!(
            harness
                .adapter
                .detect(&Scope::Project(harness.project.clone())),
            Detection::Installed { config_path, .. }
                if config_path
                    == harness
                        .adapter
                        .config_path(&Scope::Project(harness.project.clone()))
                        .expect("safe project config")
        ));
        assert_eq!(harness.adapter.detect(&Scope::User), Detection::NotFound);
    }

    #[test]
    fn test_cli_version_evidence_is_bound_to_the_found_launcher() {
        let directory = tempdir().expect("temporary directory");
        let first = directory.path().join("first/cursor");
        let second = directory.path().join("second/cursor");
        write_executable(&first);
        write_executable(&second);
        write_json(
            &directory.path().join(USER_CONFIG_RELATIVE_PATH),
            &json!({}),
        );
        let adapter = CursorIntegration::new(
            directory.path(),
            CursorDiscovery::new(
                CursorPlatform::Linux,
                [
                    CursorCliEvidence::new(first, "Cursor 01.2.3"),
                    CursorCliEvidence::new(second, "Cursor 9.8.7"),
                ],
                [],
            ),
        );

        assert!(matches!(
            adapter.detect(&Scope::User),
            Detection::Unsupported { reason } if reason.contains("malformed")
        ));
    }

    #[test]
    fn test_application_detection_requires_native_artifact_and_reads_metadata() {
        let directory = tempdir().expect("temporary directory");
        let config = directory.path().join(USER_CONFIG_RELATIVE_PATH);
        write_json(&config, &json!({}));
        let empty_bundle = directory.path().join("Cursor.app");
        fs::create_dir(&empty_bundle).expect("empty bundle created");
        let false_positive = CursorIntegration::new(
            directory.path(),
            CursorDiscovery::new(
                CursorPlatform::MacOs,
                [],
                [CursorApplicationEvidence::new(
                    empty_bundle,
                    Some("Cursor 7.7.7".to_owned()),
                )],
            ),
        );
        assert_eq!(false_positive.detect(&Scope::User), Detection::NotFound);

        let windows_home = tempdir().expect("Windows application home");
        write_json(
            &windows_home.path().join(USER_CONFIG_RELATIVE_PATH),
            &json!({}),
        );
        let windows_app = windows_home.path().join("Cursor");
        write_executable(&windows_app.join("Cursor.exe"));
        write_json(
            &windows_app.join("resources/app/package.json"),
            &json!({"name": "cursor", "version": "3.4.5"}),
        );
        let metadata_detected = CursorIntegration::new(
            windows_home.path(),
            CursorDiscovery::new(
                CursorPlatform::Windows,
                [],
                [CursorApplicationEvidence::new(windows_app, None)],
            ),
        );
        assert!(matches!(
            metadata_detected.detect(&Scope::User),
            Detection::Installed { version, .. } if version == "3.4.5"
        ));
    }

    #[test]
    fn test_semver_parser_rejects_malformed_identifiers() {
        for malformed in [
            "1.2.3-..",
            "1.2.3-01",
            "1.2.3+build+again",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2",
            "1.2.3-",
            "1.2.3+",
            "vv1.2.3",
        ] {
            assert!(!is_semver(malformed), "accepted malformed {malformed}");
        }
        for valid in ["0.0.0", "1.2.3", "1.2.3-rc.1", "1.2.3+build.7"] {
            assert!(is_semver(valid), "rejected valid {valid}");
        }
        assert_eq!(
            parse_version_evidence("Cursor v1.2.3"),
            Some("1.2.3".to_owned())
        );
        assert_eq!(parse_version_evidence("Cursor vv1.2.3"), None);
    }

    #[test]
    fn test_verify_requires_artifact_bound_live_probe_and_emits_restart_instruction() {
        let harness = Harness::new(CursorPlatform::Linux);
        let manager = PlanManager::new("cursor-probe-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        harness.install(Scope::User, &manager, &receipts);

        let without_probe =
            CursorIntegration::new(harness.home.clone(), harness.adapter.discovery.clone());
        let missing = without_probe.verify(&Scope::User);
        assert!(!missing.verified);
        assert_eq!(check(&missing, "live_mcp_probe"), Some(false));
        assert_eq!(check(&missing, "cursor_restart_instruction"), Some(true));
        assert!(
            missing
                .checks
                .iter()
                .find(|check| check.name == "cursor_restart_instruction")
                .expect("restart instruction")
                .detail
                .contains("Cursor Settings -> MCP")
        );

        let other = harness.bin_directory.join("other-compresso");
        write_executable(&other);
        let stale = without_probe
            .with_live_probe_evidence(CursorLiveProbeEvidence::new(
                other,
                true,
                EXPECTED_MCP_TOOLS.into_iter().map(str::to_owned),
            ))
            .verify(&Scope::User);
        assert_eq!(check(&stale, "live_mcp_probe"), Some(false));

        let incomplete = harness
            .adapter
            .clone()
            .with_live_probe_evidence(CursorLiveProbeEvidence::new(
                harness.compresso.clone(),
                true,
                EXPECTED_MCP_TOOLS[..9]
                    .iter()
                    .map(|name| (*name).to_owned()),
            ))
            .verify(&Scope::User);
        assert_eq!(check(&incomplete, "live_mcp_probe"), Some(false));
    }

    #[test]
    fn test_windows_verify_rejects_non_executable_extension() {
        let harness = Harness::new(CursorPlatform::Windows);
        let data_file = harness.bin_directory.join("compresso.txt");
        write_executable(&data_file);
        write_json(
            &harness.user_config(),
            &json!({
                "mcpServers": {
                    "compresso": {
                        "command": data_file,
                        "args": ["mcp", "serve"]
                    }
                }
            }),
        );
        let adapter =
            harness
                .adapter
                .clone()
                .with_live_probe_evidence(CursorLiveProbeEvidence::new(
                    harness.bin_directory.join("compresso.txt"),
                    true,
                    EXPECTED_MCP_TOOLS.into_iter().map(str::to_owned),
                ));

        let report = adapter.verify(&Scope::User);

        assert!(!report.verified);
        assert_eq!(check(&report, "binary_resolves"), Some(false));
    }

    #[test]
    fn test_missing_uninstall_is_typed_and_does_not_write() {
        let harness = Harness::new(CursorPlatform::Linux);
        let install_manager = PlanManager::new("cursor-missing-install").expect("manager");
        let receipts = MemoryReceiptStore::default();
        let receipt = harness.install(Scope::User, &install_manager, &receipts);
        fs::remove_file(&receipt.config_path).expect("installed config removed by user");
        let before = directory_entries(receipt.config_path.parent().expect("config parent"));
        let uninstall_manager = PlanManager::new("cursor-missing-uninstall").expect("manager");

        let error = harness
            .adapter
            .plan_uninstall(&uninstall_manager, &receipt)
            .expect_err("missing uninstall target rejected");

        assert!(matches!(
            error,
            IntegrationError::UninstallTargetMissing { path } if path == receipt.config_path
        ));
        assert!(!receipt.config_path.exists());
        assert_eq!(
            directory_entries(receipt.config_path.parent().expect("config parent")),
            before
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_project_scope_rejects_cursor_directory_symlink_to_user_config() {
        use std::os::unix::fs::symlink;

        let harness = Harness::new(CursorPlatform::Linux);
        let user_original = b"{\"userOnly\":true}\n";
        write_bytes(&harness.user_config(), user_original);
        symlink(
            harness
                .user_config()
                .parent()
                .expect("user Cursor directory"),
            harness.project.join(".cursor"),
        )
        .expect("malicious project symlink created");
        let manager = PlanManager::new("cursor-project-symlink").expect("manager");

        let error = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::Project(harness.project.clone()),
                    session_recall: false,
                },
            )
            .expect_err("project symlink rejected");

        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        assert_eq!(
            fs::read(harness.user_config()).expect("user config remains readable"),
            user_original
        );
        assert!(!harness.project.join("mcp.json.compresso-journal").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_project_scope_rejects_config_file_symlink_to_user_config() {
        use std::os::unix::fs::symlink;

        let harness = Harness::new(CursorPlatform::Linux);
        let user_original = b"{\"userOnly\":true}\n";
        write_bytes(&harness.user_config(), user_original);
        fs::create_dir(harness.project.join(".cursor")).expect("project Cursor directory");
        symlink(harness.user_config(), harness.project_config())
            .expect("malicious project file symlink created");
        let manager = PlanManager::new("cursor-project-file-symlink").expect("manager");

        let error = harness
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::Project(harness.project.clone()),
                    session_recall: false,
                },
            )
            .expect_err("project file symlink rejected");

        assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
        assert_eq!(
            fs::read(harness.user_config()).expect("user config remains readable"),
            user_original
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_user_scope_rejects_cursor_directory_and_config_symlinks() {
        use std::os::unix::fs::symlink;

        for symlink_directory in [true, false] {
            let harness = Harness::new(CursorPlatform::Linux);
            let outside = harness.project.join("outside-cursor");
            let outside_config = outside.join("mcp.json");
            let original = b"{\"outsideSecret\":\"never expose\"}\n";
            write_bytes(&outside_config, original);
            if symlink_directory {
                symlink(&outside, harness.home.join(".cursor"))
                    .expect("malicious user directory symlink created");
            } else {
                fs::create_dir(harness.home.join(".cursor"))
                    .expect("user Cursor directory created");
                symlink(&outside_config, harness.user_config())
                    .expect("malicious user config symlink created");
            }
            let manager = PlanManager::new(format!("cursor-user-symlink-{symlink_directory}"))
                .expect("manager");

            let error = harness
                .adapter
                .plan_install(
                    &manager,
                    &InstallCtx {
                        compresso_bin: harness.compresso.clone(),
                        scope: Scope::User,
                        session_recall: false,
                    },
                )
                .expect_err("user symlink rejected");

            assert!(matches!(error, IntegrationError::InvalidRequest { .. }));
            assert_eq!(
                fs::read(&outside_config).expect("outside config read"),
                original
            );
            assert!(matches!(
                harness.adapter.detect(&Scope::User),
                Detection::Unsupported { reason } if reason.contains("symbolic link")
            ));
            let report = harness.adapter.verify(&Scope::User);
            assert!(!report.verified);
            assert!(
                report
                    .checks
                    .iter()
                    .all(|check| !check.detail.contains("never expose"))
            );
        }
    }

    #[test]
    fn test_project_scope_does_not_consult_an_unsafe_user_home() {
        let harness = Harness::new(CursorPlatform::Linux);
        let adapter = CursorIntegration::new(
            PathBuf::from("relative-unsafe-home"),
            harness.adapter.discovery.clone(),
        )
        .with_live_probe_evidence(expected_probe(&harness.compresso));
        let manager = PlanManager::new("cursor-project-unsafe-home").expect("manager");

        let plan = adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: harness.compresso.clone(),
                    scope: Scope::Project(harness.project.clone()),
                    session_recall: false,
                },
            )
            .expect("project plan does not need user home");

        assert_eq!(
            plan.config_path,
            adapter
                .config_path(&Scope::Project(harness.project.clone()))
                .expect("safe project config")
        );
        assert!(!Path::new("relative-unsafe-home").exists());
    }

    fn harness_compresso(directory: &TempDir) -> PathBuf {
        let binary = directory.path().join("compresso");
        write_executable(&binary);
        binary
    }

    fn expected_probe(command: &Path) -> CursorLiveProbeEvidence {
        CursorLiveProbeEvidence::new(
            command,
            true,
            EXPECTED_MCP_TOOLS.into_iter().map(str::to_owned),
        )
    }

    fn write_macos_application(path: &Path, version: &str) {
        write_executable(&path.join("Contents/MacOS/Cursor"));
        write_bytes(
            &path.join("Contents/Info.plist"),
            format!(
                "<?xml version=\"1.0\"?><plist><dict><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"
            )
            .as_bytes(),
        );
    }

    fn directory_entries(path: &Path) -> Vec<String> {
        let mut entries = fs::read_dir(path)
            .expect("directory readable")
            .map(|entry| {
                entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn assert_in_order(text: &str, needles: &[&str]) {
        let mut previous = 0;
        for needle in needles {
            let position = text[previous..]
                .find(needle)
                .map(|offset| previous + offset)
                .unwrap_or_else(|| panic!("missing {needle} in {text}"));
            assert!(
                position >= previous,
                "{needle} appeared out of order in {text}"
            );
            previous = position + needle.len();
        }
    }

    fn write_executable(path: &Path) {
        fs::create_dir_all(path.parent().expect("binary parent")).expect("binary parent created");
        fs::write(path, b"test executable").expect("test executable written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .expect("test executable mode");
        }
    }

    fn write_bytes(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("config parent")).expect("config parent created");
        fs::write(path, bytes).expect("config written");
    }

    fn write_json(path: &Path, value: &Value) {
        write_bytes(
            path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(value).expect("JSON rendered")
            )
            .as_bytes(),
        );
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).expect("JSON read")).expect("JSON parsed")
    }

    fn check(report: &VerificationReport, name: &str) -> Option<bool> {
        report
            .checks
            .iter()
            .find(|check| check.name == name)
            .map(|check| check.passed)
    }
}
